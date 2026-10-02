//! Small presentation pieces shared by more than one feature.

mod closable_tab;
mod count_tabs;
mod dialog;
mod format;
mod host_mark;
mod latency_label;
mod rename_tab_dialog;
mod row_tooltip;
mod segmented_control;
mod tab_menu;
mod tint;

pub use closable_tab::ClosableTabTitle;
pub use count_tabs::count_tabs;
pub use dialog::{
    DeleteHandler, commit_footer, confirm_danger, confirm_delete, dismiss_form_error,
    form_error_notification, parse_port,
};
pub use format::{format_bytes, format_duration, format_percent};
pub use host_mark::HostMark;
pub use latency_label::LatencyLabel;
pub use rename_tab_dialog::{RenamableTab, open_rename_tab_dialog};
pub use row_tooltip::{RowTooltip, RowTooltipTrigger, RowTooltips};
pub use segmented_control::{Segment, SegmentedControl};
pub use tab_menu::close_tab_items;
pub use tint::{soft_tag, tinted};
