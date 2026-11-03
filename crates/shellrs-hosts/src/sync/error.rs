//! Why a sync job stopped, as a value: the app puts it into words when
//! shown, so it reads in the language of the moment.

use shellrs_sync::SyncError;

use crate::SecretError;

/// Why a sync job stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobError {
    Sync(SyncError),
    /// This copy cannot sync: no keychain to keep the keys in.
    Unavailable,
    /// The keychain has no sync keys, or keys it cannot read: deleted, or
    /// never saved.
    NoKeyring,
    /// The keychain refused.
    Keychain(SecretError),
    /// The database refused a round's changes, in its own words: nothing
    /// was applied.
    NotSaved(String),
    /// The database refused the sync state a round left, in its own words.
    StateNotSaved(String),
    /// The keychain has no password for the WebDAV server.
    NoLogin,
    /// A round or another job runs: one at a time.
    InProgress,
    /// Every try met a change made meanwhile, the user's or another
    /// device's: the next round will do.
    Busy,
}

impl From<SyncError> for JobError {
    fn from(error: SyncError) -> Self {
        Self::Sync(error)
    }
}

impl JobError {
    /// The keychain's refusal.
    pub fn keychain(error: anyhow::Error) -> Self {
        Self::Keychain(SecretError::of(&error))
    }
}
