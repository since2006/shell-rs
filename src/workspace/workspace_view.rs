use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme as _, Root, Sizable as _, Theme, TitleBar, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter},
    dock::{DockArea, DockEvent, DockLayout, DockPlacement, PanelId, TabGroup, panel_handle},
    form::{Field, Form},
    input::{Input, InputContentType, InputState},
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CenterTab, ClearTerminal, CloseActiveTab, CloseExplorer, CloseLocalTerminal, CloseSettings,
    CloseTabs, CloseTerminal, CollapseAllGroups, ConnectGroup, ConnectSession, CopyAgentSkill,
    CopySessionHost, CopySessionId, CopyTerminal, DeleteGroup, DeleteSession, DisconnectSession,
    DisconnectTerminal, DismissTerminalFind, DuplicateSession, EditSession, ExpandAllGroups,
    ExplorerAction, ExplorerCommand, ExplorerShortcut, FindInTerminal, FindNextInTerminal,
    FindPreviousInTerminal, FocusSearch, InstallAgentSkill, InstallCliCommand, MoveSessionNode,
    NewChildGroup, NewGroup, NewLocalTerminal, NewSession, NewSessionInGroup, OpenExplorer,
    OpenSettings, PasteTerminal, ReconnectTerminal, RefreshCliIntegration, RemoveAgentSkill,
    RemoveCliCommand, RenameExplorer, RenameGroup, RenameTerminal, RestartLocalTerminal,
    ToggleSessionPanel, ToggleTheme, ZoomIn, ZoomOut, ZoomReset,
};
use crate::cli::{CliIntegration, CliServer, CliTarget, IntegrationPaths, SshCliBackend};
use crate::connection::SharedConnectionTester;
use crate::explorer::{ExplorerId, ExplorerPanel, ExplorerPanelEvent, confirm_close_transfer};
use crate::session::{
    ConnectionState, GroupId, SessionId, SessionPanel, SessionStore, SessionStoreEvent,
    confirm_delete_group, confirm_delete_session, open_group_dialog, open_session_dialog,
};
use crate::settings::{
    Appearance, SettingsPanel, SettingsPanelEvent, SettingsStore, SettingsStoreEvent,
};
use crate::sftp::{
    SharedLocalDirectoryProvider, SharedSftpTransportProvider, SshSftpTransportProvider,
    SystemLocalDirectoryProvider,
};
use crate::shared::open_rename_tab_dialog;
use crate::terminal::{
    LocalPtyTransportFactory, LocalTerminalId, LocalTerminalPanel, LocalTerminalPanelEvent,
    RemoteTerminalId, SearchDirection, SharedRemoteTerminalTransportProvider,
    SharedTerminalTransportFactory, TerminalLifecycle, TerminalPanel, TerminalPanelEvent,
    TerminalPrompt, TerminalPromptField, TerminalPromptKind, TerminalPromptReply, TerminalSecret,
    TerminalView,
};

use super::{
    dock_skin::WorkspaceDockSkin, recent_sessions::RecentSessions, status_bar::WorkspaceStatus,
    title_bar::render_title_bar,
};

const DOCK_ID: &str = "shellrs-dock";
const DOCK_VERSION: usize = 1;
/// Interface zoom bounds for the base font, in pixels (the theme's unit).
const FONT_SIZE_MIN: f32 = 12.;
const FONT_SIZE_MAX: f32 = 20.;
const FONT_SIZE_DEFAULT: f32 = 16.;
const FONT_SIZE_STEP: f32 = 2.;
/// Paint order of the notification layer. gpui-base draws a dialog deferred
/// at priority 0, and so does the dock skin's start page; a plain child would
/// land under the dialog's backdrop and show through it only faintly. One step
/// above them, and below popups (`POPUP_PRIORITY`), keeps a notification on
/// top of dialogs without covering an open menu.
const NOTIFICATION_PRIORITY: usize = 1;

/// Window options for the main workspace window.
pub fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        // Window geometry is a platform boundary; `px` is the API's unit.
        window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
        window_min_size: Some(gpui_kit::Size {
            width: px(960.),
            height: px(600.),
        }),
        ..TitleBar::window_options()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptOwner {
    Terminal(RemoteTerminalId),
    Sftp(ExplorerId, u64),
}

/// The main window content: title bar above the dock, status bar below.
///
/// Owns the session store, the dock and the registry of open per-session
/// panels, and handles every application action.
pub struct Workspace {
    store: Entity<SessionStore>,
    dock_area: Entity<DockArea>,
    skin: Rc<WorkspaceDockSkin>,
    session_panel: Entity<SessionPanel>,
    /// The start page the dock skin shows while the center has no tab.
    recent: Entity<RecentSessions>,
    terminals: HashMap<RemoteTerminalId, Entity<TerminalPanel>>,
    /// SFTP tabs; a session can have several, like terminals.
    explorers: HashMap<ExplorerId, Entity<ExplorerPanel>>,
    local_terminals: HashMap<LocalTerminalId, Entity<LocalTerminalPanel>>,
    /// The settings tab, while it is open. There is only ever one.
    settings_tab: Option<Entity<SettingsPanel>>,
    /// What the settings tab edits; the workspace applies it to the window.
    settings: Entity<SettingsStore>,
    /// Where the `shellrs` command and the agent skills go, and whether
    /// they are there. No paths in UI tests, so nothing reaches the home
    /// directory unless a test hands it a temporary one.
    cli_integration: Entity<CliIntegration>,
    /// Answers the `shellrs` command. Production only: UI tests never
    /// listen on the real socket.
    cli_server: Option<CliServer>,
    local_terminal_factory: SharedTerminalTransportFactory,
    remote_terminal_provider: SharedRemoteTerminalTransportProvider,
    sftp_provider: SharedSftpTransportProvider,
    local_directory_provider: SharedLocalDirectoryProvider,
    /// Backs the session dialog's 「测试连接」.
    connection_tester: SharedConnectionTester,
    next_remote_terminal_id: u64,
    next_local_terminal_id: u64,
    next_explorer_id: u64,
    /// The center tab displayed most recently; `CloseActiveTab` closes it.
    active_tab: Option<CenterTab>,
    prompt_queue: VecDeque<(PromptOwner, SessionId, TerminalPrompt)>,
    active_prompt: Option<(PromptOwner, SessionId, u64)>,
    /// Dispatch target for the title bar and start page: actions sent to it
    /// reach the workspace handlers whatever is focused.
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// `store` is built by `main` from the database on disk, and by the UI
    /// tests from `SessionStore::seed`; `settings` likewise from the settings
    /// file, or kept in memory.
    pub fn new(
        store: Entity<SessionStore>,
        settings: Entity<SettingsStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_local_terminal_factory(
            store,
            settings,
            Arc::new(LocalPtyTransportFactory),
            window,
            cx,
        )
    }

    pub fn new_with_local_terminal_factory(
        store: Entity<SessionStore>,
        settings: Entity<SettingsStore>,
        local_terminal_factory: SharedTerminalTransportFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let connector = crate::ssh::SshConnector::new(
            crate::app::data_dir().join("known_hosts"),
            store.read(cx).secrets(),
        );
        let remote = Arc::new(crate::ssh::SshTerminalTransportProvider::with_connector(
            connector.clone(),
        ));
        let tester = Arc::new(crate::ssh::SshConnectionTester::new(connector.clone()));
        let sftp: SharedSftpTransportProvider = Arc::new(SshSftpTransportProvider::new(
            connector.clone(),
            crate::app::data_dir().join("upload-resume"),
            crate::app::data_dir().join("download-resume"),
        ));
        let mut this = Self::new_with_services(
            store,
            settings,
            remote,
            local_terminal_factory,
            sftp.clone(),
            Arc::new(SystemLocalDirectoryProvider),
            tester,
            window,
            cx,
        );
        this.cli_integration.update(cx, |integration, cx| {
            integration.set_paths(IntegrationPaths::system(), cx)
        });
        match CliServer::start(
            crate::app::cli_socket_path(),
            Arc::new(SshCliBackend::new(connector, sftp)),
        ) {
            Ok(server) => this.cli_server = Some(server),
            Err(error) => window.push_notification(
                Notification::error(error.to_string()).title("外部 CLI 无法启动"),
                cx,
            ),
        }
        this.sync_cli_server(cx);
        this
    }

