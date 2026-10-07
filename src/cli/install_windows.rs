//! Putting `shellrs` on the PATH the Windows way. A link would need an
//! administrator or developer mode there, and the app is a GUI program
//! anyway, so the command is a copy of the console program
//! `shellrs-cli.exe` in a folder of ShellRS's own, and that folder goes on
//! the user's PATH. Plain file work apart from the registry, so the tests
//! run everywhere, with the PATH kept in a file.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use super::install::{BinaryStatus, IntegrationPaths, UserPath};
use crate::i18n::t;

pub(super) fn binary_status(paths: &IntegrationPaths) -> BinaryStatus {
    let source = fs::metadata(&paths.exe).ok();
    let Ok(copy) = fs::metadata(&paths.bin_link) else {
        return match source {
            Some(_) => BinaryStatus::Missing,
            None => BinaryStatus::Unavailable,
        };
    };
    // Without the program beside the app there is nothing to compare, but
    // the copy can still be removed.
    if source.is_some_and(|source| !same_build(&source, &copy)) {
        return BinaryStatus::Outdated;
    }
    match on_path(paths) {
        Ok(true) => BinaryStatus::Installed,
        _ => BinaryStatus::NotOnPath,
    }
}

/// A copy keeps its source's size and modification time: installing sets
/// the time on purpose.
fn same_build(source: &fs::Metadata, copy: &fs::Metadata) -> bool {
    source.len() == copy.len() && modified(source) == modified(copy)
}

fn modified(metadata: &fs::Metadata) -> Option<SystemTime> {
    metadata.modified().ok()
}

fn on_path(paths: &IntegrationPaths) -> io::Result<bool> {
    let Some(user_path) = &paths.user_path else {
        return Ok(true);
    };
    Ok(path_contains(&read(user_path)?, folder(paths)))
}

/// Copy the command into place and put its folder on the PATH. A copy
/// that is running, because an agent is using it right now, cannot be
/// overwritten but can be moved aside.
pub(super) fn install_binary(paths: &IntegrationPaths) -> io::Result<()> {
    if !paths.exe.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            t!("cli.install.missing", path = paths.exe.display()).to_string(),
        ));
    }
    let folder = folder(paths);
    fs::create_dir_all(folder)?;
    let staged = sibling(&paths.bin_link, "new");
    fs::copy(&paths.exe, &staged)?;
    let source_time = fs::metadata(&paths.exe)?.modified()?;
    fs::File::options()
        .write(true)
        .open(&staged)?
        .set_modified(source_time)?;
    set_aside(&paths.bin_link)?;
    fs::rename(&staged, &paths.bin_link)?;
    clear_aside(&paths.bin_link);

    if let Some(user_path) = &paths.user_path {
        let value = read(user_path)?;
        if !path_contains(&value, folder) {
            write(user_path, &path_with(&value, folder))?;
        }
    }
    Ok(())
}

/// Delete the copy and take its folder off the PATH.
pub(super) fn remove_binary(paths: &IntegrationPaths) -> io::Result<()> {
    set_aside(&paths.bin_link)?;
    clear_aside(&paths.bin_link);
    let folder = folder(paths);
    if let Some(user_path) = &paths.user_path {
        let value = read(user_path)?;
        if path_contains(&value, folder) {
            write(user_path, &path_without(&value, folder))?;
        }
    }
    // Fails while a copy set aside is still running, which is fine.
    let _ = fs::remove_dir(folder);
    Ok(())
}

fn folder(paths: &IntegrationPaths) -> &Path {
    paths.bin_link.parent().unwrap_or(Path::new("."))
}

/// `file` with `.suffix` added to its name.
fn sibling(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    file.with_file_name(name)
}

/// Move `file` out of the way under a name of its own, since an older one
/// set aside may still be running too.
fn set_aside(file: &Path) -> io::Result<()> {
    let aside = sibling(file, &format!("{}.old", uuid::Uuid::new_v4().simple()));
    match fs::rename(file, aside) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Delete every copy set aside that is no longer running.
fn clear_aside(file: &Path) {
    let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
        return;
    };
    let prefix = format!("{}.", name.to_string_lossy());
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        if entry_name.starts_with(&prefix) && entry_name.ends_with(".old") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The PATH's current value, empty when it has none.
fn read(user_path: &UserPath) -> io::Result<String> {
    match user_path {
        UserPath::File(file) => match fs::read_to_string(file) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            result => result,
        },
        #[cfg(windows)]
        UserPath::Registry => registry::read(),
        #[cfg(not(windows))]
        UserPath::Registry => Err(io::Error::from(io::ErrorKind::Unsupported)),
    }
}

fn write(user_path: &UserPath, value: &str) -> io::Result<()> {
    match user_path {
        UserPath::File(file) => fs::write(file, value),
        #[cfg(windows)]
        UserPath::Registry => registry::write(value),
        #[cfg(not(windows))]
        UserPath::Registry => Err(io::Error::from(io::ErrorKind::Unsupported)),
    }
}

/// The user's `Path` under `HKEY_CURRENT_USER\Environment`.
#[cfg(windows)]
mod registry {
    use std::io;

    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    /// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`: the value is not there.
    const NOT_FOUND: i32 = 0x8007_0002_u32 as i32;

    fn environment() -> io::Result<windows_registry::Key> {
        Ok(windows_registry::CURRENT_USER.create("Environment")?)
    }

    pub(super) fn read() -> io::Result<String> {
        match environment()?.get_string("Path") {
            Ok(value) => Ok(value),
            Err(error) if error.code().0 == NOT_FOUND => Ok(String::new()),
            Err(error) => Err(error.into()),
        }
    }

