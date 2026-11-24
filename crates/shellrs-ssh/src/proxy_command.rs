//! A `ProxyCommand` transport: an external command whose stdin and stdout
//! carry the SSH bytes, the way OpenSSH runs it. The process is spawned
//! without a window and lives exactly as long as the connection that runs
//! over it; what it writes to stderr is kept, so a connection that fails
//! because the command ended can say what the command said.

use std::{
    collections::VecDeque,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use anyhow::{Result, bail};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, ReadBuf},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{oneshot, watch},
};

use super::error::{RouteFailure, WrapErr as _};

/// How much of what the command writes to stderr is kept.
const OUTPUT_KEPT: usize = 4096;
/// The lines of it a failure shows.
const OUTPUT_LINES: usize = 3;
/// How long an ended connection gives the command to go after SIGHUP
/// before it is killed.
#[cfg(unix)]
const GRACE: Duration = Duration::from_secs(1);

/// Expand OpenSSH's tokens in a `ProxyCommand`: `%h` the host, `%p` the
/// port, `%r` the remote user, `%n` the host as written (the same as `%h`
/// here), `%d` the local home, and `%%` a literal percent. Any other `%x`
/// is passed through untouched.
///
/// A host or user that would read as more than one word to the shell, or
/// as an option, is refused rather than put in the command line, as
/// OpenSSH 9.6 does (CVE-2023-51385).
pub(super) fn expand(
    command: &str,
    host: &str,
    port: u16,
    user: &str,
) -> Result<String, RouteFailure> {
    let checked = |token: char, value: &str| {
        if is_plain(value) {
            Ok(value.to_owned())
        } else {
            Err(RouteFailure::ProxyCommandUnsafe { token })
        }
    };
    let mut out = String::with_capacity(command.len());
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let value = match chars.peek() {
            Some(&token @ ('h' | 'n')) => checked(token, host)?,
            Some('r') => checked('r', user)?,
            Some('p') => port.to_string(),
            Some('d') => dirs::home_dir()
                .map(|home| home.display().to_string())
                .unwrap_or_else(|| "~".to_owned()),
            Some('%') => "%".to_owned(),
            _ => {
                out.push('%');
                continue;
            }
        };
        chars.next();
        out.push_str(&value);
    }
    Ok(out)
}

/// Whether `value` is safe to put in a shell command line as one word:
/// what OpenSSH allows in a host or user name it expands.
fn is_plain(value: &str) -> bool {
    !value.starts_with('-')
        && !value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || "'`\"$\\;&<>|(){}".contains(ch))
}

/// A stream to the host carried by a `ProxyCommand` process: what is
/// written goes to the process's stdin, what the process writes comes out
/// of its stdout. Dropping it ends the process, and on Unix every process
/// it started; on Windows its whole tree.
pub(super) struct ProxyCommandStream {
    stdin: ChildStdin,
    stdout: ChildStdout,
    _end: End,
}

/// What a proxy command wrote to stderr and how it ended, kept after its
/// stream went to the connection.
pub(super) struct ProxyCommandWatch {
    output: Arc<Mutex<VecDeque<u8>>>,
    /// Becomes true once stderr is read to its end.
    output_read: watch::Receiver<bool>,
    ended: watch::Receiver<Option<Ended>>,
}

/// How the command ended: its exit code, or none when a signal ended it.
#[derive(Clone, Copy, Debug)]
struct Ended(Option<i32>);

impl ProxyCommandWatch {
    /// Why a connection over the command failed, when the command ended
    /// within `wait`: its exit code and the last lines it wrote to stderr.
    pub(super) async fn ended_within(&self, wait: Duration) -> Option<RouteFailure> {
        let mut ended = self.ended.clone();
        let Ended(code) = {
            let ended = tokio::time::timeout(wait, ended.wait_for(Option::is_some))
                .await
                .ok()?
                .ok()?;
            (*ended)?
        };
        // What it wrote just before may still be on its way; a process it
        // left behind may hold stderr open, so not for long.
        let mut output_read = self.output_read.clone();
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            output_read.wait_for(|read| *read),
        )
        .await;
        let output = last_lines(&lock(&self.output).iter().copied().collect::<Vec<_>>());
        Some(RouteFailure::ProxyCommandExited { code, output })
    }
}

