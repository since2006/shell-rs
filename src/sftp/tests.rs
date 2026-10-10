use super::{
    client::RemoteFs,
    control::TransferControl,
    download::DownloadBatch,
    edit::{read_text, read_whole, write_in_place},
    journal::{DownloadJournal, Journal, SourceMetadata, partial_path},
    upload::UploadBatch,
    *,
};
use anyhow::{Result, bail};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

#[derive(Clone)]
struct Node {
    metadata: FileMetadata,
    data: Vec<u8>,
    link: String,
}
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    Write(usize),
    Read(usize),
    Rename(usize),
    Close,
    RejectRename(usize),
    ChangeTargetOnClose,
    RemoveTargetOnClose,
}
struct Remote {
    nodes: RefCell<BTreeMap<String, Node>>,
    fault: Cell<Option<Fault>>,
    in_flight: Cell<usize>,
    max_in_flight: Cell<usize>,
    writes: Cell<usize>,
    reads: Cell<usize>,
    short_reads: Cell<bool>,
    renames: Cell<usize>,
    denied: RefCell<Option<String>>,
}
impl Remote {
    fn new() -> Self {
        Self {
            nodes: RefCell::new(BTreeMap::new()),
            fault: Cell::new(None),
            in_flight: Cell::new(0),
            max_in_flight: Cell::new(0),
            writes: Cell::new(0),
            reads: Cell::new(0),
            short_reads: Cell::new(false),
            renames: Cell::new(0),
            denied: RefCell::new(None),
        }
    }
    fn file(&self, path: &str, bytes: &[u8]) {
        self.nodes.borrow_mut().insert(
            path.into(),
            Node {
                metadata: FileMetadata::new(
                    EntryKind::File,
                    bytes.len() as u64,
                    Some(100),
                    Some(0o640),
                ),
                data: bytes.to_vec(),
                link: String::new(),
            },
        );
    }
    fn bytes(&self, path: &str) -> Vec<u8> {
        self.nodes.borrow()[path].data.clone()
    }
    fn fail(&self, fault: Fault) -> Result<()> {
        if self.fault.get() == Some(fault) {
            self.fault.set(None);
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "injected lost reply",
            )
            .into());
        }
        Ok(())
    }
}
struct WriteGuard<'a>(&'a Cell<usize>);
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}
impl RemoteFs for Remote {
    async fn metadata(&self, path: &RemotePath) -> Result<Option<FileMetadata>> {
        Ok(self
            .nodes
            .borrow()
            .get(path.as_str())
            .map(|n| n.metadata.clone()))
    }
    async fn stat(&self, path: &RemotePath) -> Result<Option<FileMetadata>> {
        let nodes = self.nodes.borrow();
        let mut current = path.to_string();
        for _ in 0..8 {
            let Some(node) = nodes.get(&current) else {
                return Ok(None);
            };
            if node.metadata.kind() != EntryKind::Symlink {
                return Ok(Some(node.metadata.clone()));
            }
            current = if node.link.starts_with('/') {
                node.link.clone()
            } else {
                RemotePath::new(current.as_str())?
                    .parent()
                    .join(&node.link)?
                    .to_string()
            };
        }
        bail!("too many levels of symbolic links")
    }
    async fn read_dir(&self, path: &RemotePath) -> Result<Vec<DirectoryEntry>> {
        if self.denied.borrow().as_deref() == Some(path.as_str()) {
            bail!("permission denied");
        }
        let prefix = format!("{}/", path.as_str().trim_end_matches('/'));
        Ok(self
            .nodes
            .borrow()
            .iter()
            .filter_map(|(key, node)| {
                let name = key.strip_prefix(&prefix)?;
                (!name.is_empty() && !name.contains('/'))
                    .then(|| DirectoryEntry::new(name, node.metadata.clone()))
            })
            .collect())
    }
    async fn open(&self, path: &RemotePath, create: bool) -> Result<String> {
        if create {
            if self.nodes.borrow().contains_key(path.as_str()) {
                bail!("exclusive create failed");
            }
            self.file(path.as_str(), &[]);
        } else if !self.nodes.borrow().contains_key(path.as_str()) {
            bail!("file absent");
        }
        Ok(path.to_string())
    }
    async fn open_replace(&self, path: &RemotePath) -> Result<String> {
        let target = self.resolve(path.as_str())?;
        let mut nodes = self.nodes.borrow_mut();
        match nodes.get_mut(&target) {
            Some(node) if node.metadata.kind() == EntryKind::File => {
                node.data.clear();
                node.metadata = FileMetadata::new(
                    EntryKind::File,
                    0,
                    node.metadata.modified(),
                    node.metadata.permissions(),
                );
            }
            Some(_) => bail!("not a file"),
            None => {
                drop(nodes);
                self.file(&target, &[]);
            }
        }
        Ok(target)
    }
    async fn open_read(&self, path: &RemotePath) -> Result<String> {
        let target = self.resolve(path.as_str())?;
        match self.nodes.borrow().get(&target) {
            Some(node) if node.metadata.kind() == EntryKind::File => Ok(target.clone()),
            _ => bail!("no such file"),
        }
    }
    async fn read(&self, handle: &str, offset: u64, len: u32) -> Result<Option<Vec<u8>>> {
        let reads = self.reads.get() + 1;
        self.reads.set(reads);
        tokio::task::yield_now().await;
        // Replies arrive out of order: odd reads yield once more.
        if reads % 2 == 1 {
            tokio::task::yield_now().await;
        }
        let data = self
            .nodes
            .borrow()
            .get(handle)
            .map(|node| node.data.clone())
            .ok_or_else(|| anyhow::anyhow!("file absent"))?;
        self.fail(Fault::Read(reads))?;
        let start = offset as usize;
        if start >= data.len() {
            return Ok(None);
        }
        let mut len = len as usize;
        if self.short_reads.get() {
            len = len.min(1000);
        }
        Ok(Some(data[start..(start + len).min(data.len())].to_vec()))
    }
    async fn write(&self, handle: &str, offset: u64, bytes: Vec<u8>) -> Result<()> {
        let in_flight = self.in_flight.get() + 1;
        self.in_flight.set(in_flight);
        self.max_in_flight
            .set(self.max_in_flight.get().max(in_flight));
        let _guard = WriteGuard(&self.in_flight);
        tokio::task::yield_now().await;
        let mut nodes = self.nodes.borrow_mut();
        let node = nodes.get_mut(handle).unwrap();
        let end = offset as usize + bytes.len();
        node.data.resize(end.max(node.data.len()), 0);
        node.data[offset as usize..end].copy_from_slice(&bytes);
        node.metadata = FileMetadata::new(
            EntryKind::File,
            node.data.len() as u64,
            node.metadata.modified(),
            node.metadata.permissions(),
        );
        self.writes.set(self.writes.get() + 1);
        self.fail(Fault::Write(self.writes.get()))
    }
    async fn close(&self, _: &str) -> Result<()> {
        if self.fault.get() == Some(Fault::ChangeTargetOnClose) {
            self.fault.set(None);
            self.file("/dest/file", b"external mutation");
        }
        if self.fault.get() == Some(Fault::RemoveTargetOnClose) {
            self.fault.set(None);
            self.nodes.borrow_mut().remove("/dest/file");
        }
        self.fail(Fault::Close)
    }
    async fn attributes(
        &self,
        path: &RemotePath,
        modified: Option<u32>,
        permissions: Option<u32>,
    ) -> Result<()> {
        let mut nodes = self.nodes.borrow_mut();
        let n = nodes.get_mut(path.as_str()).unwrap();
        n.metadata = FileMetadata::new(
            EntryKind::File,
            n.data.len() as u64,
            modified,
            permissions.or(Some(0o644)),
        );
        Ok(())
    }
    async fn mkdir(&self, path: &RemotePath) -> Result<()> {
        if self.denied.borrow().as_deref() == Some(path.as_str()) {
            bail!("permission denied");
        }
        self.nodes.borrow_mut().insert(
            path.to_string(),
            Node {
                metadata: FileMetadata::new(EntryKind::Directory, 0, None, Some(0o755)),
                data: vec![],
                link: String::new(),
            },
        );
        Ok(())
    }
    async fn symlink(&self, target: &str, path: &RemotePath) -> Result<()> {
        self.nodes.borrow_mut().insert(
            path.to_string(),
            Node {
                metadata: FileMetadata::new(EntryKind::Symlink, 0, None, None),
                data: vec![],
                link: target.into(),
            },
        );
        Ok(())
    }
    async fn readlink(&self, path: &RemotePath) -> Result<String> {
        Ok(self.nodes.borrow()[path.as_str()].link.clone())
    }
    async fn rename(&self, from: &RemotePath, to: &RemotePath) -> Result<()> {
        if self.fault.get() == Some(Fault::RejectRename(self.renames.get() + 1)) {
            self.fault.set(None);
            bail!("publish denied");
        }
        let mut nodes = self.nodes.borrow_mut();
        if nodes.contains_key(to.as_str()) {
            bail!("target exists");
        }
        let node = nodes
            .remove(from.as_str())
            .ok_or_else(|| anyhow::anyhow!("rename source absent"))?;
        nodes.insert(to.to_string(), node);
        self.renames.set(self.renames.get() + 1);
        self.fail(Fault::Rename(self.renames.get()))
    }
    async fn remove(&self, path: &RemotePath) -> Result<()> {
        self.nodes.borrow_mut().remove(path.as_str());
        Ok(())
    }
    async fn rmdir(&self, path: &RemotePath) -> Result<()> {
        let prefix = format!("{}/", path.as_str());
        let mut nodes = self.nodes.borrow_mut();
        if nodes.keys().any(|key| key.starts_with(&prefix)) {
            bail!("directory not empty");
        }
        match nodes.get(path.as_str()) {
            Some(node) if node.metadata.kind() == EntryKind::Directory => {
                nodes.remove(path.as_str());
                Ok(())
            }
            _ => bail!("not a directory"),
        }
    }
    async fn set_permissions(&self, path: &RemotePath, permissions: u32) -> Result<()> {
        let mut nodes = self.nodes.borrow_mut();
        let node = nodes
            .get_mut(path.as_str())
            .ok_or_else(|| anyhow::anyhow!("no such file"))?;
        let m = &node.metadata;
        node.metadata = FileMetadata::new(
            m.kind(),
            m.size(),
            m.modified(),
            Some((m.permissions().unwrap_or(0) & !0o7777) | permissions),
        );
        Ok(())
    }
}
impl Remote {
    /// The path a link chain ends at, as a server's `open` follows it.
    fn resolve(&self, path: &str) -> Result<String> {
        let nodes = self.nodes.borrow();
        let mut current = path.to_string();
        for _ in 0..8 {
            match nodes.get(&current) {
                Some(node) if node.metadata.kind() == EntryKind::Symlink => {
                    current = if node.link.starts_with('/') {
                        node.link.clone()
                    } else {
                        RemotePath::new(current.as_str())?
                            .parent()
                            .join(&node.link)?
                            .to_string()
                    };
                }
                _ => return Ok(current),
            }
        }
        bail!("too many levels of symbolic links")
    }
    fn dir(&self, path: &str, mode: u32) {
        self.nodes.borrow_mut().insert(
            path.into(),
            Node {
                metadata: FileMetadata::new(EntryKind::Directory, 0, None, Some(0o040_000 | mode)),
                data: vec![],
                link: String::new(),
            },
        );
    }
    fn link(&self, path: &str, target: &str) {
        self.nodes.borrow_mut().insert(
            path.into(),
            Node {
                metadata: FileMetadata::new(EntryKind::Symlink, 0, None, Some(0o120_777)),
                data: vec![],
                link: target.into(),
            },
        );
    }
    fn mode(&self, path: &str) -> u32 {
        self.nodes.borrow()[path].metadata.permissions().unwrap() & 0o7777
    }
}
struct Answers {
    control: TransferControl,
    cancel: watch::Sender<bool>,
    questions: Arc<Mutex<Vec<TransferQuestionKind>>>,
    progress: Arc<Mutex<Vec<TransferProgress>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Answers {
    fn new(choices: Vec<TransferChoice>) -> Self {
        let (events, receiver) = async_channel::unbounded();
        let (answers, rx) = async_channel::unbounded();
        let (cancel, watch) = watch::channel(false);
        let questions = Arc::new(Mutex::new(vec![]));
        let seen_questions = questions.clone();
        let progress = Arc::new(Mutex::new(vec![]));
        let seen_progress = progress.clone();
        let task = tokio::spawn(async move {
            let mut choices = choices.into_iter();
            while let Ok(event) = receiver.recv().await {
                match event {
                    SftpEvent::Question(q) => {
                        seen_questions.lock().unwrap().push(q.kind());
                        let _ = answers
                            .send((
                                q.id(),
                                TransferAnswer::new(
                                    choices.next().unwrap_or(TransferChoice::Cancel),
                                    false,
                                ),
                            ))
                            .await;
                    }
                    SftpEvent::Progress(snapshot) => {
                        seen_progress.lock().unwrap().push(snapshot);
                    }
                    _ => {}
                }
            }
        });
        Self {
            control: TransferControl {
                events,
                cancel: watch,
                answers: rx,
            },
            cancel,
            questions,
            progress,
            task,
        }
    }
}
impl Drop for Answers {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
async fn batch(source: &Path, journal: Journal, control: &TransferControl) -> UploadBatch {
    let request =
        UploadRequest::new(vec![source.into()], RemotePath::new("/dest").unwrap()).unwrap();
    UploadBatch::scan(
        &request,
        &format!("test@{}", source.display()),
        "host-key",
        journal,
        control,
    )
    .await
    .unwrap()
}
async fn run(
    batch: &mut UploadBatch,
    remote: &impl RemoteFs,
    control: &TransferControl,
) -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        batch.run(remote, control),
    )
    .await
    .expect("upload did not finish")
}

