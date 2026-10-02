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

/// Whether this machine's `sh` reads `script` without a syntax error.
pub fn sh_accepts(script: &str) -> bool {
    let mut command = Command::new("sh");
    command.args(["-n", "-c", script]);
    let _forks = no_forks();
    command.status().expect("sh runs").success()
}
