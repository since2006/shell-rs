//! The SFTP tab: local and remote file browsing, transfers and their queue.

mod explorer_panel;
mod file_dialogs;
mod file_edit;
mod file_icons;
mod file_icons_table;
mod file_listing;
mod file_pane;
mod find;
mod find_window;
mod history;
mod image_preview;
mod marquee;
mod model;
mod open_directory_dialog;
mod pane_menu;
mod pane_operations;
mod path_label;
mod preview;
mod properties_dialog;
mod queue_panel;
mod selection;
mod sync_browsing;
mod transfer_queue;

pub use explorer_panel::{ExplorerPanel, ExplorerPanelEvent, ExplorerStatus};
pub use file_edit::FileLocation;
pub use file_icons::{FileIcon, FileIconKind, FileIconStyle};
pub use file_icons_table::FILE_ICON_ASSETS;
pub use file_listing::FileListing;
pub use file_pane::{FilePane, FilePaneEvent, PaneSide};
pub use history::{LoadIntent, NavigationHistory};
pub use model::*;
pub use pane_operations::PaneOperation;
pub use preview::{IMAGE_LIMIT, PreviewKind};
pub use selection::{ClickMode, CursorMotion, Selection};
pub use transfer_queue::{
    QueueEntry, QueueId, QueueState, Removal, TransferIndicator, TransferJob, TransferQueue,
    percent,
};

mod transfer_dialog;
pub use transfer_dialog::confirm_close_transfer;
