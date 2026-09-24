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
        /// Open the active terminal's find bar, or select its query again.
        FindInTerminal,
        /// Focus the next match down (newer output) in the active terminal.
        FindNextInTerminal,
        /// Focus the next match up (older output) in the active terminal.
        FindPreviousInTerminal,
        /// Close the active terminal's find bar and its highlights.
        DismissTerminalFind,
        /// Clear the active terminal's screen and scrollback, keeping the
        /// prompt line.
        ClearTerminal,
        /// Show or hide the left session dock.
        ToggleSessionPanel,
        /// Switch between the light and dark theme.
        ToggleTheme,
        /// Move keyboard focus to the session search field.
        FocusSearch,
        /// Connect the session selected in the focused session list.
        ConnectSelected,
        /// Expand every group in the session tree.
        ExpandAllGroups,
        /// Collapse every group in the session tree.
        CollapseAllGroups,
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
session_action!(
    /// Copy a session's host field (an IP address or a host name) to the
    /// clipboard.
    CopySessionHost
);

group_action!(
    /// Open a terminal connection for every session in this group's subtree.
    ConnectGroup
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
remote_terminal_action!(
    /// Disconnect one remote terminal, keeping its tab to reconnect from.
    DisconnectTerminal
);
remote_terminal_action!(
    /// Open the dialog that gives one remote terminal tab its own title.
    RenameTerminal
);
local_terminal_action!(
    /// Restart a local terminal with a fresh emulator and PTY.
    RestartLocalTerminal
);

/// One tab of the center area, by the identity of what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum CenterTab {
    Terminal(RemoteTerminalId),
    Explorer(SessionId),
    LocalTerminal(LocalTerminalId),
}

/// Which tabs of a tab bar a batch close takes, relative to one tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum CloseScope {
    Left,
    Right,
    Others,
    All,
}

impl CloseScope {
    /// The indexes to close in a bar of `len` tabs, relative to the tab at
    /// `ix`. Empty when there is nothing to close, which is also how a menu
    /// decides to disable the command.
    pub fn targets(self, len: usize, ix: usize) -> Vec<usize> {
        match self {
            CloseScope::Left => (0..ix.min(len)).collect(),
            CloseScope::Right => (ix + 1..len).collect(),
            CloseScope::Others => (0..len).filter(|&other| other != ix).collect(),
            CloseScope::All => (0..len).collect(),
        }
    }
}

/// Close several tabs of the tab bar that holds `tab`, each through its own
/// close path.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = shellr, no_json)]
pub struct CloseTabs {
    pub tab: CenterTab,
    pub scope: CloseScope,
}

/// Commands shared by explorer controls, drops, dialogs and keyboard bindings.
/// `remote` names the pane a command acts on.
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
    Root {
        remote: bool,
    },
    Home {
        remote: bool,
    },
    Back {
        remote: bool,
    },
    Forward {
        remote: bool,
    },
    /// Enter or double-click: open the directory under the cursor.
    Open {
        remote: bool,
    },
    /// Move keyboard focus to a pane's file list.
    FocusPane {
        remote: bool,
    },
    MoveCursor {
        remote: bool,
        motion: crate::explorer::CursorMotion,
        extend: bool,
    },
    ToggleSelection {
        remote: bool,
    },
    SelectAll {
        remote: bool,
    },
    AddBookmark {
        remote: bool,
    },
    RemoveBookmark {
        remote: bool,
        path: String,
    },
    /// F5: upload the local selection or download the remote one.
    Transfer {
        remote: bool,
    },
    /// Ask where to download these remote paths, starting from `target`.
    DownloadPaths {
        paths: Vec<String>,
        target: String,
    },
    BeginDownload {
        paths: Vec<String>,
        target: String,
    },
    /// Ask to delete the pane's selection.
    Delete {
        remote: bool,
    },
    BeginDelete {
        remote: bool,
        names: Vec<String>,
    },
    Rename {
        remote: bool,
    },
    CommitRename {
        remote: bool,
        from: String,
        to: String,
    },
    New {
        remote: bool,
        kind: crate::explorer::NewEntryKind,
    },
    CommitNew {
        remote: bool,
        kind: crate::explorer::NewEntryKind,
        name: String,
    },
    Properties {
        remote: bool,
    },
    ApplyPermissions {
        remote: bool,
        names: Vec<String>,
        edit: crate::sftp::PermissionEdit,
        recursive: bool,
        add_x_to_dirs: bool,
    },
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
        answer: crate::sftp::TransferAnswer,
    },
    CancelTransfer,
    ResumeTransfer,
    DiscardTransfer,
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
/// A file-list key binding. The binding carries the pane side, and the
/// workspace sends it to the explorer that holds focus.
#[derive(Action, Clone, PartialEq, Eq, Deserialize)]
#[action(namespace = shellr, no_json)]
pub struct ExplorerShortcut(pub ExplorerCommand);

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

#[cfg(test)]
mod tests {
    use super::CloseScope;

    #[test]
    fn close_scopes_are_relative_to_the_anchor_tab() {
        assert_eq!(CloseScope::Left.targets(4, 2), vec![0, 1]);
        assert_eq!(CloseScope::Right.targets(4, 2), vec![3]);
        assert_eq!(CloseScope::Others.targets(4, 2), vec![0, 1, 3]);
        assert_eq!(CloseScope::All.targets(4, 2), vec![0, 1, 2, 3]);
    }

    #[test]
    fn close_scopes_are_empty_at_the_edges() {
        assert!(CloseScope::Left.targets(3, 0).is_empty());
        assert!(CloseScope::Right.targets(3, 2).is_empty());
        assert!(CloseScope::Others.targets(1, 0).is_empty());
        assert_eq!(CloseScope::All.targets(1, 0), vec![0]);
    }
}
