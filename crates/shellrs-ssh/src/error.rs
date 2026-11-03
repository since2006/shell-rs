//! Why connecting, logging in or a channel failed: values the app turns
//! into its own words. The `Display` texts here are English for logs, never
//! shown as they are.
//!
//! They are the leaves and the layers of an `anyhow` chain: what lies
//! underneath stays in the chain, so a network failure on the way is still
//! retried and a missing password still recognised.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

/// A typed error over the error that made it happen. anyhow's own `context`
/// hides the context's type from the links of [`anyhow::Error::chain`];
/// this keeps it, so every link can be told by its type ([`link_as`]).
/// It prints as `error` alone, the way a context does.
#[derive(Debug)]
pub struct Wrapped<E> {
    error: E,
    /// Shared, so one cause can be told under several layers.
    cause: Arc<anyhow::Error>,
}

impl<E> Wrapped<E> {
    pub fn error(&self) -> &E {
        &self.error
    }

    /// What lies under the layer.
    pub fn into_cause(self) -> Arc<anyhow::Error> {
        self.cause
    }
}

impl<E: fmt::Display> fmt::Display for Wrapped<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl<E: Error + 'static> Error for Wrapped<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some((*self.cause).as_ref())
    }
}

/// `error` over `cause`.
pub fn wrap<E: Error + Send + Sync + 'static>(
    error: E,
    cause: impl Into<anyhow::Error>,
) -> anyhow::Error {
    wrap_shared(error, Arc::new(cause.into()))
}

/// `error` over a cause that is told elsewhere too.
pub fn wrap_shared<E: Error + Send + Sync + 'static>(
    error: E,
    cause: Arc<anyhow::Error>,
) -> anyhow::Error {
    anyhow::Error::new(Wrapped { error, cause })
}

/// [`wrap`] on a result's error, as anyhow's `Context` does it.
pub trait WrapErr<T> {
    fn wrap_err<E: Error + Send + Sync + 'static>(self, error: E) -> anyhow::Result<T>;
    fn wrap_err_with<E: Error + Send + Sync + 'static>(
        self,
        error: impl FnOnce() -> E,
    ) -> anyhow::Result<T>;
}

impl<T, C: Into<anyhow::Error>> WrapErr<T> for Result<T, C> {
    fn wrap_err<E: Error + Send + Sync + 'static>(self, error: E) -> anyhow::Result<T> {
        self.map_err(|cause| wrap(error, cause))
    }
    fn wrap_err_with<E: Error + Send + Sync + 'static>(
        self,
        error: impl FnOnce() -> E,
    ) -> anyhow::Result<T> {
        self.map_err(|cause| wrap(error(), cause))
    }
}

/// A link of a chain as an `E`: the link itself, or [`Wrapped`] over the
/// rest of the chain.
pub fn link_as<'a, E: Error + 'static>(link: &'a (dyn Error + 'static)) -> Option<&'a E> {
    link.downcast_ref::<E>()
        .or_else(|| link.downcast_ref::<Wrapped<E>>().map(Wrapped::error))
}

/// The first link of `error`'s chain that is an `E`.
pub fn find_cause<E: Error + 'static>(error: &anyhow::Error) -> Option<&E> {
    error.chain().find_map(link_as::<E>)
}

/// A step of connecting that failed, or was called off.
#[derive(Clone, Debug, PartialEq, Eq)]
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

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handshake => f.write_str("couldn't establish an SSH connection"),
            Self::Unreachable { host, port } => write!(f, "couldn't connect to {host}:{port}"),
            Self::Cancelled => f.write_str("connection cancelled"),
            Self::NoJumpHosts => f.write_str("no jump hosts to connect through"),
            Self::ViewClosed => f.write_str("the connection's view was closed"),
            Self::PromptCancelled => f.write_str("authentication request cancelled"),
        }
    }
}

impl Error for ConnectError {}

/// What went wrong with the trust file, ShellRS's own `known_hosts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KnownHostsError {
    ReadFailed {
        path: PathBuf,
    },
    WriteFailed {
        path: PathBuf,
    },
    /// A line that is no host key.
    Corrupt {
        path: PathBuf,
    },
    /// russh could not parse it.
    Unreadable {
        path: PathBuf,
    },
    /// Another connection saved a key for the host while the user decided.
    ChangedWhileSaving,
}