    /// Tell the CLI server what it may use: whether 启用外部 CLI is on, and
    /// the sessions as they are now.
    fn sync_cli_server(&self, cx: &App) {
        if let Some(server) = &self.cli_server {
            server.set_enabled(self.settings.read(cx).settings().external_cli.enabled);
            server.set_targets(CliTarget::all(self.store.read(cx)));
        }
    }

    /// Fully injectable constructor used by UI tests: remote sessions never
    /// touch the network, while production uses the SSH provider above.
    pub fn new_with_transport_providers(
        store: Entity<SessionStore>,
        settings: Entity<SettingsStore>,
        remote_terminal_provider: SharedRemoteTerminalTransportProvider,
        local_terminal_factory: SharedTerminalTransportFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let connector = crate::ssh::SshConnector::new(
            crate::app::data_dir().join("known_hosts"),
            store.read(cx).secrets(),
        );
        let tester = Arc::new(crate::ssh::SshConnectionTester::new(connector.clone()));
        let sftp = Arc::new(SshSftpTransportProvider::new(
            connector,
            crate::app::data_dir().join("upload-resume"),
            crate::app::data_dir().join("download-resume"),
        ));
        Self::new_with_services(
            store,
            settings,
            remote_terminal_provider,
            local_terminal_factory,
            sftp,
            Arc::new(SystemLocalDirectoryProvider),
            tester,
            window,
            cx,
        )
    }

    /// Inject every filesystem and transport service; tests require neither a server nor a keychain.
    // One parameter per injected service, which is the point of this
    // constructor; bundling them would only move the list elsewhere.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_services(
        store: Entity<SessionStore>,
        settings: Entity<SettingsStore>,
        remote_terminal_provider: SharedRemoteTerminalTransportProvider,
        local_terminal_factory: SharedTerminalTransportFactory,
        sftp_provider: SharedSftpTransportProvider,
        local_directory_provider: SharedLocalDirectoryProvider,
        connection_tester: SharedConnectionTester,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let recent = cx.new(|cx| RecentSessions::new(store.clone(), focus_handle.clone(), cx));
        let (dock_area, skin) = WorkspaceDockSkin::dock_area(
            DOCK_ID,
            Some(DOCK_VERSION),
            recent.clone().into(),
            window,
            cx,
        );
        let session_panel = cx.new(|cx| SessionPanel::new(store.clone(), window, cx));
        // Start with focus in the session panel so window-level actions have a
        // dispatch path. The workspace's own handle is never focused: the
        // dialog layer is its child, and a focused ancestor would keep the
        // dialog's focus trap from taking focus.
        let panel_focus = session_panel.read(cx).focus_handle(cx);
        window.focus(&panel_focus, cx);
        // Before the first frame, so a dark theme never starts out light.
        crate::settings::apply(settings.read(cx).settings(), window, cx);

        let mut subscriptions = vec![
            cx.observe(&store, |this, _, cx| {
                this.sync_cli_server(cx);
                cx.notify()
            }),
            cx.subscribe_in(
                &store,
                window,
                |this, _, event: &SessionStoreEvent, window, cx| match event {
                    SessionStoreEvent::PersistFailed(message) => {
                        window.push_notification(Notification::error(message.clone()), cx);
                    }
                    SessionStoreEvent::ConnectionSettingsChanged(id) => {
                        let panels: Vec<_> = this
                            .terminals
                            .values()
                            .filter(|panel| panel.read(cx).session_id() == *id)
                            .cloned()
                            .collect();
                        if !panels.is_empty() {
                            for panel in &panels {
                                this.cancel_prompts_for_terminal(panel.read(cx).id(), window, cx);
                            }
                            this.store.update(cx, |store, cx| {
                                store.set_state(*id, ConnectionState::Connecting, cx)
                            });
                            for panel in panels {
                                panel.update(cx, |panel, cx| panel.reconnect(window, cx));
                            }
                        }
                    }
                },
            ),
            cx.observe_in(&settings, window, |this, settings, window, cx| {
                crate::settings::apply(settings.read(cx).settings(), window, cx);
                this.sync_cli_server(cx);
            }),
            cx.subscribe_in(
                &settings,
                window,
                |_, _, event: &SettingsStoreEvent, window, cx| match event {
                    SettingsStoreEvent::PersistFailed(message) => {
                        window.push_notification(Notification::error(message.clone()), cx);
                    }
                },
            ),
            // 跟随系统 follows the system appearance as it changes.
            cx.observe_window_appearance(window, |this, window, cx| {
                crate::settings::apply(this.settings.read(cx).settings(), window, cx);
            }),
            cx.subscribe_in(
                &dock_area,
                window,
                |this, _, event: &DockEvent, window, cx| {
                    if matches!(event, DockEvent::LayoutChanged) {
                        this.on_layout_changed(window, cx);
                    }
                },
            ),
        ];

        // Sessions that start out connected get a terminal tab right away.
        let connected: Vec<SessionId> = store
            .read(cx)
            .sessions()
            .iter()
            .filter(|session| session.state.is_connected())
            .map(|session| session.id)
            .collect();
        let mut terminals = HashMap::new();
        let mut center = DockLayout::tabs();
        let mut next_remote_terminal_id = 1;
        for id in connected {
            let terminal_id = RemoteTerminalId(next_remote_terminal_id);
            next_remote_terminal_id += 1;
            let (panel, subscription) = new_terminal_panel(
                &store,
                remote_terminal_provider.clone(),
                terminal_id,
                id,
                window,
                cx,
            );
            center = center.panel_view(panel_handle(panel.clone()), cx);
            subscriptions.push(subscription);
            terminals.insert(terminal_id, panel);
        }

        dock_area.update(cx, |area, cx| {
            // With nothing to show, the center keeps the empty layout the
            // area starts with; the skin draws the start page over it.
            if !terminals.is_empty() {
                area.set_center(center, window, cx);
            }
            area.set_dock(
                DockPlacement::Left,
                DockLayout::tabs().panel_view(panel_handle(session_panel.clone()), cx),
                window,
                cx,
            );
            // Dock geometry is an API boundary that takes `Pixels`.
            area.set_dock_size(DockPlacement::Left, px(280.), window, cx);
            area.set_dock_collapsible(DockPlacement::Left, true, window, cx);
        });
        // Settled before the first `LayoutChanged` arrives, so the handler
        // does not take the initial state for a tab having just closed.
        skin.set_center_empty(terminals.is_empty(), cx);

