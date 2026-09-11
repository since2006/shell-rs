//! Pure in-memory file system model used by the explorer panes.

use std::collections::BTreeMap;

use gpui_kit::SharedString;

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

/// One row of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FileEntry {
    pub name: SharedString,
    pub kind: FileKind,
    pub size: u64,
    pub modified: SharedString,
    pub permissions: SharedString,
}

impl FileEntry {
    pub fn dir(name: &str, modified: &str, permissions: &str) -> Self {
        Self {
            name: name.to_string().into(),
            kind: FileKind::Dir,
            size: 0,
            modified: modified.to_string().into(),
            permissions: permissions.to_string().into(),
        }
    }

    pub fn file(name: &str, size: u64, modified: &str, permissions: &str) -> Self {
        Self {
            name: name.to_string().into(),
            kind: FileKind::File,
            size,
            modified: modified.to_string().into(),
            permissions: permissions.to_string().into(),
        }
    }

    pub fn symlink(name: &str, modified: &str, permissions: &str) -> Self {
        Self {
            name: name.to_string().into(),
            kind: FileKind::Symlink,
            size: 0,
            modified: modified.to_string().into(),
            permissions: permissions.to_string().into(),
        }
    }

    /// The `..` row that navigates to the parent directory.
    pub fn parent() -> Self {
        Self {
            name: "..".into(),
            kind: FileKind::Dir,
            size: 0,
            modified: "".into(),
            permissions: "".into(),
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.kind, FileKind::Dir)
    }

    pub fn is_parent(&self) -> bool {
        self.name.as_ref() == ".."
    }

