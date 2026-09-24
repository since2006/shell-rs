//! Real local/remote file browsing and SFTP upload UI.

mod explorer_panel;
mod file_dialogs;
mod file_listing;
mod file_pane;
mod history;
mod model;
mod pane_menu;
mod pane_operations;
mod properties_dialog;
mod selection;

pub use explorer_panel::{ExplorerPanel, ExplorerPanelEvent};
pub use file_dialogs::validate_entry_name;
pub use file_listing::FileListing;
pub use file_pane::{FilePane, PaneSide};
pub use history::{LoadIntent, NavigationHistory};
pub use model::*;
pub use properties_dialog::PermissionDraft;
pub use selection::{ClickMode, CursorMotion, Selection};

mod transfer_dialog;
pub use transfer_dialog::confirm_close_transfer;
