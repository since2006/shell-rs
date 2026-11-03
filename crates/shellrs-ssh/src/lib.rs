//! ShellRS's SSH core: logging in (host keys, passwords, keys, the agent,
//! jump hosts and proxies), the channels of a live connection (a shell on
//! a pseudo-terminal, commands, SFTP) and the SFTP transfer engine.
//!
//! It shows nothing and says nothing in words: questions for the user come
//! out as typed prompts, failures as typed errors in `anyhow` chains, and
//! the app (the desktop app, the mobile apps through their bindings) words
//! them in its own language. Secrets are read through [`SecretSource`],
//! only when a server asks for one, so each app keeps them in its own
//! keychain.

mod connection;
mod error;
mod exec;
mod latency;
mod login;
#[cfg(target_vendor = "apple")]
mod nat64;
mod probe;
mod prompt;
mod proxy;
mod secret;
mod session;
pub mod sftp;
#[cfg(feature = "testing")]
pub mod testing;

pub use connection::{
    AgentLocation, ForwardedTcpip, SshConnectionConfig, SshConnector, SshHandle, SshPrompts,
    Tunnel, is_network_error, timeout_excluding_prompts,
};
pub use error::{
    AgentProblem, AgentUnreachable, AuthError, ChannelError, ConnectError, ForwardProblem,
    KnownHostsError, MissingCredential, PasswordWanted, ProxyError, RouteFailure, WrapErr, Wrapped,
    find_cause, link_as, wrap, wrap_shared,
};
pub use exec::{ExecError, ExecErrorKind, ExecExit, ExecFailure, ExecStream, run_command};
pub use latency::{LATENCY_INTERVAL, Latency, LatencyLevel, round_trip};
pub use login::{
    DEFAULT_KEY_FILES, HostLogin, JumpLogin, LoginMethod, LoginRoute, ProxyKind, ProxyLogin,
};
pub use probe::{HOST_OS_KEYS, HostOsProbe, PROBE_COMMAND, ProbeOutcome, WINDOWS_PROBE_COMMAND};
pub use prompt::{
    AuthenticationPrompt, AuthenticationRequest, ConnectionPrompt, ConnectionPromptField,
    ConnectionPromptKind, ConnectionPromptReply, ConnectionSecret, HostKeyChangedPrompt,
    UnknownHostPrompt,
};
pub use secret::{SecretRef, SecretSource, SharedSecretSource, read_secret};
pub use session::{
    ChannelStream, PtySize, SftpChannel, open_probe, open_sftp_channel, open_shell, resize_pty,
    run_exec, write_shell,
};
