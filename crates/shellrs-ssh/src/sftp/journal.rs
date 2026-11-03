use super::{EntryKind, FileMetadata, JournalError, RemotePath};
use crate::WrapErr as _;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};
use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::io::AsyncWriteExt as _;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceMetadata {
    pub size: u64,
    pub modified_ns: u128,
}
impl SourceMetadata {
    pub async fn read(path: &Path) -> Result<Self> {
        let metadata = tokio::fs::symlink_metadata(path).await?;
        if !metadata.is_file() {
            bail!(JournalError::SourceNotFile);
        }
        Ok(Self {
            size: metadata.len(),
            modified_ns: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PublishPhase {
    Writing,
    Ready,
    BackingUp,
    BackedUp,
    Publishing,
    Published,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ResumeRecord {
    pub version: u32,
    pub endpoint: String,
    pub host_key: String,
    pub source: PathBuf,
    pub source_metadata: Option<SourceMetadata>,
    pub link_target: Option<String>,
    pub target: RemotePath,
    pub temporary: RemotePath,
    pub backup: RemotePath,
    pub original: Option<FileMetadata>,
    pub phase: PublishPhase,
}
impl ResumeRecord {
    pub fn new(
        endpoint: &str,
        host_key: &str,
        source: &Path,
        target: RemotePath,
        original: Option<FileMetadata>,
    ) -> Result<Self> {
        let id = uuid::Uuid::new_v4();
        let parent = target.parent();
        let temporary = RemotePath::new(format!("{}.filepart", target.as_str()))?;
        Ok(Self {
            version: 2,
            endpoint: endpoint.into(),
            host_key: host_key.into(),
            source: source.into(),
            source_metadata: None,
            link_target: None,
            temporary,
            backup: parent.join(&format!(".shellrs-{id}.backup"))?,
            target,
            original,
            phase: PublishPhase::Writing,
        })
    }
    pub fn validate(
        &self,
        endpoint: &str,
        host_key: &str,
        source: &Path,
        target: &RemotePath,
    ) -> Result<()> {
        if self.version != 2
            || self.endpoint != endpoint
            || self.host_key != host_key
            || self.source != source
            || &self.target != target
        {
            bail!(JournalError::Mismatch);
        }
        if self.temporary != RemotePath::new(format!("{}.filepart", target.as_str()))?
            || !managed_backup_path(&self.backup, target)
        {
            bail!(JournalError::TemporaryInvalid);
        }
        if self
            .original
            .as_ref()
            .is_some_and(|m| m.kind() == EntryKind::Directory)
        {
            bail!(JournalError::ReplacesFolder);
        }
        Ok(())
    }
}

/// Whether `path` is a backup name this module makes up, beside `target`.
fn managed_backup_path(path: &RemotePath, target: &RemotePath) -> bool {
    let name = path.as_str().rsplit('/').next().unwrap_or_default();
    name.strip_prefix(".shellrs-")
        .and_then(|value| value.strip_suffix(".backup"))
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .is_some()
        && path.parent() == target.parent()
        && path != target
}
#[derive(Clone)]
pub(crate) struct Journal {
    root: PathBuf,
}
impl Journal {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn path(&self, endpoint: &str, source: &Path, target: &RemotePath) -> PathBuf {
        record_path(
            &self.root,
            [
                endpoint.as_bytes(),
                source.as_os_str().as_encoded_bytes(),
                target.as_str().as_bytes(),
            ],
        )
    }
    pub async fn load(
        &self,
        endpoint: &str,
        source: &Path,
        target: &RemotePath,
    ) -> Result<Option<ResumeRecord>> {
        read_record(
            &self.path(endpoint, source, target),
            JournalError::UploadDamaged,
        )
        .await
    }
    pub async fn save(&self, record: &ResumeRecord) -> Result<()> {
        let path = self.path(&record.endpoint, &record.source, &record.target);
        write_atomic(&self.root, &path, &serde_json::to_vec(record)?).await
    }
    pub async fn remove(&self, record: &ResumeRecord) -> Result<()> {
        remove_if_present(&self.path(&record.endpoint, &record.source, &record.target)).await
    }
}

/// Read a record, if there is one. `damaged` is what to say when the file
/// is there and cannot be understood.
async fn read_record<T: DeserializeOwned>(path: &Path, damaged: JournalError) -> Result<Option<T>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).wrap_err(damaged)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).wrap_err(JournalError::Unreadable),
    }
}

/// Remove a file that may already be gone.
pub(crate) async fn remove_if_present(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Write a record so a crash leaves either the old or the new file: write a
/// sibling, fsync it, rename over, then fsync the directory. Private to the
/// user (0600), since records name local and remote paths.
async fn write_atomic(root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    tokio::fs::create_dir_all(root).await?;
    let temp = path.with_extension("pending");
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).await?;
    file.write_all(bytes).await?;
    sync(file).await?;
    tokio::fs::rename(&temp, path)
        .await
        .wrap_err(JournalError::Unsaved)?;
    #[cfg(unix)]
    {
        sync(tokio::fs::File::open(root).await?).await?;
    }
    Ok(())
}

/// Flush a file to the disk, as `sync_all` does, but on Apple's systems with a plain
/// `fsync`: std's `F_FULLFSYNC` also empties the drive's own cache, some
/// 10 ms each time, which SQLite does not do there by default either.
pub(crate) async fn sync(file: tokio::fs::File) -> std::io::Result<()> {
    let file = file.into_std().await;
    tokio::task::spawn_blocking(move || sync_file(&file))
        .await
        .map_err(std::io::Error::other)?
}

/// [`sync`], blocking.
pub(crate) fn sync_file(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd as _;
        // SAFETY: the descriptor is the open file's own, for the call.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        file.sync_all()
    }
}

