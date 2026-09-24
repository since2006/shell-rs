//! Shared SSH connection service and remote terminal adapter.
mod connection;
mod probe;
mod tester;
mod transport;
pub use connection::{SshConnectionConfig, SshConnector, SshHandle, SshPrompts};
pub use tester::SshConnectionTester;
pub use transport::SshTerminalTransportProvider;
