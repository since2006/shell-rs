//! Why something failed, link by link and typed, for the app to word.
//!
//! The core's failures are chains: each link says what failed and the
//! next one why, down to the cause. [`Failure`] keeps that shape with each
//! link typed. The enums below mirror the core's; what a later core adds
//! comes as [`FailureLink::Other`] until this interface names it.

use std::error::Error;
use std::fmt;

use shellrs_ssh::{self as ssh, link_as};

/// Why something failed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Failure {
    /// What went wrong, outermost first: each link says what failed and
    /// the next one why. Word each in the user's language and join them,
    /// as the desktop does with ": ".
    pub links: Vec<FailureLink>,
    /// The network failed (refused, unreachable, timed out, dropped):
    /// trying again may work, unlike after a refused login or an untrusted
    /// key.
    pub network: bool,
}

impl Failure {
    /// A failure of logging in or of a channel.
    pub(crate) fn ssh(error: &anyhow::Error) -> Self {
        Self::of(error, ssh::is_network_error(error))
    }

    /// A failure under an SFTP session, whose network failures are its own.
    pub(crate) fn sftp(error: &anyhow::Error) -> Self {
        Self::of(error, ssh::sftp::is_network_error(error))
    }

    /// A connection that dropped on its own.
    pub(crate) fn lost() -> Self {
        Self {
            links: vec![FailureLink::ConnectionLost],
            network: true,
        }
    }

    fn of(error: &anyhow::Error, network: bool) -> Self {
        Self {
            links: error.chain().map(link).collect(),
            network,
        }
    }
}

/// One link of a [`Failure`].
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum FailureLink {
    Connect {
        error: ConnectError,
    },
    KnownHosts {
        error: KnownHostsError,
    },
    Auth {
        error: AuthError,
    },
    Agent {
        error: AgentProblem,
    },
    Proxy {
        error: ProxyError,
    },
    Route {
        error: RouteFailure,
    },
    /// A secret nobody could be asked for.
    MissingCredential {
        need: MissingCredential,
    },
    /// The server wants a password, which a login without one never gives.
    PasswordWanted,
    Channel {
        error: ChannelError,
    },
    /// The connection and the server have no algorithm of `kind` in
    /// common; `theirs` are the server's.
    NoCommonAlgorithm {
        kind: AlgorithmKind,
        theirs: Vec<String>,
    },
    /// Nothing came back in time.
    TimedOut,
    /// The connection dropped, or is not up.
    ConnectionLost,
    /// The system's own error: `kind` for the common ones, `detail` in the
    /// system's words.
    Io {
        kind: IoKind,
        detail: String,
    },
    Sftp {
        error: SftpError,
    },
    Upload {
        error: UploadError,
    },
    Download {
        error: DownloadError,
    },
    /// A resumable transfer's record.
    Journal {
        error: JournalError,
    },
    Edit {
        error: EditError,
    },
    /// The SFTP server refused a request; `message` is the server's own.
    SftpStatus {
        status: SftpStatus,
        message: String,
    },
    /// The user cancelled.
    Cancelled,
    /// A link this interface has no name for, in the core's English.
    Other {
        detail: String,
    },
}

