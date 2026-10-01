//! macOS: the app bundle is replaced whole.
//!
//! The package is the stapled bundle zipped by `ditto`. It is unpacked
//! next to the installed bundle, into `.ShellRS.app.update-<version>`, so
//! that putting it in place is two renames on one volume: the running
//! bundle moves aside to `.ShellRS.app.old-<id>` and the new one takes its
//! name. The running process keeps its files open, and its assets are built
//! into the binary, so it carries on until the restart. The old bundle is
//! removed by the next version's clean-up.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use super::build_info::APPLE_TEAM_ID;
use super::error::UpdateError;
use super::install::{InstallKind, Installer, Relaunch, Staged, sibling};
use super::manifest::Release;

/// The macOS tools staging relies on, behind a seam for the tests.
pub trait MacTools: Send + Sync {
    /// Unpack a zip made by `ditto -c -k --keepParent` into `into`.
    fn unpack(&self, zip: &Path, into: &Path) -> Result<(), UpdateError>;

    /// The bundle's `CFBundleShortVersionString`.
    fn bundle_version(&self, bundle: &Path) -> Result<String, UpdateError>;

    /// Whether the bundle's signature is intact and, given a team, made by
    /// that team's Developer ID.
    fn check_signature(&self, bundle: &Path, team: Option<&str>) -> Result<(), UpdateError>;
}

/// `ditto`, `plutil` and `codesign`.
pub struct SystemTools;

impl SystemTools {
    fn run(program: &str, args: &[&OsStr]) -> Result<String, UpdateError> {
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|error| UpdateError::Install(format!("{program}：{error}")))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(UpdateError::Install(format!(
                "{program}：{}",
                stderr.trim()
            )))
        }
    }
}

impl MacTools for SystemTools {
    fn unpack(&self, zip: &Path, into: &Path) -> Result<(), UpdateError> {
        Self::run(
            "/usr/bin/ditto",
            &[
                "-x".as_ref(),
                "-k".as_ref(),
                zip.as_os_str(),
                into.as_os_str(),
            ],
        )
        .map(drop)
    }

    fn bundle_version(&self, bundle: &Path) -> Result<String, UpdateError> {
        let plist = bundle.join("Contents/Info.plist");
        Self::run(
            "/usr/bin/plutil",
            &[
                "-extract".as_ref(),
                "CFBundleShortVersionString".as_ref(),
                "raw".as_ref(),
                "-o".as_ref(),
                "-".as_ref(),
                plist.as_os_str(),
            ],
        )
    }

    fn check_signature(&self, bundle: &Path, team: Option<&str>) -> Result<(), UpdateError> {
        let requirement = team.map(|team| {
            format!("-R=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"")
        });
        let mut args: Vec<&OsStr> =
            vec!["--verify".as_ref(), "--deep".as_ref(), "--strict".as_ref()];
        if let Some(requirement) = &requirement {
            args.push(requirement.as_ref());
        }
        args.push(bundle.as_os_str());
        Self::run("/usr/bin/codesign", &args)
            .map(drop)
            .map_err(|_| UpdateError::Install("新版本的签名无效或不是 ShellRS 的开发者".into()))
    }
}

pub struct MacInstaller {
    kind: InstallKind,
    bundle: PathBuf,
    tools: Arc<dyn MacTools>,
    /// The Developer ID team a new bundle must be signed by: the one this
    /// release was signed by.
    team: Option<String>,
}

impl MacInstaller {
    pub fn new(bundle: PathBuf, tools: Arc<dyn MacTools>) -> Self {
        Self {
            kind: InstallKind::MacBundle {
                bundle: bundle.clone(),
            },
            bundle,
            tools,
            team: APPLE_TEAM_ID.map(String::from),
        }
    }

    pub fn with_team(mut self, team: Option<&str>) -> Self {
        self.team = team.map(String::from);
        self
    }

    /// `ShellRS.app`, whatever the user named it.
    fn name(&self) -> String {
        self.bundle
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    fn staging(&self, release: &Release) -> PathBuf {
        sibling(
            &self.bundle,
            &format!(".{}.update-{}", self.name(), release.version),
        )
    }

    fn unpack(
        &self,
        package: &Path,
        release: &Release,
        staging: &Path,
    ) -> Result<PathBuf, UpdateError> {
        self.tools.unpack(package, staging)?;
        let app = fs::read_dir(staging)?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|extension| extension == "app"))
            .ok_or_else(|| UpdateError::Install("更新包里没有应用程序".into()))?;
        if self.tools.bundle_version(&app)? != release.version.to_string() {
            return Err(UpdateError::Install("更新包的版本与更新清单不符".into()));
        }
        self.tools.check_signature(&app, self.team.as_deref())?;
        Ok(app)
    }
}