/// The last few lines of `output` that say something, one per line.
fn last_lines(output: &[u8]) -> String {
    let text = String::from_utf8_lossy(output);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines[lines.len().saturating_sub(OUTPUT_LINES)..].join("\n")
}

/// Spawn `command` with its stdio wired as the stream. On Unix it runs
/// as OpenSSH runs it, `exec`ed by `sh -c`, so shell syntax works and the
/// shell does not stay in between; on Windows through `cmd /S /C` without
/// a console. Phones run no other programs.
pub(super) fn spawn(command: &str) -> Result<(ProxyCommandStream, ProxyCommandWatch)> {
    if cfg!(any(target_os = "ios", target_os = "android")) {
        bail!(RouteFailure::ProxyCommandUnsupported);
    }
    let mut builder = shell_command(command);
    builder
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // The last resort, should the runtime go before the waiter does.
        .kill_on_drop(true);
    let mut child = builder.spawn().wrap_err(RouteFailure::ProxyCommand)?;
    #[cfg(windows)]
    let job = job::contain(&child).wrap_err(RouteFailure::ProxyCommand)?;
    let (Some(stdin), Some(stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        unreachable!("all three are piped");
    };

    // Read stderr all along, or a chatty command would fill the pipe and
    // stall; keep only its end.
    let output = Arc::new(Mutex::new(VecDeque::with_capacity(OUTPUT_KEPT)));
    let kept = output.clone();
    let (read_to_end, output_read) = watch::channel(false);
    tokio::spawn(async move {
        let mut buffer = [0; 1024];
        while let Ok(read @ 1..) = stderr.read(&mut buffer).await {
            let mut kept = lock(&kept);
            kept.extend(&buffer[..read]);
            let over = kept.len().saturating_sub(OUTPUT_KEPT);
            kept.drain(..over);
        }
        let _ = read_to_end.send(true);
    });

    let (ended_tx, ended) = watch::channel(None);
    let (stop, stopped) = oneshot::channel();
    #[cfg(unix)]
    let group = child.id().and_then(|pid| i32::try_from(pid).ok());
    tokio::spawn(wait(child, stopped, ended_tx));
    let end = End {
        _stop: stop,
        #[cfg(unix)]
        ended: ended.clone(),
        #[cfg(unix)]
        group,
        #[cfg(windows)]
        _job: job,
    };
    Ok((
        ProxyCommandStream {
            stdin,
            stdout,
            _end: end,
        },
        ProxyCommandWatch {
            output,
            output_read,
            ended,
        },
    ))
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut builder = Command::new("sh");
    builder.arg("-c").arg(format!("exec {command}"));
    // Its own process group, so the group can be signalled as one when the
    // connection ends.
    builder.process_group(0);
    #[cfg(target_os = "macos")]
    if let Some(path) = macos::search_path() {
        builder.env("PATH", path);
    }
    builder
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut builder = Command::new("cmd");
    // As typed: `arg` would quote it for the C runtime's rules, and cmd
    // reads quotes its own way. `/S` takes away just the outer pair.
    builder.raw_arg("/S /C").raw_arg(format!("\"{command}\""));
    // CREATE_NO_WINDOW: a ProxyCommand has no console of its own.
    builder.creation_flags(0x0800_0000);
    builder
}

/// Waits for the command to end, and records how. Once the stream is
/// dropped (`stopped`), gives it a second to go on its own, then kills it.
async fn wait(
    mut child: Child,
    stopped: oneshot::Receiver<()>,
    ended: watch::Sender<Option<Ended>>,
) {
    let status = tokio::select! {
        status = child.wait() => status,
        _ = stopped => {
            #[cfg(unix)]
            let group = child.id().and_then(|pid| i32::try_from(pid).ok());
            #[cfg(unix)]
            let status = match tokio::time::timeout(GRACE, child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    if let Some(group) = group {
                        signal_group(group, libc::SIGKILL);
                    }
                    child.wait().await
                }
            };
            // Closing the job ended the tree already.
            #[cfg(windows)]
            let status = child.wait().await;
            status
        }
    };
    let _ = ended.send(Some(Ended(status.ok().and_then(|status| status.code()))));
}

