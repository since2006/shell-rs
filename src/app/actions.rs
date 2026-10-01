//! Every user command, so toolbar buttons, menus and key bindings dispatch
//! the same action and cannot disagree.

use gpui_kit::*;

use crate::{
    cli::AgentKind,
    explorer::{ExplorerId, FileSizeFormat},
    session::{CredentialId, ForwardId, GroupId, NodeDrop, SessionId, SessionNode},
    terminal::{LocalTerminalId, RemoteTerminalId},
};

gpui_kit::actions!(
    shellrs,
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
        /// Show or hide the left dock, whichever list it is showing.
        ToggleSessionPanel,
        /// Show the session list in the left dock.
        ShowSessions,
        /// Show the port-forwarding list in the left dock.
        ShowForwards,
        /// Open the new-forward dialog.
        NewForward,
        /// Start the forward selected in the forward list, or stop it if it
        /// is running.
        ToggleSelectedForward,
        /// Move the forward list's selection to the row above.
        SelectPreviousForward,
        /// Move the forward list's selection to the row below.
        SelectNextForward,
        /// Show the credential list in the left dock.
        ShowCredentials,
        /// Open the new-credential dialog.
        NewCredential,
        /// Open the new-credential dialog at a key it generates.
        GenerateCredentialKey,
        /// Open the edit dialog of the credential selected in the credential
        /// list.
        EditSelectedCredential,
        /// Move the credential list's selection to the row above.
        SelectPreviousCredential,
        /// Move the credential list's selection to the row below.
        SelectNextCredential,
        /// Switch between the light and dark theme.
        ToggleTheme,
        /// Move keyboard focus to the search field of the list the left dock
        /// is showing.
        FocusSearch,
        /// Connect the session selected in the focused session list.
        ConnectSelected,
        /// Expand every group in the session tree.
        ExpandAllGroups,
        /// Collapse every group in the session tree.
        CollapseAllGroups,
        /// Close the center tab displayed most recently (the one ⌘W acts on).
        CloseActiveTab,
        /// Open the settings tab, or bring it forward if it is already open.
        OpenSettings,
        /// Close the settings tab.
        CloseSettings,
        /// Increase the application base font (interface zoom).
        ZoomIn,
        /// Decrease the application base font (interface zoom).
        ZoomOut,
        /// Reset the application base font.
        ZoomReset,
        /// Quit the application.
        Quit,
        /// Put the `shellrs` command on the PATH.
        InstallCliCommand,
        /// Take the `shellrs` command off the PATH.
        RemoveCliCommand,
        /// Look again at what of the external CLI is installed.
        RefreshCliIntegration,
        /// Copy the agent skill's text to the clipboard.
        CopyAgentSkill,
    ]
);

/// Commands that carry the id of what they act on.
macro_rules! id_actions {
    ($($(#[$doc:meta])* $name:ident($id:ty);)*) => {
        $(
            $(#[$doc])*
            #[derive(Action, Clone, PartialEq, Eq)]
            #[action(namespace = shellrs, no_json)]
            pub struct $name(pub $id);
        )*
    };
}

id_actions! {
    /// Open a new terminal connection for a session.
    ConnectSession(SessionId);
    /// Mark a session disconnected.
    DisconnectSession(SessionId);
    /// Open a new SFTP tab for a session, like a new terminal connection.
    OpenExplorer(SessionId);
    /// Open the edit-session dialog for a session.
    EditSession(SessionId);
    /// Duplicate a session in the same group.
    DuplicateSession(SessionId);
    /// Ask for confirmation, then delete a session.
    DeleteSession(SessionId);
    /// Copy a session's host field (an IP address or a host name) to the
    /// clipboard.
    CopySessionHost(SessionId);
    /// Copy a session's public id to the clipboard, for another tool to
    /// name the machine by.
    CopySessionId(SessionId);

    /// Open a terminal connection for every session in this group's subtree.
    ConnectGroup(GroupId);
    /// Open the new-session dialog with this group pre-selected.
    NewSessionInGroup(GroupId);
    /// Open the new-group dialog for a group nested inside this one.
    NewChildGroup(GroupId);
    /// Open the rename-group dialog.
    RenameGroup(GroupId);
    /// Ask for confirmation, then delete a group with everything inside it.
    DeleteGroup(GroupId);

    /// Close one remote terminal connection and its tab.
    CloseTerminal(RemoteTerminalId);
    /// Reconnect one remote terminal using the session's latest settings.
    ReconnectTerminal(RemoteTerminalId);
    /// Disconnect one remote terminal, keeping its tab to reconnect from.
    DisconnectTerminal(RemoteTerminalId);
    /// Open the dialog that gives one remote terminal tab its own title.
    RenameTerminal(RemoteTerminalId);

    /// Close a local terminal tab and terminate its child process.
    CloseLocalTerminal(LocalTerminalId);
    /// Restart a local terminal with a fresh emulator and PTY.
    RestartLocalTerminal(LocalTerminalId);

    /// Close one SFTP tab.
    CloseExplorer(ExplorerId);
    /// Open the dialog that gives one SFTP tab its own title.
    RenameExplorer(ExplorerId);

    /// Install (or update) the agent skill for one agent.
    InstallAgentSkill(AgentKind);
    /// Remove the agent skill of one agent.
    RemoveAgentSkill(AgentKind);

    /// Show the SFTP 大小 column in another format, from its title's menu.
    SetFileSizeFormat(FileSizeFormat);

    /// Start a port forward over a connection of its own.
    StartForward(ForwardId);
    /// Stop a running port forward and log out its connection.
    StopForward(ForwardId);
    /// Open the edit-forward dialog.
    EditForward(ForwardId);
    /// Ask for confirmation, then delete a port-forwarding rule.
    DeleteForward(ForwardId);

    /// Open the edit-credential dialog.
    EditCredential(CredentialId);
    /// Copy the public half of a key credential's key, for a server's
    /// `authorized_keys`.
    CopyCredentialPublicKey(CredentialId);
    /// Ask for confirmation, then delete a credential. The hosts using it
    /// go back to logging in on their own.
    DeleteCredential(CredentialId);
}

/// Move a session-tree row by dropping it beside a peer or into a group.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct MoveSessionNode {
    pub source: SessionNode,
    pub destination: NodeDrop,
}

/// One tab of the center area, by the identity of what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenterTab {
    Terminal(RemoteTerminalId),
    Explorer(ExplorerId),
    LocalTerminal(LocalTerminalId),
    /// There is at most one settings tab.
    Settings,
}

/// Which tabs of a tab bar a batch close takes, relative to one tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct CloseTabs {
    pub tab: CenterTab,
    pub scope: CloseScope,
}

