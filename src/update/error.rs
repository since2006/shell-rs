//! What can go wrong while checking for, fetching or installing an update,
//! worded for the person reading 设置 › 关于 or the update dialog.

use std::fmt;

use crate::i18n::t;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateError {
    /// The server could not be reached, or the transfer broke off.
    Network(String),
    /// The server answered, but not with the file.
    Http(u16),
    /// Nothing arrived for a while in the middle of a download.
    Stalled,
    /// The manifest is not signed by a key this build trusts, or was
    /// signed for another channel or version.
    BadSignature,
    /// The manifest could not be read.
    BadManifest(String),
    /// The downloaded file is not the one the manifest describes.
    Corrupt,
    /// Not enough room for the download and the unpacked copy.
    NoSpace,
    /// Reading or writing a local file failed.
    Disk(String),
    /// Putting the new version in place failed.
    Install(String),
    /// Stopped on purpose: a newer check, or automatic updates turned off.
    Cancelled,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Network(reason) => t!("update.error.network", reason = reason),
            Self::Http(status) => t!("update.error.http", status = status),
            Self::Stalled => t!("update.error.stalled"),
            Self::BadSignature => t!("update.error.bad_signature"),
            Self::BadManifest(reason) => t!("update.error.bad_manifest", reason = reason),
            Self::Corrupt => t!("update.error.corrupt"),
            Self::NoSpace => t!("update.error.no_space"),
            Self::Disk(reason) => t!("update.error.disk", reason = reason),
            Self::Install(reason) => t!("update.error.install", reason = reason),
            Self::Cancelled => t!("update.error.cancelled"),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for UpdateError {}

impl From<std::io::Error> for UpdateError {
    fn from(error: std::io::Error) -> Self {
        Self::Disk(error.to_string())
    }
}