impl Installer for MacInstaller {
    fn kind(&self) -> &InstallKind {
        &self.kind
    }

    fn stage(&self, package: &Path, release: &Release) -> Result<Staged, UpdateError> {
        let staging = self.staging(release);
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        match self.unpack(package, release, &staging) {
            Ok(path) => Ok(Staged {
                version: release.version.clone(),
                path,
            }),
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                Err(error)
            }
        }
    }

    fn apply(&self, staged: &Staged, relaunch: bool) -> Result<Relaunch, UpdateError> {
        let install = |error: std::io::Error| UpdateError::Install(error.to_string());
        let aside = sibling(
            &self.bundle,
            &format!(".{}.old-{}", self.name(), uuid::Uuid::new_v4().simple()),
        );
        fs::rename(&self.bundle, &aside).map_err(install)?;
        if let Err(error) = fs::rename(&staged.path, &self.bundle) {
            let _ = fs::rename(&aside, &self.bundle);
            return Err(install(error));
        }
        if let Some(staging) = staged.path.parent() {
            let _ = fs::remove_dir(staging);
        }
        Ok(if relaunch {
            Relaunch::Restart(self.bundle.clone())
        } else {
            Relaunch::Nothing
        })
    }

    fn clean_up(&self) {
        let Some(folder) = self.bundle.parent() else {
            return;
        };
        let (old, update) = (
            format!(".{}.old-", self.name()),
            format!(".{}.update-", self.name()),
        );
        for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&old) || name.starts_with(&update) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use semver::Version;

    use super::*;
    use crate::update::manifest::Asset;

    /// Unpacks a fake bundle whose version and team are whatever the test
    /// says, and checks the team against the one asked for.
    struct FakeTools {
        version: String,
        team: Option<String>,
        asked_for: Mutex<Vec<Option<String>>>,
    }

    impl FakeTools {
        fn new(version: &str, team: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                version: version.into(),
                team: team.map(Into::into),
                asked_for: Mutex::default(),
            })
        }
    }

    impl MacTools for FakeTools {
        fn unpack(&self, _: &Path, into: &Path) -> Result<(), UpdateError> {
            let contents = into.join("ShellRS.app/Contents");
            fs::create_dir_all(&contents)?;
            fs::write(contents.join("version"), &self.version)?;
            Ok(())
        }

        fn bundle_version(&self, bundle: &Path) -> Result<String, UpdateError> {
            Ok(fs::read_to_string(bundle.join("Contents/version"))?)
        }

        fn check_signature(&self, _: &Path, team: Option<&str>) -> Result<(), UpdateError> {
            self.asked_for.lock().unwrap().push(team.map(Into::into));
            match (team, &self.team) {
                (Some(wanted), Some(signed)) if wanted != signed => {
                    Err(UpdateError::Install("another team".into()))
                }
                _ => Ok(()),
            }
        }
    }

    fn release(version: &str) -> Release {
        Release {
            version: Version::parse(version).unwrap(),
            published_at: None,
            notes: String::new(),
            notes_url: None,
            asset: Asset {
                urls: Vec::new(),
                size: 0,
                sha256: String::new(),
            },
            installer: None,
        }
    }

    /// `/Applications`-like folder holding a running 0.1.0 bundle.
    fn installed() -> (tempfile::TempDir, PathBuf) {
        let folder = tempfile::tempdir().unwrap();
        let bundle = folder.path().join("ShellRS.app");
        fs::create_dir_all(bundle.join("Contents")).unwrap();
        fs::write(bundle.join("Contents/version"), "0.1.0").unwrap();
        (folder, bundle)
    }

    fn version_at(bundle: &Path) -> String {
        fs::read_to_string(bundle.join("Contents/version")).unwrap()
    }

    fn names_in(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_staged_bundle_replaces_the_running_one_and_the_old_one_waits_for_cleanup() {
        let (folder, bundle) = installed();
        let installer = MacInstaller::new(bundle.clone(), FakeTools::new("0.2.0", None));
        let staged = installer
            .stage(Path::new("package.zip"), &release("0.2.0"))
            .unwrap();
        assert!(
            staged
                .path
                .starts_with(folder.path().join(".ShellRS.app.update-0.2.0"))
        );
        assert_eq!(
            version_at(&bundle),
            "0.1.0",
            "staging leaves the running copy alone"
        );

        assert_eq!(
            installer.apply(&staged, true),
            Ok(Relaunch::Restart(bundle.clone()))
        );
        assert_eq!(version_at(&bundle), "0.2.0");
        let names = names_in(folder.path());
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(names[0].starts_with(".ShellRS.app.old-"), "{names:?}");

        installer.clean_up();
        assert_eq!(names_in(folder.path()), ["ShellRS.app"]);
    }

    #[test]
    fn a_failed_swap_puts_the_old_bundle_back() {
        let (folder, bundle) = installed();
        let installer = MacInstaller::new(bundle.clone(), FakeTools::new("0.2.0", None));
        let staged = installer
            .stage(Path::new("package.zip"), &release("0.2.0"))
            .unwrap();
        fs::remove_dir_all(&staged.path).unwrap();

        assert!(matches!(
            installer.apply(&staged, true),
            Err(UpdateError::Install(_))
        ));
        assert_eq!(version_at(&bundle), "0.1.0");
        installer.clean_up();
        assert_eq!(names_in(folder.path()), ["ShellRS.app"]);
    }

    #[test]
    fn installing_on_quit_does_not_restart() {
        let (_folder, bundle) = installed();
        let installer = MacInstaller::new(bundle.clone(), FakeTools::new("0.2.0", None));
        let staged = installer
            .stage(Path::new("package.zip"), &release("0.2.0"))
            .unwrap();
        assert_eq!(installer.apply(&staged, false), Ok(Relaunch::Nothing));
        assert_eq!(version_at(&bundle), "0.2.0");
    }

    #[test]
    fn a_package_of_another_version_is_refused() {
        let (folder, bundle) = installed();
        let installer = MacInstaller::new(bundle, FakeTools::new("0.1.5", None));
        assert!(matches!(
            installer.stage(Path::new("package.zip"), &release("0.2.0")),
            Err(UpdateError::Install(_))
        ));
        assert_eq!(
            names_in(folder.path()),
            ["ShellRS.app"],
            "no staging left behind"
        );
    }

    #[test]
    fn a_bundle_signed_by_another_team_is_refused() {
        let (folder, bundle) = installed();
        let tools = FakeTools::new("0.2.0", Some("SOMEONEELSE"));
        let installer = MacInstaller::new(bundle.clone(), tools.clone()).with_team(Some("SHELLRS"));
        assert!(matches!(
            installer.stage(Path::new("package.zip"), &release("0.2.0")),
            Err(UpdateError::Install(_))
        ));
        assert_eq!(
            *tools.asked_for.lock().unwrap(),
            [Some("SHELLRS".to_string())]
        );
        assert_eq!(names_in(folder.path()), ["ShellRS.app"]);

        let ours = FakeTools::new("0.2.0", Some("SHELLRS"));
        let installer = MacInstaller::new(bundle, ours).with_team(Some("SHELLRS"));
        assert!(
            installer
                .stage(Path::new("package.zip"), &release("0.2.0"))
                .is_ok()
        );
    }

    /// The real `ditto`, `plutil` and `codesign` on a small bundle signed
    /// ad hoc: unpacked, checked and swapped in like a release.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_tools_unpack_check_and_swap_a_real_bundle() {
        let work = tempfile::tempdir().unwrap();
        let bundle_at = |folder: &Path, version: &str| {
            let app = folder.join("ShellRS.app");
            fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
            fs::copy("/usr/bin/true", app.join("Contents/MacOS/shellrs")).unwrap();
            fs::write(
                app.join("Contents/Info.plist"),
                format!(
                    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>shellrs</string>
<key>CFBundleIdentifier</key><string>com.shellrs.test</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
</dict></plist>"#
                ),
            )
            .unwrap();
            let signed = Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&app)
                .output()
                .unwrap();
            assert!(signed.status.success(), "{signed:?}");
            app
        };
        let installed = work.path().join("Applications");
        let bundle = bundle_at(&installed, "0.1.0");
        let built = work.path().join("built");
        let new = bundle_at(&built, "0.2.0");
        let package = work.path().join("ShellRS-0.2.0-macos-universal.app.zip");
        let zipped = Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&new)
            .arg(&package)
            .output()
            .unwrap();
        assert!(zipped.status.success(), "{zipped:?}");

        let installer = MacInstaller::new(bundle.clone(), Arc::new(SystemTools)).with_team(None);
        assert!(matches!(
            installer.stage(&package, &release("0.3.0")),
            Err(UpdateError::Install(_))
        ));
        let staged = installer.stage(&package, &release("0.2.0")).unwrap();
        assert_eq!(
            installer.apply(&staged, true),
            Ok(Relaunch::Restart(bundle.clone()))
        );
        assert_eq!(SystemTools.bundle_version(&bundle).unwrap(), "0.2.0");
        assert_eq!(SystemTools.check_signature(&bundle, None), Ok(()));
        // Signed ad hoc, so by no team.
        assert!(
            SystemTools
                .check_signature(&bundle, Some("SHELLRS"))
                .is_err()
        );
        installer.clean_up();
        assert_eq!(names_in(&installed), ["ShellRS.app"]);
    }
}