#[test]
fn recursive_upload_deduplicates_sources_and_preserves_empty_directories_and_links() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let root = tmp.path().join("中文 目录");
        std::fs::create_dir_all(root.join("空/嵌套")).unwrap();
        std::fs::write(root.join("零 字节"), []).unwrap();
        let data = vec![47; 2 * 1024 * 1024 + 13];
        std::fs::write(root.join("large.bin"), &data).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../不存在", root.join("链接")).unwrap();
        let answers = Answers::new(vec![]);
        let request = UploadRequest::new(
            vec![root.clone(), root.join("large.bin"), root.clone()],
            RemotePath::new("/dest").unwrap(),
        )
        .unwrap();
        let mut batch = UploadBatch::scan(
            &request,
            "recursive",
            "key",
            Journal::new(tmp.path().join("journal")),
            &answers.control,
        )
        .await
        .unwrap();
        let remote = Remote::new();
        run(&mut batch, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/中文 目录/large.bin"), data);
        assert_eq!(remote.bytes("/dest/中文 目录/零 字节"), Vec::<u8>::new());
        assert!(
            remote
                .metadata(&RemotePath::new("/dest/中文 目录/空/嵌套").unwrap())
                .await
                .unwrap()
                .is_some()
        );
        #[cfg(unix)]
        assert_eq!(
            remote
                .readlink(&RemotePath::new("/dest/中文 目录/链接").unwrap())
                .await
                .unwrap(),
            "../不存在"
        );
        assert_eq!(batch.meter.progress.failed(), 0);
        assert!(answers.questions.lock().unwrap().is_empty());
        assert!(
            std::fs::read_dir(tmp.path().join("journal"))
                .unwrap()
                .next()
                .is_none()
        );
    });
}

#[test]
fn winscp_style_upload_finishes_without_a_verification_phase() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        let data = vec![73; 2 * 1024 * 1024 + 17];
        std::fs::write(&source, &data).unwrap();
        let answers = Answers::new(vec![]);
        let mut batch = batch(
            &source,
            Journal::new(tmp.path().join("journal")),
            &answers.control,
        )
        .await;
        let remote = Remote::new();

        run(&mut batch, &remote, &answers.control).await.unwrap();
        tokio::task::yield_now().await;

        assert_eq!(remote.bytes("/dest/file"), data);
        assert!(remote.max_in_flight.get() > 1);
        assert_eq!(batch.meter.progress.succeeded(), 1);
        let progress = answers.progress.lock().unwrap();
        assert!(progress.iter().all(|snapshot| matches!(
            snapshot.phase(),
            TransferPhase::Transferring | TransferPhase::Completed
        )));
        // The file in flight is reported on its own, up to its full size,
        // and cleared once the batch is done.
        let size = data.len() as u64;
        assert!(progress.iter().any(|snapshot| {
            snapshot.current_source() == source.display().to_string()
                && snapshot.current_total() == size
                && snapshot.current_bytes() == size
        }));
        assert!(
            progress
                .windows(2)
                .all(|pair| pair[0].current_source() != pair[1].current_source()
                    || pair[0].current_bytes() <= pair[1].current_bytes())
        );
        let last = progress.last().unwrap();
        assert_eq!(last.phase(), TransferPhase::Completed);
        assert_eq!(last.current_source(), "");
        assert_eq!(last.fraction(), 1.0);
        assert_eq!(last.remaining(), None);
    });
}

#[test]
fn resume_uses_remote_filepart_size_and_preserves_old_target() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        let data = vec![71; 150_000];
        std::fs::write(&source, &data).unwrap();
        let journal = Journal::new(tmp.path().join("journal"));
        let remote = Remote::new();
        remote.file("/dest/file", b"old file intact");
        remote.fault.set(Some(Fault::Write(2)));
        let answers = Answers::new(vec![TransferChoice::Overwrite]);
        let mut first = batch(&source, journal.clone(), &answers.control).await;
        assert!(super::client::is_network_error(
            &run(&mut first, &remote, &answers.control)
                .await
                .unwrap_err()
        ));
        assert_eq!(remote.bytes("/dest/file"), b"old file intact");
        let endpoint = format!("test@{}", source.display());
        let target = RemotePath::new("/dest/file").unwrap();
        let record = journal
            .load(&endpoint, &source, &target)
            .await
            .unwrap()
            .unwrap();
        remote.file(record.temporary.as_str(), &data[..65_536]);
        let answers = Answers::new(vec![TransferChoice::Resume]);
        let mut second = batch(&source, journal.clone(), &answers.control).await;
        run(&mut second, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/file"), data);
        assert!(
            journal
                .load(&endpoint, &source, &target)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            remote
                .metadata(&target)
                .await
                .unwrap()
                .unwrap()
                .permissions(),
            Some(0o640)
        );
        assert_eq!(second.meter.progress.succeeded(), 1);
    });
}

