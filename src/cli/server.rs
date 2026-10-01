//! The app's end of the CLI socket. Listens for as long as the app runs and
//! answers each connection on a thread of its own; the work itself is the
//! backend's, so tests can put a fake behind the same server.

use std::{
    io::{self, BufReader},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

#[cfg(unix)]
use tokio::sync::watch;

use super::protocol::{
    CliError, Envelope, ErrorCode, FrameKind, HostInfo, PROTOCOL_VERSION, Reply, Request,
    TransferCounters, TransferSummary, parse_json, read_frame, write_frame, write_json,
};
use crate::host::{Host, HostLogin, HostStore, matches_query};
use crate::ssh::ExecStream;

/// Does what a CLI request asks. Every method blocks: each request has a
/// thread of its own.
pub trait CliBackend: Send + Sync + 'static {
    /// Run `command` on the host, passing its output on as it
    /// comes; the remote exit code. When `output` fails, the caller is gone
    /// and the command should be abandoned.
    fn exec(
        &self,
        target: &CliTarget,
        command: &str,
        output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    ) -> Result<i32, CliError>;

    /// Copy the local `source` to `destination` on the host, the way scp
    /// does. When `progress` fails, the caller is gone and the transfer
    /// should stop.
    fn upload(
        &self,
        target: &CliTarget,
        source: &Path,
        destination: &str,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError>;

    /// Copy `source` on the host to the local `destination`, the way scp
    /// does.
    fn download(
        &self,
        target: &CliTarget,
        source: &str,
        destination: &Path,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError>;
}

/// A saved host as the CLI sees it, with the login it connects with
/// resolved when the target list was built.
#[derive(Clone, Debug)]
pub struct CliTarget {
    info: HostInfo,
    host: Host,
    login: HostLogin,
}

impl CliTarget {
    /// `group` is the host's full group path, if it has one.
    pub fn new(host: &Host, login: HostLogin, group: Option<String>) -> Self {
        Self {
            info: HostInfo {
                id: host.public_id.to_string(),
                name: host.name.to_string(),
                group,
                user: host.user.to_string(),
                host: host.address.to_string(),
                port: host.port,
                os: host.os.map(|os| os.as_str().to_string()),
            },
            host: host.clone(),
            login,
        }
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// How a request logs in to the host.
    pub fn login(&self) -> &HostLogin {
        &self.login
    }

    /// Every host in `store`, with its group path.
    pub fn all(store: &HostStore) -> Vec<Self> {
        store
            .hosts()
            .iter()
            .map(|host| {
                let names = host
                    .group
                    .map(|id| store.group_names(id))
                    .unwrap_or_default();
                Self::new(
                    host,
                    store.login_of(host),
                    (!names.is_empty()).then(|| names.join("/")),
                )
            })
            .collect()
    }

    /// The host tree's search, plus the group path and the ID.
    fn matches(&self, query: &str) -> bool {
        let needle = query.trim().to_lowercase();
        matches_query(&self.host, query)
            || self
                .info
                .group
                .as_ref()
                .is_some_and(|group| group.to_lowercase().contains(&needle))
            || self.info.id.to_lowercase() == needle
    }
}

/// What the listener and the request threads share with the app.
struct Shared {
    enabled: AtomicBool,
    targets: RwLock<Vec<CliTarget>>,
    backend: Arc<dyn CliBackend>,
    /// Set when ShellRS was opened again while this one runs, until the
    /// app has brought its window forward.
    activation: AtomicBool,
}

/// Listens on the CLI socket until dropped. Always listening, even with
/// 启用外部 CLI off, so a caller hears "not enabled" rather than "not
/// running" and can tell the user which of the two to fix.
pub struct CliServer {
    shared: Arc<Shared>,
    _listener: Listener,
}

impl CliServer {
    /// Take over `endpoint` (from [`crate::app::cli_endpoint`]), which must
    /// not be another running ShellRS's. Only the user running the app can
    /// connect.
    pub fn start(endpoint: PathBuf, backend: Arc<dyn CliBackend>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            enabled: AtomicBool::new(false),
            targets: RwLock::new(Vec::new()),
            backend,
            activation: AtomicBool::new(false),
        });
        let listener = listen(endpoint, shared.clone())?;
        Ok(Self {
            shared,
            _listener: listener,
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::Release);
    }

    pub fn set_targets(&self, targets: Vec<CliTarget>) {
        *self
            .shared
            .targets
            .write()
            .unwrap_or_else(|error| error.into_inner()) = targets;
    }

    /// Whether ShellRS was opened again since this was last asked. The
    /// request threads cannot reach the window, so the app asks here on a
    /// timer and brings its window forward when the answer is yes.
    pub fn take_activation(&self) -> bool {
        self.shared.activation.swap(false, Ordering::AcqRel)
    }
}

/// The listening thread, stopped when dropped. On Unix, the socket file
/// goes with it.
#[cfg(unix)]
struct Listener {
    socket: PathBuf,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// The socket is the user's alone, and every caller's user id is checked
/// again.
#[cfg(unix)]
fn listen(socket: PathBuf, shared: Arc<Shared>) -> io::Result<Listener> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};

    if std::fs::symlink_metadata(&socket).is_ok() {
        if UnixStream::connect(&socket).is_ok() {
            return Err(another_app());
        }
        // Left behind by a ShellRS that did not get to clean up.
        std::fs::remove_file(&socket)?;
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let owner = std::fs::metadata(&socket)?.uid();
    listener.set_nonblocking(true)?;

    let (shutdown, mut stop) = watch::channel(false);
    let thread = std::thread::Builder::new()
        .name("shellrs-cli".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::UnixListener::from_std(listener) else {
                    return;
                };
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            if let Ok((stream, _)) = accepted {
                                accept(stream, owner, &shared);
                            }
                        }
                        _ = stop.changed() => break,
                    }
                }
            });
        })?;
    Ok(Listener {
        socket,
        shutdown,
        thread: Some(thread),
    })
}

