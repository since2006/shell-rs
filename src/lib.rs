//! ShellRS: an SSH host manager with real SSH and local terminals.
//!
//! Modules are organized by capability so they can become crates later:
//! `host` (主机管理), `terminal` (终端), `ssh` (SSH 连接), `sftp` (SFTP
//! 传输), `explorer` (SFTP 文件浏览), `forward` (端口转发), `credential`
//! (凭据), `docker` (Docker), `monitor` (系统监控), `services` (系统服务), `processes` (进程管理),
//! `netstat` (网络连接), `history` (历史命令), `snippets` (命令片段), `editor` (内置编辑器),
//! `secrets` (系统钥匙串), `settings` (设置), `cli` (外部 CLI), `update` (在线升级), `analytics`
//! (匿名使用统计), `connection` 与 `shared` (跨模块共用), `i18n` (多语言), `workspace` (窗口壳), `app` (动作、快捷键、资源).

pub mod analytics;
pub mod app;
pub mod cli;
pub mod connection;
pub mod credential;
pub mod docker;
pub mod editor;
pub mod explorer;
pub mod forward;
pub mod history;
pub mod host;
pub mod i18n;
pub mod monitor;
pub mod netstat;
pub mod processes;
pub mod secrets;
pub mod services;
pub mod settings;
pub mod sftp;
pub mod shared;
pub mod snippets;
pub mod ssh;
pub mod ssh_agent;
pub mod terminal;
pub mod update;
pub mod workspace;

#[cfg(test)]
mod testing;

// The text of every language, from `locales/`: see `i18n`.
rust_i18n::i18n!("locales", fallback = "zh-CN");

/// Initialize GPUI Kit, locale, key bindings and global actions. Call once
/// inside `Application::run` before creating any view.
pub fn init(cx: &mut gpui_kit::App) {
    // The identity the system shows notifications under, set before any
    // window opens: on Windows the AppUserModelID, the same as the macOS
    // bundle's.
    cx.set_app_identity("com.shellrs.ShellRS", "ShellRS");
    app::init(cx);
}