impl fmt::Display for KnownHostsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFailed { path } => {
                write!(f, "couldn't read the known hosts file {}", path.display())
            }
            Self::WriteFailed { path } => {
                write!(f, "couldn't write the known hosts file {}", path.display())
            }
            Self::Corrupt { path } => {
                write!(f, "the known hosts file {} is damaged", path.display())
            }
            Self::Unreadable { path } => {
                write!(f, "the known hosts file {} is unreadable", path.display())
            }
            Self::ChangedWhileSaving => {
                f.write_str("the known hosts file changed while saving the host key")
            }
        }
    }
}

impl Error for KnownHostsError {}

/// Why logging in failed, once connected.
#[derive(Clone, Debug, PartialEq, Eq)]
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
        path: PathBuf,
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

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MethodsUnknown => f.write_str("couldn't ask the server for its login methods"),
            Self::AgentRejected => f.write_str("the server accepted no key in the SSH agent"),
            Self::AgentNoPublicKey => f.write_str("the server doesn't take public keys"),
            Self::RsaHash => f.write_str("couldn't agree on an RSA signature algorithm"),
            Self::AgentHash => f.write_str("couldn't agree on a signature algorithm for the agent"),
            Self::KeyFailed => f.write_str("private key authentication failed"),
            Self::KeyRejected => f.write_str("the server didn't accept the private key"),
            Self::NoKeyAccepted => f.write_str("the server accepted none of the default keys"),
            Self::KeyNotChosen => f.write_str("no private key file chosen"),
            Self::KeyUnreadable { path } => {
                write!(f, "couldn't read the private key file {}", path.display())
            }
            Self::KeyTooManyAttempts => f.write_str("too many incorrect passphrases"),
            Self::PasswordFailed => f.write_str("password authentication failed"),
            Self::Cancelled => f.write_str("authentication cancelled"),
            Self::InvalidReply => f.write_str("invalid authentication reply"),
            Self::KeyboardStartFailed => f.write_str("couldn't start keyboard-interactive login"),
            Self::KeyboardCancelled => f.write_str("keyboard-interactive login cancelled"),
            Self::KeyboardFailed => f.write_str("keyboard-interactive login failed"),
            Self::Failed => f.write_str("the server accepted none of the login methods"),
        }
    }
}

impl Error for AuthError {}

/// Why the SSH agent's keys could not be offered at all. The cause is
/// folded in rather than chained, so a missing agent is never mistaken for
/// a network failure worth retrying.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentProblem {
    Unreachable(AgentUnreachable),
    Empty,
    /// Every key failed to sign: a locked agent, or one that asked to
    /// confirm and was told no.
    Unsigned,
}

/// Why the agent could not be reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentUnreachable {
    /// `SSH_AUTH_SOCK` is not set.
    NoSocket,
    MissingSocket {
        path: PathBuf,
    },
    NoResponse,
    /// Windows' OpenSSH Authentication Agent service is not running.
    ServiceNotRunning,
    /// No agent on this platform.
    Unsupported,
    /// The agent's own words.
    Other(String),
}

impl fmt::Display for AgentProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable(reason) => write!(f, "couldn't connect to the SSH agent: {reason}"),
            Self::Empty => f.write_str("the SSH agent has no keys"),
            Self::Unsigned => f.write_str("the SSH agent refused to sign"),
        }
    }
}

impl fmt::Display for AgentUnreachable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSocket => f.write_str("SSH_AUTH_SOCK isn't set"),
            Self::MissingSocket { path } => write!(f, "{} doesn't exist", path.display()),
            Self::NoResponse => f.write_str("no response"),
            Self::ServiceNotRunning => f.write_str("the agent service isn't running"),
            Self::Unsupported => f.write_str("not supported on this platform"),
            Self::Other(reason) => f.write_str(reason),
        }
    }
}

impl Error for AgentProblem {}