fn record_path(root: &Path, parts: [&[u8]; 3]) -> PathBuf {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    root.join(format!("{:x}.json", hash.finalize()))
}

/// Resume state of one downloaded file. The remote size and modification
/// time are what the partial `.filepart` was read from; a changed remote
/// file cannot be resumed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct DownloadRecord {
    pub version: u32,
    pub endpoint: String,
    pub host_key: String,
    pub source: RemotePath,
    pub source_metadata: FileMetadata,
    pub target: PathBuf,
    pub temporary: PathBuf,
}
impl DownloadRecord {
    pub fn new(
        endpoint: &str,
        host_key: &str,
        source: RemotePath,
        source_metadata: FileMetadata,
        target: PathBuf,
    ) -> Self {
        Self {
            version: 1,
            endpoint: endpoint.into(),
            host_key: host_key.into(),
            temporary: partial_path(&target),
            source,
            source_metadata,
            target,
        }
    }
    pub fn validate(
        &self,
        endpoint: &str,
        host_key: &str,
        source: &RemotePath,
        target: &Path,
    ) -> Result<()> {
        if self.version != 1
            || self.endpoint != endpoint
            || self.host_key != host_key
            || &self.source != source
            || self.target != target
            || self.temporary != partial_path(target)
        {
            bail!(JournalError::Mismatch);
        }
        Ok(())
    }
}

/// `<target>.filepart`, next to the target so the final rename stays on one
/// file system.
pub(crate) fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".filepart");
    target.with_file_name(name)
}

#[derive(Clone)]
pub(crate) struct DownloadJournal {
    root: PathBuf,
}
impl DownloadJournal {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
    fn path(&self, endpoint: &str, source: &RemotePath, target: &Path) -> PathBuf {
        record_path(
            &self.root,
            [
                endpoint.as_bytes(),
                source.as_str().as_bytes(),
                target.as_os_str().as_encoded_bytes(),
            ],
        )
    }
    pub async fn load(
        &self,
        endpoint: &str,
        source: &RemotePath,
        target: &Path,
    ) -> Result<Option<DownloadRecord>> {
        read_record(
            &self.path(endpoint, source, target),
            JournalError::DownloadDamaged,
        )
        .await
    }
    pub async fn save(&self, record: &DownloadRecord) -> Result<()> {
        let path = self.path(&record.endpoint, &record.source, &record.target);
        write_atomic(&self.root, &path, &serde_json::to_vec(record)?).await
    }
    pub async fn remove(&self, record: &DownloadRecord) -> Result<()> {
        remove_if_present(&self.path(&record.endpoint, &record.source, &record.target)).await
    }
}

#[cfg(test)]
mod tests {
    use super::{DownloadJournal, Journal, RemotePath};
    use std::path::Path;

    /// Records are found again by their file name: one that changed would
    /// lose every transfer left to resume.
    #[test]
    fn record_names_never_change() {
        let root = Path::new("/records");
        let target = RemotePath::new("/srv/www/index.html").unwrap();
        let upload = Journal::new(root.into()).path(
            "root@10.0.0.1:22",
            Path::new("/home/me/index.html"),
            &target,
        );
        assert_eq!(
            upload,
            root.join("57c7d5d6e5e211f62c4bb24b5c9631e6f4235d3ec6ee8748c1343ce71a0bed23.json")
        );
        let download = DownloadJournal::new(root.into()).path(
            "root@10.0.0.1:22",
            &target,
            Path::new("/home/me/index.html"),
        );
        assert_eq!(
            download,
            root.join("408f1db18c527479432101dee6562deee38dd802091cd972af239a93065b19d3.json")
        );
    }
}
