//! 主机管理: the saved hosts, which the code calls hosts. The host
//! model, its store, and the host panel/dialog.

mod credential;
mod database;
mod forward;
mod group_dialog;
mod host_dialog;
mod host_panel;
mod login;
mod model;
mod outline;
mod private_key;
mod quick_connect;
mod secret_fields;
mod snippet;
mod ssh_link;
mod store;

pub use credential::*;
pub use database::{HostDatabase, StoredData};
pub use forward::*;
pub use group_dialog::{GroupForm, confirm_delete_group, open_group_dialog};
pub use host_dialog::{
    DeleteHandler, Dependents, HostForm, confirm_delete_host, open_host_dialog,
    open_temporary_connection_dialog,
};
pub use host_panel::{HostPanel, host_menu};
pub use login::{HostLogin, JumpLogin, LoginMethod, LoginRoute, ProxyLogin};
pub use model::*;
pub use outline::{
    HostNode, NodeDrop, first_matching_host, group_options, host_tree_items, matches_query,
};
pub use private_key::{GeneratedKey, KeyAlgorithm, PastedKey, PastedKeyError, read_public_key};
pub use quick_connect::{QuickChoice, QuickHost, open_quick_connect_dialog, quick_hosts};
pub use secret_fields::SecretFields;
pub use snippet::*;
pub use ssh_link::{LinkKind, SshLink};
pub use store::{HostStore, HostStoreEvent};
