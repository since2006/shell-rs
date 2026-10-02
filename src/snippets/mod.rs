//! 命令片段: the right sidebar's tool that keeps commands for every host,
//! in categories one level deep, and puts them on the input line of the
//! SSH terminal in front. The snippets themselves are in the host store
//! (`host::Snippet`); this is their list and their dialogs.

mod model;
mod snippet_dialog;
mod snippet_panel;

pub use snippet_dialog::{
    CategoryDialog, SnippetDialog, open_category_dialog, open_snippet_dialog,
};
pub use snippet_panel::SnippetPanel;
