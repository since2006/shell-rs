//! 系统钥匙串（macOS Keychain Services / Windows 凭据管理器 / Secret Service）。
//!
//! ShellRS 的秘密只存在这里。数据库里永远不出现密码或口令，`session/database.rs`
//! 的 `schema_never_contains_secret_columns` 守着这条不变量。

mod keychain;
mod memory;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use zeroize::Zeroizing;

pub use keychain::KeychainSecretStore;
pub use memory::{InMemorySecretStore, NoSecretStore};

/// 钥匙串里的服务名，所有条目共用。改了会让已保存的秘密全部失联。
pub const SERVICE: &str = "shellrs";

/// 一条秘密的身份。
///
/// 按「用哪个用户连到哪个端点」而不是「哪条保存的主机」归属：主机改名、复制都
/// 不丢密码，端点和用户相同的几条共用一条；私钥口令按文件路径归属，同一把钥匙
/// 只问一次。密码凭据的密码归凭据自己，用它的主机共用这一条。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// 某个 SSH 端点的登录密码。
    Password {
        user: String,
        host: String,
        port: u16,
    },
    /// 某个私钥文件的口令。
    Passphrase { key_path: String },
    /// 某条密码凭据的密码，按凭据随机生成、永不复用的 `keychain_id` 归属。
    Credential { keychain_id: String },
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

    /// 钥匙串条目的账户名。前缀区分种类，在 Keychain Access 里直接可读。
    pub fn account(&self) -> String {
        match self {
            Self::Password { user, host, port } => format!("password:{user}@{host}:{port}"),
            Self::Passphrase { key_path } => format!("passphrase:{key_path}"),
            Self::Credential { keychain_id } => format!("credential:{keychain_id}"),
        }
    }
}

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

/// 生产环境用的秘密存储：能用系统钥匙串就用，用不了就退化成一个什么都不存的
/// 实现，界面据此把密码字段禁用掉。
pub fn system_secret_store() -> SharedSecretStore {
    let keychain = KeychainSecretStore::new();
    if keychain.is_available() {
        Arc::new(keychain)
    } else {
        Arc::new(NoSecretStore)
    }
}

#[cfg(test)]
mod tests {
    use super::{InMemorySecretStore, NoSecretStore, SecretRef, SecretStore};

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
    fn accounts_of_different_kinds_never_collide() {
        let password = SecretRef::password("root", "10.0.1.12", 22);
        let passphrase = SecretRef::passphrase("root@10.0.1.12:22");
        let credential = SecretRef::credential("root@10.0.1.12:22");
        assert_ne!(password.account(), passphrase.account());
        assert_ne!(password.account(), credential.account());
        assert_ne!(passphrase.account(), credential.account());
    }

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