#[test]
fn filepart_larger_than_source_requires_explicit_restart() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        std::fs::write(&source, vec![1; 100_000]).unwrap();
        let journal = Journal::new(tmp.path().join("journal"));
        let remote = Remote::new();
        remote.fault.set(Some(Fault::Write(2)));
        let answers = Answers::new(vec![]);
        let mut first = batch(&source, journal.clone(), &answers.control).await;
        assert!(run(&mut first, &remote, &answers.control).await.is_err());
        std::fs::write(&source, vec![2; 32_000]).unwrap();
        let target = RemotePath::new("/dest/file").unwrap();
        let answers = Answers::new(vec![TransferChoice::Resume, TransferChoice::Skip]);
        let mut next = batch(&source, journal, &answers.control).await;
        run(&mut next, &remote, &answers.control).await.unwrap();
        assert!(remote.metadata(&target).await.unwrap().is_none());
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![
                TransferQuestionKind::Resume,
                TransferQuestionKind::InvalidResume
            ]
        );
        assert_eq!(next.meter.progress.skipped(), 1);
    });
}

#[test]
fn winscp_style_resume_does_not_prevalidate_the_source_version() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        let data = vec![1; 100_000];
        std::fs::write(&source, &data).unwrap();
        let journal = Journal::new(tmp.path().join("journal"));
        let remote = Remote::new();
        remote.fault.set(Some(Fault::Write(2)));
        let answers = Answers::new(vec![]);
        let mut first = batch(&source, journal.clone(), &answers.control).await;
        assert!(run(&mut first, &remote, &answers.control).await.is_err());
        let endpoint = format!("test@{}", source.display());
        let target = RemotePath::new("/dest/file").unwrap();
        let record = journal
            .load(&endpoint, &source, &target)
            .await
            .unwrap()
            .unwrap();
        let old_prefix = remote.bytes(record.temporary.as_str());
        std::fs::write(&source, vec![2; 100_000]).unwrap();
        let answers = Answers::new(vec![TransferChoice::Resume]);
        let mut resumed = batch(&source, journal, &answers.control).await;

        run(&mut resumed, &remote, &answers.control).await.unwrap();

        let mut expected = vec![2; 100_000];
        expected[..old_prefix.len()].copy_from_slice(&old_prefix);
        assert_eq!(remote.bytes("/dest/file"), expected);
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![TransferQuestionKind::Resume]
        );
        assert_eq!(resumed.meter.progress.succeeded(), 1);
    });
}

#[test]
fn winscp_style_resume_detects_filepart_without_a_local_record() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        let data = vec![3; 100_000];
        std::fs::write(&source, &data).unwrap();
        let remote = Remote::new();
        remote.file("/dest/file.filepart", &data[..32_768]);
        let answers = Answers::new(vec![TransferChoice::Resume]);
        let mut upload = batch(
            &source,
            Journal::new(tmp.path().join("journal")),
            &answers.control,
        )
        .await;

        run(&mut upload, &remote, &answers.control).await.unwrap();

        assert_eq!(remote.bytes("/dest/file"), data);
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![TransferQuestionKind::Resume]
        );
        assert_eq!(upload.meter.progress.succeeded(), 1);
    });
}

#[test]
fn lost_replies_at_backup_publish_and_close_are_reconciled() {
    runtime().block_on(async {
        for fault in [Fault::Rename(1), Fault::Rename(2), Fault::Close] {
            let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let source = tmp.path().join("file");
            std::fs::write(&source, b"new contents").unwrap();
            let journal = Journal::new(tmp.path().join("journal"));
            let remote = Remote::new();
            remote.file("/dest/file", b"original");
            remote.fault.set(Some(fault));
            let answers = Answers::new(vec![TransferChoice::Overwrite]);
            let mut first = batch(&source, journal.clone(), &answers.control).await;
            assert!(run(&mut first, &remote, &answers.control).await.is_err());
            let answers = Answers::new(vec![TransferChoice::Resume]);
            let mut resumed = batch(&source, journal.clone(), &answers.control).await;
            run(&mut resumed, &remote, &answers.control).await.unwrap();
            assert_eq!(remote.bytes("/dest/file"), b"new contents");
            assert_eq!(remote.nodes.borrow().len(), 1);
            assert_eq!(resumed.meter.progress.succeeded(), 1);
        }
    });
}

#[test]
fn cancellation_retains_journal_and_skip_does_not_overwrite() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        std::fs::write(&source, b"new").unwrap();
        let remote = Remote::new();
        remote.file("/dest/file", b"old");
        let answers = Answers::new(vec![TransferChoice::Skip]);
        let journal = Journal::new(tmp.path().join("journal"));
        let mut skipped = batch(&source, journal.clone(), &answers.control).await;
        run(&mut skipped, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/file"), b"old");
        assert_eq!(skipped.meter.progress.skipped(), 1);
        let answers = Answers::new(vec![TransferChoice::Overwrite]);
        let mut canceled = batch(&source, journal, &answers.control).await;
        answers.cancel.send_replace(true);
        assert!(
            run(&mut canceled, &remote, &answers.control)
                .await
                .unwrap_err()
                .is::<super::control::Cancelled>()
        );
        assert_eq!(remote.bytes("/dest/file"), b"old");
    });
}

#[test]
fn directory_permission_error_is_reported_as_failure() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("blocked");
        std::fs::create_dir(&source).unwrap();
        let remote = Remote::new();
        *remote.denied.borrow_mut() = Some("/dest/blocked".into());
        let answers = Answers::new(vec![TransferChoice::Skip]);
        let mut batch = batch(
            &source,
            Journal::new(tmp.path().join("journal")),
            &answers.control,
        )
        .await;
        run(&mut batch, &remote, &answers.control).await.unwrap();
        assert_eq!(batch.meter.progress.failed(), 1);
        assert_eq!(batch.meter.progress.succeeded(), 0);
        let detail = &batch.meter.progress.details()[0];
        assert_eq!(detail.outcome(), TransferOutcome::Failed);
        assert!(detail.reason().unwrap().contains("permission denied"));
    });
}

/// A folder that could not be listed is scanned again on 重试. The batch then
/// grows by the files in it, and by nothing for the folder itself.
#[cfg(unix)]
#[test]
fn retrying_an_unreadable_folder_adds_only_its_files_to_the_upload() {
    use std::os::unix::fs::PermissionsExt as _;
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let folder = tmp.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("inside.bin"), vec![1; 300]).unwrap();
        let beside = tmp.path().join("beside.bin");
        std::fs::write(&beside, vec![2; 10_000]).unwrap();
        let set_mode = |mode| {
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        // Unreadable while the batch is scanned, readable again by the retry.
        set_mode(0o000);
        if std::fs::read_dir(&folder).is_ok() {
            // Nothing is unreadable to root.
            set_mode(0o755);
            return;
        }
        let answers = Answers::new(vec![TransferChoice::Retry]);
        let request = UploadRequest::new(
            vec![folder.clone(), beside],
            RemotePath::new("/dest").unwrap(),
        )
        .unwrap();
        let mut batch = UploadBatch::scan(
            &request,
            "retry",
            "key",
            Journal::new(tmp.path().join("journal")),
            &answers.control,
        )
        .await
        .unwrap();
        set_mode(0o755);
        assert_eq!(batch.meter.progress.total_bytes(), 10_000);

        let remote = Remote::new();
        run(&mut batch, &remote, &answers.control).await.unwrap();
        assert_eq!(batch.meter.progress.failed(), 0);
        assert_eq!(batch.meter.progress.total_bytes(), 10_300);
        assert_eq!(batch.meter.progress.completed_bytes(), 10_300);
        assert_eq!(remote.bytes("/dest/folder/inside.bin"), vec![1; 300]);
    });
}

/// The same for a download: a remote folder has a size of its own, which is
/// not part of what the batch moves.
#[test]
fn retrying_an_unreadable_folder_adds_only_its_files_to_the_download() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let remote = Remote::new();
        remote.nodes.borrow_mut().insert(
            "/srv/folder".into(),
            Node {
                metadata: FileMetadata::new(EntryKind::Directory, 4096, None, Some(0o040_755)),
                data: vec![],
                link: String::new(),
            },
        );
        remote.file("/srv/folder/inside.bin", &[1; 300]);
        remote.file("/srv/beside.bin", &[2; 10_000]);
        // Unreadable while the batch is scanned, readable again by the retry.
        *remote.denied.borrow_mut() = Some("/srv/folder".into());
        let answers = Answers::new(vec![TransferChoice::Retry]);
        let mut batch = download_batch(
            &["/srv/folder", "/srv/beside.bin"],
            &out,
            DownloadJournal::new(tmp.path().join("journal")),
            &remote,
            &answers.control,
        )
        .await;
        *remote.denied.borrow_mut() = None;
        assert_eq!(batch.meter.progress.total_bytes(), 10_000);

        run_download(&mut batch, &remote, &answers.control)
            .await
            .unwrap();
        assert_eq!(batch.meter.progress.failed(), 0);
        assert_eq!(batch.meter.progress.total_bytes(), 10_300);
        assert_eq!(batch.meter.progress.completed_bytes(), 10_300);
        assert_eq!(
            std::fs::read(out.join("folder/inside.bin")).unwrap(),
            vec![1; 300]
        );
    });
}

