//! Downloading remote files and trees, the mirror of `upload`: each file is
//! written to `<target>.filepart` next to its target and renamed over it
//! once complete. An interrupted file resumes from the partial file's length,
//! as WinSCP does, as long as the remote file still has the size and time it
//! had when the partial file was started.

use super::{
    DownloadRequest, EntryKind, FileMetadata, RemotePath, SftpEvent, TransferChoice,
    TransferDirection, TransferPhase, TransferProgress, TransferQuestionKind,
    client::RemoteFs,
    control::{Cancelled, TargetGuard, TransferControl},
    journal::{DownloadJournal, DownloadRecord, partial_path},
};
use anyhow::{Result, anyhow, bail};
use futures::{StreamExt as _, stream::FuturesUnordered};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};

const CHUNK: u32 = 32 * 1024;
const MAX_IN_FLIGHT: usize = 16;

#[derive(Clone)]
struct DownloadItem {
    source: RemotePath,
    target: PathBuf,
    metadata: FileMetadata,
    error: Option<String>,
}

pub(crate) struct DownloadBatch {
    items: Vec<DownloadItem>,
    cursor: usize,
    pub progress: TransferProgress,
    endpoint: String,
    host_key: String,
    journal: DownloadJournal,
    all_conflicts: Option<TransferChoice>,
    approved_resumes: HashSet<PathBuf>,
    blocked_directories: Vec<PathBuf>,
    started: Instant,
    received_bytes: u64,
    completed_bytes: u64,
    last_progress: Instant,
}

/// A name that cannot be created on this machine gets a clear error instead
/// of a path that means something else.
fn local_name(name: &str) -> Result<&str> {
    let invalid = if cfg!(windows) {
        name.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*', '\0'])
            || name.ends_with([' ', '.'])
    } else {
        name.contains(['/', '\0'])
    };
    if invalid || matches!(name, "" | "." | "..") {
        bail!("名称「{name}」在本机无效");
    }
    Ok(name)
}

