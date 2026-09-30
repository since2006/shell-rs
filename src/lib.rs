//! ShellRS: an SSH session manager with real SSH and local terminals.
//!
//! Modules are organized by capability so they can become crates later:
//! `session` (会话管理), `terminal` (终端), `ssh` (SSH 连接), `sftp` (SFTP
//! 传输), `explorer` (SFTP 文件浏览), `secrets` (系统钥匙串), `settings`
//! (设置), `cli` (外部 CLI), `connection` 与 `shared` (跨模块共用),
//! `workspace` (窗口壳), `app` (动作、快捷键、资源).

pub mod app;
pub mod cli;
pub mod connection;
pub mod explorer;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod sftp;
pub mod shared;
pub mod ssh;
pub mod terminal;
pub mod workspace;

/// Initialize GPUI Kit, locale, key bindings and global actions. Call once
/// inside `Application::run` before creating any view.
pub fn init(cx: &mut gpui_kit::App) {
    app::init(cx);
}
