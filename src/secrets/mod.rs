//! 系统钥匙串（macOS Keychain Services / Windows 凭据管理器 / Secret Service）。
//!
//! ShellRS 的秘密只存在这里。读写接口和不碰钥匙串的实现在主机核心的 `secrets`，
//! 这里是系统钥匙串本身。

mod keychain;

use std::sync::Arc;

pub use keychain::KeychainSecretStore;
pub use shellrs_hosts::secrets::*;

/// 钥匙串里的服务名，所有条目共用。改了会让已保存的秘密全部失联。
pub const SERVICE: &str = "shellrs";

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
