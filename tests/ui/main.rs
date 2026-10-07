//! UI integration tests: the production `Workspace` rendered in a headless
//! window, driven through real pointer and keyboard events.
//!
//! One test binary, one module per feature: every file directly under
//! `tests/` would be a binary of its own, linking the whole GPUI stack
//! again. `support` holds the fixtures and fakes the modules share.

mod support;

mod analytics;
mod cli;
mod connection;
mod credential;
mod docker;
mod editor;
mod forward;
mod history;
mod host_dialog;
mod host_tree;
mod monitor;
mod netstat;
mod processes;
mod services;
mod settings;
mod sftp;
mod shortcuts;
mod snippets;
mod terminal;
mod transfer;
mod update;
mod workspace;
