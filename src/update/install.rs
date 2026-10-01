//! Putting a new version in place.
//!
//! Who replaces ShellRS depends on how it was installed: a macOS bundle is
//! swapped whole, the Windows setup program updates its own install, an
//! AppImage is one file renamed over. Anything else (a development build, a
//! copy still on its disk image, a distribution package) still hears about
//! new versions but is sent to the download page.
//!
//! Staging does the slow part in the background right after the download
//! is verified; applying is renames or starting a process, quick enough for
//! the moment ShellRS restarts or quits.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use semver::Version;

use super::error::UpdateError;
use super::manifest::Release;

/// Where this ShellRS runs from, as far as updating goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// A macOS app bundle in a folder the user can write to.
    MacBundle {
        bundle: PathBuf,
    },
    /// Installed by ShellRS's setup program, which also updates it.
    WindowsSetup {
        exe: PathBuf,
    },
    /// An AppImage file in a folder the user can write to.
    AppImage {
        file: PathBuf,
    },
    Unsupported(Unsupported),
}

/// Why this copy of ShellRS cannot replace itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Built by hand: no release replaces it.
    DevelopmentBuild,
    /// macOS runs it from a read-only copy because it was never moved out of
    /// the folder it was downloaded to.
    Translocated,
    /// Still on the disk image it came on.
    DiskImage,
    /// The folder it is in cannot be written to.
    ReadOnly(PathBuf),
    /// macOS, not from an app bundle.
    NotBundled,
    /// Windows, not installed by the setup program.
    NotInstalled,
    /// Linux, not an AppImage: a distribution package or a build of its own.
    NotAppImage,
    /// No package of ShellRS for this platform.
    Platform,
}

impl Unsupported {
    /// Why, for 设置 › 关于 and the update dialog.
    pub fn reason(&self) -> String {
        match self {
            Self::DevelopmentBuild => "开发构建，不检查更新".into(),
            Self::Translocated => {
                "ShellRS 不在“应用程序”文件夹中，无法自动更新。请把它移到“应用程序”文件夹后重新打开"
                    .into()
            }
            Self::DiskImage => {
                "ShellRS 正在从磁盘映像运行，无法自动更新。请先把它拖到“应用程序”文件夹".into()
            }
            Self::ReadOnly(folder) => {
                format!("没有权限写入 {}，无法自动更新", folder.display())
            }
            Self::NotBundled => "ShellRS 不是从应用程序包运行的，无法自动更新".into(),
            Self::NotInstalled => "这份 ShellRS 不是用安装程序安装的，无法自动更新".into(),
            Self::NotAppImage => "这份 ShellRS 不是 AppImage，请用安装它的方式更新".into(),
            Self::Platform => "ShellRS 没有为这个系统提供更新".into(),
        }
    }
}

/// What this machine looks like, for [`detect`]. Separate from the real
/// file system so that every case can be tested anywhere.
pub struct Surroundings<'a> {
    /// `std::env::consts::OS`.
    pub os: &'a str,
    /// The running executable.
    pub exe: &'a Path,
    /// macOS: the app bundle as the system sees it (`cx.app_path()`).
    pub bundle: Option<&'a Path>,
    /// Linux: `$APPIMAGE`, set by the AppImage runtime.
    pub appimage: Option<&'a Path>,
    pub writable: &'a dyn Fn(&Path) -> bool,
    pub exists: &'a dyn Fn(&Path) -> bool,
}

/// How this copy of ShellRS can be updated.
pub fn detect(at: &Surroundings) -> InstallKind {
    match at.os {
        "macos" => {
            let Some(bundle) = at.bundle.filter(|bundle| {
                bundle
                    .extension()
                    .is_some_and(|extension| extension == "app")
            }) else {
                return InstallKind::Unsupported(Unsupported::NotBundled);
            };
            if bundle
                .components()
                .any(|part| part.as_os_str() == "AppTranslocation")
            {
                return InstallKind::Unsupported(Unsupported::Translocated);
            }
            let folder = bundle.parent().unwrap_or(Path::new("/"));
            if !(at.writable)(folder) || !(at.writable)(bundle) {
                return InstallKind::Unsupported(if bundle.starts_with("/Volumes") {
                    Unsupported::DiskImage
                } else {
                    Unsupported::ReadOnly(folder.to_path_buf())
                });
            }
            InstallKind::MacBundle {
                bundle: bundle.to_path_buf(),
            }
        }
        "windows" => {
            let folder = at.exe.parent().unwrap_or(Path::new("."));
            if (at.exists)(&folder.join("unins000.exe")) {
                InstallKind::WindowsSetup {
                    exe: at.exe.to_path_buf(),
                }
            } else {
                InstallKind::Unsupported(Unsupported::NotInstalled)
            }
        }
        "linux" => match at.appimage {
            Some(file) => {
                let folder = file.parent().unwrap_or(Path::new("/"));
                if (at.writable)(folder) {
                    InstallKind::AppImage {
                        file: file.to_path_buf(),
                    }
                } else {
                    InstallKind::Unsupported(Unsupported::ReadOnly(folder.to_path_buf()))
                }
            }
            None => InstallKind::Unsupported(Unsupported::NotAppImage),
        },
        _ => InstallKind::Unsupported(Unsupported::Platform),
    }
}