        Self {
            store,
            dock_area,
            skin,
            session_panel,
            recent,
            terminals,
            explorers: HashMap::new(),
            local_terminals: HashMap::new(),
            settings_tab: None,
            settings,
            cli_integration: cx.new(|_| CliIntegration::new(None)),
            cli_server: None,
            local_terminal_factory,
            remote_terminal_provider,
            sftp_provider,
            local_directory_provider,
            connection_tester,
            next_remote_terminal_id,
            next_local_terminal_id: 1,
            next_explorer_id: 1,
            active_tab: None,
            prompt_queue: VecDeque::new(),
            active_prompt: None,
            focus_handle,
            _subscriptions: subscriptions,
        }
    }

    /// The session store, for tests and for panels created later.
    pub fn store(&self) -> &Entity<SessionStore> {
        &self.store
    }

    pub fn terminal(&self, id: SessionId, cx: &App) -> Option<&Entity<TerminalPanel>> {
        self.terminals
            .values()
            .filter(|panel| panel.read(cx).session_id() == id)
            .max_by_key(|panel| panel.read(cx).id().0)
    }

    pub fn remote_terminal(&self, id: RemoteTerminalId) -> Option<&Entity<TerminalPanel>> {
        self.terminals.get(&id)
    }

    pub fn terminal_count(&self, id: SessionId, cx: &App) -> usize {
        self.terminals
            .values()
            .filter(|panel| panel.read(cx).session_id() == id)
            .count()
    }

    pub fn explorer(&self, id: ExplorerId) -> Option<&Entity<ExplorerPanel>> {
        self.explorers.get(&id)
    }

    /// A session's SFTP tabs, oldest first.
    pub fn explorers_of(&self, session: SessionId, cx: &App) -> Vec<Entity<ExplorerPanel>> {
        let mut explorers: Vec<_> = self
            .explorers
            .iter()
            .filter(|(_, panel)| panel.read(cx).session_id() == session)
            .collect();
        explorers.sort_by_key(|(id, _)| id.0);
        explorers
            .into_iter()
            .map(|(_, panel)| panel.clone())
            .collect()
    }

    pub fn local_terminal(&self, id: LocalTerminalId) -> Option<&Entity<LocalTerminalPanel>> {
        self.local_terminals.get(&id)
    }

    pub fn settings_tab(&self) -> Option<&Entity<SettingsPanel>> {
        self.settings_tab.as_ref()
    }

    pub fn settings(&self) -> &Entity<SettingsStore> {
        &self.settings
    }

    /// Tests hand it a temporary home and bin directory.
    pub fn cli_integration(&self) -> &Entity<CliIntegration> {
        &self.cli_integration
    }

    /// Keep the start page and focus in step with the center: once its last
    /// tab closes, the page fills it and takes the focus the tab held, so
    /// window-level shortcuts keep a dispatch path.
    fn on_layout_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let empty = self.dock_area.read(cx).is_empty(DockPlacement::Center, cx);
        let was_empty = self.skin.is_center_empty();
        self.skin.set_center_empty(empty, cx);
        if empty && !was_empty {
            self.recent
                .update(cx, |recent, cx| recent.clear_selection(cx));
            let page = self.recent.read(cx).focus_handle(cx);
            window.focus(&page, cx);
        }
    }

    fn enqueue_prompt(
        &mut self,
        terminal_id: PromptOwner,
        session_id: SessionId,
        prompt: TerminalPrompt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_queue
            .push_back((terminal_id, session_id, prompt));
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                let done = this
                    .update_in(cx, |this, window, cx| {
                        if this.prompt_queue.is_empty() {
                            return true;
                        }
                        if this.active_prompt.is_some() || window.has_active_dialog(cx) {
                            return false;
                        }
                        this.open_next_prompt(window, cx);
                        true
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    fn open_next_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (terminal_id, session_id, prompt) = loop {
            let Some(next) = self.prompt_queue.pop_front() else {
                return;
            };
            if self.prompt_owner_is_live(next.0, cx) {
                break next;
            }
        };
        let request_id = prompt.request_id();
        self.active_prompt = Some((terminal_id, session_id, request_id));
        let workspace = cx.entity().downgrade();
        match prompt.kind().clone() {
            TerminalPromptKind::UnknownHost(prompt) => {
                let description = prompt.description();
                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .title("首次连接此主机")
                        .description(description.clone())
                        .button_props(
                            DialogButtonProps::default()
                                .ok_text("信任并连接")
                                .cancel_text("取消"),
                        )
                        .show_cancel(true)
                        .on_ok({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                resolve_prompt(
                                    &workspace,
                                    terminal_id,
                                    request_id,
                                    TerminalPromptReply::TrustAndSave,
                                    cx,
                                );
                                true
                            }
                        })
                        .on_cancel({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                resolve_prompt(
                                    &workspace,
                                    terminal_id,
                                    request_id,
                                    TerminalPromptReply::Cancel,
                                    cx,
                                );
                                true
                            }
                        })
                        .on_close({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                resolve_prompt(
                                    &workspace,
                                    terminal_id,
                                    request_id,
                                    TerminalPromptReply::Cancel,
                                    cx,
                                );
                            }
                        })
                });
            }
            TerminalPromptKind::HostKeyChanged(prompt) => {
                let old = prompt.old_fingerprints().join("、");
                let description = format!(
                    "主机：{}:{}\n算法：{}\n已保存指纹：{old}\n服务器当前指纹：{}\n\n连接已阻断。请核验服务器身份后手动处理：{}",
                    prompt.host(),
                    prompt.port(),
                    prompt.algorithm(),
                    prompt.fingerprint(),
                    prompt.known_hosts_path().display(),
                );
                window.open_alert_dialog(cx, move |alert, _, _| {
                    alert
                        .title("服务器主机密钥已变更")
                        .description(description.clone())
                        .button_props(
                            DialogButtonProps::default()
                                .ok_text("关闭")
                                .ok_variant(ButtonVariant::Danger),
                        )
                        .on_ok({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                resolve_prompt(
                                    &workspace,
                                    terminal_id,
                                    request_id,
                                    TerminalPromptReply::Cancel,
                                    cx,
                                );
                                true
                            }
                        })
                        .on_close({
                            let workspace = workspace.clone();
                            move |_, _, cx| {
                                resolve_prompt(
                                    &workspace,
                                    terminal_id,
                                    request_id,
                                    TerminalPromptReply::Cancel,
                                    cx,
                                );
                            }
                        })
                });
            }
            TerminalPromptKind::Authentication(prompt) => {
                let title = prompt.title().to_string();
                let instructions = prompt.instructions().to_string();
                let fields = prompt.fields().to_vec();
                let dialog_title: SharedString = if title.trim().is_empty() {
                    "SSH 认证".into()
                } else {
                    title.clone().into()
                };
                let form = cx.new(|cx| AuthenticationPromptForm::new(fields, window, cx));
                window.open_dialog(cx, {
                    let workspace_for_ok = workspace.clone();
                    let workspace_for_cancel = workspace.clone();
                    let form_for_ok = form.clone();
                    move |dialog, _, _| {
                        dialog
                            .title(dialog_title.clone())
                            .child(
                                v_flex()
                                    .gap_3()
                                    .when(!instructions.trim().is_empty(), |view| {
                                        view.child(div().text_sm().child(instructions.clone()))
                                    })
                                    .child(form.clone()),
                            )
                            .footer(
                                DialogFooter::new()
                                    .child(
                                        DialogClose::new().trigger(|button| button.label("取消")),
                                    )
                                    .child(DialogAction::new().child(
                                        Button::new("ssh-auth-submit").primary().label("继续"),
                                    )),
                            )
                            .on_ok({
                                let workspace = workspace_for_ok.clone();
                                let form = form_for_ok.clone();
                                move |_, window, cx| {
                                    let answers =
                                        form.update(cx, |form, cx| form.take_answers(window, cx));
                                    resolve_prompt(
                                        &workspace,
                                        terminal_id,
                                        request_id,
                                        TerminalPromptReply::Answers(answers),
                                        cx,
                                    );
                                    true
                                }
                            })
                            .on_cancel({
                                let workspace = workspace_for_cancel.clone();
                                move |_, _, cx| {
                                    resolve_prompt(
                                        &workspace,
                                        terminal_id,
                                        request_id,
                                        TerminalPromptReply::Cancel,
                                        cx,
                                    );
                                    true
                                }
                            })
                            .on_close({
                                let workspace = workspace.clone();
                                move |_, _, cx| {
                                    resolve_prompt(
                                        &workspace,
                                        terminal_id,
                                        request_id,
                                        TerminalPromptReply::Cancel,
                                        cx,
                                    );
                                }
                            })
                            .overlay_closable(false)
                    }
                });
            }
        }
    }

    fn finish_prompt(
        &mut self,
        terminal_id: PromptOwner,
        request_id: u64,
        reply: TerminalPromptReply,
        cx: &mut Context<Self>,
    ) {
        let Some((active_terminal, session_id, active_request)) = self.active_prompt else {
            return;
        };
        if (active_terminal, active_request) != (terminal_id, request_id) {
            return;
        }
        self.active_prompt = None;
        let canceled = matches!(&reply, TerminalPromptReply::Cancel);
        self.reply_to_owner(terminal_id, request_id, reply, cx);
        if canceled
            && let PromptOwner::Terminal(id) = terminal_id
            && let Some(panel) = self.terminals.get(&id).cloned()
        {
            panel.update(cx, |panel, cx| panel.cancel_connection(cx));
        }
        self.refresh_session_connection_state(session_id, cx);
    }

    fn prompt_owner_is_live(&self, owner: PromptOwner, cx: &App) -> bool {
        match owner {
            PromptOwner::Terminal(id) => self.terminals.contains_key(&id),
            PromptOwner::Sftp(id, generation) => self
                .explorers
                .get(&id)
                .is_some_and(|p| p.read(cx).generation() == generation),
        }
    }

    fn reply_to_owner(
        &self,
        owner: PromptOwner,
        request_id: u64,
        reply: TerminalPromptReply,
        cx: &App,
    ) {
        match owner {
            PromptOwner::Terminal(id) => {
                if let Some(panel) = self.terminals.get(&id) {
                    panel.read(cx).reply_to_prompt(request_id, reply, cx);
                }
            }
            PromptOwner::Sftp(id, generation) => {
                if let Some(panel) = self.explorers.get(&id)
                    && panel.read(cx).generation() == generation
                {
                    panel.read(cx).reply_to_prompt(request_id, reply);
                }
            }
        }
    }

    fn cancel_prompts_for_owner(
        &mut self,
        owner: PromptOwner,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_queue.retain(|(id, _, _)| *id != owner);
        if let Some((id, _, request_id)) = self.active_prompt
            && id == owner
        {
            self.active_prompt = None;
            self.reply_to_owner(id, request_id, TerminalPromptReply::Cancel, cx);
            if window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
        }
    }
    fn cancel_prompts_for_terminal(
        &mut self,
        terminal_id: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel_prompts_for_owner(PromptOwner::Terminal(terminal_id), window, cx);
    }
    fn cancel_prompts_for_session(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_queue.retain(|(_, id, _)| *id != session_id);
        if let Some((owner, id, _)) = self.active_prompt
            && id == session_id
        {
            self.cancel_prompts_for_owner(owner, window, cx);
        }
    }

    fn refresh_session_connection_state(&mut self, session_id: SessionId, cx: &mut Context<Self>) {
        let mut has_starting = false;
        let mut has_running = false;
        for panel in self
            .terminals
            .values()
            .filter(|panel| panel.read(cx).session_id() == session_id)
        {
            match panel.read(cx).lifecycle(cx) {
                TerminalLifecycle::Running => has_running = true,
                TerminalLifecycle::Starting => has_starting = true,
                TerminalLifecycle::Exited { .. }
                | TerminalLifecycle::Failed(_)
                | TerminalLifecycle::Closing => {}
            }
        }
        for panel in self.explorers_of(session_id, cx) {
            match panel.read(cx).connection_state() {
                ConnectionState::Connected => has_running = true,
                ConnectionState::Connecting => has_starting = true,
                ConnectionState::Disconnected => {}
            }
        }
        let state = if has_running {
            ConnectionState::Connected
        } else if has_starting {
            ConnectionState::Connecting
        } else {
            ConnectionState::Disconnected
        };
        self.store
            .update(cx, |store, cx| store.set_state(session_id, state, cx));
    }

    fn on_connect_session(
        &mut self,
        action: &ConnectSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connect_session(action.0, window, cx);
    }

    fn on_connect_group(
        &mut self,
        action: &ConnectGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_ids = self.store.read(cx).sessions_under(action.0);
        for session_id in session_ids {
            self.connect_session(session_id, window, cx);
        }
    }

    fn connect_session(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).session(session_id).is_none() {
            return;
        }
        let terminal_id = RemoteTerminalId(self.next_remote_terminal_id);
        self.next_remote_terminal_id += 1;
        let (panel, subscription) = new_terminal_panel(
            &self.store,
            self.remote_terminal_provider.clone(),
            terminal_id,
            session_id,
            window,
            cx,
        );
        self._subscriptions.push(subscription);
        self.terminals.insert(terminal_id, panel.clone());
        self.refresh_session_connection_state(session_id, cx);
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    fn on_open_explorer(
        &mut self,
        action: &OpenExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_id = action.0;
        if self.store.read(cx).session(session_id).is_none() {
            return;
        }
        // Every request opens a tab of its own, as connecting does for
        // terminals: two directories of one host side by side.
        let id = ExplorerId(self.next_explorer_id);
        self.next_explorer_id += 1;
        let (panel, subscription) = new_explorer_panel(self, id, session_id, window, cx);
        self._subscriptions.push(subscription);
        self.explorers.insert(id, panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    fn on_new_local_terminal(
        &mut self,
        _: &NewLocalTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = LocalTerminalId(self.next_local_terminal_id);
        self.next_local_terminal_id += 1;
        let (panel, subscription) =
            new_local_terminal_panel(id, self.local_terminal_factory.clone(), window, cx);
        self._subscriptions.push(subscription);
        self.local_terminals.insert(id, panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    /// Open the settings tab, or bring the open one forward: settings are
    /// one place, not one tab per visit.
    fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.settings_tab.clone() {
            let group = panel.read(cx).tab_group();
            self.activate_tab(group, panel.entity_id(), window, cx);
            // Already displayed, the group reports nothing, so the panel
            // takes the focus itself.
            panel.update(cx, |panel, cx| panel.show(window, cx));
            return;
        }
        let store = self.settings.clone();
        let integration = self.cli_integration.clone();
        // Something may have been installed or removed since last time.
        integration.update(cx, |integration, cx| integration.refresh(cx));
        let panel = cx.new(|cx| SettingsPanel::new(store, integration, cx));
        let subscription = cx.subscribe(&panel, |this, _, event: &SettingsPanelEvent, cx| {
            match event {
                SettingsPanelEvent::Activated => {
                    this.store
                        .update(cx, |store, cx| store.set_active(None, cx));
                    this.active_tab = Some(CenterTab::Settings);
                }
                SettingsPanelEvent::Closed => {
                    this.settings_tab = None;
                    if this.active_tab == Some(CenterTab::Settings) {
                        this.active_tab = None;
                    }
                }
            }
            cx.notify();
        });
        self._subscriptions.push(subscription);
        self.settings_tab = Some(panel.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel_handle(panel), DockPlacement::Center, None, window, cx);
        });
    }

    fn on_install_cli_command(
        &mut self,
        _: &InstallCliCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_cli_integration(
            "无法安装 shellrs 命令",
            crate::cli::install_binary,
            window,
            cx,
        );
    }

    fn on_remove_cli_command(
        &mut self,
        _: &RemoveCliCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_cli_integration(
            "无法移除 shellrs 命令",
            crate::cli::remove_binary,
            window,
            cx,
        );
    }

    fn on_install_agent_skill(
        &mut self,
        action: &InstallAgentSkill,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = action.0;
        self.change_cli_integration(
            "无法安装 skill",
            move |paths| crate::cli::install_skill(paths, agent),
            window,
            cx,
        );
    }

    fn on_remove_agent_skill(
        &mut self,
        action: &RemoveAgentSkill,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = action.0;
        self.change_cli_integration(
            "无法移除 skill",
            move |paths| crate::cli::remove_skill(paths, agent),
            window,
            cx,
        );
    }

    fn on_refresh_cli_integration(
        &mut self,
        _: &RefreshCliIntegration,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cli_integration
            .update(cx, |integration, cx| integration.refresh(cx));
    }

    fn on_copy_agent_skill(
        &mut self,
        _: &CopyAgentSkill,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(crate::cli::SKILL.to_string()));
        window.push_notification(Notification::success("已复制 shellrs skill"), cx);
    }

    /// Install or remove part of the external CLI off the main thread, then
    /// look again at what is there. Installing the command can wait on the
    /// system's administrator prompt.
    fn change_cli_integration(
        &mut self,
        failure: &'static str,
        change: impl FnOnce(&IntegrationPaths) -> std::io::Result<()> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let integration = self.cli_integration.clone();
        let Some(paths) = integration.read(cx).paths().cloned() else {
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            let result = cx.background_spawn(async move { change(&paths) }).await;
            let _ = cx.update(|window, cx| {
                match result {
                    // The user closed the administrator prompt.
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).title(failure),
                        cx,
                    ),
                    Ok(()) => {}
                }
                integration.update(cx, |integration, cx| integration.refresh(cx));
            });
        })
        .detach();
    }

    fn on_close_settings(
        &mut self,
        _: &CloseSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.settings_tab.clone() {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(panel, window, cx));
        }
    }

    /// Display one tab of a tab group, found by its panel's entity.
    fn activate_tab(
        &self,
        group: Option<WeakEntity<TabGroup>>,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = group.and_then(|group| group.upgrade()) else {
            return;
        };
        group.update(cx, |group, cx| {
            let target = PanelId::from(panel);
            if let Some(ix) = group
                .panels()
                .iter()
                .position(|panel| panel.panel_id(cx) == target)
            {
                group.select_tab(ix, window, cx);
            }
        });
    }

    /// Close a session's terminal tab. Goes through the dock area rather
    /// than the tab group: the group refuses to close the last tab of the
    /// center, and here every tab is closable.
    fn on_close_terminal(
        &mut self,
        action: &CloseTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.terminals.get(&action.0).cloned() {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(panel, window, cx));
        }
    }

    fn on_close_explorer(
        &mut self,
        action: &CloseExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.explorers.get(&action.0).cloned() {
            let generation = panel.read(cx).generation();
            if panel.read(cx).is_transferring() {
                let direction = panel.read(cx).transfer_direction();
                confirm_close_transfer(
                    action.0,
                    generation,
                    direction,
                    self.focus_handle.clone(),
                    window,
                    cx,
                );
            } else {
                self.remove_explorer(action.0, window, cx);
            }
        }
    }

    fn remove_explorer(&mut self, id: ExplorerId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.explorers.get(&id).cloned() {
            self.cancel_prompts_for_owner(
                PromptOwner::Sftp(id, panel.read(cx).generation()),
                window,
                cx,
            );
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(panel, window, cx));
        }
    }
    fn on_explorer_action(
        &mut self,
        action: &ExplorerAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.explorers.get(&action.explorer()).cloned() else {
            return;
        };
        if action
            .generation()
            .is_some_and(|g| g != panel.read(cx).generation())
        {
            return;
        }
        if matches!(action.command(), ExplorerCommand::CancelTransfer) {
            self.cancel_prompts_for_owner(
                PromptOwner::Sftp(action.explorer(), panel.read(cx).generation()),
                window,
                cx,
            );
        }
        if matches!(action.command(), ExplorerCommand::CloseConfirmed) {
            self.remove_explorer(action.explorer(), window, cx);
        } else {
            panel.update(cx, |panel, cx| panel.execute(action.command(), window, cx));
        }
    }
    /// File-list shortcuts go to the explorer holding focus, not the tab
    /// activated last: two SFTP tabs can sit side by side in split groups.
    fn on_explorer_shortcut(
        &mut self,
        action: &ExplorerShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self
            .explorers
            .iter()
            .find(|(_, panel)| panel.read(cx).contains_focus(window, cx))
            .map(|(id, _)| *id);
        if let Some(id) = focused {
            self.on_explorer_action(&ExplorerAction::new(id, action.0.clone()), window, cx);
        }
    }

    fn on_close_local_terminal(
        &mut self,
        action: &CloseLocalTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.local_terminals.get(&action.0).cloned() {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(panel, window, cx));
        }
    }

    fn on_close_active_tab(
        &mut self,
        _: &CloseActiveTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.active_tab {
            self.close_center_tab(tab, window, cx);
        }
    }

    /// Close one center tab through its own close path, so an explorer that
    /// is uploading still asks first.
    fn close_center_tab(&mut self, tab: CenterTab, window: &mut Window, cx: &mut Context<Self>) {
        match tab {
            CenterTab::Terminal(id) => self.on_close_terminal(&CloseTerminal(id), window, cx),
            CenterTab::Explorer(id) => self.on_close_explorer(&CloseExplorer(id), window, cx),
            CenterTab::LocalTerminal(id) => {
                self.on_close_local_terminal(&CloseLocalTerminal(id), window, cx)
            }
            CenterTab::Settings => self.on_close_settings(&CloseSettings, window, cx),
        }
    }

    /// The tab group holding a center tab, and the tab's panel id in it.
    fn center_tab_location(&self, tab: CenterTab, cx: &App) -> Option<(Entity<TabGroup>, PanelId)> {
        let (group, entity) = match tab {
            CenterTab::Terminal(id) => {
                let panel = self.terminals.get(&id)?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
            CenterTab::Explorer(id) => {
                let panel = self.explorers.get(&id)?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
            CenterTab::LocalTerminal(id) => {
                let panel = self.local_terminals.get(&id)?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
            CenterTab::Settings => {
                let panel = self.settings_tab.as_ref()?;
                (panel.read(cx).tab_group(), panel.entity_id())
            }
        };
        Some((group?.upgrade()?, PanelId::from(entity)))
    }

    /// The center tab a dock panel stands for, if it is one of ours.
    fn center_tab_for_panel(&self, panel: PanelId) -> Option<CenterTab> {
        let is = |entity: EntityId| PanelId::from(entity) == panel;
        self.terminals
            .iter()
            .find(|(_, entity)| is(entity.entity_id()))
            .map(|(id, _)| CenterTab::Terminal(*id))
            .or_else(|| {
                self.explorers
                    .iter()
                    .find(|(_, entity)| is(entity.entity_id()))
                    .map(|(id, _)| CenterTab::Explorer(*id))
            })
            .or_else(|| {
                self.local_terminals
                    .iter()
                    .find(|(_, entity)| is(entity.entity_id()))
                    .map(|(id, _)| CenterTab::LocalTerminal(*id))
            })
            .or_else(|| {
                self.settings_tab
                    .as_ref()
                    .filter(|panel| is(panel.entity_id()))
                    .map(|_| CenterTab::Settings)
            })
    }

    fn on_close_tabs(&mut self, action: &CloseTabs, window: &mut Window, cx: &mut Context<Self>) {
        let Some((group, anchor)) = self.center_tab_location(action.tab, cx) else {
            return;
        };
        let panels: Vec<PanelId> = group
            .read(cx)
            .panels()
            .iter()
            .map(|panel| panel.panel_id(cx))
            .collect();
        let Some(ix) = panels.iter().position(|panel| *panel == anchor) else {
            return;
        };
        let tabs: Vec<CenterTab> = action
            .scope
            .targets(panels.len(), ix)
            .into_iter()
            .filter_map(|target| self.center_tab_for_panel(panels[target]))
            .collect();
        for tab in tabs {
            self.close_center_tab(tab, window, cx);
        }
    }

    fn on_rename_terminal(
        &mut self,
        action: &RenameTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.terminals.get(&action.0).cloned() {
            open_rename_tab_dialog(panel, window, cx);
        }
    }

    fn on_rename_explorer(
        &mut self,
        action: &RenameExplorer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.explorers.get(&action.0).cloned() {
            open_rename_tab_dialog(panel, window, cx);
        }
    }

    fn on_copy_session_host(
        &mut self,
        action: &CopySessionHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(host) = self
            .store
            .read(cx)
            .session(action.0)
            .map(|session| session.host.clone())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(host.to_string()));
        window.push_notification(Notification::success(format!("已复制 {host}")), cx);
    }

    fn on_copy_session_id(
        &mut self,
        action: &CopySessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(public_id) = self
            .store
            .read(cx)
            .session(action.0)
            .map(|session| session.public_id.clone())
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(public_id.to_string()));
        window.push_notification(Notification::success(format!("已复制 ID {public_id}")), cx);
    }

    fn on_restart_local_terminal(
        &mut self,
        action: &RestartLocalTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.local_terminals.get(&action.0) {
            panel.update(cx, |panel, cx| panel.restart(window, cx));
        }
    }

    /// The terminal of the center tab shown most recently, remote or local.
    fn active_terminal(&self, cx: &App) -> Option<Entity<TerminalView>> {
        match self.active_tab? {
            CenterTab::Terminal(id) => Some(self.terminals.get(&id)?.read(cx).terminal().clone()),
            CenterTab::LocalTerminal(id) => {
                Some(self.local_terminals.get(&id)?.read(cx).terminal().clone())
            }
            CenterTab::Explorer(_) | CenterTab::Settings => None,
        }
    }

    fn on_copy_terminal(&mut self, _: &CopyTerminal, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| terminal.copy_selection(cx));
        }
    }

    fn on_paste_terminal(&mut self, _: &PasteTerminal, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| terminal.paste_clipboard(cx));
        }
    }

    fn on_find_in_terminal(
        &mut self,
        _: &FindInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| terminal.open_find(window, cx));
        }
    }

    fn on_find_next_in_terminal(
        &mut self,
        _: &FindNextInTerminal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| {
                terminal.step_find(SearchDirection::Down, cx)
            });
        }
    }

    fn on_find_previous_in_terminal(
        &mut self,
        _: &FindPreviousInTerminal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| {
                terminal.step_find(SearchDirection::Up, cx)
            });
        }
    }

    fn on_dismiss_terminal_find(
        &mut self,
        _: &DismissTerminalFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| terminal.dismiss_find(window, cx));
        }
    }

    fn on_clear_terminal(&mut self, _: &ClearTerminal, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| terminal.clear(cx));
        }
    }

    fn on_disconnect_terminal(
        &mut self,
        action: &DisconnectTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.cancel_prompts_for_terminal(id, window, cx);
        if let Some(terminal) = self.terminals.get(&id).cloned() {
            let session_id = terminal.read(cx).session_id();
            terminal.update(cx, |terminal, cx| terminal.disconnect(cx));
            self.refresh_session_connection_state(session_id, cx);
        }
    }

    fn on_disconnect_session(
        &mut self,
        action: &DisconnectSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.cancel_prompts_for_session(id, window, cx);
        self.store.update(cx, |store, cx| {
            store.set_state(id, ConnectionState::Disconnected, cx);
        });
        let terminals: Vec<_> = self
            .terminals
            .values()
            .filter(|panel| panel.read(cx).session_id() == id)
            .cloned()
            .collect();
        for terminal in terminals {
            terminal.update(cx, |terminal, cx| terminal.disconnect(cx));
        }
        for panel in self.explorers_of(id, cx) {
            panel.update(cx, |panel, cx| panel.disconnect(window, cx));
        }
    }

    fn on_reconnect_terminal(
        &mut self,
        action: &ReconnectTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.cancel_prompts_for_terminal(id, window, cx);
        if let Some(terminal) = self.terminals.get(&id).cloned() {
            let session_id = terminal.read(cx).session_id();
            terminal.update(cx, |terminal, cx| terminal.reconnect(window, cx));
            self.refresh_session_connection_state(session_id, cx);
        }
    }

    fn on_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        open_session_dialog(
            None,
            None,
            self.store.clone(),
            self.connection_tester.clone(),
            window,
            cx,
        );
    }

    fn on_edit_session(
        &mut self,
        action: &EditSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).session(action.0).is_some() {
            open_session_dialog(
                Some(action.0),
                None,
                self.store.clone(),
                self.connection_tester.clone(),
                window,
                cx,
            );
        }
    }

    fn on_duplicate_session(
        &mut self,
        action: &DuplicateSession,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let copy = self
            .store
            .update(cx, |store, cx| store.duplicate(action.0, cx));
        if let Some(copy) = copy {
            self.session_panel
                .update(cx, |panel, cx| panel.select_session(copy, cx));
        }
    }

    fn on_move_session_node(
        &mut self,
        action: &MoveSessionNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let moved = self.store.update(cx, |store, cx| {
            store.move_node(action.source, action.destination, cx)
        });
        if moved {
            self.session_panel
                .update(cx, |panel, cx| panel.reveal_node(action.source, cx));
        }
    }

    fn on_delete_session(
        &mut self,
        action: &DeleteSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let Some(session) = self.store.read(cx).session(id).cloned() else {
            return;
        };
        let closes_tabs = self
            .terminals
            .values()
            .any(|panel| panel.read(cx).session_id() == id)
            || !self.explorers_of(id, cx).is_empty();
        let transfers = self
            .explorers_of(id, cx)
            .iter()
            .filter(|panel| panel.read(cx).is_transferring())
            .count();
        let workspace = cx.entity().downgrade();
        confirm_delete_session(
            &session,
            (closes_tabs, transfers),
            Rc::new(move |window, cx| {
                workspace
                    .update(cx, |this, cx| this.remove_session(id, window, cx))
                    .ok();
            }),
            window,
            cx,
        );
    }

    fn remove_session(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        self.close_session_tabs(id, window, cx);
        self.store.update(cx, |store, cx| {
            store.remove(id, cx);
        });
    }

    /// Close whatever a session has open in the center, leaving the store
    /// alone. Deleting a session and deleting the group around it both need
    /// this, the latter for every session in the subtree.
    fn close_session_tabs(&mut self, id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_prompts_for_session(id, window, cx);
        let terminals: Vec<_> = self
            .terminals
            .values()
            .filter(|panel| panel.read(cx).session_id() == id)
            .cloned()
            .collect();
        for terminal in terminals {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(terminal, window, cx));
        }
        for explorer in self.explorers_of(id, cx) {
            self.explorers.remove(&explorer.read(cx).id());
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(explorer, window, cx));
        }
    }

    fn on_new_session_in_group(
        &mut self,
        action: &NewSessionInGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_session_dialog(
            None,
            Some(action.0),
            self.store.clone(),
            self.connection_tester.clone(),
            window,
            cx,
        );
    }

    fn on_new_group(&mut self, _: &NewGroup, window: &mut Window, cx: &mut Context<Self>) {
        open_group_dialog(None, None, self.store.clone(), window, cx);
    }

    fn on_expand_all_groups(
        &mut self,
        _: &ExpandAllGroups,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_all_groups_expanded(true, cx);
    }

    fn on_collapse_all_groups(
        &mut self,
        _: &CollapseAllGroups,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_all_groups_expanded(false, cx);
    }

    fn set_all_groups_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.set_all_groups_expanded(expanded, cx));
        self.session_panel
            .update(cx, |panel, cx| panel.set_all_groups_expanded(expanded, cx));
    }

    fn on_new_child_group(
        &mut self,
        action: &NewChildGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).group(action.0).is_some() {
            open_group_dialog(None, Some(action.0), self.store.clone(), window, cx);
        }
    }

    fn on_rename_group(
        &mut self,
        action: &RenameGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).group(action.0).is_some() {
            open_group_dialog(Some(action.0), None, self.store.clone(), window, cx);
        }
    }

    fn on_delete_group(
        &mut self,
        action: &DeleteGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let store = self.store.read(cx);
        let Some(group) = store.group(id) else {
            return;
        };
        let name = group.name.to_string();
        let subgroups = store.descendant_groups(id).len();
        let doomed = store.sessions_under(id);
        let closes_tabs = doomed.iter().any(|id| {
            self.terminals
                .values()
                .any(|panel| panel.read(cx).session_id() == *id)
                || !self.explorers_of(*id, cx).is_empty()
        });
        let workspace = cx.entity().downgrade();
        confirm_delete_group(
            &name,
            doomed.len(),
            subgroups,
            (
                closes_tabs,
                doomed
                    .iter()
                    .flat_map(|id| self.explorers_of(*id, cx))
                    .filter(|panel| panel.read(cx).is_transferring())
                    .count(),
            ),
            Rc::new(move |window, cx| {
                workspace
                    .update(cx, |this, cx| this.remove_group(id, window, cx))
                    .ok();
            }),
            window,
            cx,
        );
    }

    /// The store cascades the delete; the workspace only has to close the
    /// tabs of the sessions that went with the group.
    fn remove_group(&mut self, id: GroupId, window: &mut Window, cx: &mut Context<Self>) {
        let removed = self
            .store
            .update(cx, |store, cx| store.remove_group(id, cx));
        for session in removed {
            self.close_session_tabs(session, window, cx);
        }
    }

    fn on_toggle_session_panel(
        &mut self,
        _: &ToggleSessionPanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dock_area.update(cx, |area, cx| {
            area.toggle_dock(DockPlacement::Left, window, cx);
        });
        cx.notify();
    }

    /// The title bar's quick switch: it picks 应用外观 outright, leaving
    /// 跟随系统 if that was chosen, and the settings observer applies it.
    fn on_toggle_theme(&mut self, _: &ToggleTheme, _: &mut Window, cx: &mut Context<Self>) {
        let appearance = if cx.theme().is_dark() {
            Appearance::Light
        } else {
            Appearance::Dark
        };
        self.settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.appearance = appearance, cx)
        });
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dock_area.read(cx).is_dock_open(DockPlacement::Left) {
            self.dock_area.update(cx, |area, cx| {
                area.toggle_dock(DockPlacement::Left, window, cx);
            });
        }
        self.session_panel
            .update(cx, |panel, cx| panel.focus_search(window, cx));
    }

    fn set_font_size(&mut self, size: f32, window: &mut Window, cx: &mut Context<Self>) {
        // The base font is the interface zoom axis; it anchors the rem scale.
        Theme::global_mut(cx).font_size = px(size.clamp(FONT_SIZE_MIN, FONT_SIZE_MAX));
        Theme::sync_base(cx);
        window.refresh();
    }

    fn on_zoom_in(&mut self, _: &ZoomIn, window: &mut Window, cx: &mut Context<Self>) {
        let size = cx.theme().font_size.as_f32() + FONT_SIZE_STEP;
        self.set_font_size(size, window, cx);
    }

    fn on_zoom_out(&mut self, _: &ZoomOut, window: &mut Window, cx: &mut Context<Self>) {
        let size = cx.theme().font_size.as_f32() - FONT_SIZE_STEP;
        self.set_font_size(size, window, cx);
    }

    fn on_zoom_reset(&mut self, _: &ZoomReset, window: &mut Window, cx: &mut Context<Self>) {
        self.set_font_size(FONT_SIZE_DEFAULT, window, cx);
    }
}

