use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use anyhow::Result;
use async_channel::Sender;
use tokio::sync::oneshot;

use super::{LocalLaunch, LocalShell, TerminalSize};
use crate::connection::Latency;
use crate::connection::{ConnectionPrompt, ConnectionPromptReply};
use crate::host::{HostConnection, HostLogin, HostOs, SerialLine, TelnetLogin};

/// Commands accepted by every terminal byte-stream transport.
pub enum TerminalTransportCommand {
    Write(Vec<u8>),
    /// A file transfer's bytes (ZMODEM): written as they are, which for
    /// a Telnet terminal means past its local line editing.
    WriteData(Vec<u8>),
    Resize(TerminalSize),
    PromptReply {
        request_id: u64,
        reply: ConnectionPromptReply,
    },
    /// Run a command beside the shell. Only a transport with a connection of
    /// its own runs it; the others drop the request.
    Exec(ExecRequest),
    /// Open an SFTP channel beside the shell, for the right sidebar's
    /// 文件. Dropped like `Exec` by a transport without a connection.
    OpenSftp(SftpChannelRequest),
    /// 发送 Break: a serial transport holds its line in the break condition
    /// for a moment, a Telnet one sends Telnet's break (which a console
    /// server passes on to its line). SSH has none and drops it.
    SendBreak,
    Shutdown,
}

/// What a command run beside the shell printed, or why it did not run.
pub type ExecResult = Result<String, String>;

/// A command to run beside the shell, on the terminal's own SSH connection:
/// how the right sidebar's tools read the host, with no login of their own.
/// The command gets no terminal and no input.
///
/// Answered once on `reply` with the command's standard output, or with
/// why it could not run. A transport without a connection (a local PTY)
/// drops the request, which the asker sees as the reply channel closing.
pub struct ExecRequest {
    pub command: String,
    pub reply: mpsc::Sender<ExecResult>,
}

pub use crate::ssh::{ChannelStream, SftpChannel};

/// Answered once on `reply` with the channel, or with why the server or the
/// terminal would not open it. A request sent while the terminal logs in
/// waits for its shell; one with no connection to go to is dropped, which
/// the asker sees as the reply channel closing.
pub struct SftpChannelRequest {
    pub reply: oneshot::Sender<Result<SftpChannel, String>>,
}

/// Where a terminal's channels beside the shell are opened: whichever
/// connection the terminal has now. Each launch and restart attaches its
/// own, so a handle given out once keeps reaching the current one.
#[derive(Clone, Default)]
pub struct TerminalChannels(Arc<Mutex<Option<mpsc::Sender<TerminalTransportCommand>>>>);

impl TerminalChannels {
    pub(crate) fn attach(&self, commands: mpsc::Sender<TerminalTransportCommand>) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(commands);
    }

    /// The terminal stopped: nothing opens until the next start attaches.
    pub(crate) fn detach(&self) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }

    /// Ask for an SFTP channel. The answer closes without a value when there
    /// is no connection to open it on.
    pub fn open_sftp(&self) -> oneshot::Receiver<Result<SftpChannel, String>> {
        let (reply, answer) = oneshot::channel();
        if let Some(commands) = self
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            let _ = commands.send(TerminalTransportCommand::OpenSftp(SftpChannelRequest {
                reply,
            }));
        }
        answer
    }
}

impl std::fmt::Debug for TerminalTransportCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Write(bytes) => formatter.debug_tuple("Write").field(&bytes.len()).finish(),
            Self::WriteData(bytes) => formatter
                .debug_tuple("WriteData")
                .field(&bytes.len())
                .finish(),
            Self::Resize(size) => formatter.debug_tuple("Resize").field(size).finish(),
            Self::PromptReply { request_id, reply } => formatter
                .debug_struct("PromptReply")
                .field("request_id", request_id)
                .field("reply", reply)
                .finish(),
            Self::Exec(request) => formatter
                .debug_tuple("Exec")
                .field(&request.command.len())
                .finish(),
            Self::OpenSftp(_) => formatter.write_str("OpenSftp"),
            Self::SendBreak => formatter.write_str("SendBreak"),
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
    /// The folder a local terminal's shell is in now, sent when it changes;
    /// only a local transport of a system that tells sends it.
    WorkingDirectory(PathBuf),
    /// What the terminal showed when it last closed, drawn before anything
    /// else. Sent first, by a local transport given it; nothing watches it
    /// as it would output (highlight rules, notices, ZMODEM).
    Replay(Vec<u8>),
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

/// Creates the factory of a serial host's terminal from the line the store
/// has for it, asked when it opens and when it reconnects. The workspace owns
/// this boundary too, and UI tests inject a fake.
pub trait SerialTerminalTransportProvider: Send + Sync + 'static {
    fn factory_for(&self, line: &SerialLine) -> SharedTerminalTransportFactory;
}

