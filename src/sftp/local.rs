//! This computer's side of the SFTP tab: its folders, the well-known
//! places, the trash, and the editor's local files. Its failures are worded
//! here, as the local pane shows them.

use super::{
    DirectoryEntry, DirectoryListing, FileBytes, FileStamp, PermissionEdit, ReadFailure,
    SaveFailure, TextFile, local_metadata,
};
use crate::i18n::t;
use anyhow::{Result, bail};
use gpui_kit::SharedString;
use std::path::{Path, PathBuf};

/// A well-known local folder, offered by the 目录列表 select.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Home,
    Desktop,
    Documents,
    Downloads,
    /// A Windows drive by its letter, as WinSCP lists them: `C: 本地磁盘`.
    Drive(char, DriveKind),
}

/// What a Windows drive is, which its title says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Local,
    Removable,
    Network,
    Optical,
    Other,
}

impl Place {
    pub fn title(self) -> SharedString {
        match self {
            Self::Home => t!("sftp.place.home"),
            Self::Desktop => t!("sftp.place.desktop"),
            Self::Documents => t!("sftp.place.documents"),
            Self::Downloads => t!("sftp.place.downloads"),
            Self::Drive(letter, kind) => {
                let drive = format!("{letter}:");
                match kind {
                    DriveKind::Local => t!("sftp.place.drive_local", drive = drive),
                    DriveKind::Removable => t!("sftp.place.drive_removable", drive = drive),
                    DriveKind::Network => t!("sftp.place.drive_network", drive = drive),
                    DriveKind::Optical => t!("sftp.place.drive_optical", drive = drive),
                    DriveKind::Other => drive.into(),
                }
            }
        }
    }
}

/// Local I/O is blocking and must be called on a background executor.
pub trait LocalDirectoryProvider: Send + Sync + 'static {
    fn home(&self) -> PathBuf;
    fn list(&self, path: &Path) -> Result<DirectoryListing>;
    /// Well-known folders for the 目录列表 select, home first, then the
    /// drives on Windows. Read again with each listing, on the same
    /// background executor, so a drive plugged in since shows up.
    fn places(&self) -> Vec<(Place, PathBuf)> {
        Vec::new()
    }
    /// Where downloads go when nothing else says: the system's Downloads
    /// folder. Only works out the path, so it may run anywhere.
    fn downloads(&self) -> PathBuf {
        self.home()
    }
    /// Move items to the system trash. Links go as links.
    fn trash(&self, paths: &[PathBuf]) -> Result<()>;
    /// Rename without replacing an existing item.
    fn rename(&self, from: &Path, to: &Path) -> Result<()>;
    fn create_dir(&self, path: &Path) -> Result<()>;
    /// Create an empty file; fails if the name exists.
    fn create_file(&self, path: &Path) -> Result<()>;
    /// Apply `edit` to each path (and, when `recursive`, everything under the
    /// directories), skipping links.
    fn set_permissions(
        &self,
        paths: &[PathBuf],
        edit: PermissionEdit,
        recursive: bool,
        add_x_to_dirs: bool,
    ) -> Result<()>;
    /// The bytes of a file to edit, following links, and its stamp. A
    /// directory or a file larger than `limit` fails with a `ReadFailure`
    /// before anything is read.
    fn read_file(&self, path: &Path, limit: u64) -> Result<(Vec<u8>, FileStamp)>;
    /// Write `bytes` over a file in place, following links and keeping its
    /// permissions; created when missing. With `expected`, the file must
    /// still match it, or this fails with `SaveFailure::Changed`.
    fn write_file(
        &self,
        path: &Path,
        bytes: &[u8],
        expected: Option<FileStamp>,
    ) -> Result<FileStamp>;
}
pub type SharedLocalDirectoryProvider = std::sync::Arc<dyn LocalDirectoryProvider>;

