//! An SFTP session on a channel of a connection: browsing, changing and
//! transferring files.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use shellrs_ssh::sftp::{
    self as engine, EngineEvent, EventSink, Reason, RemoteOperation, RemotePath, SftpCommand,
    SftpWorker,
};
use shellrs_ssh::{self as ssh, SftpChannel};
use tokio::sync::{oneshot, watch};

use crate::connection::{Inner, State};
use crate::events::Events;
use crate::failure::{Failure, SshError};

/// How long opening the SFTP channel may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// What an SFTP session says, besides answering calls.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum SftpEvent {
    /// Up again after it dropped; `home` is the user's home folder.
    Connected {
        home: String,
    },
    Disconnected {
        reason: SftpDisconnect,
    },
    /// How the transfer goes: often while it runs, and once at its end.
    Progress {
        progress: TransferProgress,
    },
    /// A transfer waits for the user: answer with
    /// [`SftpSession::answer`] and the question's `id`.
    Question {
        question: TransferQuestion,
    },
    /// A transfer could not start or stopped, outside any one file.
    Notice {
        failure: Failure,
    },
    /// The transfer is over, done or not; another may start.
    Idle,
}

/// Why an SFTP session has no channel now.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SftpDisconnect {
    ByUser,
    /// It dropped while idle; the next call fails until the app opens a
    /// new session.
    Dropped,
    /// It dropped under a transfer, which keeps its progress and
    /// reconnects on its own.
    DroppedKept,
    /// Opening the channel again failed.
    LoginFailed {
        failure: Failure,
    },
    /// A transfer's reconnecting failed.
    Failed {
        failure: Failure,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryListing {
    /// The folder, as the server names it.
    pub path: String,
    pub entries: Vec<DirectoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryEntry {
    pub name: String,
    /// What it is itself: a link is a link.
    pub kind: EntryKind,
    pub size: u64,
    /// Seconds since 1970, when the server says.
    pub modified: Option<u32>,
    /// The Unix mode bits, when the server says.
    pub permissions: Option<u32>,
    pub owner: Option<String>,
    pub group: Option<String>,
    /// What a link points at, when it could be seen.
    pub target_kind: Option<EntryKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferDirection {
    Upload,
    Download,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferPhase {
    Scanning,
    Transferring,
    /// Waiting for an answer to a question.
    Waiting,
    Reconnecting,
    /// Cancelled or failed, with what is done kept: resume or discard it.
    Stopped,
    Completed,
}

/// A transfer batch as it stands.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct TransferProgress {
    pub direction: TransferDirection,
    pub phase: TransferPhase,
    /// The file moving now, where it goes.
    pub current: String,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    /// The batch's files, and how they ended so far.
    pub total: u64,
    pub succeeded: u64,
    pub skipped: u64,
    pub failed: u64,
    /// Remote files a sync deleted.
    pub deleted: u64,
    pub bytes_per_second: u64,
    /// From 0 to 1, for a progress bar.
    pub fraction: f32,
    /// How long the rest should take at the current speed.
    pub remaining_seconds: Option<u64>,
    pub elapsed_seconds: u64,
    /// What a reconnecting batch waits for.
    pub note: Option<Failure>,
    /// The files that ended, and how.
    pub details: Vec<TransferDetail>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TransferDetail {
    pub path: String,
    pub outcome: TransferOutcome,
    pub reason: Option<Failure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferOutcome {
    Done,
    Skipped,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TransferQuestion {
    pub id: u64,
    pub kind: TransferQuestionKind,
    pub path: String,
    /// What is asked: the conflict, or the failure to retry or skip.
    pub message: Failure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferQuestionKind {
    /// The target exists: overwrite or skip?
    Conflict,
    /// A partial copy is there: resume or restart?
    Resume,
    /// A partial copy is there that cannot be resumed: restart or skip?
    InvalidResume,
    /// A file failed: retry, skip or cancel?
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TransferAnswer {
    pub choice: TransferChoice,
    /// Answer the same to the batch's other questions of this kind.
    pub apply_to_all: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TransferChoice {
    Overwrite,
    Skip,
    Cancel,
    Retry,
    Resume,
    Restart,
}

/// An SFTP session on its connection. Listings and changes answer their
/// call; one transfer batch runs at a time and says how it goes through
/// [`next_event`](Self::next_event). It closes on [`end`](Self::end),
/// when its connection does, or once nothing holds it, a call waiting on
/// it included.
#[derive(uniffi::Object)]
pub struct SftpSession {
    /// Holds the connection open as long as the session is.
    _connection: Arc<Inner>,
    commands: async_channel::Sender<SftpCommand>,
    replies: Arc<Replies>,
    next_request: AtomicU64,
    events: Arc<Events<SftpEvent>>,
    home: String,
}

/// The calls waiting for the engine's answer, by request.
#[derive(Default)]
struct Replies(Mutex<HashMap<u64, oneshot::Sender<Reply>>>);

enum Reply {
    Listed(Result<engine::DirectoryListing, Reason>),
    Operated(Result<(), Reason>),
}

impl Replies {
    fn lock(&self) -> MutexGuard<'_, HashMap<u64, oneshot::Sender<Reply>>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn answer(&self, request_id: u64, reply: Reply) {
        if let Some(waiting) = self.lock().remove(&request_id) {
            let _ = waiting.send(reply);
        }
    }
}

impl SftpSession {
    /// Start the session's worker on a thread of its own, as the desktop
    /// does, and wait for it to be up.
    pub(crate) async fn start(connection: Arc<Inner>) -> Result<Arc<Self>, SshError> {
        let route = engine::SftpRoute::Channel {
            endpoint: connection.endpoint.clone(),
            opener: Arc::new(Opener(connection.state.clone())),
        };
        let worker = SftpWorker::new(
            connection.connector.clone(),
            route,
            connection.data_dir.join("upload-resume"),
            connection.data_dir.join("download-resume"),
        );
        let (commands, commands_rx) = async_channel::unbounded();
        let replies = Arc::new(Replies::default());
        let events = Arc::new(Events::new());
        let (ready_tx, ready) = oneshot::channel();
        let sink = sink(replies.clone(), events.clone(), ready_tx);
        let spawned = std::thread::Builder::new()
            .name("shellrs-sftp".into())
            .spawn({
                let replies = replies.clone();
                let events = events.clone();
                move || {
                    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        let _ = runtime.block_on(worker.run(commands_rx, sink));
                    }
                    // Whatever still waits gets its answer: closed.
                    replies.lock().clear();
                    events.finish(None);
                }
            });
        if let Err(error) = spawned {
            return Err(SshError::Internal {
                detail: error.to_string(),
            });
        }
        let home = match ready.await {
            Ok(Ok(home)) => home,
            Ok(Err(reason)) => {
                let _ = commands.try_send(SftpCommand::Shutdown);
                return Err(SshError::Failed {
                    failure: Failure::sftp(reason.error()),
                });
            }
            Err(_) => return Err(SshError::Closed),
        };
        Ok(Arc::new(Self {
            _connection: connection,
            commands,
            replies,
            next_request: AtomicU64::new(1),
            events,
            home,
        }))
    }
}

#[uniffi::export]
impl SftpSession {
    /// The user's home folder, where browsing starts.
    pub fn home(&self) -> String {
        self.home.clone()
    }

    /// The next event; `None` once the session has closed.
    pub async fn next_event(&self) -> Option<SftpEvent> {
        self.events.next().await
    }

    pub async fn list(&self, path: String) -> Result<DirectoryListing, SshError> {
        let path = remote(&path)?;
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        match self
            .call(request_id, SftpCommand::List { request_id, path })
            .await?
        {
            Reply::Listed(result) => result.map(Into::into).map_err(failed),
            Reply::Operated(_) => Err(SshError::Internal {
                detail: "a listing answered as an operation".into(),
            }),
        }
    }

    pub async fn create_directory(&self, path: String) -> Result<(), SshError> {
        let path = remote(&path)?;
        self.operate(RemoteOperation::CreateDirectory { path })
            .await
    }

    /// Create an empty file; fails if the name is taken.
    pub async fn create_file(&self, path: String) -> Result<(), SshError> {
        let path = remote(&path)?;
        self.operate(RemoteOperation::CreateFile { path }).await
    }

    /// Rename or move; never replaces what is at `to`.
    pub async fn rename(&self, from: String, to: String) -> Result<(), SshError> {
        let (from, to) = (remote(&from)?, remote(&to)?);
        self.operate(RemoteOperation::Rename { from, to }).await
    }

    /// Delete files, links (not what they point at) and folders with
    /// everything in them.
    pub async fn delete(&self, paths: Vec<String>) -> Result<(), SshError> {
        let paths = paths
            .iter()
            .map(|path| remote(path))
            .collect::<Result<_, _>>()?;
        self.operate(RemoteOperation::Delete { paths }).await
    }

    /// Set the mode bits of `paths`, and with `recursive` of everything in
    /// them; links are skipped.
    pub async fn set_permissions(
        &self,
        paths: Vec<String>,
        mode: u32,
        recursive: bool,
    ) -> Result<(), SshError> {
        let paths = paths
            .iter()
            .map(|path| remote(path))
            .collect::<Result<_, _>>()?;
        self.operate(RemoteOperation::SetPermissions {
            paths,
            edit: engine::PermissionEdit::exact(mode),
            recursive,
            add_x_to_dirs: false,
        })
        .await
    }

    /// Upload the local files and folders `sources` into the remote folder
    /// `destination`. Returns once queued: the transfer says how it goes
    /// in events. A batch already running refuses it with a notice.
    pub fn upload(&self, sources: Vec<String>, destination: String) -> Result<(), SshError> {
        let request = engine::UploadRequest::new(
            sources.into_iter().map(PathBuf::from).collect(),
            remote(&destination)?,
        )
        .map_err(failed)?;
        self.send(SftpCommand::Upload(request))
    }

    /// Download the remote files and folders `sources` into the local
    /// folder `destination`; as [`upload`](Self::upload).
    pub fn download(&self, sources: Vec<String>, destination: String) -> Result<(), SshError> {
        let sources = sources
            .iter()
            .map(|path| remote(path))
            .collect::<Result<_, _>>()?;
        let request =
            engine::DownloadRequest::new(sources, PathBuf::from(destination)).map_err(failed)?;
        self.send(SftpCommand::Download(request))
    }

    /// Stop the transfer, keeping what is done to resume.
    pub fn cancel(&self) -> Result<(), SshError> {
        self.send(SftpCommand::Cancel)
    }

    /// Go on with a stopped transfer.
    pub fn resume(&self) -> Result<(), SshError> {
        self.send(SftpCommand::Resume)
    }

    /// Give up a stopped transfer, removing its partial files.
    pub fn discard(&self) -> Result<(), SshError> {
        self.send(SftpCommand::Discard)
    }

    pub fn answer(&self, question_id: u64, answer: TransferAnswer) -> Result<(), SshError> {
        self.send(SftpCommand::Answer {
            request_id: question_id,
            answer: answer.into(),
        })
    }

    /// Close the session's channel. The connection stays. (Not `close`,
    /// which Kotlin's `AutoCloseable` takes.)
    pub fn end(&self) {
        let _ = self.commands.try_send(SftpCommand::Shutdown);
    }
}

impl SftpSession {
    fn send(&self, command: SftpCommand) -> Result<(), SshError> {
        self.commands
            .try_send(command)
            .map_err(|_| SshError::Closed)
    }

    async fn operate(&self, operation: RemoteOperation) -> Result<(), SshError> {
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        match self
            .call(
                request_id,
                SftpCommand::Operate {
                    request_id,
                    operation,
                },
            )
            .await?
        {
            Reply::Operated(result) => result.map_err(failed),
            Reply::Listed(_) => Err(SshError::Internal {
                detail: "an operation answered as a listing".into(),
            }),
        }
    }

    /// Send `command` and wait for its answer. A call the app gives up on
    /// stops waiting; the engine's answer then goes nowhere.
    async fn call(&self, request_id: u64, command: SftpCommand) -> Result<Reply, SshError> {
        let (reply, answer) = oneshot::channel();
        self.replies.lock().insert(request_id, reply);
        let _forget = Forget(&self.replies, request_id);
        self.send(command)?;
        answer.await.map_err(|_| SshError::Closed)
    }
}

impl Drop for SftpSession {
    fn drop(&mut self) {
        let _ = self.commands.try_send(SftpCommand::Shutdown);
    }
}

/// Drops a call's place in the replies however the call ends.
struct Forget<'a>(&'a Replies, u64);

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.0.lock().remove(&self.1);
    }
}

/// Where the worker's events go: answers to their calls, the first
/// connection to whoever waits for the session, the rest to the app.
fn sink(
    replies: Arc<Replies>,
    events: Arc<Events<SftpEvent>>,
    ready: oneshot::Sender<Result<String, Reason>>,
) -> EventSink {
    let ready = Mutex::new(Some(ready));
    let take_ready = move || {
        ready
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    };
    EventSink::new(move |event| {
        match event {
            EngineEvent::Listed { request_id, result } => {
                replies.answer(request_id, Reply::Listed(result));
            }
            EngineEvent::Operated { request_id, result } => {
                replies.answer(request_id, Reply::Operated(result));
            }
            EngineEvent::Connected { home } => match take_ready() {
                Some(ready) => {
                    let _ = ready.send(Ok(home.to_string()));
                }
                None => events.push(SftpEvent::Connected {
                    home: home.to_string(),
                }),
            },
            EngineEvent::Disconnected(why) => {
                if let (engine::Disconnect::LoginFailed(reason), Some(ready)) = (&why, take_ready())
                {
                    let _ = ready.send(Err(reason.clone()));
                } else {
                    events.push(SftpEvent::Disconnected {
                        reason: (&why).into(),
                    });
                }
            }
            EngineEvent::Progress(progress) => events.push(SftpEvent::Progress {
                progress: (&progress).into(),
            }),
            EngineEvent::Question(question) => events.push(SftpEvent::Question {
                question: (&question).into(),
            }),
            EngineEvent::Notice(reason) => events.push(SftpEvent::Notice {
                failure: Failure::sftp(reason.error()),
            }),
            EngineEvent::Idle => events.push(SftpEvent::Idle),
            // Not asked for here: no editor, no login of its own, and the
            // connection measures its own latency.
            EngineEvent::Connecting
            | EngineEvent::Prompt(_)
            | EngineEvent::Latency(_)
            | EngineEvent::FileRead { .. }
            | EngineEvent::FileWritten { .. }
            | EngineEvent::BytesRead { .. } => {}
        }
        true
    })
}

/// SFTP channels on the connection, whenever it is up.
struct Opener(watch::Receiver<State>);

impl engine::SftpChannelOpener for Opener {
    fn open(&self) -> BoxFuture<'static, Result<SftpChannel>> {
        let state = self.0.borrow().clone();
        Box::pin(async move {
            let State::Up {
                handle,
                fingerprint,
            } = state
            else {
                return Err(engine::TerminalNotConnected.into());
            };
            let stream = ssh::open_sftp_channel(&handle, OPEN_TIMEOUT).await?;
            Ok(SftpChannel {
                stream,
                fingerprint,
            })
        })
    }
}

fn remote(path: &str) -> Result<RemotePath, SshError> {
    RemotePath::new(path).map_err(failed)
}

fn failed(reason: impl Into<Reason>) -> SshError {
    SshError::Failed {
        failure: Failure::sftp(reason.into().error()),
    }
}

fn reason(reason: Option<&Reason>) -> Option<Failure> {
    reason.map(|reason| Failure::sftp(reason.error()))
}

impl From<engine::DirectoryListing> for DirectoryListing {
    fn from(listing: engine::DirectoryListing) -> Self {
        Self {
            path: listing.path().into(),
            entries: listing
                .entries()
                .iter()
                .map(|entry| DirectoryEntry {
                    name: entry.name().into(),
                    kind: entry.metadata().kind().into(),
                    size: entry.metadata().size(),
                    modified: entry.metadata().modified(),
                    permissions: entry.metadata().permissions(),
                    owner: entry.owner().map(Into::into),
                    group: entry.group().map(Into::into),
                    target_kind: entry.target_kind().map(Into::into),
                })
                .collect(),
        }
    }
}

impl From<engine::EntryKind> for EntryKind {
    fn from(kind: engine::EntryKind) -> Self {
        match kind {
            engine::EntryKind::File => Self::File,
            engine::EntryKind::Directory => Self::Directory,
            engine::EntryKind::Symlink => Self::Symlink,
            engine::EntryKind::Other => Self::Other,
        }
    }
}

impl From<&engine::Disconnect> for SftpDisconnect {
    fn from(why: &engine::Disconnect) -> Self {
        match why {
            engine::Disconnect::ByUser => Self::ByUser,
            engine::Disconnect::Dropped => Self::Dropped,
            engine::Disconnect::DroppedKept => Self::DroppedKept,
            engine::Disconnect::LoginFailed(reason) => Self::LoginFailed {
                failure: Failure::sftp(reason.error()),
            },
            engine::Disconnect::Failed(reason) => Self::Failed {
                failure: Failure::sftp(reason.error()),
            },
        }
    }
}

impl From<&engine::TransferProgress> for TransferProgress {
    fn from(progress: &engine::TransferProgress) -> Self {
        Self {
            direction: match progress.direction() {
                engine::TransferDirection::Upload => TransferDirection::Upload,
                engine::TransferDirection::Download => TransferDirection::Download,
            },
            phase: match progress.phase() {
                engine::TransferPhase::Scanning => TransferPhase::Scanning,
                engine::TransferPhase::Transferring => TransferPhase::Transferring,
                engine::TransferPhase::Waiting => TransferPhase::Waiting,
                engine::TransferPhase::Reconnecting => TransferPhase::Reconnecting,
                engine::TransferPhase::Stopped => TransferPhase::Stopped,
                engine::TransferPhase::Completed => TransferPhase::Completed,
            },
            current: progress.current().into(),
            completed_bytes: progress.completed_bytes(),
            total_bytes: progress.total_bytes(),
            total: progress.total() as u64,
            succeeded: progress.succeeded() as u64,
            skipped: progress.skipped() as u64,
            failed: progress.failed() as u64,
            deleted: progress.deleted() as u64,
            bytes_per_second: progress.bytes_per_second(),
            fraction: progress.fraction(),
            remaining_seconds: progress.remaining().map(|left| left.as_secs()),
            elapsed_seconds: progress.elapsed().as_secs(),
            note: reason(progress.note()),
            details: progress
                .details()
                .iter()
                .map(|detail| TransferDetail {
                    path: detail.path().into(),
                    outcome: match detail.outcome() {
                        engine::TransferOutcome::Done => TransferOutcome::Done,
                        engine::TransferOutcome::Skipped => TransferOutcome::Skipped,
                        engine::TransferOutcome::Failed => TransferOutcome::Failed,
                    },
                    reason: reason(detail.reason()),
                })
                .collect(),
        }
    }
}

impl From<&engine::TransferQuestion> for TransferQuestion {
    fn from(question: &engine::TransferQuestion) -> Self {
        Self {
            id: question.id(),
            kind: match question.kind() {
                engine::TransferQuestionKind::Conflict => TransferQuestionKind::Conflict,
                engine::TransferQuestionKind::Resume => TransferQuestionKind::Resume,
                engine::TransferQuestionKind::InvalidResume => TransferQuestionKind::InvalidResume,
                engine::TransferQuestionKind::Error => TransferQuestionKind::Error,
            },
            path: question.path().into(),
            message: Failure::sftp(question.message().error()),
        }
    }
}

impl From<TransferAnswer> for engine::TransferAnswer {
    fn from(answer: TransferAnswer) -> Self {
        let choice = match answer.choice {
            TransferChoice::Overwrite => engine::TransferChoice::Overwrite,
            TransferChoice::Skip => engine::TransferChoice::Skip,
            TransferChoice::Cancel => engine::TransferChoice::Cancel,
            TransferChoice::Retry => engine::TransferChoice::Retry,
            TransferChoice::Resume => engine::TransferChoice::Resume,
            TransferChoice::Restart => engine::TransferChoice::Restart,
        };
        engine::TransferAnswer::new(choice, answer.apply_to_all)
    }
}
