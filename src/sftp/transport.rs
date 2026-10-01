use super::{
    DirectoryListing, DownloadRequest, PermissionEdit, RemotePath, TransferAnswer,
    TransferProgress, TransferQuestion, UploadRequest,
};
use crate::{
    connection::{ConnectionPrompt, ConnectionPromptReply, Latency},
    host::HostLogin,
};
use anyhow::Result;
use async_channel::{Receiver, Sender};
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
#[derive(Clone, Debug)]
pub enum SftpEvent {
    Connecting,
    Connected {
        home: RemotePath,
    },
    Disconnected(String),
    Prompt(ConnectionPrompt),
    Listed {
        request_id: u64,
        result: Result<DirectoryListing, String>,
    },
    Operated {
        request_id: u64,
        result: Result<(), String>,
    },
    Progress(TransferProgress),
    Question(TransferQuestion),
    Notice(String),
    Idle,
    /// The connection's latest round trip: every few seconds while connected,
    /// and right after connecting, transfers or not.
    Latency(Latency),
}
/// One worker lifetime; the caller runs it on its own thread.
pub trait SftpTransport: Send + 'static {
    fn run(
        self: Box<Self>,
        commands: Receiver<SftpCommand>,
        events: Sender<SftpEvent>,
    ) -> Result<()>;
}
pub trait SftpTransportProvider: Send + Sync + 'static {
    fn create(&self, login: &HostLogin) -> Box<dyn SftpTransport>;
}
pub type SharedSftpTransportProvider = Arc<dyn SftpTransportProvider>;
