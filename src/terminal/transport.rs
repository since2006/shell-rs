use std::sync::{Arc, mpsc};

use anyhow::Result;
use async_channel::Sender;
use zeroize::Zeroizing;

use super::TerminalSize;
use crate::session::Session;

/// One field requested by an SSH keyboard-interactive challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalPromptField {
    label: String,
    echo: bool,
}

impl TerminalPromptField {
    pub fn new(label: impl Into<String>, echo: bool) -> Self {
        Self {
            label: label.into(),
            echo,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn echo(&self) -> bool {
        self.echo
    }
}

/// A transport question that must be answered by the UI before SSH setup can
/// continue. The id is scoped to a single transport generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalPrompt {
    request_id: u64,
    kind: TerminalPromptKind,
}

impl TerminalPrompt {
    pub fn new(request_id: u64, kind: TerminalPromptKind) -> Self {
        Self { request_id, kind }
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }
    pub fn kind(&self) -> &TerminalPromptKind {
        &self.kind
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPromptKind {
    UnknownHost(UnknownHostPrompt),
    HostKeyChanged(HostKeyChangedPrompt),
    Authentication(AuthenticationPrompt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownHostPrompt {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
}

impl UnknownHostPrompt {
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyChangedPrompt {
    host: String,
    port: u16,
    algorithm: String,
    old_fingerprints: Vec<String>,
    fingerprint: String,
    known_hosts_path: std::path::PathBuf,
}

impl HostKeyChangedPrompt {
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }
    pub fn old_fingerprints(&self) -> &[String] {
        &self.old_fingerprints
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn known_hosts_path(&self) -> &std::path::Path {
        &self.known_hosts_path
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticationPrompt {
    title: String,
    instructions: String,
    fields: Vec<TerminalPromptField>,
}

impl AuthenticationPrompt {
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn instructions(&self) -> &str {
        &self.instructions
    }
    pub fn fields(&self) -> &[TerminalPromptField] {
        &self.fields
    }
}

impl TerminalPromptKind {
    pub fn unknown_host(
        host: impl Into<String>,
        port: u16,
        algorithm: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self::UnknownHost(UnknownHostPrompt {
            host: host.into(),
            port,
            algorithm: algorithm.into(),
            fingerprint: fingerprint.into(),
        })
    }

    pub fn host_key_changed(
        host: impl Into<String>,
        port: u16,
        algorithm: impl Into<String>,
        old_fingerprints: Vec<String>,
        fingerprint: impl Into<String>,
        known_hosts_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::HostKeyChanged(HostKeyChangedPrompt {
            host: host.into(),
            port,
            algorithm: algorithm.into(),
            old_fingerprints,
            fingerprint: fingerprint.into(),
            known_hosts_path: known_hosts_path.into(),
        })
    }

    pub fn authentication(
        title: impl Into<String>,
        instructions: impl Into<String>,
        fields: Vec<TerminalPromptField>,
    ) -> Self {
        Self::Authentication(AuthenticationPrompt {
            title: title.into(),
            instructions: instructions.into(),
            fields,
        })
    }
}

/// A secret that is erased when dropped and never prints its contents.
pub struct TerminalSecret(Zeroizing<String>);

impl TerminalSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }
    pub(crate) fn expose(&self) -> &str {
        self.0.as_str()
    }
    pub(crate) fn into_inner(mut self) -> String {
        std::mem::take(&mut *self.0)
    }
}

impl std::fmt::Debug for TerminalSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TerminalSecret([已隐藏])")
    }
}

/// UI response to a transport prompt. Deliberately not `Clone`: answers may
/// contain passwords, private-key passphrases or one-time codes.
pub enum TerminalPromptReply {
    TrustAndSave,
    Answers(Vec<TerminalSecret>),
    Cancel,
}

impl std::fmt::Debug for TerminalPromptReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TrustAndSave => formatter.write_str("TrustAndSave"),
            Self::Answers(answers) => formatter
                .debug_tuple("Answers")
                .field(&format_args!("{} 个已隐藏答案", answers.len()))
                .finish(),
            Self::Cancel => formatter.write_str("Cancel"),
        }
    }
}

/// Commands accepted by every terminal byte-stream transport.
pub enum TerminalTransportCommand {
    Write(Vec<u8>),
    Resize(TerminalSize),
    PromptReply {
        request_id: u64,
        reply: TerminalPromptReply,
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
    Prompt(TerminalPrompt),
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

/// Creates a fresh remote factory from the latest saved session. Keeping this
/// boundary at the workspace makes reconnects pick up edits immediately and
/// lets UI tests inject a deterministic fake.
pub trait RemoteTerminalTransportProvider: Send + Sync + 'static {
    fn factory_for(&self, session: &Session) -> SharedTerminalTransportFactory;
}

pub type SharedRemoteTerminalTransportProvider = Arc<dyn RemoteTerminalTransportProvider>;

/// Provider useful for tests and embedders that want every session to use the
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
    fn factory_for(&self, _: &Session) -> SharedTerminalTransportFactory {
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
