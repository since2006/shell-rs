//! What the unit tests share: starting processes without upsetting the
//! tests that need a closed socket to stay closed.

use std::process::Command;
use std::sync::{Mutex, MutexGuard};

static FORKS: Mutex<()> = Mutex::new(());

/// Hold while starting a process, or while making a socket that must refuse
/// connections once it is closed.
///
/// macOS makes a socket and marks it not to be handed to started programs
/// in two steps. A process another test starts in between, on its own
/// thread, gets a copy of the socket and keeps it open as long as it runs:
/// a listener the test has closed still takes connections. A process gets
/// its copies only as it starts, so holding this around starting one and
/// around making the socket keeps the two apart.
pub fn no_forks() -> MutexGuard<'static, ()> {
    FORKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run `script` with this machine's `sh`, with `PATH` set to `path` when
/// given. Unix only, like the tests that run scripts for real.
#[cfg(unix)]
pub fn sh(script: &str, path: Option<&str>) -> std::process::Output {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let _forks = no_forks();
    command.output().expect("sh runs")
}

/// A program on a terminal of its own: what it prints comes in on `output`.
/// Unix only, like `sh`.
#[cfg(unix)]
pub struct Pty {
    pub output: std::sync::mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

#[cfg(unix)]
impl Pty {
    pub fn write(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("pty takes input");
        self.writer.flush().expect("pty takes input");
    }
}

#[cfg(unix)]
impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Start `argv` on a new terminal of 120 × 40.
#[cfg(unix)]
pub fn pty(argv: &[&str]) -> Pty {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty opens");
    let mut command = CommandBuilder::from_argv(argv.iter().map(Into::into).collect());
    command.env("TERM", "xterm-256color");
    let child = {
        let _forks = no_forks();
        pair.slave.spawn_command(command).expect("program starts")
    };
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("pty reads");
    let writer = pair.master.take_writer().expect("pty writes");
    let (sender, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        while let Ok(count) = std::io::Read::read(&mut reader, &mut buffer) {
            if count == 0 || sender.send(buffer[..count].to_vec()).is_err() {
                break;
            }
        }
    });
    Pty {
        output,
        writer,
        child,
        _master: pair.master,
    }
}

/// Whether this machine's `sh` reads `script` without a syntax error.
pub fn sh_accepts(script: &str) -> bool {
    let mut command = Command::new("sh");
    command.args(["-n", "-c", script]);
    let _forks = no_forks();
    command.status().expect("sh runs").success()
}
