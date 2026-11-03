use super::{
    EntryKind, FileMetadata, RESUME_THRESHOLD, Reason, RemotePath, SftpError, TransferAnswer,
    TransferChoice, TransferDetail, TransferDirection, TransferPhase, TransferQuestionKind,
    UploadError, UploadRequest,
    client::{RemoteFs, is_network_error},
    control::{Cancelled, TargetGuard, TransferControl},
    file_size,
    journal::{Journal, PublishPhase, ResumeRecord, SourceMetadata},
    meter::{FileId, TransferMeter},
    model::local_metadata,
    parallel,
};
use anyhow::{Result, anyhow, bail};
use futures::{StreamExt as _, stream::FuturesUnordered};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
const CHUNK: usize = 32 * 1024;
const MAX_IN_FLIGHT: usize = 16;

#[derive(Clone)]
struct UploadItem {
    source: PathBuf,
    target: RemotePath,
    metadata: FileMetadata,
    /// Why the scan could not tell what to upload.
    error: Option<Reason>,
}
pub(crate) struct UploadBatch {
    items: Vec<UploadItem>,
    /// The items not settled yet, in the order they go: a folder before
    /// what is in it.
    pending: VecDeque<usize>,
    pub meter: TransferMeter,
    endpoint: String,
    host_key: String,
    journal: Journal,
    all_conflicts: Option<TransferChoice>,
    approved_resumes: HashSet<RemotePath>,
    blocked_directories: Vec<RemotePath>,
    /// Folders this batch made. Nothing is in them yet, so what goes in is
    /// not looked for first, as WinSCP skips the look in a new directory.
    made_directories: HashSet<RemotePath>,
    /// Small files this batch opened at their own name, and whether it made
    /// them. Another try writes them over without asking again; set aside
    /// part way, they are removed.
    written: HashMap<RemotePath, bool>,
    /// A new remote file takes the local file's permission bits, as scp
    /// gives them; otherwise the server's defaults apply.
    preserve_mode: bool,
    /// The external CLI's sync: what is already there as it is here is
    /// left alone, folders included, and counted as skipped.
    sync: bool,
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
                    // Without Windows' `\\?\`, as the queue shows it.
                    Ok(parent) => dunce::simplified(&parent).join(name),
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
                    .ok_or_else(|| anyhow!(UploadError::SourceNameNotUtf8))?,
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
                        error: Some(anyhow::Error::from(error).into()),
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
                            .map_err(|_| anyhow!(SftpError::FileNameNotUtf8))?;
                        children.push((entry.path(), target.join(&name)?));
                    }
                    children.sort_by(|a, b| a.0.cmp(&b.0));
                    Ok(children)
                }
                .await;
                match read {
                    Ok(children) => stack.extend(children.into_iter().rev()),
                    Err(e) => scan_error = Some(e.into()),
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
        Ok(Self {
            meter: TransferMeter::new(TransferDirection::Upload, items.len(), total_bytes),
            pending: (0..items.len()).collect(),
            items,
            endpoint: endpoint.into(),
            host_key: host_key.into(),
            journal,
            all_conflicts: None,
            approved_resumes: HashSet::new(),
            blocked_directories: Vec::new(),
            made_directories: HashSet::new(),
            written: HashMap::new(),
            preserve_mode: request.is_scp() || request.sync_deletes().is_some(),
            sync: request.sync_deletes().is_some(),
        })
    }

    /// Count what a sync deleted before the batch was scanned, and what it
    /// failed to delete among the items that failed.
    pub fn add_pruned(&mut self, deleted: usize, failures: Vec<TransferDetail>) {
        let progress = &mut self.meter.progress;
        progress.deleted += deleted;
        progress.total += failures.len();
        progress.failed += failures.len();
        progress.details.extend(failures);
    }
    pub fn verify_host(&self, fingerprint: &str) -> Result<()> {
        if self.host_key != fingerprint {
            bail!(SftpError::HostKeyChanged);
        }
        Ok(())
    }
    pub fn is_complete(&self) -> bool {
        self.pending.is_empty()
    }
    pub async fn run<F: RemoteFs>(&mut self, fs: &F, control: &TransferControl) -> Result<()> {
        self.meter.phase(TransferPhase::Transferring, control);
        // Its borrows of the batch end with the block.
        let result = {
            let upload = Upload {
                fs,
                control,
                endpoint: &self.endpoint,
                host_key: &self.host_key,
                journal: &self.journal,
                preserve_mode: self.preserve_mode,
                sync: self.sync,
                items: RefCell::new(&mut self.items),
                meter: RefCell::new(&mut self.meter),
                all_conflicts: RefCell::new(&mut self.all_conflicts),
                approved_resumes: RefCell::new(&mut self.approved_resumes),
                blocked_directories: RefCell::new(&mut self.blocked_directories),
                made_directories: RefCell::new(&mut self.made_directories),
                written: RefCell::new(&mut self.written),
                asking: tokio::sync::Mutex::new(()),
            };
            parallel::run(
                &mut self.pending,
                parallel::parallel_transfers,
                |next, running| upload.waits(next, running),
                |index| upload.item(index),
            )
            .await
        };
        self.meter.abandon();
        result?;
        self.meter.phase(TransferPhase::Completed, control);
        Ok(())
    }
    pub async fn discard<F: RemoteFs>(&self, fs: &F) -> Result<()> {
        for item in &self.items {
            if let Some(record) = self
                .journal
                .load(&self.endpoint, &item.source, &item.target)
                .await?
            {
                discard_record(fs, &self.journal, &self.endpoint, &self.host_key, &record).await?;
            }
        }
        // A small file cut off part way is of no use, and nothing resumes it:
        // it goes, as WinSCP removes one.
        for &index in &self.pending {
            let target = &self.items[index].target;
            if self.written.contains_key(target) {
                fs.remove(target).await?;
            }
        }
        Ok(())
    }
}

