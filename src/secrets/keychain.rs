use anyhow::{Result, anyhow};
use keyring::{Entry, Error};
use zeroize::Zeroizing;

use super::{SERVICE, SecretRef, SecretStore};

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
        Entry::new(SERVICE, &secret.account()).map_err(|error| describe("打开", error))
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
            Err(error) => Err(describe("读取", error)),
        }
    }

    fn set(&self, secret: &SecretRef, value: &str) -> Result<()> {
        self.entry(secret)?
            .set_password(value)
            .map_err(|error| describe("写入", error))
    }

    fn delete(&self, secret: &SecretRef) -> Result<()> {
        match self.entry(secret)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(error) => Err(describe("删除", error)),
        }
    }

    fn is_available(&self) -> bool {
        self.available
    }
}

/// 把钥匙串错误翻成中文。`keyring` 的 `Display` 从不打印秘密本身，但仍然只在
/// 兜底分支里用它，免得将来上游改了实现把明文带出来。
fn describe(action: &str, error: Error) -> anyhow::Error {
    let reason = match error {
        Error::NoDefaultStore => "系统钥匙串不可用".to_string(),
        Error::NoStorageAccess(_) => "系统钥匙串被锁定或拒绝访问".to_string(),
        Error::BadEncoding(_) => "已保存的内容不是有效的 UTF-8 文本".to_string(),
        Error::Ambiguous(items) => format!("钥匙串里有 {} 条同名条目", items.len()),
        Error::TooLong(_, limit) => format!("超过系统钥匙串 {limit} 个字符的长度上限"),
        other => other.to_string(),
    };
    anyhow!("{action}系统钥匙串条目失败：{reason}")
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