/// Hand a connection from the same user to a thread of its own.
#[cfg(unix)]
fn accept(stream: tokio::net::UnixStream, owner: u32, shared: &Arc<Shared>) {
    if !stream
        .peer_cred()
        .is_ok_and(|credentials| credentials.uid() == owner)
    {
        return;
    }
    let Ok(stream) = stream.into_std() else {
        return;
    };
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let shared = shared.clone();
    let _ = std::thread::Builder::new()
        .name("shellrs-cli-request".into())
        .spawn(move || {
            // A caller that hangs up mid-answer is no one's problem.
            let _ = serve(&mut BufReader::new(&stream), &mut &stream, &shared);
        });
}

/// The listening thread, stopped when dropped.
#[cfg(windows)]
struct Listener {
    pipe: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let Some(thread) = self.thread.take() else {
            return;
        };
        // The thread waits in ConnectNamedPipe; a caller wakes it. Until it
        // has gone: between two pipe instances there is nothing to connect
        // to, so one knock may not be heard.
        while !thread.is_finished() {
            let _ = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.pipe);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = thread.join();
    }
}

/// A named pipe only the current user can open, and only from this
/// machine. Its name is the machine's, not a file of the user's, so it is
/// created as the first instance: if anyone already holds it, the server
/// does not start.
#[cfg(windows)]
fn listen(pipe: PathBuf, shared: Arc<Shared>) -> io::Result<Listener> {
    use super::pipe_windows;

    let first = pipe_windows::create_instance(&pipe, true).map_err(|error| {
        if pipe_windows::is_taken(&error) {
            another_app()
        } else {
            error
        }
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name("shellrs-cli".into())
        .spawn({
            let (pipe, stop) = (pipe.clone(), stop.clone());
            move || {
                let mut waiting = first;
                loop {
                    let connected = pipe_windows::wait_for_client(&waiting);
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    // The next instance first, so a caller arriving now
                    // finds the pipe busy rather than gone.
                    let next = loop {
                        match pipe_windows::create_instance(&pipe, false) {
                            Ok(next) => break Some(next),
                            Err(_) if !stop.load(Ordering::Acquire) => {
                                std::thread::sleep(std::time::Duration::from_millis(100))
                            }
                            Err(_) => break None,
                        }
                    };
                    let Some(next) = next else {
                        break;
                    };
                    let current = std::mem::replace(&mut waiting, next);
                    // Otherwise the caller came and went already.
                    if connected.is_ok() {
                        answer(std::fs::File::from(current), &shared);
                    }
                }
            }
        })?;
    Ok(Listener {
        pipe,
        stop,
        thread: Some(thread),
    })
}

/// Answer a connected caller on a thread of its own. Closing the pipe
/// afterwards keeps what the caller has not read yet: only
/// `DisconnectNamedPipe` would throw it away.
#[cfg(windows)]
fn answer(pipe: std::fs::File, shared: &Arc<Shared>) {
    let shared = shared.clone();
    let _ = std::thread::Builder::new()
        .name("shellrs-cli-request".into())
        .spawn(move || {
            // A caller that hangs up mid-answer is no one's problem.
            let _ = serve(&mut BufReader::new(&pipe), &mut &pipe, &shared);
        });
}

#[cfg(not(any(unix, windows)))]
struct Listener;

#[cfg(not(any(unix, windows)))]
fn listen(_: PathBuf, _: Arc<Shared>) -> io::Result<Listener> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "此系统暂不支持外部 CLI",
    ))
}

