//! Fake SFTP transports and local folders, and an SFTP tab opened on them.

use super::*;

/// A fake server's files for the editor: bytes and modification time.
pub type FakeFiles = Arc<Mutex<std::collections::BTreeMap<String, (Vec<u8>, u32)>>>;
/// The editor's writes: path, bytes and the stamp the file had to have.
pub type FakeWrites = Arc<Mutex<Vec<(String, Vec<u8>, Option<FileStamp>)>>>;

#[derive(Default)]
pub struct FakeSftpProvider {
    pub requests: Arc<Mutex<Vec<UploadRequest>>>,
    pub downloads: Arc<Mutex<Vec<shellrs::sftp::DownloadRequest>>>,
    pub operations: Arc<Mutex<Vec<shellrs::sftp::RemoteOperation>>>,
    pub events: Arc<Mutex<Vec<async_channel::Sender<SftpEvent>>>>,
    /// When set, the next connection waits for a message before it is up.
    pub hold_connection: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
    /// Connections made again on 重新连接 (a bare 继续).
    pub reconnects: Arc<Mutex<usize>>,
    /// The remote home is `/slow`, which never answers: a network too slow
    /// for the first directory to arrive.
    pub slow_home: bool,
    /// The remote files the editor reads and writes: bytes and modification
    /// time. Reads of a path under `/slow` never answer.
    pub files: FakeFiles,
    /// The editor's reads, by path.
    pub reads: Arc<Mutex<Vec<String>>>,
    /// The editor's writes: path, bytes and the stamp the file had to have.
    pub writes: FakeWrites,
}

impl FakeSftpProvider {
    /// A server with these files on it.
    pub fn with_files(files: &[(&str, &[u8])]) -> Self {
        let provider = Self::default();
        provider.files.lock().unwrap().extend(
            files
                .iter()
                .map(|(path, bytes)| (path.to_string(), (bytes.to_vec(), 100))),
        );
        provider
    }
    /// Someone else writes a file on the server.
    pub fn change_file(&self, path: &str, bytes: &[u8]) {
        let mut files = self.files.lock().unwrap();
        let modified = files.get(path).map_or(100, |(_, modified)| modified + 7);
        files.insert(path.into(), (bytes.to_vec(), modified));
    }
    pub fn file(&self, path: &str) -> Vec<u8> {
        self.files.lock().unwrap()[path].0.clone()
    }
}

/// What a fake remote file's stamp is.
pub fn fake_stamp(bytes: &[u8], modified: u32) -> FileStamp {
    FileStamp::new(
        bytes.len() as u64,
        Some(std::time::UNIX_EPOCH + Duration::from_secs(u64::from(modified))),
    )
}

impl SftpTransportProvider for FakeSftpProvider {
    fn create(&self, _: &HostLogin) -> Box<dyn SftpTransport> {
        Box::new(FakeSftpTransport {
            requests: self.requests.clone(),
            downloads: self.downloads.clone(),
            operations: self.operations.clone(),
            events: self.events.clone(),
            hold: self.hold_connection.lock().unwrap().take(),
            reconnects: self.reconnects.clone(),
            files: self.files.clone(),
            reads: self.reads.clone(),
            writes: self.writes.clone(),
            home: if self.slow_home {
                "/slow"
            } else {
                "/home/tester"
            },
        })
    }
}

pub struct FakeSftpTransport {
    pub requests: Arc<Mutex<Vec<UploadRequest>>>,
    pub downloads: Arc<Mutex<Vec<shellrs::sftp::DownloadRequest>>>,
    pub operations: Arc<Mutex<Vec<shellrs::sftp::RemoteOperation>>>,
    pub events: Arc<Mutex<Vec<async_channel::Sender<SftpEvent>>>>,
    pub hold: Option<mpsc::Receiver<()>>,
    pub reconnects: Arc<Mutex<usize>>,
    pub files: FakeFiles,
    pub reads: Arc<Mutex<Vec<String>>>,
    pub writes: FakeWrites,
    pub home: &'static str,
}

