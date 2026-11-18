//! Windows shortcuts (`.lnk`) in the local pane (issue #144). Windows lists
//! a shortcut as the small file it is; the pane takes one for a link to what
//! it points at, as Explorer does: one to a folder opens the folder, one to a
//! file opens that file. The shell reads them, as it does for Explorer, so a
//! Unicode path, a network path and one kept only in the shell's own terms
//! all come out right.

use std::path::{Path, PathBuf};

/// What a shortcut points to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub path: PathBuf,
    /// Whether it was a folder when the shortcut was saved, as the shortcut
    /// says: that spares reaching for the target, which may be on a network
    /// drive that is gone.
    pub is_dir: bool,
}

/// Whether `path` names a shortcut, which only Windows has.
pub(super) fn is_shortcut(path: &Path) -> bool {
    cfg!(windows)
        && path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
}

/// Reads shortcuts on the thread it is made on, with COM started there from
/// the first one read until it is dropped.
#[derive(Default)]
pub(super) struct Shortcuts {
    #[cfg(windows)]
    com: std::cell::OnceCell<com::Com>,
}

impl Shortcuts {
    /// What the shortcut at `path` points to; `None` for a file that is not
    /// a shortcut, and for one to something that is no file or folder (This
    /// PC, Control Panel).
    pub fn target(&self, path: &Path) -> Option<Target> {
        if !is_shortcut(path) {
            return None;
        }
        #[cfg(windows)]
        {
            self.com.get_or_init(com::Com::start).target(path)
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    /// Where `path` leads: what it points to when it is a shortcut to a
    /// folder (with `folder`) or to a file, else `path` itself.
    pub fn follow(&self, path: &Path, folder: bool) -> PathBuf {
        self.target(path)
            .filter(|target| target.is_dir == folder)
            .map_or_else(|| path.to_path_buf(), |target| target.path)
    }
}

#[cfg(windows)]
mod com {
    use super::Target;
    use std::marker::PhantomData;
    use std::os::windows::ffi::OsStringExt as _;
    use std::path::{Path, PathBuf};
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, WIN32_FIND_DATAW};
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
        CoInitializeEx, CoUninitialize, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface as _};

    /// COM on this thread for as long as it lives. In a single-threaded
    /// apartment, as `trash` starts it on the same background threads:
    /// were it left started the other way, `trash` could not start it there
    /// and would panic. Started already by someone else, either way, it is
    /// used as it is.
    pub(super) struct Com {
        started: bool,
        /// Stopped on the thread it was started on.
        _thread: PhantomData<*const ()>,
    }

    impl Com {
        pub(super) fn start() -> Self {
            // SAFETY: no reserved pointer; a success is undone on drop, on
            // this thread.
            let result =
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
            Self {
                started: result.is_ok(),
                _thread: PhantomData,
            }
        }

        /// See [`super::Shortcuts::target`]. Only reads the shortcut: it does
        /// not look for a target that has moved, which may take long.
        pub(super) fn target(&self, path: &Path) -> Option<Target> {
            // SAFETY: COM is started on this thread for as long as `self`
            // lives, and the objects go before the end of this call.
            unsafe {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
                link.cast::<IPersistFile>()
                    .ok()?
                    .Load(&HSTRING::from(path), STGM_READ)
                    .ok()?;
                // As long as a Windows path gets.
                let mut buffer = vec![0u16; 32_768];
                let mut found = WIN32_FIND_DATAW::default();
                link.GetPath(&mut buffer, &mut found, 0).ok()?;
                let length = buffer.iter().position(|&unit| unit == 0)?;
                (length > 0).then(|| Target {
                    path: PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length])),
                    is_dir: found.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0,
                })
            }
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.started {
                // SAFETY: balances the start that succeeded on this thread;
                // `Com` is not `Send`.
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Save a shortcut at `at` to `to`, as Explorer's 创建快捷方式 does.
    #[cfg(test)]
    pub(in crate::sftp) fn make_shortcut(at: &Path, to: &Path) {
        let _com = Com::start();
        // SAFETY: COM is started on this thread until the end of the call.
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .expect("the shell makes shortcuts");
            link.SetPath(&HSTRING::from(to)).unwrap();
            link.cast::<IPersistFile>()
                .unwrap()
                .Save(&HSTRING::from(at), true)
                .unwrap();
        }
    }
}

#[cfg(all(test, windows))]
pub(super) use com::make_shortcut;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_windows_has_shortcuts() {
        assert_eq!(is_shortcut(Path::new("工作.lnk")), cfg!(windows));
        assert_eq!(is_shortcut(Path::new("工作.LNK")), cfg!(windows));
        assert!(!is_shortcut(Path::new("lnk")));
        assert!(!is_shortcut(Path::new("工作.lnk.txt")));
        // Not one, the path stays as it is.
        let shortcuts = Shortcuts::default();
        assert_eq!(
            shortcuts.follow(Path::new("notes.txt"), false),
            Path::new("notes.txt")
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_shortcut_tells_where_it_points_and_whether_to_a_folder() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("项目 文件夹");
        std::fs::create_dir(&folder).unwrap();
        let file = folder.join("说明.txt");
        std::fs::write(&file, "你好").unwrap();
        make_shortcut(&temp.path().join("项目.lnk"), &folder);
        make_shortcut(&temp.path().join("说明.lnk"), &file);
        std::fs::write(temp.path().join("坏的.lnk"), b"not a shortcut").unwrap();

        let shortcuts = Shortcuts::default();
        let to_folder = shortcuts.target(&temp.path().join("项目.lnk")).unwrap();
        assert_eq!(
            std::fs::canonicalize(&to_folder.path).unwrap(),
            std::fs::canonicalize(&folder).unwrap()
        );
        assert!(to_folder.is_dir);
        let to_file = shortcuts.target(&temp.path().join("说明.lnk")).unwrap();
        assert!(!to_file.is_dir);
        assert_eq!(
            std::fs::canonicalize(shortcuts.follow(&temp.path().join("说明.lnk"), false)).unwrap(),
            std::fs::canonicalize(&file).unwrap()
        );
        // A shortcut to a file does not lead into a folder.
        assert_eq!(
            shortcuts.follow(&temp.path().join("说明.lnk"), true),
            temp.path().join("说明.lnk")
        );
        assert_eq!(shortcuts.target(&temp.path().join("坏的.lnk")), None);
        assert_eq!(shortcuts.target(&temp.path().join("没有.lnk")), None);
    }
}
