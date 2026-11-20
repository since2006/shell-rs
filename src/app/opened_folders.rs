//! Folders the system asks ShellRS to open (issue #146): on macOS Finder's
//! 打开方式 and the tools that add to its menu, a folder dropped on the
//! Dock icon, `open -a ShellRS 文件夹`. Each opens a local terminal there.
//! The bundle says it opens folders and nothing else (`Info.plist.in`).
//!
//! The system tells the app as soon as it starts, before there is a
//! window, so the folders wait here: the workspace asks for them on its
//! timer once it is there and the app lock is open.

use std::path::PathBuf;
use std::sync::Mutex;

static OPENED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Keep the folders among `urls` for the workspace. Anything else, a file
/// above all, is left alone: ShellRS opens folders only.
pub fn note_opened_urls(urls: Vec<String>) {
    let folders = folders_in(&urls);
    OPENED
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .extend(folders);
}

/// The folders noted since the last time this was asked.
pub fn take_opened_folders() -> Vec<PathBuf> {
    std::mem::take(&mut *OPENED.lock().unwrap_or_else(|error| error.into_inner()))
}

/// The `file:` URLs among `urls` that name a folder here, as paths.
fn folders_in(urls: &[String]) -> Vec<PathBuf> {
    urls.iter()
        .filter_map(|url| url::Url::parse(url).ok())
        .filter(|url| url.scheme() == "file")
        .filter_map(|url| url.to_file_path().ok())
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_folders_are_opened() {
        let dir = tempfile::tempdir().unwrap();
        // As Finder writes it: escaped, with a slash at the end.
        let folder = dir.path().join("新 建");
        std::fs::create_dir(&folder).unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "").unwrap();
        let url = |path: &std::path::Path| url::Url::from_directory_path(path).unwrap().to_string();
        let urls = vec![
            url(&folder),
            url::Url::from_file_path(&file).unwrap().to_string(),
            url(&dir.path().join("gone")),
            "https://example.com/".to_string(),
            "not a url".to_string(),
        ];
        assert!(urls[0].contains("%20"), "{}", urls[0]);

        assert_eq!(folders_in(&urls), [folder]);
    }
}
