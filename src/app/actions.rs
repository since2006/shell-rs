//! Every user command, so toolbar buttons, menus and key bindings dispatch
//! the same action and cannot disagree.

use gpui_kit::*;

use crate::{
    cli::AgentKind,
    docker::{ContainerCommand, DockerObject},
    editor::EditorId,
    explorer::{ExplorerId, FileSizeFormat},
    host::{
        CredentialId, ForwardId, GroupId, HostId, HostNode, NodeDrop, SnippetCategoryId, SnippetId,
    },
    monitor::MonitorDetail,
    processes::ProcessSort,
    services::ServiceCommand,
    terminal::{LocalTerminalId, RemoteTerminalId},
};

gpui_kit::actions!(
    shellrs,
    [
        /// Open the new-host dialog for a host at the root of the tree.
        NewHost,
        /// Open the 临时连接 dialog: connect to a host without saving it.
        NewTemporaryConnection,
        /// Open 快速连接: search the saved hosts and connect to one or
        /// several.
        QuickConnect,
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
        ToggleHostPanel,
        /// Show or hide the right sidebar, whichever tool it is showing.
        ToggleToolSidebar,
        /// Read the network connections of the SSH terminal's host again.
        RefreshConnections,
        /// Read the processes of the SSH terminal's host again.
        RefreshProcesses,
        /// Read the services of the SSH terminal's host again.
        RefreshServices,
        /// Read the Docker of the SSH terminal's host again.
        RefreshDocker,
        /// Read the bash history of the SSH terminal's host again.
        RefreshHistory,
        /// Open the new-snippet dialog, for a snippet of no category.
        NewSnippet,
        /// Open the new-category dialog of 命令片段.
        NewSnippetCategory,
        /// Show the host list in the left dock.
        ShowHosts,
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
        /// Connect the host selected in the focused host list.
        ConnectSelected,
        /// Expand every group in the host tree.
        ExpandAllGroups,
        /// Collapse every group in the host tree.
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
        /// Show the previewed image larger, by one step.
        ZoomPreviewIn,
        /// Show the previewed image smaller, by one step.
        ZoomPreviewOut,
        /// Fit the previewed image into its frame.
        FitPreview,
        /// Show the previewed image at its own size (100%).
        ActualSizePreview,
        /// Put the `shellrs` command on the PATH.
        InstallCliCommand,
        /// Take the `shellrs` command off the PATH.
        RemoveCliCommand,
        /// Look again at what of the external CLI is installed.
        RefreshCliIntegration,
        /// Copy the agent skill's text to the clipboard.
        CopyAgentSkill,
        /// Look for a newer ShellRS now.
        CheckForUpdates,
        /// Download the newer ShellRS that was found.
        DownloadUpdate,
        /// Show the newer ShellRS: restarting into it, and where to read
        /// what changed.
        ShowUpdate,
        /// Restart into the downloaded ShellRS.
        RestartToUpdate,
        /// Open the page to download ShellRS by hand.
        OpenDownloadPage,
        /// Open the website's changelog.
        OpenChangelog,
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
    /// Open a new terminal connection for a host.
    ConnectHost(HostId);
    /// Mark a host disconnected.
    DisconnectHost(HostId);
    /// Open a new SFTP tab for a host, like a new terminal connection.
    OpenExplorer(HostId);
    /// Open the edit-host dialog for a host.
    EditHost(HostId);
    /// Duplicate a host in the same group.
    DuplicateHost(HostId);
    /// Ask for confirmation, then delete a host.
    DeleteHost(HostId);
    /// Copy a host's address (an IP address or a host name) to the
    /// clipboard.
    CopyHostAddress(HostId);
    /// Copy a host's public id to the clipboard, for another tool to
    /// name the machine by.
    CopyHostId(HostId);

    /// Open a terminal connection for every host in this group's subtree.
    ConnectGroup(GroupId);
    /// Open the new-host dialog with this group pre-selected.
    NewHostInGroup(GroupId);
    /// Open the new-group dialog for a group nested inside this one.
    NewChildGroup(GroupId);
    /// Open the rename-group dialog.
    RenameGroup(GroupId);
    /// Ask for confirmation, then delete a group with everything inside it.
    DeleteGroup(GroupId);

    /// Close one remote terminal connection and its tab.
    CloseTerminal(RemoteTerminalId);
    /// Reconnect one remote terminal using the host's latest settings.
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

    /// Close one editor tab, asking first when it has unsaved changes.
    CloseEditor(EditorId);
    /// Open the dialog that gives one SFTP tab its own title.
    RenameExplorer(ExplorerId);

    /// Install (or update) the agent skill for one agent.
    InstallAgentSkill(AgentKind);
    /// Remove the agent skill of one agent.
    RemoveAgentSkill(AgentKind);

    /// Show the SFTP 大小 column in another format, from its title's menu.
    SetFileSizeFormat(FileSizeFormat);

    /// Show a tool in the right sidebar, or hide the sidebar if it is
    /// already showing that tool.
    ToggleTool(ToolKind);
    /// Unfold a part of the system monitor, or fold it away again.
    ToggleMonitorDetail(MonitorDetail);
    /// Sort the process list by memory or CPU, the most first; by the same
    /// again, the other way.
    SortProcesses(ProcessSort);
    /// Open the details of a process of the host of the SSH terminal in
    /// front, by its PID.
    ShowProcess(u32);
    /// Open the details of a service of the host of the SSH terminal in
    /// front, by its name.
    ShowService(String);
    /// Fold a compose project of the Docker tool away, or unfold it, by
    /// its name.
    ToggleDockerProject(String);
    /// Copy a command of 历史命令 or 命令片段 to the clipboard.
    CopyCommand(String);
    /// Open the new-snippet dialog with this category picked.
    NewSnippetIn(SnippetCategoryId);
    /// Open the edit-snippet dialog.
    EditSnippet(SnippetId);
    /// Ask for confirmation, then delete a snippet.
    DeleteSnippet(SnippetId);
    /// Open the rename dialog of a snippet category.
    RenameSnippetCategory(SnippetCategoryId);
    /// Ask for confirmation, then delete a snippet category with the
    /// snippets in it.
    DeleteSnippetCategory(SnippetCategoryId);
    /// Fold a category of 命令片段 away, or unfold it; `None` is 未分类.
    ToggleSnippetCategory(Option<SnippetCategoryId>);

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

/// Move a host-tree row by dropping it beside a peer or into a group.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct MoveHostNode {
    pub source: HostNode,
    pub destination: NodeDrop,
}

/// Ask for confirmation, then end a process of the host of the SSH terminal
/// in front: with SIGTERM, or with `force` SIGKILL.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct EndProcess {
    pub pid: u32,
    pub force: bool,
}