/// One run of a batch: what its items share while several are in flight.
struct Upload<'a, R> {
    fs: &'a R,
    control: &'a TransferControl,
    endpoint: &'a str,
    host_key: &'a str,
    journal: &'a Journal,
    preserve_mode: bool,
    sync: bool,
    items: RefCell<&'a mut Vec<UploadItem>>,
    meter: RefCell<&'a mut TransferMeter>,
    all_conflicts: RefCell<&'a mut Option<TransferChoice>>,
    approved_resumes: RefCell<&'a mut HashSet<RemotePath>>,
    blocked_directories: RefCell<&'a mut Vec<RemotePath>>,
    made_directories: RefCell<&'a mut HashSet<RemotePath>>,
    written: RefCell<&'a mut HashMap<RemotePath, bool>>,
    /// One question at a time: the other items with one wait their turn.
    asking: tokio::sync::Mutex<()>,
}

impl<R: RemoteFs> Upload<'_, R> {
    /// Whether `next` goes in a folder that is still being made.
    fn waits(&self, next: usize, running: &[usize]) -> bool {
        let items = self.items.borrow();
        let parent = items[next].target.parent();
        running.iter().any(|&index| {
            items[index].metadata.kind() == EntryKind::Directory && items[index].target == parent
        })
    }

    /// Upload one item to its end: done, skipped, or failed as the user
    /// chose. Returns what a folder scanned again on 重试 found, to go next.
    async fn item(&self, index: usize) -> Result<Vec<usize>> {
        let item = self.items.borrow()[index].clone();
        let size = file_size(&item.metadata);
        let file = self.meter.borrow_mut().begin(
            item.source.display().to_string(),
            item.target.to_string(),
            size,
            self.control,
        );
        let _target_guard = TargetGuard::acquire(
            format!("{}\0{}", self.endpoint, item.target),
            UploadError::TargetBusy,
        )?;
        loop {
            let blocked = self
                .blocked_directories
                .borrow()
                .iter()
                .any(|p| item.target.as_str().starts_with(&format!("{p}/")));
            let reason = if blocked {
                Reason::new(UploadError::ParentMissing)
            } else if let Some(reason) = &item.error {
                reason.clone()
            } else {
                match self.upload_item(&item, file).await {
                    Ok(done) => {
                        let target = item.target.to_string();
                        let detail = if done {
                            TransferDetail::done(target)
                        } else {
                            TransferDetail::skipped(target)
                        };
                        self.meter
                            .borrow_mut()
                            .settle(file, detail, size, self.control);
                        return Ok(Vec::new());
                    }
                    Err(error) if error.is::<Cancelled>() || is_network_error(&error) => {
                        return Err(error);
                    }
                    Err(error) => Reason::from(error),
                }
            };
            let answer = self
                .ask(
                    TransferQuestionKind::Error,
                    item.target.as_str(),
                    reason.under(UploadError::Failed),
                )
                .await?;
            if answer.choice() == TransferChoice::Retry {
                if item.error.is_none() {
                    continue;
                }
                let request = UploadRequest::new(vec![item.source.clone()], item.target.parent())?;
                let scanned = UploadBatch::scan(
                    &request,
                    self.endpoint,
                    self.host_key,
                    self.journal.clone(),
                    self.control,
                )
                .await?;
                // What the scan had counted for this item goes; a folder
                // counted for nothing.
                let mut meter = self.meter.borrow_mut();
                meter.forget(file);
                let progress = &mut meter.progress;
                progress.total = progress.total.saturating_sub(1) + scanned.items.len();
                progress.total_bytes =
                    progress.total_bytes.saturating_sub(size) + scanned.meter.progress.total_bytes;
                let mut items = self.items.borrow_mut();
                let first = items.len();
                items.extend(scanned.items);
                return Ok((first..items.len()).collect());
            }
            self.meter.borrow_mut().settle(
                file,
                TransferDetail::failed(item.target.to_string(), reason),
                size,
                self.control,
            );
            if item.metadata.kind() == EntryKind::Directory {
                self.blocked_directories
                    .borrow_mut()
                    .push(item.target.clone());
            }
            return Ok(Vec::new());
        }
    }

    /// Ask about an item, when no other item is being asked about.
    async fn ask(
        &self,
        kind: TransferQuestionKind,
        path: &str,
        message: Reason,
    ) -> Result<TransferAnswer> {
        let _turn = self.asking.lock().await;
        self.question(kind, path, message).await
    }

    /// Ask, holding the turn already.
    async fn question(
        &self,
        kind: TransferQuestionKind,
        path: &str,
        message: Reason,
    ) -> Result<TransferAnswer> {
        self.meter
            .borrow_mut()
            .phase(TransferPhase::Waiting, self.control);
        let answer = self.control.ask(kind, path, message).await?;
        self.meter
            .borrow_mut()
            .phase(TransferPhase::Transferring, self.control);
        Ok(answer)
    }

    async fn approve(
        &self,
        target: &RemotePath,
        original: &Option<FileMetadata>,
        changed: bool,
    ) -> Result<bool> {
        if original.is_none() && !changed {
            return Ok(true);
        }
        if original
            .as_ref()
            .is_some_and(|m| m.kind() == EntryKind::Directory)
        {
            bail!(UploadError::FolderInTheWay);
        }
        let for_all = || (!changed).then(|| **self.all_conflicts.borrow()).flatten();
        if let Some(choice) = for_all() {
            return Ok(choice == TransferChoice::Overwrite);
        }
        let _turn = self.asking.lock().await;
        // Answered for all while this one waited its turn.
        if let Some(choice) = for_all() {
            return Ok(choice == TransferChoice::Overwrite);
        }
        let message = match original {
            Some(metadata) => {
                let (size, modified) = (metadata.size(), metadata.modified().map(u64::from));
                if changed {
                    UploadError::ExistsChanged { size, modified }
                } else {
                    UploadError::Exists { size, modified }
                }
            }
            None => UploadError::TargetGone,
        };
        let answer = self
            .question(
                TransferQuestionKind::Conflict,
                target.as_str(),
                Reason::new(message),
            )
            .await?;
        if answer.apply_to_all() && !changed {
            **self.all_conflicts.borrow_mut() = Some(answer.choice());
        }
        Ok(answer.choice() == TransferChoice::Overwrite)
    }

    async fn upload_item(&self, item: &UploadItem, file: FileId) -> Result<bool> {
        let existing_record = self
            .journal
            .load(self.endpoint, &item.source, &item.target)
            .await?;
        if item.metadata.kind() == EntryKind::Directory {
            if let Some(record) = &existing_record {
                record.validate(self.endpoint, self.host_key, &item.source, &item.target)?;
                let answer = self
                    .ask(
                        TransferQuestionKind::InvalidResume,
                        item.target.as_str(),
                        Reason::new(UploadError::SourceNowFolder),
                    )
                    .await?;
                if answer.choice() != TransferChoice::Restart {
                    return Ok(false);
                }
                self.discard_record(record).await?;
            }
            return self.make_directory(item).await;
        }
        if item.metadata.kind() == EntryKind::Other {
            bail!(UploadError::SpecialFile);
        }
        let mut approved = false;
        if existing_record.is_none()
            && item.metadata.kind() == EntryKind::File
            && item.metadata.size() < RESUME_THRESHOLD
        {
            match self.upload_small(item, file).await? {
                Small::Settled(done) => return Ok(done),
                Small::Replace { approved: answered } => approved = answered,
            }
        }
        self.upload_resumable(item, existing_record, approved, file)
            .await
    }

    /// Make a folder, unless it is there already; in a folder this batch
    /// made, it cannot be.
    async fn make_directory(&self, item: &UploadItem) -> Result<bool> {
        let (fs, control) = (self.fs, self.control);
        if !self
            .made_directories
            .borrow()
            .contains(&item.target.parent())
        {
            match control.run(fs.metadata(&item.target)).await? {
                Some(metadata) if metadata.kind() == EntryKind::Directory => return Ok(!self.sync),
                Some(_) => bail!(UploadError::TargetNotFolder),
                None => {}
            }
        }
        if let Err(error) = control.run(fs.mkdir(&item.target)).await {
            if is_network_error(&error) || error.is::<Cancelled>() {
                return Err(error);
            }
            // Made by an earlier try whose reply was lost.
            if !control
                .run(fs.metadata(&item.target))
                .await?
                .is_some_and(|metadata| metadata.kind() == EntryKind::Directory)
            {
                return Err(error);
            }
        }
        self.made_directories
            .borrow_mut()
            .insert(item.target.clone());
        Ok(true)
    }

    /// A file under [`RESUME_THRESHOLD`], sent as WinSCP sends one: straight
    /// to its name, opened exclusively so that a new file needs no look
    /// first. Only when that fails is what is there looked at and asked
    /// about, and a file then written over in place. A link in the way, or
    /// a file that cannot be written over (a read-only one), is replaced the
    /// resumable way instead, which moves it aside.
    async fn upload_small(&self, item: &UploadItem, file: FileId) -> Result<Small> {
        let (fs, control, target) = (self.fs, self.control, &item.target);
        let again = self.written.borrow().get(target).copied();
        let (handle, made) = if let Some(made) = again {
            (control.run(fs.open_replace(target)).await?, made)
        } else {
            let existing = if self.sync {
                // A sync looks first, to leave what is the same alone.
                let existing = control.run(fs.metadata(target)).await?;
                if self.unchanged(item, existing.as_ref()).await? {
                    return Ok(Small::Settled(false));
                }
                existing
            } else {
                match control.run(fs.open(target, true)).await {
                    Ok(handle) => {
                        self.written.borrow_mut().insert(target.clone(), true);
                        let done = self.write_small(item, &handle, true, file).await?;
                        return Ok(Small::Settled(done));
                    }
                    Err(error) if is_network_error(&error) || error.is::<Cancelled>() => {
                        return Err(error);
                    }
                    // There already, or refused: look to tell which.
                    Err(error) => match control.run(fs.metadata(target)).await? {
                        Some(existing) => Some(existing),
                        None => return Err(error),
                    },
                }
            };
            match &existing {
                None => (control.run(fs.open(target, true)).await?, true),
                Some(metadata)
                    if matches!(metadata.kind(), EntryKind::File | EntryKind::Directory) =>
                {
                    if !self.approve(target, &existing, false).await? {
                        return Ok(Small::Settled(false));
                    }
                    match control.run(fs.open_replace(target)).await {
                        Ok(handle) => (handle, false),
                        Err(error) if is_network_error(&error) || error.is::<Cancelled>() => {
                            return Err(error);
                        }
                        Err(_) => return Ok(Small::Replace { approved: true }),
                    }
                }
                Some(_) => return Ok(Small::Replace { approved: false }),
            }
        };
        self.written.borrow_mut().insert(target.clone(), made);
        let done = self.write_small(item, &handle, made, file).await?;
        Ok(Small::Settled(done))
    }

    /// Write a small file at the handle opened for it, then close it and set
    /// its time (and, when it is new and scp's, its mode) in one go, as
    /// WinSCP sends the two together. One that fails part way is removed,
    /// as WinSCP removes it: nothing resumes it.
    async fn write_small(
        &self,
        item: &UploadItem,
        handle: &str,
        made: bool,
        file: FileId,
    ) -> Result<bool> {
        let fs = self.fs;
        let sent = async {
            let mut local = tokio::fs::File::open(&item.source).await?;
            let metadata = local_metadata(&local.metadata().await?);
            self.send(&mut local, handle, 0, metadata.size(), file)
                .await?;
            Ok::<_, anyhow::Error>(metadata)
        }
        .await;
        let metadata = match sent {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = fs.close(handle).await;
                if !is_network_error(&error) {
                    let _ = fs.remove(&item.target).await;
                }
                return Err(error);
            }
        };
        let permissions = (made && self.preserve_mode)
            .then(|| metadata.permissions())
            .flatten()
            .map(|mode| mode & 0o777);
        let (closed, set) = futures::join!(
            fs.close(handle),
            fs.attributes(&item.target, metadata.modified(), permissions)
        );
        closed?;
        set?;
        Ok(true)
    }

    /// Send `size` bytes of `local` from `offset` on to the remote `handle`,
    /// several writes at once.
    async fn send(
        &self,
        local: &mut tokio::fs::File,
        handle: &str,
        offset: u64,
        size: u64,
        file: FileId,
    ) -> Result<()> {
        let (fs, control) = (self.fs, self.control);
        local.seek(std::io::SeekFrom::Start(offset)).await?;
        self.meter.borrow_mut().advance(file, 0, offset, control);
        let mut next_offset = offset;
        let mut sent = offset;
        let mut writes = FuturesUnordered::new();
        while next_offset < size || !writes.is_empty() {
            while next_offset < size && writes.len() < MAX_IN_FLIGHT {
                control.check()?;
                let length = (size - next_offset).min(CHUNK as u64) as usize;
                let mut bytes = vec![0; length];
                local.read_exact(&mut bytes).await?;
                let write_offset = next_offset;
                next_offset += length as u64;
                writes.push(async move {
                    control.run(fs.write(handle, write_offset, bytes)).await?;
                    Ok::<_, anyhow::Error>(length as u64)
                });
            }
            let length = writes
                .next()
                .await
                .ok_or_else(|| anyhow!(UploadError::PipelineEnded))??;
            sent += length;
            self.meter.borrow_mut().advance(file, length, sent, control);
        }
        Ok(())
    }

    /// A file of [`RESUME_THRESHOLD`] or more, or a link: written to
    /// `<name>.filepart` under a resume record, then put in place.
    /// `approved` when replacing what is there was agreed to already.
    async fn upload_resumable(
        &self,
        item: &UploadItem,
        existing_record: Option<ResumeRecord>,
        approved: bool,
        file: FileId,
    ) -> Result<bool> {
        let (fs, control) = (self.fs, self.control);
        // What is at the temporary name, when this try looked already.
        let mut partial = None;
        let mut record = if let Some(record) = existing_record {
            record.validate(self.endpoint, self.host_key, &item.source, &item.target)?;
            if !self.approved_resumes.borrow().contains(&item.target) {
                let answer = self
                    .ask(
                        TransferQuestionKind::Resume,
                        item.target.as_str(),
                        Reason::new(UploadError::ResumeFound),
                    )
                    .await?;
                match answer.choice() {
                    TransferChoice::Skip => return Ok(false),
                    TransferChoice::Restart => {
                        self.discard_record(&record).await?;
                        return Box::pin(self.upload_item(item, file)).await;
                    }
                    _ => {}
                }
            }
            record
        } else {
            // In a folder this batch made there is nothing to look for.
            let new_folder = self
                .made_directories
                .borrow()
                .contains(&item.target.parent());
            let original = if new_folder {
                None
            } else {
                control.run(fs.metadata(&item.target)).await?
            };
            if self.sync && self.unchanged(item, original.as_ref()).await? {
                return Ok(false);
            }
            if !approved && !self.approve(&item.target, &original, false).await? {
                return Ok(false);
            }
            let mut record = ResumeRecord::new(
                self.endpoint,
                self.host_key,
                &item.source,
                item.target.clone(),
                original,
            )?;
            if item.metadata.kind() == EntryKind::File {
                record.source_metadata =
                    Some(control.run(SourceMetadata::read(&item.source)).await?);
                let mut found = if new_folder {
                    None
                } else {
                    control.run(fs.metadata(&record.temporary)).await?
                };
                if found.is_some() {
                    let answer = self
                        .ask(
                            TransferQuestionKind::Resume,
                            item.target.as_str(),
                            Reason::new(UploadError::ResumeFound),
                        )
                        .await?;
                    match answer.choice() {
                        TransferChoice::Skip => return Ok(false),
                        TransferChoice::Restart => {
                            control.run(fs.remove(&record.temporary)).await?;
                            found = None;
                        }
                        _ => {}
                    }
                }
                partial = Some(found);
            } else {
                record.link_target = Some(
                    tokio::fs::read_link(&item.source)
                        .await?
                        .into_os_string()
                        .into_string()
                        .map_err(|_| anyhow!(UploadError::LinkTargetNotUtf8))?,
                );
            }
            self.journal.save(&record).await?;
            record
        };
        self.approved_resumes
            .borrow_mut()
            .insert(item.target.clone());
        if record.phase != PublishPhase::Writing {
            return self.recover_publish(&mut record).await;
        }
        let uploaded = if item.metadata.kind() == EntryKind::File {
            if partial.is_none() {
                // Resumed from a record: from the file as it is now.
                record.source_metadata =
                    Some(control.run(SourceMetadata::read(&item.source)).await?);
                self.journal.save(&record).await?;
            }
            self.upload_file(item, &record, partial, file).await
        } else {
            let target = record
                .link_target
                .as_ref()
                .ok_or_else(|| anyhow!(UploadError::RecordWithoutLink))?;
            if let Some(existing) = control.run(fs.metadata(&record.temporary)).await? {
                if existing.kind() != EntryKind::Symlink
                    || control.run(fs.readlink(&record.temporary)).await? != *target
                {
                    bail!(UploadError::TemporaryLinkChanged);
                }
            } else {
                control.run(fs.symlink(target, &record.temporary)).await?;
            }
            Ok(())
        };
        if let Err(error) = uploaded {
            let InvalidResume(why) = error.downcast::<InvalidResume>()?;
            let answer = self
                .ask(
                    TransferQuestionKind::InvalidResume,
                    item.target.as_str(),
                    Reason::new(why),
                )
                .await?;
            if answer.choice() == TransferChoice::Restart {
                self.discard_record(&record).await?;
                return Box::pin(self.upload_item(item, file)).await;
            }
            return Ok(false);
        }
        // The temporary is whole. Nothing records it: another try of a file
        // still being written finds nothing left to write.
        let current = control.run(fs.metadata(&record.target)).await?;
        if !self.confirm_target(&mut record, current).await? {
            return Ok(false);
        }
        self.publish(&mut record, false).await
    }
    /// For a sync: whether `remote`, what is at the item's target, is
    /// already what the item is. A file of the same size and modification
    /// time (an upload gives the copy the local file's), a link to the same
    /// place. A side that does not say when it was modified has changed.
    async fn unchanged(&self, item: &UploadItem, remote: Option<&FileMetadata>) -> Result<bool> {
        let Some(remote) = remote else {
            return Ok(false);
        };
        let local = &item.metadata;
        Ok(match (local.kind(), remote.kind()) {
            (EntryKind::File, EntryKind::File) => {
                local.size() == remote.size()
                    && local.modified().is_some()
                    && local.modified() == remote.modified()
            }
            (EntryKind::Symlink, EntryKind::Symlink) => {
                let here = tokio::fs::read_link(&item.source).await?;
                let there = self.control.run(self.fs.readlink(&item.target)).await?;
                here.to_str() == Some(there.as_str())
            }
            _ => false,
        })
    }
    /// Write the file into the temporary, from where the partial one there
    /// ends. `partial` is what is there, when looked at already.
    async fn upload_file(
        &self,
        item: &UploadItem,
        record: &ResumeRecord,
        partial: Option<Option<FileMetadata>>,
        file: FileId,
    ) -> Result<()> {
        let (fs, control) = (self.fs, self.control);
        let source_metadata = record
            .source_metadata
            .clone()
            .ok_or_else(|| anyhow!(UploadError::RecordWithoutSource))?;
        let remote = match partial {
            Some(partial) => partial,
            None => control.run(fs.metadata(&record.temporary)).await?,
        };
        if remote.as_ref().is_some_and(|m| m.kind() != EntryKind::File) {
            return Err(InvalidResume(UploadError::TemporaryKindChanged).into());
        }
        let resume_offset = remote.as_ref().map(FileMetadata::size).unwrap_or(0);
        if resume_offset > source_metadata.size {
            return Err(InvalidResume(UploadError::PartialTooLarge).into());
        }
        let handle = control
            .run(fs.open(&record.temporary, remote.is_none()))
            .await?;
        let sent = async {
            let mut local = tokio::fs::File::open(&item.source).await?;
            self.send(
                &mut local,
                &handle,
                resume_offset,
                source_metadata.size,
                file,
            )
            .await
        }
        .await;
        if let Err(error) = sent {
            // Always close. After a broken connection, WinSCP-style resume
            // uses the .filepart size reported by the server as the next
            // source offset.
            let _ = fs.close(&handle).await;
            return Err(error);
        }
        // Time and mode go by path once the file is closed; the request
        // follows the close without waiting for it, as WinSCP sends them.
        let (closed, set) = futures::join!(
            fs.close(&handle),
            control.run(
                fs.attributes(
                    &record.temporary,
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
        );
        closed?;
        set
    }
    async fn matches_expected_item(
        &self,
        path: &RemotePath,
        record: &ResumeRecord,
    ) -> Result<bool> {
        let (fs, control) = (self.fs, self.control);
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
    /// Carry on with a record whose temporary was written whole by an
    /// earlier try, from wherever that try got to.
    async fn recover_publish(&self, record: &mut ResumeRecord) -> Result<bool> {
        let (fs, control) = (self.fs, self.control);
        let temporary = control.run(fs.metadata(&record.temporary)).await?;
        if temporary.is_none() {
            if matches!(
                record.phase,
                PublishPhase::Publishing | PublishPhase::Published
            ) && self.matches_expected_item(&record.target, record).await?
            {
                self.cleanup(record).await?;
                return Ok(true);
            }
            // We cannot infer success from a lost reply. A backup may still be
            // the only copy of the old file; restore only into an absent target.
            if fs.metadata(&record.target).await?.is_none()
                && fs.metadata(&record.backup).await?.is_some()
            {
                fs.rename(&record.backup, &record.target).await?;
            }
            bail!(UploadError::PublishUnconfirmed);
        }
        if !self
            .matches_expected_item(&record.temporary, record)
            .await?
        {
            bail!(UploadError::TemporaryChanged);
        }
        let backup = control.run(fs.metadata(&record.backup)).await?;
        let current = control.run(fs.metadata(&record.target)).await?;
        if backup.is_some() {
            if current.is_some() {
                bail!(UploadError::TargetCreated);
            }
            if backup != record.original {
                bail!(UploadError::BackupChanged);
            }
            record.phase = PublishPhase::BackedUp;
            self.journal.save(record).await?;
        } else if !self.confirm_target(record, current).await? {
            return Ok(false);
        }
        self.publish(record, backup.is_some()).await
    }
    /// Whether the target is still what was approved for replacing, `current`
    /// being what is there now; one that changed is asked about again.
    async fn confirm_target(
        &self,
        record: &mut ResumeRecord,
        current: Option<FileMetadata>,
    ) -> Result<bool> {
        if current == record.original {
            return Ok(true);
        }
        if !self.approve(&record.target, &current, true).await? {
            self.discard_record(record).await?;
            return Ok(false);
        }
        record.original = current;
        self.journal.save(record).await?;
        if let Some(source_metadata) = &record.source_metadata {
            self.control
                .run(self.fs.attributes(
                    &record.temporary,
                    u32::try_from(source_metadata.modified_ns / 1_000_000_000).ok(),
                    record.original.as_ref().and_then(FileMetadata::permissions),
                ))
                .await?;
        }
        Ok(true)
    }
    /// Put the whole temporary in the target's place. Over a file, the old
    /// one is moved aside first and removed once the new one is in its
    /// place: no `posix-rename@openssh.com`. `backed_up` when an earlier try
    /// moved it aside already.
    async fn publish(&self, record: &mut ResumeRecord, backed_up: bool) -> Result<bool> {
        let (fs, control) = (self.fs, self.control);
        let backup = backed_up || record.original.is_some();
        if !backed_up && record.original.is_some() {
            record.phase = PublishPhase::BackingUp;
            self.journal.save(record).await?;
            control
                .run(fs.rename(&record.target, &record.backup))
                .await?;
        }
        record.phase = PublishPhase::Publishing;
        self.journal.save(record).await?;
        let result = control
            .run(fs.rename(&record.temporary, &record.target))
            .await;
        if let Err(error) = result {
            if !is_network_error(&error)
                && !error.is::<Cancelled>()
                && fs.metadata(&record.target).await?.is_none()
                && fs.metadata(&record.backup).await?.is_some()
            {
                fs.rename(&record.backup, &record.target).await?;
                record.phase = PublishPhase::Ready;
                self.journal.save(record).await?;
            }
            return Err(error);
        }
        // A record that says `Publishing` already tells another try to remove
        // the backup; without one there is nothing left to do.
        if backup {
            self.cleanup(record).await?;
        } else {
            self.journal.remove(record).await?;
        }
        Ok(true)
    }
    async fn cleanup(&self, record: &ResumeRecord) -> Result<()> {
        if let Some(backup) = self.fs.metadata(&record.backup).await? {
            if Some(backup) != record.original {
                bail!(UploadError::BackupChangedAfterPublish);
            }
            self.fs.remove(&record.backup).await?;
        }
        self.journal.remove(record).await
    }
    async fn discard_record(&self, record: &ResumeRecord) -> Result<()> {
        discard_record(self.fs, self.journal, self.endpoint, self.host_key, record).await
    }
}

/// How a small file went: settled (done or skipped), or to be replaced the
/// resumable way.
enum Small {
    Settled(bool),
    Replace { approved: bool },
}

/// Undo what a record's upload left on the server: the old file back in its
/// place, the temporary gone, and the record with it.
async fn discard_record<F: RemoteFs>(
    fs: &F,
    journal: &Journal,
    endpoint: &str,
    host_key: &str,
    record: &ResumeRecord,
) -> Result<()> {
    record.validate(endpoint, host_key, &record.source, &record.target)?;
    if fs.metadata(&record.backup).await?.is_some() {
        if fs.metadata(&record.target).await?.is_none() {
            fs.rename(&record.backup, &record.target).await?;
        } else {
            bail!(UploadError::BackupAndTarget);
        }
    }
    fs.remove(&record.temporary).await?;
    journal.remove(record).await
}

/// A partial file that cannot be resumed from, and why: asked about rather
/// than failed.
#[derive(Debug)]
struct InvalidResume(UploadError);
impl std::fmt::Display for InvalidResume {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for InvalidResume {}
