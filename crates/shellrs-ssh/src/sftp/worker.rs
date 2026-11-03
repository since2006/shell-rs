//! One SFTP connection's worker: it connects, serves listings, operations
//! and the editor's files alongside one transfer batch at a time, and
//! reconnects a batch whose connection dropped.

use super::{
    Disconnect, EngineEvent, EntryKind, EventSink, Reason, RemotePath, SftpCommand, SftpError,
    TransferDetail, TransferDirection, TransferPhase, TransferProgress,
    client::RemoteFs as _,
    client::{SftpClient, is_network_error},
    control::{Cancelled, TransferControl},
    download::DownloadBatch,
    edit::{self, ReadFailure, SaveFailure},
    engine::SftpRoute,
    journal::{DownloadJournal, Journal},
    meter::TransferMeter,
    model::{scp_local_target, scp_remote_target},
    operations, sync,
    upload::UploadBatch,
};
use crate::{LATENCY_INTERVAL, SshConnectionConfig, SshConnector, SshPrompts};
use anyhow::{Result, anyhow};
use async_channel::Receiver;
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
fn not_connected() -> Reason {
    Reason::new(SftpError::NotConnected)
}

/// How often the latency task looks for a connection to measure, and for
/// the one it is measuring to be dropped.
const LATENCY_POLL: Duration = Duration::from_millis(250);

/// One worker lifetime, run on the caller's runtime; see
/// [`run`](Self::run).
pub struct SftpWorker {
    connector: SshConnector,
    route: SftpRoute,
    endpoint: String,
    journal: Journal,
    download_journal: DownloadJournal,
}