impl SftpTransport for FakeSftpTransport {
    fn run(
        self: Box<Self>,
        commands: async_channel::Receiver<SftpCommand>,
        events: async_channel::Sender<SftpEvent>,
    ) -> anyhow::Result<()> {
        use shellrs::sftp::{TransferChoice, TransferPhase, TransferProgress};
        self.events.lock().unwrap().push(events.clone());
        if let Some(hold) = &self.hold {
            let _ = hold.recv();
        }
        events.send_blocking(SftpEvent::Connected {
            home: RemotePath::new(self.home)?,
        })?;
        // As in the real engine, 继续 goes on with a stopped batch, and with
        // none it only reconnects.
        let mut stopped = false;
        while let Ok(command) = commands.recv_blocking() {
            match command {
                SftpCommand::List { request_id, path } => {
                    if path.as_str() == "/slow" {
                        continue;
                    }
                    let result = match path.as_str() {
                        "/denied" => Err("权限不足".into()),
                        "/empty" => Ok(DirectoryListing::new("/empty", vec![])),
                        "/dotfiles" => Ok(dotfiles_listing()),
                        // The files put there, as `with_files` gave them.
                        "/pictures" => Ok(files_listing(&self.files, "/pictures")),
                        _ => Ok(fake_listing(path.as_str())),
                    };
                    events.send_blocking(SftpEvent::Listed { request_id, result })?;
                }
                SftpCommand::Operate {
                    request_id,
                    operation,
                } => {
                    let refused = format!("{operation:?}").contains("denied");
                    if let shellrs::sftp::RemoteOperation::CreateFile { path } = &operation {
                        self.files
                            .lock()
                            .unwrap()
                            .insert(path.to_string(), (Vec::new(), 100));
                    }
                    self.operations.lock().unwrap().push(operation);
                    events.send_blocking(SftpEvent::Operated {
                        request_id,
                        result: if refused {
                            Err("权限不足".into())
                        } else {
                            Ok(())
                        },
                    })?;
                }
                SftpCommand::ReadFile { request_id, path } => {
                    self.reads.lock().unwrap().push(path.to_string());
                    if path.as_str().starts_with("/slow") {
                        continue;
                    }
                    let result = match self.files.lock().unwrap().get(path.as_str()) {
                        Some((bytes, _)) if bytes.len() as u64 > EDIT_LIMIT => {
                            Err(ReadFailure::TooLarge(bytes.len() as u64))
                        }
                        Some((bytes, modified)) => {
                            TextFile::decode(bytes.clone(), fake_stamp(bytes, *modified))
                        }
                        None => Err(ReadFailure::Failed("文件不存在".into())),
                    };
                    events.send_blocking(SftpEvent::FileRead { request_id, result })?;
                }
                SftpCommand::ReadBytes {
                    request_id,
                    path,
                    limit,
                } => {
                    self.reads.lock().unwrap().push(path.to_string());
                    if path.as_str().starts_with("/slow") {
                        continue;
                    }
                    let result = match self.files.lock().unwrap().get(path.as_str()) {
                        Some((bytes, _)) if bytes.len() as u64 > limit => {
                            Err(ReadFailure::TooLarge(bytes.len() as u64))
                        }
                        Some((bytes, _)) => Ok(FileBytes::new(bytes.clone())),
                        None => Err(ReadFailure::Failed("文件不存在".into())),
                    };
                    events.send_blocking(SftpEvent::BytesRead { request_id, result })?;
                }
                SftpCommand::WriteFile {
                    request_id,
                    path,
                    bytes,
                    expected,
                } => {
                    self.writes
                        .lock()
                        .unwrap()
                        .push((path.to_string(), bytes.clone(), expected));
                    let mut files = self.files.lock().unwrap();
                    let now = files.get(path.as_str()).map(|(b, m)| fake_stamp(b, *m));
                    let result = if expected.is_some() && expected != now {
                        Err(SaveFailure::Changed)
                    } else {
                        // Every save is a second later.
                        let modified = files.get(path.as_str()).map_or(100, |(_, m)| m + 1);
                        let stamp = fake_stamp(&bytes, modified);
                        files.insert(path.to_string(), (bytes, modified));
                        Ok(stamp)
                    };
                    drop(files);
                    events.send_blocking(SftpEvent::FileWritten { request_id, result })?;
                }
                SftpCommand::Download(request) => {
                    self.downloads.lock().unwrap().push(request);
                    events.send_blocking(SftpEvent::Progress(
                        TransferProgress::new(TransferPhase::Transferring)
                            .with_direction(shellrs::sftp::TransferDirection::Download),
                    ))?;
                }
                SftpCommand::Upload(request) => {
                    self.requests.lock().unwrap().push(request);
                    events.send_blocking(SftpEvent::Progress(Default::default()))?;
                }
                // The real engine says Idle once a batch stops or ends,
                // which is when the queue may move on.
                SftpCommand::Cancel => {
                    stopped = true;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        TransferPhase::Stopped,
                    )))?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Discard => {
                    stopped = false;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        TransferPhase::Completed,
                    )))?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Resume if stopped => {
                    stopped = false;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        TransferPhase::Transferring,
                    )))?;
                }
                SftpCommand::Resume => {
                    *self.reconnects.lock().unwrap() += 1;
                    events.send_blocking(SftpEvent::Connected {
                        home: RemotePath::new("/home/tester")?,
                    })?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Answer { answer, .. } => {
                    stopped = answer.choice() == TransferChoice::Cancel;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        if answer.choice() == TransferChoice::Cancel {
                            TransferPhase::Stopped
                        } else {
                            TransferPhase::Completed
                        },
                    )))?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Shutdown => break,
                _ => {}
            }
        }
        Ok(())
    }
}

