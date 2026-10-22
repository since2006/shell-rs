use std::io::{ErrorKind, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use async_channel::Sender;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::i18n::t;

use super::{
    LocalLaunch, LocalShell, LocalTerminalTransportProvider, SharedTerminalTransportFactory,
    TerminalSize, TerminalTransport, TerminalTransportCommand, TerminalTransportEvent,
    TerminalTransportFactory, send_event, strip_launcher_vars,
};

/// Local terminals in a PTY of this computer.
#[derive(Default)]
pub struct LocalPtyTransportProvider;

impl LocalTerminalTransportProvider for LocalPtyTransportProvider {
    fn factory_for(&self, launch: &LocalLaunch) -> SharedTerminalTransportFactory {
        Arc::new(LocalPtyTransportFactory {
            launch: launch.clone(),
        })
    }
}

struct LocalPtyTransportFactory {
    launch: LocalLaunch,
}

impl TerminalTransportFactory for LocalPtyTransportFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(LocalPtyTransport {
            launch: self.launch.clone(),
        })
    }
}

struct LocalPtyTransport {
    launch: LocalLaunch,
}

/// How often at most the shell's folder is read, while it writes: it moves
/// with a `cd`, which a prompt follows.
const LOOK_FOR_THE_FOLDER: Duration = Duration::from_millis(200);

impl TerminalTransport for LocalPtyTransport {
    fn run(
        self: Box<Self>,
        initial_size: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        let LocalLaunch {
            shell,
            directory,
            replay,
        } = self.launch;
        // Before the shell can write anything.
        if let Some(replay) = replay {
            send_event(&events, TerminalTransportEvent::Replay(replay));
        }
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(to_pty_size(initial_size))
            .context(t!("terminal.pty.open_failed"))?;

        let mut command = shell_command(&shell)?;
        // A folder gone since leaves the home folder, as with none.
        if let Some(directory) = directory.filter(|directory| directory.is_dir()) {
            command.cwd(directory);
        }

        let mut child =
            pair.slave
                .spawn_command(command)
                .map_err(|error| match shell.command() {
                    // Why, too: the path or the arguments are the user's.
                    Some(command) => anyhow!(t!(
                        "terminal.pty.command_failed",
                        command = command,
                        error = format!("{error:#}")
                    )),
                    None => error.context(t!("terminal.pty.shell_failed")),
                })?;
        drop(pair.slave);
        let pid = child.process_id();

        let mut reader = pair
            .master
            .try_clone_reader()
            .context(t!("terminal.pty.reader_failed"))?;
        let mut writer = pair
            .master
            .take_writer()
            .context(t!("terminal.pty.writer_failed"))?;

        let output_events = events.clone();
        let wrote = Arc::new(AtomicBool::new(false));
        let output_seen = wrote.clone();
        let reader_thread = thread::Builder::new()
            .name("shellrs-pty-reader".into())
            .spawn(move || {
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            output_seen.store(true, Ordering::Relaxed);
                            if !send_event(
                                &output_events,
                                TerminalTransportEvent::Output(buffer[..count].to_vec()),
                            ) {
                                break;
                            }
                        }
                        Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                        // Unix PTYs commonly report EIO when the slave closes.
                        Err(error) if error.raw_os_error() == Some(5) => break,
                        Err(error) => {
                            send_event(
                                &output_events,
                                TerminalTransportEvent::Failed(
                                    t!("terminal.pty.read_error", error = error).into(),
                                ),
                            );
                            break;
                        }
                    }
                }
            })
            .context(t!("terminal.pty.reader_thread"))?;

        send_event(&events, TerminalTransportEvent::Started);
        let mut shutting_down = false;
        // The shell's folder as last said, and when it was last read.
        let mut folder: Option<PathBuf> = None;
        let mut looked: Option<Instant> = None;
        let exit = loop {
            match commands.recv_timeout(Duration::from_millis(20)) {
                Ok(
                    TerminalTransportCommand::Write(bytes)
                    | TerminalTransportCommand::WriteData(bytes),
                ) if !shutting_down => {
                    if let Err(error) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                        send_event(
                            &events,
                            TerminalTransportEvent::Failed(
                                t!("terminal.pty.write_error", error = error).into(),
                            ),
                        );
                        let _ = child.kill();
                        shutting_down = true;
                    }
                }
                Ok(TerminalTransportCommand::Resize(size)) if !shutting_down => {
                    if let Err(error) = pair.master.resize(to_pty_size(size)) {
                        send_event(
                            &events,
                            TerminalTransportEvent::Failed(
                                t!("terminal.pty.resize_error", error = error).into(),
                            ),
                        );
                    }
                }
                Ok(TerminalTransportCommand::Shutdown)
                | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if !shutting_down {
                        shutting_down = true;
                        let _ = child.kill();
                    }
                }
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            // After the shell wrote, and no sooner than a while after the
            // last look: so the prompt after a `cd` is always looked after.
            if let Some(pid) = pid
                && !shutting_down
                && looked.is_none_or(|at| at.elapsed() >= LOOK_FOR_THE_FOLDER)
                && wrote.swap(false, Ordering::Relaxed)
            {
                looked = Some(Instant::now());
                if let Some(now) = working_directory(pid)
                    && folder.as_ref() != Some(&now)
                {
                    folder = Some(now.clone());
                    send_event(&events, TerminalTransportEvent::WorkingDirectory(now));
                }
            }

            if let Some(status) = child.try_wait().context(t!("terminal.pty.exit_status"))? {
                break status;
            }
        };

        drop(writer);
        drop(pair.master);
        let _ = reader_thread.join();
        if !shutting_down {
            send_event(
                &events,
                TerminalTransportEvent::Exited {
                    code: exit.exit_code(),
                    signal: exit.signal().map(str::to_owned),
                },
            );
        }
        Ok(())
    }
}

