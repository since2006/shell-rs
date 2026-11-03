//! Downloading remote files and trees, the mirror of `upload`: each file is
//! written to `<target>.filepart` next to its target and renamed over it
//! once complete. An interrupted file resumes from the partial file's length,
//! as WinSCP does, as long as the remote file still has the size and time it
//! had when the partial file was started. A file under [`RESUME_THRESHOLD`]
//! is not resumed, as WinSCP resumes none: it keeps no record and is not
//! flushed to the disk on its own.

use super::{
    DownloadError, DownloadRequest, EntryKind, FileMetadata, RESUME_THRESHOLD, Reason, RemotePath,
    SftpError, TransferAnswer, TransferChoice, TransferDetail, TransferDirection, TransferPhase,
    TransferQuestionKind,
    client::{RemoteFs, is_network_error},
    control::{Cancelled, TargetGuard, TransferControl},
    file_size,
    journal::{DownloadJournal, DownloadRecord, partial_path, remove_if_present, sync_file},
    meter::{FileId, TransferMeter},
    parallel,
};
use anyhow::{Result, anyhow, bail};
use futures::{StreamExt as _, stream::FuturesUnordered};
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncSeekExt as _, AsyncWriteExt as _};

const CHUNK: u32 = 32 * 1024;
const MAX_IN_FLIGHT: usize = 16;

#[derive(Clone)]
struct DownloadItem {
    source: RemotePath,
    target: PathBuf,
    metadata: FileMetadata,
    /// Why the scan could not tell what to download.
    error: Option<Reason>,
}

pub(crate) struct DownloadBatch {
    items: Vec<DownloadItem>,
    /// The items not settled yet, in the order they go: a folder before
    /// what is in it.
    pending: VecDeque<usize>,
    pub meter: TransferMeter,
    endpoint: String,
    host_key: String,
    journal: DownloadJournal,
    all_conflicts: Option<TransferChoice>,
    approved_resumes: HashSet<PathBuf>,
    blocked_directories: Vec<PathBuf>,
    /// A new local file takes the remote file's execute bits, as scp keeps
    /// them; otherwise it gets this machine's defaults.
    preserve_mode: bool,
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
        bail!(DownloadError::NameInvalid { name: name.into() });
    }
    Ok(name)
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
            let name = request.target_name().unwrap_or(source.file_name());
            let target = local_name(name).map(|name| request.destination().join(name));
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
                        target: request.destination().join(source.file_name()),
                        source,
                        metadata: FileMetadata::new(EntryKind::Other, 0, None, None),
                        error: Some(error.into()),
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
                        Ok(None) => Err(Reason::new(DownloadError::SourceMissing)),
                        Err(error)
                            if super::client::is_network_error(&error)
                                || error.is::<Cancelled>() =>
                        {
                            return Err(error);
                        }
                        Err(error) => Err(error.into()),
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
                    Err(error) => scan_error = Some(error.into()),
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
        Ok(Self {
            meter: TransferMeter::new(TransferDirection::Download, items.len(), total_bytes),
            pending: (0..items.len()).collect(),
            items,
            endpoint: endpoint.into(),
            host_key: host_key.into(),
            journal,
            all_conflicts: None,
            approved_resumes: HashSet::new(),
            blocked_directories: Vec::new(),
            preserve_mode: request.is_scp(),
        })
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
            let download = Download {
                fs,
                control,
                endpoint: &self.endpoint,
                host_key: &self.host_key,
                journal: &self.journal,
                preserve_mode: self.preserve_mode,
                items: RefCell::new(&mut self.items),
                meter: RefCell::new(&mut self.meter),
                all_conflicts: RefCell::new(&mut self.all_conflicts),
                approved_resumes: RefCell::new(&mut self.approved_resumes),
                blocked_directories: RefCell::new(&mut self.blocked_directories),
                asking: tokio::sync::Mutex::new(()),
            };
            parallel::run(
                &mut self.pending,
                parallel::parallel_transfers,
                |next, running| download.waits(next, running),
                |index| download.item(index),
            )
            .await
        };
        self.meter.abandon();
        result?;
        self.meter.phase(TransferPhase::Completed, control);
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
                remove_if_present(&record.temporary).await?;
                self.journal.remove(&record).await?;
            }
        }
        Ok(())
    }
}