fn remote_name(path: &RemotePath) -> &str {
    path.as_str()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

impl DownloadBatch {
    pub async fn scan<F: RemoteFs>(
        request: &DownloadRequest,
        endpoint: &str,
        host_key: &str,
        journal: DownloadJournal,
        fs: &F,
        control: &TransferControl,
    ) -> Result<Self> {
        let mut sources = request.sources().to_vec();
        sources.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        sources.dedup();
        // A source inside another source comes with its parent.
        let mut roots: Vec<RemotePath> = Vec::new();
        for source in sources {
            if !roots.iter().any(|parent| {
                source
                    .as_str()
                    .starts_with(&format!("{}/", parent.as_str().trim_end_matches('/')))
            }) {
                roots.push(source);
            }
        }
        let mut stack = Vec::new();
        for source in roots.into_iter().rev() {
            let target =
                local_name(remote_name(&source)).map(|name| request.destination().join(name));
            stack.push((source, target, None));
        }
        let mut items = Vec::new();
        let mut total_bytes: u64 = 0;
        while let Some((source, target, known)) = stack.pop() {
            control.check()?;
            let target = match target {
                Ok(target) => target,
                Err(error) => {
                    items.push(DownloadItem {
                        target: request.destination().join(remote_name(&source)),
                        source,
                        metadata: FileMetadata::new(EntryKind::Other, 0, None, None),
                        error: Some(error.to_string()),
                    });
                    continue;
                }
            };
            let metadata = match known {
                Some(metadata) => metadata,
                None => {
                    let found = control.run(fs.metadata(&source)).await;
                    let error = match found {
                        Ok(Some(metadata)) => Ok(metadata),
                        Ok(None) => Err("远程项目不存在".to_string()),
                        Err(error)
                            if super::client::is_network_error(&error)
                                || error.is::<Cancelled>() =>
                        {
                            return Err(error);
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    match error {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            items.push(DownloadItem {
                                source,
                                target,
                                metadata: FileMetadata::new(EntryKind::Other, 0, None, None),
                                error: Some(error),
                            });
                            continue;
                        }
                    }
                }
            };
            let mut scan_error = None;
            if metadata.kind() == EntryKind::Directory {
                match control.run(fs.read_dir(&source)).await {
                    Ok(mut children) => {
                        children.sort_by(|a, b| b.name().cmp(a.name()));
                        for child in children {
                            stack.push((
                                source.join(child.name())?,
                                local_name(child.name()).map(|name| target.join(name)),
                                Some(child.metadata().clone()),
                            ));
                        }
                    }
                    Err(error)
                        if super::client::is_network_error(&error) || error.is::<Cancelled>() =>
                    {
                        return Err(error);
                    }
                    Err(error) => scan_error = Some(error.to_string()),
                }
            } else if metadata.kind() == EntryKind::File {
                total_bytes = total_bytes.saturating_add(metadata.size());
            }
            items.push(DownloadItem {
                source,
                target,
                metadata,
                error: scan_error,
            });
        }
        let progress = TransferProgress {
            direction: TransferDirection::Download,
            total: items.len(),
            total_bytes,
            ..TransferProgress::default()
        };
        Ok(Self {
            items,
            cursor: 0,
            progress,
            endpoint: endpoint.into(),
            host_key: host_key.into(),
            journal,
            all_conflicts: None,
            approved_resumes: HashSet::new(),
            blocked_directories: Vec::new(),
            started: Instant::now(),
            received_bytes: 0,
            completed_bytes: 0,
            last_progress: Instant::now(),
        })
    }

    pub fn verify_host(&self, fingerprint: &str) -> Result<()> {
        if self.host_key != fingerprint {
            bail!("服务器主机指纹已改变，已停止续传");
        }
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.cursor >= self.items.len()
    }

    pub fn emit(&self, control: &TransferControl) {
        let _ = control
            .events
            .try_send(SftpEvent::Progress(self.progress.clone()));
    }

    pub fn phase(&mut self, phase: TransferPhase, control: &TransferControl) {
        self.progress.phase = phase;
        self.emit(control);
    }

    pub async fn run<F: RemoteFs>(&mut self, fs: &F, control: &TransferControl) -> Result<()> {
        self.phase(TransferPhase::Transferring, control);
        while self.cursor < self.items.len() {
            control.check()?;
            let item = self.items[self.cursor].clone();
            self.progress.current = item.target.display().to_string();
            self.emit(control);
            let _target_guard = TargetGuard::acquire(
                format!("local\0{}", item.target.display()),
                "其他会话正在下载到同一位置，请稍后继续下载",
            )?;
            let result = if self
                .blocked_directories
                .iter()
                .any(|dir| item.target.starts_with(dir) && item.target != *dir)
            {
                Err(anyhow!("父目录未创建，无法下载"))
            } else if let Some(error) = &item.error {
                Err(anyhow!("{error}"))
            } else {
                self.download_item(fs, &item, control).await
            };
            match result {
                Ok(true) => {
                    self.progress.succeeded += 1;
                    self.progress
                        .details
                        .push(format!("已下载：{}", item.target.display()));
                    if item.metadata.kind() == EntryKind::File {
                        self.completed_bytes =
                            self.completed_bytes.saturating_add(item.metadata.size());
                    }
                }
                Ok(false) => {
                    self.progress.skipped += 1;
                    self.progress
                        .details
                        .push(format!("已跳过：{}", item.target.display()));
                    if item.metadata.kind() == EntryKind::Directory {
                        self.blocked_directories.push(item.target.clone());
                    }
                }
                Err(error)
                    if error.is::<Cancelled>() || super::client::is_network_error(&error) =>
                {
                    return Err(error);
                }
                Err(error) => {
                    self.phase(TransferPhase::Waiting, control);
                    let answer = control
                        .ask(
                            TransferQuestionKind::Error,
                            item.source.as_str(),
                            &format!("无法下载：{error:#}"),
                        )
                        .await?;
                    if answer.choice() == TransferChoice::Retry {
                        if item.error.is_some() {
                            let request = DownloadRequest::new(
                                vec![item.source.clone()],
                                item.target
                                    .parent()
                                    .map(Path::to_path_buf)
                                    .ok_or_else(|| anyhow!("无效的下载目标"))?,
                            )?;
                            let scanned = Self::scan(
                                &request,
                                &self.endpoint,
                                &self.host_key,
                                self.journal.clone(),
                                fs,
                                control,
                            )
                            .await?;
                            self.progress.total =
                                self.progress.total.saturating_sub(1) + scanned.items.len();
                            self.progress.total_bytes = self
                                .progress
                                .total_bytes
                                .saturating_sub(item.metadata.size())
                                + scanned.progress.total_bytes;
                            self.items.splice(self.cursor..=self.cursor, scanned.items);
                        }
                        continue;
                    }
                    self.progress.failed += 1;
                    self.progress
                        .details
                        .push(format!("失败：{}：{error:#}", item.target.display()));
                    if item.metadata.kind() == EntryKind::Directory {
                        self.blocked_directories.push(item.target.clone());
                    }
                }
            }
            self.cursor += 1;
            self.progress.completed_bytes = self.completed_bytes;
            self.phase(TransferPhase::Transferring, control);
        }
        self.phase(TransferPhase::Completed, control);
        Ok(())
    }

    /// Ask before replacing an existing local item; remembers "apply to all".
    /// `changed` means the target appeared or changed while downloading.
    async fn approve(
        &mut self,
        target: &Path,
        existing: &std::fs::Metadata,
        changed: bool,
        control: &TransferControl,
    ) -> Result<bool> {
        if existing.is_dir() {
            bail!("本地已有同名目录，不会删除它");
        }
        if !changed && let Some(choice) = self.all_conflicts {
            return Ok(choice == TransferChoice::Overwrite);
        }
        self.phase(TransferPhase::Waiting, control);
        let modified = existing
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs());
        let message = format!(
            "{}本地已有同名项目（{} 字节，修改时间 {:?}）。覆盖会替换它。",
            if changed {
                "目标在下载期间发生变化。"
            } else {
                ""
            },
            existing.len(),
            modified
        );
        let answer = control
            .ask(
                TransferQuestionKind::Conflict,
                &target.display().to_string(),
                &message,
            )
            .await?;
        if answer.apply_to_all() && !changed {
            self.all_conflicts = Some(answer.choice());
        }
        self.phase(TransferPhase::Transferring, control);
        Ok(answer.choice() == TransferChoice::Overwrite)
    }

    async fn download_item<F: RemoteFs>(
        &mut self,
        fs: &F,
        item: &DownloadItem,
        control: &TransferControl,
    ) -> Result<bool> {
        let existing = local_metadata(&item.target).await?;
        match item.metadata.kind() {
            EntryKind::Directory => match existing {
                Some(metadata) if metadata.is_dir() => Ok(true),
                Some(_) => bail!("本地已有同名文件，不会替换为目录"),
                None => {
                    tokio::fs::create_dir(&item.target).await?;
                    Ok(true)
                }
            },
            EntryKind::Symlink => {
                let link = control.run(fs.readlink(&item.source)).await?;
                if let Some(existing) = &existing
                    && !self.approve(&item.target, existing, false, control).await?
                {
                    return Ok(false);
                }
                create_link(&link, &item.target, existing.is_some()).await?;
                Ok(true)
            }
            EntryKind::Other => bail!("不支持下载设备、套接字或其他特殊文件"),
            EntryKind::File => self.download_regular(fs, item, existing, control).await,
        }
    }

    async fn download_regular<F: RemoteFs>(
        &mut self,
        fs: &F,
        item: &DownloadItem,
        existing: Option<std::fs::Metadata>,
        control: &TransferControl,
    ) -> Result<bool> {
        let temporary = partial_path(&item.target);
        let record = self
            .journal
            .load(&self.endpoint, &item.source, &item.target)
            .await?;
        let partial = local_metadata(&temporary).await?;
        let resumable = record.as_ref().is_some_and(|record| {
            record
                .validate(&self.endpoint, &self.host_key, &item.source, &item.target)
                .is_ok()
                && record.source_metadata == item.metadata
        });
        // Already answered in this batch: a reconnect retries the same item.
        let answered = self.approved_resumes.contains(&item.target);
        // A matching record means overwriting was approved when it was made.
        if !answered
            && !resumable
            && let Some(existing) = &existing
            && !self.approve(&item.target, existing, false, control).await?
        {
            return Ok(false);
        }
        let mut offset = 0;
        if let Some(partial) = &partial {
            if !partial.is_file() {
                bail!("临时文件 {} 不是普通文件", temporary.display());
            }
            if partial.len() > item.metadata.size() || (record.is_some() && !resumable) {
                self.phase(TransferPhase::Waiting, control);
                let reason = if partial.len() > item.metadata.size() {
                    "本地 .filepart 文件比远程文件大，无法续传。"
                } else {
                    "远程文件在上次下载后已变化，无法续传。"
                };
                let answer = control
                    .ask(
                        TransferQuestionKind::InvalidResume,
                        item.source.as_str(),
                        reason,
                    )
                    .await?;
                if answer.choice() != TransferChoice::Restart {
                    return Ok(false);
                }
            } else if answered {
                offset = partial.len();
            } else {
                self.phase(TransferPhase::Waiting, control);
                let answer = control
                    .ask(
                        TransferQuestionKind::Resume,
                        item.source.as_str(),
                        "发现未完成的 .filepart 文件。续传会按它的现有大小跳过远程文件前缀，请确认远程文件仍是同一版本。",
                    )
                    .await?;
                match answer.choice() {
                    TransferChoice::Skip => return Ok(false),
                    TransferChoice::Restart => {}
                    _ => offset = partial.len(),
                }
                self.phase(TransferPhase::Transferring, control);
            }
        }
        self.approved_resumes.insert(item.target.clone());
        let record = DownloadRecord::new(
            &self.endpoint,
            &self.host_key,
            item.source.clone(),
            item.metadata.clone(),
            item.target.clone(),
        );
        self.journal.save(&record).await?;
        self.download_file(fs, item, &temporary, offset, control)
            .await?;
        // The target may have appeared or changed while the file downloaded.
        let current = local_metadata(&item.target).await?;
        if let Some(current) = &current
            && changed(existing.as_ref(), current)
            && !self.approve(&item.target, current, true, control).await?
        {
            return Ok(false);
        }
        publish(&temporary, &item.target, current.as_ref(), &item.metadata).await?;
        self.journal.remove(&record).await?;
        Ok(true)
    }

    /// Read the remote file from `offset` into the partial file. Reads run
    /// in parallel and may complete out of order or short, but the file is
    /// written strictly in sequence: resuming trusts its length.
    async fn download_file<F: RemoteFs>(
        &mut self,
        fs: &F,
        item: &DownloadItem,
        temporary: &Path,
        offset: u64,
        control: &TransferControl,
    ) -> Result<()> {
        let size = item.metadata.size();
        let handle = control.run(fs.open_read(&item.source)).await?;
        let result: Result<()> = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(offset == 0)
                .open(temporary)
                .await?;
            file.set_len(offset).await?;
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut next_request = offset;
            let mut next_write = offset;
            let mut remainders: Vec<(u64, u32)> = Vec::new();
            let mut ready: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
            let mut reads = FuturesUnordered::new();
            self.phase(TransferPhase::Transferring, control);
            while next_write < size {
                while reads.len() < MAX_IN_FLIGHT && (!remainders.is_empty() || next_request < size)
                {
                    control.check()?;
                    let (at, len) = remainders.pop().unwrap_or_else(|| {
                        let len = (size - next_request).min(u64::from(CHUNK)) as u32;
                        let request = (next_request, len);
                        next_request += u64::from(len);
                        request
                    });
                    let handle = handle.as_str();
                    reads.push(async move {
                        let bytes = control.run(fs.read(handle, at, len)).await;
                        (at, len, bytes)
                    });
                }
                let (at, len, bytes) = reads
                    .next()
                    .await
                    .ok_or_else(|| anyhow!("下载读取流水线意外结束"))?;
                let bytes = match bytes? {
                    Some(bytes) if !bytes.is_empty() => bytes,
                    _ => bail!("远程文件在下载期间变短，已停止"),
                };
                let got = bytes.len().min(len as usize);
                if got < len as usize {
                    remainders.push((at + got as u64, len - got as u32));
                }
                ready.insert(at, bytes[..got].to_vec());
                while let Some(chunk) = ready.remove(&next_write) {
                    file.write_all(&chunk).await?;
                    next_write += chunk.len() as u64;
                    self.received_bytes += chunk.len() as u64;
                    self.progress.completed_bytes = self.completed_bytes.saturating_add(next_write);
                    self.progress.bytes_per_second = (self.received_bytes as f64
                        / self.started.elapsed().as_secs_f64().max(0.001))
                        as u64;
                    if self.last_progress.elapsed() >= Duration::from_millis(50) {
                        self.emit(control);
                        self.last_progress = Instant::now();
                    }
                }
            }
            file.flush().await?;
            file.sync_all().await?;
            Ok(())
        }
        .await;
        // Always close; the partial file's length is the resume point.
        let close = fs.close(&handle).await;
        result?;
        close?;
        Ok(())
    }

    /// Remove the partial files and resume records this batch left behind.
    pub async fn discard(&self) -> Result<()> {
        for item in &self.items {
            if let Some(record) = self
                .journal
                .load(&self.endpoint, &item.source, &item.target)
                .await?
            {
                record.validate(&self.endpoint, &self.host_key, &item.source, &item.target)?;
                match tokio::fs::remove_file(&record.temporary).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                self.journal.remove(&record).await?;
            }
        }
        Ok(())
    }
}

