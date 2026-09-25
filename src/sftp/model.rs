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
/// One directory row. Owner names and link targets are display data only, so
/// they stay out of `FileMetadata`, which resume records persist and compare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    name: String,
    metadata: FileMetadata,
    owner: Option<String>,
    group: Option<String>,
    target_kind: Option<EntryKind>,
}
impl DirectoryEntry {
    pub fn new(name: impl Into<String>, metadata: FileMetadata) -> Self {
        Self {
            name: name.into(),
            metadata,
            owner: None,
            group: None,
            target_kind: None,
        }
    }
    pub fn with_owner(mut self, owner: Option<String>, group: Option<String>) -> Self {
        self.owner = owner;
        self.group = group;
        self
    }
    /// For a symbolic link: the kind of what it points to, `None` when broken.
    pub fn with_target_kind(mut self, kind: Option<EntryKind>) -> Self {
        self.target_kind = kind;
        self
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn metadata(&self) -> &FileMetadata {
        &self.metadata
    }
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }
    pub fn target_kind(&self) -> Option<EntryKind> {
        self.target_kind
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
    scp: bool,
    target_name: Option<String>,
}
impl UploadRequest {
    pub fn new(sources: Vec<PathBuf>, destination: RemotePath) -> Result<Self> {
        if sources.is_empty() {
            bail!("请选择要上传的文件或目录");
        }
        Ok(Self {
            sources,
            destination,
            scp: false,
            target_name: None,
        })
    }
    /// One source copied the way scp copies it, for the external CLI:
    /// `destination` is the copy's own path unless it is an existing
    /// directory, and a new file keeps the source's permission bits.
    pub fn scp(source: PathBuf, destination: RemotePath) -> Self {
        Self {
            sources: vec![source],
            destination,
            scp: true,
            target_name: None,
        }
    }
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }
    pub fn destination(&self) -> &RemotePath {
        &self.destination
    }
    pub fn is_scp(&self) -> bool {
        self.scp
    }
    /// The name the source is copied under, once an scp destination is
    /// resolved; `None` keeps the source's own.
    pub fn target_name(&self) -> Option<&str> {
        self.target_name.as_deref()
    }
    /// Copy into `directory`, under `name` or the source's own name.
    pub(crate) fn resolved(mut self, directory: RemotePath, name: Option<String>) -> Self {
        self.destination = directory;
        self.target_name = name;
        self
    }
}

/// Where scp puts a copy: into `destination` when it is an existing
/// directory, otherwise at `destination` itself. Returns the directory the
/// copy goes into and the name it takes there, `None` for the source's own.
pub(crate) fn scp_remote_target(
    destination: &RemotePath,
    is_directory: bool,
) -> (RemotePath, Option<String>) {
    let name = destination
        .as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next();
    match name {
        Some(name) if !is_directory && !name.is_empty() => {
            (destination.parent(), Some(name.to_string()))
        }
        _ => (destination.clone(), None),
    }
}