    /// The type shown in the 类型 column: the kind, or the extension for files.
    pub fn type_label(&self) -> String {
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

/// An in-memory directory tree keyed by absolute path (`/`, `/etc`, ...).
#[derive(Clone, Debug, Default)]
pub struct DirTree {
    dirs: BTreeMap<String, Vec<FileEntry>>,
}

impl DirTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_dir(&mut self, path: &str, entries: Vec<FileEntry>) -> &mut Self {
        self.dirs.insert(normalize(path), entries);
        self
    }

    pub fn contains(&self, path: &str) -> bool {
        self.dirs.contains_key(&normalize(path))
    }

    pub fn entries(&self, path: &str) -> &[FileEntry] {
        self.dirs
            .get(&normalize(path))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// A pane's current directory inside a `DirTree`, with navigation.
#[derive(Clone, Debug)]
pub struct Location {
    tree: DirTree,
    cwd: Vec<String>,
    home: Option<Vec<String>>,
}

impl Location {
    /// Start at `start`; when `home` is given, paths under it display as `~/…`.
    pub fn new(tree: DirTree, start: &str, home: Option<&str>) -> Self {
        let mut location = Self {
            tree,
            cwd: Vec::new(),
            home: home.map(split_path),
        };
        if !location.set_path(start) {
            location.cwd.clear();
        }
        location
    }

    /// Absolute path of the current directory.
    pub fn path(&self) -> String {
        join_path(&self.cwd)
    }

    /// The path as shown in the address field (`~/Downloads` under home).
    pub fn display(&self) -> String {
        match &self.home {
            Some(home) if self.cwd.starts_with(home) => {
                let rest = &self.cwd[home.len()..];
                if rest.is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", rest.join("/"))
                }
            }
            _ => self.path(),
        }
    }

    pub fn is_root(&self) -> bool {
        self.cwd.is_empty()
    }

    /// Enter a child directory by name. Returns `false` if it does not exist.
    pub fn enter(&mut self, name: &str) -> bool {
        if name == ".." {
            return self.up();
        }
        let mut next = self.cwd.clone();
        next.push(name.to_string());
        let path = join_path(&next);
        if !self.tree.contains(&path) {
            return false;
        }
        self.cwd = next;
        true
    }

    /// Go to the parent directory. Returns `false` at the root.
    pub fn up(&mut self) -> bool {
        self.cwd.pop().is_some()
    }

    /// Jump to an absolute path, or a `~`-relative one when a home is set.
    pub fn set_path(&mut self, path: &str) -> bool {
        let path = path.trim();
        let expanded = match (&self.home, path.strip_prefix('~')) {
            (Some(home), Some(rest)) => format!("{}{}", join_path(home), rest),
            _ => path.to_string(),
        };
        if !self.tree.contains(&expanded) {
            return false;
        }
        self.cwd = split_path(&expanded);
        true
    }

    /// Rows for the table: `..` first (unless at the root), then directories,
    /// then files, each group sorted by name.
    pub fn rows(&self) -> Vec<FileEntry> {
        let mut entries: Vec<FileEntry> = self.tree.entries(&self.path()).to_vec();
        entries.sort_by(|a, b| {
            b.is_dir()
                .cmp(&a.is_dir())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        if !self.is_root() {
            entries.insert(0, FileEntry::parent());
        }
        entries
    }
}

/// Human-readable size: `12 B`, `348 KB`, `1.2 MB`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn normalize(path: &str) -> String {
    join_path(&split_path(path))
}

fn split_path(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .map(str::to_string)
        .collect()
}

fn join_path(parts: &[String]) -> String {
    if parts.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parts.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> DirTree {
        let mut tree = DirTree::new();
        tree.add_dir(
            "/",
            vec![
                FileEntry::dir("home", "", ""),
                FileEntry::file("readme.txt", 12, "", ""),
            ],
        )
        .add_dir("/home", vec![FileEntry::dir("xuz", "", "")])
        .add_dir(
            "/home/xuz",
            vec![
                FileEntry::file("b.txt", 1, "", ""),
                FileEntry::dir("Downloads", "", ""),
                FileEntry::file("A.txt", 2, "", ""),
            ],
        )
        .add_dir("/home/xuz/Downloads", vec![]);
        tree
    }

    #[test]
    fn enter_known_dir_updates_cwd() {
        let mut location = Location::new(tree(), "/", None);
        assert!(location.enter("home"));
        assert!(location.enter("xuz"));
        assert_eq!(location.path(), "/home/xuz");
    }

    #[test]
    fn enter_unknown_name_is_rejected() {
        let mut location = Location::new(tree(), "/home", None);
        assert!(!location.enter("nope"));
        assert_eq!(location.path(), "/home");
    }

    #[test]
    fn up_at_root_stays_root() {
        let mut location = Location::new(tree(), "/", None);
        assert!(!location.up());
        assert!(location.is_root());
        assert!(location.enter("home"));
        assert!(location.up());
        assert!(location.is_root());
    }

    #[test]
    fn set_path_rejects_missing_path_and_expands_home() {
        let mut location = Location::new(tree(), "/", Some("/home/xuz"));
        assert!(!location.set_path("/nowhere"));
        assert_eq!(location.path(), "/");
        assert!(location.set_path("~/Downloads"));
        assert_eq!(location.path(), "/home/xuz/Downloads");
        assert_eq!(location.display(), "~/Downloads");
        assert!(location.set_path("/home"));
        assert_eq!(location.display(), "/home");
    }

    #[test]
    fn rows_put_dotdot_first_then_dirs_then_files_by_name() {
        let location = Location::new(tree(), "/home/xuz", None);
        let names: Vec<_> = location.rows().iter().map(|e| e.name.to_string()).collect();
        assert_eq!(names, ["..", "Downloads", "A.txt", "b.txt"]);
        let root = Location::new(tree(), "/", None);
        assert_eq!(root.rows()[0].name.as_ref(), "home");
    }

    #[test]
    fn format_size_units() {
        assert_eq!(format_size(12), "12 B");
        assert_eq!(format_size(348 * 1024), "348 KB");
        assert_eq!(format_size(1_258_291), "1.2 MB");
        assert_eq!(format_size(0), "0 B");
    }

    #[test]
    fn type_label_uses_extension_for_files() {
        assert_eq!(FileEntry::file("a.log", 1, "", "").type_label(), "LOG 文件");
        assert_eq!(FileEntry::file(".zshrc", 1, "", "").type_label(), "文件");
        assert_eq!(FileEntry::dir("etc", "", "").type_label(), "文件夹");
    }
}
