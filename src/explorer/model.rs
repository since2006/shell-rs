//! Directory row snapshots and display formatting.

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

#[cfg(test)]
mod tests {
    use super::*;

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