/// Start, stop or restart a service of the host of the SSH terminal in
/// front, or enable or disable it at boot; asks first before stopping or
/// restarting it.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct ControlService {
    pub name: String,
    pub command: ServiceCommand,
}

/// Start, stop or restart containers of the host of the SSH terminal in
/// front: one, or a compose project's all; asks first before stopping or
/// restarting them.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct ControlContainers {
    /// What they are, for the question and the notification: 「容器“web”」,
    /// 「项目“php-56”」.
    pub subject: String,
    pub ids: Vec<String>,
    pub command: ContainerCommand,
}

/// Open the details of a container, an image, a volume or a network of the
/// host of the SSH terminal in front.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct ShowDockerObject {
    pub object: DockerObject,
    /// What Docker knows it by: the ID, or a volume's name.
    pub id: String,
}

/// Ask, then remove a container, an image, a volume or a network of the
/// host of the SSH terminal in front.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct RemoveDockerObject {
    pub object: DockerObject,
    /// What Docker knows it by: the ID, or a volume's name.
    pub id: String,
    pub name: String,
}

/// Put a command on the input line of the SSH terminal in front, in place
/// of what is typed there, and with `run` run it.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct EnterCommand {
    pub command: String,
    pub run: bool,
}

/// One tab of the center area, by the identity of what it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CenterTab {
    Terminal(RemoteTerminalId),
    Explorer(ExplorerId),
    LocalTerminal(LocalTerminalId),
    /// There is at most one settings tab.
    Settings,
    Editor(EditorId),
}

/// A tool of the right sidebar, in the order of the switch beside it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToolKind {
    #[default]
    Snippets,
    History,
    Docker,
    Services,
    Processes,
    Connections,
    Monitor,
}

impl ToolKind {
    pub const ALL: [ToolKind; 7] = [
        ToolKind::Snippets,
        ToolKind::History,
        ToolKind::Docker,
        ToolKind::Services,
        ToolKind::Processes,
        ToolKind::Connections,
        ToolKind::Monitor,
    ];
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
    /// Enter or double-click: open the directory under the cursor, or edit
    /// the file there.
    Open {
        remote: bool,
    },
    /// F4 and 编辑: open a file in the editor, `path` or else the one under
    /// the cursor (WinSCP's Edit).
    Edit {
        remote: bool,
        path: Option<String>,
    },
    /// 预览: show an image or a Markdown file, `path` or else the one under
    /// the cursor.
    Preview {
        remote: bool,
        path: Option<String>,
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
/// Commands of an editor tab, shared by its toolbar, tab menu, dialogs and
/// key bindings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    /// ⌘S: write the file back, unless it changed since it was read.
    Save,
    /// 覆盖: write it back although it changed; `close` once that worked.
    Overwrite {
        close: bool,
    },
    /// Read the file again, asking first when there are changes.
    Reload,
    /// Read it again, throwing the changes away.
    ReloadConfirmed,
    /// 保存 in the close question: save, then close if that worked.
    SaveAndClose,
    /// 放弃修改 in the close question: close without saving.
    CloseConfirmed,
    CopyPath,
}

#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct EditorAction {
    editor: EditorId,
    command: EditorCommand,
}
impl EditorAction {
    pub fn new(editor: EditorId, command: EditorCommand) -> Self {
        Self { editor, command }
    }
    pub fn editor(&self) -> EditorId {
        self.editor
    }
    pub fn command(&self) -> EditorCommand {
        self.command
    }
}
/// An editor key binding; the workspace sends it to the editor that holds
/// focus.
#[derive(Action, Clone, PartialEq, Eq)]
#[action(namespace = shellrs, no_json)]
pub struct EditorShortcut(pub EditorCommand);

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
