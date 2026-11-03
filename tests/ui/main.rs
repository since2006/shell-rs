//! UI integration tests: the production `Workspace` rendered in a headless
//! window, driven through real pointer and keyboard events.
//!
//! One test binary, one module per feature: every file directly under
//! `tests/` would be a binary of its own, linking the whole GPUI stack
//! again. `support` holds the fixtures and fakes the modules share.

mod support;

mod analytics;
mod app_lock;
mod cli;
mod compose;
mod connection;
mod credential;
mod docker;
mod editor;
mod file_sidebar;
mod forward;
mod gpu;
mod history;
mod host_dialog;
mod host_import;
mod host_tree;
mod kubernetes;
mod monitor;
mod netstat;
mod processes;
mod projects;
mod serial;
mod services;
mod settings;
mod sftp;
mod shortcuts;
mod snippets;
mod sync;
mod telnet;
mod terminal;
mod terminal_groups;
mod tmux;
mod transfer;
mod update;
mod workspace;
mod zmodem;
