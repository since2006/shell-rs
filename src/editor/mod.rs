//! The built-in editor: a center tab that edits one text file, a remote one
//! over the SFTP tab that opened it, or a local one.

mod editor_panel;
mod model;

pub use editor_panel::{EditorPanel, EditorPanelEvent, EditorSource};
pub use model::*;
