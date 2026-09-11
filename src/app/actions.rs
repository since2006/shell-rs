//! Every user command, so toolbar buttons, menus and key bindings dispatch
//! the same action and cannot disagree.

use gpui_kit::*;
use serde::Deserialize;

use crate::{session::SessionId, terminal::LocalTerminalId};

gpui_kit::actions!(
    shellr,
    [
        /// Open the new-session dialog.
        NewSession,
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

macro_rules! local_terminal_action {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Action, Clone, PartialEq, Eq, Deserialize)]
        #[action(namespace = shellr, no_json)]
        pub struct $name(pub LocalTerminalId);
    };
}

session_action!(
    /// Open (or activate) the terminal tab of a session.
    ConnectSession
);
session_action!(
    /// Mark a session disconnected.
    DisconnectSession
);
session_action!(
    /// Reconnect a session (mock: a short "connecting" state).
    ReconnectSession
);
session_action!(
    /// Open (or activate) the SFTP explorer tab of a session.
    OpenExplorer
);
session_action!(
    /// Close the terminal tab of a session.
    CloseTerminal
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

local_terminal_action!(
    /// Close a local terminal tab and terminate its child process.
    CloseLocalTerminal
);
local_terminal_action!(
    /// Restart a local terminal with a fresh emulator and PTY.
    RestartLocalTerminal
);
