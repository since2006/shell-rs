use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, bail};
use zeroize::Zeroizing;

use super::{SecretRef, SecretStore};
use crate::i18n::t;

/// 进程内的秘密存储，给测试用：行为和真钥匙串一致，但什么都不落地。
#[derive(Default)]
pub struct InMemorySecretStore {
    entries: Mutex<HashMap<String, Zeroizing<String>>>,
}

impl InMemorySecretStore {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Zeroizing<String>>> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// 测试断言用：当前存了几条。
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for InMemorySecretStore {
    fn get(&self, secret: &SecretRef) -> Result<Option<Zeroizing<String>>> {
        Ok(self.entries().get(&secret.account()).cloned())
    }

    fn set(&self, secret: &SecretRef, value: &str) -> Result<()> {
        self.entries()
            .insert(secret.account(), Zeroizing::new(value.to_string()));
        Ok(())
    }

    fn delete(&self, secret: &SecretRef) -> Result<()> {
        self.entries().remove(&secret.account());
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// 用在没有系统钥匙串的机器上：读永远是空，写明确报错，界面据此把密码字段
/// 禁用掉。也是 `HostStore` 的默认值，所以单元测试不会碰到任何秘密存储。
pub struct NoSecretStore;

impl SecretStore for NoSecretStore {
    fn get(&self, _: &SecretRef) -> Result<Option<Zeroizing<String>>> {
        Ok(None)
    }

    fn set(&self, _: &SecretRef, _: &str) -> Result<()> {
        bail!(t!("secrets.unavailable"))
    }

    fn delete(&self, _: &SecretRef) -> Result<()> {
        Ok(())
    }

    fn is_available(&self) -> bool {
        false
    }
}