impl SftpWorker {
    /// Upload and download resume records live in separate directories.
    pub fn new(
        connector: SshConnector,
        route: SftpRoute,
        resume_dir: PathBuf,
        download_resume_dir: PathBuf,
    ) -> Self {
        Self {
            connector,
            endpoint: route.endpoint(),
            route,
            journal: Journal::new(resume_dir),
            download_journal: DownloadJournal::new(download_resume_dir),
        }
    }
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
    fn set_note(&mut self, note: SftpError) {
        self.meter().progress.note = Some(Reason::new(note));
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
impl SftpWorker {
    async fn connect(
        &self,
        prompts: Arc<SshPrompts>,
        events: &EventSink,
        shutdown: &watch::Receiver<bool>,
        shared: &RwLock<Option<Arc<SftpClient>>>,
    ) -> Result<Arc<SftpClient>> {
        events.send(EngineEvent::Connecting).await?;
        let mut stop = shutdown.clone();
        // A terminal's channel measures no latency: the terminal already
        // shows its connection's.
        let client = async {
            match &self.route {
                SftpRoute::Login(login) => {
                    SftpClient::connect(&self.connector, &SshConnectionConfig::from(login), prompts)
                        .await
                }
                SftpRoute::Channel { opener, .. } => SftpClient::on_channel(opener.as_ref()).await,
            }
        };
        let client = tokio::select! {
            result = client => Arc::new(result?),
            _ = stop.changed() => return Err(Cancelled.into()),
        };
        let home = client.canonicalize(&RemotePath::new(".")?).await?;
        *shared.write().await = Some(client.clone());
        events.send(EngineEvent::Connected { home }).await?;
        Ok(client)
    }

    /// Serve `commands` until `Shutdown`, or until they close, telling
    /// `events` what happens.
    pub async fn run(self, commands: Receiver<SftpCommand>, events: EventSink) -> Result<()> {
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (cancel_tx, cancel) = watch::channel(false);
        let (answers_tx, answers) = async_channel::unbounded();
        let (operations_tx, operations) = async_channel::bounded(1);
        let prompt_events = events.clone();
        let prompts = Arc::new(SshPrompts::new(
            Arc::new(move |p| prompt_events.try_send(EngineEvent::Prompt(p))),
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
                                .send(EngineEvent::Disconnected(Disconnect::ByUser))
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
                                            Err(Reason::from(error).under(SftpError::ListFailed))
                                        }
                                    },
                                    None => Err(not_connected()),
                                };
                                let _ = events
                                    .send(EngineEvent::Listed { request_id, result })
                                    .await;
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
                                                Err(error.into())
                                            }
                                        }
                                    }
                                    None => Err(not_connected()),
                                };
                                let _ = events
                                    .send(EngineEvent::Operated { request_id, result })
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
                                                Err(ReadFailure::from_error(error, |why| why))
                                            }
                                        }
                                    }
                                    None => Err(ReadFailure::Failed(not_connected())),
                                };
                                let _ = events
                                    .send(EngineEvent::FileRead { request_id, result })
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
                                                Err(ReadFailure::from_error(error, |why| why))
                                            }
                                        }
                                    }
                                    None => Err(ReadFailure::Failed(not_connected())),
                                };
                                let _ = events
                                    .send(EngineEvent::BytesRead { request_id, result })
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
                                            Err(SaveFailure::from_error(error, |why| why))
                                        }
                                    },
                                    None => Err(SaveFailure::Failed(not_connected())),
                                };
                                let _ = events
                                    .send(EngineEvent::FileWritten { request_id, result })
                                    .await;
                            });
                        }
                        command => {
                            if busy.swap(true, Ordering::AcqRel) {
                                let _ = events
                                    .send(EngineEvent::Notice(Reason::new(SftpError::Busy)))
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
                    .send(EngineEvent::Disconnected(Disconnect::LoginFailed(
                        error.into(),
                    )))
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
                            .send(EngineEvent::Disconnected(Disconnect::LoginFailed(
                                error.into(),
                            )))
                            .await?;
                        events.send(EngineEvent::Idle).await?;
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
                        .send(EngineEvent::Progress(TransferProgress::default()))
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
                            .ok_or_else(|| {
                                anyhow!(SftpError::LocalFolderMissing {
                                    path: source.clone()
                                })
                            })?;
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
                        let missing = || {
                            anyhow!(SftpError::RemoteFolderMissing {
                                path: directory.to_string()
                            })
                        };
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
                            &self.endpoint,
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
                            anyhow::bail!(SftpError::LocalFolderMissing { path: directory });
                        }
                        request.clone().resolved(directory, name)
                    } else {
                        request.clone()
                    };
                    events
                        .send(EngineEvent::Progress(TransferProgress {
                            direction: TransferDirection::Download,
                            ..TransferProgress::default()
                        }))
                        .await?;
                    batch = Some(TransferBatch::Download(
                        DownloadBatch::scan(
                            request,
                            &self.endpoint,
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
                        .send(EngineEvent::Progress(TransferProgress {
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
                let reason = Reason::from(error);
                events.send(EngineEvent::Notice(reason.clone())).await?;
                if let Some(batch) = &mut batch {
                    batch.phase(TransferPhase::Stopped, &control);
                } else {
                    events
                        .send(EngineEvent::Progress(TransferProgress {
                            direction,
                            phase: TransferPhase::Stopped,
                            details: vec![TransferDetail::failed("", reason)],
                            ..Default::default()
                        }))
                        .await?;
                }
                // Done, as far as whoever sent the batch is concerned.
                events.send(EngineEvent::Idle).await?;
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
                                    SftpError::NoConnection,
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
                                .send(EngineEvent::Disconnected(Disconnect::DroppedKept))
                                .await?;
                            let mut recovered = false;
                            while reconnects < 3 {
                                batch.set_note(SftpError::ReconnectIn {
                                    seconds: [1, 3, 10][reconnects],
                                    attempt: reconnects + 1,
                                });
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
                                            events.send(EngineEvent::Notice(error.into())).await?;
                                            break;
                                        }
                                    },
                                    Err(error) => {
                                        // Authentication/trust failures must require user action.
                                        let give_up = !is_network_error(&error);
                                        events
                                            .send(EngineEvent::Disconnected(Disconnect::Failed(
                                                error.into(),
                                            )))
                                            .await?;
                                        if give_up {
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
                                events.send(EngineEvent::Notice(error.into())).await?;
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
            events.send(EngineEvent::Idle).await?;
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
        Some(_) => anyhow::bail!(SftpError::NotAFolder {
            path: destination.to_string()
        }),
        None => {
            let parent = destination.parent();
            let missing = || {
                anyhow!(SftpError::RemoteFolderMissing {
                    path: parent.to_string()
                })
            };
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
        anyhow::bail!(SftpError::SyncToRoot);
    }
    Ok(target)
}

/// Measure the connection in use every [`LATENCY_INTERVAL`], and a new one
/// at once, transfers or not. The client is held only for a ping, and let go
/// as soon as it is no longer the one in use, so 断开 still closes the
/// connection at once.
async fn measure_latency(shared: Arc<RwLock<Option<Arc<SftpClient>>>>, events: EventSink) {
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
            && events.send(EngineEvent::Latency(latency)).await.is_err()
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
    events: &EventSink,
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
    events: &EventSink,
) {
    let mut current = shared.write().await;
    if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, connected)) {
        *current = None;
        let _ = events
            .send(EngineEvent::Disconnected(Disconnect::Dropped))
            .await;
    }
}
