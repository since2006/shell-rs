//! Real SSH transport. SFTP intentionally remains a separate mock capability.

mod transport;

pub use transport::SshTerminalTransportProvider;
