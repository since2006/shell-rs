use super::speed::TransferClock;
use super::{
    EntryKind, FileMetadata, RemotePath, SftpEvent, TransferChoice, TransferDetail, TransferPhase,
    TransferProgress, TransferQuestionKind, UploadRequest,
    client::RemoteFs,
    control::{TargetGuard, TransferControl},
    file_size,
    journal::{Journal, PublishPhase, ResumeRecord, SourceMetadata},
    model::local_metadata,
};
use anyhow::{Result, anyhow, bail};
use futures::{StreamExt as _, stream::FuturesUnordered};
use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
const CHUNK: usize = 32 * 1024;
const MAX_IN_FLIGHT: usize = 16;

#[derive(Clone)]
struct UploadItem {
    source: PathBuf,
    target: RemotePath,
    metadata: FileMetadata,
    error: Option<String>,
}
pub(crate) struct UploadBatch {
    items: Vec<UploadItem>,
    cursor: usize,
    pub progress: TransferProgress,
    endpoint: String,
    host_key: String,
    journal: Journal,
    all_conflicts: Option<TransferChoice>,
    approved_resumes: HashSet<RemotePath>,
    blocked_directories: Vec<RemotePath>,
    clock: TransferClock,
    sent_bytes: u64,
    completed_bytes: u64,
    last_progress: Instant,
    /// A new remote file takes the local file's permission bits, as scp
    /// gives them; otherwise the server's defaults apply.
    preserve_mode: bool,
}
impl UploadBatch {
    pub async fn scan(
        request: &UploadRequest,
        endpoint: &str,
        host_key: &str,
        journal: Journal,
        control: &TransferControl,
    ) -> Result<Self> {
        let mut sources = Vec::new();
        for source in request.sources() {
            let absolute = std::path::absolute(source)?;
            // Resolve parent aliases while preserving the leaf symlink itself.
            let path = if let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name())
            {
                match tokio::fs::canonicalize(parent).await {
                    Ok(parent) => parent.join(name),
                    Err(_) => absolute,
                }
            } else {
                absolute
            };
            sources.push(path);
        }
        sources.sort();
        sources.dedup();
        let mut roots: Vec<PathBuf> = Vec::new();
        for source in sources {
            if !roots.iter().any(|parent| source.starts_with(parent)) {
                roots.push(source);
            }
        }
        let mut stack = Vec::new();
        for source in roots.into_iter().rev() {
            let name = match request.target_name() {
                Some(name) => name,
                None => source
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| anyhow!("来源文件名不是有效的 UTF-8"))?,
            };
            let target = request.destination().join(name)?;
            stack.push((source, target));
        }
        let mut items = Vec::new();
        let mut total_bytes: u64 = 0;
        while let Some((source, target)) = stack.pop() {
            control.check()?;
            let metadata = match tokio::fs::symlink_metadata(&source).await {
                Ok(metadata) => local_metadata(&metadata),
                Err(error) => {
                    items.push(UploadItem {
                        source,
                        target,
                        metadata: FileMetadata::new(EntryKind::Other, 0, None, None),
                        error: Some(error.to_string()),
                    });
                    continue;
                }
            };
            let mut scan_error = None;
            if metadata.kind() == EntryKind::Directory {
                let read: Result<Vec<(PathBuf, RemotePath)>> = async {
                    let mut dir = tokio::fs::read_dir(&source).await?;
                    let mut children = Vec::new();
                    while let Some(entry) = dir.next_entry().await? {
                        let name = entry
                            .file_name()
                            .into_string()
                            .map_err(|_| anyhow!("目录包含无法用 UTF-8 表示的文件名"))?;
                        children.push((entry.path(), target.join(&name)?));
                    }
                    children.sort_by(|a, b| a.0.cmp(&b.0));
                    Ok(children)
                }
                .await;
                match read {
                    Ok(children) => stack.extend(children.into_iter().rev()),
                    Err(e) => scan_error = Some(e.to_string()),
                }
            } else if metadata.kind() == EntryKind::File {
                total_bytes = total_bytes.saturating_add(metadata.size());
            }
            items.push(UploadItem {
                source,
                target,
                metadata,
                error: scan_error,
            });
        }
        let progress = TransferProgress {
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
            clock: TransferClock::default(),
            sent_bytes: 0,
            completed_bytes: 0,
            last_progress: Instant::now(),
            preserve_mode: request.is_scp(),
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
        let now = Instant::now();
        if phase == TransferPhase::Transferring {
            self.clock.run(now, self.sent_bytes);
        } else {
            self.clock.pause(now);
        }
        if phase != TransferPhase::Reconnecting {
            self.progress.note = None;
        }
        if phase == TransferPhase::Completed {
            self.progress.current.clear();
            self.progress.current_source.clear();
            self.progress.current_bytes = 0;
            self.progress.current_total = 0;
        }
        self.progress.phase = phase;
        self.progress.elapsed = self.clock.elapsed(now);
        self.progress.bytes_per_second = self.clock.speed();
        self.emit(control);
    }
    pub async fn run<F: RemoteFs>(&mut self, fs: &F, control: &TransferControl) -> Result<()> {
        self.phase(TransferPhase::Transferring, control);
        while self.cursor < self.items.len() {
            control.check()?;
            let item = self.items[self.cursor].clone();
            self.progress.current = item.target.to_string();
            self.progress.current_source = item.source.display().to_string();
            self.progress.current_bytes = 0;
            self.progress.current_total = file_size(&item.metadata);
            self.emit(control);
            let _target_guard = TargetGuard::acquire(
                format!("{}\0{}", self.endpoint, item.target),
                "其他会话正在上传同一目标，请稍后继续上传",
            )?;
            let result = if self
                .blocked_directories
                .iter()
                .any(|p| item.target.as_str().starts_with(&format!("{p}/")))
            {
                Err(anyhow!("父目录未创建，无法上传"))
            } else if let Some(error) = &item.error {
                Err(anyhow!("{error}"))
            } else {
                self.upload_item(fs, &item, control).await
            };
            match result {
                Ok(true) => {
                    self.progress.succeeded += 1;
                    self.progress
                        .details
                        .push(TransferDetail::done(item.target.to_string()));
                    if item.metadata.kind() == EntryKind::File {
                        self.completed_bytes =
                            self.completed_bytes.saturating_add(item.metadata.size());
                    }
                }
                Ok(false) => {
                    self.progress.skipped += 1;
                    self.progress.settled_bytes += file_size(&item.metadata);
                    self.progress
                        .details
                        .push(TransferDetail::skipped(item.target.to_string()));
                }
                Err(error)
                    if error.is::<super::control::Cancelled>()
                        || super::client::is_network_error(&error) =>
                {
                    return Err(error);
                }
                Err(error) => {
                    self.phase(TransferPhase::Waiting, control);
                    let answer = control
                        .ask(
                            TransferQuestionKind::Error,
                            item.target.as_str(),
                            &format!("无法上传：{error:#}"),
                        )
                        .await?;
                    if answer.choice() == TransferChoice::Retry {
                        if item.error.is_some() {
                            let request = UploadRequest::new(
                                vec![item.source.clone()],
                                item.target.parent(),
                            )?;
                            let scanned = Self::scan(
                                &request,
                                &self.endpoint,
                                &self.host_key,
                                self.journal.clone(),
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
                    self.progress.settled_bytes += file_size(&item.metadata);
                    self.progress.details.push(TransferDetail::failed(
                        item.target.to_string(),
                        format!("{error:#}"),
                    ));
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
    async fn approve(
        &mut self,
        target: &RemotePath,
        original: &Option<FileMetadata>,
        changed: bool,
        control: &TransferControl,
    ) -> Result<bool> {
        if original.is_none() && !changed {
            return Ok(true);
        }
        if original
            .as_ref()
            .is_some_and(|m| m.kind() == EntryKind::Directory)
        {
            bail!("目录与文件类型冲突，不会删除远程目录");
        }
        if !changed && let Some(choice) = self.all_conflicts {
            return Ok(choice == TransferChoice::Overwrite);
        }
        self.phase(TransferPhase::Waiting, control);
        let message = match original {
            Some(metadata) => format!(
                "{}远程项目已存在（{} 字节，修改时间 {:?}）。覆盖会替换这个文件或链接。",
                if changed {
                    "目标在上传期间发生变化。"
                } else {
                    ""
                },
                metadata.size(),
                metadata.modified()
            ),
            None => "原目标在上传期间已被删除或移走。是否继续发布此文件？".into(),
        };
        let answer = control
            .ask(TransferQuestionKind::Conflict, target.as_str(), &message)
            .await?;
        if answer.apply_to_all() && !changed {
            self.all_conflicts = Some(answer.choice());
        }
        self.phase(TransferPhase::Transferring, control);
        Ok(answer.choice() == TransferChoice::Overwrite)
    }
    async fn upload_item<F: RemoteFs>(
        &mut self,
        fs: &F,
        item: &UploadItem,
        control: &TransferControl,
    ) -> Result<bool> {
        let existing_record = self
            .journal
            .load(&self.endpoint, &item.source, &item.target)
            .await?;
        if item.metadata.kind() == EntryKind::Directory {
            if let Some(record) = &existing_record {
                record.validate(&self.endpoint, &self.host_key, &item.source, &item.target)?;
                self.phase(TransferPhase::Waiting, control);
                let answer = control
                    .ask(
                        TransferQuestionKind::InvalidResume,
                        item.target.as_str(),
                        "来源已变为目录，请丢弃旧文件进度后重新上传，或跳过。",
                    )
                    .await?;
                if answer.choice() != TransferChoice::Restart {
                    return Ok(false);
                }
                self.discard_record(fs, record).await?;
            }
            return match control.run(fs.metadata(&item.target)).await? {
                Some(metadata) if metadata.kind() == EntryKind::Directory => Ok(true),
                Some(_) => bail!("目标已存在且不是目录，不会跟随或替换链接"),
                None => {
                    control.run(fs.mkdir(&item.target)).await?;
                    Ok(true)
                }
            };
        }
        if item.metadata.kind() == EntryKind::Other {
            bail!("不支持上传设备、套接字或其他特殊文件");
        }
        let mut record = if let Some(record) = existing_record {
            record.validate(&self.endpoint, &self.host_key, &item.source, &item.target)?;
            if !self.approved_resumes.contains(&item.target) {
                self.phase(TransferPhase::Waiting, control);
                let answer = control
                    .ask(
                        TransferQuestionKind::Resume,
                        item.target.as_str(),
                        "发现未完成的 .filepart 文件。续传会按它的现有大小跳过本地文件前缀，请确认本地文件仍是同一版本。",
                    )
                    .await?;
                match answer.choice() {
                    TransferChoice::Skip => return Ok(false),
                    TransferChoice::Restart => {
                        self.discard_record(fs, &record).await?;
                        return Box::pin(self.upload_item(fs, item, control)).await;
                    }
                    _ => {}
                }
            }
            record
        } else {
            let original = control.run(fs.metadata(&item.target)).await?;
            if !self
                .approve(&item.target, &original, false, control)
                .await?
            {
                return Ok(false);
            }
            let mut record = ResumeRecord::new(
                &self.endpoint,
                &self.host_key,
                &item.source,
                item.target.clone(),
                original,
            )?;
            if item.metadata.kind() == EntryKind::File {
                record.source_metadata =
                    Some(control.run(SourceMetadata::read(&item.source)).await?);
                if control.run(fs.metadata(&record.temporary)).await?.is_some() {
                    self.phase(TransferPhase::Waiting, control);
                    let answer = control
                        .ask(
                            TransferQuestionKind::Resume,
                            item.target.as_str(),
                            "发现未完成的 .filepart 文件。续传会按它的现有大小跳过本地文件前缀，请确认本地文件仍是同一版本。",
                        )
                        .await?;
                    match answer.choice() {
                        TransferChoice::Skip => return Ok(false),
                        TransferChoice::Restart => {
                            control.run(fs.remove(&record.temporary)).await?;
                        }
                        _ => {}
                    }
                    self.phase(TransferPhase::Transferring, control);
                }
            } else {
                record.link_target = Some(
                    tokio::fs::read_link(&item.source)
                        .await?
                        .into_os_string()
                        .into_string()
                        .map_err(|_| anyhow!("链接目标不是有效的 UTF-8"))?,
                );
            }
            self.journal.save(&record).await?;
            record
        };
        self.approved_resumes.insert(item.target.clone());
        if record.phase != PublishPhase::Writing {
            return self.recover_publish(fs, &mut record, control).await;
        }
        let uploaded = if item.metadata.kind() == EntryKind::File {
            record.source_metadata = Some(control.run(SourceMetadata::read(&item.source)).await?);
            self.journal.save(&record).await?;
            self.upload_file(fs, item, &mut record, control).await
        } else {
            let target = record
                .link_target
                .as_ref()
                .ok_or_else(|| anyhow!("续传记录中缺少链接目标"))?;
            if let Some(existing) = control.run(fs.metadata(&record.temporary)).await? {
                if existing.kind() != EntryKind::Symlink
                    || control.run(fs.readlink(&record.temporary)).await? != *target
                {
                    bail!("临时链接已变化，无法继续上传");
                }
            } else {
                control.run(fs.symlink(target, &record.temporary)).await?;
            }
            Ok(())
        };
        if let Err(error) = uploaded {
            if error.is::<InvalidResume>() {
                self.phase(TransferPhase::Waiting, control);
                let answer = control
                    .ask(
                        TransferQuestionKind::InvalidResume,
                        item.target.as_str(),
                        &error.to_string(),
                    )
                    .await?;
                if answer.choice() == TransferChoice::Restart {
                    self.discard_record(fs, &record).await?;
                    return Box::pin(self.upload_item(fs, item, control)).await;
                }
                return Ok(false);
            }
            return Err(error);
        }
        record.phase = PublishPhase::Ready;
        self.journal.save(&record).await?;
        self.recover_publish(fs, &mut record, control).await
    }
    async fn upload_file<F: RemoteFs>(
        &mut self,
        fs: &F,
        item: &UploadItem,
        record: &mut ResumeRecord,
        control: &TransferControl,
    ) -> Result<()> {
        let source_metadata = record
            .source_metadata
            .clone()
            .ok_or_else(|| anyhow!("续传记录中缺少来源文件信息"))?;
        let remote = control.run(fs.metadata(&record.temporary)).await?;
        if remote.as_ref().is_some_and(|m| m.kind() != EntryKind::File) {
            return Err(InvalidResume("远端临时文件类型已变化").into());
        }
        let resume_offset = remote.as_ref().map(FileMetadata::size).unwrap_or(0);
        if resume_offset > source_metadata.size {
            return Err(InvalidResume("远端续传偏移大于本地文件").into());
        }
        let handle = control
            .run(fs.open(&record.temporary, remote.is_none()))
            .await?;
        let result: Result<()> = async {
            let mut local = tokio::fs::File::open(&item.source).await?;
            local.seek(std::io::SeekFrom::Start(resume_offset)).await?;
            self.phase(TransferPhase::Transferring, control);
            let mut next_offset = resume_offset;
            let mut uploaded = resume_offset;
            self.progress.current_bytes = uploaded;
            let mut writes = FuturesUnordered::new();
            while next_offset < source_metadata.size || !writes.is_empty() {
                while next_offset < source_metadata.size && writes.len() < MAX_IN_FLIGHT {
                    control.check()?;
                    let length = (source_metadata.size - next_offset).min(CHUNK as u64) as usize;
                    let mut bytes = vec![0; length];
                    local.read_exact(&mut bytes).await?;
                    let write_offset = next_offset;
                    next_offset += length as u64;
                    let handle = handle.as_str();
                    writes.push(async move {
                        control.run(fs.write(handle, write_offset, bytes)).await?;
                        Ok::<_, anyhow::Error>(length as u64)
                    });
                }
                let length = writes
                    .next()
                    .await
                    .ok_or_else(|| anyhow!("上传写入流水线意外结束"))??;
                self.sent_bytes += length;
                uploaded += length;
                let now = Instant::now();
                self.clock.record(now, self.sent_bytes);
                self.progress.completed_bytes = self.completed_bytes.saturating_add(uploaded);
                self.progress.current_bytes = uploaded;
                self.progress.bytes_per_second = self.clock.speed();
                self.progress.elapsed = self.clock.elapsed(now);
                if self.last_progress.elapsed() >= Duration::from_millis(50) {
                    self.emit(control);
                    self.last_progress = Instant::now();
                }
            }
            control
                .run(
                    fs.attributes(
                        &handle,
                        u32::try_from(source_metadata.modified_ns / 1_000_000_000).ok(),
                        record
                            .original
                            .as_ref()
                            .and_then(FileMetadata::permissions)
                            .or_else(|| {
                                self.preserve_mode
                                    .then(|| item.metadata.permissions())
                                    .flatten()
                                    .map(|mode| mode & 0o777)
                            }),
                    ),
                )
                .await?;
            control.run(fs.sync(&handle)).await?;
            Ok(())
        }
        .await;
        // Always close. After a broken connection, WinSCP-style resume uses the
        // .filepart size reported by the server as the next source offset.
        let close = fs.close(&handle).await;
        result?;
        close?;
        Ok(())
    }
    async fn matches_expected_item<F: RemoteFs>(
        &self,
        fs: &F,
        path: &RemotePath,
        record: &ResumeRecord,
        control: &TransferControl,
    ) -> Result<bool> {
        let Some(metadata) = control.run(fs.metadata(path)).await? else {
            return Ok(false);
        };
        if let Some(target) = &record.link_target {
            return Ok(metadata.kind() == EntryKind::Symlink
                && control.run(fs.readlink(path)).await? == *target);
        }
        let Some(source_metadata) = &record.source_metadata else {
            return Ok(false);
        };
        Ok(metadata.kind() == EntryKind::File && metadata.size() == source_metadata.size)
    }
    async fn recover_publish<F: RemoteFs>(
        &mut self,
        fs: &F,
        record: &mut ResumeRecord,
        control: &TransferControl,
    ) -> Result<bool> {
        let temporary = control.run(fs.metadata(&record.temporary)).await?;
        if temporary.is_none() {
            if matches!(
                record.phase,
                PublishPhase::Publishing | PublishPhase::Published
            ) && self
                .matches_expected_item(fs, &record.target, record, control)
                .await?
            {
                self.cleanup(fs, record).await?;
                return Ok(true);
            }
            // We cannot infer success from a lost reply. A backup may still be
            // the only copy of the old file; restore only into an absent target.
            if fs.metadata(&record.target).await?.is_none()
                && fs.metadata(&record.backup).await?.is_some()
            {
                fs.rename(&record.backup, &record.target, false).await?;
            }
            bail!("无法确认文件发布结果，已保留恢复文件；请检查远端目录");
        }
        if !self
            .matches_expected_item(fs, &record.temporary, record, control)
            .await?
        {
            bail!("临时文件大小或类型已变化，未替换目标");
        }
        let backup = control.run(fs.metadata(&record.backup)).await?;
        let current = control.run(fs.metadata(&record.target)).await?;
        if backup.is_some() {
            if current.is_some() {
                bail!("替换期间目标被其他操作创建，已保留目标、备份和临时文件");
            }
            if backup != record.original {
                bail!("恢复备份已变化，停止替换");
            }
            record.phase = PublishPhase::BackedUp;
            self.journal.save(record).await?;
        } else if current != record.original {
            if !self
                .approve(&record.target, &current, true, control)
                .await?
            {
                self.discard_record(fs, record).await?;
                return Ok(false);
            }
            record.original = current;
            self.journal.save(record).await?;
            if let Some(source_metadata) = &record.source_metadata {
                let handle = control.run(fs.open(&record.temporary, false)).await?;
                let result = control
                    .run(fs.attributes(
                        &handle,
                        u32::try_from(source_metadata.modified_ns / 1_000_000_000).ok(),
                        record.original.as_ref().and_then(FileMetadata::permissions),
                    ))
                    .await;
                let closed = fs.close(&handle).await;
                result?;
                closed?;
            }
        }
        if backup.is_none() && record.original.is_some() && !fs.atomic_replace() {
            record.phase = PublishPhase::BackingUp;
            self.journal.save(record).await?;
            control
                .run(fs.rename(&record.target, &record.backup, false))
                .await?;
            record.phase = PublishPhase::BackedUp;
            self.journal.save(record).await?;
        }
        record.phase = PublishPhase::Publishing;
        self.journal.save(record).await?;
        let result = control
            .run(fs.rename(
                &record.temporary,
                &record.target,
                fs.atomic_replace() && record.original.is_some() && backup.is_none(),
            ))
            .await;
        if let Err(error) = result {
            if !super::client::is_network_error(&error)
                && !error.is::<super::control::Cancelled>()
                && fs.metadata(&record.target).await?.is_none()
                && fs.metadata(&record.backup).await?.is_some()
            {
                fs.rename(&record.backup, &record.target, false).await?;
                record.phase = PublishPhase::Ready;
                self.journal.save(record).await?;
            }
            return Err(error);
        }
        record.phase = PublishPhase::Published;
        self.journal.save(record).await?;
        self.cleanup(fs, record).await?;
        Ok(true)
    }
    async fn cleanup<F: RemoteFs>(&self, fs: &F, record: &ResumeRecord) -> Result<()> {
        if let Some(backup) = fs.metadata(&record.backup).await? {
            if Some(backup) != record.original {
                bail!("备份已变化，未清理；上传的目标文件已发布");
            }
            fs.remove(&record.backup).await?;
        }
        self.journal.remove(record).await
    }
    async fn discard_record<F: RemoteFs>(&self, fs: &F, record: &ResumeRecord) -> Result<()> {
        record.validate(
            &self.endpoint,
            &self.host_key,
            &record.source,
            &record.target,
        )?;
        if fs.metadata(&record.backup).await?.is_some() {
            if fs.metadata(&record.target).await?.is_none() {
                fs.rename(&record.backup, &record.target, false).await?;
            } else {
                bail!("存在恢复备份和目标文件，请先检查远端目录；未删除任何文件");
            }
        }
        fs.remove(&record.temporary).await?;
        self.journal.remove(record).await
    }
    pub async fn discard<F: RemoteFs>(&self, fs: &F) -> Result<()> {
        for item in &self.items {
            if let Some(record) = self
                .journal
                .load(&self.endpoint, &item.source, &item.target)
                .await?
            {
                self.discard_record(fs, &record).await?;
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
struct InvalidResume(&'static str);
impl std::fmt::Display for InvalidResume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidResume {}
