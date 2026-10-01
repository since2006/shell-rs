//! The window shell: title bar, dock area with its sidebar, status bar, and
//! the start page (recent hosts) the center shows while no tab is open.

mod credentials;
mod dock_skin;
mod forwards;
mod recent_hosts;
mod sidebar;
mod status_bar;
mod title_bar;
mod updates;
mod workspace_view;

pub use workspace_view::{Workspace, notify_once_open, window_options};