/// One link of a chain.
fn link(cause: &(dyn Error + 'static)) -> FailureLink {
    if let Some(error) = link_as::<ssh::ConnectError>(cause) {
        return FailureLink::Connect {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::KnownHostsError>(cause) {
        return FailureLink::KnownHosts {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::AuthError>(cause) {
        return FailureLink::Auth {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::AgentProblem>(cause) {
        return FailureLink::Agent {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::ProxyError>(cause) {
        return FailureLink::Proxy {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::RouteFailure>(cause) {
        return FailureLink::Route {
            error: error.into(),
        };
    }
    if let Some(need) = link_as::<ssh::MissingCredential>(cause) {
        return FailureLink::MissingCredential { need: need.into() };
    }
    if link_as::<ssh::PasswordWanted>(cause).is_some() {
        return FailureLink::PasswordWanted;
    }
    if let Some(error) = link_as::<ssh::ChannelError>(cause) {
        return FailureLink::Channel {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::sftp::SftpError>(cause) {
        return FailureLink::Sftp {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::sftp::UploadError>(cause) {
        return FailureLink::Upload {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::sftp::DownloadError>(cause) {
        return FailureLink::Download {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::sftp::JournalError>(cause) {
        return FailureLink::Journal {
            error: error.into(),
        };
    }
    if let Some(error) = link_as::<ssh::sftp::EditError>(cause) {
        return FailureLink::Edit {
            error: error.into(),
        };
    }
    if cause.is::<ssh::sftp::Cancelled>() {
        return FailureLink::Cancelled;
    }
    if cause.is::<ssh::sftp::TerminalNotConnected>() {
        return FailureLink::ConnectionLost;
    }
    if let Some(io) = cause.downcast_ref::<std::io::Error>() {
        return io_link(io);
    }
    if let Some(error) = cause.downcast_ref::<russh::Error>() {
        return match error {
            russh::Error::IO(io) => io_link(io),
            russh::Error::ConnectionTimeout
            | russh::Error::KeepaliveTimeout
            | russh::Error::InactivityTimeout => FailureLink::TimedOut,
            russh::Error::Disconnect | russh::Error::HUP => FailureLink::ConnectionLost,
            russh::Error::NoCommonAlgo { kind, theirs, .. } => FailureLink::NoCommonAlgorithm {
                kind: kind.into(),
                theirs: theirs.clone(),
            },
            _ => other(cause),
        };
    }
    if let Some(error) = cause.downcast_ref::<russh_sftp::client::error::Error>() {
        use russh_sftp::client::error::Error as SftpClientError;
        return match error {
            SftpClientError::Status(status) => FailureLink::SftpStatus {
                status: status.status_code.into(),
                message: status.error_message.clone(),
            },
            SftpClientError::Timeout => FailureLink::TimedOut,
            _ => other(cause),
        };
    }
    other(cause)
}

/// An I/O error: the link the core put in it, or the system's own.
fn io_link(io: &std::io::Error) -> FailureLink {
    if let Some(inner) = io.get_ref() {
        return link(inner);
    }
    use std::io::ErrorKind;
    let kind = match io.kind() {
        ErrorKind::ConnectionRefused => IoKind::ConnectionRefused,
        ErrorKind::ConnectionReset => IoKind::ConnectionReset,
        ErrorKind::ConnectionAborted => IoKind::ConnectionAborted,
        ErrorKind::NotConnected => IoKind::NotConnected,
        ErrorKind::TimedOut => IoKind::TimedOut,
        ErrorKind::HostUnreachable => IoKind::HostUnreachable,
        ErrorKind::NetworkUnreachable => IoKind::NetworkUnreachable,
        ErrorKind::NotFound => IoKind::NotFound,
        ErrorKind::PermissionDenied => IoKind::PermissionDenied,
        ErrorKind::AlreadyExists => IoKind::AlreadyExists,
        ErrorKind::StorageFull => IoKind::StorageFull,
        ErrorKind::UnexpectedEof | ErrorKind::BrokenPipe => IoKind::Closed,
        _ => IoKind::Other,
    };
    FailureLink::Io {
        kind,
        detail: io.to_string(),
    }
}

fn other(cause: &(dyn Error + 'static)) -> FailureLink {
    FailureLink::Other {
        detail: cause.to_string(),
    }
}

/// What a call failed with.
#[derive(Debug, uniffi::Error)]
pub enum SshError {
    /// The connection is not up: still logging in, or closed.
    NotConnected,
    /// The terminal or session has been closed.
    Closed,
    Failed {
        failure: Failure,
    },
    /// What should never happen: a bug in ShellRS.
    Internal {
        detail: String,
    },
}

impl SshError {
    pub(crate) fn ssh(error: &anyhow::Error) -> Self {
        Self::Failed {
            failure: Failure::ssh(error),
        }
    }
}

impl fmt::Display for SshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConnected => f.write_str("not connected"),
            Self::Closed => f.write_str("closed"),
            Self::Failed { failure } => write!(f, "failed: {:?}", failure.links),
            Self::Internal { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

impl Error for SshError {}

/// Kinds of algorithms a connection agrees on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AlgorithmKind {
    KeyExchange,
    HostKey,
    Cipher,
    Mac,
    Compression,
}

impl From<&russh::AlgorithmKind> for AlgorithmKind {
    fn from(kind: &russh::AlgorithmKind) -> Self {
        match kind {
            russh::AlgorithmKind::Kex => Self::KeyExchange,
            russh::AlgorithmKind::Key => Self::HostKey,
            russh::AlgorithmKind::Cipher => Self::Cipher,
            russh::AlgorithmKind::Mac => Self::Mac,
            russh::AlgorithmKind::Compression => Self::Compression,
        }
    }
}

/// The system errors worth their own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum IoKind {
    ConnectionRefused,
    ConnectionReset,
    ConnectionAborted,
    NotConnected,
    TimedOut,
    HostUnreachable,
    NetworkUnreachable,
    NotFound,
    PermissionDenied,
    AlreadyExists,
    StorageFull,
    /// The other end closed the stream.
    Closed,
    Other,
}

/// What an SFTP server answered a request with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SftpStatus {
    Eof,
    NoSuchFile,
    PermissionDenied,
    Failure,
    BadMessage,
    NoConnection,
    ConnectionLost,
    OpUnsupported,
}

impl From<russh_sftp::protocol::StatusCode> for SftpStatus {
    fn from(code: russh_sftp::protocol::StatusCode) -> Self {
        use russh_sftp::protocol::StatusCode;
        match code {
            StatusCode::Eof => Self::Eof,
            StatusCode::NoSuchFile => Self::NoSuchFile,
            StatusCode::PermissionDenied => Self::PermissionDenied,
            // An error that says it is fine is a failure all the same.
            StatusCode::Ok | StatusCode::Failure => Self::Failure,
            StatusCode::BadMessage => Self::BadMessage,
            StatusCode::NoConnection => Self::NoConnection,
            StatusCode::ConnectionLost => Self::ConnectionLost,
            StatusCode::OpUnsupported => Self::OpUnsupported,
        }
    }
}

/// A step of connecting that failed, or was called off.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectError {
    /// Over the error the SSH handshake failed with.
    Handshake,
    /// Over the socket's error.
    Unreachable {
        host: String,
        port: u16,
    },
    Cancelled,
    /// The jump host list came up empty.
    NoJumpHosts,
    /// The question's tab or dialog was closed before it could be asked.
    ViewClosed,
    /// The question was dropped unanswered.
    PromptCancelled,
}

impl From<&ssh::ConnectError> for ConnectError {
    fn from(error: &ssh::ConnectError) -> Self {
        match error {
            ssh::ConnectError::Handshake => Self::Handshake,
            ssh::ConnectError::Unreachable { host, port } => Self::Unreachable {
                host: host.clone(),
                port: *port,
            },
            ssh::ConnectError::Cancelled => Self::Cancelled,
            ssh::ConnectError::NoJumpHosts => Self::NoJumpHosts,
            ssh::ConnectError::ViewClosed => Self::ViewClosed,
            ssh::ConnectError::PromptCancelled => Self::PromptCancelled,
        }
    }
}

/// What went wrong with the trust file, ShellRS's own `known_hosts`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum KnownHostsError {
    ReadFailed {
        path: String,
    },
    WriteFailed {
        path: String,
    },
    /// A line that is no host key.
    Corrupt {
        path: String,
    },
    /// russh could not parse it.
    Unreadable {
        path: String,
    },
    /// Another connection saved a key for the host while the user decided.
    ChangedWhileSaving,
}

impl From<&ssh::KnownHostsError> for KnownHostsError {
    fn from(error: &ssh::KnownHostsError) -> Self {
        match error {
            ssh::KnownHostsError::ReadFailed { path } => Self::ReadFailed {
                path: path.display().to_string(),
            },
            ssh::KnownHostsError::WriteFailed { path } => Self::WriteFailed {
                path: path.display().to_string(),
            },
            ssh::KnownHostsError::Corrupt { path } => Self::Corrupt {
                path: path.display().to_string(),
            },
            ssh::KnownHostsError::Unreadable { path } => Self::Unreadable {
                path: path.display().to_string(),
            },
            ssh::KnownHostsError::ChangedWhileSaving => Self::ChangedWhileSaving,
        }
    }
}

/// Why logging in failed, once connected.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AuthError {
    MethodsUnknown,
    AgentRejected,
    AgentNoPublicKey,
    RsaHash,
    AgentHash,
    KeyFailed,
    KeyRejected,
    NoKeyAccepted,
    KeyNotChosen,
    KeyUnreadable {
        path: String,
    },
    KeyTooManyAttempts,
    PasswordFailed,
    Cancelled,
    InvalidReply,
    KeyboardStartFailed,
    KeyboardCancelled,
    KeyboardFailed,
    /// The server accepted none of the methods tried.
    Failed,
}

impl From<&ssh::AuthError> for AuthError {
    fn from(error: &ssh::AuthError) -> Self {
        match error {
            ssh::AuthError::MethodsUnknown => Self::MethodsUnknown,
            ssh::AuthError::AgentRejected => Self::AgentRejected,
            ssh::AuthError::AgentNoPublicKey => Self::AgentNoPublicKey,
            ssh::AuthError::RsaHash => Self::RsaHash,
            ssh::AuthError::AgentHash => Self::AgentHash,
            ssh::AuthError::KeyFailed => Self::KeyFailed,
            ssh::AuthError::KeyRejected => Self::KeyRejected,
            ssh::AuthError::NoKeyAccepted => Self::NoKeyAccepted,
            ssh::AuthError::KeyNotChosen => Self::KeyNotChosen,
            ssh::AuthError::KeyUnreadable { path } => Self::KeyUnreadable {
                path: path.display().to_string(),
            },
            ssh::AuthError::KeyTooManyAttempts => Self::KeyTooManyAttempts,
            ssh::AuthError::PasswordFailed => Self::PasswordFailed,
            ssh::AuthError::Cancelled => Self::Cancelled,
            ssh::AuthError::InvalidReply => Self::InvalidReply,
            ssh::AuthError::KeyboardStartFailed => Self::KeyboardStartFailed,
            ssh::AuthError::KeyboardCancelled => Self::KeyboardCancelled,
            ssh::AuthError::KeyboardFailed => Self::KeyboardFailed,
            ssh::AuthError::Failed => Self::Failed,
        }
    }
}

/// Why the SSH agent's keys could not be offered at all. The cause is
/// folded in rather than chained, so a missing agent is never mistaken for
/// a network failure worth retrying.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentProblem {
    Unreachable {
        reason: AgentUnreachable,
    },
    Empty,
    /// Every key failed to sign: a locked agent, or one that asked to
    /// confirm and was told no.
    Unsigned,
}

impl From<&ssh::AgentProblem> for AgentProblem {
    fn from(error: &ssh::AgentProblem) -> Self {
        match error {
            ssh::AgentProblem::Unreachable(value) => Self::Unreachable {
                reason: value.into(),
            },
            ssh::AgentProblem::Empty => Self::Empty,
            ssh::AgentProblem::Unsigned => Self::Unsigned,
        }
    }
}

/// Why the agent could not be reached.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AgentUnreachable {
    /// `SSH_AUTH_SOCK` is not set.
    NoSocket,
    MissingSocket {
        path: String,
    },
    NoResponse,
    /// Windows' OpenSSH Authentication Agent service is not running.
    ServiceNotRunning,
    /// No agent on this platform.
    Unsupported,
    /// The agent's own words.
    Other {
        detail: String,
    },
}

impl From<&ssh::AgentUnreachable> for AgentUnreachable {
    fn from(error: &ssh::AgentUnreachable) -> Self {
        match error {
            ssh::AgentUnreachable::NoSocket => Self::NoSocket,
            ssh::AgentUnreachable::MissingSocket { path } => Self::MissingSocket {
                path: path.display().to_string(),
            },
            ssh::AgentUnreachable::NoResponse => Self::NoResponse,
            ssh::AgentUnreachable::ServiceNotRunning => Self::ServiceNotRunning,
            ssh::AgentUnreachable::Unsupported => Self::Unsupported,
            ssh::AgentUnreachable::Other(value) => Self::Other {
                detail: value.clone(),
            },
        }
    }
}

/// What an HTTP or SOCKS5 proxy answered instead of a tunnel. The ones that
/// report the host unreachable come wrapped in the `io::Error` a direct
/// connection would have failed with, so a retry works as it does there.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ProxyError {
    BadCredentials,
    AuthRequired,
    Forbidden {
        target: String,
    },
    ReplyTooLong,
    Closed,
    HttpBadGateway {
        target: String,
    },
    HttpGatewayTimeout {
        target: String,
    },
    /// Any other status, as its status line.
    HttpRefused {
        status: String,
    },
    HttpUnrecognized,
    SocksUnrecognized,
    SocksCredentialsTooLong,
    SocksNoPasswordAuth,
    SocksHostTooLong,
    SocksRulesForbid {
        target: String,
    },
    SocksNetworkUnreachable {
        target: String,
    },
    SocksHostUnreachable {
        target: String,
    },
    SocksRefused {
        target: String,
    },
    SocksTimeout {
        target: String,
    },
    SocksInternal {
        target: String,
    },
    SocksNoConnect,
    SocksAddressType,
}

impl From<&ssh::ProxyError> for ProxyError {
    fn from(error: &ssh::ProxyError) -> Self {
        match error {
            ssh::ProxyError::BadCredentials => Self::BadCredentials,
            ssh::ProxyError::AuthRequired => Self::AuthRequired,
            ssh::ProxyError::Forbidden { target } => Self::Forbidden {
                target: target.clone(),
            },
            ssh::ProxyError::ReplyTooLong => Self::ReplyTooLong,
            ssh::ProxyError::Closed => Self::Closed,
            ssh::ProxyError::HttpBadGateway { target } => Self::HttpBadGateway {
                target: target.clone(),
            },
            ssh::ProxyError::HttpGatewayTimeout { target } => Self::HttpGatewayTimeout {
                target: target.clone(),
            },
            ssh::ProxyError::HttpRefused { status } => Self::HttpRefused {
                status: status.clone(),
            },
            ssh::ProxyError::HttpUnrecognized => Self::HttpUnrecognized,
            ssh::ProxyError::SocksUnrecognized => Self::SocksUnrecognized,
            ssh::ProxyError::SocksCredentialsTooLong => Self::SocksCredentialsTooLong,
            ssh::ProxyError::SocksNoPasswordAuth => Self::SocksNoPasswordAuth,
            ssh::ProxyError::SocksHostTooLong => Self::SocksHostTooLong,
            ssh::ProxyError::SocksRulesForbid { target } => Self::SocksRulesForbid {
                target: target.clone(),
            },
            ssh::ProxyError::SocksNetworkUnreachable { target } => Self::SocksNetworkUnreachable {
                target: target.clone(),
            },
            ssh::ProxyError::SocksHostUnreachable { target } => Self::SocksHostUnreachable {
                target: target.clone(),
            },
            ssh::ProxyError::SocksRefused { target } => Self::SocksRefused {
                target: target.clone(),
            },
            ssh::ProxyError::SocksTimeout { target } => Self::SocksTimeout {
                target: target.clone(),
            },
            ssh::ProxyError::SocksInternal { target } => Self::SocksInternal {
                target: target.clone(),
            },
            ssh::ProxyError::SocksNoConnect => Self::SocksNoConnect,
            ssh::ProxyError::SocksAddressType => Self::SocksAddressType,
        }
    }
}

/// Where on the way to a host a connection failed: at a jump host or at the
/// proxy. What went wrong there stays underneath it in the chain.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RouteFailure {
    /// The jump host at `number` (from 1) was deleted.
    JumpDeleted { number: u64 },
    /// Logging in to the jump host `name` failed.
    Jump { name: String },
    /// The jump host `name` could not reach the next one.
    JumpForward {
        name: String,
        host: String,
        port: u16,
        problem: ForwardProblem,
    },
    /// The proxy itself could not be reached.
    ProxyUnreachable { host: String, port: u16 },
    /// The proxy answered other than with a tunnel.
    Proxy,
}

impl From<&ssh::RouteFailure> for RouteFailure {
    fn from(error: &ssh::RouteFailure) -> Self {
        match error {
            ssh::RouteFailure::JumpDeleted { number } => Self::JumpDeleted {
                number: *number as u64,
            },
            ssh::RouteFailure::Jump { name } => Self::Jump { name: name.clone() },
            ssh::RouteFailure::JumpForward {
                name,
                host,
                port,
                problem,
            } => Self::JumpForward {
                name: name.clone(),
                host: host.clone(),
                port: *port,
                problem: problem.into(),
            },
            ssh::RouteFailure::ProxyUnreachable { host, port } => Self::ProxyUnreachable {
                host: host.clone(),
                port: *port,
            },
            ssh::RouteFailure::Proxy => Self::Proxy,
        }
    }
}

/// Why a jump host could not open the way onward.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ForwardProblem {
    ConnectFailed,
    Prohibited,
    TimedOut,
    ChannelFailed,
}

impl From<&ssh::ForwardProblem> for ForwardProblem {
    fn from(error: &ssh::ForwardProblem) -> Self {
        match error {
            ssh::ForwardProblem::ConnectFailed => Self::ConnectFailed,
            ssh::ForwardProblem::Prohibited => Self::Prohibited,
            ssh::ForwardProblem::TimedOut => Self::TimedOut,
            ssh::ForwardProblem::ChannelFailed => Self::ChannelFailed,
        }
    }
}

/// A credential authentication needed and could not ask for, because the
/// connection runs without anyone to answer (a connection test, or a command
/// from the external CLI).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MissingCredential {
    /// `rejected` when a password was tried and the server refused it.
    Password {
        rejected: bool,
    },
    /// `rejected` when a passphrase was tried and did not unlock the key.
    Passphrase {
        rejected: bool,
    },
    KeyboardInteractive,
}

impl From<&ssh::MissingCredential> for MissingCredential {
    fn from(error: &ssh::MissingCredential) -> Self {
        match error {
            ssh::MissingCredential::Password { rejected } => Self::Password {
                rejected: *rejected,
            },
            ssh::MissingCredential::Passphrase { rejected } => Self::Passphrase {
                rejected: *rejected,
            },
            ssh::MissingCredential::KeyboardInteractive => Self::KeyboardInteractive,
        }
    }
}

/// Why a channel on a live connection failed: the shell's, a command's
/// beside it, or SFTP's.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChannelError {
    SessionFailed,
    PtyRefused,
    ShellRefused,
    WriteFailed,
    ResizeFailed,
    ExecChannelFailed,
    ExecSendFailed,
    ExecRefused,
    ExecTimeout,
    SftpRefused,
    SftpTimeout,
    /// A bastion host's link carries the shell and nothing else.
    ShellOnly,
    ShellOnlySftp,
}

impl From<&ssh::ChannelError> for ChannelError {
    fn from(error: &ssh::ChannelError) -> Self {
        match error {
            ssh::ChannelError::SessionFailed => Self::SessionFailed,
            ssh::ChannelError::PtyRefused => Self::PtyRefused,
            ssh::ChannelError::ShellRefused => Self::ShellRefused,
            ssh::ChannelError::WriteFailed => Self::WriteFailed,
            ssh::ChannelError::ResizeFailed => Self::ResizeFailed,
            ssh::ChannelError::ExecChannelFailed => Self::ExecChannelFailed,
            ssh::ChannelError::ExecSendFailed => Self::ExecSendFailed,
            ssh::ChannelError::ExecRefused => Self::ExecRefused,
            ssh::ChannelError::ExecTimeout => Self::ExecTimeout,
            ssh::ChannelError::SftpRefused => Self::SftpRefused,
            ssh::ChannelError::SftpTimeout => Self::SftpTimeout,
            ssh::ChannelError::ShellOnly => Self::ShellOnly,
            ssh::ChannelError::ShellOnlySftp => Self::ShellOnlySftp,
        }
    }
}

/// A browsing, connection or batch failure, and what a batch notes.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SftpError {
    StartTimeout,
    NoV3,
    NoPath,
    NoLinkTarget,
    /// A remote name decoded lossily: using it could mean another file.
    AmbiguousName,
    RemotePathInvalid,
    FileNameInvalid {
        name: String,
    },
    NothingToUpload,
    NothingToDownload,
    DownloadTargetRelative,
    FileNameNotUtf8,
    Exists {
        name: String,
    },
    /// A resumed transfer met a server whose key changed.
    HostKeyChanged,
    /// A request came while there was no connection.
    NotConnected,
    /// A batch went on while there was no connection.
    NoConnection,
    /// Over the error reading the folder failed with.
    ListFailed,
    /// A batch came while another ran.
    Busy,
    LocalFolderMissing {
        path: String,
    },
    RemoteFolderMissing {
        path: String,
    },
    NotAFolder {
        path: String,
    },
    SyncToRoot,
    /// A dropped batch waits `seconds` before try `attempt` of 3.
    ReconnectIn {
        seconds: u64,
        attempt: u64,
    },
    /// Over the error a sync's delete failed with.
    DeleteFailed,
}

impl From<&ssh::sftp::SftpError> for SftpError {
    fn from(error: &ssh::sftp::SftpError) -> Self {
        match error {
            ssh::sftp::SftpError::StartTimeout => Self::StartTimeout,
            ssh::sftp::SftpError::NoV3 => Self::NoV3,
            ssh::sftp::SftpError::NoPath => Self::NoPath,
            ssh::sftp::SftpError::NoLinkTarget => Self::NoLinkTarget,
            ssh::sftp::SftpError::AmbiguousName => Self::AmbiguousName,
            ssh::sftp::SftpError::RemotePathInvalid => Self::RemotePathInvalid,
            ssh::sftp::SftpError::FileNameInvalid { name } => {
                Self::FileNameInvalid { name: name.clone() }
            }
            ssh::sftp::SftpError::NothingToUpload => Self::NothingToUpload,
            ssh::sftp::SftpError::NothingToDownload => Self::NothingToDownload,
            ssh::sftp::SftpError::DownloadTargetRelative => Self::DownloadTargetRelative,
            ssh::sftp::SftpError::FileNameNotUtf8 => Self::FileNameNotUtf8,
            ssh::sftp::SftpError::Exists { name } => Self::Exists { name: name.clone() },
            ssh::sftp::SftpError::HostKeyChanged => Self::HostKeyChanged,
            ssh::sftp::SftpError::NotConnected => Self::NotConnected,
            ssh::sftp::SftpError::NoConnection => Self::NoConnection,
            ssh::sftp::SftpError::ListFailed => Self::ListFailed,
            ssh::sftp::SftpError::Busy => Self::Busy,
            ssh::sftp::SftpError::LocalFolderMissing { path } => Self::LocalFolderMissing {
                path: path.display().to_string(),
            },
            ssh::sftp::SftpError::RemoteFolderMissing { path } => {
                Self::RemoteFolderMissing { path: path.clone() }
            }
            ssh::sftp::SftpError::NotAFolder { path } => Self::NotAFolder { path: path.clone() },
            ssh::sftp::SftpError::SyncToRoot => Self::SyncToRoot,
            ssh::sftp::SftpError::ReconnectIn { seconds, attempt } => Self::ReconnectIn {
                seconds: *seconds,
                attempt: *attempt as u64,
            },
            ssh::sftp::SftpError::DeleteFailed => Self::DeleteFailed,
        }
    }
}

/// An upload's failures, and what it asks.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum UploadError {
    SourceNameNotUtf8,
    /// Another transfer writes the same target.
    TargetBusy,
    ParentMissing,
    /// Over the error the item failed with.
    Failed,
    FolderInTheWay,
    /// Asked before replacing what is there; `modified` in Unix seconds.
    Exists {
        size: u64,
        modified: Option<u64>,
    },
    /// Asked again: the target changed while the item went up.
    ExistsChanged {
        size: u64,
        modified: Option<u64>,
    },
    TargetGone,
    SourceNowFolder,
    TargetNotFolder,
    SpecialFile,
    ResumeFound,
    LinkTargetNotUtf8,
    RecordWithoutLink,
    TemporaryLinkChanged,
    RecordWithoutSource,
    TemporaryKindChanged,
    PartialTooLarge,
    PipelineEnded,
    PublishUnconfirmed,
    TemporaryChanged,
    TargetCreated,
    BackupChanged,
    BackupChangedAfterPublish,
    BackupAndTarget,
}

impl From<&ssh::sftp::UploadError> for UploadError {
    fn from(error: &ssh::sftp::UploadError) -> Self {
        match error {
            ssh::sftp::UploadError::SourceNameNotUtf8 => Self::SourceNameNotUtf8,
            ssh::sftp::UploadError::TargetBusy => Self::TargetBusy,
            ssh::sftp::UploadError::ParentMissing => Self::ParentMissing,
            ssh::sftp::UploadError::Failed => Self::Failed,
            ssh::sftp::UploadError::FolderInTheWay => Self::FolderInTheWay,
            ssh::sftp::UploadError::Exists { size, modified } => Self::Exists {
                size: *size,
                modified: *modified,
            },
            ssh::sftp::UploadError::ExistsChanged { size, modified } => Self::ExistsChanged {
                size: *size,
                modified: *modified,
            },
            ssh::sftp::UploadError::TargetGone => Self::TargetGone,
            ssh::sftp::UploadError::SourceNowFolder => Self::SourceNowFolder,
            ssh::sftp::UploadError::TargetNotFolder => Self::TargetNotFolder,
            ssh::sftp::UploadError::SpecialFile => Self::SpecialFile,
            ssh::sftp::UploadError::ResumeFound => Self::ResumeFound,
            ssh::sftp::UploadError::LinkTargetNotUtf8 => Self::LinkTargetNotUtf8,
            ssh::sftp::UploadError::RecordWithoutLink => Self::RecordWithoutLink,
            ssh::sftp::UploadError::TemporaryLinkChanged => Self::TemporaryLinkChanged,
            ssh::sftp::UploadError::RecordWithoutSource => Self::RecordWithoutSource,
            ssh::sftp::UploadError::TemporaryKindChanged => Self::TemporaryKindChanged,
            ssh::sftp::UploadError::PartialTooLarge => Self::PartialTooLarge,
            ssh::sftp::UploadError::PipelineEnded => Self::PipelineEnded,
            ssh::sftp::UploadError::PublishUnconfirmed => Self::PublishUnconfirmed,
            ssh::sftp::UploadError::TemporaryChanged => Self::TemporaryChanged,
            ssh::sftp::UploadError::TargetCreated => Self::TargetCreated,
            ssh::sftp::UploadError::BackupChanged => Self::BackupChanged,
            ssh::sftp::UploadError::BackupChangedAfterPublish => Self::BackupChangedAfterPublish,
            ssh::sftp::UploadError::BackupAndTarget => Self::BackupAndTarget,
        }
    }
}

/// A download's failures, and what it asks.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DownloadError {
    NameInvalid {
        name: String,
    },
    SourceMissing,
    /// Another transfer writes the same target.
    TargetBusy,
    ParentMissing,
    /// Over the error the item failed with.
    Failed,
    TargetInvalid,
    FolderInTheWay,
    /// Asked before replacing what is there; `modified` in Unix seconds.
    Exists {
        size: u64,
        modified: Option<u64>,
    },
    /// Asked again: the target changed while the item came down.
    ExistsChanged {
        size: u64,
        modified: Option<u64>,
    },
    FileInTheWay,
    SpecialFile,
    PartialNotFile {
        path: String,
    },
    PartialTooLarge,
    SourceChanged,
    ResumeFound,
    PipelineEnded,
    SourceShrank,
    LinksUnsupported,
}

impl From<&ssh::sftp::DownloadError> for DownloadError {
    fn from(error: &ssh::sftp::DownloadError) -> Self {
        match error {
            ssh::sftp::DownloadError::NameInvalid { name } => {
                Self::NameInvalid { name: name.clone() }
            }
            ssh::sftp::DownloadError::SourceMissing => Self::SourceMissing,
            ssh::sftp::DownloadError::TargetBusy => Self::TargetBusy,
            ssh::sftp::DownloadError::ParentMissing => Self::ParentMissing,
            ssh::sftp::DownloadError::Failed => Self::Failed,
            ssh::sftp::DownloadError::TargetInvalid => Self::TargetInvalid,
            ssh::sftp::DownloadError::FolderInTheWay => Self::FolderInTheWay,
            ssh::sftp::DownloadError::Exists { size, modified } => Self::Exists {
                size: *size,
                modified: *modified,
            },
            ssh::sftp::DownloadError::ExistsChanged { size, modified } => Self::ExistsChanged {
                size: *size,
                modified: *modified,
            },
            ssh::sftp::DownloadError::FileInTheWay => Self::FileInTheWay,
            ssh::sftp::DownloadError::SpecialFile => Self::SpecialFile,
            ssh::sftp::DownloadError::PartialNotFile { path } => Self::PartialNotFile {
                path: path.display().to_string(),
            },
            ssh::sftp::DownloadError::PartialTooLarge => Self::PartialTooLarge,
            ssh::sftp::DownloadError::SourceChanged => Self::SourceChanged,
            ssh::sftp::DownloadError::ResumeFound => Self::ResumeFound,
            ssh::sftp::DownloadError::PipelineEnded => Self::PipelineEnded,
            ssh::sftp::DownloadError::SourceShrank => Self::SourceShrank,
            ssh::sftp::DownloadError::LinksUnsupported => Self::LinksUnsupported,
        }
    }
}

/// What is wrong with a resume record.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum JournalError {
    SourceNotFile,
    Mismatch,
    TemporaryInvalid,
    ReplacesFolder,
    /// Over the parse error of an upload's record.
    UploadDamaged,
    /// Over the parse error of a download's record.
    DownloadDamaged,
    /// Over the error reading a record failed with.
    Unreadable,
    /// Over the error saving a record failed with.
    Unsaved,
}

impl From<&ssh::sftp::JournalError> for JournalError {
    fn from(error: &ssh::sftp::JournalError) -> Self {
        match error {
            ssh::sftp::JournalError::SourceNotFile => Self::SourceNotFile,
            ssh::sftp::JournalError::Mismatch => Self::Mismatch,
            ssh::sftp::JournalError::TemporaryInvalid => Self::TemporaryInvalid,
            ssh::sftp::JournalError::ReplacesFolder => Self::ReplacesFolder,
            ssh::sftp::JournalError::UploadDamaged => Self::UploadDamaged,
            ssh::sftp::JournalError::DownloadDamaged => Self::DownloadDamaged,
            ssh::sftp::JournalError::Unreadable => Self::Unreadable,
            ssh::sftp::JournalError::Unsaved => Self::Unsaved,
        }
    }
}

/// Why the editor's read or save of a remote file failed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum EditError {
    Missing,
    Shrank,
    GoneAfterSave,
    /// Over the error a write failed with after the file was truncated.
    Interrupted,
}

impl From<&ssh::sftp::EditError> for EditError {
    fn from(error: &ssh::sftp::EditError) -> Self {
        match error {
            ssh::sftp::EditError::Missing => Self::Missing,
            ssh::sftp::EditError::Shrank => Self::Shrank,
            ssh::sftp::EditError::GoneAfterSave => Self::GoneAfterSave,
            ssh::sftp::EditError::Interrupted => Self::Interrupted,
        }
    }
}
