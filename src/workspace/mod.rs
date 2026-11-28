//! The window shell: title bar, dock area with its sidebars, the switch of
//! the right sidebar's tools, status bar, and the start page (recent hosts)
//! the center shows while no tab is open.

mod analytics;
mod app_lock;
mod app_shell;
mod assistant;
mod cli_changes;
mod compose;
mod compose_history;
mod compose_pane;
mod credentials;
mod dock_skin;
mod editors;
mod follow;
mod forwards;
mod front;
mod imports;
mod links;
mod local_files;
mod notices;
mod projects;
mod promotions;
mod quitting;
mod recent_hosts;
mod recording;
mod sidebar;
mod sidebar_widths;
mod snippets;
mod status_bar;
mod sync;
mod tabs;
mod terminal_groups;
mod title_bar;
mod tmux;
mod tool_sidebar;
mod tools;
mod updates;
mod window_state;
mod workspace_view;
mod zmodem;

pub use links::open_link_once_open;
pub use tool_sidebar::ToolTerminal;
pub use window_state::{WindowState, keep_window_place};
pub use workspace_view::{Workspace, notify_once_open, window_options};

pub use app_shell::{AppShell, StartupFailure};