/// A verified package, unpacked or copied to where installing it is quick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub version: Version,
    pub path: PathBuf,
}

/// What ShellRS does once the new version is in place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Relaunch {
    /// Restart from this path (`cx.set_restart_path` and `cx.restart`).
    Restart(PathBuf),
    /// Start this, which installs once ShellRS has quit, then quit.
    Hand(Launch),
    /// Nothing: ShellRS is quitting anyway.
    Nothing,
}

/// A program to start, prepared on the UI thread and started outside any
/// entity update: on Windows, creating a process can pump the message loop
/// and re-enter GPUI (gpui's own restart defers its spawn for this reason).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub envs: Vec<(OsString, OsString)>,
}

impl Launch {
    pub fn spawn(&self) -> std::io::Result<()> {
        let mut command = std::process::Command::new(&self.program);
        command.args(&self.args).envs(self.envs.iter().cloned());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // No console window flashing up.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        command.spawn().map(drop)
    }
}

/// Installs updates the way this copy of ShellRS was installed.
pub trait Installer: Send + Sync {
    fn kind(&self) -> &InstallKind;

    /// Get a verified package ready to install. Runs on an update worker.
    fn stage(&self, package: &Path, release: &Release) -> Result<Staged, UpdateError>;

    /// Put the staged version in place. Runs on the UI thread as ShellRS
    /// restarts (`relaunch`) or quits, so it only renames or starts a
    /// process.
    fn apply(&self, staged: &Staged, relaunch: bool) -> Result<Relaunch, UpdateError>;

    /// Remove what earlier updates left next to the installed copy. Called a
    /// while after start, so a version that crashes right away leaves the
    /// previous one's backup behind.
    fn clean_up(&self);
}

/// For copies that cannot replace themselves.
pub struct NoInstaller(InstallKind);

impl NoInstaller {
    pub fn new(reason: Unsupported) -> Self {
        Self(InstallKind::Unsupported(reason))
    }
}

impl Installer for NoInstaller {
    fn kind(&self) -> &InstallKind {
        &self.0
    }

    fn stage(&self, _: &Path, _: &Release) -> Result<Staged, UpdateError> {
        Err(UpdateError::Install("这份 ShellRS 无法自动更新".into()))
    }

    fn apply(&self, _: &Staged, _: bool) -> Result<Relaunch, UpdateError> {
        Err(UpdateError::Install("这份 ShellRS 无法自动更新".into()))
    }

    fn clean_up(&self) {}
}

/// The installer for this copy of ShellRS. `bundle` is `cx.app_path()`.
pub fn system_installer(bundle: Option<PathBuf>) -> Arc<dyn Installer> {
    let exe = std::env::current_exe().unwrap_or_default();
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let kind = detect(&Surroundings {
        os: std::env::consts::OS,
        exe: &exe,
        bundle: bundle.as_deref(),
        appimage: appimage.as_deref(),
        writable: &is_writable,
        exists: &|path| path.exists(),
    });
    match kind {
        #[cfg(target_os = "macos")]
        InstallKind::MacBundle { bundle } => Arc::new(super::install_macos::MacInstaller::new(
            bundle,
            Arc::new(super::install_macos::SystemTools),
        )),
        #[cfg(windows)]
        InstallKind::WindowsSetup { exe } => {
            Arc::new(super::install_windows::SetupInstaller::new(exe))
        }
        #[cfg(target_os = "linux")]
        InstallKind::AppImage { file } => {
            Arc::new(super::install_linux::AppImageInstaller::new(file))
        }
        InstallKind::Unsupported(reason) => Arc::new(NoInstaller::new(reason)),
        #[allow(unreachable_patterns)]
        _ => Arc::new(NoInstaller::new(Unsupported::Platform)),
    }
}

