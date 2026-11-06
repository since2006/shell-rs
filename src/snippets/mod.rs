//! 命令片段: the right sidebar's tool that keeps commands for every host,
//! in categories that nest as host groups do, and puts them on the input
//! line of the terminal in front, an SSH terminal or a local one. The
//! snippets themselves are in the host store (`host::Snippet`); this is
//! their list and their dialogs.

mod model;
mod snippet_dialog;
mod snippet_panel;

pub use model::category_options;
pub use snippet_dialog::{
    CategoryDialog, SnippetDialog, open_category_dialog, open_snippet_dialog,
};
pub use snippet_panel::SnippetPanel;
