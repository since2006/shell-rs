use super::{EntryKind, FileMetadata, RemotePath};
use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
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
            bail!("本地来源已不再是普通文件");
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
    #[serde(alias = "fingerprint")]
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
            backup: parent.join(&format!(".shellr-{id}.backup"))?,
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
        if !matches!(self.version, 1 | 2)
            || self.endpoint != endpoint
            || self.host_key != host_key
            || self.source != source
            || &self.target != target
        {
            bail!("续传记录与当前来源或服务器不匹配");
        }
        let temporary_is_valid = if self.version == 1 {
            managed_uuid_path(&self.temporary, target, ".filepart")
        } else {
            self.temporary == RemotePath::new(format!("{}.filepart", target.as_str()))?
        };
        if !temporary_is_valid || !managed_uuid_path(&self.backup, target, ".backup") {
            bail!("无效的续传临时路径");
        }
        if self
            .original
            .as_ref()
            .is_some_and(|m| m.kind() == EntryKind::Directory)
        {
            bail!("续传记录不能替换目录");
        }
        Ok(())
    }
}

fn managed_uuid_path(path: &RemotePath, target: &RemotePath, suffix: &str) -> bool {
    let name = path.as_str().rsplit('/').next().unwrap_or_default();
    name.strip_prefix(".shellr-")
        .and_then(|value| value.strip_suffix(suffix))
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
        match tokio::fs::read(self.path(endpoint, source, target)).await {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("续传记录损坏，原文件和临时文件均已保留")?,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("无法读取续传记录"),
        }
    }
    pub async fn save(&self, record: &ResumeRecord) -> Result<()> {
        let path = self.path(&record.endpoint, &record.source, &record.target);
        write_atomic(&self.root, &path, &serde_json::to_vec(record)?).await
    }
    pub async fn remove(&self, record: &ResumeRecord) -> Result<()> {
        match tokio::fs::remove_file(self.path(&record.endpoint, &record.source, &record.target))
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
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
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(&temp, path)
        .await
        .context("无法保存续传进度")?;
    #[cfg(unix)]
    {
        tokio::fs::File::open(root).await?.sync_all().await?;
    }
    Ok(())
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
            bail!("续传记录与当前来源或服务器不匹配");
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
        match tokio::fs::read(self.path(endpoint, source, target)).await {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("续传记录损坏，临时文件已保留")?,
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).context("无法读取续传记录"),
        }
    }
    pub async fn save(&self, record: &DownloadRecord) -> Result<()> {
        let path = self.path(&record.endpoint, &record.source, &record.target);
        write_atomic(&self.root, &path, &serde_json::to_vec(record)?).await
    }
    pub async fn remove(&self, record: &DownloadRecord) -> Result<()> {
        match tokio::fs::remove_file(self.path(&record.endpoint, &record.source, &record.target))
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
