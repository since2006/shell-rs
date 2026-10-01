//! The seam between the forward list and whatever carries a forward, so the
//! UI tests can run the list against a transport that opens no socket.

use std::{sync::Arc, time::Duration};

use async_channel::{Receiver, Sender};

use crate::{
    connection::{ConnectionPrompt, ConnectionPromptReply},
    host::{ForwardRule, HostLogin},
};

/// What the list tells a running forward.
pub enum ForwardCommand {
    /// The answer to a [`ForwardEvent::Prompt`].
    PromptReply {
        request_id: u64,
        reply: ConnectionPromptReply,
    },
    /// Stop listening, close every connection and log out.
    Stop,
}

/// What a running forward tells the list. `Failed` and `Stopped` are the
/// last event of a run; nothing follows either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardEvent {
    /// Logging in to the server.
    Connecting,
    /// Logging in needs an answer: a host key to trust, a password.
    Prompt(ConnectionPrompt),
    /// The forward is up and its listening end accepts connections.
    Listening,
    /// How many connections the forward carries right now.
    Connections(usize),
    /// One connection could not be carried. The forward keeps running.
    ConnectionFailed(String),
    /// The connection to the server dropped. Attempt `attempt` of `of` to
    /// log in again starts after `delay`.
    Reconnecting {
        attempt: usize,
        of: usize,
        delay: Duration,
    },
    /// The forward ended on its own, and why.
    Failed(String),
    /// The forward ended because it was told to.
    Stopped,
}

/// One run of a forwarding rule, from start to stop.
pub trait ForwardTransport: Send + 'static {
    /// Carry the forward until it fails or is told to stop. Blocks, so it
    /// is called on a thread of its own.
    fn run(self: Box<Self>, commands: Receiver<ForwardCommand>, events: Sender<ForwardEvent>);
}

pub trait ForwardTransportProvider: Send + Sync + 'static {
    /// A transport for `rule` as it stands now, logging in to its host
    /// with `login`.
    fn create(&self, rule: &ForwardRule, login: &HostLogin) -> Box<dyn ForwardTransport>;
}

pub type SharedForwardTransportProvider = Arc<dyn ForwardTransportProvider>;
