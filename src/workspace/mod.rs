//! The window shell: title bar, dock area, status bar, and the start page
//! (recent sessions) the center shows while no tab is open.

mod dock_skin;
mod recent_sessions;
mod status_bar;
mod title_bar;
mod workspace_view;

pub use workspace_view::{Workspace, window_options};