/// Commands shared by explorer controls, drops, dialogs and keyboard bindings.
/// `remote` names the pane a command acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// WinSCP's 打开目录/书签 dialog: type a directory or pick a bookmark.
    OpenDirectory {
        remote: bool,
    },
    CopyPath {
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
    /// Bookmark `path`, or the pane's directory when `None`.
    AddBookmark {
        remote: bool,
        path: Option<String>,
    },
    RemoveBookmark {
        remote: bool,
        path: String,
    },
    MoveBookmark {
        remote: bool,
        path: String,
        to: usize,
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
    /// Put a confirmed upload or download on the transfer queue.
    Enqueue(crate::explorer::TransferJob),
    /// Ask to delete the pane's selection.
    Delete {
        remote: bool,
    },
    /// Carry out a confirmed delete, rename, new item or permission change
    /// in the pane's directory.
    Operate {
        remote: bool,
        operation: crate::explorer::PaneOperation,
    },
    Rename {
        remote: bool,
    },
    New {
        remote: bool,
        kind: crate::explorer::NewEntryKind,
    },
    Properties {
        remote: bool,
    },
    ChooseFiles,
    UploadPaths {
        paths: Vec<std::path::PathBuf>,
        target: String,
    },
    Answer {
        request_id: u64,
        answer: crate::sftp::TransferAnswer,
    },
    /// Stop the running batch, keeping its progress for 继续.
    CancelTransfer,
    /// Go on with the stopped batch at the head of the queue, or reconnect.
    ResumeTransfer,
    /// 重新连接, as on a terminal tab: a fresh SFTP connection, dropping the
    /// current one first. Not while a batch runs or waits stopped.
    Reconnect,
    /// Throw away the stopped head's progress; the queue moves on.
    DiscardTransfer,
    /// Select a row of the transfer queue, by its id.
    SelectQueueEntry {
        id: u64,
    },
    /// Show or hide a queued batch's item results.
    ToggleQueueEntry {
        id: u64,
    },
    /// Take the selected batch, or a stopped head, off the queue.
    RemoveQueueEntry,
    /// 清除已完成.
    ClearFinishedTransfers,
    CloseConfirmed,
}
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct ExplorerAction {
    explorer: ExplorerId,
    command: ExplorerCommand,
    generation: Option<u64>,
}
impl ExplorerAction {
    pub fn new(explorer: ExplorerId, command: ExplorerCommand) -> Self {
        Self {
            explorer,
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
    pub fn explorer(&self) -> ExplorerId {
        self.explorer
    }
    pub fn command(&self) -> &ExplorerCommand {
        &self.command
    }
}
/// A file-list key binding. The binding carries the pane side, and the
/// workspace sends it to the explorer that holds focus.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
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