async fn local_metadata(path: &Path) -> Result<Option<std::fs::Metadata>> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Whether the local target is no longer what the user approved replacing.
fn changed(before: Option<&std::fs::Metadata>, now: &std::fs::Metadata) -> bool {
    match before {
        None => true,
        Some(before) => {
            before.len() != now.len()
                || before.modified().ok() != now.modified().ok()
                || before.file_type() != now.file_type()
        }
    }
}

/// Give the finished partial file the remote time, keep the replaced file's
/// permissions, and rename it over the target (atomic on one file system).
async fn publish(
    temporary: &Path,
    target: &Path,
    replaced: Option<&std::fs::Metadata>,
    metadata: &FileMetadata,
) -> Result<()> {
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(temporary)
        .await?
        .into_std()
        .await;
    if let Some(seconds) = metadata.modified() {
        file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(u64::from(seconds)))?;
    }
    if let Some(replaced) = replaced
        && replaced.is_file()
    {
        file.set_permissions(replaced.permissions())?;
    }
    file.sync_all()?;
    drop(file);
    tokio::fs::rename(temporary, target).await?;
    Ok(())
}

#[cfg(unix)]
async fn create_link(link: &str, target: &Path, replace: bool) -> Result<()> {
    if replace {
        tokio::fs::remove_file(target).await?;
    }
    tokio::fs::symlink(link, target).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn create_link(_: &str, _: &Path, _: bool) -> Result<()> {
    bail!("此系统不支持创建符号链接")
}
