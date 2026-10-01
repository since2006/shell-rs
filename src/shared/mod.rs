//! Small presentation pieces shared by more than one feature.

mod closable_tab;
mod dialog;
mod host_mark;
mod latency_label;
mod rename_tab_dialog;
mod segmented_control;
mod tab_menu;

pub use closable_tab::ClosableTabTitle;
pub use dialog::{
    DeleteHandler, commit_footer, confirm_delete, dismiss_form_error, form_error_notification,
    parse_port,
};
pub use host_mark::HostMark;
pub use latency_label::LatencyLabel;
pub use rename_tab_dialog::{RenamableTab, open_rename_tab_dialog};
pub use segmented_control::{Segment, SegmentedControl};
pub use tab_menu::close_tab_items;