#[test]
fn paths_and_journals_reject_unsafe_identity() {
    runtime().block_on(async {
        assert!(RemotePath::new("bad\0name").is_err());
        assert!(RemotePath::new("/dest").unwrap().join("../file").is_err());
        assert_eq!(
            RemotePath::new("/目录\\name/file")
                .unwrap()
                .parent()
                .as_str(),
            "/目录\\name"
        );
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        std::fs::write(&source, b"data").unwrap();
        let source_metadata = SourceMetadata::read(&source).await.unwrap();
        assert_eq!(source_metadata.size, 4);
        let mut record = super::journal::ResumeRecord::new(
            "endpoint",
            "key",
            &source,
            RemotePath::new("/dest/file").unwrap(),
            None,
        )
        .unwrap();
        record.source_metadata = Some(source_metadata);
        assert_eq!(record.version, 2);
        assert_eq!(record.temporary.as_str(), "/dest/file.filepart");
        assert!(
            record
                .validate("endpoint", "changed key", &source, &record.target)
                .is_err()
        );
        record.temporary = RemotePath::new("/other/.shellrs-evil.filepart").unwrap();
        assert!(
            record
                .validate("endpoint", "key", &source, &record.target)
                .is_err()
        );
    });
}

/// Drives are titled as WinSCP does: the letter, then what kind it is.
#[test]
fn drives_are_titled_by_letter_and_kind() {
    assert_eq!(Place::Drive('C', DriveKind::Local).title(), "C: 本地磁盘");
    assert_eq!(
        Place::Drive('E', DriveKind::Removable).title(),
        "E: 可移动磁盘"
    );
    assert_eq!(
        Place::Drive('Z', DriveKind::Network).title(),
        "Z: 网络驱动器"
    );
    assert_eq!(
        Place::Drive('F', DriveKind::Optical).title(),
        "F: CD 驱动器"
    );
    assert_eq!(Place::Drive('G', DriveKind::Other).title(), "G:");
}

/// Without the drives, the local pane could not leave the one it started on.
#[cfg(windows)]
#[test]
fn local_listing_places_include_the_system_drive() {
    let system = std::env::var("SystemDrive").unwrap();
    let letter = system.chars().next().unwrap().to_ascii_uppercase();
    let places = SystemLocalDirectoryProvider.places();
    assert!(
        places.contains(&(
            Place::Drive(letter, DriveKind::Local),
            std::path::PathBuf::from(format!("{letter}:\\"))
        )),
        "{places:?}"
    );
}

/// On Windows std canonicalizes to `\\?\C:\…`, which the pane showed as is.
#[test]
fn local_listing_reports_the_path_people_type() {
    let tmp = tempfile::tempdir().unwrap();
    let listing = SystemLocalDirectoryProvider.list(tmp.path()).unwrap();
    assert!(!listing.path().starts_with(r"\\?\"), "{}", listing.path());
    // Still the directory asked for, links resolved.
    assert_eq!(
        std::fs::canonicalize(listing.path()).unwrap(),
        std::fs::canonicalize(tmp.path()).unwrap()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn local_listing_rejects_unrepresentable_names() {
    use std::os::unix::ffi::OsStringExt as _;
    let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    std::fs::write(
        tmp.path().join(std::ffi::OsString::from_vec(vec![0xff])),
        [],
    )
    .unwrap();
    assert!(SystemLocalDirectoryProvider.list(tmp.path()).is_err());
}

#[cfg(unix)]
#[test]
fn openssh_protocol_uploads_offsets_links_metadata_and_replaces_readonly_targets() {
    runtime().block_on(async {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let dest = temp.path().join("remote");
        let source = temp.path().join("中文 目录");
        std::fs::create_dir_all(source.join("空目录/嵌套")).unwrap();
        std::fs::create_dir(&dest).unwrap();
        let data = vec![28; 1024 * 1024 + 7];
        std::fs::write(source.join("文件.bin"), &data).unwrap();
        std::fs::write(source.join("零字节"), []).unwrap();
        std::os::unix::fs::symlink("文件.bin", source.join("链接")).unwrap();
        let (client, mut process) = super::client::SftpClient::local_test_server(&dest)
            .await
            .unwrap();
        let target = client
            .canonicalize(&RemotePath::new(".").unwrap())
            .await
            .unwrap();
        let answers = Answers::new(vec![]);
        let request = UploadRequest::new(vec![source.clone()], target.clone()).unwrap();
        let journal = Journal::new(temp.path().join("journal"));
        let mut upload = UploadBatch::scan(
            &request,
            "local-protocol",
            client.fingerprint(),
            journal.clone(),
            &answers.control,
        )
        .await
        .unwrap();
        run(&mut upload, &client, &answers.control).await.unwrap();
        assert_eq!(
            std::fs::read(dest.join("中文 目录/文件.bin")).unwrap(),
            data
        );
        assert!(dest.join("中文 目录/空目录/嵌套").is_dir());
        assert_eq!(
            std::fs::read_link(dest.join("中文 目录/链接")).unwrap(),
            Path::new("文件.bin")
        );
        assert_eq!(
            std::fs::metadata(dest.join("中文 目录/零字节"))
                .unwrap()
                .len(),
            0
        );
        let listing = client
            .list(&target.join("中文 目录").unwrap())
            .await
            .unwrap();
        assert_eq!(listing.entries().len(), 4);
        let file = source.join("文件.bin");
        let dest_file = dest.join("中文 目录/文件.bin");
        std::fs::set_permissions(&dest_file, std::fs::Permissions::from_mode(0o400)).unwrap();
        std::fs::write(&file, b"replaced").unwrap();
        let request =
            UploadRequest::new(vec![file.clone()], target.join("中文 目录").unwrap()).unwrap();
        let answers = Answers::new(vec![TransferChoice::Overwrite]);
        let mut upload = UploadBatch::scan(
            &request,
            "local-protocol",
            client.fingerprint(),
            journal,
            &answers.control,
        )
        .await
        .unwrap();
        run(&mut upload, &client, &answers.control).await.unwrap();
        assert_eq!(std::fs::read(&dest_file).unwrap(), b"replaced");
        assert_eq!(
            std::fs::metadata(&dest_file).unwrap().permissions().mode() & 0o777,
            0o400
        );
        let local_time = SourceMetadata::read(&file).await.unwrap().modified_ns / 1_000_000_000;
        assert_eq!(
            u128::from(
                client
                    .metadata(&target.join("中文 目录").unwrap().join("文件.bin").unwrap())
                    .await
                    .unwrap()
                    .unwrap()
                    .modified()
                    .unwrap()
            ),
            local_time
        );
        drop(client);
        process.kill().await.ok();
        process.wait().await.unwrap();
    });
}

#[test]
fn failed_compatibility_publish_restores_old_target_and_reports_failure() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("file");
        std::fs::write(&source, b"new").unwrap();
        let remote = Remote::new();
        remote.file("/dest/file", b"original");
        remote.fault.set(Some(Fault::RejectRename(2)));
        let answers = Answers::new(vec![TransferChoice::Overwrite, TransferChoice::Skip]);
        let mut upload = batch(
            &source,
            Journal::new(temp.path().join("journal")),
            &answers.control,
        )
        .await;
        run(&mut upload, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/file"), b"original");
        assert_eq!(upload.meter.progress.failed(), 1);
        assert_eq!(upload.meter.progress.succeeded(), 0);
        assert!(
            remote
                .nodes
                .borrow()
                .keys()
                .any(|p| p.ends_with(".filepart"))
        );
        assert!(!remote.nodes.borrow().keys().any(|p| p.ends_with(".backup")));
    });
}

#[test]
fn target_changed_during_upload_requires_another_conflict_answer() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("file");
        std::fs::write(&source, b"new").unwrap();
        let remote = Remote::new();
        remote.file("/dest/file", b"original");
        remote.fault.set(Some(Fault::ChangeTargetOnClose));
        let answers = Answers::new(vec![TransferChoice::Overwrite, TransferChoice::Skip]);
        let mut upload = batch(
            &source,
            Journal::new(temp.path().join("journal")),
            &answers.control,
        )
        .await;
        run(&mut upload, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/file"), b"external mutation");
        assert_eq!(upload.meter.progress.skipped(), 1);
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![
                TransferQuestionKind::Conflict,
                TransferQuestionKind::Conflict
            ]
        );
        assert_eq!(remote.nodes.borrow().len(), 1);
    });
}

