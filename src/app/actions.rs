//! Every user command, so toolbar buttons, menus and key bindings dispatch
//! the same action and cannot disagree.

use gpui_kit::*;
use serde::Deserialize;

use crate::{
    session::{GroupId, NodeDrop, SessionId, SessionNode},
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
        /// Connect the session selected in the focused session list.
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

/// Move a session-tree row by dropping it beside a peer or into a group.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = shellr, no_json)]
pub struct MoveSessionNode {
    pub source: SessionNode,
    pub destination: NodeDrop,
}
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

/// Commands shared by explorer controls, drops, dialogs and keyboard bindings.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub enum ExplorerCommand {
    Navigate {
        remote: bool,
        path: String,
    },
    Up {
        remote: bool,
    },
    Refresh {
        remote: bool,
    },
    Check {
        name: String,
        checked: bool,
        extend: bool,
    },
    ToggleSelection,
    SelectAll,
    UploadSelected,
    ChooseFiles,
    UploadPaths {
        paths: Vec<std::path::PathBuf>,
        target: String,
    },
    BeginUpload {
        paths: Vec<std::path::PathBuf>,
        target: String,
    },
    Answer {
        request_id: u64,
        answer: crate::sftp::UploadAnswer,
    },
    CancelUpload,
    ResumeUpload,
    DiscardUpload,
    ToggleDetails,
    CloseConfirmed,
}
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = shellr, no_json)]
pub struct ExplorerAction {
    session: SessionId,
    command: ExplorerCommand,
    generation: Option<u64>,
}
impl ExplorerAction {
    pub fn new(session: SessionId, command: ExplorerCommand) -> Self {
        Self {
            session,
            command,
            generation: None,
        }
    }
    pub fn with_generation(mut self, generation: u64) -> Self {
        self.generation = Some(generation);
        self
    }
    pub fn generation(&self) -> Option<u64> {
        self.generation
    }
    pub fn session(&self) -> SessionId {
        self.session
    }
    pub fn command(&self) -> &ExplorerCommand {
        &self.command
    }
}
gpui_kit::actions!(
    shellr,
    [
        UploadSelectedFiles,
        ToggleUploadSelection,
        SelectAllUploadFiles
    ]
);

/// UI entities dispatch after their update has finished so Workspace can read
/// their latest snapshot without re-entering a leased entity.
pub(crate) trait ExplorerDispatch {
    fn dispatch_explorer_action(
        &self,
        action: &ExplorerAction,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::App,
    );
}
impl ExplorerDispatch for gpui_kit::FocusHandle {
    fn dispatch_explorer_action(
        &self,
        action: &ExplorerAction,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::App,
    ) {
        let focus = self.clone();
        let action = action.clone();
        window.defer(cx, move |window, cx| {
            focus.dispatch_action(&action, window, cx)
        });
    }
}
