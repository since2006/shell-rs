//! What a paste does when the clipboard holds a screenshot: write a file
//! the agent can read, and put that path on the input line.
//!
//! A local terminal writes into the system temp folder. A saved remote
//! host is written over SFTP by the workspace. An external connection does
//! not take images: another channel would end the bastion session.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui_kit::{ClipboardEntry, ClipboardItem, SharedString};

use crate::i18n::t;

/// Largest screenshot ShellRS will write. Bigger than that is refused.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;

/// How many paste files to keep in the local folder.
const KEEP: usize = 20;

/// How a terminal handles a screenshot on the clipboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImagePaste {
    /// Write a file on this computer and paste its path.
    Local,
    /// Ask the owner to put the file on the host, then paste that path.
    Remote,
    /// Leave the image alone (an external connection, or a host not saved).
    #[default]
    Off,
}

/// Bytes to write, and the extension the file should have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

/// What is on the clipboard for a terminal paste.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardPaste {
    Text(String),
    Image(ClipboardImage),
    Empty,
}

/// Text wins when the clipboard has both: copying a path or a sentence
/// should still paste that, even if an app also put a thumbnail there.
pub fn clipboard_paste(item: &ClipboardItem) -> ClipboardPaste {
    if let Some(text) = item.text().filter(|text| !text.is_empty()) {
        return ClipboardPaste::Text(text);
    }
    for entry in item.entries() {
        if let ClipboardEntry::Image(image) = entry {
            if image.bytes.is_empty() {
                continue;
            }
            return ClipboardPaste::Image(ClipboardImage {
                bytes: image.bytes.clone(),
                extension: image.format.extension(),
            });
        }
    }
    ClipboardPaste::Empty
}

/// The folder local pastes go to: the system temp directory, so they do
/// not sit next to the database.
pub fn pastes_dir() -> PathBuf {
    std::env::temp_dir().join("shellrs-pastes")
}

/// A unique name such as `shellrs-paste-….png`.
pub fn paste_file_name(extension: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("shellrs-paste-{nanos:x}.{extension}")
}

/// Write `bytes` into `dir` under a new name. Creates the directory.
pub fn save_in(dir: &Path, bytes: &[u8], extension: &str) -> Result<PathBuf, SharedString> {
    if bytes.is_empty() {
        return Err(t!("terminal.paste.image_empty"));
    }
    if bytes.len() > MAX_BYTES {
        return Err(t!("terminal.paste.image_too_large"));
    }
    create_private_dir(dir).map_err(|error| {
        t!(
            "terminal.paste.image_save_failed",
            error = error.to_string()
        )
    })?;
    prune(dir);
    let path = dir.join(paste_file_name(extension));
    write_private_file(&path, bytes).map_err(|error| {
        t!(
            "terminal.paste.image_save_failed",
            error = error.to_string()
        )
    })?;
    Ok(path)
}

/// Write a screenshot for a local terminal.
pub fn save_local(bytes: &[u8], extension: &str) -> Result<PathBuf, SharedString> {
    save_in(&pastes_dir(), bytes, extension)
}

fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((meta.modified().ok()?, path))
        })
        .collect();
    files.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let week = Duration::from_secs(7 * 24 * 3600);
    let now = SystemTime::now();
    for (index, (modified, path)) in files.into_iter().enumerate() {
        let old = now.duration_since(modified).is_ok_and(|age| age > week);
        if index >= KEEP || old {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Image;

    #[test]
    fn text_on_the_clipboard_is_pasted_as_text() {
        let item = ClipboardItem::new_string("hello".into());
        assert_eq!(clipboard_paste(&item), ClipboardPaste::Text("hello".into()));
    }

    #[test]
    fn an_image_without_text_is_pasted_as_a_file() {
        let item = ClipboardItem::new_image(&Image {
            format: gpui_kit::ImageFormat::Png,
            bytes: b"png".to_vec(),
            id: 1,
        });
        match clipboard_paste(&item) {
            ClipboardPaste::Image(image) => {
                assert_eq!(image.bytes, b"png");
                assert_eq!(image.extension, "png");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_screenshot_is_written_where_a_local_agent_can_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = save_in(dir.path(), b"png-bytes", "png").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"png-bytes");
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("shellrs-paste-")
        );
        assert!(path.extension().is_some_and(|ext| ext == "png"));
    }

    #[test]
    fn a_huge_image_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![0; MAX_BYTES + 1];
        assert_eq!(
            save_in(dir.path(), &bytes, "png").unwrap_err(),
            t!("terminal.paste.image_too_large")
        );
    }
}