#[test]
fn cancel_then_reset_allows_new_commands_without_false_cancellation() {
    runtime().block_on(async {
        let answers = Answers::new(vec![]);
        answers.cancel.send_replace(false);
        assert_eq!(answers.control.run(async { Ok(42) }).await.unwrap(), 42);
        answers.cancel.send_replace(true);
        assert!(answers.control.run(async { Ok(42) }).await.is_err());
        answers.cancel.send_replace(false);
        assert_eq!(answers.control.run(async { Ok(43) }).await.unwrap(), 43);
    });
}

#[test]
fn target_removed_during_upload_still_requires_confirmation() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("file");
        std::fs::write(&source, b"new").unwrap();
        let remote = Remote::new();
        remote.file("/dest/file", b"old");
        remote.fault.set(Some(Fault::RemoveTargetOnClose));
        let answers = Answers::new(vec![TransferChoice::Overwrite, TransferChoice::Skip]);
        let mut upload = batch(
            &source,
            Journal::new(temp.path().join("journal")),
            &answers.control,
        )
        .await;
        run(&mut upload, &remote, &answers.control).await.unwrap();
        assert!(remote.nodes.borrow().is_empty());
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![
                TransferQuestionKind::Conflict,
                TransferQuestionKind::Conflict
            ]
        );
        assert_eq!(upload.meter.progress.skipped(), 1);
    });
}

fn remote_path(path: &str) -> RemotePath {
    RemotePath::new(path).unwrap()
}

#[test]
fn deleting_a_tree_removes_links_but_never_what_they_point_to() {
    runtime().block_on(async {
        let remote = Remote::new();
        remote.dir("/keep", 0o755);
        remote.file("/keep/important", b"data");
        remote.dir("/gone", 0o755);
        remote.dir("/gone/nested", 0o755);
        remote.file("/gone/nested/file", b"x");
        remote.link("/gone/link-to-keep", "/keep");
        remote.file("/lone", b"y");
        super::operations::run(
            &remote,
            &RemoteOperation::Delete {
                paths: vec![
                    remote_path("/gone"),
                    remote_path("/lone"),
                    remote_path("/missing"),
                ],
            },
        )
        .await
        .unwrap();
        let keys: Vec<_> = remote.nodes.borrow().keys().cloned().collect();
        assert_eq!(keys, ["/keep", "/keep/important"]);
    });
}

#[test]
fn recursive_permissions_skip_links_and_can_keep_directories_searchable() {
    runtime().block_on(async {
        let remote = Remote::new();
        remote.dir("/site", 0o700);
        remote.dir("/site/css", 0o700);
        remote.file("/site/css/a.css", b"a");
        remote.file("/site/index.html", b"i");
        remote.link("/site/outside", "/etc");
        remote.dir("/etc", 0o755);
        let chmod = |recursive, add_x_to_dirs| RemoteOperation::SetPermissions {
            paths: vec![remote_path("/site")],
            edit: PermissionEdit::exact(0o644),
            recursive,
            add_x_to_dirs,
        };
        super::operations::run(&remote, &chmod(true, true))
            .await
            .unwrap();
        assert_eq!(remote.mode("/site"), 0o755);
        assert_eq!(remote.mode("/site/css"), 0o755);
        assert_eq!(remote.mode("/site/css/a.css"), 0o644);
        assert_eq!(remote.mode("/site/index.html"), 0o644);
        assert_eq!(remote.mode("/site/outside"), 0o777, "links are skipped");
        assert_eq!(remote.mode("/etc"), 0o755, "and never followed");
        super::operations::run(&remote, &chmod(false, false))
            .await
            .unwrap();
        assert_eq!(remote.mode("/site"), 0o644);
        assert_eq!(remote.mode("/site/css"), 0o755, "not recursive this time");
    });
}

#[test]
fn rename_and_create_refuse_existing_names() {
    runtime().block_on(async {
        let remote = Remote::new();
        remote.file("/a", b"a");
        remote.file("/b", b"b");
        let error = super::operations::run(
            &remote,
            &RemoteOperation::Rename {
                from: remote_path("/a"),
                to: remote_path("/b"),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "已有名为「b」的项目");
        assert_eq!(remote.bytes("/b"), b"b");
        super::operations::run(
            &remote,
            &RemoteOperation::Rename {
                from: remote_path("/a"),
                to: remote_path("/c"),
            },
        )
        .await
        .unwrap();
        assert_eq!(remote.bytes("/c"), b"a");
        for operation in [
            RemoteOperation::CreateDirectory {
                path: remote_path("/c"),
            },
            RemoteOperation::CreateFile {
                path: remote_path("/c"),
            },
        ] {
            assert!(super::operations::run(&remote, &operation).await.is_err());
        }
        super::operations::run(
            &remote,
            &RemoteOperation::CreateDirectory {
                path: remote_path("/new"),
            },
        )
        .await
        .unwrap();
        super::operations::run(
            &remote,
            &RemoteOperation::CreateFile {
                path: remote_path("/new/empty.txt"),
            },
        )
        .await
        .unwrap();
        assert_eq!(remote.bytes("/new/empty.txt"), b"");
    });
}

async fn download_batch(
    sources: &[&str],
    destination: &Path,
    journal: DownloadJournal,
    remote: &Remote,
    control: &TransferControl,
) -> DownloadBatch {
    let request = DownloadRequest::new(
        sources.iter().map(|source| remote_path(source)).collect(),
        destination.to_path_buf(),
    )
    .unwrap();
    DownloadBatch::scan(
        &request,
        "test@remote",
        "host-key",
        journal,
        remote,
        control,
    )
    .await
    .unwrap()
}
async fn run_download(
    batch: &mut DownloadBatch,
    remote: &Remote,
    control: &TransferControl,
) -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        batch.run(remote, control),
    )
    .await
    .expect("download did not finish")
}

#[test]
fn download_copies_trees_links_and_empty_directories_in_order() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let data: Vec<u8> = (0..2 * 1024 * 1024 + 13).map(|i| (i % 251) as u8).collect();
        for short_reads in [false, true] {
            let out = tmp.path().join(format!("out-{short_reads}"));
            std::fs::create_dir(&out).unwrap();
            let remote = Remote::new();
            remote.short_reads.set(short_reads);
            remote.dir("/srv/中文 目录", 0o755);
            remote.file("/srv/中文 目录/large.bin", &data);
            remote.file("/srv/中文 目录/零 字节", &[]);
            remote.dir("/srv/中文 目录/空", 0o755);
            remote.dir("/srv/中文 目录/嵌套", 0o755);
            remote.file("/srv/中文 目录/嵌套/b.txt", b"nested");
            remote.link("/srv/中文 目录/链接", "large.bin");
            let answers = Answers::new(vec![]);
            let journal = DownloadJournal::new(tmp.path().join(format!("journal-{short_reads}")));
            let mut batch = download_batch(
                &["/srv/中文 目录", "/srv/中文 目录/嵌套"],
                &out,
                journal,
                &remote,
                &answers.control,
            )
            .await;
            run_download(&mut batch, &remote, &answers.control)
                .await
                .unwrap();
            let root = out.join("中文 目录");
            assert_eq!(std::fs::read(root.join("large.bin")).unwrap(), data);
            assert_eq!(std::fs::read(root.join("零 字节")).unwrap(), b"");
            assert_eq!(std::fs::read(root.join("嵌套/b.txt")).unwrap(), b"nested");
            assert!(root.join("空").is_dir());
            #[cfg(unix)]
            assert_eq!(
                std::fs::read_link(root.join("链接")).unwrap(),
                Path::new("large.bin")
            );
            assert!(!partial_path(&root.join("large.bin")).exists());
            assert_eq!(batch.meter.progress.failed(), 0);
            assert_eq!(
                batch.meter.progress.direction(),
                TransferDirection::Download
            );
            assert!(answers.questions.lock().unwrap().is_empty());
            let modified = std::fs::metadata(root.join("large.bin"))
                .unwrap()
                .modified()
                .unwrap();
            assert_eq!(
                modified,
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(100)
            );
        }
    });
}

#[test]
fn download_resumes_from_the_partial_file_after_a_lost_connection() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let data: Vec<u8> = (0..300_000).map(|i| (i % 253) as u8).collect();
        let remote = Remote::new();
        remote.file("/big", &data);
        remote.fault.set(Some(Fault::Read(5)));
        let journal = DownloadJournal::new(tmp.path().join("journal"));
        let answers = Answers::new(vec![]);
        let mut first = download_batch(
            &["/big"],
            tmp.path(),
            journal.clone(),
            &remote,
            &answers.control,
        )
        .await;
        let error = run_download(&mut first, &remote, &answers.control)
            .await
            .unwrap_err();
        assert!(super::client::is_network_error(&error));
        let partial = partial_path(&tmp.path().join("big"));
        let kept = std::fs::read(&partial).unwrap();
        assert!(!kept.is_empty() && kept.len() < data.len());
        assert_eq!(kept, data[..kept.len()], "written strictly in order");
        assert!(!tmp.path().join("big").exists());

        let answers = Answers::new(vec![TransferChoice::Resume]);
        let mut second =
            download_batch(&["/big"], tmp.path(), journal, &remote, &answers.control).await;
        run_download(&mut second, &remote, &answers.control)
            .await
            .unwrap();
        assert_eq!(std::fs::read(tmp.path().join("big")).unwrap(), data);
        assert!(!partial.exists());
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![TransferQuestionKind::Resume]
        );
        assert!(
            std::fs::read_dir(tmp.path().join("journal"))
                .unwrap()
                .next()
                .is_none()
        );
    });
}

