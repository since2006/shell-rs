//! ShellRS's sync core: hosts and the rest kept alike on every device of a
//! user, through storage that only keeps files (a folder a cloud drive
//! syncs, a WebDAV server, an S3 bucket).
//!
//! Each device writes one file, an encrypted snapshot of every record it
//! knows, and reads the others'; per record, the latest version wins. The
//! storage sees ciphertext only. The app hands its records in and takes
//! changes out (`pull`), stores them, then uploads (`push`): the crate
//! knows nothing of the app's database or windows, so the mobile apps use
//! it too. Secrets (passwords, passphrases, private keys) sync with the
//! rest and are never kept in the clear (see `secret`). `FORMAT.md`
//! describes the files.

#![forbid(unsafe_code)]

mod clock;
mod env;
mod envelope;
mod error;
mod fixup;
#[cfg(test)]
mod golden;
mod hex;
mod id;
pub mod layout;
mod merge;
mod record;
mod round;
#[cfg(test)]
mod round_tests;
mod secret;
#[cfg(test)]
mod secret_tests;
mod setup_code;
#[cfg(test)]
mod simulation;
mod snapshot;
mod state;
pub mod store;
#[cfg(test)]
mod testing;
mod vault;
mod wire;

pub use clock::{Stamp, Version};
pub use env::{Env, FixedEnv, SystemEnv};
pub use error::{StoreError, SyncError, Warning};
pub use id::{DeviceId, Kind, RecordKey};
pub use round::{
    Change, JoinMode, LocalRecord, LocalSnapshot, Pulled, Pushed, RemoteDevice, RoundOptions,
    RoundReport, Unchanged, pull, push,
};
pub use secret::{EndpointSecret, Secret, endpoint_secret_id};
pub use setup_code::{check_setup_code, is_setup_code, open_setup_code, seal_setup_code};
pub use snapshot::DeviceInfo;
pub use state::SyncState;
pub use store::{
    Fetched, LocalFolderStore, MemoryStore, PutCondition, RemoteFile, S3Location, S3Store,
    SyncStore, WebDavStore,
};
pub use vault::{
    KdfParams, Keyring, VaultStatus, change_password, create_vault, refresh_keyring, rotate_key,
    unlock_vault, vault_status,
};
pub use wire::{
    Auth, BookmarkData, Capabilities, CredentialData, CredentialKind, Endpoint, ForwardData,
    ForwardKind, GroupData, HostData, Protocol, ProxyData, ProxyKind, RecordData, Route,
    SnippetCategoryData, SnippetData, SnippetScope, TelnetData, TotpData, bookmark_id,
};

/// The snapshot format this version writes and reads.
pub const FORMAT: u32 = 1;
