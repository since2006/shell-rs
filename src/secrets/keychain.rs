use anyhow::{Result, anyhow};
use keyring::{Entry, Error};
use zeroize::Zeroizing;

use super::{SERVICE, SecretRef, SecretStore};
use crate::i18n::{t, tn};

/// 真正的系统钥匙串。
///
/// macOS 上未签名的开发构建每次重新编译后都会被 Keychain Services 当成另一个
/// 程序，于是弹一次系统授权框。那个框由 SecurityAgent 进程绘制，只会卡住发起
/// 调用的线程，所以调用方必须待在后台线程上。
pub struct KeychainSecretStore {
    available: bool,
}

impl KeychainSecretStore {
    /// 探测平台钥匙串。第一次调用会初始化后端，失败说明这台机器上用不了。
    pub fn new() -> Self {
        Self {
            available: Entry::store_status().is_ok(),
        }
    }

    fn entry(&self, secret: &SecretRef) -> Result<Entry> {
        Entry::new(SERVICE, &secret.account()).map_err(|error| describe(Action::Open, error))
    }
}

impl Default for KeychainSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeychainSecretStore {
    fn get(&self, secret: &SecretRef) -> Result<Option<Zeroizing<String>>> {
        match self.entry(secret)?.get_password() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(Error::NoEntry) => Ok(None),
            Err(error) => Err(describe(Action::Read, error)),
        }
    }

    fn set(&self, secret: &SecretRef, value: &str) -> Result<()> {
        self.entry(secret)?
            .set_password(value)
            .map_err(|error| describe(Action::Write, error))
    }

    fn delete(&self, secret: &SecretRef) -> Result<()> {
        match self.entry(secret)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(describe(Action::Delete, error)),
        }
    }

    fn is_available(&self) -> bool {
        self.available
    }
}

/// 把钥匙串错误说成界面语言的话。`keyring` 的 `Display` 从不打印秘密本身，但仍然只在
/// 兜底分支里用它，免得将来上游改了实现把明文带出来。
fn describe(action: Action, error: Error) -> anyhow::Error {
    let reason = match error {
        Error::NoDefaultStore => t!("secrets.keychain.no_store"),
        Error::NoStorageAccess(_) => t!("secrets.keychain.no_access"),
        Error::BadEncoding(_) => t!("secrets.keychain.bad_encoding"),
        Error::Ambiguous(items) => tn!("secrets.keychain.ambiguous", items.len()),
        Error::TooLong(_, limit) => t!("secrets.keychain.too_long", limit = limit),
        other => other.to_string().into(),
    };
    anyhow!(match action {
        Action::Open => t!("secrets.keychain.open_failed", reason = reason),
        Action::Read => t!("secrets.keychain.read_failed", reason = reason),
        Action::Write => t!("secrets.keychain.write_failed", reason = reason),
        Action::Delete => t!("secrets.keychain.delete_failed", reason = reason),
    })
}

/// What was done to a keychain entry, for the error that says it failed.
#[derive(Clone, Copy)]
enum Action {
    Open,
    Read,
    Write,
    Delete,
}

#[cfg(test)]
mod tests {
    use super::KeychainSecretStore;
    use crate::secrets::{SecretRef, SecretStore};

    /// 真钥匙串的往返测试。会在当前用户的钥匙串里建一条临时条目，还可能弹出
    /// 系统授权框，所以默认不跑：`cargo test -- --ignored keychain`。
    #[test]
    #[ignore = "读写真实的系统钥匙串"]
    fn the_system_keychain_round_trips() {
        let store = KeychainSecretStore::new();
        assert!(store.is_available(), "这台机器上没有可用的系统钥匙串");
        let secret = SecretRef::password("shellrs-test", "invalid.example", 22);

        store.delete(&secret).unwrap();
        assert!(store.get(&secret).unwrap().is_none());

        store.set(&secret, "hunter2").unwrap();
        assert_eq!(
            store.get(&secret).unwrap().as_deref().map(String::as_str),
            Some("hunter2")
        );

        store.delete(&secret).unwrap();
        assert!(store.get(&secret).unwrap().is_none());
    }
}