/// The local file system of the tests: every directory lists
/// `fake_listing`, and changes are recorded, never made.
#[derive(Clone, Default)]
pub struct FakeLocalDirectory {
    pub calls: Arc<Mutex<Vec<String>>>,
    /// The files the editor reads and writes, in memory.
    pub files: Arc<Mutex<std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>>>,
}

impl FakeLocalDirectory {
    pub fn record(&self, call: String) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl LocalDirectoryProvider for FakeLocalDirectory {
    fn home(&self) -> std::path::PathBuf {
        "/local/tester".into()
    }
    fn list(&self, path: &std::path::Path) -> anyhow::Result<DirectoryListing> {
        Ok(fake_listing(path.to_str().unwrap()))
    }
    fn trash(&self, paths: &[std::path::PathBuf]) -> anyhow::Result<()> {
        self.record(format!("trash {paths:?}"))
    }
    fn rename(&self, from: &std::path::Path, to: &std::path::Path) -> anyhow::Result<()> {
        self.record(format!("rename {} -> {}", from.display(), to.display()))
    }
    fn create_dir(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.record(format!("mkdir {}", path.display()))
    }
    fn create_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.files.lock().unwrap().insert(path.into(), Vec::new());
        self.record(format!("touch {}", path.display()))
    }
    fn read_file(
        &self,
        path: &std::path::Path,
        limit: u64,
    ) -> anyhow::Result<(Vec<u8>, FileStamp)> {
        let bytes = self
            .files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("「{}」不存在", path.display()))?;
        if bytes.len() as u64 > limit {
            return Err(ReadFailure::TooLarge(bytes.len() as u64).into());
        }
        let stamp = FileStamp::new(bytes.len() as u64, None);
        Ok((bytes, stamp))
    }
    fn write_file(
        &self,
        path: &std::path::Path,
        bytes: &[u8],
        expected: Option<FileStamp>,
    ) -> anyhow::Result<FileStamp> {
        let mut files = self.files.lock().unwrap();
        let now = files
            .get(path)
            .map(|bytes| FileStamp::new(bytes.len() as u64, None));
        if expected.is_some() && expected != now {
            return Err(SaveFailure::Changed.into());
        }
        files.insert(path.into(), bytes.to_vec());
        drop(files);
        self.record(format!("write {}", path.display()))?;
        Ok(FileStamp::new(bytes.len() as u64, None))
    }
    fn set_permissions(
        &self,
        paths: &[std::path::PathBuf],
        edit: shellrs::sftp::PermissionEdit,
        recursive: bool,
        add_x_to_dirs: bool,
    ) -> anyhow::Result<()> {
        self.record(format!(
            "chmod {paths:?} +{:o} -{:o} recursive={recursive} x={add_x_to_dirs}",
            edit.set(),
            edit.clear()
        ))
    }
}

