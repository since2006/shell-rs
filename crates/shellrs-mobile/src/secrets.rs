//! The app's keychain, where every saved secret lives: passwords and
//! passphrases, and the keys and login of sync. The app implements it on
//! its platform's keychain; the core reads and writes it by account name,
//! the same names as the desktop's, so synced secrets land where they are
//! looked for.

use std::fmt;
use std::sync::Arc;

use shellrs_hosts::secrets::{SecretStore, SecretsUnavailable};
use shellrs_ssh as ssh;
use zeroize::Zeroizing;

/// Which saved secret a connection asks for. The same secret has the same
/// keychain account name as on the desktop ([`account_for`]), so synced
/// secrets are found under it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum SecretRef {
    /// The login password of `user@host:port`.
    Password {
        user: String,
        host: String,
        port: u16,
    },
    /// The passphrase of the private key file at `key_path`.
    Passphrase { key_path: String },
    /// A password credential's password, by the credential's `keychain_id`.
    Credential { keychain_id: String },
    /// The password of an HTTP or SOCKS5 proxy.
    Proxy {
        user: String,
        host: String,
        port: u16,
    },
    /// The password of a connection that is not saved, by an id the app
    /// gave it. The core keeps it in memory only: the keychain never sees
    /// it.
    Temporary { host: u64 },
    /// Sync's keys.
    SyncKeyring,
    /// The password of the WebDAV server, or the secret key of the S3
    /// bucket, sync goes through.
    SyncLogin,
}

impl From<&ssh::SecretRef> for SecretRef {
    fn from(secret: &ssh::SecretRef) -> Self {
        match secret {
            ssh::SecretRef::Password { user, host, port } => Self::Password {
                user: user.clone(),
                host: host.clone(),
                port: *port,
            },
            ssh::SecretRef::Passphrase { key_path } => Self::Passphrase {
                key_path: key_path.clone(),
            },
            ssh::SecretRef::Credential { keychain_id } => Self::Credential {
                keychain_id: keychain_id.clone(),
            },
            ssh::SecretRef::Proxy { user, host, port } => Self::Proxy {
                user: user.clone(),
                host: host.clone(),
                port: *port,
            },
            ssh::SecretRef::Temporary { host } => Self::Temporary { host: *host },
            ssh::SecretRef::SyncKeyring => Self::SyncKeyring,
            ssh::SecretRef::SyncLogin => Self::SyncLogin,
        }
    }
}

impl From<SecretRef> for ssh::SecretRef {
    fn from(secret: SecretRef) -> Self {
        match secret {
            SecretRef::Password { user, host, port } => Self::password(user, host, port),
            SecretRef::Passphrase { key_path } => Self::Passphrase { key_path },
            SecretRef::Credential { keychain_id } => Self::credential(keychain_id),
            SecretRef::Proxy { user, host, port } => Self::proxy(user, host, port),
            SecretRef::Temporary { host } => Self::temporary(host),
            SecretRef::SyncKeyring => Self::SyncKeyring,
            SecretRef::SyncLogin => Self::SyncLogin,
        }
    }
}

/// The keychain account name `secret` is saved under, the same as on the
/// desktop.
#[uniffi::export]
pub fn account_for(secret: SecretRef) -> String {
    ssh::SecretRef::from(secret).account()
}

/// The app's keychain: implemented in Swift or Kotlin, on the platform's
/// own (Keychain Services on iOS, the Keystore on Android). Each secret is
/// a string under an account name ([`account_for`]); keep them all under
/// one service, as the desktop keeps them under `shellrs`.
///
/// Only the core writes it: a password the app saves itself is one sync
/// never hears of. Save through the store ([`crate::HostStore`]) instead.
///
/// Every method is called on one of the core's background threads and
/// may take its time, but must not wait for the main thread (Face ID, a
/// dialog): fail with [`KeychainError::Unavailable`] instead.
#[uniffi::export(with_foreign)]
pub trait Keychain: Send + Sync {
    /// Whether there is a keychain to keep secrets in now. Without one,
    /// the app cannot save passwords or sync.
    fn available(&self) -> bool;

    /// The secret saved under `account`, `None` when none is.
    fn get(&self, account: String) -> Result<Option<String>, KeychainError>;

    /// Save `value` under `account`, in place of what was there.
    fn set(&self, account: String, value: String) -> Result<(), KeychainError>;

    /// Delete what is saved under `account`. Nothing being there is no
    /// failure.
    fn delete(&self, account: String) -> Result<(), KeychainError>;
}

/// Why the keychain did not do what it was asked.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum KeychainError {
    /// There is no keychain now, or it is locked: nothing is read or kept.
    Unavailable,
    /// The keychain's own failure, in its words.
    Failed { detail: String },
}

impl fmt::Display for KeychainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("no keychain"),
            Self::Failed { detail } => f.write_str(detail),
        }
    }
}

impl std::error::Error for KeychainError {}

/// What the app's implementation threw that it should not have.
impl From<uniffi::UnexpectedUniFFICallbackError> for KeychainError {
    fn from(error: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Failed {
            detail: error.reason,
        }
    }
}

/// The app's keychain as the hosts core uses it.
pub(crate) struct AppKeychain(pub(crate) Arc<dyn Keychain>);

impl AppKeychain {
    fn failed(error: KeychainError) -> anyhow::Error {
        match error {
            // What the core knows as no keychain at all.
            KeychainError::Unavailable => SecretsUnavailable.into(),
            KeychainError::Failed { detail } => anyhow::Error::msg(detail),
        }
    }
}

impl SecretStore for AppKeychain {
    fn get(&self, secret: &ssh::SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
        self.0
            .get(secret.account())
            .map(|value| value.map(Zeroizing::new))
            .map_err(Self::failed)
    }

    fn set(&self, secret: &ssh::SecretRef, value: &str) -> anyhow::Result<()> {
        self.0
            .set(secret.account(), value.to_owned())
            .map_err(Self::failed)
    }

    fn delete(&self, secret: &ssh::SecretRef) -> anyhow::Result<()> {
        self.0.delete(secret.account()).map_err(Self::failed)
    }

    fn is_available(&self) -> bool {
        self.0.available()
    }
}

#[cfg(test)]
mod tests {
    use super::{SecretRef, account_for};

    /// Secrets synced from the desktop are found under the same names.
    #[test]
    fn accounts_are_the_desktops() {
        assert_eq!(
            account_for(SecretRef::Password {
                user: "root".into(),
                host: "10.0.0.1".into(),
                port: 22,
            }),
            "password:root@10.0.0.1:22"
        );
        assert_eq!(
            account_for(SecretRef::Passphrase {
                key_path: "/keys/id".into()
            }),
            "passphrase:/keys/id"
        );
        assert_eq!(
            account_for(SecretRef::Credential {
                keychain_id: "abc".into()
            }),
            "credential:abc"
        );
        assert_eq!(
            account_for(SecretRef::Proxy {
                user: "me".into(),
                host: "proxy".into(),
                port: 3128,
            }),
            "proxy:me@proxy:3128"
        );
        assert_eq!(account_for(SecretRef::Temporary { host: 7 }), "temporary:7");
        assert_eq!(account_for(SecretRef::SyncKeyring), "sync:keyring");
        assert_eq!(account_for(SecretRef::SyncLogin), "sync:login");
    }
}
