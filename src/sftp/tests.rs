use super::{
    client::RemoteFs,
    control::UploadControl,
    journal::{Journal, SourceMetadata},
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
    renames: Cell<usize>,
    atomic: bool,
    denied: RefCell<Option<String>>,
}
impl Remote {
    fn new(atomic: bool) -> Self {
        Self {
            nodes: RefCell::new(BTreeMap::new()),
            fault: Cell::new(None),
            in_flight: Cell::new(0),
            max_in_flight: Cell::new(0),
            writes: Cell::new(0),
            renames: Cell::new(0),
            atomic,
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
    async fn sync(&self, _: &str) -> Result<()> {
        Ok(())
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
        handle: &str,
        modified: Option<u32>,
        permissions: Option<u32>,
    ) -> Result<()> {
        let mut nodes = self.nodes.borrow_mut();
        let n = nodes.get_mut(handle).unwrap();
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
    async fn rename(&self, from: &RemotePath, to: &RemotePath, replace: bool) -> Result<()> {
        if self.fault.get() == Some(Fault::RejectRename(self.renames.get() + 1)) {
            self.fault.set(None);
            bail!("publish denied");
        }
        let mut nodes = self.nodes.borrow_mut();
        if !replace && nodes.contains_key(to.as_str()) {
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
    fn atomic_replace(&self) -> bool {
        self.atomic
    }
}
struct Answers {
    control: UploadControl,
    cancel: watch::Sender<bool>,
    questions: Arc<Mutex<Vec<UploadQuestionKind>>>,
    progress: Arc<Mutex<Vec<UploadProgress>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Answers {
    fn new(choices: Vec<UploadChoice>) -> Self {
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
                                UploadAnswer::new(
                                    choices.next().unwrap_or(UploadChoice::Cancel),
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
            control: UploadControl {
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
async fn batch(source: &Path, journal: Journal, control: &UploadControl) -> UploadBatch {
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
    control: &UploadControl,
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
        let remote = Remote::new(true);
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
        assert_eq!(batch.progress.failed(), 0);
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
        let remote = Remote::new(true);

        run(&mut batch, &remote, &answers.control).await.unwrap();
        tokio::task::yield_now().await;

        assert_eq!(remote.bytes("/dest/file"), data);
        assert!(remote.max_in_flight.get() > 1);
        assert_eq!(batch.progress.succeeded(), 1);
        let progress = answers.progress.lock().unwrap();
        assert!(progress.iter().all(|snapshot| matches!(
            snapshot.phase(),
            UploadPhase::Uploading | UploadPhase::Completed
        )));
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
        let remote = Remote::new(false);
        remote.file("/dest/file", b"old file intact");
        remote.fault.set(Some(Fault::Write(2)));
        let answers = Answers::new(vec![UploadChoice::Overwrite]);
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
        let answers = Answers::new(vec![UploadChoice::Resume]);
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
        assert_eq!(second.progress.succeeded(), 1);
    });
}

#[test]
fn filepart_larger_than_source_requires_explicit_restart() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        std::fs::write(&source, vec![1; 100_000]).unwrap();
        let journal = Journal::new(tmp.path().join("journal"));
        let remote = Remote::new(true);
        remote.fault.set(Some(Fault::Write(2)));
        let answers = Answers::new(vec![]);
        let mut first = batch(&source, journal.clone(), &answers.control).await;
        assert!(run(&mut first, &remote, &answers.control).await.is_err());
        std::fs::write(&source, vec![2; 32_000]).unwrap();
        let target = RemotePath::new("/dest/file").unwrap();
        let answers = Answers::new(vec![UploadChoice::Resume, UploadChoice::Skip]);
        let mut next = batch(&source, journal, &answers.control).await;
        run(&mut next, &remote, &answers.control).await.unwrap();
        assert!(remote.metadata(&target).await.unwrap().is_none());
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![
                UploadQuestionKind::Resume,
                UploadQuestionKind::InvalidResume
            ]
        );
        assert_eq!(next.progress.skipped(), 1);
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
        let remote = Remote::new(true);
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
        let answers = Answers::new(vec![UploadChoice::Resume]);
        let mut resumed = batch(&source, journal, &answers.control).await;

        run(&mut resumed, &remote, &answers.control).await.unwrap();

        let mut expected = vec![2; 100_000];
        expected[..old_prefix.len()].copy_from_slice(&old_prefix);
        assert_eq!(remote.bytes("/dest/file"), expected);
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![UploadQuestionKind::Resume]
        );
        assert_eq!(resumed.progress.succeeded(), 1);
    });
}

#[test]
fn winscp_style_resume_detects_filepart_without_a_local_record() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        let data = vec![3; 100_000];
        std::fs::write(&source, &data).unwrap();
        let remote = Remote::new(true);
        remote.file("/dest/file.filepart", &data[..32_768]);
        let answers = Answers::new(vec![UploadChoice::Resume]);
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
            vec![UploadQuestionKind::Resume]
        );
        assert_eq!(upload.progress.succeeded(), 1);
    });
}