#[test]
fn download_asks_before_overwriting_and_keeps_the_replaced_permissions() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let target = tmp.path().join("file");
        std::fs::write(&target, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let remote = Remote::new();
        remote.file("/file", b"new contents");
        let journal = DownloadJournal::new(tmp.path().join("journal"));
        let answers = Answers::new(vec![TransferChoice::Skip]);
        let mut skipped = download_batch(
            &["/file"],
            tmp.path(),
            journal.clone(),
            &remote,
            &answers.control,
        )
        .await;
        run_download(&mut skipped, &remote, &answers.control)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        assert_eq!(skipped.meter.progress.skipped(), 1);

        let answers = Answers::new(vec![TransferChoice::Overwrite]);
        let mut replaced =
            download_batch(&["/file"], tmp.path(), journal, &remote, &answers.control).await;
        run_download(&mut replaced, &remote, &answers.control)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new contents");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![TransferQuestionKind::Conflict]
        );
    });
}

#[test]
fn a_changed_remote_file_is_not_resumed_and_discard_cleans_up() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let remote = Remote::new();
        remote.file("/file", &vec![7; 200_000]);
        remote.fault.set(Some(Fault::Read(4)));
        let journal = DownloadJournal::new(tmp.path().join("journal"));
        let answers = Answers::new(vec![]);
        let mut first = download_batch(
            &["/file"],
            tmp.path(),
            journal.clone(),
            &remote,
            &answers.control,
        )
        .await;
        assert!(
            run_download(&mut first, &remote, &answers.control)
                .await
                .is_err()
        );
        let partial = partial_path(&tmp.path().join("file"));
        assert!(partial.exists());

        // Discarding a stopped batch removes the partial file and its record.
        first.discard().await.unwrap();
        assert!(!partial.exists());
        assert!(
            std::fs::read_dir(tmp.path().join("journal"))
                .unwrap()
                .next()
                .is_none()
        );

        // A partial file from a remote file that has since changed.
        remote.reads.set(0);
        remote.fault.set(Some(Fault::Read(4)));
        let answers = Answers::new(vec![]);
        let mut again = download_batch(
            &["/file"],
            tmp.path(),
            journal.clone(),
            &remote,
            &answers.control,
        )
        .await;
        assert!(
            run_download(&mut again, &remote, &answers.control)
                .await
                .is_err()
        );
        remote.file("/file", b"rewritten");
        let answers = Answers::new(vec![TransferChoice::Restart]);
        let mut restarted =
            download_batch(&["/file"], tmp.path(), journal, &remote, &answers.control).await;
        run_download(&mut restarted, &remote, &answers.control)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(tmp.path().join("file")).unwrap(),
            b"rewritten"
        );
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![TransferQuestionKind::InvalidResume]
        );
    });
}

#[test]
fn download_cancellation_keeps_the_partial_file() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let remote = Remote::new();
        remote.file("/file", &vec![1; 100_000]);
        let answers = Answers::new(vec![]);
        let mut batch = download_batch(
            &["/file"],
            tmp.path(),
            DownloadJournal::new(tmp.path().join("journal")),
            &remote,
            &answers.control,
        )
        .await;
        answers.cancel.send_replace(true);
        let error = run_download(&mut batch, &remote, &answers.control)
            .await
            .unwrap_err();
        assert!(error.is::<super::control::Cancelled>());
        assert!(!tmp.path().join("file").exists());
    });
}

#[cfg(unix)]
#[test]
fn openssh_protocol_downloads_lists_owners_and_changes_files_without_following_links() {
    runtime().block_on(async {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let served = temp.path().join("remote");
        let tree = served.join("树");
        std::fs::create_dir_all(tree.join("空目录")).unwrap();
        let data: Vec<u8> = (0..1024 * 1024 + 7).map(|i| (i % 249) as u8).collect();
        std::fs::write(tree.join("文件.bin"), &data).unwrap();
        std::os::unix::fs::symlink("文件.bin", tree.join("链接")).unwrap();
        std::fs::create_dir(served.join("keep")).unwrap();
        std::fs::write(served.join("keep/important"), b"keep").unwrap();
        std::os::unix::fs::symlink("../keep", tree.join("外部")).unwrap();
        let (client, mut process) = super::client::SftpClient::local_test_server(&served)
            .await
            .unwrap();
        let root = client
            .canonicalize(&RemotePath::new(".").unwrap())
            .await
            .unwrap();

        // Listing: owners come from the server's long names, links resolve.
        let listing = client.list(&root.join("树").unwrap()).await.unwrap();
        let user = std::env::var("USER").unwrap_or_default();
        let entry = |name: &str| {
            listing
                .entries()
                .iter()
                .find(|entry| entry.name() == name)
                .unwrap()
                .clone()
        };
        if !user.is_empty() {
            assert_eq!(entry("文件.bin").owner(), Some(user.as_str()));
        }
        assert_eq!(entry("外部").target_kind(), Some(EntryKind::Directory));
        assert_eq!(entry("链接").target_kind(), Some(EntryKind::File));

        // Download the tree.
        let out = temp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let answers = Answers::new(vec![]);
        let request = DownloadRequest::new(vec![root.join("树").unwrap()], out.clone()).unwrap();
        let mut download = DownloadBatch::scan(
            &request,
            "local-protocol",
            client.fingerprint(),
            DownloadJournal::new(temp.path().join("journal")),
            &client,
            &answers.control,
        )
        .await
        .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(20),
            download.run(&client, &answers.control),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(std::fs::read(out.join("树/文件.bin")).unwrap(), data);
        assert!(out.join("树/空目录").is_dir());
        assert_eq!(
            std::fs::read_link(out.join("树/链接")).unwrap(),
            Path::new("文件.bin")
        );

        // Operations on the real server.
        let path =
            |relative: &str| RemotePath::new(format!("{}/{relative}", root.as_str())).unwrap();
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        let keep_mode = mode(&served.join("keep"));
        super::operations::run(
            &client,
            &RemoteOperation::SetPermissions {
                paths: vec![path("树")],
                edit: PermissionEdit::exact(0o640),
                recursive: true,
                add_x_to_dirs: true,
            },
        )
        .await
        .unwrap();
        assert_eq!(mode(&tree.join("文件.bin")), 0o640);
        assert_eq!(mode(&tree.join("空目录")), 0o750);
        assert_eq!(
            mode(&served.join("keep")),
            keep_mode,
            "a linked directory is not followed"
        );
        super::operations::run(
            &client,
            &RemoteOperation::Rename {
                from: path("树/文件.bin"),
                to: path("树/改名.bin"),
            },
        )
        .await
        .unwrap();
        assert!(tree.join("改名.bin").exists());
        super::operations::run(
            &client,
            &RemoteOperation::CreateFile {
                path: path("树/空目录/新文件.txt"),
            },
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(tree.join("空目录/新文件.txt")).unwrap(), b"");
        super::operations::run(
            &client,
            &RemoteOperation::Delete {
                paths: vec![path("树")],
            },
        )
        .await
        .unwrap();
        assert!(!tree.exists());
        assert_eq!(
            std::fs::read(served.join("keep/important")).unwrap(),
            b"keep"
        );
        drop(client);
        process.kill().await.ok();
        process.wait().await.unwrap();
    });
}

#[test]
fn scp_puts_a_copy_inside_a_directory_or_at_the_path_itself() {
    use super::model::{scp_local_target, scp_remote_target};
    let path = |path: &str| RemotePath::new(path).unwrap();
    assert_eq!(
        scp_remote_target(&path("/tmp/dist"), true),
        (path("/tmp/dist"), None)
    );
    assert_eq!(
        scp_remote_target(&path("/tmp/release.tar.gz"), false),
        (path("/tmp"), Some("release.tar.gz".into()))
    );
    // Relative to the login directory, not to the root.
    assert_eq!(
        scp_remote_target(&path("./app"), false),
        (path("."), Some("app".into()))
    );
    assert_eq!(scp_remote_target(&path("/"), true), (path("/"), None));

    assert_eq!(
        scp_local_target(Path::new("/work/logs"), true),
        (Path::new("/work/logs").to_path_buf(), None)
    );
    assert_eq!(
        scp_local_target(Path::new("/work/latest.tar.gz"), false),
        (
            Path::new("/work").to_path_buf(),
            Some("latest.tar.gz".into())
        )
    );
}

