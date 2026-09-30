//! Small presentation pieces shared by more than one feature.

mod closable_tab;
mod dialog;
mod host_mark;
mod rename_tab_dialog;
mod tab_menu;

pub use closable_tab::ClosableTabTitle;
pub use dialog::{commit_footer, form_error};
pub use host_mark::HostMark;
pub use rename_tab_dialog::{RenamableTab, open_rename_tab_dialog};
pub use tab_menu::close_tab_items;