#[test]
fn lost_replies_at_backup_publish_and_close_are_reconciled() {
    runtime().block_on(async {
        for (atomic, fault) in [
            (true, Fault::Rename(1)),
            (false, Fault::Rename(1)),
            (false, Fault::Rename(2)),
            (false, Fault::Close),
        ] {
            let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let source = tmp.path().join("file");
            std::fs::write(&source, b"new contents").unwrap();
            let journal = Journal::new(tmp.path().join("journal"));
            let remote = Remote::new(atomic);
            remote.file("/dest/file", b"original");
            remote.fault.set(Some(fault));
            let answers = Answers::new(vec![UploadChoice::Overwrite]);
            let mut first = batch(&source, journal.clone(), &answers.control).await;
            assert!(run(&mut first, &remote, &answers.control).await.is_err());
            let answers = Answers::new(vec![UploadChoice::Resume]);
            let mut resumed = batch(&source, journal.clone(), &answers.control).await;
            run(&mut resumed, &remote, &answers.control).await.unwrap();
            assert_eq!(remote.bytes("/dest/file"), b"new contents");
            assert_eq!(remote.nodes.borrow().len(), 1);
            assert_eq!(resumed.progress.succeeded(), 1);
        }
    });
}

#[test]
fn cancellation_retains_journal_and_skip_does_not_overwrite() {
    runtime().block_on(async {
        let tmp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let source = tmp.path().join("file");
        std::fs::write(&source, b"new").unwrap();
        let remote = Remote::new(true);
        remote.file("/dest/file", b"old");
        let answers = Answers::new(vec![UploadChoice::Skip]);
        let journal = Journal::new(tmp.path().join("journal"));
        let mut skipped = batch(&source, journal.clone(), &answers.control).await;
        run(&mut skipped, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/file"), b"old");
        assert_eq!(skipped.progress.skipped(), 1);
        let answers = Answers::new(vec![UploadChoice::Overwrite]);
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
        let remote = Remote::new(true);
        *remote.denied.borrow_mut() = Some("/dest/blocked".into());
        let answers = Answers::new(vec![UploadChoice::Skip]);
        let mut batch = batch(
            &source,
            Journal::new(tmp.path().join("journal")),
            &answers.control,
        )
        .await;
        run(&mut batch, &remote, &answers.control).await.unwrap();
        assert_eq!(batch.progress.failed(), 1);
        assert_eq!(batch.progress.succeeded(), 0);
        assert!(batch.progress.details()[0].contains("permission denied"));
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
        record.temporary = RemotePath::new("/other/.shellr-evil.filepart").unwrap();
        assert!(
            record
                .validate("endpoint", "key", &source, &record.target)
                .is_err()
        );
    });
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
        let answers = Answers::new(vec![UploadChoice::Overwrite]);
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
        let remote = Remote::new(false);
        remote.file("/dest/file", b"original");
        remote.fault.set(Some(Fault::RejectRename(2)));
        let answers = Answers::new(vec![UploadChoice::Overwrite, UploadChoice::Skip]);
        let mut upload = batch(
            &source,
            Journal::new(temp.path().join("journal")),
            &answers.control,
        )
        .await;
        run(&mut upload, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/file"), b"original");
        assert_eq!(upload.progress.failed(), 1);
        assert_eq!(upload.progress.succeeded(), 0);
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
        let remote = Remote::new(true);
        remote.file("/dest/file", b"original");
        remote.fault.set(Some(Fault::ChangeTargetOnClose));
        let answers = Answers::new(vec![UploadChoice::Overwrite, UploadChoice::Skip]);
        let mut upload = batch(
            &source,
            Journal::new(temp.path().join("journal")),
            &answers.control,
        )
        .await;
        run(&mut upload, &remote, &answers.control).await.unwrap();
        assert_eq!(remote.bytes("/dest/file"), b"external mutation");
        assert_eq!(upload.progress.skipped(), 1);
        assert_eq!(
            *answers.questions.lock().unwrap(),
            vec![UploadQuestionKind::Conflict, UploadQuestionKind::Conflict]
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
        let remote = Remote::new(true);
        remote.file("/dest/file", b"old");
        remote.fault.set(Some(Fault::RemoveTargetOnClose));
        let answers = Answers::new(vec![UploadChoice::Overwrite, UploadChoice::Skip]);
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
            vec![UploadQuestionKind::Conflict, UploadQuestionKind::Conflict]
        );
        assert_eq!(upload.progress.skipped(), 1);
    });
}
