use std::sync::{Arc, mpsc};

use anyhow::Result;
use async_channel::Sender;

use super::TerminalSize;
use crate::connection::Latency;
use crate::connection::{ConnectionPrompt, ConnectionPromptReply};
use crate::host::{HostLogin, HostOs};

/// Commands accepted by every terminal byte-stream transport.
pub enum TerminalTransportCommand {
    Write(Vec<u8>),
    Resize(TerminalSize),
    PromptReply {
        request_id: u64,
        reply: ConnectionPromptReply,
    },
    Shutdown,
}

impl std::fmt::Debug for TerminalTransportCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write(bytes) => formatter.debug_tuple("Write").field(&bytes.len()).finish(),
            Self::Resize(size) => formatter.debug_tuple("Resize").field(size).finish(),
            Self::PromptReply { request_id, reply } => formatter
                .debug_struct("PromptReply")
                .field("request_id", request_id)
                .field("reply", reply)
                .finish(),
            Self::Shutdown => formatter.write_str("Shutdown"),
        }
    }
}

/// Events produced by a terminal byte-stream transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalTransportEvent {
    Started,
    Output(Vec<u8>),
    Prompt(ConnectionPrompt),
    /// What the transport found running on the host. Sent once per successful
    /// connection, after the shell is up.
    HostOsDetected(HostOs),
    /// A round trip measured on the live connection. Sent periodically while
    /// the shell runs; transports without a network hop never send it.
    Latency(Latency),
    Exited {
        code: u32,
        signal: Option<String>,
    },
    Failed(String),
}

/// One running transport. Implementations may block because ShellRS always
/// invokes this method on a dedicated worker thread.
pub trait TerminalTransport: Send + 'static {
    fn run(
        self: Box<Self>,
        initial_size: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()>;
}

/// Creates a fresh one-shot transport for initial launch and restart.
pub trait TerminalTransportFactory: Send + Sync + 'static {
    fn create(&self) -> Box<dyn TerminalTransport>;
}

/// Creates a fresh remote factory from the latest saved login of a host.
/// Keeping this boundary at the workspace makes reconnects pick up edits
/// immediately and lets UI tests inject a deterministic fake.
pub trait RemoteTerminalTransportProvider: Send + Sync + 'static {
    fn factory_for(&self, login: &HostLogin) -> SharedTerminalTransportFactory;
}

pub type SharedRemoteTerminalTransportProvider = Arc<dyn RemoteTerminalTransportProvider>;

/// Provider useful for tests and embedders that want every host to use the
/// same factory.
pub struct FixedRemoteTerminalTransportProvider {
    factory: SharedTerminalTransportFactory,
}

impl FixedRemoteTerminalTransportProvider {
    pub fn new(factory: SharedTerminalTransportFactory) -> Self {
        Self { factory }
    }
}

impl RemoteTerminalTransportProvider for FixedRemoteTerminalTransportProvider {
    fn factory_for(&self, _: &HostLogin) -> SharedTerminalTransportFactory {
        self.factory.clone()
    }
}

pub type SharedTerminalTransportFactory = Arc<dyn TerminalTransportFactory>;

pub(crate) fn send_event(
    events: &Sender<TerminalTransportEvent>,
    event: TerminalTransportEvent,
) -> bool {
    events.send_blocking(event).is_ok()
}
