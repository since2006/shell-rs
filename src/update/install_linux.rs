//! Linux: an AppImage is one file, renamed over.
//!
//! The new AppImage is copied next to the running one first, so putting it
//! in place is one rename on one volume. The running copy keeps its mount
//! of the old file until it exits; the restart starts `$APPIMAGE`, not the
//! mounted executable, which disappears with the old process.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::error::UpdateError;
use semver::Version;

use super::install::{InstallKind, Installer, Relaunch, Staged, is_stale_update, sibling};
use super::manifest::Release;

pub struct AppImageInstaller {
    kind: InstallKind,
    file: PathBuf,
}

impl AppImageInstaller {
    pub fn new(file: PathBuf) -> Self {
        Self {
            kind: InstallKind::AppImage { file: file.clone() },
            file,
        }
    }

    fn prefix(&self) -> String {
        format!(
            ".{}.update-",
            self.file.file_name().unwrap_or_default().to_string_lossy()
        )
    }
}

impl Installer for AppImageInstaller {
    fn kind(&self) -> &InstallKind {
        &self.kind
    }

    fn stage(&self, package: &Path, release: &Release) -> Result<Staged, UpdateError> {
        let staged = sibling(&self.file, &format!("{}{}", self.prefix(), release.version));
        fs::copy(package, &staged)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
        Ok(Staged {
            version: release.version.clone(),
            path: staged,
        })
    }

    fn apply(&self, staged: &Staged, relaunch: bool) -> Result<Relaunch, UpdateError> {
        fs::rename(&staged.path, &self.file)
            .map_err(|error| UpdateError::Install(error.to_string()))?;
        Ok(if relaunch {
            Relaunch::Restart(self.file.clone())
        } else {
            Relaunch::Nothing
        })
    }

    fn clean_up(&self, current: &Version) {
        let Some(folder) = self.file.parent() else {
            return;
        };
        let prefix = self.prefix();
        for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
            if is_stale_update(&entry.file_name().to_string_lossy(), &prefix, current) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;
    use crate::update::manifest::Asset;

    #[test]
    fn the_appimage_is_replaced_in_one_rename_and_stays_executable() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("ShellRS-x86_64.AppImage");
        fs::write(&file, "old").unwrap();
        let package = folder.path().join("downloaded");
        fs::write(&package, "new").unwrap();
        let release = Release {
            version: Version::new(0, 2, 0),
            published_at: None,
            notes: String::new(),
            notes_url: None,
            asset: Asset {
                urls: Vec::new(),
                size: 3,
                sha256: String::new(),
            },
            installer: None,
        };
        let installer = AppImageInstaller::new(file.clone());
        let staged = installer.stage(&package, &release).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "old");

        assert_eq!(
            installer.apply(&staged, true),
            Ok(Relaunch::Restart(file.clone()))
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);

        fs::write(
            sibling(&file, ".ShellRS-x86_64.AppImage.update-0.1.9"),
            "stale",
        )
        .unwrap();
        // Staged since start and waiting for the restart: kept.
        fs::write(
            sibling(&file, ".ShellRS-x86_64.AppImage.update-0.3.0"),
            "next",
        )
        .unwrap();
        installer.clean_up(&Version::new(0, 2, 0));
        let mut left: Vec<_> = fs::read_dir(folder.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                ".ShellRS-x86_64.AppImage.update-0.3.0",
                "ShellRS-x86_64.AppImage",
                "downloaded"
            ]
        );
    }
}
