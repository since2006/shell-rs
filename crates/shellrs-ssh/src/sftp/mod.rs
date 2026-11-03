//! The SFTP engine: one worker per session lists and changes remote files,
//! reads and saves them for an editor, and transfers them in batches,
//! several small files at once, resuming what a lost connection cut off.
//!
//! It sends only the requests WinSCP sends (no extensions, no `fsetstat`):
//! bastion hosts end the session on others. What happens comes out as
//! [`EngineEvent`]s through an [`EventSink`], with failures typed
//! ([`Reason`]); the app words them.
mod client;
mod control;
mod download;
mod edit;
mod engine;
mod error;
mod journal;
mod meter;
mod model;
mod operations;
mod parallel;
mod speed;
mod sync;
mod upload;
mod worker;
pub use client::{TerminalNotConnected, is_network_error};
pub use control::Cancelled;
pub use edit::{EDIT_LIMIT, FileBytes, FileStamp, ReadFailure, SaveFailure, TextFile, TextFormat};
pub use engine::{
    Disconnect, EngineEvent, EventSink, EventsClosed, RemoteOperation, SftpChannelOpener,
    SftpCommand, SftpRoute,
};
pub use error::*;
pub use model::*;
pub use parallel::{DEFAULT_PARALLEL_TRANSFERS, PARALLEL_TRANSFERS_RANGE, set_parallel_transfers};
pub use worker::SftpWorker;

#[cfg(test)]
mod tests;
