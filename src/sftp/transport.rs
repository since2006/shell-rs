use super::{
    DirectoryListing, RemotePath, UploadAnswer, UploadProgress, UploadQuestion, UploadRequest,
};
use crate::{
    connection::{ConnectionPrompt, ConnectionPromptReply},
    session::Session,
};
use anyhow::Result;
use async_channel::{Receiver, Sender};
use std::sync::Arc;

#[derive(Debug)]
pub enum SftpCommand {
    List {
        request_id: u64,
        path: RemotePath,
    },
    Upload(UploadRequest),
    PromptReply {
        request_id: u64,
        reply: ConnectionPromptReply,
    },
    Answer {
        request_id: u64,
        answer: UploadAnswer,
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
    Progress(UploadProgress),
    Question(UploadQuestion),
    Notice(String),
    Idle,
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
    fn create(&self, session: &Session) -> Box<dyn SftpTransport>;
}
pub type SharedSftpTransportProvider = Arc<dyn SftpTransportProvider>;
