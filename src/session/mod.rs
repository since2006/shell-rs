//! 主机管理: the saved hosts, which the code calls sessions. The session
//! model, its store, and the session panel/dialog.

mod credential;
mod database;
mod forward;
mod group_dialog;
mod login;
mod model;
mod outline;
mod private_key;
mod secret_fields;
mod session_dialog;
mod session_panel;
mod store;

pub use credential::*;
pub use database::{SessionDatabase, StoredData};
pub use forward::*;
pub use group_dialog::{GroupForm, confirm_delete_group, open_group_dialog};
pub use login::{JumpLogin, LoginMethod, LoginRoute, ProxyLogin, SessionLogin};
pub use model::*;
pub use outline::{NodeDrop, SessionNode, group_options, matches_query, session_tree_items};
pub use private_key::{GeneratedKey, KeyAlgorithm, PastedKey, PastedKeyError, read_public_key};
pub use secret_fields::SecretFields;
pub use session_dialog::{
    DeleteHandler, Dependents, SessionForm, confirm_delete_session, open_session_dialog,
};
pub use session_panel::{SessionPanel, session_menu};
pub use store::{SessionStore, SessionStoreEvent};
