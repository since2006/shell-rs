//! 秘密的读写接口和不碰系统钥匙串的几种实现。系统钥匙串本身
//! （`KeychainSecretStore`）在应用里。
//!
//! ShellRS 的秘密只存在钥匙串里。数据库里永远不出现密码或口令，`database.rs`
//! 的 `schema_never_contains_secret_columns` 守着这条不变量。

mod memory;
mod temporary;

use std::sync::Arc;

use anyhow::Result;
use zeroize::Zeroizing;

pub use memory::{InMemorySecretStore, NoSecretStore, SecretsUnavailable};
pub use shellrs_ssh::{SecretRef, SecretSource, SharedSecretSource};
pub use temporary::TemporarySecretStore;

/// 系统钥匙串的读写。
///
/// 每个方法都是阻塞的，在 macOS 上还可能弹出系统授权框，所以只能在后台执行器
/// 或传输层自己的工作线程上调用，**永远不要在 UI 线程上调用**。
pub trait SecretStore: Send + Sync + 'static {
    /// 读一条秘密。没存过是 `Ok(None)`，不是错误。
    fn get(&self, secret: &SecretRef) -> Result<Option<Zeroizing<String>>>;
    fn set(&self, secret: &SecretRef, value: &str) -> Result<()>;
    /// 删一条秘密。条目本来就不存在也算成功。
    fn delete(&self, secret: &SecretRef) -> Result<()>;
    /// 这台机器上钥匙串是否可用。为 false 时 `set` 一定失败。
    fn is_available(&self) -> bool;
}

pub type SharedSecretStore = Arc<dyn SecretStore>;

/// 钥匙串交给连接用的那一面：只读。
pub struct KeychainSource(SharedSecretStore);

impl KeychainSource {
    pub fn shared(store: SharedSecretStore) -> SharedSecretSource {
        Arc::new(Self(store))
    }
}

impl SecretSource for KeychainSource {
    fn secret(&self, secret: &SecretRef) -> Result<Option<Zeroizing<String>>> {
        self.0.get(secret)
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemorySecretStore, NoSecretStore, SecretRef, SecretStore};

    #[test]
    fn the_memory_store_round_trips() {
        let store = InMemorySecretStore::default();
        let secret = SecretRef::password("root", "10.0.1.12", 22);
        assert!(store.get(&secret).unwrap().is_none());

        store.set(&secret, "hunter2").unwrap();
        assert_eq!(
            store.get(&secret).unwrap().as_deref().map(String::as_str),
            Some("hunter2")
        );

        store.set(&secret, "hunter3").unwrap();
        assert_eq!(
            store.get(&secret).unwrap().as_deref().map(String::as_str),
            Some("hunter3")
        );

        store.delete(&secret).unwrap();
        assert!(store.get(&secret).unwrap().is_none());
    }

    #[test]
    fn deleting_a_missing_secret_succeeds() {
        let store = InMemorySecretStore::default();
        let secret = SecretRef::passphrase("/tmp/id_ed25519");
        assert!(store.delete(&secret).is_ok());
    }

    #[test]
    fn the_memory_store_keys_by_account() {
        let store = InMemorySecretStore::default();
        let one = SecretRef::password("root", "10.0.1.12", 22);
        let other = SecretRef::password("root", "10.0.1.12", 2222);
        store.set(&one, "hunter2").unwrap();
        assert!(store.get(&other).unwrap().is_none());
    }

    #[test]
    fn the_disabled_store_reads_empty_and_refuses_writes() {
        let store = NoSecretStore;
        let secret = SecretRef::password("root", "10.0.1.12", 22);
        assert!(!store.is_available());
        assert!(store.get(&secret).unwrap().is_none());
        assert!(store.set(&secret, "hunter2").is_err());
        assert!(store.delete(&secret).is_ok());
    }
}