#[cfg(unix)]
#[test]
fn an_scp_upload_takes_the_destination_name_and_keeps_the_execute_bits() {
    use std::os::unix::fs::PermissionsExt as _;
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("build.sh");
        std::fs::write(&source, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        let remote = Remote::new();

        let request = UploadRequest::scp(source.clone(), remote_path("/dest/deploy.sh"))
            .resolved(remote_path("/dest"), Some("deploy.sh".into()));
        let answers = Answers::new(vec![]);
        let mut batch = UploadBatch::scan(
            &request,
            "test@remote",
            "host-key",
            Journal::new(tmp.path().join("journal")),
            &answers.control,
        )
        .await
        .unwrap();
        run(&mut batch, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/deploy.sh"), b"#!/bin/sh\n");
        assert_eq!(remote.mode("/dest/deploy.sh"), 0o755);

        // The SFTP tab's uploads leave new files to the server's defaults.
        let mut plain = batch_to(&source, "/plain", tmp.path(), &answers.control).await;
        run(&mut plain, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.mode("/plain/build.sh"), 0o644);
    });
}

#[cfg(unix)]
async fn batch_to(
    source: &Path,
    destination: &str,
    tmp: &Path,
    control: &TransferControl,
) -> UploadBatch {
    let request = UploadRequest::new(vec![source.into()], remote_path(destination)).unwrap();
    UploadBatch::scan(
        &request,
        "test@remote",
        "host-key",
        Journal::new(tmp.join("plain-journal")),
        control,
    )
    .await
    .unwrap()
}

#[cfg(unix)]
#[test]
fn an_scp_download_takes_the_destination_name_and_keeps_the_execute_bits() {
    use std::os::unix::fs::PermissionsExt as _;
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let remote = Remote::new();
        remote.file("/srv/run.sh", b"#!/bin/sh\n");
        remote
            .nodes
            .borrow_mut()
            .get_mut("/srv/run.sh")
            .unwrap()
            .metadata = FileMetadata::new(EntryKind::File, 10, Some(100), Some(0o755));

        let answers = Answers::new(vec![]);
        let request = DownloadRequest::scp(remote_path("/srv/run.sh"), out.join("start.sh"))
            .unwrap()
            .resolved(out.clone(), Some("start.sh".into()));
        let mut batch = DownloadBatch::scan(
            &request,
            "test@remote",
            "host-key",
            DownloadJournal::new(tmp.path().join("journal")),
            &remote,
            &answers.control,
        )
        .await
        .unwrap();
        run_download(&mut batch, &remote, &answers.control)
            .await
            .unwrap();
        let copy = out.join("start.sh");
        assert_eq!(std::fs::read(&copy).unwrap(), b"#!/bin/sh\n");
        assert_ne!(
            std::fs::metadata(&copy).unwrap().permissions().mode() & 0o100,
            0
        );
        assert!(!out.join("run.sh").exists());

        // The SFTP tab's downloads keep this machine's defaults.
        let plain = tmp.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        let mut batch = download_batch(
            &["/srv/run.sh"],
            &plain,
            DownloadJournal::new(tmp.path().join("plain-journal")),
            &remote,
            &answers.control,
        )
        .await;
        run_download(&mut batch, &remote, &answers.control)
            .await
            .unwrap();
        let mode = std::fs::metadata(plain.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0);
    });
}

#[test]
fn a_text_file_is_read_whole_through_short_and_reordered_reads() {
    runtime().block_on(async {
        let remote = Remote::new();
        let text: String = (0..40_000).map(|i| format!("line {i}\n")).collect();
        remote.file("/etc/app.conf", text.as_bytes());
        remote.short_reads.set(true);
        let file = read_text(&remote, &remote_path("/etc/app.conf"))
            .await
            .unwrap();
        assert_eq!(file.text(), text);
        assert_eq!(
            file.stamp(),
            FileStamp::of(&remote.nodes.borrow()["/etc/app.conf"].metadata)
        );
        assert!(remote.reads.get() > text.len() / 1000);
    });
}

#[test]
fn a_link_is_followed_to_the_file_it_points_at() {
    runtime().block_on(async {
        let remote = Remote::new();
        remote.file("/srv/real.conf", b"old\n");
        remote.link("/etc/app.conf", "/srv/real.conf");
        let path = remote_path("/etc/app.conf");
        let file = read_text(&remote, &path).await.unwrap();
        assert_eq!(file.text(), "old\n");
        write_in_place(&remote, &path, b"new\n".to_vec(), Some(file.stamp()))
            .await
            .unwrap();
        // The link is still a link; the file behind it changed.
        assert_eq!(
            remote.nodes.borrow()["/etc/app.conf"].metadata.kind(),
            EntryKind::Symlink
        );
        assert_eq!(remote.bytes("/srv/real.conf"), b"new\n");
    });
}

#[test]
fn directories_large_files_and_binary_files_are_refused() {
    runtime().block_on(async {
        let remote = Remote::new();
        remote.dir("/etc", 0o755);
        remote.file("/var/big.log", &vec![b'x'; EDIT_LIMIT as usize + 1]);
        let mut binary = vec![b'a'; 1_000_000];
        binary[10] = 0;
        remote.file("/bin/tool", &binary);
        let refusal = |path: &'static str| {
            let remote = &remote;
            async move {
                ReadFailure::from_error(read_text(remote, &remote_path(path)).await.unwrap_err())
            }
        };
        assert_eq!(refusal("/etc").await, ReadFailure::NotFile);
        assert_eq!(
            refusal("/var/big.log").await,
            ReadFailure::TooLarge(EDIT_LIMIT + 1)
        );
        let reads = remote.reads.get();
        assert_eq!(refusal("/bin/tool").await, ReadFailure::NotText);
        // Given up after the first chunk, not after reading it all.
        assert!(remote.reads.get() - reads < 31);
        assert!(matches!(refusal("/missing").await, ReadFailure::Failed(_)));
    });
}

#[test]
fn a_save_writes_in_place_and_keeps_the_permissions() {
    runtime().block_on(async {
        let remote = Remote::new();
        let long: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8 + 1).collect();
        remote.file("/etc/app.conf", &long);
        let path = remote_path("/etc/app.conf");
        let file_stamp = FileStamp::of(&remote.nodes.borrow()["/etc/app.conf"].metadata);
        let shorter = b"short".to_vec();
        let stamp = write_in_place(&remote, &path, shorter.clone(), Some(file_stamp))
            .await
            .unwrap();
        // Truncated, not overwritten at the start.
        assert_eq!(remote.bytes("/etc/app.conf"), shorter);
        assert_eq!(stamp.size(), 5);
        let metadata = remote.nodes.borrow()["/etc/app.conf"].metadata.clone();
        assert_eq!(metadata.permissions(), Some(0o640));
        assert_eq!(remote.renames.get(), 0);

        // A long write goes in parallel chunks.
        write_in_place(&remote, &path, long.clone(), None)
            .await
            .unwrap();
        assert_eq!(remote.bytes("/etc/app.conf"), long);
        assert!(remote.max_in_flight.get() > 1);
    });
}

#[test]
fn a_save_over_a_file_changed_or_removed_since_is_refused() {
    runtime().block_on(async {
        let remote = Remote::new();
        remote.file("/etc/app.conf", b"mine\n");
        let path = remote_path("/etc/app.conf");
        let file = read_text(&remote, &path).await.unwrap();
        remote.file("/etc/app.conf", b"someone else's\n");
        let error = write_in_place(&remote, &path, b"mine 2\n".to_vec(), Some(file.stamp()))
            .await
            .unwrap_err();
        assert_eq!(SaveFailure::from_error(error), SaveFailure::Changed);
        assert_eq!(remote.bytes("/etc/app.conf"), b"someone else's\n");

        remote.nodes.borrow_mut().remove("/etc/app.conf");
        let error = write_in_place(&remote, &path, b"mine 2\n".to_vec(), Some(file.stamp()))
            .await
            .unwrap_err();
        assert_eq!(SaveFailure::from_error(error), SaveFailure::Changed);

        // Saving anyway creates it again.
        write_in_place(&remote, &path, b"mine 2\n".to_vec(), None)
            .await
            .unwrap();
        assert_eq!(remote.bytes("/etc/app.conf"), b"mine 2\n");
    });
}

#[test]
fn a_save_that_fails_after_truncating_is_interrupted() {
    runtime().block_on(async {
        let remote = Remote::new();
        remote.file("/etc/app.conf", &vec![b'a'; 200_000]);
        remote.fault.set(Some(Fault::Write(2)));
        let error = write_in_place(
            &remote,
            &remote_path("/etc/app.conf"),
            vec![b'b'; 200_000],
            None,
        )
        .await
        .unwrap_err();
        assert!(super::client::is_network_error(&error));
        assert!(matches!(
            SaveFailure::from_error(error),
            SaveFailure::Interrupted(_)
        ));
    });
}

