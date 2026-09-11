use std::sync::{Arc, mpsc};

use anyhow::Result;
use async_channel::Sender;

use super::TerminalSize;

/// Commands accepted by every terminal byte-stream transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalTransportCommand {
    Write(Vec<u8>),
    Resize(TerminalSize),
    Shutdown,
}

/// Events produced by a terminal byte-stream transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalTransportEvent {
    Started,
    Output(Vec<u8>),
    Exited { code: u32, signal: Option<String> },
    Failed(String),
}

/// One running transport. Implementations may block because shellr always
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

pub type SharedTerminalTransportFactory = Arc<dyn TerminalTransportFactory>;

pub(crate) fn send_event(
    events: &Sender<TerminalTransportEvent>,
    event: TerminalTransportEvent,
) -> bool {
    events.send_blocking(event).is_ok()
}
