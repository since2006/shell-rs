//! shellr: an SSH session manager with real SSH and local terminals.
//!
//! Modules are organized by capability so they can become crates later:
//! `session` (会话管理), `terminal` (终端), `explorer` (SFTP 文件浏览),
//! `workspace` (窗口壳), `app` (动作、快捷键、资源).

pub mod app;
pub mod explorer;
pub mod session;
pub mod shared;
pub mod ssh;
pub mod terminal;
pub mod workspace;

/// Initialize GPUI Kit, locale, key bindings and global actions. Call once
/// inside `Application::run` before creating any view.
pub fn init(cx: &mut gpui_kit::App) {
    app::init(cx);
}