/// Ends the command when the stream goes.
struct End {
    /// Dropped with the stream: tells [`wait`] to see the command off.
    _stop: oneshot::Sender<()>,
    #[cfg(unix)]
    ended: watch::Receiver<Option<Ended>>,
    /// The command's process group: its PID (`process_group(0)`).
    #[cfg(unix)]
    group: Option<i32>,
    /// The job the command's tree runs in: closing it ends them all.
    #[cfg(windows)]
    _job: std::os::windows::io::OwnedHandle,
}

impl Drop for End {
    fn drop(&mut self) {
        // SIGHUP to the whole group, as OpenSSH sends it, here rather than
        // in the waiter: the runtime may be going down with the stream.
        // Not once the command has been waited for, as the group's number
        // may then be another's.
        #[cfg(unix)]
        if let Some(group) = self.group
            && self.ended.borrow().is_none()
        {
            signal_group(group, libc::SIGHUP);
        }
    }
}

#[cfg(unix)]
fn signal_group(group: i32, signal: i32) {
    // SAFETY: kill(2) with a negative PID signals that process group; it
    // reads no memory of ours.
    unsafe {
        libc::kill(-group, signal);
    }
}

impl AsyncRead for ProxyCommandStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdout).poll_read(cx, buf)
    }
}

impl AsyncWrite for ProxyCommandStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stdin).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_shutdown(cx)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Where programs live on a Mac beyond what an app started from the Finder
/// is given (`/usr/bin:/bin:/usr/sbin:/sbin`): Homebrew, MacPorts, pip,
/// Go.
#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};

    pub(super) fn search_path() -> Option<OsString> {
        let current = std::env::var_os("PATH").unwrap_or_default();
        search_path_from(&current, dirs::home_dir().as_deref())
    }

    /// `current` with the folders that are there and not in it yet after
    /// it.
    pub(super) fn search_path_from(current: &OsStr, home: Option<&Path>) -> Option<OsString> {
        let mut folders: Vec<PathBuf> = std::env::split_paths(current)
            .filter(|folder| !folder.as_os_str().is_empty())
            .collect();
        let more = [
            "/opt/homebrew/bin",
            "/opt/homebrew/sbin",
            "/usr/local/bin",
            "/opt/local/bin",
        ]
        .map(PathBuf::from)
        .into_iter()
        .chain(home.map(|home| home.join(".local/bin")))
        .chain(home.map(|home| home.join("go/bin")));
        for folder in more {
            if folder.is_dir() && !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        std::env::join_paths(folders).ok()
    }
}

