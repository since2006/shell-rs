//! Directory row snapshots and display formatting.

use super::PaneSide;
use crate::sftp::{DirectoryEntry, EntryKind};
use gpui_kit::{App, Global, SharedString};
use serde::{Deserialize, Serialize};

/// Stable identity for one SFTP tab. A host can have several, each with
/// its own connection and transfer queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ExplorerId(pub u64);

/// What the 新建 menu creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewEntryKind {
    Folder,
    File,
}

impl NewEntryKind {
    pub fn title(self) -> &'static str {
        match self {
            NewEntryKind::Folder => "新建文件夹",
            NewEntryKind::File => "新建文件",
        }
    }

    pub fn default_name(self) -> &'static str {
        match self {
            NewEntryKind::Folder => "新建文件夹",
            NewEntryKind::File => "新建文件.txt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    File,
    Symlink,
}

impl FileKind {
    pub fn label(self) -> &'static str {
        match self {
            FileKind::Dir => "文件夹",
            FileKind::File => "文件",
            FileKind::Symlink => "链接",
        }
    }
}

impl From<EntryKind> for FileKind {
    /// Devices, sockets and pipes list as files; nothing opens them.
    fn from(kind: EntryKind) -> Self {
        match kind {
            EntryKind::Directory => FileKind::Dir,
            EntryKind::Symlink => FileKind::Symlink,
            EntryKind::File | EntryKind::Other => FileKind::File,
        }
    }
}

/// One row of a directory listing. Values stay raw; the table formats them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileEntry {
    pub name: SharedString,
    pub kind: FileKind,
    /// What a symbolic link points to; `None` for other rows and broken links.
    pub target: Option<FileKind>,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<i64>,
    /// The raw mode, type bits included.
    pub permissions: Option<u32>,
    pub owner: Option<SharedString>,
    pub group: Option<SharedString>,
}

impl FileEntry {
    pub fn new(name: &str, kind: FileKind) -> Self {
        Self {
            name: name.to_string().into(),
            kind,
            target: None,
            size: 0,
            modified: None,
            permissions: None,
            owner: None,
            group: None,
        }
    }

    pub fn dir(name: &str) -> Self {
        Self::new(name, FileKind::Dir)
    }

    #[cfg(test)]
    pub fn file(name: &str, size: u64) -> Self {
        Self {
            size,
            ..Self::new(name, FileKind::File)
        }
    }

    /// The `..` row that navigates to the parent directory.
    pub fn parent() -> Self {
        Self::dir("..")
    }

    pub fn from_directory_entry(entry: &DirectoryEntry) -> Self {
        let metadata = entry.metadata();
        let kind = FileKind::from(metadata.kind());
        Self {
            name: entry.name().to_string().into(),
            kind,
            target: (kind == FileKind::Symlink)
                .then(|| entry.target_kind().map(FileKind::from))
                .flatten(),
            size: metadata.size(),
            modified: metadata.modified().map(i64::from),
            permissions: metadata.permissions(),
            owner: entry.owner().map(|owner| owner.to_string().into()),
            group: entry.group().map(|group| group.to_string().into()),
        }
    }

    /// Directories and links to directories: both open on double-click and
    /// sort ahead of files.
    pub fn is_dir(&self) -> bool {
        self.kind == FileKind::Dir || self.target == Some(FileKind::Dir)
    }

    pub fn is_link(&self) -> bool {
        self.kind == FileKind::Symlink
    }

    pub fn is_parent(&self) -> bool {
        self.name.as_ref() == ".."
    }

    /// A dot file, which the lists leave out while hidden files are not
    /// shown. `..` is not one.
    pub fn is_hidden(&self) -> bool {
        !self.is_parent() && self.name.starts_with('.')
    }

    /// The type shown in the 类型 column: the kind, or the extension for files.
    pub fn type_label(&self) -> String {
        if self.is_parent() {
            return "上级目录".into();
        }
        match self.kind {
            FileKind::File => match self.name.rsplit_once('.') {
                Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
                    format!("{} 文件", ext.to_uppercase())
                }
                _ => self.kind.label().to_string(),
            },
            kind => kind.label().to_string(),
        }
    }
}

