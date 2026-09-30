//! Shared SSH connection service and remote terminal adapter.
mod connection;
mod exec;
mod probe;
mod tester;
mod transport;
pub use connection::{
    ForwardedTcpip, MissingCredential, SshConnectionConfig, SshConnector, SshHandle, SshPrompts,
    is_network_error,
};
pub use exec::{ExecError, ExecErrorKind, ExecExit, ExecStream, run_command};
pub use tester::{SshConnectionTester, describe_login_error};
pub use transport::SshTerminalTransportProvider;
