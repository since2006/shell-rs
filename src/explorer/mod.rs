//! Real local/remote file browsing and SFTP upload UI.

mod explorer_panel;
mod file_dialogs;
mod file_listing;
mod file_pane;
mod history;
mod model;
mod open_directory_dialog;
mod pane_menu;
mod pane_operations;
mod path_label;
mod properties_dialog;
mod queue_panel;
mod selection;
mod transfer_queue;

pub use explorer_panel::{ExplorerPanel, ExplorerPanelEvent};
pub use file_dialogs::validate_entry_name;
pub use file_listing::FileListing;
pub use file_pane::{FilePane, PaneSide};
pub use history::{LoadIntent, NavigationHistory};
pub use model::*;
pub use properties_dialog::PermissionDraft;
pub use selection::{ClickMode, CursorMotion, Selection};
pub use transfer_queue::{
    QueueEntry, QueueId, QueueState, Removal, TransferJob, TransferQueue, percent,
};

mod transfer_dialog;
pub use transfer_dialog::confirm_close_transfer;
