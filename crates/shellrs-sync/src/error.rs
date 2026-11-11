//! What can go wrong, as values the app turns into its own words: the
//! `Display` texts here are English for logs, never shown as they are.

use std::fmt;

/// A storage backend's failure, the same for every backend so a round can
/// tell what to do: retry later, ask for credentials, or report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// No such file. Listing a folder that does not exist is not this: it
    /// is an empty listing.
    NotFound,
    /// A conditional write lost: the file was not as the condition said.
    PreconditionFailed,
    /// The file is larger than the caller would read.
    TooLarge,
    /// The backend did not answer in time.
    Timeout,
    /// The backend refused the account or its credentials.
    Unauthorized,
    /// The backend is unreachable or failed, for now.
    Unavailable(String),
    /// Anything else, such as a local file the system would not write.
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::PreconditionFailed => f.write_str("the file changed meanwhile"),
            Self::TooLarge => f.write_str("the file is too large"),
            Self::Timeout => f.write_str("timed out"),
            Self::Unauthorized => f.write_str("not authorized"),
            Self::Unavailable(reason) => write!(f, "unavailable: {reason}"),
            Self::Io(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Why a sync call stopped. Each one leaves the state the caller passed in
/// as it was, so the caller only has to show it and try again later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncError {
    Store(StoreError),
    /// There is no `vault.json`: sync was never set up here, or was reset.
    VaultMissing,
    /// Creating a vault where another device just created one.
    VaultExists,
    /// The vault was written by a newer ShellRS, in this format.
    VaultTooNew(u32),
    /// `vault.json` is unreadable or out of bounds.
    BadVault(String),
    WrongPassword,
    /// The keys this device keeps no longer open the vault: its password
    /// was changed on another device. Ask for the new one.
    NeedsPassword,
    /// The vault was deleted and made again (sync was reset): the files are
    /// someone else's now. Join it anew with its password.
    VaultReplaced,
    /// The saved sync state cannot be read back.
    StateUnreadable(String),
    /// The round would delete `count` of the `of` records the app had
    /// before, which looks like a fault rather than the user's doing. It
    /// goes through with `RoundOptions::allow_mass_delete`.
    MassDelete {
        count: u32,
        of: u32,
    },
    /// A snapshot needs this newer format: the device reads but must not
    /// write until ShellRS is updated.
    ReadOnly {
        min_reader: u32,
    },
    /// The system's random source failed.
    Random,
    /// The text is not a setup code, or not a whole one.
    BadSetupCode,
    /// The setup code was made by a newer ShellRS, in a version or for a
    /// kind of place this one does not know.
    SetupCodeTooNew,
    /// The setup code does not open with this password: it is not the
    /// sync password, or the code was made before the password changed.
    SetupCodeWrongPassword,
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "storage: {error}"),
            Self::VaultMissing => f.write_str("sync is not set up there"),
            Self::VaultExists => f.write_str("sync is already set up there"),
            Self::VaultTooNew(format) => write!(f, "the vault needs format {format}"),
            Self::BadVault(reason) => write!(f, "unreadable vault: {reason}"),
            Self::WrongPassword => f.write_str("wrong sync password"),
            Self::NeedsPassword => f.write_str("the sync password was changed"),
            Self::VaultReplaced => f.write_str("sync was reset on another device"),
            Self::StateUnreadable(reason) => write!(f, "unreadable sync state: {reason}"),
            Self::MassDelete { count, of } => write!(f, "would delete {count} of {of} records"),
            Self::ReadOnly { min_reader } => write!(f, "a snapshot needs format {min_reader}"),
            Self::Random => f.write_str("no random numbers"),
            Self::BadSetupCode => f.write_str("not a setup code"),
            Self::SetupCodeTooNew => f.write_str("the setup code needs a newer version"),
            Self::SetupCodeWrongPassword => {
                f.write_str("the setup code does not open with this password")
            }
        }
    }
}

impl std::error::Error for SyncError {}

impl From<StoreError> for SyncError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// Something a round worked around rather than stopped for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    /// A device's file could not be read and was left out of the merge.
    CorruptSnapshot { device: String, reason: String },
    /// A device's file needs a newer ShellRS; until then this one only
    /// reads the others.
    SnapshotTooNew { device: String, min_reader: u32 },
    /// A device's file is encrypted with a key this device cannot get.
    UnknownKeyGeneration { device: String, generation: u32 },
    /// A device's file belongs to another vault: one made before a reset.
    OtherVault { device: String },
    /// A device's clock runs this far ahead of this one's.
    ClockAhead { device: String, ahead_ms: u64 },
    /// A record this version cannot use, kept and passed on untouched.
    Parked {
        kind: String,
        id: String,
        reason: String,
    },
    /// The secrets sealed in the sync state did not open with this
    /// keyring: they come back from the other devices' files and the app.
    SecretsUnreadable,
}