#[cfg(unix)]
#[test]
fn openssh_protocol_edits_in_place_through_links_keeping_mode_and_hard_links() {
    runtime().block_on(async {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let served = temp.path().join("remote");
        std::fs::create_dir(&served).unwrap();
        let real = served.join("真的.conf");
        std::fs::write(&real, "line one\r\nline two\r\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink("真的.conf", served.join("链接.conf")).unwrap();
        std::fs::hard_link(&real, served.join("硬链接.conf")).unwrap();
        let (client, mut process) = super::client::SftpClient::local_test_server(&served)
            .await
            .unwrap();
        let base = client
            .canonicalize(&RemotePath::new(".").unwrap())
            .await
            .unwrap();
        let link = base.join("链接.conf").unwrap();

        let file = read_text(&client, &link).await.unwrap();
        assert_eq!(file.text(), "line one\nline two\n");
        assert!(file.format().crlf());
        let bytes = file.format().encode("short\n");
        let stamp = write_in_place(&client, &link, bytes, Some(file.stamp()))
            .await
            .unwrap();

        assert!(
            std::fs::symlink_metadata(served.join("链接.conf"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"short\r\n");
        assert_eq!(
            std::fs::read(served.join("硬链接.conf")).unwrap(),
            b"short\r\n"
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(stamp.size(), 7);

        // Someone else writes: the next save with the old stamp is refused.
        std::fs::write(&real, "theirs, longer\n").unwrap();
        let error = write_in_place(&client, &link, b"mine\n".to_vec(), Some(stamp))
            .await
            .unwrap_err();
        assert_eq!(SaveFailure::from_error(error), SaveFailure::Changed);
        assert_eq!(std::fs::read(&real).unwrap(), b"theirs, longer\n");

        // Too large, without reading it.
        let big = std::fs::File::create(served.join("big.log")).unwrap();
        big.set_len(EDIT_LIMIT + 1).unwrap();
        let error = read_text(&client, &base.join("big.log").unwrap())
            .await
            .unwrap_err();
        assert_eq!(
            ReadFailure::from_error(error),
            ReadFailure::TooLarge(EDIT_LIMIT + 1)
        );
        drop(client);
        let _ = process.kill().await;
    });
}

#[cfg(unix)]
#[test]
fn local_files_are_edited_in_place_through_links() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let provider = SystemLocalDirectoryProvider;
    let real = temp.path().join("real.conf");
    std::fs::write(&real, b"\xEF\xBB\xBFold\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = temp.path().join("link.conf");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let file = read_local_text(&provider, &link).unwrap();
    assert_eq!(file.text(), "old\n");
    assert!(file.format().bom());
    let stamp = write_local_text(
        &provider,
        &link,
        &file.format().encode("new\n"),
        Some(file.stamp()),
    )
    .unwrap();
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&real).unwrap(), b"\xEF\xBB\xBFnew\n");
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o600
    );

    std::fs::write(&real, b"theirs, longer\n").unwrap();
    assert_eq!(
        write_local_text(&provider, &link, b"mine\n", Some(stamp)),
        Err(SaveFailure::Changed)
    );
    assert_eq!(
        read_local_text(&provider, temp.path()),
        Err(ReadFailure::NotFile)
    );
    let big = std::fs::File::create(temp.path().join("big.log")).unwrap();
    big.set_len(EDIT_LIMIT + 1).unwrap();
    assert_eq!(
        read_local_text(&provider, &temp.path().join("big.log")),
        Err(ReadFailure::TooLarge(EDIT_LIMIT + 1))
    );
}

#[test]
fn a_binary_file_is_read_whole_for_a_preview_within_its_limit() {
    runtime().block_on(async {
        let remote = Remote::new();
        let image: Vec<u8> = (0..300_000u32).map(|i| (i % 256) as u8).collect();
        remote.file("/srv/photo.png", &image);
        remote.short_reads.set(true);
        let path = remote_path("/srv/photo.png");
        let bytes = read_whole(&remote, &path, 1_000_000).await.unwrap();
        assert_eq!(bytes.into_inner(), image);
        let error = read_whole(&remote, &path, 1000).await.unwrap_err();
        assert_eq!(
            ReadFailure::from_error(error),
            ReadFailure::TooLarge(300_000)
        );
    });
}

/// The external CLI's sync of `local` into `/srv/app`, resolved as the
/// worker resolves it.
#[cfg(unix)]
async fn sync_batch(local: &Path, tmp: &Path, control: &TransferControl) -> UploadBatch {
    let request = UploadRequest::sync(local.into(), remote_path("/srv/app"), false)
        .resolved(remote_path("/srv"), Some("app".into()));
    UploadBatch::scan(
        &request,
        "test@remote",
        "host-key",
        Journal::new(tmp.join("sync-journal")),
        control,
    )
    .await
    .unwrap()
}

#[cfg(unix)]
#[test]
fn a_sync_copies_only_what_changed_since_the_last() {
    use std::os::unix::fs::PermissionsExt as _;
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let dist = tmp.path().join("dist");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), b"v1").unwrap();
        std::fs::write(dist.join("run.sh"), b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(dist.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::write(dist.join("assets/logo.png"), b"png").unwrap();
        std::os::unix::fs::symlink("index.html", dist.join("home.html")).unwrap();
        let remote = Remote::new();
        remote.dir("/srv", 0o755);

        let answers = Answers::new(vec![]);
        let mut first = sync_batch(&dist, tmp.path(), &answers.control).await;
        run(&mut first, &remote, &answers.control).await.unwrap();
        let progress = &first.meter.progress;
        assert_eq!((progress.succeeded(), progress.skipped()), (6, 0));
        assert_eq!(remote.bytes("/srv/app/assets/logo.png"), b"png");
        // New files keep their permission bits, as with scp.
        assert_eq!(remote.mode("/srv/app/run.sh"), 0o755);

        // Nothing changed: nothing copied, nothing asked, folders included.
        let mut second = sync_batch(&dist, tmp.path(), &answers.control).await;
        run(&mut second, &remote, &answers.control).await.unwrap();
        let progress = &second.meter.progress;
        assert_eq!((progress.succeeded(), progress.skipped()), (0, 6));
        assert!(answers.questions.lock().unwrap().is_empty());

        // One file changed: that one, over what is there.
        std::fs::write(dist.join("index.html"), b"version 2").unwrap();
        let answers = Answers::new(vec![TransferChoice::Overwrite]);
        let mut third = sync_batch(&dist, tmp.path(), &answers.control).await;
        run(&mut third, &remote, &answers.control).await.unwrap();
        let progress = &third.meter.progress;
        assert_eq!((progress.succeeded(), progress.skipped()), (1, 5));
        assert_eq!(remote.bytes("/srv/app/index.html"), b"version 2");
        assert_eq!(
            *answers.questions.lock().unwrap(),
            [TransferQuestionKind::Conflict]
        );
    });
}

#[cfg(unix)]
#[test]
fn a_sync_with_delete_removes_what_is_not_here_and_nothing_it_cannot_see() {
    use std::os::unix::fs::PermissionsExt as _;
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let dist = tmp.path().join("dist");
        std::fs::create_dir_all(dist.join("sub")).unwrap();
        std::fs::create_dir_all(dist.join("locked")).unwrap();
        std::fs::write(dist.join("keep.txt"), b"k").unwrap();
        std::fs::write(dist.join("kind"), b"a file here").unwrap();
        std::fs::set_permissions(dist.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();

        let remote = Remote::new();
        remote.dir("/srv/app", 0o755);
        remote.file("/srv/app/keep.txt", b"k");
        remote.file("/srv/app/extra.txt", b"x");
        remote.dir("/srv/app/old", 0o755);
        remote.file("/srv/app/old/file", b"o");
        // A folder there where a file is here.
        remote.dir("/srv/app/kind", 0o755);
        remote.file("/srv/app/kind/inner", b"i");
        // What a transfer leaves while it runs.
        remote.file("/srv/app/keep.txt.filepart", b"partial");
        remote.file("/srv/app/.shellrs-1234.backup", b"original");
        // A folder this side cannot read, and one that side cannot.
        remote.dir("/srv/app/locked", 0o755);
        remote.file("/srv/app/locked/extra", b"?");
        remote.dir("/srv/app/sub", 0o755);
        remote.file("/srv/app/sub/extra", b"?");
        *remote.denied.borrow_mut() = Some("/srv/app/sub".into());

        let answers = Answers::new(vec![TransferChoice::Skip]);
        let pruned = super::sync::prune(&remote, &dist, &remote_path("/srv/app"), &answers.control)
            .await
            .unwrap();
        std::fs::set_permissions(dist.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert_eq!(pruned.deleted, 3);
        assert_eq!(pruned.failures.len(), 1);
        let keys: Vec<_> = remote.nodes.borrow().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "/srv/app",
                "/srv/app/.shellrs-1234.backup",
                "/srv/app/keep.txt",
                "/srv/app/keep.txt.filepart",
                "/srv/app/locked",
                "/srv/app/locked/extra",
                "/srv/app/sub",
                "/srv/app/sub/extra",
            ]
        );
        assert_eq!(
            *answers.questions.lock().unwrap(),
            [TransferQuestionKind::Error]
        );

        // Counted with the batch that follows: what went, and what failed.
        let mut batch = sync_batch(&dist, tmp.path(), &answers.control).await;
        let total = batch.meter.progress.total();
        batch.add_pruned(pruned.deleted, pruned.failures);
        let progress = &batch.meter.progress;
        assert_eq!((progress.deleted(), progress.failed()), (3, 1));
        assert_eq!(progress.total(), total + 1);
    });
}
