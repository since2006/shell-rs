//! 会话管理: the session model, its store, and the session panel/dialog.

mod model;
mod outline;
mod session_dialog;
mod session_panel;
mod store;

pub use model::*;
pub use outline::{SessionNode, matches_query, session_tree_items};
pub use session_dialog::{DeleteHandler, SessionForm, confirm_delete_session, open_session_dialog};
pub use session_panel::SessionPanel;
pub use store::SessionStore;