/// Every drive letter in use, `A:\` to `Z:\`: without them the local pane
/// could not leave the drive it started on.
#[cfg(windows)]
fn drives() -> Vec<(Place, PathBuf)> {
    use windows_sys::Win32::{
        Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives},
        System::WindowsProgramming::{
            DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
        },
    };
    // SAFETY: takes nothing and returns a bit mask, bit 0 for `A:`.
    let letters = unsafe { GetLogicalDrives() };
    (b'A'..=b'Z')
        .filter(|letter| letters & (1 << (letter - b'A')) != 0)
        .map(|letter| {
            let root = format!("{}:\\", char::from(letter));
            let wide: Vec<u16> = root.encode_utf16().chain([0]).collect();
            // SAFETY: `wide` is a null-terminated UTF-16 string that outlives
            // the call.
            let kind = match unsafe { GetDriveTypeW(wide.as_ptr()) } {
                DRIVE_FIXED | DRIVE_RAMDISK => DriveKind::Local,
                DRIVE_REMOVABLE => DriveKind::Removable,
                DRIVE_REMOTE => DriveKind::Network,
                DRIVE_CDROM => DriveKind::Optical,
                _ => DriveKind::Other,
            };
            (Place::Drive(char::from(letter), kind), PathBuf::from(root))
        })
        .collect()
}
#[cfg(not(windows))]
fn drives() -> Vec<(Place, PathBuf)> {
    Vec::new()
}
#[derive(Default)]
pub struct SystemLocalDirectoryProvider;
impl LocalDirectoryProvider for SystemLocalDirectoryProvider {
    fn home(&self) -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }
    fn downloads(&self) -> PathBuf {
        dirs::download_dir().unwrap_or_else(|| self.home())
    }
    fn places(&self) -> Vec<(Place, PathBuf)> {
        [
            (Place::Home, dirs::home_dir()),
            (Place::Desktop, dirs::desktop_dir()),
            (Place::Documents, dirs::document_dir()),
            (Place::Downloads, dirs::download_dir()),
        ]
        .into_iter()
        .filter_map(|(place, path)| Some((place, path?)))
        .chain(drives())
        .collect()
    }
    fn list(&self, path: &Path) -> Result<DirectoryListing> {
        // std's canonical form on Windows is `\\?\C:\Users`; the pane shows
        // and bookmarks the `C:\Users` people type, wherever that means the
        // same.
        let path = dunce::canonicalize(path)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!(t!("sftp.error.file_name_not_utf8")))?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            let mut row = DirectoryEntry::new(name, local_metadata(&metadata));
            if metadata.is_symlink() {
                row = row.with_target_kind(
                    std::fs::metadata(entry.path())
                        .ok()
                        .map(|target| local_metadata(&target).kind()),
                );
            }
            entries.push(row);
        }
        Ok(DirectoryListing::new(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!(t!("sftp.error.path_not_utf8")))?,
            entries,
        ))
    }
    fn trash(&self, paths: &[PathBuf]) -> Result<()> {
        #[allow(unused_mut)]
        let mut context = trash::TrashContext::default();
        // Finder's method asks for Automation permission and plays a sound;
        // the file manager call needs neither.
        #[cfg(target_os = "macos")]
        {
            use trash::macos::{DeleteMethod, TrashContextExtMacos as _};
            context.set_delete_method(DeleteMethod::NsFileManager);
        }
        context.delete_all(paths).map_err(|error| {
            anyhow::anyhow!(if cfg!(windows) {
                t!("sftp.error.recycle_bin_failed", error = error)
            } else {
                t!("sftp.error.trash_failed", error = error)
            })
        })
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        // POSIX rename silently replaces the target.
        if std::fs::symlink_metadata(to).is_ok() {
            bail!(t!("sftp.error.exists", name = display_name(to)));
        }
        std::fs::rename(from, to).map_err(|error| describe_io(error, to))
    }
    fn create_dir(&self, path: &Path) -> Result<()> {
        std::fs::create_dir(path).map_err(|error| describe_io(error, path))
    }
    fn create_file(&self, path: &Path) -> Result<()> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop)
            .map_err(|error| describe_io(error, path))
    }
    #[cfg(unix)]
    fn set_permissions(
        &self,
        paths: &[PathBuf],
        edit: PermissionEdit,
        recursive: bool,
        add_x_to_dirs: bool,
    ) -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let mut stack: Vec<PathBuf> = paths.iter().rev().cloned().collect();
        while let Some(path) = stack.pop() {
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|error| describe_io(error, &path))?;
            if metadata.is_symlink() {
                continue;
            }
            let mode = metadata.permissions().mode();
            let next = edit.apply(mode, metadata.is_dir(), add_x_to_dirs) & 0o7777;
            if next != mode & 0o7777 {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(next))
                    .map_err(|error| describe_io(error, &path))?;
            }
            if recursive && metadata.is_dir() {
                let mut children = std::fs::read_dir(&path)
                    .map_err(|error| describe_io(error, &path))?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<std::io::Result<Vec<_>>>()
                    .map_err(|error| describe_io(error, &path))?;
                children.sort_by(|a, b| b.cmp(a));
                stack.extend(children);
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    fn set_permissions(&self, _: &[PathBuf], _: PermissionEdit, _: bool, _: bool) -> Result<()> {
        bail!(t!("sftp.error.permissions_unsupported"))
    }
    fn read_file(&self, path: &Path, limit: u64) -> Result<(Vec<u8>, FileStamp)> {
        let metadata = std::fs::metadata(path).map_err(|error| describe_io(error, path))?;
        if !metadata.is_file() {
            return Err(ReadFailure::NotFile.into());
        }
        if metadata.len() > limit {
            return Err(ReadFailure::TooLarge(metadata.len()).into());
        }
        let bytes = std::fs::read(path).map_err(|error| describe_io(error, path))?;
        Ok((bytes, FileStamp::local(&metadata)))
    }
    fn write_file(
        &self,
        path: &Path,
        bytes: &[u8],
        expected: Option<FileStamp>,
    ) -> Result<FileStamp> {
        use std::io::Write as _;
        if let Some(expected) = expected {
            let now = std::fs::metadata(path).ok().map(|m| FileStamp::local(&m));
            if now != Some(expected) {
                return Err(SaveFailure::Changed.into());
            }
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|error| describe_io(error, path))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| describe_io(error, path))?;
        drop(file);
        let metadata = std::fs::metadata(path).map_err(|error| describe_io(error, path))?;
        Ok(FileStamp::local(&metadata))
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// A local I/O failure in the interface's language, naming the item.
fn describe_io(error: std::io::Error, path: &Path) -> anyhow::Error {
    let name = display_name(path);
    match error.kind() {
        std::io::ErrorKind::AlreadyExists => anyhow::anyhow!(t!("sftp.error.exists", name = name)),
        std::io::ErrorKind::PermissionDenied => {
            anyhow::anyhow!(t!("sftp.error.permission_denied", name = name))
        }
        std::io::ErrorKind::NotFound => anyhow::anyhow!(t!("sftp.error.not_found", name = name)),
        _ => anyhow::anyhow!(t!("sftp.error.item", name = name, error = error)),
    }
}

/// Read a local text file through `provider`. Blocking, like the provider.
pub fn read_local_text(
    provider: &dyn LocalDirectoryProvider,
    path: &Path,
) -> Result<TextFile, ReadFailure> {
    let (bytes, stamp) = provider
        .read_file(path, super::EDIT_LIMIT)
        .map_err(read_failure)?;
    TextFile::decode(bytes, stamp)
}

/// Read a local file whole through `provider`, binary or not. Blocking.
pub fn read_local_bytes(
    provider: &dyn LocalDirectoryProvider,
    path: &Path,
    limit: u64,
) -> Result<FileBytes, ReadFailure> {
    let (bytes, _) = provider.read_file(path, limit).map_err(read_failure)?;
    Ok(FileBytes::new(bytes))
}

/// Write the editor's bytes over a local file through `provider`. Blocking.
pub fn write_local_text(
    provider: &dyn LocalDirectoryProvider,
    path: &Path,
    bytes: &[u8],
    expected: Option<FileStamp>,
) -> Result<FileStamp, SaveFailure> {
    provider
        .write_file(path, bytes, expected)
        .map_err(|error| SaveFailure::from_error(error, |why| format!("{why:#}")))
}

/// The provider's refusal inside `error`, or the error itself in words.
fn read_failure(error: anyhow::Error) -> ReadFailure {
    ReadFailure::from_error(error, |why| format!("{why:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives are titled as WinSCP does: the letter, then what kind it is.
    #[test]
    fn drives_are_titled_by_letter_and_kind() {
        assert_eq!(Place::Drive('C', DriveKind::Local).title(), "C: 本地磁盘");
        assert_eq!(
            Place::Drive('E', DriveKind::Removable).title(),
            "E: 可移动磁盘"
        );
        assert_eq!(
            Place::Drive('Z', DriveKind::Network).title(),
            "Z: 网络驱动器"
        );
        assert_eq!(
            Place::Drive('F', DriveKind::Optical).title(),
            "F: CD 驱动器"
        );
        assert_eq!(Place::Drive('G', DriveKind::Other).title(), "G:");
    }

    /// Without the drives, the local pane could not leave the one it started on.
    #[cfg(windows)]
    #[test]
    fn local_listing_places_include_the_system_drive() {
        let system = std::env::var("SystemDrive").unwrap();
        let letter = system.chars().next().unwrap().to_ascii_uppercase();
        let places = SystemLocalDirectoryProvider.places();
        assert!(
            places.contains(&(
                Place::Drive(letter, DriveKind::Local),
                std::path::PathBuf::from(format!("{letter}:\\"))
            )),
            "{places:?}"
        );
    }

    /// On Windows std canonicalizes to `\\?\C:\…`, which the pane showed as is.
    #[test]
    fn local_listing_reports_the_path_people_type() {
        let tmp = tempfile::tempdir().unwrap();
        let listing = SystemLocalDirectoryProvider.list(tmp.path()).unwrap();
        assert!(!listing.path().starts_with(r"\\?\"), "{}", listing.path());
        // Still the directory asked for, links resolved.
        assert_eq!(
            std::fs::canonicalize(listing.path()).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn local_listing_rejects_unrepresentable_names() {
        use std::os::unix::ffi::OsStringExt as _;
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        std::fs::write(
            tmp.path().join(std::ffi::OsString::from_vec(vec![0xff])),
            [],
        )
        .unwrap();
        assert!(SystemLocalDirectoryProvider.list(tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn local_files_are_edited_in_place_through_links() {
        use crate::sftp::EDIT_LIMIT;
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        let provider = SystemLocalDirectoryProvider;
        let real = temp.path().join("real.conf");
        std::fs::write(&real, b"\xEF\xBB\xBFold\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = temp.path().join("link.conf");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let file = read_local_text(&provider, &link).unwrap();
        assert_eq!(file.text(), "old\n");
        assert!(file.format().bom());
        let stamp = write_local_text(
            &provider,
            &link,
            &file.format().encode("new\n"),
            Some(file.stamp()),
        )
        .unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"\xEF\xBB\xBFnew\n");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );

        std::fs::write(&real, b"theirs, longer\n").unwrap();
        assert_eq!(
            write_local_text(&provider, &link, b"mine\n", Some(stamp)),
            Err(SaveFailure::Changed)
        );
        assert_eq!(
            read_local_text(&provider, temp.path()),
            Err(ReadFailure::NotFile)
        );
        let big = std::fs::File::create(temp.path().join("big.log")).unwrap();
        big.set_len(EDIT_LIMIT + 1).unwrap();
        assert_eq!(
            read_local_text(&provider, &temp.path().join("big.log")),
            Err(ReadFailure::TooLarge(EDIT_LIMIT + 1))
        );
    }
}
