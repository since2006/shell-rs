use super::{
    EntryKind, ReadFailure, RemotePath, SaveFailure, SftpCommand, SftpEvent, SftpTransport,
    SftpTransportProvider, TransferDetail, TransferDirection, TransferPhase, TransferProgress,
    client::RemoteFs as _,
    client::{SftpClient, is_network_error},
    control::{Cancelled, TransferControl},
    download::DownloadBatch,
    edit,
    journal::{DownloadJournal, Journal},
    meter::TransferMeter,
    model::{scp_local_target, scp_remote_target},
    operations, sync,
    upload::UploadBatch,
};
use crate::{
    host::HostLogin,
    ssh::{LATENCY_INTERVAL, SshConnectionConfig, SshConnector, SshPrompts, describe_login_error},
};
use anyhow::{Result, anyhow};
use async_channel::{Receiver, Sender};
use std::{
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{RwLock, watch};

/// The answer to a request that needs the connection while there is none.
const NOT_CONNECTED: &str = "SFTP 未连接，请重新连接";

/// How often the latency task looks for a connection to measure, and for
/// the one it is measuring to be dropped.
const LATENCY_POLL: Duration = Duration::from_millis(250);

pub struct SshSftpTransportProvider {
    connector: SshConnector,
    resume_dir: PathBuf,
    download_resume_dir: PathBuf,
}
impl SshSftpTransportProvider {
    /// Upload and download resume records live in separate directories.
    pub fn new(connector: SshConnector, resume_dir: PathBuf, download_resume_dir: PathBuf) -> Self {
        Self {
            connector,
            resume_dir,
            download_resume_dir,
        }
    }
}
impl SftpTransportProvider for SshSftpTransportProvider {
    fn create(&self, login: &HostLogin) -> Box<dyn SftpTransport> {
        Box::new(SshSftpTransport {
            connector: self.connector.clone(),
            config: SshConnectionConfig::from(login),
            journal: Journal::new(self.resume_dir.clone()),
            download_journal: DownloadJournal::new(self.download_resume_dir.clone()),
        })
    }
}
struct SshSftpTransport {
    connector: SshConnector,
    config: SshConnectionConfig,
    journal: Journal,
    download_journal: DownloadJournal,
}

/// The host's one transfer batch, whichever way it goes.
enum TransferBatch {
    Upload(UploadBatch),
    Download(DownloadBatch),
}
impl TransferBatch {
    fn verify_host(&self, fingerprint: &str) -> Result<()> {
        match self {
            Self::Upload(batch) => batch.verify_host(fingerprint),
            Self::Download(batch) => batch.verify_host(fingerprint),
        }
    }
    fn is_complete(&self) -> bool {
        match self {
            Self::Upload(batch) => batch.is_complete(),
            Self::Download(batch) => batch.is_complete(),
        }
    }
    fn meter(&mut self) -> &mut TransferMeter {
        match self {
            Self::Upload(batch) => &mut batch.meter,
            Self::Download(batch) => &mut batch.meter,
        }
    }
    fn phase(&mut self, phase: TransferPhase, control: &TransferControl) {
        self.meter().phase(phase, control);
    }
    /// Say what a reconnecting batch is waiting for; the next phase
    /// other than reconnecting clears it.
    fn set_note(&mut self, note: String) {
        self.meter().progress.note = Some(note);
    }
    async fn run(&mut self, client: &SftpClient, control: &TransferControl) -> Result<()> {
        match self {
            Self::Upload(batch) => batch.run(client, control).await,
            Self::Download(batch) => batch.run(client, control).await,
        }
    }
    async fn discard(&self, client: &SftpClient) -> Result<()> {
        match self {
            Self::Upload(batch) => batch.discard(client).await,
            Self::Download(batch) => batch.discard().await,
        }
    }
}
impl SftpTransport for SshSftpTransport {
    fn run(
        self: Box<Self>,
        commands: Receiver<SftpCommand>,
        events: Sender<SftpEvent>,
    ) -> Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(self.run_async(commands, events))
    }
}
impl SshSftpTransport {
    async fn connect(
        &self,
        prompts: Arc<SshPrompts>,
        events: &Sender<SftpEvent>,
        shutdown: &watch::Receiver<bool>,
        shared: &RwLock<Option<Arc<SftpClient>>>,
    ) -> Result<Arc<SftpClient>> {
        events.send(SftpEvent::Connecting).await?;
        let mut stop = shutdown.clone();
        let client = tokio::select! {
            result = SftpClient::connect(&self.connector, &self.config, prompts) => Arc::new(result?),
            _ = stop.changed() => return Err(Cancelled.into()),
        };
        let home = client.canonicalize(&RemotePath::new(".")?).await?;
        *shared.write().await = Some(client.clone());
        events.send(SftpEvent::Connected { home }).await?;
        Ok(client)
    }
    async fn run_async(
        &self,
        commands: Receiver<SftpCommand>,
        events: Sender<SftpEvent>,
    ) -> Result<()> {
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (cancel_tx, cancel) = watch::channel(false);
        let (answers_tx, answers) = async_channel::unbounded();
        let (operations_tx, operations) = async_channel::bounded(1);
        let prompt_events = events.clone();
        let prompts = Arc::new(SshPrompts::new(
            Arc::new(move |p| prompt_events.try_send(SftpEvent::Prompt(p)).is_ok()),
            shutdown.clone(),
        ));
        let client: Arc<RwLock<Option<Arc<SftpClient>>>> = Arc::new(RwLock::new(None));
        let busy = Arc::new(AtomicBool::new(false));
        let router = {
            let prompts = prompts.clone();
            let events = events.clone();
            let client = client.clone();
            let busy = busy.clone();
            let cancel_tx = cancel_tx.clone();
            tokio::spawn(async move {
                while let Ok(command) = commands.recv().await {
                    match command {
                        SftpCommand::PromptReply { request_id, reply } => {
                            prompts.respond(request_id, reply)
                        }
                        SftpCommand::Answer { request_id, answer } => {
                            let _ = answers_tx.send((request_id, answer)).await;
                        }
                        SftpCommand::Cancel => {
                            cancel_tx.send_replace(true);
                            prompts.cancel_all();
                        }
                        SftpCommand::Disconnect => {
                            cancel_tx.send_replace(true);
                            prompts.cancel_all();
                            *client.write().await = None;
                            let _ = events
                                .send(SftpEvent::Disconnected("SFTP 已断开".into()))
                                .await;
                        }
                        SftpCommand::Shutdown => break,
                        SftpCommand::List { request_id, path } => {
                            let shared = client.clone();
                            let connected = client.read().await.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                let result = match connected {
                                    Some(connected) => match connected.list(&path).await {
                                        Ok(listing) => Ok(listing),
                                        Err(error) => {
                                            drop_broken(&shared, &connected, &events, &error).await;
                                            Err(format!("无法读取目录：{error:#}"))
                                        }
                                    },
                                    None => Err(NOT_CONNECTED.into()),
                                };
                                let _ = events.send(SftpEvent::Listed { request_id, result }).await;
                            });
                        }
                        SftpCommand::Operate {
                            request_id,
                            operation,
                        } => {
                            let shared = client.clone();
                            let connected = client.read().await.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                let result = match connected {
                                    Some(connected) => {
                                        match operations::run(connected.as_ref(), &operation).await
                                        {
                                            Ok(()) => Ok(()),
                                            Err(error) => {
                                                drop_broken(&shared, &connected, &events, &error)
                                                    .await;
                                                Err(format!("{error:#}"))
                                            }
                                        }
                                    }
                                    None => Err(NOT_CONNECTED.into()),
                                };
                                let _ = events
                                    .send(SftpEvent::Operated { request_id, result })
                                    .await;
                            });
                        }
                        SftpCommand::ReadFile { request_id, path } => {
                            let shared = client.clone();
                            let connected = client.read().await.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                let result = match connected {
                                    Some(connected) => {
                                        match edit::read_text(connected.as_ref(), &path).await {
                                            Ok(file) => Ok(file),
                                            Err(error) => {
                                                drop_broken(&shared, &connected, &events, &error)
                                                    .await;
                                                Err(ReadFailure::from_error(error))
                                            }
                                        }
                                    }
                                    None => Err(ReadFailure::Failed(NOT_CONNECTED.into())),
                                };
                                let _ = events
                                    .send(SftpEvent::FileRead { request_id, result })
                                    .await;
                            });
                        }
                        SftpCommand::ReadBytes {
                            request_id,
                            path,
                            limit,
                        } => {
                            let shared = client.clone();
                            let connected = client.read().await.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                let result = match connected {
                                    Some(connected) => {
                                        match edit::read_whole(connected.as_ref(), &path, limit)
                                            .await
                                        {
                                            Ok(bytes) => Ok(bytes),
                                            Err(error) => {
                                                drop_broken(&shared, &connected, &events, &error)
                                                    .await;
                                                Err(ReadFailure::from_error(error))
                                            }
                                        }
                                    }
                                    None => Err(ReadFailure::Failed(NOT_CONNECTED.into())),
                                };
                                let _ = events
                                    .send(SftpEvent::BytesRead { request_id, result })
                                    .await;
                            });
                        }
                        SftpCommand::WriteFile {
                            request_id,
                            path,
                            bytes,
                            expected,
                        } => {
                            let shared = client.clone();
                            let connected = client.read().await.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                let result = match connected {
                                    Some(connected) => match edit::write_in_place(
                                        connected.as_ref(),
                                        &path,
                                        bytes,
                                        expected,
                                    )
                                    .await
                                    {
                                        Ok(stamp) => Ok(stamp),
                                        Err(error) => {
                                            drop_broken(&shared, &connected, &events, &error).await;
                                            Err(SaveFailure::from_error(error))
                                        }
                                    },
                                    None => Err(SaveFailure::Failed(NOT_CONNECTED.into())),
                                };
                                let _ = events
                                    .send(SftpEvent::FileWritten { request_id, result })
                                    .await;
                            });
                        }
                        command => {
                            if busy.swap(true, Ordering::AcqRel) {
                                let _ = events
                                    .send(SftpEvent::Notice("已有传输批次正在处理".into()))
                                    .await;
                            } else {
                                cancel_tx.send_replace(false);
                                if operations_tx.send(command).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
                shutdown_tx.send_replace(true);
                cancel_tx.send_replace(true);
                prompts.cancel_all();
            })
        };
        let latency = tokio::spawn(measure_latency(client.clone(), events.clone()));
        let control = TransferControl {
            events: events.clone(),
            cancel,
            answers,
        };
        let mut batch: Option<TransferBatch> = None;
        match control
            .run(self.connect(prompts.clone(), &events, &shutdown, &client))
            .await
        {
            Ok(connected) => *client.write().await = Some(connected),
            Err(error) => {
                let _ = events
                    .send(SftpEvent::Disconnected(describe_login_error(&error)))
                    .await;
            }
        }
        let mut stop = shutdown.clone();
        while !*stop.borrow() {
            // Weakly: a client dropped on 断开 must still close its connection.
            let watched = client
                .read()
                .await
                .as_ref()
                .map(|connected| (Arc::downgrade(connected), connected.closed()));
            let (watched, closed) = watched.unzip();
            let operation = tokio::select! {
                operation = operations.recv() => match operation {
                    Ok(operation) => operation,
                    Err(_) => break,
                },
                _ = stop.changed() => break,
                // An idle connection that drops says so at once, as a
                // terminal on the same host does, not at the next request.
                _ = async {
                    match closed {
                        Some(closed) => closed.await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(connected) = watched.and_then(|weak| weak.upgrade()) {
                        forget(&client, &connected, &events).await;
                    }
                    continue;
                }
            };
            if client.read().await.is_none() {
                match control
                    .run(self.connect(prompts.clone(), &events, &shutdown, &client))
                    .await
                {
                    Ok(connected) => *client.write().await = Some(connected),
                    Err(error) => {
                        events
                            .send(SftpEvent::Disconnected(describe_login_error(&error)))
                            .await?;
                        events.send(SftpEvent::Idle).await?;
                        busy.store(false, Ordering::Release);
                        continue;
                    }
                }
            }
            let connected = client.read().await.clone().expect("connected above");
            let direction = match &operation {
                SftpCommand::Download(_) => TransferDirection::Download,
                SftpCommand::Upload(_) => TransferDirection::Upload,
                _ => batch
                    .as_ref()
                    .map(|batch| match batch {
                        TransferBatch::Upload(_) => TransferDirection::Upload,
                        TransferBatch::Download(_) => TransferDirection::Download,
                    })
                    .unwrap_or_default(),
            };
            let prepared: Result<()> = async {
                if let SftpCommand::Upload(request) = &operation {
                    batch = None;
                    events
                        .send(SftpEvent::Progress(TransferProgress::default()))
                        .await?;
                    let mut pruned = sync::Pruned::default();
                    let request = if let Some(delete) = request.sync_deletes() {
                        // The local folder itself, through any link to it:
                        // what it holds is what is copied.
                        let source = &request.sources()[0];
                        let local = tokio::fs::canonicalize(source)
                            .await
                            .ok()
                            .filter(|path| path.is_dir())
                            .map(|path| dunce::simplified(&path).to_path_buf())
                            .ok_or_else(|| anyhow!("本地目录 {} 不存在", source.display()))?;
                        let target =
                            sync_target(connected.as_ref(), request.destination(), &control)
                                .await?;
                        if delete {
                            pruned =
                                sync::prune(connected.as_ref(), &local, &target, &control).await?;
                        }
                        // Copied as the folder it goes to, which is there.
                        request
                            .clone()
                            .with_source(local)
                            .resolved(target.parent(), Some(target.file_name().to_string()))
                    } else if request.is_scp() {
                        let destination = request.destination();
                        let is_directory = destination.is_root()
                            || control
                                .run(connected.stat(destination))
                                .await?
                                .is_some_and(|metadata| metadata.kind() == EntryKind::Directory);
                        let (directory, name) = scp_remote_target(destination, is_directory);
                        // Some servers resolve a path that is not there, so
                        // ask whether it is a directory too.
                        let missing = || anyhow!("远程目录 {directory} 不存在");
                        let resolved = control
                            .run(connected.canonicalize(&directory))
                            .await
                            .map_err(|_| missing())?;
                        if !control
                            .run(connected.stat(&resolved))
                            .await?
                            .is_some_and(|metadata| metadata.kind() == EntryKind::Directory)
                        {
                            return Err(missing());
                        }
                        request.clone().resolved(resolved, name)
                    } else {
                        let target = control
                            .run(connected.canonicalize(request.destination()))
                            .await?;
                        super::UploadRequest::new(request.sources().to_vec(), target)?
                    };
                    let mut upload = control
                        .run(UploadBatch::scan(
                            &request,
                            &self.config.endpoint(),
                            connected.fingerprint(),
                            self.journal.clone(),
                            &control,
                        ))
                        .await?;
                    upload.add_pruned(pruned.deleted, pruned.failures);
                    batch = Some(TransferBatch::Upload(upload));
                }
                if let SftpCommand::Download(request) = &operation {
                    batch = None;
                    let request = &if request.is_scp() {
                        let destination = request.destination();
                        let is_directory = tokio::fs::metadata(destination)
                            .await
                            .is_ok_and(|metadata| metadata.is_dir());
                        let (directory, name) = scp_local_target(destination, is_directory);
                        if !tokio::fs::metadata(&directory)
                            .await
                            .is_ok_and(|metadata| metadata.is_dir())
                        {
                            anyhow::bail!("本地目录 {} 不存在", directory.display());
                        }
                        request.clone().resolved(directory, name)
                    } else {
                        request.clone()
                    };
                    events
                        .send(SftpEvent::Progress(TransferProgress {
                            direction: TransferDirection::Download,
                            ..TransferProgress::default()
                        }))
                        .await?;
                    batch = Some(TransferBatch::Download(
                        DownloadBatch::scan(
                            request,
                            &self.config.endpoint(),
                            connected.fingerprint(),
                            self.download_journal.clone(),
                            connected.as_ref(),
                            &control,
                        )
                        .await?,
                    ));
                }
                if let Some(batch) = &batch {
                    batch.verify_host(connected.fingerprint())?;
                }
                if matches!(operation, SftpCommand::Discard) {
                    if let Some(current) = &batch {
                        current.discard(connected.as_ref()).await?;
                    }
                    batch = None;
                    events
                        .send(SftpEvent::Progress(TransferProgress {
                            direction,
                            phase: TransferPhase::Completed,
                            ..Default::default()
                        }))
                        .await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = prepared {
                events.send(SftpEvent::Notice(error.to_string())).await?;
                if let Some(batch) = &mut batch {
                    batch.phase(TransferPhase::Stopped, &control);
                } else {
                    events
                        .send(SftpEvent::Progress(TransferProgress {
                            direction,
                            phase: TransferPhase::Stopped,
                            details: vec![TransferDetail::failed("", error.to_string())],
                            ..Default::default()
                        }))
                        .await?;
                }
                // Done, as far as whoever sent the batch is concerned.
                events.send(SftpEvent::Idle).await?;
                busy.store(false, Ordering::Release);
                continue;
            }
            if !matches!(operation, SftpCommand::Discard)
                && let Some(batch) = &mut batch
            {
                let mut reconnects = 0;
                loop {
                    let connected = client.read().await.clone();
                    let result = match connected {
                        Some(connected) => batch.run(connected.as_ref(), &control).await,
                        None => {
                            if control.check().is_err() {
                                Err(Cancelled.into())
                            } else {
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::NotConnected,
                                    "SFTP 未连接",
                                )
                                .into())
                            }
                        }
                    };
                    match result {
                        Ok(()) => break,
                        Err(error) if is_network_error(&error) => {
                            *client.write().await = None;
                            events
                                .send(SftpEvent::Disconnected("连接中断，传输进度已保留".into()))
                                .await?;
                            let mut recovered = false;
                            while reconnects < 3 {
                                batch.set_note(format!(
                                    "将在 {} 秒后重连（{}/3）",
                                    [1, 3, 10][reconnects],
                                    reconnects + 1
                                ));
                                batch.phase(TransferPhase::Reconnecting, &control);
                                let delay = Duration::from_secs([1, 3, 10][reconnects]);
                                reconnects += 1;
                                if control
                                    .run(async {
                                        tokio::time::sleep(delay).await;
                                        Ok(())
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                match control
                                    .run(self.connect(prompts.clone(), &events, &shutdown, &client))
                                    .await
                                {
                                    Ok(next) => match batch.verify_host(next.fingerprint()) {
                                        Ok(()) => {
                                            *client.write().await = Some(next);
                                            recovered = true;
                                            break;
                                        }
                                        Err(error) => {
                                            events
                                                .send(SftpEvent::Notice(error.to_string()))
                                                .await?;
                                            break;
                                        }
                                    },
                                    Err(error) => {
                                        events
                                            .send(SftpEvent::Disconnected(error.to_string()))
                                            .await?;
                                        // Authentication/trust failures must require user action.
                                        if !is_network_error(&error) {
                                            break;
                                        }
                                    }
                                }
                            }
                            if recovered {
                                continue;
                            }
                            batch.phase(TransferPhase::Stopped, &control);
                            break;
                        }
                        Err(error) => {
                            if !error.is::<Cancelled>() {
                                events.send(SftpEvent::Notice(error.to_string())).await?;
                            }
                            batch.phase(TransferPhase::Stopped, &control);
                            break;
                        }
                    }
                }
                if batch.is_complete() {
                    batch.phase(TransferPhase::Completed, &control);
                }
            }
            busy.store(false, Ordering::Release);
            events.send(SftpEvent::Idle).await?;
        }
        router.abort();
        latency.abort();
        *client.write().await = None;
        Ok(())
    }
}

/// The folder a sync copies into: `destination` when it is a folder, links
/// to it resolved; made when it is not there, in a folder that is, as scp
/// would. Never the root: a sync with `--delete` would empty the server.
async fn sync_target(
    fs: &SftpClient,
    destination: &RemotePath,
    control: &TransferControl,
) -> Result<RemotePath> {
    let target = match control.run(fs.stat(destination)).await? {
        Some(metadata) if metadata.kind() == EntryKind::Directory => {
            control.run(fs.canonicalize(destination)).await?
        }
        Some(_) => anyhow::bail!("远程路径 {destination} 不是目录"),
        None => {
            let parent = destination.parent();
            let missing = || anyhow!("远程目录 {parent} 不存在");
            let parent = control
                .run(fs.canonicalize(&parent))
                .await
                .map_err(|_| missing())?;
            if !control
                .run(fs.stat(&parent))
                .await?
                .is_some_and(|metadata| metadata.kind() == EntryKind::Directory)
            {
                return Err(missing());
            }
            let target = parent.join(destination.file_name())?;
            if !target.is_root() {
                control.run(fs.mkdir(&target)).await?;
            }
            target
        }
    };
    if target.is_root() {
        anyhow::bail!("不能同步到根目录 /");
    }
    Ok(target)
}

/// Measure the connection in use every [`LATENCY_INTERVAL`], and a new one
/// at once, transfers or not. The client is held only for a ping, and let go
/// as soon as it is no longer the one in use, so 断开 still closes the
/// connection at once.
async fn measure_latency(shared: Arc<RwLock<Option<Arc<SftpClient>>>>, events: Sender<SftpEvent>) {
    let mut measured = Weak::new();
    let mut last = Instant::now();
    loop {
        tokio::time::sleep(LATENCY_POLL).await;
        let Some(connected) = shared.read().await.clone() else {
            continue;
        };
        let fresh = !Weak::ptr_eq(&measured, &Arc::downgrade(&connected));
        if !fresh && last.elapsed() < LATENCY_INTERVAL {
            continue;
        }
        measured = Arc::downgrade(&connected);
        last = Instant::now();
        let latency = tokio::select! {
            latency = connected.round_trip() => latency,
            () = replaced(&shared, &connected) => None,
        };
        drop(connected);
        if let Some(latency) = latency
            && events.send(SftpEvent::Latency(latency)).await.is_err()
        {
            break;
        }
    }
}

/// Resolves once `connected` is no longer the client in use.
async fn replaced(shared: &RwLock<Option<Arc<SftpClient>>>, connected: &Arc<SftpClient>) {
    loop {
        tokio::time::sleep(LATENCY_POLL).await;
        let current = shared.read().await;
        if !current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, connected))
        {
            return;
        }
    }
}

/// A request failed: if the connection broke, `forget` it.
async fn drop_broken(
    shared: &RwLock<Option<Arc<SftpClient>>>,
    connected: &Arc<SftpClient>,
    events: &Sender<SftpEvent>,
    error: &anyhow::Error,
) {
    if is_network_error(error) {
        forget(shared, connected, events).await;
    }
}

/// The connection is gone: forget that client (unless a newer one already
/// replaced it) and tell the panel once.
async fn forget(
    shared: &RwLock<Option<Arc<SftpClient>>>,
    connected: &Arc<SftpClient>,
    events: &Sender<SftpEvent>,
) {
    let mut current = shared.write().await;
    if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, connected)) {
        *current = None;
        let _ = events
            .send(SftpEvent::Disconnected("SFTP 连接中断，请重新连接".into()))
            .await;
    }
}
