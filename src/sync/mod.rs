//! 同步: the hosts core's sync (`shellrs_hosts::sync`: a round, the jobs'
//! bodies, the checks of what the user types) as the app uses it, and what
//! is the app's own.
//!
//! [`SyncService`] says when rounds run and runs the jobs that set sync up,
//! change its password and turn it off; the workspace tells it of the
//! moments (starting, the window coming forward, a pause after an edit,
//! 立即同步, quitting) and keeps the location in the settings. The keys stay
//! in the keychain, the state in the database, and the secrets go where the
//! store keeps them. Its dialogs and the words for its errors are here too.

mod describe;
mod remote_dialog;
mod service;
mod sync_dialog;

pub use remote_dialog::{RemoteDialog, RemoteKind, open_remote_dialog};
pub use service::{EDIT_PAUSE, SyncEvent, SyncPhase, SyncService, SyncSnapshot, system_services};
pub use shellrs_hosts::sync::*;
pub use sync_dialog::{JoinReason, PasswordDialog, open_password_dialog, open_turn_off_dialog};
