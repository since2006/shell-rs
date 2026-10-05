use super::{
    DirectoryListing, DownloadRequest, FileBytes, FileStamp, PermissionEdit, ReadFailure,
    RemotePath, SaveFailure, TextFile, TransferAnswer, TransferProgress, TransferQuestion,
    UploadRequest,
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
    FileRead {
        request_id: u64,
        result: Result<TextFile, ReadFailure>,
    },
    FileWritten {
        request_id: u64,
        result: Result<FileStamp, SaveFailure>,
    },
    BytesRead {
        request_id: u64,
        result: Result<FileBytes, ReadFailure>,
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
