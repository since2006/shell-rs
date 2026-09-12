//! Real SSH transport. SFTP intentionally remains a separate mock capability.

mod probe;
mod transport;

pub use transport::SshTerminalTransportProvider;