/// Whether ShellRS may create and rename things in `path`.
fn is_writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: a valid C string; `access` only reads it.
        unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).is_ok_and(|meta| !meta.permissions().readonly())
    }
}

/// A sibling of `path` named `name`, for staging next to an installed copy
/// so that every rename stays on one volume.
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(crate) fn sibling(path: &Path, name: &str) -> PathBuf {
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at<'a>(os: &'a str, exe: &'a Path) -> Surroundings<'a> {
        Surroundings {
            os,
            exe,
            bundle: None,
            appimage: None,
            writable: &|_| true,
            exists: &|_| false,
        }
    }

    #[test]
    fn an_app_in_applications_replaces_itself() {
        let exe = Path::new("/Applications/ShellRS.app/Contents/MacOS/shellrs");
        let bundle = Path::new("/Applications/ShellRS.app");
        let kind = detect(&Surroundings {
            bundle: Some(bundle),
            ..at("macos", exe)
        });
        assert_eq!(
            kind,
            InstallKind::MacBundle {
                bundle: bundle.into()
            }
        );
    }

    #[test]
    fn a_translocated_or_disk_image_app_asks_to_be_moved_first() {
        let exe = Path::new("/x/ShellRS.app/Contents/MacOS/shellrs");
        let translocated =
            Path::new("/private/var/folders/x/T/AppTranslocation/1234/d/ShellRS.app");
        assert_eq!(
            detect(&Surroundings {
                bundle: Some(translocated),
                ..at("macos", exe)
            }),
            InstallKind::Unsupported(Unsupported::Translocated)
        );
        let on_disk_image = Path::new("/Volumes/ShellRS/ShellRS.app");
        assert_eq!(
            detect(&Surroundings {
                bundle: Some(on_disk_image),
                writable: &|_| false,
                ..at("macos", exe)
            }),
            InstallKind::Unsupported(Unsupported::DiskImage)
        );
        let theirs = Path::new("/Applications/ShellRS.app");
        assert_eq!(
            detect(&Surroundings {
                bundle: Some(theirs),
                writable: &|path| path != Path::new("/Applications"),
                ..at("macos", exe)
            }),
            InstallKind::Unsupported(Unsupported::ReadOnly("/Applications".into()))
        );
        assert!(
            Unsupported::Translocated
                .reason()
                .contains("“应用程序”文件夹")
        );
    }

    #[test]
    fn a_mac_build_outside_a_bundle_cannot_replace_itself() {
        let exe = Path::new("/src/shellr/target/debug/shellrs");
        assert_eq!(
            detect(&Surroundings {
                bundle: Some(Path::new("/src/shellr/target/debug")),
                ..at("macos", exe)
            }),
            InstallKind::Unsupported(Unsupported::NotBundled)
        );
    }

    #[test]
    fn only_a_copy_the_setup_program_installed_updates_on_windows() {
        let exe = Path::new(r"C:\Users\me\AppData\Local\Programs\ShellRS\shellrs.exe");
        assert_eq!(
            detect(&Surroundings {
                exists: &|path| path.ends_with("unins000.exe"),
                ..at("windows", exe)
            }),
            InstallKind::WindowsSetup { exe: exe.into() }
        );
        assert_eq!(
            detect(&at("windows", exe)),
            InstallKind::Unsupported(Unsupported::NotInstalled)
        );
    }

    #[test]
    fn linux_outside_an_appimage_points_to_the_download_page() {
        let exe = Path::new("/usr/bin/shellrs");
        assert_eq!(
            detect(&at("linux", exe)),
            InstallKind::Unsupported(Unsupported::NotAppImage)
        );
        let file = Path::new("/home/me/Apps/ShellRS-x86_64.AppImage");
        let mounted = Path::new("/tmp/.mount_ShellRabc/usr/bin/shellrs");
        assert_eq!(
            detect(&Surroundings {
                appimage: Some(file),
                ..at("linux", mounted)
            }),
            InstallKind::AppImage { file: file.into() }
        );
        assert_eq!(
            detect(&Surroundings {
                appimage: Some(file),
                writable: &|_| false,
                ..at("linux", mounted)
            }),
            InstallKind::Unsupported(Unsupported::ReadOnly("/home/me/Apps".into()))
        );
    }
}