fn new_terminal_panel(
    store: &Entity<SessionStore>,
    remote_provider: SharedRemoteTerminalTransportProvider,
    terminal_id: RemoteTerminalId,
    session_id: SessionId,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<TerminalPanel>, Subscription) {
    let panel = cx.new(|cx| {
        TerminalPanel::new(
            terminal_id,
            session_id,
            store.clone(),
            remote_provider,
            window,
            cx,
        )
    });
    let subscription = cx.subscribe_in(
        &panel,
        window,
        |this, _, event: &TerminalPanelEvent, window, cx| match event {
            TerminalPanelEvent::Activated(terminal_id, _) => {
                this.active_tab = Some(CenterTab::Terminal(*terminal_id))
            }
            TerminalPanelEvent::Closed(terminal_id, session_id) => {
                this.cancel_prompts_for_terminal(*terminal_id, window, cx);
                this.terminals.remove(terminal_id);
                if this.active_tab == Some(CenterTab::Terminal(*terminal_id)) {
                    this.active_tab = None;
                }
                this.refresh_session_connection_state(*session_id, cx);
                if !this
                    .terminals
                    .values()
                    .any(|panel| panel.read(cx).session_id() == *session_id)
                    && this.explorers_of(*session_id, cx).is_empty()
                    && this.store.read(cx).active().map(|session| session.id) == Some(*session_id)
                {
                    this.store
                        .update(cx, |store, cx| store.set_active(None, cx));
                }
            }
            TerminalPanelEvent::StatusChanged(_, session_id) => {
                this.refresh_session_connection_state(*session_id, cx);
                cx.notify();
            }
            TerminalPanelEvent::PromptRequested(terminal_id, session_id, prompt) => this
                .enqueue_prompt(
                    PromptOwner::Terminal(*terminal_id),
                    *session_id,
                    prompt.clone(),
                    window,
                    cx,
                ),
            TerminalPanelEvent::HostOsDetected(session_id, os) => {
                let (session_id, os) = (*session_id, *os);
                this.store
                    .update(cx, |store, cx| store.set_host_os(session_id, Some(os), cx));
            }
        },
    );
    (panel, subscription)
}