fn another_app() -> io::Error {
    io::Error::new(io::ErrorKind::AddrInUse, "另一个 ShellRS 已在提供外部 CLI")
}

/// Read one request and answer it.
fn serve(
    reader: &mut impl io::Read,
    writer: &mut impl io::Write,
    shared: &Shared,
) -> io::Result<()> {
    let Some((FrameKind::Json, payload)) = read_frame(reader)? else {
        return Ok(());
    };
    let reply = respond(&payload, writer, shared).unwrap_or_else(|error| Reply::Error {
        code: error.code,
        message: error.message,
    });
    write_json(writer, &reply)
}

/// Carry out one request. Output and progress go to `writer` as they come;
/// what comes back is the reply that ends the connection, or the error sent
/// in its place.
fn respond(
    payload: &[u8],
    writer: &mut impl io::Write,
    shared: &Shared,
) -> Result<Reply, CliError> {
    let envelope: Envelope = parse_json(payload)
        .map_err(|error| CliError::new(ErrorCode::BadRequest, error.to_string()))?;
    // Before the checks below: coming forward means the same in every
    // version, and it is ShellRS being opened again, not the external CLI.
    if envelope.request == Request::Activate {
        shared.activation.store(true, Ordering::Release);
        return Ok(Reply::Activated);
    }
    if envelope.version != PROTOCOL_VERSION {
        return Err(CliError::new(
            ErrorCode::VersionMismatch,
            "shellrs 命令与正在运行的 ShellRS 版本不同：\
             请重新启动 ShellRS，或在 设置 → 外部 CLI 中更新 CLI",
        ));
    }
    if !shared.enabled.load(Ordering::Acquire) {
        return Err(CliError::new(
            ErrorCode::NotEnabled,
            "ShellRS 未启用外部 CLI：请在 ShellRS 的 设置 → 外部 CLI 中打开「启用外部 CLI」",
        ));
    }
    match envelope.request {
        Request::List { query } => {
            let targets = shared
                .targets
                .read()
                .unwrap_or_else(|error| error.into_inner());
            let hosts = targets
                .iter()
                .filter(|target| query.as_deref().is_none_or(|query| target.matches(query)))
                .map(|target| target.info.clone())
                .collect();
            Ok(Reply::Hosts { hosts })
        }
        Request::Exec { host, command } => {
            let target = find(shared, &host)?;
            let code = shared
                .backend
                .exec(&target, &command, &mut |stream, bytes| {
                    let kind = match stream {
                        ExecStream::Stdout => FrameKind::Stdout,
                        ExecStream::Stderr => FrameKind::Stderr,
                    };
                    write_frame(writer, kind, bytes)
                })?;
            Ok(Reply::Exit { code })
        }
        Request::Upload {
            host,
            source,
            destination,
        } => {
            let target = find(shared, &host)?;
            if !source.is_absolute() {
                return Err(not_absolute(&source));
            }
            shared
                .backend
                .upload(&target, &source, &destination, &mut |counters| {
                    write_json(writer, &Reply::Progress(counters))
                })
                .map(Reply::TransferDone)
        }
        Request::Download {
            host,
            source,
            destination,
        } => {
            let target = find(shared, &host)?;
            if !destination.is_absolute() {
                return Err(not_absolute(&destination));
            }
            shared
                .backend
                .download(&target, &source, &destination, &mut |counters| {
                    write_json(writer, &Reply::Progress(counters))
                })
                .map(Reply::TransferDone)
        }
        // Answered above, before anything was checked.
        Request::Activate => Ok(Reply::Activated),
    }
}

fn find(shared: &Shared, id: &str) -> Result<CliTarget, CliError> {
    shared
        .targets
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .find(|target| target.info.id == id.trim())
        .cloned()
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::HostNotFound,
                format!("没有 ID 为 {id} 的主机：请用 shellrs list 查看"),
            )
        })
}

fn not_absolute(path: &Path) -> CliError {
    CliError::new(
        ErrorCode::BadRequest,
        format!("本地路径必须是绝对路径：{}", path.display()),
    )
}
