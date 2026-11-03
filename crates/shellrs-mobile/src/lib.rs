//! ShellRS for the mobile apps: the SSH core (`shellrs-ssh`) and the hosts
//! core (`shellrs-hosts`) behind an interface Swift and Kotlin bindings are
//! generated from (UniFFI). This is the one interface the apps rely on,
//! kept stable; the cores under it change as the desktop app needs.
//!
//! A [`Core`] per data folder holds the runtime everything runs on, the
//! saved hosts ([`HostStore`]) and sync ([`SyncService`]). A [`Connection`]
//! logs in once and carries any number of terminals ([`Terminal`]) and
//! SFTP sessions ([`SftpSession`]). Each object says what happens through
//! `next_event()`, which the app awaits in a loop: nothing calls back into
//! the app but its [`Keychain`]. Nothing here is in words: failures are
//! typed, and the app words them in its own language.

#[cfg(target_os = "android")]
mod android;
mod connection;
mod core;
mod events;
mod failure;
mod hosts;
mod login;
mod prompt;
mod secrets;
mod sftp;
mod store;
mod sync;
mod task;
mod terminal;

pub use connection::{Connection, ConnectionEvent, ExecOutput};
pub use core::{Core, CoreConfig, CoreError};
pub use failure::*;
pub use hosts::{
    Credential, CredentialDraft, CredentialKind, CredentialProblem, Dependents, Group, GroupDraft,
    Host, HostAuth, HostDraft, HostOs, HostProblem, HostProtocol, HostProxy, HostRoute,
    KeyAlgorithm, KeyChange, KeyProblem, NewKey, PrivateKeyInfo, SecretChange, TreeDrop, TreeNode,
    check_private_key,
};
pub use login::{JumpHost, Login, LoginMethod, Proxy, ProxyKind, Route};
pub use prompt::{Prompt, PromptField, PromptKind, PromptReply};
pub use secrets::{Keychain, KeychainError, SecretRef, account_for};
pub use sftp::{
    DirectoryEntry, DirectoryListing, EntryKind, SftpDisconnect, SftpEvent, SftpSession,
    TransferAnswer, TransferChoice, TransferDetail, TransferDirection, TransferOutcome,
    TransferPhase, TransferProgress, TransferQuestion, TransferQuestionKind,
};
pub use store::{
    ChangeKind, HostStore, SavedSecret, SecretProblem, StoreError, StoreEvent, StoreFailure,
};
pub use sync::{
    Attention, CheckedLogin, LoginProblem, PasswordProblem, S3Login, StorageProblem, SyncDevice,
    SyncFailure, SyncLocation, SyncOutcome, SyncService, SyncStatus, check_new_sync_password,
    check_s3_login, check_sync_password, check_webdav_login, min_sync_password_length,
    random_sync_password,
};
pub use terminal::{Terminal, TerminalEvent, TerminalOptions};

uniffi::setup_scaffolding!();