fn resolve_prompt(
    workspace: &WeakEntity<Workspace>,
    terminal_id: PromptOwner,
    request_id: u64,
    reply: TerminalPromptReply,
    cx: &mut App,
) {
    workspace
        .update(cx, |workspace, cx| {
            workspace.finish_prompt(terminal_id, request_id, reply, cx)
        })
        .ok();
}

struct AuthenticationPromptForm {
    fields: Vec<(TerminalPromptField, Entity<InputState>)>,
}

impl AuthenticationPromptForm {
    fn new(fields: Vec<TerminalPromptField>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let fields = fields
            .into_iter()
            .map(|field| {
                let echo = field.echo();
                let input = cx.new(|cx| InputState::new(window, cx).masked(!echo));
                (field, input)
            })
            .collect();
        Self { fields }
    }

    fn take_answers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<TerminalSecret> {
        self.fields
            .iter()
            .map(|(_, input)| {
                let answer = TerminalSecret::new(input.read(cx).value().to_string());
                input.update(cx, |input, cx| input.set_value("", window, cx));
                answer
            })
            .collect()
    }
}

impl Render for AuthenticationPromptForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Form::new().children(
            self.fields
                .iter()
                .enumerate()
                .map(|(index, (field, input))| {
                    let mut input = Input::new(input).id(("ssh-auth-answer", index)).small();
                    if !field.echo() {
                        input = input.content_type(InputContentType::Password).mask_toggle();
                    }
                    Field::new().label(field.label().to_string()).child(input)
                }),
        )
    }
}

