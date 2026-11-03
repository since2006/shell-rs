//! 主机管理: the saved hosts, which the code calls hosts. The hosts core
//! (`shellrs-hosts`) as the app uses it, and what is the app's own: the
//! GPUI store around its data, the words for its labels and errors
//! (`describe`), the host panel, dialogs, tree and import.

mod describe;
mod group_dialog;
mod host_dialog;
mod host_panel;
mod import;
mod outline;
mod quick_connect;
mod secret_fields;
mod ssh_link;
mod state;
mod store;

pub use describe::{
    ErrorMessage, KindLabel, credential_summary, matches_credential_query, matches_forward_query,
    secret_error,
};
pub use group_dialog::{GroupForm, confirm_delete_group, open_group_dialog};
pub use host_dialog::{
    DeleteHandler, Dependents, HostForm, confirm_delete_host, join_sentences, open_host_dialog,
    open_temporary_connection_dialog,
};
pub use host_panel::{HostPanel, host_menu, import_menu_items};
pub use import::{ImportHandler, ImportOutcome, ImportSource, open_import_dialog};
pub use outline::{first_matching_host, group_options, host_tree_items, matches_query};
pub use quick_connect::{QuickChoice, QuickHost, open_quick_connect_dialog, quick_hosts};
pub use secret_fields::SecretFields;
pub use shellrs_hosts::*;
pub use ssh_link::{LinkKind, SshLink, standard_url};
pub use state::ConnectionState;
pub use store::{HostStore, HostStoreEvent};