/// [`scp_remote_target`] for a local destination.
pub(crate) fn scp_local_target(
    destination: &Path,
    is_directory: bool,
) -> (PathBuf, Option<String>) {
    match (destination.parent(), destination.file_name()) {
        (Some(parent), Some(name)) if !is_directory => (
            parent.to_path_buf(),
            Some(name.to_string_lossy().into_owned()),
        ),
        _ => (destination.to_path_buf(), None),
    }
}
/// Which way one transfer batch moves files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransferDirection {
    #[default]
    Upload,
    Download,
}
impl TransferDirection {
    /// The verb used in progress and confirmation copy.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Upload => "上传",
            Self::Download => "下载",
        }
    }
}
/// Remote files and directories to copy into one local directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRequest {
    sources: Vec<RemotePath>,
    destination: PathBuf,
    scp: bool,
    target_name: Option<String>,
}
impl DownloadRequest {
    pub fn new(sources: Vec<RemotePath>, destination: PathBuf) -> Result<Self> {
        if sources.is_empty() {
            bail!("请选择要下载的文件或目录");
        }
        if !destination.is_absolute() {
            bail!("下载目标必须是绝对路径");
        }
        Ok(Self {
            sources,
            destination,
            scp: false,
            target_name: None,
        })
    }
    /// One source copied the way scp copies it; see [`UploadRequest::scp`].
    pub fn scp(source: RemotePath, destination: PathBuf) -> Result<Self> {
        if !destination.is_absolute() {
            bail!("下载目标必须是绝对路径");
        }
        Ok(Self {
            sources: vec![source],
            destination,
            scp: true,
            target_name: None,
        })
    }
    pub fn sources(&self) -> &[RemotePath] {
        &self.sources
    }
    pub fn destination(&self) -> &Path {
        &self.destination
    }
    pub fn is_scp(&self) -> bool {
        self.scp
    }
    /// See [`UploadRequest::target_name`].
    pub fn target_name(&self) -> Option<&str> {
        self.target_name.as_deref()
    }
    pub(crate) fn resolved(mut self, directory: PathBuf, name: Option<String>) -> Self {
        self.destination = directory;
        self.target_name = name;
        self
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferPhase {
    Scanning,
    Transferring,
    Waiting,
    Reconnecting,
    Stopped,
    Completed,
}
#[derive(Clone, Debug, PartialEq)]
pub struct TransferProgress {
    pub(crate) direction: TransferDirection,
    pub(crate) phase: TransferPhase,
    pub(crate) current: String,
    pub(crate) completed_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) total: usize,
    pub(crate) succeeded: usize,
    pub(crate) skipped: usize,
    pub(crate) failed: usize,
    pub(crate) bytes_per_second: u64,
    pub(crate) details: Vec<TransferDetail>,
}

/// How one item of a transfer ended, for 详情.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferDetail {
    outcome: TransferOutcome,
    /// Where the item went; empty when the batch failed before any item.
    path: String,
    /// Why it failed.
    reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferOutcome {
    Done,
    Skipped,
    Failed,
}

impl TransferDetail {
    pub fn done(path: impl Into<String>) -> Self {
        Self {
            outcome: TransferOutcome::Done,
            path: path.into(),
            reason: None,
        }
    }
    pub fn skipped(path: impl Into<String>) -> Self {
        Self {
            outcome: TransferOutcome::Skipped,
            path: path.into(),
            reason: None,
        }
    }
    pub fn failed(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            outcome: TransferOutcome::Failed,
            path: path.into(),
            reason: Some(reason.into()),
        }
    }
    pub fn outcome(&self) -> TransferOutcome {
        self.outcome
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}
impl Default for TransferProgress {
    fn default() -> Self {
        Self {
            direction: TransferDirection::Upload,
            phase: TransferPhase::Scanning,
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
impl TransferProgress {
    pub fn new(phase: TransferPhase) -> Self {
        Self {
            phase,
            ..Self::default()
        }
    }
    pub fn with_direction(mut self, direction: TransferDirection) -> Self {
        self.direction = direction;
        self
    }
    pub fn direction(&self) -> TransferDirection {
        self.direction
    }
    pub fn phase(&self) -> TransferPhase {
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
    /// The items that have ended, oldest first.
    pub fn details(&self) -> &[TransferDetail] {
        &self.details
    }
    pub fn is_active(&self) -> bool {
        !matches!(
            self.phase,
            TransferPhase::Stopped | TransferPhase::Completed
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferQuestionKind {
    Conflict,
    Resume,
    InvalidResume,
    Error,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferQuestion {
    id: u64,
    kind: TransferQuestionKind,
    path: String,
    message: String,
}
impl TransferQuestion {
    pub fn new(
        id: u64,
        kind: TransferQuestionKind,
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
    pub fn kind(&self) -> TransferQuestionKind {
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
pub enum TransferChoice {
    Overwrite,
    Skip,
    Cancel,
    Retry,
    Resume,
    Restart,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct TransferAnswer {
    choice: TransferChoice,
    apply_to_all: bool,
}
impl TransferAnswer {
    pub fn new(choice: TransferChoice, apply_to_all: bool) -> Self {
        Self {
            choice,
            apply_to_all,
        }
    }
    pub fn choice(&self) -> TransferChoice {
        self.choice
    }
    pub fn apply_to_all(&self) -> bool {
        self.apply_to_all
    }
}

/// One permission change applied to many items, as WinSCP's properties
/// dialog does for a multi-selection: bits in `set` turn on, bits in `clear`
/// turn off, and every other bit (file type, setuid, setgid, sticky, and the
/// rwx bits the user left alone) keeps its value per item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct PermissionEdit {
    set: u32,
    clear: u32,
}
impl PermissionEdit {
    pub fn new(set: u32, clear: u32) -> Self {
        let set = set & 0o777;
        Self {
            set,
            clear: clear & 0o777 & !set,
        }
    }
    /// Replace all nine rwx bits with `mode`.
    pub fn exact(mode: u32) -> Self {
        Self::new(mode, !mode)
    }
    pub fn set(&self) -> u32 {
        self.set
    }
    pub fn clear(&self) -> u32 {
        self.clear
    }
    pub fn is_empty(&self) -> bool {
        self.set == 0 && self.clear == 0
    }
    /// The new mode for an item whose current mode is `mode`. With
    /// `add_x_to_dirs`, a directory also becomes searchable by whoever can
    /// read it, so a recursive `644` does not lock the tree.
    pub fn apply(&self, mode: u32, is_dir: bool, add_x_to_dirs: bool) -> u32 {
        let mode = (mode & !self.clear) | self.set;
        if is_dir && add_x_to_dirs {
            mode | ((mode & 0o444) >> 2)
        } else {
            mode
        }
    }
}

/// Local I/O is blocking and must be called on a background executor.
pub trait LocalDirectoryProvider: Send + Sync + 'static {
    fn home(&self) -> PathBuf;
    fn list(&self, path: &Path) -> Result<DirectoryListing>;
    /// Well-known folders for the 目录列表 select, home first. Looked up
    /// from the platform, without touching the file system.
    fn places(&self) -> Vec<(String, PathBuf)> {
        Vec::new()
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
}
pub type SharedLocalDirectoryProvider = std::sync::Arc<dyn LocalDirectoryProvider>;
#[derive(Default)]
pub struct SystemLocalDirectoryProvider;
impl LocalDirectoryProvider for SystemLocalDirectoryProvider {
    fn home(&self) -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }
    fn places(&self) -> Vec<(String, PathBuf)> {
        [
            ("主目录", dirs::home_dir()),
            ("桌面", dirs::desktop_dir()),
            ("文稿", dirs::document_dir()),
            ("下载", dirs::download_dir()),
        ]
        .into_iter()
        .filter_map(|(title, path)| Some((title.to_string(), path?)))
        .collect()
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
                .ok_or_else(|| anyhow::anyhow!("路径不是有效的 UTF-8"))?,
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
        context
            .delete_all(paths)
            .map_err(|error| anyhow::anyhow!("无法移到废纸篓：{error}"))
    }
    fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        // POSIX rename silently replaces the target.
        if std::fs::symlink_metadata(to).is_ok() {
            bail!("已有名为「{}」的项目", display_name(to));
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
        bail!("此系统不支持修改权限")
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
        std::io::ErrorKind::AlreadyExists => anyhow::anyhow!("已有名为「{name}」的项目"),
        std::io::ErrorKind::PermissionDenied => anyhow::anyhow!("没有权限操作「{name}」"),
        std::io::ErrorKind::NotFound => anyhow::anyhow!("「{name}」不存在"),
        _ => anyhow::anyhow!("「{name}」：{error}"),
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