fn new_explorer_panel(
    workspace: &Workspace,
    id: ExplorerId,
    session_id: SessionId,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<ExplorerPanel>, Subscription) {
    let panel = cx.new(|cx| {
        ExplorerPanel::new(
            id,
            session_id,
            workspace.store.clone(),
            workspace.sftp_provider.clone(),
            workspace.local_directory_provider.clone(),
            workspace.focus_handle.clone(),
            window,
            cx,
        )
    });
    let subscription = cx.subscribe_in(
        &panel,
        window,
        |this, _, event: &ExplorerPanelEvent, window, cx| match event {
            ExplorerPanelEvent::Activated(id) => this.active_tab = Some(CenterTab::Explorer(*id)),
            ExplorerPanelEvent::Closed(id, session_id) => {
                this.explorers.remove(id);
                if this.active_tab == Some(CenterTab::Explorer(*id)) {
                    this.active_tab = None;
                }
                this.refresh_session_connection_state(*session_id, cx);
            }
            ExplorerPanelEvent::StateChanged(id, session_id) => {
                if let Some(panel) = this.explorers.get(id) {
                    let generation = panel.read(cx).generation();
                    this.prompt_queue.retain(|(owner,_,_)| !matches!(owner,PromptOwner::Sftp(s,g) if s == id && *g != generation));
                    if let Some((owner @ PromptOwner::Sftp(s,g),_,_)) = this.active_prompt && s == *id && g != generation {
                        this.cancel_prompts_for_owner(owner,window,cx);
                    }
                }
                this.refresh_session_connection_state(*session_id, cx);
            },
            ExplorerPanelEvent::PromptRequested(id, session_id, generation, prompt) => this.enqueue_prompt(
                PromptOwner::Sftp(*id, *generation),
                *session_id,
                prompt.clone(),
                window,
                cx,
            ),
        },
    );
    (panel, subscription)
}