    /// Save the value, expandable as Windows keeps it, and tell running
    /// programs the environment changed. Only programs started after this
    /// see it; an open console keeps its old PATH.
    pub(super) fn write(value: &str) -> io::Result<()> {
        let key = environment()?;
        if value.is_empty() {
            key.remove_value("Path")?;
        } else {
            key.set_expand_string("Path", value)?;
        }
        let environment: Vec<u16> = "Environment\0".encode_utf16().collect();
        let mut result = 0;
        // SAFETY: a NUL-terminated string that outlives the call.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                environment.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            )
        };
        Ok(())
    }
}

/// One PATH entry, as Windows compares them: without case or a trailing
/// separator.
fn entry_key(entry: &str) -> String {
    entry.trim().trim_end_matches(['\\', '/']).to_lowercase()
}

fn path_contains(value: &str, folder: &Path) -> bool {
    let folder = entry_key(&folder.to_string_lossy());
    value.split(';').any(|entry| entry_key(entry) == folder)
}

/// `value` with `folder` at the end.
fn path_with(value: &str, folder: &Path) -> String {
    let folder = folder.to_string_lossy();
    if value.is_empty() || value.ends_with(';') {
        format!("{value}{folder}")
    } else {
        format!("{value};{folder}")
    }
}

/// `value` without `folder`, everything else as it was.
fn path_without(value: &str, folder: &Path) -> String {
    let folder = entry_key(&folder.to_string_lossy());
    value
        .split(';')
        .filter(|entry| entry_key(entry) != folder)
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn the_folder_is_found_added_once_and_removed_by_windows_rules() {
        let folder = Path::new(r"C:\Users\me\AppData\Local\ShellRS\bin");
        let value = r"C:\Windows;c:\users\ME\appdata\local\shellrs\BIN\;D:\tools";
        assert!(path_contains(value, folder));
        assert!(!path_contains(
            r"C:\Windows;C:\Users\me\AppData\Local\ShellRS",
            folder
        ));

        assert_eq!(
            path_with(r"C:\Windows", folder),
            r"C:\Windows;C:\Users\me\AppData\Local\ShellRS\bin"
        );
        assert_eq!(
            path_with(r"C:\Windows;", folder),
            r"C:\Windows;C:\Users\me\AppData\Local\ShellRS\bin"
        );
        assert_eq!(
            path_with("", folder),
            r"C:\Users\me\AppData\Local\ShellRS\bin"
        );

        assert_eq!(path_without(value, folder), r"C:\Windows;D:\tools");
        // Someone else's empty entries stay.
        assert_eq!(
            path_without(r"C:\a;;C:\Users\me\AppData\Local\ShellRS\bin", folder),
            r"C:\a;"
        );
        assert_eq!(path_without(r"C:\a", folder), r"C:\a");
    }

    fn paths(root: &Path) -> IntegrationPaths {
        let exe = root.join("app").join("shellrs-cli.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "build 1").unwrap();
        IntegrationPaths {
            home: root.join("home"),
            bin_link: root.join("ShellRS").join("bin").join("shellrs.exe"),
            exe,
            user_path: Some(UserPath::File(root.join("PATH.txt"))),
        }
    }

    fn user_path(paths: &IntegrationPaths) -> String {
        let Some(UserPath::File(file)) = &paths.user_path else {
            unreachable!()
        };
        fs::read_to_string(file).unwrap_or_default()
    }

    #[test]
    fn the_command_is_copied_onto_the_path_and_taken_off_again() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let Some(UserPath::File(file)) = &paths.user_path else {
            unreachable!()
        };
        fs::write(file, r"C:\Windows").unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::Missing);

        install_binary(&paths).unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::Installed);
        assert_eq!(fs::read_to_string(&paths.bin_link).unwrap(), "build 1");
        let folder = paths.bin_link.parent().unwrap().display().to_string();
        assert_eq!(user_path(&paths), format!(r"C:\Windows;{folder}"));
        // Again changes nothing.
        install_binary(&paths).unwrap();
        assert_eq!(user_path(&paths), format!(r"C:\Windows;{folder}"));

        // Taken off the PATH by hand.
        fs::write(file, r"C:\Windows").unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::NotOnPath);
        install_binary(&paths).unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::Installed);

        remove_binary(&paths).unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::Missing);
        assert_eq!(user_path(&paths), r"C:\Windows");
        assert!(!paths.bin_link.parent().unwrap().exists());
        assert!(paths.exe.exists());
        // Removing what is not there is not an error.
        remove_binary(&paths).unwrap();
    }

    #[test]
    fn a_new_build_replaces_the_copy_even_while_it_is_open() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        install_binary(&paths).unwrap();

        fs::write(&paths.exe, "build 2, a little longer").unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::Outdated);
        // An agent running the old copy holds it open.
        let running = fs::File::open(&paths.bin_link).unwrap();
        install_binary(&paths).unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::Installed);
        assert_eq!(
            fs::read_to_string(&paths.bin_link).unwrap(),
            "build 2, a little longer"
        );
        drop(running);
        // Nothing left over once the old copy is closed and the next
        // install tidies up.
        install_binary(&paths).unwrap();
        let names: Vec<String> = fs::read_dir(paths.bin_link.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["shellrs.exe"]);
    }

    #[test]
    fn without_the_console_program_there_is_nothing_to_install() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        fs::remove_file(&paths.exe).unwrap();
        assert_eq!(binary_status(&paths), BinaryStatus::Unavailable);
        assert!(install_binary(&paths).is_err());
        assert!(!paths.bin_link.exists());
    }
}