/// What an HTTP or SOCKS5 proxy answered instead of a tunnel. The ones that
/// report the host unreachable come wrapped in the `io::Error` a direct
/// connection would have failed with, so a retry works as it does there.
#[derive(Clone, Debug, PartialEq, Eq)]
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

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadCredentials => f.write_str("the proxy rejected the username or password"),
            Self::AuthRequired => f.write_str("the proxy requires authentication"),
            Self::Forbidden { target } => write!(f, "the proxy doesn't allow {target}"),
            Self::ReplyTooLong => f.write_str("the proxy's reply is too long"),
            Self::Closed => f.write_str("the proxy closed the connection without replying"),
            Self::HttpBadGateway { target } => {
                write!(f, "the proxy couldn't connect to {target} (HTTP 502)")
            }
            Self::HttpGatewayTimeout { target } => {
                write!(f, "the proxy timed out connecting to {target} (HTTP 504)")
            }
            Self::HttpRefused { status } => write!(f, "the proxy refused: {status}"),
            Self::HttpUnrecognized => f.write_str("not an HTTP proxy's reply"),
            Self::SocksUnrecognized => f.write_str("not a SOCKS5 proxy's reply"),
            Self::SocksCredentialsTooLong => f.write_str("proxy username or password too long"),
            Self::SocksNoPasswordAuth => f.write_str("the proxy doesn't take passwords"),
            Self::SocksHostTooLong => f.write_str("hostname too long for SOCKS5"),
            Self::SocksRulesForbid { target } => write!(f, "the proxy's rules forbid {target}"),
            Self::SocksNetworkUnreachable { target } => {
                write!(f, "the proxy can't reach {target}: network unreachable")
            }
            Self::SocksHostUnreachable { target } => {
                write!(f, "the proxy can't reach {target}: host unreachable")
            }
            Self::SocksRefused { target } => {
                write!(f, "the proxy can't reach {target}: connection refused")
            }
            Self::SocksTimeout { target } => write!(f, "the proxy timed out reaching {target}"),
            Self::SocksInternal { target } => {
                write!(f, "the proxy failed internally reaching {target}")
            }
            Self::SocksNoConnect => f.write_str("the proxy doesn't support CONNECT"),
            Self::SocksAddressType => f.write_str("the proxy doesn't support this address type"),
        }
    }
}

impl Error for ProxyError {}

/// Where on the way to a host a connection failed: at a jump host or at the
/// proxy. What went wrong there stays underneath it in the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteFailure {
    /// The jump host at `number` (from 1) was deleted.
    JumpDeleted { number: usize },
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

/// Why a jump host could not open the way onward.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardProblem {
    ConnectFailed,
    Prohibited,
    TimedOut,
    ChannelFailed,
}

impl fmt::Display for RouteFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::JumpDeleted { number } => write!(f, "jump host {number} was deleted"),
            Self::Jump { name } => write!(f, "jump host \u{201c}{name}\u{201d}"),
            Self::JumpForward {
                name,
                host,
                port,
                problem,
            } => {
                let problem = match problem {
                    ForwardProblem::ConnectFailed => "connection failed",
                    ForwardProblem::Prohibited => "port forwarding not allowed",
                    ForwardProblem::TimedOut => "connection timed out",
                    ForwardProblem::ChannelFailed => "no forwarding channel",
                };
                write!(
                    f,
                    "jump host \u{201c}{name}\u{201d} couldn't connect to {host}:{port}: {problem}"
                )
            }
            Self::ProxyUnreachable { host, port } => {
                write!(f, "couldn't connect to the proxy at {host}:{port}")
            }
            Self::Proxy => f.write_str("the proxy"),
        }
    }
}

impl Error for RouteFailure {}

/// A credential authentication needed and could not ask for, because the
/// connection runs without anyone to answer (a connection test, or a command
/// from the external CLI).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

impl fmt::Display for MissingCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Password { rejected: true } => "incorrect username or password",
            Self::Password { rejected: false } => "no password entered",
            Self::Passphrase { rejected: true } => "incorrect private key passphrase",
            Self::Passphrase { rejected: false } => "the private key needs a passphrase",
            Self::KeyboardInteractive => "the server asks keyboard-interactive questions",
        })
    }
}

impl Error for MissingCredential {}

/// A login without a password (「无密码」) that the server wanted a password
/// from. Typed, so that a connection test with the password left empty can
/// say that rather than point at 「无密码」.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PasswordWanted;

impl fmt::Display for PasswordWanted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the server wants a password")
    }
}

impl Error for PasswordWanted {}

/// Why a channel on a live connection failed: the shell's, a command's
/// beside it, or SFTP's.
#[derive(Clone, Debug, PartialEq, Eq)]
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

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SessionFailed => "couldn't open an SSH session channel",
            Self::PtyRefused => "the server refused to create a terminal",
            Self::ShellRefused => "the server refused to start a shell",
            Self::WriteFailed => "couldn't write to the remote terminal",
            Self::ResizeFailed => "couldn't resize the remote terminal",
            Self::ExecChannelFailed => "couldn't open an SSH channel",
            Self::ExecSendFailed => "couldn't send the command",
            Self::ExecRefused => "the server refused to run the command",
            Self::ExecTimeout => "the command timed out",
            Self::SftpRefused => "the server doesn't offer SFTP",
            Self::SftpTimeout => "opening SFTP timed out",
            Self::ShellOnly => "this connection carries the shell only",
            Self::ShellOnlySftp => "this connection carries the shell only, not SFTP",
        })
    }
}

impl Error for ChannelError {}