/// Where the item called `name` in `directory` is, by its side's path rules.
pub fn child_path(directory: &str, name: &str, remote: bool) -> String {
    if remote {
        format!("{}/{name}", directory.trim_end_matches('/'))
    } else {
        std::path::Path::new(directory)
            .join(name)
            .to_string_lossy()
            .into_owned()
    }
}

/// A typed directory resolved against a pane: `~` is the home directory, a
/// relative path is under `current`, and a trailing separator is dropped, so
/// `/var/log/` and `/var/log` name the same bookmark.
pub fn expand_path(text: &str, current: &str, home: &str, remote: bool) -> String {
    let joined = if text == "~" {
        home.to_string()
    } else if let Some(rest) = text.strip_prefix("~/") {
        format!("{}/{rest}", home.trim_end_matches('/'))
    } else if remote {
        if text.starts_with('/') {
            text.to_string()
        } else {
            format!("{}/{text}", current.trim_end_matches('/'))
        }
    } else {
        std::path::Path::new(current)
            .join(text)
            .to_string_lossy()
            .into_owned()
    };
    if remote {
        match joined.trim_end_matches('/') {
            "" => "/".into(),
            trimmed => trimmed.into(),
        }
    } else {
        std::path::Path::new(&joined)
            .components()
            .collect::<std::path::PathBuf>()
            .to_string_lossy()
            .into_owned()
    }
}

/// Every directory from the root down to `path`, as `(title, path)`: what the
/// 目录列表 select offers. The root's title is the root itself (`/`, `C:\`).
pub fn path_ancestors(path: &str, remote: bool) -> Vec<(String, String)> {
    if path.is_empty() {
        return Vec::new();
    }
    let mut chain = Vec::new();
    if remote {
        let Ok(mut current) = crate::sftp::RemotePath::new(path) else {
            return chain;
        };
        loop {
            let text = current.as_str().trim_end_matches('/');
            let title = text.rsplit('/').next().filter(|name| !name.is_empty());
            chain.push((
                title.unwrap_or("/").to_string(),
                if text.is_empty() {
                    "/".into()
                } else {
                    text.into()
                },
            ));
            if current.is_root() || text.is_empty() {
                break;
            }
            current = current.parent();
        }
    } else {
        for ancestor in std::path::Path::new(path).ancestors() {
            let text = ancestor.to_string_lossy().into_owned();
            if text.is_empty() {
                continue;
            }
            let title = ancestor
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| text.clone());
            chain.push((title, text));
        }
    }
    chain.reverse();
    chain
}

/// How the 大小 column shows sizes: WinSCP's 文件大小显示为, chosen from the
/// column title's menu and kept in the settings. A global, set from the
/// settings, because the explorer does not depend on them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSizeFormat {
    /// Every byte: `4,105,175,040 B`.
    Bytes,
    /// Whole kilobytes, the way Explorer and WinSCP show them by default:
    /// `4,008,960 KB`. One unit down the column, so the length of a number
    /// is its magnitude, and fine enough to compare both sides of a copy.
    #[default]
    Kilobytes,
    /// The unit that suits each size: `3.8 GB`.
    Short,
}

impl Global for FileSizeFormat {}

/// 显示隐藏文件: whether the lists show dot files, for each side on its own
/// (dot files on the server are often wanted, the local home's seldom). A
/// side's choice holds for every SFTP tab, kept in the settings and set from
/// them like `FileSizeFormat`. Off until the user turns it on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowHiddenFiles {
    pub local: bool,
    pub remote: bool,
}

impl Global for ShowHiddenFiles {}

impl ShowHiddenFiles {
    /// Whether `side` shows them now.
    pub fn get(side: PaneSide, cx: &App) -> bool {
        cx.try_global::<Self>()
            .copied()
            .unwrap_or_default()
            .on(side)
    }

    pub fn on(self, side: PaneSide) -> bool {
        match side {
            PaneSide::Local => self.local,
            PaneSide::Remote => self.remote,
        }
    }

