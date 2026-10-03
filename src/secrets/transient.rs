use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use zeroize::Zeroizing;

use super::{SecretRef, SecretStore, SharedSecretStore};

/// 钥匙串外面包的一层：临时连接的密码（[`SecretRef::Transient`]）只记在这层
/// 内存里，其余的原样交给里面的钥匙串。
///
/// `HostStore` 把注入的秘密存储包成这样，连接器读到的也是它，所以终端和 SFTP
/// 用得上链接里的密码，而这个密码不会落进钥匙串；没有钥匙串的机器上照样能用。
pub struct TransientSecretStore {
    keychain: SharedSecretStore,
    passwords: Mutex<HashMap<u64, Zeroizing<String>>>,
}

impl TransientSecretStore {
    pub fn new(keychain: SharedSecretStore) -> Self {
        Self {
            keychain,
            passwords: Mutex::new(HashMap::new()),
        }
    }

    fn passwords(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Zeroizing<String>>> {
        self.passwords
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// 记下临时主机 `host` 的密码。只碰内存，可以在 UI 线程上调用。
    pub fn remember(&self, host: u64, password: Zeroizing<String>) {
        self.passwords().insert(host, password);
    }

    /// 忘掉临时主机 `host` 的密码。只碰内存。
    pub fn forget(&self, host: u64) {
        self.passwords().remove(&host);
    }
}

impl SecretStore for TransientSecretStore {
    fn get(&self, secret: &SecretRef) -> Result<Option<Zeroizing<String>>> {
        match secret {
            SecretRef::Transient { host } => Ok(self.passwords().get(host).cloned()),
            _ => self.keychain.get(secret),
        }
    }

    fn set(&self, secret: &SecretRef, value: &str) -> Result<()> {
        match secret {
            SecretRef::Transient { host } => {
                self.remember(*host, Zeroizing::new(value.to_string()));
                Ok(())
            }
            _ => self.keychain.set(secret, value),
        }
    }

    fn delete(&self, secret: &SecretRef) -> Result<()> {
        match secret {
            SecretRef::Transient { host } => {
                self.forget(*host);
                Ok(())
            }
            _ => self.keychain.delete(secret),
        }
    }

    fn is_available(&self) -> bool {
        self.keychain.is_available()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::secrets::{InMemorySecretStore, NoSecretStore};

    #[test]
    fn link_passwords_stay_in_memory_and_the_rest_reaches_the_keychain() {
        let keychain = Arc::new(InMemorySecretStore::default());
        let store = TransientSecretStore::new(keychain.clone());
        let link = SecretRef::transient(7);
        store.remember(7, Zeroizing::new("token".into()));
        assert_eq!(
            store.get(&link).unwrap().as_deref().map(String::as_str),
            Some("token")
        );
        assert!(keychain.is_empty());

        let saved = SecretRef::password("root", "10.0.1.12", 22);
        store.set(&saved, "hunter2").unwrap();
        assert_eq!(keychain.len(), 1);
        assert_eq!(
            store.get(&saved).unwrap().as_deref().map(String::as_str),
            Some("hunter2")
        );
        // Another host's password is not the link's.
        assert!(store.get(&SecretRef::transient(8)).unwrap().is_none());

        store.forget(7);
        assert!(store.get(&link).unwrap().is_none());
        assert_eq!(keychain.len(), 1);
    }

    #[test]
    fn without_a_keychain_link_passwords_still_work() {
        let store = TransientSecretStore::new(Arc::new(NoSecretStore));
        assert!(!store.is_available());
        store.set(&SecretRef::transient(1), "token").unwrap();
        assert!(store.get(&SecretRef::transient(1)).unwrap().is_some());
        assert!(
            store
                .set(&SecretRef::password("root", "h", 22), "x")
                .is_err()
        );
    }
}
