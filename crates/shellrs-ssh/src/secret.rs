//! Which secret a connection needs, and where it reads the saved ones.
//! Each app keeps its secrets in its own platform's keychain; the
//! connection only ever asks for one by its [`SecretRef`].

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use zeroize::Zeroizing;

/// The identity of one secret.
///
/// A login password belongs to the user and endpoint it logs in to, not to
/// a saved host: renaming or copying a host keeps it, and hosts of the same
/// user and endpoint share it. A passphrase belongs to its key file, so one
/// key is asked about once. A password credential's password belongs to the
/// credential, shared by the hosts that use it. A proxy's password belongs
/// to the proxy's address and user, shared by the hosts behind it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// The login password of an SSH endpoint.
    Password {
        user: String,
        host: String,
        port: u16,
    },
    /// The passphrase of a private key file.
    Passphrase { key_path: String },
    /// A password credential's password, by the credential's random
    /// `keychain_id` that is never reused.
    Credential { keychain_id: String },
    /// The password of an HTTP or SOCKS5 proxy.
    Proxy {
        user: String,
        host: String,
        port: u16,
    },
    /// The password of a connection that is not saved (an external or quick
    /// connection), by that temporary host's id. Only ever held in memory,
    /// never in a keychain.
    Temporary { host: u64 },
    /// The sync keyring (`shellrs_sync::Keyring`), one per device: with it,
    /// sync does not ask for the sync password each time.
    SyncKeyring,
    /// The login secret of the server sync goes through (a WebDAV password,
    /// an S3 secret key), one per device: a device syncs to one place at a
    /// time.
    SyncLogin,
}

impl SecretRef {
    pub fn password(user: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self::Password {
            user: user.into(),
            host: host.into(),
            port,
        }
    }

    pub fn passphrase(key_path: impl AsRef<Path>) -> Self {
        Self::Passphrase {
            key_path: key_path.as_ref().display().to_string(),
        }
    }

    pub fn credential(keychain_id: impl Into<String>) -> Self {
        Self::Credential {
            keychain_id: keychain_id.into(),
        }
    }

    pub fn proxy(user: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self::Proxy {
            user: user.into(),
            host: host.into(),
            port,
        }
    }

    pub fn temporary(host: u64) -> Self {
        Self::Temporary { host }
    }

    /// The keychain entry's account name. Its prefix says the kind, readable
    /// as it is in Keychain Access. Saved secrets are found by it, so it
    /// never changes.
    pub fn account(&self) -> String {
        match self {
            Self::Password { user, host, port } => format!("password:{user}@{host}:{port}"),
            Self::Passphrase { key_path } => format!("passphrase:{key_path}"),
            Self::Credential { keychain_id } => format!("credential:{keychain_id}"),
            Self::Proxy { user, host, port } => format!("proxy:{user}@{host}:{port}"),
            Self::Temporary { host } => format!("temporary:{host}"),
            Self::SyncKeyring => "sync:keyring".into(),
            Self::SyncLogin => "sync:login".into(),
        }
    }
}

/// Where a connection reads the secrets saved for it.
///
/// It may block (a keychain may ask the user for access), so a connection
/// reads only what the server asks for, and never on an async worker: see
/// [`read_secret`].
pub trait SecretSource: Send + Sync + 'static {
    /// The saved secret, or `Ok(None)` when none is saved.
    fn secret(&self, secret: &SecretRef) -> Result<Option<Zeroizing<String>>>;
}

pub type SharedSecretSource = Arc<dyn SecretSource>;

/// Read a saved secret off the async workers. A source that errors, is
/// locked, or holds an empty value counts as nothing saved: the connection
/// then falls back to asking, which is always better than refusing to
/// connect.
pub async fn read_secret(
    secrets: &SharedSecretSource,
    secret: &SecretRef,
) -> Option<Zeroizing<String>> {
    let (secrets, secret) = (secrets.clone(), secret.clone());
    tokio::task::spawn_blocking(move || secrets.secret(&secret))
        .await
        .ok()?
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::SecretRef;

    #[test]
    fn password_accounts_name_the_endpoint() {
        let secret = SecretRef::password("root", "10.0.1.12", 22);
        assert_eq!(secret.account(), "password:root@10.0.1.12:22");
    }

    #[test]
    fn passphrase_accounts_name_the_key_file() {
        let secret = SecretRef::passphrase("/Users/me/.ssh/id_ed25519");
        assert_eq!(secret.account(), "passphrase:/Users/me/.ssh/id_ed25519");
    }

    #[test]
    fn credential_accounts_name_the_keychain_id() {
        let secret = SecretRef::credential("Jwg5rHvXCxw89paM");
        assert_eq!(secret.account(), "credential:Jwg5rHvXCxw89paM");
    }

    #[test]
    fn proxy_accounts_name_the_proxy_and_its_user() {
        let secret = SecretRef::proxy("me", "127.0.0.1", 7890);
        assert_eq!(secret.account(), "proxy:me@127.0.0.1:7890");
    }

    #[test]
    fn other_accounts_are_fixed() {
        assert_eq!(SecretRef::temporary(7).account(), "temporary:7");
        assert_eq!(SecretRef::SyncKeyring.account(), "sync:keyring");
        assert_eq!(SecretRef::SyncLogin.account(), "sync:login");
    }

    #[test]
    fn accounts_of_different_kinds_never_collide() {
        let accounts = [
            SecretRef::password("root", "10.0.1.12", 22),
            SecretRef::passphrase("root@10.0.1.12:22"),
            SecretRef::credential("root@10.0.1.12:22"),
            SecretRef::proxy("root", "10.0.1.12", 22),
            SecretRef::temporary(22),
            SecretRef::SyncKeyring,
            SecretRef::SyncLogin,
        ]
        .map(|secret| secret.account());
        for (ix, account) in accounts.iter().enumerate() {
            assert!(!accounts[ix + 1..].contains(account), "{account}");
        }
    }
}
