//! The app's side of an SFTP worker: the events as the explorer and the
//! external CLI take them, worded, and the provider that starts workers on
//! their own threads.

use super::{
    DirectoryListing, EngineEvent, EventSink, FileBytes, FileStamp, ReadFailure, RemotePath,
    SaveFailure, SftpChannelOpener, SftpCommand, SftpWorker, TerminalNotConnected, TextFile,
    TransferProgress, TransferQuestion, describe_disconnect, describe_reason, describe_reason_link,
};
use crate::{
    connection::{ConnectionPrompt, Latency},
    host::HostLogin,
    ssh::{SftpChannel, SshConnector},
    terminal::TerminalChannels,
};
use anyhow::{Result, anyhow};
use async_channel::{Receiver, Sender};
use futures::future::BoxFuture;
use shellrs_ssh::sftp as engine;
use std::{path::PathBuf, sync::Arc};

/// What a worker says, with every failure in words: those of the
/// interface language when it happened. Progress and questions keep theirs
/// as they are, worded when shown.
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
/// Where a worker's SFTP channel comes from.
#[derive(Clone)]
pub enum SftpRoute {
    /// A connection of its own, logged in for the SFTP tab.
    Login,
    /// A channel beside the shell of a remote terminal, on whichever
    /// connection the terminal has: the right sidebar's 文件. No login and
    /// no questions; it is up when the terminal is.
    Terminal(TerminalChannels),
}
pub trait SftpTransportProvider: Send + Sync + 'static {
    /// `login` still matters on the terminal route: resumable transfers are
    /// recorded by its endpoint.
    fn create(&self, login: &HostLogin, route: SftpRoute) -> Box<dyn SftpTransport>;
}
pub type SharedSftpTransportProvider = Arc<dyn SftpTransportProvider>;

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
    fn create(&self, login: &HostLogin, route: SftpRoute) -> Box<dyn SftpTransport> {
        let route = match route {
            SftpRoute::Login => engine::SftpRoute::Login(login.clone()),
            SftpRoute::Terminal(channels) => engine::SftpRoute::Channel {
                endpoint: login.endpoint(),
                opener: Arc::new(channels),
            },
        };
        Box::new(SshSftpTransport(SftpWorker::new(
            self.connector.clone(),
            route,
            self.resume_dir.clone(),
            self.download_resume_dir.clone(),
        )))
    }
}

struct SshSftpTransport(SftpWorker);

impl SftpTransport for SshSftpTransport {
    fn run(
        self: Box<Self>,
        commands: Receiver<SftpCommand>,
        events: Sender<SftpEvent>,
    ) -> Result<()> {
        // Worded here, on the worker's thread, as each event happens.
        let events = EventSink::new(move |event| events.try_send(app_event(event)).is_ok());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(self.0.run(commands, events))
    }
}

/// `event` as the app takes it.
fn app_event(event: EngineEvent) -> SftpEvent {
    match event {
        EngineEvent::Connecting => SftpEvent::Connecting,
        EngineEvent::Connected { home } => SftpEvent::Connected { home },
        EngineEvent::Disconnected(why) => SftpEvent::Disconnected(describe_disconnect(&why)),
        EngineEvent::Prompt(prompt) => SftpEvent::Prompt(prompt),
        EngineEvent::Listed { request_id, result } => SftpEvent::Listed {
            request_id,
            result: result.map_err(|why| describe_reason(&why)),
        },
        EngineEvent::Operated { request_id, result } => SftpEvent::Operated {
            request_id,
            result: result.map_err(|why| describe_reason(&why)),
        },
        EngineEvent::FileRead { request_id, result } => SftpEvent::FileRead {
            request_id,
            result: result.map_err(|failure| failure.map_failed(|why| describe_reason(&why))),
        },
        EngineEvent::FileWritten { request_id, result } => SftpEvent::FileWritten {
            request_id,
            result: result.map_err(|failure| failure.map_failed(|why| describe_reason(&why))),
        },
        EngineEvent::BytesRead { request_id, result } => SftpEvent::BytesRead {
            request_id,
            result: result.map_err(|failure| failure.map_failed(|why| describe_reason(&why))),
        },
        EngineEvent::Progress(progress) => SftpEvent::Progress(progress),
        EngineEvent::Question(question) => SftpEvent::Question(question),
        EngineEvent::Notice(why) => SftpEvent::Notice(describe_reason_link(&why)),
        EngineEvent::Idle => SftpEvent::Idle,
        EngineEvent::Latency(latency) => SftpEvent::Latency(latency),
    }
}

/// 文件 opens its channels on the terminal's connection, whichever it has
/// now.
impl SftpChannelOpener for TerminalChannels {
    fn open(&self) -> BoxFuture<'static, Result<SftpChannel>> {
        let answer = self.open_sftp();
        Box::pin(async move {
            answer
                .await
                .map_err(|_| TerminalNotConnected)?
                .map_err(|refusal| anyhow!(refusal))
        })
    }
}