/// The command a local terminal's shell runs: the shell the settings name,
/// in the environment ShellRS itself was started in, with the few
/// variables a terminal needs and none of the packaging's.
fn shell_command(shell: &LocalShell) -> Result<CommandBuilder> {
    let mut command = shell.command_builder()?;
    // The packaging that put ShellRS on this computer left variables in
    // the environment; they are none of the user's shell's business.
    strip_launcher_vars(&mut command);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("TERM_PROGRAM", "ShellRS");
    Ok(command)
}

/// The folder process `pid` is in, where the system tells it.
#[cfg(target_os = "linux")]
fn working_directory(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The folder process `pid` is in, where the system tells it.
#[cfg(target_os = "macos")]
fn working_directory(pid: u32) -> Option<PathBuf> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt as _;

    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: plain data, for which all zeroes is a value.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the buffer is `info`, and `size` is how big it is.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    let path = &info.pvi_cdir.vip_path;
    // SAFETY: the path's rows of characters lie one after another.
    let bytes = unsafe {
        std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), std::mem::size_of_val(path))
    };
    let path = CStr::from_bytes_until_nul(bytes).ok()?;
    (!path.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

/// Windows has no way to read another process's folder: its terminals
/// open in the home folder again.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn working_directory(_: u32) -> Option<PathBuf> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::terminal::environment::LAUNCHER_VARS;

    /// `/bin/sh -c script`, through the command line the settings hold.
    fn shell_transport(script: &str) -> Box<dyn TerminalTransport> {
        let command = format!("/bin/sh -c {}", crate::terminal::quote_word(script));
        LocalPtyTransportProvider
            .factory_for(&LocalLaunch::new(LocalShell::new(&command)))
            .create()
    }

    fn drain_until_exit(
        events: &async_channel::Receiver<TerminalTransportEvent>,
        commands: &mpsc::Sender<TerminalTransportCommand>,
    ) -> (String, Option<u32>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        let mut code = None;
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(TerminalTransportEvent::Output(bytes)) => output.extend(bytes),
                Ok(TerminalTransportEvent::Exited {
                    code: exit_code, ..
                }) => {
                    code = Some(exit_code);
                    break;
                }
                Ok(
                    TerminalTransportEvent::Started
                    | TerminalTransportEvent::Failed(_)
                    | TerminalTransportEvent::Prompt(_)
                    | TerminalTransportEvent::HostOsDetected(_)
                    | TerminalTransportEvent::Latency(_)
                    | TerminalTransportEvent::WorkingDirectory(_)
                    | TerminalTransportEvent::Replay(_),
                ) => {}
                Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
                Err(async_channel::TryRecvError::Closed) => break,
            }
        }
        if code.is_none() {
            let _ = commands.send(TerminalTransportCommand::Shutdown);
        }
        (String::from_utf8_lossy(&output).into_owned(), code)
    }

    /// The command a local terminal's shell runs in: the environment
    /// ShellRS was started in, without what the packaging left in it.
    #[test]
    fn the_shell_starts_without_the_packaging() {
        let command = shell_command(&LocalShell::new("/bin/sh")).unwrap();
        for name in LAUNCHER_VARS {
            assert_eq!(command.get_env(name), None, "{name} is left in");
        }
        // The rest of the environment is still there.
        assert!(command.get_env("PATH").is_some());
    }

    /// The same test in a child process whose environment holds what the
    /// AppImage runtime leaves in it.
    ///
    /// The parent cannot put a variable into its own environment while
    /// the other tests run, and the variable has to be in the environment
    /// before the shell starts for the shell to find it at all.
    ///
    /// The shell is zsh, the one that takes `ARGV0` for a special
    /// parameter and writes it into the argv[0] of every command it runs.
    #[test]
    fn the_shell_does_not_see_the_packaging() {
        const CHILD: &str = "SHELLRS_TEST_WITH_THE_PACKAGING";
        const APPIMAGE: &str = "/tmp/ShellRS-0.1.14-linux-x86_64.AppImage";
        if std::env::var_os(CHILD).is_some() {
            // The child: the shell runs and says what it sees.
            let shell = if PathBuf::from("/bin/zsh").exists() {
                "/bin/zsh"
            } else {
                "/bin/sh"
            };
            let script = "printf '%s' \"${ARGV0-gone}|${APPIMAGE-gone}|${APPDIR-gone}|${OWD-gone}\"; \
                 printf '|argv0=%s' \"$0\"; \
                 if command -v rustup >/dev/null 2>&1; then \
                 printf '|rustup=%s' \"$(rustup --version 2>&1 | head -1)\"; fi";
            let (commands, command_receiver) = mpsc::channel();
            let (events, event_receiver) = async_channel::unbounded();
            let transport = shell_transport(&format!(
                "{shell} -c {}",
                crate::terminal::quote_word(script)
            ));
            let worker = thread::spawn(move || {
                transport.run(TerminalSize::DEFAULT, command_receiver, events)
            });
            let (output, code) = drain_until_exit(&event_receiver, &commands);
            assert_eq!(code, Some(0), "{output}");
            // What the shell saw, said out loud: this is the acceptance,
            // readable without a window.
            println!("the shell saw: {output}");
            assert!(
                output.starts_with("gone|gone|gone|gone|"),
                "the shell sees the packaging: {output}"
            );
            // The shell's own name, not the AppImage's: zsh would take
            // ARGV0 for its argv[0] if it were there.
            let argv0 = output
                .split_once("|argv0=")
                .and_then(|(_, rest)| rest.split('|').next())
                .unwrap_or_default();
            assert!(
                !argv0.contains("ShellRS") && !argv0.contains("AppImage"),
                "the shell's argv[0] is the AppImage's: {output}"
            );
            // And rustup, which is one binary that tells its tools apart
            // by its own argv[0], still knows what it is.
            assert!(
                !output.contains("unknown proxy name"),
                "rustup does not know itself: {output}"
            );
            worker.join().unwrap().unwrap();
            // The parent looks for this line, to know the test really ran.
            println!("the shell did not see the packaging");
            return;
        }
        // The parent: the same test, in a child whose environment has them.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal::local_pty::tests::the_shell_does_not_see_the_packaging",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("ARGV0", APPIMAGE)
            .env("APPIMAGE", APPIMAGE)
            .env("APPDIR", "/tmp/.mount_ShellRSxxxx")
            .env("OWD", "/tmp")
            .output()
            .expect("the test runs again");
        let text = String::from_utf8_lossy(&output.stdout);
        // What the shell saw, said out loud: this is the acceptance,
        // readable without a window.
        for line in text.lines() {
            if let Some(report) = line.trim().strip_prefix("the shell saw: ") {
                println!("{report}");
            }
        }
        assert!(output.status.success(), "the shell saw them:\n{text}");
        assert!(
            text.contains("the shell did not see the packaging"),
            "the test did not run in the child:\n{text}"
        );
    }

    #[test]
    fn runs_resizes_writes_and_reaps_a_deterministic_shell() {
        let (commands, command_receiver) = mpsc::channel();
        let (events, event_receiver) = async_channel::unbounded();
        let transport = shell_transport(
            "printf 'ready\\n'; read value; printf 'size:'; stty size; printf 'got:%s\\n' \"$value\"; exit 7",
        );
        let worker = thread::spawn(move || {
            transport.run(TerminalSize::new(80, 24, 8, 16), command_receiver, events)
        });

        commands
            .send(TerminalTransportCommand::Resize(TerminalSize::new(
                100, 31, 9, 18,
            )))
            .unwrap();
        commands
            .send(TerminalTransportCommand::Write(b"hello\n".to_vec()))
            .unwrap();

        let (output, code) = drain_until_exit(&event_receiver, &commands);
        assert_eq!(code, Some(7));
        assert!(output.contains("ready"));
        assert!(output.contains("size:31 100"));
        assert!(output.contains("got:hello"));
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn a_command_that_cannot_start_says_which() {
        let (_commands, command_receiver) = mpsc::channel();
        let (events, _event_receiver) = async_channel::unbounded();
        let error = LocalPtyTransportProvider
            .factory_for(&LocalLaunch::new(LocalShell::new("/no/such/shell -l")))
            .create()
            .run(TerminalSize::DEFAULT, command_receiver, events)
            .unwrap_err()
            .to_string();
        assert!(error.contains("无法启动 /no/such/shell -l"), "{error}");
    }

    /// `/bin/sh -c script` started in `directory`.
    fn shell_in(script: &str, directory: &std::path::Path) -> Box<dyn TerminalTransport> {
        let launch = LocalLaunch {
            directory: Some(directory.to_path_buf()),
            ..LocalLaunch::new(LocalShell::new(&format!(
                "/bin/sh -c {}",
                crate::terminal::quote_word(script)
            )))
        };
        LocalPtyTransportProvider.factory_for(&launch).create()
    }

    #[test]
    fn a_saved_terminal_starts_in_its_folder_or_home_when_it_is_gone() {
        let folder = tempfile::tempdir().unwrap();
        let script = "printf 'in:%s\\n' \"$(pwd -P)\"";
        for (directory, starts_in) in [
            (folder.path().to_path_buf(), Some(folder.path())),
            ("/no/such/folder".into(), None),
        ] {
            let (commands, command_receiver) = mpsc::channel();
            let (events, event_receiver) = async_channel::unbounded();
            let transport = shell_in(script, &directory);
            let worker = thread::spawn(move || {
                transport.run(TerminalSize::DEFAULT, command_receiver, events)
            });
            let (output, code) = drain_until_exit(&event_receiver, &commands);
            assert_eq!(code, Some(0), "{output}");
            match starts_in {
                Some(folder) => {
                    let folder = folder.canonicalize().unwrap();
                    assert!(
                        output.contains(&format!("in:{}", folder.display())),
                        "{output}"
                    );
                }
                None => assert!(!output.contains("/no/such"), "{output}"),
            }
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn what_a_terminal_kept_comes_before_its_shell() {
        let (commands, command_receiver) = mpsc::channel();
        let (events, event_receiver) = async_channel::unbounded();
        let launch = LocalLaunch {
            replay: Some(b"$ ls\r\nsrc\r\n".to_vec()),
            ..LocalLaunch::new(LocalShell::new("/bin/sh -c 'printf now'"))
        };
        let transport = LocalPtyTransportProvider.factory_for(&launch).create();
        let worker =
            thread::spawn(move || transport.run(TerminalSize::DEFAULT, command_receiver, events));
        assert_eq!(
            event_receiver.recv_blocking().unwrap(),
            TerminalTransportEvent::Replay(b"$ ls\r\nsrc\r\n".to_vec())
        );
        let (output, code) = drain_until_exit(&event_receiver, &commands);
        assert_eq!(code, Some(0));
        assert!(output.contains("now"), "{output}");
        worker.join().unwrap().unwrap();
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn the_shells_folder_is_told_as_it_moves() {
        let start = tempfile::tempdir().unwrap();
        let next = tempfile::tempdir().unwrap();
        let script = format!(
            "printf 'a\\n'; sleep 0.5; cd {}; printf 'b\\n'; sleep 0.5",
            crate::terminal::quote_word(&next.path().display().to_string())
        );
        let (commands, command_receiver) = mpsc::channel();
        let (events, event_receiver) = async_channel::unbounded();
        let transport = shell_in(&script, start.path());
        let worker =
            thread::spawn(move || transport.run(TerminalSize::DEFAULT, command_receiver, events));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut folders = Vec::new();
        while Instant::now() < deadline {
            match event_receiver.try_recv() {
                Ok(TerminalTransportEvent::WorkingDirectory(folder)) => folders.push(folder),
                Ok(TerminalTransportEvent::Exited { .. })
                | Err(async_channel::TryRecvError::Closed) => break,
                Ok(_) => {}
                Err(async_channel::TryRecvError::Empty) => thread::sleep(Duration::from_millis(5)),
            }
        }
        let _ = commands.send(TerminalTransportCommand::Shutdown);
        worker.join().unwrap().unwrap();
        assert_eq!(
            folders,
            [
                start.path().canonicalize().unwrap(),
                next.path().canonicalize().unwrap()
            ]
        );
    }

    #[test]
    fn shutdown_terminates_and_reaps_the_child() {
        let (commands, command_receiver) = mpsc::channel();
        let (events, _event_receiver) = async_channel::unbounded();
        let transport = shell_transport("printf 'ready\\n'; sleep 30");
        let worker =
            thread::spawn(move || transport.run(TerminalSize::DEFAULT, command_receiver, events));

        commands.send(TerminalTransportCommand::Shutdown).unwrap();
        worker.join().unwrap().unwrap();
    }
}

fn to_pty_size(size: TerminalSize) -> PtySize {
    let rows = size.rows().min(u16::MAX as usize) as u16;
    let cols = size.columns().min(u16::MAX as usize) as u16;
    PtySize {
        rows,
        cols,
        pixel_width: size.cell_width().saturating_mul(cols),
        pixel_height: size.cell_height().saturating_mul(rows),
    }
}
