//! Windows: the setup program updates its own install.
//!
//! ShellRS is installed per user by an Inno Setup program, which owns the
//! files, the shortcuts and the entry in 应用和功能. An update runs the new
//! version's setup silently, the same way the person ran the first one. A
//! running program cannot be overwritten, so a hidden PowerShell waits for
//! ShellRS to exit, runs the setup, and starts ShellRS again when asked to.
//! When the setup fails it rolls back, and the restart brings up the old
//! version, which then says the update did not finish.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use semver::Version;

use super::error::UpdateError;
use super::install::{InstallKind, Installer, Launch, Relaunch, Staged};
use super::manifest::Release;
use crate::i18n::t;

/// Waits for ShellRS to exit, runs the setup, then starts ShellRS again if
/// asked. Values come in through the environment, not the script's text,
/// so no path needs quoting.
const WAITER: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
$waitFor = [int]$env:SHELLRS_UPDATE_PID
while (Get-Process -Id $waitFor) { Start-Sleep -Milliseconds 100 }
Start-Process -FilePath $env:SHELLRS_UPDATE_SETUP -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/SP-' -Wait
if ($env:SHELLRS_UPDATE_RELAUNCH -eq '1') { Start-Process -FilePath $env:SHELLRS_UPDATE_APP }
"#;

pub struct SetupInstaller {
    kind: InstallKind,
    exe: PathBuf,
}

impl SetupInstaller {
    pub fn new(exe: PathBuf) -> Self {
        Self {
            kind: InstallKind::WindowsSetup { exe: exe.clone() },
            exe,
        }
    }
}

/// The hidden PowerShell that installs `setup` once process `pid` is gone.
pub fn waiter(pid: u32, setup: &Path, app: &Path, relaunch: bool) -> Launch {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
    let program = PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let env = |name: &str, value: OsString| (OsString::from(name), value);
    Launch {
        program,
        args: [
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            WAITER,
        ]
        .map(OsString::from)
        .to_vec(),
        envs: vec![
            env("SHELLRS_UPDATE_PID", pid.to_string().into()),
            env("SHELLRS_UPDATE_SETUP", setup.as_os_str().to_owned()),
            env("SHELLRS_UPDATE_APP", app.as_os_str().to_owned()),
            env(
                "SHELLRS_UPDATE_RELAUNCH",
                if relaunch { "1" } else { "0" }.into(),
            ),
        ],
    }
}

impl Installer for SetupInstaller {
    fn kind(&self) -> &InstallKind {
        &self.kind
    }

    /// The verified setup program is run as it is.
    fn stage(&self, package: &Path, release: &Release) -> Result<Staged, UpdateError> {
        Ok(Staged {
            version: release.version.clone(),
            path: package.to_path_buf(),
        })
    }

    fn apply(&self, staged: &Staged, relaunch: bool) -> Result<Relaunch, UpdateError> {
        if !staged.path.is_file() {
            return Err(UpdateError::Install(
                t!("update.install.setup_missing").to_string(),
            ));
        }
        Ok(Relaunch::Hand(waiter(
            std::process::id(),
            &staged.path,
            &self.exe,
            relaunch,
        )))
    }

    /// The setup program removes what the previous version left.
    // The package waits in the downloads folder, which the updater tidies.
    fn clean_up(&self, _: &Version) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_waiter_runs_the_installer_then_relaunches() {
        let setup = Path::new(r"C:\Users\me\AppData\Local\shellrs\updates\0.2.0\setup.exe");
        let app = Path::new(r"C:\Users\me\AppData\Local\Programs\ShellRS\shellrs.exe");
        let launch = waiter(4242, setup, app, true);
        // A string compare: on the Mac running the tests, backslashes are
        // not separators.
        let program = launch.program.to_string_lossy().replace('/', "\\");
        assert!(
            program.ends_with(r"\System32\WindowsPowerShell\v1.0\powershell.exe"),
            "{program}"
        );
        assert_eq!(
            launch.args.last().map(|script| script.to_str().unwrap()),
            Some(WAITER)
        );
        let env = |name: &str| {
            launch
                .envs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(env("SHELLRS_UPDATE_PID"), Some("4242".into()));
        assert_eq!(env("SHELLRS_UPDATE_SETUP"), Some(setup.as_os_str().into()));
        assert_eq!(env("SHELLRS_UPDATE_APP"), Some(app.as_os_str().into()));
        assert_eq!(env("SHELLRS_UPDATE_RELAUNCH"), Some("1".into()));
        assert_eq!(
            waiter(1, setup, app, false)
                .envs
                .iter()
                .find(|(key, _)| key == "SHELLRS_UPDATE_RELAUNCH")
                .map(|(_, value)| value.clone()),
            Some("0".into())
        );
        // Silent, and without the setup's own 继续吗？ question.
        for switch in ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"] {
            assert!(WAITER.contains(switch), "{switch}");
        }
    }

    #[test]
    fn a_missing_setup_is_not_handed_over() {
        let installer = SetupInstaller::new(PathBuf::from(r"C:\ShellRS\shellrs.exe"));
        let staged = Staged {
            version: semver::Version::new(0, 2, 0),
            path: PathBuf::from("/nowhere/setup.exe"),
        };
        assert!(matches!(
            installer.apply(&staged, true),
            Err(UpdateError::Install(_))
        ));
    }
}
