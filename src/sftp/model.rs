use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A server path. Never interpreted using the client operating system's rules.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemotePath(String);
impl RemotePath {
    pub fn new(path: impl Into<String>) -> Result<Self> {
        let path = path.into();
        if path.is_empty() || path.contains('\0') {
            bail!("远程路径不能为空或包含空字符");
        }
        Ok(Self(path))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn join(&self, name: &str) -> Result<Self> {
        if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\0']) {
            bail!("无效的文件名：{name}");
        }
        Self::new(format!("{}/{name}", self.0.trim_end_matches('/')))
    }
    pub fn parent(&self) -> Self {
        let path = self.0.trim_end_matches('/');
        Self(
            path.rsplit_once('/')
                .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
                .unwrap_or("/")
                .to_owned(),
        )
    }
    pub fn is_root(&self) -> bool {
        self.0 == "/"
    }
}
impl std::fmt::Display for RemotePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadata {
    kind: EntryKind,
    size: u64,
    modified: Option<u32>,
    permissions: Option<u32>,
}
impl FileMetadata {
    pub fn new(
        kind: EntryKind,
        size: u64,
        modified: Option<u32>,
        permissions: Option<u32>,
    ) -> Self {
        Self {
            kind,
            size,
            modified,
            permissions,
        }
    }
    pub fn kind(&self) -> EntryKind {
        self.kind
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn modified(&self) -> Option<u32> {
        self.modified
    }
    pub fn permissions(&self) -> Option<u32> {
        self.permissions
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    name: String,
    metadata: FileMetadata,
}
impl DirectoryEntry {
    pub fn new(name: impl Into<String>, metadata: FileMetadata) -> Self {
        Self {
            name: name.into(),
            metadata,
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn metadata(&self) -> &FileMetadata {
        &self.metadata
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryListing {
    path: String,
    entries: Vec<DirectoryEntry>,
}
impl DirectoryListing {
    pub fn new(path: impl Into<String>, entries: Vec<DirectoryEntry>) -> Self {
        Self {
            path: path.into(),
            entries,
        }
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn entries(&self) -> &[DirectoryEntry] {
        &self.entries
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadRequest {
    sources: Vec<PathBuf>,
    destination: RemotePath,
}
impl UploadRequest {
    pub fn new(sources: Vec<PathBuf>, destination: RemotePath) -> Result<Self> {
        if sources.is_empty() {
            bail!("请选择要上传的文件或目录");
        }
        Ok(Self {
            sources,
            destination,
        })
    }
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }
    pub fn destination(&self) -> &RemotePath {
        &self.destination
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadPhase {
    Scanning,
    Uploading,
    Waiting,
    Reconnecting,
    Stopped,
    Completed,
}
#[derive(Clone, Debug, PartialEq)]
pub struct UploadProgress {
    pub(crate) phase: UploadPhase,
    pub(crate) current: String,
    pub(crate) completed_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) total: usize,
    pub(crate) succeeded: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
    pub(crate) bytes_per_second: u64,
    pub(crate) details: Vec<String>,
}
impl Default for UploadProgress {
    fn default() -> Self {
        Self {
            phase: UploadPhase::Scanning,
            current: String::new(),
            completed_bytes: 0,
            total_bytes: 0,
            total: 0,
            succeeded: 0,
            skipped: 0,
            failed: 0,
            bytes_per_second: 0,
            details: Vec::new(),
        }
    }
}
impl UploadProgress {
    pub fn new(phase: UploadPhase) -> Self {
        Self {
            phase,
            ..Self::default()
        }
    }
    pub fn phase(&self) -> UploadPhase {
        self.phase
    }
    pub fn current(&self) -> &str {
        &self.current
    }
    pub fn completed_bytes(&self) -> u64 {
        self.completed_bytes
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    pub fn total(&self) -> usize {
        self.total
    }
    pub fn succeeded(&self) -> usize {
        self.succeeded
    }
    pub fn skipped(&self) -> usize {
        self.skipped
    }
    pub fn failed(&self) -> usize {
        self.failed
    }
    pub fn bytes_per_second(&self) -> u64 {
        self.bytes_per_second
    }
    pub fn details(&self) -> &[String] {
        &self.details
    }
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, UploadPhase::Stopped | UploadPhase::Completed)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadQuestionKind {
    Conflict,
    Resume,
    InvalidResume,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadQuestion {
    id: u64,
    kind: UploadQuestionKind,
    path: String,
    message: String,
}
impl UploadQuestion {
    pub fn new(
        id: u64,
        kind: UploadQuestionKind,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id,
            kind,
            path: path.into(),
            message: message.into(),
        }
    }
    pub fn id(&self) -> u64 {
        self.id
    }
    pub fn kind(&self) -> UploadQuestionKind {
        self.kind
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum UploadChoice {
    Overwrite,
    Skip,
    Cancel,
    Retry,
    Resume,
    Restart,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct UploadAnswer {
    choice: UploadChoice,
    apply_to_all: bool,
}
impl UploadAnswer {
    pub fn new(choice: UploadChoice, apply_to_all: bool) -> Self {
        Self {
            choice,
            apply_to_all,
        }
    }
    pub fn choice(&self) -> UploadChoice {
        self.choice
    }
    pub fn apply_to_all(&self) -> bool {
        self.apply_to_all
    }
}

/// Local I/O is blocking and must be called on a background executor.
pub trait LocalDirectoryProvider: Send + Sync + 'static {
    fn home(&self) -> PathBuf;
    fn list(&self, path: &Path) -> Result<DirectoryListing>;
}
pub type SharedLocalDirectoryProvider = std::sync::Arc<dyn LocalDirectoryProvider>;
#[derive(Default)]
pub struct SystemLocalDirectoryProvider;
impl LocalDirectoryProvider for SystemLocalDirectoryProvider {
    fn home(&self) -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }
    fn list(&self, path: &Path) -> Result<DirectoryListing> {
        let path = std::fs::canonicalize(path)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("目录包含无法用 UTF-8 表示的文件名"))?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            entries.push(DirectoryEntry::new(name, local_metadata(&metadata)));
        }
        Ok(DirectoryListing::new(
            path.to_str()
                .ok_or_else(|| anyhow::anyhow!("路径不是有效的 UTF-8"))?,
            entries,
        ))
    }
}
pub(crate) fn local_metadata(metadata: &std::fs::Metadata) -> FileMetadata {
    let kind = if metadata.is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| u32::try_from(d.as_secs()).ok());
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode())
    };
    #[cfg(not(unix))]
    let permissions = None;
    FileMetadata::new(kind, metadata.len(), modified, permissions)
}
