//! How the errors of sync read: its jobs', and what is wrong with a sync
//! password or a login as typed.

use gpui_kit::SharedString;
use shellrs_sync::{StoreError, SyncError};

use super::{JobError, LoginError, MIN_PASSWORD, PasswordError};
use crate::host::ErrorMessage;
use crate::i18n::{t, tn};

impl ErrorMessage for JobError {
    fn message(&self) -> SharedString {
        match self {
            Self::Sync(error) => describe_sync(error),
            Self::Unavailable => t!("sync.error.unavailable"),
            Self::NoKeyring => t!("sync.error.no_keyring"),
            Self::Keychain(error) => error.message(),
            Self::NotSaved(error) => t!("host.store.sync.not_saved", error = error),
            Self::StateNotSaved(error) => t!("host.store.sync.state_not_saved", error = error),
            Self::NoLogin => t!("sync.error.no_login"),
            Self::InProgress => t!("sync.error.in_progress"),
            Self::Busy => t!("sync.error.busy"),
        }
    }
}

impl ErrorMessage for PasswordError {
    fn message(&self) -> SharedString {
        match self {
            Self::Missing => t!("sync.dialog.password_missing"),
            Self::TooShort => t!("sync.dialog.too_short", count = MIN_PASSWORD),
            Self::Mismatch => t!("sync.dialog.mismatch"),
        }
    }
}

impl ErrorMessage for LoginError {
    fn message(&self) -> SharedString {
        match self {
            Self::UrlInvalid => t!("sync.webdav.url_invalid"),
            Self::UsernameMissing => t!("sync.webdav.username_missing"),
            Self::PasswordMissing => t!("sync.webdav.password_missing"),
            Self::EndpointInvalid => t!("sync.s3.endpoint_invalid"),
            Self::BucketInvalid => t!("sync.s3.bucket_invalid"),
            Self::AccessKeyMissing => t!("sync.s3.access_key_missing"),
            Self::SecretKeyMissing => t!("sync.s3.secret_key_missing"),
        }
    }
}

fn describe_sync(error: &SyncError) -> SharedString {
    match error {
        SyncError::Store(error) => describe_store(error),
        SyncError::VaultMissing => t!("sync.error.vault_missing"),
        SyncError::VaultExists => t!("sync.error.vault_exists"),
        SyncError::VaultTooNew(_) => t!("sync.error.vault_too_new"),
        SyncError::BadVault(reason) => t!("sync.error.bad_vault", reason = reason),
        SyncError::WrongPassword => t!("sync.error.wrong_password"),
        SyncError::NeedsPassword => t!("sync.error.needs_password"),
        SyncError::VaultReplaced => t!("sync.error.vault_replaced"),
        SyncError::StateUnreadable(reason) => t!("sync.error.state_unreadable", reason = reason),
        SyncError::MassDelete { count, of } => {
            tn!("sync.error.mass_delete", *count as usize, of = of)
        }
        SyncError::ReadOnly { .. } => t!("sync.error.read_only"),
        SyncError::Random => t!("sync.error.random"),
    }
}

fn describe_store(error: &StoreError) -> SharedString {
    match error {
        StoreError::NotFound => t!("sync.error.store.not_found"),
        StoreError::PreconditionFailed => t!("sync.error.store.changed"),
        StoreError::TooLarge => t!("sync.error.store.too_large"),
        StoreError::Timeout => t!("sync.error.store.timeout"),
        StoreError::Unauthorized => t!("sync.error.store.unauthorized"),
        StoreError::Unavailable(reason) => t!("sync.error.store.unavailable", reason = reason),
        StoreError::Io(reason) => t!("sync.error.store.io", reason = reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::SecretError;

    #[test]
    fn a_refused_round_says_what_was_not_kept_in_either_language() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        assert_eq!(
            JobError::NotSaved("disk I/O error".into()).message(),
            "同步来的改动未能保存，这一轮没有应用：disk I/O error"
        );
        assert_eq!(
            JobError::StateNotSaved("disk I/O error".into()).message(),
            "同步状态未能保存：disk I/O error"
        );
        assert_eq!(
            JobError::Keychain(SecretError::Unavailable).message(),
            SecretError::Unavailable.message()
        );
        assert_eq!(
            PasswordError::TooShort.message(),
            "同步密码至少要 12 个字符。"
        );
        crate::i18n::set_locale("en");
        assert_eq!(
            JobError::NotSaved("disk I/O error".into()).message(),
            "Couldn’t save what sync brought, so none of it was applied: disk I/O error"
        );
        assert_eq!(
            JobError::StateNotSaved("disk I/O error".into()).message(),
            "Couldn’t save the sync state: disk I/O error"
        );
    }
}
