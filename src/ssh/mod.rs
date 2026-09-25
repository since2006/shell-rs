//! Shared SSH connection service and remote terminal adapter.
mod connection;
mod exec;
mod probe;
mod tester;
mod transport;
pub use connection::{SshConnectionConfig, SshConnector, SshHandle, SshPrompts};
pub use exec::{ExecError, ExecErrorKind, ExecExit, ExecStream, run_command};
pub use tester::SshConnectionTester;
pub use transport::SshTerminalTransportProvider;