/// A directory of the fake server's files, as they are now.
pub fn files_listing(files: &FakeFiles, path: &str) -> DirectoryListing {
    let prefix = format!("{path}/");
    let entries = files
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(file, (bytes, modified))| {
            let name = file.strip_prefix(&prefix)?;
            Some(DirectoryEntry::new(
                name,
                FileMetadata::new(
                    EntryKind::File,
                    bytes.len() as u64,
                    Some(*modified),
                    Some(0o644),
                ),
            ))
        })
        .collect();
    DirectoryListing::new(path, entries)
}

/// A home of mostly hidden files: `.bashrc`, `.ssh` and one `notes.txt`.
fn dotfiles_listing() -> DirectoryListing {
    DirectoryListing::new(
        "/dotfiles",
        vec![
            DirectoryEntry::new(
                ".bashrc",
                FileMetadata::new(EntryKind::File, 10, Some(100), Some(0o644)),
            ),
            DirectoryEntry::new(
                ".ssh",
                FileMetadata::new(EntryKind::Directory, 0, None, Some(0o700)),
            ),
            DirectoryEntry::new(
                "notes.txt",
                FileMetadata::new(EntryKind::File, 20, Some(200), Some(0o644)),
            ),
        ],
    )
}

pub fn fake_listing(path: &str) -> DirectoryListing {
    DirectoryListing::new(
        path,
        vec![
            DirectoryEntry::new(
                "文件 甲.txt",
                FileMetadata::new(EntryKind::File, 12, Some(100), Some(0o644)),
            ),
            DirectoryEntry::new(
                "文件 乙.txt",
                FileMetadata::new(EntryKind::File, 34, Some(200), Some(0o644)),
            ),
            DirectoryEntry::new(
                "目录",
                FileMetadata::new(EntryKind::Directory, 0, None, Some(0o755)),
            ),
            DirectoryEntry::new(
                "链接目录",
                FileMetadata::new(EntryKind::Symlink, 7, Some(300), Some(0o120_777)),
            )
            .with_target_kind(Some(EntryKind::Directory)),
        ]
        .into_iter()
        .map(|entry| entry.with_owner(Some("root".into()), Some("wheel".into())))
        .collect(),
    )
}

pub fn open_workspace_with_sftp(
    cx: &mut TestAppContext,
    provider: Arc<FakeSftpProvider>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_services(cx, provider, FakeLocalDirectory::default())
}

pub fn open_workspace_with_services(
    cx: &mut TestAppContext,
    provider: Arc<FakeSftpProvider>,
    local: FakeLocalDirectory,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    init_app(cx);
    // Dialogs slide in over real time; small targets such as checkboxes
    // would move between the frame that locates them and the click.
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| HostStore::seed());
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| settings_store()),
                remote,
                Arc::new(FakeTerminalFactory::default()),
                provider,
                Arc::new(local),
                Arc::new(FakeConnectionTester::default()),
                Arc::new(FakeForwardProvider::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.unwrap())
}

pub async fn open_test_explorer(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(OpenExplorer(HostId(DB_01))), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("remote-path")
            .is_some_and(|p| p.value() == Some("/home/tester"))
            && window
                .within(("local-pane", SFTP_TAB))
                .try_find("file:文件 甲.txt")
                .is_some()
    })
    .await;
}

/// The selected names of one explorer pane, in display order.
pub fn pane_selection(workspace: &Entity<Workspace>, remote: bool, cx: &App) -> Vec<String> {
    workspace
        .read(cx)
        .explorer(ExplorerId(SFTP_TAB))
        .unwrap()
        .read(cx)
        .pane(remote)
        .read(cx)
        .selected_names(cx)
}

/// Click a row in one pane, then press a key with the list focused.
pub fn press_on_row(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    pane: &'static str,
    row: &str,
    key: &str,
) {
    let row = ElementId::Name(format!("name:{row}").into());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within((pane, SFTP_TAB)).click(row, cx);
        window.press(key, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