/// One run of a batch: what its items share while several are in flight.
struct Download<'a, R> {
    fs: &'a R,
    control: &'a TransferControl,
    endpoint: &'a str,
    host_key: &'a str,
    journal: &'a DownloadJournal,
    preserve_mode: bool,
    items: RefCell<&'a mut Vec<DownloadItem>>,
    meter: RefCell<&'a mut TransferMeter>,
    all_conflicts: RefCell<&'a mut Option<TransferChoice>>,
    approved_resumes: RefCell<&'a mut HashSet<PathBuf>>,
    blocked_directories: RefCell<&'a mut Vec<PathBuf>>,
    /// One question at a time: the other items with one wait their turn.
    asking: tokio::sync::Mutex<()>,
}

impl<R: RemoteFs> Download<'_, R> {
    /// Whether `next` goes in a folder that is still being made.
    fn waits(&self, next: usize, running: &[usize]) -> bool {
        let items = self.items.borrow();
        let Some(parent) = items[next].target.parent() else {
            return false;
        };
        running.iter().any(|&index| {
            items[index].metadata.kind() == EntryKind::Directory && items[index].target == parent
        })
    }

    /// Download one item to its end: done, skipped, or failed as the user
    /// chose. Returns what a folder scanned again on 重试 found, to go next.
    async fn item(&self, index: usize) -> Result<Vec<usize>> {
        let item = self.items.borrow()[index].clone();
        let size = file_size(&item.metadata);
        let target = item.target.display().to_string();
        let file = self.meter.borrow_mut().begin(
            item.source.to_string(),
            target.clone(),
            size,
            self.control,
        );
        let _target_guard = TargetGuard::acquire(
            format!("local\0{}", item.target.display()),
            DownloadError::TargetBusy,
        )?;
        loop {
            let blocked = self
                .blocked_directories
                .borrow()
                .iter()
                .any(|dir| item.target.starts_with(dir) && item.target != *dir);
            let reason = if blocked {
                Reason::new(DownloadError::ParentMissing)
            } else if let Some(reason) = &item.error {
                reason.clone()
            } else {
                match self.download_item(&item, file).await {
                    Ok(true) => {
                        self.meter.borrow_mut().settle(
                            file,
                            TransferDetail::done(target),
                            size,
                            self.control,
                        );
                        return Ok(Vec::new());
                    }
                    Ok(false) => {
                        self.meter.borrow_mut().settle(
                            file,
                            TransferDetail::skipped(target),
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
                    Err(error) if error.is::<Cancelled>() || is_network_error(&error) => {
                        return Err(error);
                    }
                    Err(error) => Reason::from(error),
                }
            };
            let answer = self
                .ask(
                    TransferQuestionKind::Error,
                    item.source.as_str(),
                    reason.under(DownloadError::Failed),
                )
                .await?;
            if answer.choice() == TransferChoice::Retry {
                if item.error.is_none() {
                    continue;
                }
                let request = DownloadRequest::new(
                    vec![item.source.clone()],
                    item.target
                        .parent()
                        .map(Path::to_path_buf)
                        .ok_or_else(|| anyhow!(DownloadError::TargetInvalid))?,
                )?;
                let scanned = DownloadBatch::scan(
                    &request,
                    self.endpoint,
                    self.host_key,
                    self.journal.clone(),
                    self.fs,
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
                TransferDetail::failed(target, reason),
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

    /// Ask before replacing an existing local item; remembers "apply to all".
    /// `changed` means the target appeared or changed while downloading.
    async fn approve(
        &self,
        target: &Path,
        existing: &std::fs::Metadata,
        changed: bool,
    ) -> Result<bool> {
        if existing.is_dir() {
            bail!(DownloadError::FolderInTheWay);
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
        let modified = existing
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| time.as_secs());
        let size = existing.len();
        let message = if changed {
            DownloadError::ExistsChanged { size, modified }
        } else {
            DownloadError::Exists { size, modified }
        };
        let answer = self
            .question(
                TransferQuestionKind::Conflict,
                &target.display().to_string(),
                Reason::new(message),
            )
            .await?;
        if answer.apply_to_all() && !changed {
            **self.all_conflicts.borrow_mut() = Some(answer.choice());
        }
        Ok(answer.choice() == TransferChoice::Overwrite)
    }

    async fn download_item(&self, item: &DownloadItem, file: FileId) -> Result<bool> {
        let existing = local_metadata(&item.target).await?;
        match item.metadata.kind() {
            EntryKind::Directory => match existing {
                Some(metadata) if metadata.is_dir() => Ok(true),
                Some(_) => bail!(DownloadError::FileInTheWay),
                None => {
                    tokio::fs::create_dir(&item.target).await?;
                    Ok(true)
                }
            },
            EntryKind::Symlink => {
                let link = self.control.run(self.fs.readlink(&item.source)).await?;
                if let Some(existing) = &existing
                    && !self.approve(&item.target, existing, false).await?
                {
                    return Ok(false);
                }
                create_link(&link, &item.target, existing.is_some()).await?;
                Ok(true)
            }
            EntryKind::Other => bail!(DownloadError::SpecialFile),
            EntryKind::File if item.metadata.size() < RESUME_THRESHOLD => {
                self.download_small(item, existing, file).await
            }
            EntryKind::File => self.download_regular(item, existing, file).await,
        }
    }

    /// A file under [`RESUME_THRESHOLD`]: no record, nothing to resume and
    /// nothing asked about a partial file, which is simply written again.
    async fn download_small(
        &self,
        item: &DownloadItem,
        existing: Option<std::fs::Metadata>,
        file: FileId,
    ) -> Result<bool> {
        // Already answered in this batch: a reconnect retries the same item.
        let answered = self.approved_resumes.borrow().contains(&item.target);
        if !answered
            && let Some(existing) = &existing
            && !self.approve(&item.target, existing, false).await?
        {
            return Ok(false);
        }
        self.approved_resumes
            .borrow_mut()
            .insert(item.target.clone());
        let temporary = partial_path(&item.target);
        let placed = async {
            self.download_file(item, &temporary, 0, file).await?;
            self.place(item, &temporary, existing.as_ref(), false).await
        }
        .await;
        if !matches!(placed, Ok(true)) {
            let _ = remove_if_present(&temporary).await;
        }
        placed
    }

    async fn download_regular(
        &self,
        item: &DownloadItem,
        existing: Option<std::fs::Metadata>,
        file: FileId,
    ) -> Result<bool> {
        let temporary = partial_path(&item.target);
        let record = self
            .journal
            .load(self.endpoint, &item.source, &item.target)
            .await?;
        let partial = local_metadata(&temporary).await?;
        let resumable = record.as_ref().is_some_and(|record| {
            record
                .validate(self.endpoint, self.host_key, &item.source, &item.target)
                .is_ok()
                && record.source_metadata == item.metadata
        });
        // Already answered in this batch: a reconnect retries the same item.
        let answered = self.approved_resumes.borrow().contains(&item.target);
        // A matching record means overwriting was approved when it was made.
        if !answered
            && !resumable
            && let Some(existing) = &existing
            && !self.approve(&item.target, existing, false).await?
        {
            return Ok(false);
        }
        let mut offset = 0;
        if let Some(partial) = &partial {
            if !partial.is_file() {
                bail!(DownloadError::PartialNotFile {
                    path: temporary.clone()
                });
            }
            if partial.len() > item.metadata.size() || (record.is_some() && !resumable) {
                let reason = if partial.len() > item.metadata.size() {
                    DownloadError::PartialTooLarge
                } else {
                    DownloadError::SourceChanged
                };
                let answer = self
                    .ask(
                        TransferQuestionKind::InvalidResume,
                        item.source.as_str(),
                        Reason::new(reason),
                    )
                    .await?;
                if answer.choice() != TransferChoice::Restart {
                    return Ok(false);
                }
            } else if answered {
                offset = partial.len();
            } else {
                let answer = self
                    .ask(
                        TransferQuestionKind::Resume,
                        item.source.as_str(),
                        Reason::new(DownloadError::ResumeFound),
                    )
                    .await?;
                match answer.choice() {
                    TransferChoice::Skip => return Ok(false),
                    TransferChoice::Restart => {}
                    _ => offset = partial.len(),
                }
            }
        }
        self.approved_resumes
            .borrow_mut()
            .insert(item.target.clone());
        let record = DownloadRecord::new(
            self.endpoint,
            self.host_key,
            item.source.clone(),
            item.metadata.clone(),
            item.target.clone(),
        );
        self.journal.save(&record).await?;
        self.download_file(item, &temporary, offset, file).await?;
        if !self
            .place(item, &temporary, existing.as_ref(), true)
            .await?
        {
            return Ok(false);
        }
        self.journal.remove(&record).await?;
        Ok(true)
    }

    /// Put the finished partial file in the target's place, unless the
    /// target appeared or changed while the file downloaded and the user
    /// would rather keep it. `durable` flushes the file to the disk first.
    async fn place(
        &self,
        item: &DownloadItem,
        temporary: &Path,
        existing: Option<&std::fs::Metadata>,
        durable: bool,
    ) -> Result<bool> {
        let current = local_metadata(&item.target).await?;
        if let Some(current) = &current
            && changed(existing, current)
            && !self.approve(&item.target, current, true).await?
        {
            return Ok(false);
        }
        publish(
            temporary,
            &item.target,
            current.as_ref(),
            &item.metadata,
            self.preserve_mode,
            durable,
        )
        .await?;
        Ok(true)
    }

    /// Read the remote file from `offset` into the partial file. Reads run
    /// in parallel and may complete out of order or short, but the file is
    /// written strictly in sequence: resuming trusts its length.
    async fn download_file(
        &self,
        item: &DownloadItem,
        temporary: &Path,
        offset: u64,
        file: FileId,
    ) -> Result<()> {
        let (fs, control) = (self.fs, self.control);
        let size = item.metadata.size();
        let handle = control.run(fs.open_read(&item.source)).await?;
        let result: Result<()> = async {
            let mut local = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(offset == 0)
                .open(temporary)
                .await?;
            local.set_len(offset).await?;
            local.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut next_request = offset;
            let mut next_write = offset;
            self.meter
                .borrow_mut()
                .advance(file, 0, next_write, control);
            let mut remainders: Vec<(u64, u32)> = Vec::new();
            let mut ready: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
            let mut reads = FuturesUnordered::new();
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
                    .ok_or_else(|| anyhow!(DownloadError::PipelineEnded))?;
                let bytes = match bytes? {
                    Some(bytes) if !bytes.is_empty() => bytes,
                    _ => bail!(DownloadError::SourceShrank),
                };
                let got = bytes.len().min(len as usize);
                if got < len as usize {
                    remainders.push((at + got as u64, len - got as u32));
                }
                ready.insert(at, bytes[..got].to_vec());
                while let Some(chunk) = ready.remove(&next_write) {
                    local.write_all(&chunk).await?;
                    next_write += chunk.len() as u64;
                    self.meter
                        .borrow_mut()
                        .advance(file, chunk.len() as u64, next_write, control);
                }
            }
            local.flush().await?;
            Ok(())
        }
        .await;
        // Always close; the partial file's length is the resume point.
        let close = fs.close(&handle).await;
        result?;
        close?;
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
/// `durable` flushes it to the disk before the rename.
async fn publish(
    temporary: &Path,
    target: &Path,
    replaced: Option<&std::fs::Metadata>,
    metadata: &FileMetadata,
    preserve_mode: bool,
    durable: bool,
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
    } else if preserve_mode && let Some(source) = metadata.permissions() {
        keep_execute_bits(&file, source)?;
    }
    if durable {
        sync_file(&file)?;
    }
    drop(file);
    tokio::fs::rename(temporary, target).await?;
    Ok(())
}

/// Give `file` the execute bits `source` has, for the classes that can
/// already read it; this machine's defaults decide the rest.
#[cfg(unix)]
fn keep_execute_bits(file: &std::fs::File, source: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = file.metadata()?.permissions();
    let mode = permissions.mode();
    let execute = source & 0o111 & ((mode & 0o444) >> 2);
    if execute != 0 {
        permissions.set_mode(mode | execute);
        file.set_permissions(permissions)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn keep_execute_bits(_: &std::fs::File, _: u32) -> Result<()> {
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
    bail!(DownloadError::LinksUnsupported)
}