pub type SharedSerialTerminalTransportProvider = Arc<dyn SerialTerminalTransportProvider>;

/// Creates the factory of a Telnet host's terminal for the address, route
/// and 自动登录 it has, asked when it opens and when it reconnects. The
/// workspace owns this boundary too, and UI tests inject a fake.
pub trait TelnetTerminalTransportProvider: Send + Sync + 'static {
    fn factory_for(&self, login: &TelnetLogin) -> SharedTerminalTransportFactory;
}

pub type SharedTelnetTerminalTransportProvider = Arc<dyn TelnetTerminalTransportProvider>;

/// Where a host's terminal gets its bytes: an SSH connection, a Telnet
/// one, or a serial port of this computer.
#[derive(Clone)]
pub struct HostTerminalTransports {
    pub ssh: SharedRemoteTerminalTransportProvider,
    pub telnet: SharedTelnetTerminalTransportProvider,
    pub serial: SharedSerialTerminalTransportProvider,
}

impl HostTerminalTransports {
    pub fn factory_for(&self, connection: &HostConnection) -> SharedTerminalTransportFactory {
        match connection {
            HostConnection::Ssh(login) => self.ssh.factory_for(login),
            HostConnection::Telnet(login) => self.telnet.factory_for(login),
            HostConnection::Serial(line) => self.serial.factory_for(line),
        }
    }
}

/// Creates the factory of a local terminal for the shell the settings name
/// and, for a project's terminal, its folder; asked when it
/// opens and when it restarts, so a changed setting reaches the next one.
/// Like the remote provider, the workspace owns this boundary and UI tests
/// inject a fake.
pub trait LocalTerminalTransportProvider: Send + Sync + 'static {
    fn factory_for(&self, launch: &LocalLaunch) -> SharedTerminalTransportFactory;
}

pub type SharedLocalTerminalTransportProvider = Arc<dyn LocalTerminalTransportProvider>;

/// For tests: every local terminal gets the same factory, and the launches
/// asked for are kept.
pub struct FixedLocalTerminalTransportProvider {
    factory: SharedTerminalTransportFactory,
    launches: Mutex<Vec<LocalLaunch>>,
}

impl FixedLocalTerminalTransportProvider {
    pub fn new(factory: SharedTerminalTransportFactory) -> Self {
        Self {
            factory,
            launches: Default::default(),
        }
    }

    /// How each local terminal started, in order.
    pub fn launches(&self) -> Vec<LocalLaunch> {
        self.launches
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// The shell of each local terminal started, in order.
    pub fn shells(&self) -> Vec<LocalShell> {
        self.launches()
            .into_iter()
            .map(|launch| launch.shell)
            .collect()
    }
}

impl LocalTerminalTransportProvider for FixedLocalTerminalTransportProvider {
    fn factory_for(&self, launch: &LocalLaunch) -> SharedTerminalTransportFactory {
        self.launches
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(launch.clone());
        self.factory.clone()
    }
}

/// For tests: every serial terminal gets the same factory, and the lines
/// asked for are kept.
pub struct FixedSerialTerminalTransportProvider {
    factory: SharedTerminalTransportFactory,
    lines: Mutex<Vec<SerialLine>>,
}

impl FixedSerialTerminalTransportProvider {
    pub fn new(factory: SharedTerminalTransportFactory) -> Self {
        Self {
            factory,
            lines: Default::default(),
        }
    }

    /// The line of each serial terminal started, in order.
    pub fn lines(&self) -> Vec<SerialLine> {
        self.lines
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl SerialTerminalTransportProvider for FixedSerialTerminalTransportProvider {
    fn factory_for(&self, line: &SerialLine) -> SharedTerminalTransportFactory {
        self.lines
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(line.clone());
        self.factory.clone()
    }
}

/// Gives every Telnet terminal the same factory, and remembers what each
/// was for.
pub struct FixedTelnetTerminalTransportProvider {
    factory: SharedTerminalTransportFactory,
    logins: Mutex<Vec<TelnetLogin>>,
}

impl FixedTelnetTerminalTransportProvider {
    pub fn new(factory: SharedTerminalTransportFactory) -> Self {
        Self {
            factory,
            logins: Default::default(),
        }
    }

    /// The connection of each Telnet terminal started, in order.
    pub fn logins(&self) -> Vec<TelnetLogin> {
        self.logins
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl TelnetTerminalTransportProvider for FixedTelnetTerminalTransportProvider {
    fn factory_for(&self, login: &TelnetLogin) -> SharedTerminalTransportFactory {
        self.logins
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(login.clone());
        self.factory.clone()
    }
}

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