    pub fn toggle(&mut self, side: PaneSide) {
        match side {
            PaneSide::Local => self.local = !self.local,
            PaneSide::Remote => self.remote = !self.remote,
        }
    }
}

impl FileSizeFormat {
    /// In the order the menu lists them.
    pub const ALL: [FileSizeFormat; 3] = [
        FileSizeFormat::Bytes,
        FileSizeFormat::Kilobytes,
        FileSizeFormat::Short,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FileSizeFormat::Bytes => "字节",
            FileSizeFormat::Kilobytes => "千字节（KB）",
            FileSizeFormat::Short => "简短格式（B、KB、MB、GB）",
        }
    }

    pub fn format(self, bytes: u64) -> String {
        match self {
            FileSizeFormat::Bytes => format!("{} B", group_thousands(bytes)),
            // Rounded up, so only an empty file shows 0 KB.
            FileSizeFormat::Kilobytes => format!("{} KB", group_thousands(bytes.div_ceil(1024))),
            FileSizeFormat::Short => format_size(bytes),
        }
    }
}

/// `4105175040` as `4,105,175,040`.
fn group_thousands(number: u64) -> String {
    let digits = number.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Human-readable size in the smallest unit that keeps the number under
/// 1024: `12 B`, `1.5 KB`, `348 KB`, `1.2 MB`. One decimal below 100,
/// whole numbers from there. The progress bar and 属性 use it, and the 大小
/// column in its short format.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 1;
    loop {
        // As it will be printed, so rounding up to 1024 moves to the next
        // unit and rounding up to 100 drops the decimal.
        let shown = if value < 99.95 {
            (value * 10.0).round() / 10.0
        } else {
            value.round()
        };
        if shown >= 1024.0 && unit < UNITS.len() - 1 {
            value /= 1024.0;
            unit += 1;
            continue;
        }
        return if shown < 100.0 {
            format!("{shown:.1} {}", UNITS[unit])
        } else {
            format!("{shown:.0} {}", UNITS[unit])
        };
    }
}

