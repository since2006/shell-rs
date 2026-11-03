//! The SFTP engine of the SSH core (`shellrs-ssh`) as the app uses it, and
//! what is the app's own: the words for what the engine says (`describe`),
//! this computer's files (`local`), and workers on their own threads,
//! their events worded (`transport`).
mod describe;
mod local;
mod transport;
pub use describe::*;
pub use local::*;
pub use shellrs_ssh::sftp::*;
pub use transport::{
    SftpEvent, SftpRoute, SftpTransport, SftpTransportProvider, SharedSftpTransportProvider,
    SshSftpTransportProvider,
};

/// Why the editor could not open a file, as the app tells it.
pub type ReadFailure = shellrs_ssh::sftp::ReadFailure<String>;
/// Why the editor's save did not happen or finish, as the app tells it.
pub type SaveFailure = shellrs_ssh::sftp::SaveFailure<String>;

#[cfg(all(test, unix))]
mod protocol_tests;