fn new_local_terminal_panel(
    id: LocalTerminalId,
    factory: SharedTerminalTransportFactory,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<LocalTerminalPanel>, Subscription) {
    let panel = cx.new(|cx| LocalTerminalPanel::new(id, factory, window, cx));
    let subscription =
        cx.subscribe(
            &panel,
            |this, _, event: &LocalTerminalPanelEvent, cx| match event {
                LocalTerminalPanelEvent::Activated(id) => {
                    this.store
                        .update(cx, |store, cx| store.set_active(None, cx));
                    this.active_tab = Some(CenterTab::LocalTerminal(*id));
                    cx.notify();
                }
                LocalTerminalPanelEvent::Closed(id) => {
                    this.local_terminals.remove(id);
                    if this.active_tab == Some(CenterTab::LocalTerminal(*id)) {
                        this.active_tab = None;
                    }
                    cx.notify();
                }
                LocalTerminalPanelEvent::StatusChanged(_) => cx.notify(),
            },
        );
    (panel, subscription)
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.store.read(cx).active().cloned();
        let status = match self.active_tab {
            Some(CenterTab::LocalTerminal(id)) => self
                .local_terminals
                .get(&id)
                .map(|panel| WorkspaceStatus::local(panel.read(cx).status(cx)))
                .unwrap_or_else(|| WorkspaceStatus::session(active)),
            _ => WorkspaceStatus::session(active),
        };
        let sessions_visible = self.dock_area.read(cx).is_dock_open(DockPlacement::Left);

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_new_session))
            .on_action(cx.listener(Self::on_new_local_terminal))
            .on_action(cx.listener(Self::on_edit_session))
            .on_action(cx.listener(Self::on_duplicate_session))
            .on_action(cx.listener(Self::on_move_session_node))
            .on_action(cx.listener(Self::on_delete_session))
            .on_action(cx.listener(Self::on_new_session_in_group))
            .on_action(cx.listener(Self::on_new_group))
            .on_action(cx.listener(Self::on_expand_all_groups))
            .on_action(cx.listener(Self::on_collapse_all_groups))
            .on_action(cx.listener(Self::on_new_child_group))
            .on_action(cx.listener(Self::on_rename_group))
            .on_action(cx.listener(Self::on_delete_group))
            .on_action(cx.listener(Self::on_connect_session))
            .on_action(cx.listener(Self::on_connect_group))
            .on_action(cx.listener(Self::on_disconnect_session))
            .on_action(cx.listener(Self::on_reconnect_terminal))
            .on_action(cx.listener(Self::on_disconnect_terminal))
            .on_action(cx.listener(Self::on_open_explorer))
            .on_action(cx.listener(Self::on_explorer_action))
            .on_action(cx.listener(Self::on_explorer_shortcut))
            .on_action(cx.listener(Self::on_close_terminal))
            .on_action(cx.listener(Self::on_close_explorer))
            .on_action(cx.listener(Self::on_close_local_terminal))
            .on_action(cx.listener(Self::on_open_settings))
            .on_action(cx.listener(Self::on_install_cli_command))
            .on_action(cx.listener(Self::on_remove_cli_command))
            .on_action(cx.listener(Self::on_install_agent_skill))
            .on_action(cx.listener(Self::on_remove_agent_skill))
            .on_action(cx.listener(Self::on_refresh_cli_integration))
            .on_action(cx.listener(Self::on_copy_agent_skill))
            .on_action(cx.listener(Self::on_close_settings))
            .on_action(cx.listener(Self::on_close_active_tab))
            .on_action(cx.listener(Self::on_close_tabs))
            .on_action(cx.listener(Self::on_rename_terminal))
            .on_action(cx.listener(Self::on_rename_explorer))
            .on_action(cx.listener(Self::on_copy_session_host))
            .on_action(cx.listener(Self::on_copy_session_id))
            .on_action(cx.listener(Self::on_restart_local_terminal))
            .on_action(cx.listener(Self::on_copy_terminal))
            .on_action(cx.listener(Self::on_paste_terminal))
            .on_action(cx.listener(Self::on_find_in_terminal))
            .on_action(cx.listener(Self::on_find_next_in_terminal))
            .on_action(cx.listener(Self::on_find_previous_in_terminal))
            .on_action(cx.listener(Self::on_dismiss_terminal_find))
            .on_action(cx.listener(Self::on_clear_terminal))
            .on_action(cx.listener(Self::on_toggle_session_panel))
            .on_action(cx.listener(Self::on_toggle_theme))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_zoom_in))
            .on_action(cx.listener(Self::on_zoom_out))
            .on_action(cx.listener(Self::on_zoom_reset))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(render_title_bar(sessions_visible, &self.focus_handle, cx))
            .child(div().flex_1().min_h_0().child(self.dock_area.clone()))
            .child(status)
            .children(Root::render_sheet_layer(window, cx))
            .children(Root::render_dialog_layer(window, cx))
            .children(
                Root::render_notification_layer(window, cx)
                    .map(|layer| deferred(layer).with_priority(NOTIFICATION_PRIORITY)),
            )
    }
}