/// The job object a command's process tree runs in.
#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::{FromRawHandle as _, OwnedHandle};

    use tokio::process::Child;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// Put `child` in a job of its own that ends every process in it when
    /// the returned handle closes: `cmd` and what it started.
    pub(super) fn contain(child: &Child) -> io::Result<OwnedHandle> {
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("the proxy command already ended"))?;
        // SAFETY: no attributes and no name; a null result is an error.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `job` is a handle just made, owned from here on.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        let raw = std::os::windows::io::AsRawHandle::as_raw_handle(&job);
        // SAFETY: zeroed is a valid value of this plain C struct.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the buffer is the struct the class names, of its size.
        let set = unsafe {
            SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both handles are open.
        if unsafe { AssignProcessToJobObject(raw, process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_expand_as_openssh_does() {
        assert_eq!(
            expand("ncat %h %p", "db.internal", 2222, "root").unwrap(),
            "ncat db.internal 2222"
        );
        assert_eq!(
            expand("%r@%n:%p", "10.0.0.5", 22, "deploy").unwrap(),
            "deploy@10.0.0.5:22"
        );
        assert_eq!(expand("%%h", "h", 22, "u").unwrap(), "%h");
        assert_eq!(expand("x %q y 100%", "h", 22, "u").unwrap(), "x %q y 100%");
    }

    #[test]
    fn a_host_or_user_the_shell_would_split_is_refused() {
        assert_eq!(
            expand("nc %h %p", "h; rm -rf ~", 22, "u"),
            Err(RouteFailure::ProxyCommandUnsafe { token: 'h' })
        );
        assert_eq!(
            expand("ssh -l %r jump", "h", 22, "$(id)"),
            Err(RouteFailure::ProxyCommandUnsafe { token: 'r' })
        );
        assert_eq!(
            expand("nc %h %p", "-oProxyCommand=x", 22, "u"),
            Err(RouteFailure::ProxyCommandUnsafe { token: 'h' })
        );
        // Only what the command uses is looked at.
        assert!(expand("nc %h %p", "h", 22, "it's me").is_ok());
    }

    #[test]
    fn a_failure_shows_the_last_lines_that_say_something() {
        assert_eq!(
            last_lines(b"one\n\n two \nthree\r\nfour\n\n"),
            "two\nthree\nfour"
        );
        assert_eq!(last_lines(b""), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_proxy_command_streams_both_ways() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let (mut stream, _watch) = spawn("cat").unwrap();
        stream.write_all(b"ping").await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_ended_command_says_why() {
        let (_stream, watch) = spawn("sh -c 'echo first >&2; echo nope >&2; exit 3'").unwrap();
        assert_eq!(
            watch.ended_within(Duration::from_secs(5)).await,
            Some(RouteFailure::ProxyCommandExited {
                code: Some(3),
                output: "first\nnope".into()
            })
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_running_command_has_not_ended() {
        let (_stream, watch) = spawn("cat").unwrap();
        assert_eq!(watch.ended_within(Duration::from_millis(100)).await, None);
    }

    /// Dropping the stream ends the command and what it started, though
    /// it ignores the hang-up.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_stream_ends_the_whole_group() {
        use tokio::io::AsyncReadExt as _;

        let (mut stream, watch) =
            spawn("sh -c 'trap \"\" HUP; sleep 30 & echo ready; wait'").unwrap();
        let mut ready = [0u8; 6];
        stream.read_exact(&mut ready).await.unwrap();
        let group = stream._end.group.unwrap();
        drop(stream);
        let ended = watch.ended_within(Duration::from_secs(5)).await;
        assert_eq!(
            ended,
            Some(RouteFailure::ProxyCommandExited {
                code: None,
                output: String::new()
            }),
            "killed after the hang-up went unheeded"
        );
        // The `sleep` it started is gone with it.
        tokio::time::sleep(Duration::from_millis(100)).await;
        // SAFETY: signal 0 only asks whether the group has a process left.
        let alive = unsafe { libc::kill(-group, 0) } == 0;
        assert!(!alive, "nothing of the group is left");
    }

    /// A whole SSH login over `nc`, to a server in this process, the way
    /// a host with this route connects.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_ssh_login_runs_over_the_command() {
        use crate::testing::{TEST_PASSWORD, TEST_USER, TestServer};
        use crate::{
            HostLogin, LoginMethod, LoginRoute, ProxyCommandLogin, SecretRef, SecretSource,
            SshConnectionConfig, SshConnector, SshPrompts,
        };
        use std::sync::Weak;
        use zeroize::Zeroizing;

        let has_nc = std::process::Command::new("sh")
            .args(["-c", "command -v nc"])
            .output()
            .is_ok_and(|output| output.status.success());
        if !has_nc {
            eprintln!("skipped: no nc here");
            return;
        }
        struct Saved;
        impl SecretSource for Saved {
            fn secret(&self, _: &SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
                Ok(Some(Zeroizing::new(TEST_PASSWORD.into())))
            }
        }
        let data = tempfile::tempdir().unwrap();
        let server = TestServer::start(data.path().into()).await.unwrap();
        let connector = SshConnector::new(data.path().join("known_hosts"), Arc::new(Saved));
        let login = HostLogin::new("127.0.0.1", server.port, TEST_USER, LoginMethod::Password)
            .with_route(LoginRoute::ProxyCommand(ProxyCommandLogin {
                command: "nc %h %p".into(),
            }));
        let (_shutdown, shutdown) = watch::channel(false);
        // The server's key is new: trusted when asked.
        let broker = Arc::new_cyclic(|broker: &Weak<SshPrompts>| {
            let broker = broker.clone();
            SshPrompts::new(
                Arc::new(move |prompt| {
                    if let Some(broker) = broker.upgrade() {
                        broker.respond(
                            prompt.request_id(),
                            crate::ConnectionPromptReply::TrustAndSave,
                        );
                    }
                    true
                }),
                shutdown,
            )
        });
        let (handle, _fingerprint) = tokio::time::timeout(
            Duration::from_secs(20),
            connector.connect(&SshConnectionConfig::from(&login), broker),
        )
        .await
        .expect("in time")
        .unwrap();
        crate::hang_up(handle, "test finished").await;

        // A command that is not there: the failure is its end, and what
        // the shell said about it.
        let missing = login.with_route(LoginRoute::ProxyCommand(ProxyCommandLogin {
            command: "no-such-relay-here %h %p".into(),
        }));
        let (_shutdown, shutdown) = watch::channel(false);
        let broker = Arc::new(SshPrompts::new(Arc::new(|_| false), shutdown));
        let Err(error) = connector
            .connect(&SshConnectionConfig::from(&missing), broker)
            .await
        else {
            panic!("connected through a command that is not there");
        };
        let Some(RouteFailure::ProxyCommandExited { code, output }) =
            crate::error::find_cause::<RouteFailure>(&error)
        else {
            panic!("{error:?}");
        };
        assert_eq!(*code, Some(127));
        assert!(output.contains("no-such-relay-here"), "{output}");
    }

    /// An app started from the Finder has only the system's folders; the
    /// usual places programs are installed come after them.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_mac_looks_where_programs_are_installed() {
        use std::path::Path;

        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("go/bin")).unwrap();
        let path =
            macos::search_path_from("/usr/bin::/bin:/usr/local/bin".as_ref(), Some(home.path()))
                .unwrap();
        let folders: Vec<_> = std::env::split_paths(&path).collect();
        assert_eq!(
            folders[..3],
            [
                Path::new("/usr/bin"),
                Path::new("/bin"),
                Path::new("/usr/local/bin")
            ]
        );
        assert_eq!(folders.last().unwrap(), &home.path().join("go/bin"));
        // Not there, so not looked in; already in, so not twice.
        assert!(!folders.contains(&home.path().join(".local/bin")));
        assert_eq!(
            folders
                .iter()
                .filter(|folder| folder.as_path() == Path::new("/usr/local/bin"))
                .count(),
            1
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_quoted_command_runs_as_typed_on_windows() {
        use tokio::io::AsyncReadExt as _;

        let (mut stream, _watch) = spawn(r#"echo "a b"&& echo done"#).unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).await.unwrap();
        assert_eq!(out.replace("\r\n", "\n"), "\"a b\"\ndone\n");
    }
}