/// The 修改时间 column in local time: `2026/4/22 12:44:53`.
pub fn format_changed(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y/%-m/%-d %-H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// The 权限 column: `rwxr-xr-x`, with setuid, setgid and sticky shown as
/// `s`/`S` and `t`/`T` like `ls -l`.
pub fn format_rights(mode: u32) -> String {
    let special = [(0o4000, 's', 'S'), (0o2000, 's', 'S'), (0o1000, 't', 'T')];
    let mut rights = String::with_capacity(9);
    for (class, (special_bit, with_x, without_x)) in special.into_iter().enumerate() {
        let shift = 6 - class * 3;
        let bits = (mode >> shift) & 0o7;
        rights.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        rights.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        let execute = bits & 0o1 != 0;
        rights.push(match (mode & special_bit != 0, execute) {
            (true, true) => with_x,
            (true, false) => without_x,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    rights
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(348 * 1024), "348 KB");
        assert_eq!(format_size(1_258_291), "1.2 MB");
        assert_eq!(format_size(4_105_175_040), "3.8 GB");
        assert_eq!(format_size(3 * 1024u64.pow(4)), "3.0 TB");
    }

    #[test]
    fn the_size_column_shows_bytes_whole_kilobytes_or_short_sizes() {
        let [bytes, kilobytes, short] = FileSizeFormat::ALL;
        assert_eq!(FileSizeFormat::default(), kilobytes);

        assert_eq!(bytes.format(0), "0 B");
        assert_eq!(bytes.format(999), "999 B");
        assert_eq!(bytes.format(4_105_175_040), "4,105,175,040 B");

        assert_eq!(kilobytes.format(0), "0 KB");
        assert_eq!(kilobytes.format(1), "1 KB");
        assert_eq!(kilobytes.format(1024), "1 KB");
        assert_eq!(kilobytes.format(137_216), "134 KB");
        assert_eq!(kilobytes.format(999 * 1024 + 1), "1,000 KB");
        assert_eq!(kilobytes.format(4_105_175_040), "4,008,960 KB");

        assert_eq!(short.format(4_105_175_040), "3.8 GB");
    }

    #[test]
    fn a_size_that_rounds_up_is_shown_as_it_rounds() {
        // 99.96 KB would read "100.0 KB".
        assert_eq!(format_size(102_359), "100 KB");
        assert_eq!(format_size(102_297), "99.9 KB");
        // 1023.6 KB would read "1024 KB".
        assert_eq!(format_size(1_048_166), "1.0 MB");
        assert_eq!(format_size(1_047_961), "1023 KB");
    }

    #[test]
    fn rights_follow_ls() {
        assert_eq!(format_rights(0o100_755), "rwxr-xr-x");
        assert_eq!(format_rights(0o600), "rw-------");
        assert_eq!(format_rights(0o41_777), "rwxrwxrwt");
        assert_eq!(format_rights(0o1_776), "rwxrwxrwT");
        assert_eq!(format_rights(0o4_755), "rwsr-xr-x");
        assert_eq!(format_rights(0o2_745), "rwxr-Sr-x");
    }

    #[test]
    fn changed_uses_unpadded_date_and_seconds() {
        let formatted = format_changed(1_700_000_000);
        let (date, time) = formatted.split_once(' ').unwrap();
        let date: Vec<_> = date.split('/').collect();
        let time: Vec<_> = time.split(':').collect();
        assert_eq!(date[0].len(), 4, "{formatted}");
        assert!(date[1..].iter().all(|part| !part.starts_with('0')));
        assert_eq!(date.len(), 3);
        assert_eq!(time.len(), 3);
        assert_eq!(time[2].len(), 2);
    }

    #[test]
    fn type_label_uses_extension_for_files() {
        assert_eq!(FileEntry::file("a.log", 1).type_label(), "LOG 文件");
        assert_eq!(FileEntry::file(".zshrc", 1).type_label(), "文件");
        assert_eq!(FileEntry::dir("etc").type_label(), "文件夹");
        assert_eq!(FileEntry::parent().type_label(), "上级目录");
    }

    #[test]
    fn ancestors_run_from_the_root_down() {
        assert_eq!(
            path_ancestors("/home/tester/目录", true),
            [
                ("/".to_string(), "/".to_string()),
                ("home".into(), "/home".into()),
                ("tester".into(), "/home/tester".into()),
                ("目录".into(), "/home/tester/目录".into()),
            ]
        );
        assert_eq!(
            path_ancestors("/", true),
            [("/".to_string(), "/".to_string())]
        );
        #[cfg(unix)]
        assert_eq!(
            path_ancestors("/Users/me", false),
            [
                ("/".to_string(), "/".to_string()),
                ("Users".into(), "/Users".into()),
                ("me".into(), "/Users/me".into()),
            ]
        );
        assert!(path_ancestors("", true).is_empty());
    }

    #[test]
    fn typed_paths_resolve_against_the_pane() {
        let expand = |text| expand_path(text, "/srv/app", "/home/me", true);
        assert_eq!(expand("~"), "/home/me");
        assert_eq!(expand("~/logs/"), "/home/me/logs");
        assert_eq!(expand("releases"), "/srv/app/releases");
        assert_eq!(expand("/var/log/"), "/var/log");
        assert_eq!(expand("/"), "/");
        #[cfg(unix)]
        {
            assert_eq!(
                expand_path("docs/", "/Users/me", "/Users/me", false),
                "/Users/me/docs"
            );
            assert_eq!(expand_path("/tmp", "/Users/me", "/Users/me", false), "/tmp");
            assert_eq!(expand_path("/", "/Users/me", "/Users/me", false), "/");
        }
    }

    #[test]
    fn links_to_directories_count_as_directories() {
        let link = FileEntry {
            target: Some(FileKind::Dir),
            ..FileEntry::new("bin", FileKind::Symlink)
        };
        assert!(link.is_dir() && link.is_link());
        assert!(!FileEntry::new("broken", FileKind::Symlink).is_dir());
    }
}
