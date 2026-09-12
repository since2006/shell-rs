//! Every user command, so toolbar buttons, menus and key bindings dispatch
//! the same action and cannot disagree.

use gpui_kit::*;
use serde::Deserialize;

use crate::{
    session::{GroupId, SessionId},
    terminal::{LocalTerminalId, RemoteTerminalId},
};

gpui_kit::actions!(
    shellr,
    [
        /// Open the new-session dialog for a session at the root of the tree.
        NewSession,
        /// Open the new-group dialog for a top-level group.
        NewGroup,
        /// Open a new local login-shell terminal.
        NewLocalTerminal,
        /// Copy the active terminal selection.
        CopyTerminal,
        /// Paste the clipboard into the active terminal.
        PasteTerminal,
        /// Show or hide the left session dock.
        ToggleSessionPanel,
        /// Switch between the light and dark theme.
        ToggleTheme,
        /// Move keyboard focus to the session search field.
        FocusSearch,
        /// Connect the session selected in the tree (Enter in the tree).
        ConnectSelected,
        /// Close the center tab displayed most recently (the one ⌘W acts on).
        CloseActiveTab,
        /// Increase the application base font (interface zoom).
        ZoomIn,
        /// Decrease the application base font (interface zoom).
        ZoomOut,
        /// Reset the application base font.
        ZoomReset,
        /// Quit the application.
        Quit,
    ]
);

macro_rules! session_action {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Action, Clone, PartialEq, Eq, Deserialize)]
        #[action(namespace = shellr, no_json)]
        pub struct $name(pub SessionId);
    };
}

macro_rules! group_action {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Action, Clone, PartialEq, Eq, Deserialize)]
        #[action(namespace = shellr, no_json)]
        pub struct $name(pub GroupId);
    };
}

macro_rules! local_terminal_action {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Action, Clone, PartialEq, Eq, Deserialize)]
        #[action(namespace = shellr, no_json)]
        pub struct $name(pub LocalTerminalId);
    };
}

macro_rules! remote_terminal_action {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Action, Clone, PartialEq, Eq, Deserialize)]
        #[action(namespace = shellr, no_json)]
        pub struct $name(pub RemoteTerminalId);
    };
}

session_action!(
    /// Open a new terminal connection for a session.
    ConnectSession
);
session_action!(
    /// Mark a session disconnected.
    DisconnectSession
);
session_action!(
    /// Open (or activate) the SFTP explorer tab of a session.
    OpenExplorer
);
session_action!(
    /// Close the SFTP explorer tab of a session.
    CloseExplorer
);
session_action!(
    /// Open the edit-session dialog for a session.
    EditSession
);
session_action!(
    /// Duplicate a session in the same group.
    DuplicateSession
);
session_action!(
    /// Ask for confirmation, then delete a session.
    DeleteSession
);

group_action!(
    /// Open the new-session dialog with this group pre-selected.
    NewSessionInGroup
);
group_action!(
    /// Open the new-group dialog for a group nested inside this one.
    NewChildGroup
);
group_action!(
    /// Open the rename-group dialog.
    RenameGroup
);
group_action!(
    /// Ask for confirmation, then delete a group with everything inside it.
    DeleteGroup
);

local_terminal_action!(
    /// Close a local terminal tab and terminate its child process.
    CloseLocalTerminal
);

remote_terminal_action!(
    /// Close one remote terminal connection and its tab.
    CloseTerminal
);
remote_terminal_action!(
    /// Reconnect one remote terminal using the session's latest settings.
    ReconnectTerminal
);
local_terminal_action!(
    /// Restart a local terminal with a fresh emulator and PTY.
    RestartLocalTerminal
);
