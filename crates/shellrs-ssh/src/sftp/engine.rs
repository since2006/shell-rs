//! What an SFTP worker is told and what it says: the commands it serves,
//! the events it sends, and where its channel comes from. Every failure in
//! an event is a [`Reason`], for whoever shows it to word.

use super::{
    DirectoryListing, DownloadRequest, FileBytes, FileStamp, PermissionEdit, Reason, RemotePath,
    TransferAnswer, TransferProgress, TransferQuestion, UploadRequest,
    edit::{ReadFailure, SaveFailure},
};
use crate::{ConnectionPrompt, ConnectionPromptReply, HostLogin, Latency, SftpChannel};
use anyhow::Result;
use futures::future::BoxFuture;
use std::sync::Arc;

/// A one-shot change to remote files. None of them follows symbolic links:
/// deleting a link removes the link, and permission changes skip links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteOperation {
    /// Delete files, links, and directories with everything inside them.
    Delete {
        paths: Vec<RemotePath>,
    },
    /// Rename without replacing an existing item.
    Rename {
        from: RemotePath,
        to: RemotePath,
    },
    CreateDirectory {
        path: RemotePath,
    },
    /// Create an empty file; fails if the name exists.
    CreateFile {
        path: RemotePath,
    },
    SetPermissions {
        paths: Vec<RemotePath>,
        edit: PermissionEdit,
        recursive: bool,
        add_x_to_dirs: bool,
    },
}

#[derive(Debug)]
pub enum SftpCommand {
    List {
        request_id: u64,
        path: RemotePath,
    },
    /// Runs alongside listings and transfers; answered by `Operated`.
    Operate {
        request_id: u64,
        operation: RemoteOperation,
    },
    /// Read a whole text file for the editor; answered by `FileRead`. Runs
    /// alongside listings and transfers, like an operation.
    ReadFile {
        request_id: u64,
        path: RemotePath,
    },
    /// Read a whole file as it is, up to `limit` bytes, for a preview;
    /// answered by `BytesRead`.
    ReadBytes {
        request_id: u64,
        path: RemotePath,
        limit: u64,
    },
    /// Write the editor's text over a file in place; answered by
    /// `FileWritten`. With `expected`, only if the file still matches it.
    WriteFile {
        request_id: u64,
        path: RemotePath,
        bytes: Vec<u8>,
        expected: Option<FileStamp>,
    },
    Upload(UploadRequest),
    /// A download batch; like an upload, one batch runs at a time.
    Download(DownloadRequest),
    PromptReply {
        request_id: u64,
        reply: ConnectionPromptReply,
    },
    Answer {
        request_id: u64,
        answer: TransferAnswer,
    },
    Cancel,
    Resume,
    Discard,
    Disconnect,
    Shutdown,
}

/// Why there is no connection now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Disconnect {
    /// The user disconnected.
    ByUser,
    /// The connection dropped, idle or under a request: reconnecting is up
    /// to the user.
    Dropped,
    /// The connection dropped under a transfer batch, which keeps its
    /// progress and reconnects on its own.
    DroppedKept,
    /// Connecting failed, logging in or starting SFTP.
    LoginFailed(Reason),
    /// One of a batch's reconnects failed.
    Failed(Reason),
}

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Connecting,
    Connected {
        home: RemotePath,
    },
    Disconnected(Disconnect),
    Prompt(ConnectionPrompt),
    Listed {
        request_id: u64,
        result: Result<DirectoryListing, Reason>,
    },
    Operated {
        request_id: u64,
        result: Result<(), Reason>,
    },
    FileRead {
        request_id: u64,
        result: Result<super::TextFile, ReadFailure<Reason>>,
    },
    FileWritten {
        request_id: u64,
        result: Result<FileStamp, SaveFailure<Reason>>,
    },
    BytesRead {
        request_id: u64,
        result: Result<FileBytes, ReadFailure<Reason>>,
    },
    Progress(TransferProgress),
    Question(TransferQuestion),
    /// Something went wrong outside any item: a batch that could not start
    /// or stopped. Worded as its first link.
    Notice(Reason),
    Idle,
    /// The connection's latest round trip: every few seconds while connected,
    /// and right after connecting, transfers or not.
    Latency(Latency),
}

/// Where a worker's events go. Taking one is never blocked on: whatever is
/// behind it queues them, and says once it no longer listens.
#[derive(Clone)]
pub struct EventSink(Arc<dyn Fn(EngineEvent) -> bool + Send + Sync>);

impl EventSink {
    /// Events handed to `take`, which says whether anyone still listens.
    pub fn new(take: impl Fn(EngineEvent) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(take))
    }

    /// Events sent on `sender` as they are.
    pub fn channel(sender: async_channel::Sender<EngineEvent>) -> Self {
        Self::new(move |event| sender.try_send(event).is_ok())
    }

    /// Hand on `event`; false once nobody listens.
    pub fn try_send(&self, event: EngineEvent) -> bool {
        (self.0)(event)
    }

    /// Hand on `event`, failing once nobody listens, as a closed channel
    /// does.
    pub async fn send(&self, event: EngineEvent) -> Result<()> {
        if self.try_send(event) {
            Ok(())
        } else {
            Err(EventsClosed.into())
        }
    }
}

/// Nobody listens to the worker's events any more.
#[derive(Debug)]
pub struct EventsClosed;

impl std::fmt::Display for EventsClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("nobody listens to the SFTP events")
    }
}

impl std::error::Error for EventsClosed {}

/// Where a worker's SFTP channel comes from.
#[derive(Clone)]
pub enum SftpRoute {
    /// A connection of its own, logged in as `login` says.
    Login(HostLogin),
    /// A channel on a connection something else holds, such as a remote
    /// terminal's: no login and no questions; it is up when that connection
    /// is. `endpoint`, `user@host:port`, is what resumable transfers are
    /// recorded by.
    Channel {
        endpoint: String,
        opener: Arc<dyn SftpChannelOpener>,
    },
}

impl SftpRoute {
    /// `user@host:port`, which resumable transfers are recorded by.
    pub fn endpoint(&self) -> String {
        match self {
            SftpRoute::Login(login) => login.endpoint(),
            SftpRoute::Channel { endpoint, .. } => endpoint.clone(),
        }
    }
}

/// Opens SFTP channels on a connection something else holds.
pub trait SftpChannelOpener: Send + Sync + 'static {
    /// A channel with the SFTP subsystem started. Fails with
    /// [`TerminalNotConnected`](super::TerminalNotConnected) while there is
    /// no connection to open it on, which is retried like a dropped one.
    fn open(&self) -> BoxFuture<'static, Result<SftpChannel>>;
}
