//! Shared SSH connection service and remote terminal adapter.
mod connection;
mod probe;
mod transport;
pub use connection::{SshConnectionConfig, SshConnector, SshHandle, SshPrompts};
pub use transport::SshTerminalTransportProvider;
