use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme as _, Root, Sizable as _, Theme, ThemeMode, TitleBar, WindowExt as _,
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
    CloseActiveTab, CloseExplorer, CloseLocalTerminal, CloseTerminal, ConnectSession, CopyTerminal,
    DeleteGroup, DeleteSession, DisconnectSession, DuplicateSession, EditSession, FocusSearch,
    NewChildGroup, NewGroup, NewLocalTerminal, NewSession, NewSessionInGroup, OpenExplorer,
    PasteTerminal, ReconnectTerminal, RenameGroup, RestartLocalTerminal, ToggleSessionPanel,
    ToggleTheme, ZoomIn, ZoomOut, ZoomReset,
};
use crate::explorer::{ExplorerPanel, ExplorerPanelEvent};
use crate::session::{
    ConnectionState, GroupId, SessionId, SessionPanel, SessionStore, SessionStoreEvent,
    confirm_delete_group, confirm_delete_session, open_group_dialog, open_session_dialog,
};
use crate::terminal::{
    LocalPtyTransportFactory, LocalTerminalId, LocalTerminalPanel, LocalTerminalPanelEvent,
    RemoteTerminalId, SharedRemoteTerminalTransportProvider, SharedTerminalTransportFactory,
    TerminalLifecycle, TerminalPanel, TerminalPanelEvent, TerminalPrompt, TerminalPromptField,
    TerminalPromptKind, TerminalPromptReply, TerminalSecret,
};

use super::{
    dock_skin::WorkspaceDockSkin, recent_sessions::RecentSessions, status_bar::WorkspaceStatus,
    title_bar::render_title_bar,
};

const DOCK_ID: &str = "shellr-dock";
const DOCK_VERSION: usize = 1;
/// Interface zoom bounds for the base font, in pixels (the theme's unit).
const FONT_SIZE_MIN: f32 = 12.;
const FONT_SIZE_MAX: f32 = 20.;
const FONT_SIZE_DEFAULT: f32 = 16.;
const FONT_SIZE_STEP: f32 = 2.;

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

/// A center tab, by the session it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CenterTab {
    Terminal(RemoteTerminalId),
    Explorer(SessionId),
    LocalTerminal(LocalTerminalId),
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
    explorers: HashMap<SessionId, Entity<ExplorerPanel>>,
    local_terminals: HashMap<LocalTerminalId, Entity<LocalTerminalPanel>>,
    local_terminal_factory: SharedTerminalTransportFactory,
    remote_terminal_provider: SharedRemoteTerminalTransportProvider,
    next_remote_terminal_id: u64,
    next_local_terminal_id: u64,
    /// The center tab displayed most recently; `CloseActiveTab` closes it.
    active_tab: Option<CenterTab>,
    prompt_queue: VecDeque<(RemoteTerminalId, SessionId, TerminalPrompt)>,
    active_prompt: Option<(RemoteTerminalId, SessionId, u64)>,
    /// Dispatch target for the title bar and start page: actions sent to it
    /// reach the workspace handlers whatever is focused.
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// `store` is built by `main` from the database on disk, and by the UI
    /// tests from `SessionStore::seed`.
    pub fn new(store: Entity<SessionStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let known_hosts = crate::app::known_hosts_path()
            .unwrap_or_else(|_| crate::app::data_dir().join("known_hosts"));
        let remote = Arc::new(crate::ssh::SshTerminalTransportProvider::new(known_hosts));
        Self::new_with_transport_providers(
            store,
            remote,
            Arc::new(LocalPtyTransportFactory),
            window,
            cx,
        )
    }

    /// Alternate constructor used by UI tests to avoid launching a real
    /// login shell while exercising workspace behavior.
    pub fn new_with_local_terminal_factory(
        store: Entity<SessionStore>,
        local_terminal_factory: SharedTerminalTransportFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let known_hosts = crate::app::known_hosts_path()
            .unwrap_or_else(|_| crate::app::data_dir().join("known_hosts"));
        let remote = Arc::new(crate::ssh::SshTerminalTransportProvider::new(known_hosts));
        Self::new_with_transport_providers(store, remote, local_terminal_factory, window, cx)
    }

    /// Fully injectable constructor used by UI tests: remote sessions never
    /// touch the network, while production uses the SSH provider above.
    pub fn new_with_transport_providers(
        store: Entity<SessionStore>,
        remote_terminal_provider: SharedRemoteTerminalTransportProvider,
        local_terminal_factory: SharedTerminalTransportFactory,
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

        let mut subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
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
                            this.cancel_prompts_for_session(*id, window, cx);
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
            local_terminal_factory,
            remote_terminal_provider,
            next_remote_terminal_id,
            next_local_terminal_id: 1,
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

    pub fn explorer(&self, id: SessionId) -> Option<&Entity<ExplorerPanel>> {
        self.explorers.get(&id)
    }

    pub fn local_terminal(&self, id: LocalTerminalId) -> Option<&Entity<LocalTerminalPanel>> {
        self.local_terminals.get(&id)
    }

    /// Keep the start page and focus in step with the center: once its last
    /// tab closes, the page fills it and takes the focus the tab held, so
    /// window-level shortcuts keep a dispatch path.
    fn on_layout_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let empty = self.dock_area.read(cx).is_empty(DockPlacement::Center, cx);
        let was_empty = self.skin.is_center_empty();
        self.skin.set_center_empty(empty, cx);
        if empty && !was_empty {
            let page = self.recent.read(cx).focus_handle(cx);
            window.focus(&page, cx);
        }
    }

    fn enqueue_prompt(
        &mut self,
        terminal_id: RemoteTerminalId,
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
            if self.terminals.contains_key(&next.0) {
                break next;
            }
        };
        let request_id = prompt.request_id();
        self.active_prompt = Some((terminal_id, session_id, request_id));
        let workspace = cx.entity().downgrade();
        match prompt.kind().clone() {
            TerminalPromptKind::UnknownHost(prompt) => {
                let description = format!(
                    "主机：{}:{}\n算法：{}\nSHA-256 指纹：{}\n\n请先确认该指纹来自可信渠道。",
                    prompt.host(),
                    prompt.port(),
                    prompt.algorithm(),
                    prompt.fingerprint(),
                );
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
        terminal_id: RemoteTerminalId,
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
        if let Some(panel) = self.terminals.get(&terminal_id).cloned() {
            panel.read(cx).reply_to_prompt(request_id, reply, cx);
            if canceled {
                panel.update(cx, |panel, cx| panel.cancel_connection(cx));
                self.refresh_session_connection_state(session_id, cx);
            }
        }
    }

    fn cancel_prompts_for_terminal(
        &mut self,
        terminal_id: RemoteTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_queue.retain(|(id, _, _)| *id != terminal_id);
        if let Some((id, _, request_id)) = self.active_prompt
            && id == terminal_id
        {
            self.active_prompt = None;
            if let Some(panel) = self.terminals.get(&terminal_id) {
                panel
                    .read(cx)
                    .reply_to_prompt(request_id, TerminalPromptReply::Cancel, cx);
            }
            if window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
        }
    }

    fn cancel_prompts_for_session(
        &mut self,
        session_id: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prompt_queue.retain(|(_, id, _)| *id != session_id);
        if let Some((terminal_id, id, request_id)) = self.active_prompt
            && id == session_id
        {
            self.active_prompt = None;
            if let Some(panel) = self.terminals.get(&terminal_id) {
                panel
                    .read(cx)
                    .reply_to_prompt(request_id, TerminalPromptReply::Cancel, cx);
            }
            if window.has_active_dialog(cx) {
                window.close_dialog(cx);
            }
        }
    }

    /// Select the tab of an already open panel.
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
        let session_id = action.0;
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
        let id = action.0;
        if let Some(panel) = self.explorers.get(&id).cloned() {
            let group = panel.read(cx).tab_group();
            self.activate_tab(group, panel.entity_id(), window, cx);
            return;
        }
        if self.store.read(cx).session(id).is_none() {
            return;
        }
        let (panel, subscription) = new_explorer_panel(&self.store, id, window, cx);
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
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(panel, window, cx));
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
        match self.active_tab {
            Some(CenterTab::Terminal(id)) => self.on_close_terminal(&CloseTerminal(id), window, cx),
            Some(CenterTab::Explorer(id)) => self.on_close_explorer(&CloseExplorer(id), window, cx),
            Some(CenterTab::LocalTerminal(id)) => {
                self.on_close_local_terminal(&CloseLocalTerminal(id), window, cx)
            }
            None => {}
        }
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

    fn on_copy_terminal(&mut self, _: &CopyTerminal, _: &mut Window, cx: &mut Context<Self>) {
        match self.active_tab {
            Some(CenterTab::Terminal(id)) => {
                if let Some(panel) = self.terminals.get(&id) {
                    let terminal = panel.read(cx).terminal().clone();
                    terminal.update(cx, |terminal, cx| terminal.copy_selection(cx));
                }
            }
            Some(CenterTab::LocalTerminal(id)) => {
                if let Some(panel) = self.local_terminals.get(&id) {
                    panel.update(cx, |panel, cx| panel.copy(cx));
                }
            }
            Some(CenterTab::Explorer(_)) | None => {}
        }
    }

    fn on_paste_terminal(&mut self, _: &PasteTerminal, _: &mut Window, cx: &mut Context<Self>) {
        match self.active_tab {
            Some(CenterTab::Terminal(id)) => {
                if let Some(panel) = self.terminals.get(&id) {
                    let terminal = panel.read(cx).terminal().clone();
                    terminal.update(cx, |terminal, cx| terminal.paste_clipboard(cx));
                }
            }
            Some(CenterTab::LocalTerminal(id)) => {
                if let Some(panel) = self.local_terminals.get(&id) {
                    panel.update(cx, |panel, cx| panel.paste(cx));
                }
            }
            Some(CenterTab::Explorer(_)) | None => {}
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
        open_session_dialog(None, None, self.store.clone(), window, cx);
    }

    fn on_edit_session(
        &mut self,
        action: &EditSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).session(action.0).is_some() {
            open_session_dialog(Some(action.0), None, self.store.clone(), window, cx);
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
            || self.explorers.contains_key(&id);
        let workspace = cx.entity().downgrade();
        confirm_delete_session(
            &session,
            closes_tabs,
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
        if let Some(explorer) = self.explorers.remove(&id) {
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
        open_session_dialog(None, Some(action.0), self.store.clone(), window, cx);
    }

    fn on_new_group(&mut self, _: &NewGroup, window: &mut Window, cx: &mut Context<Self>) {
        open_group_dialog(None, None, self.store.clone(), window, cx);
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
                || self.explorers.contains_key(id)
        });
        let workspace = cx.entity().downgrade();
        confirm_delete_group(
            &name,
            doomed.len(),
            subgroups,
            closes_tabs,
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

    fn on_toggle_theme(&mut self, _: &ToggleTheme, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if cx.theme().is_dark() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(mode, Some(window), cx);
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
                    && !this.explorers.contains_key(session_id)
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
            TerminalPanelEvent::PromptRequested(terminal_id, session_id, prompt) => {
                this.enqueue_prompt(*terminal_id, *session_id, prompt.clone(), window, cx)
            }
        },
    );
    (panel, subscription)
}

fn resolve_prompt(
    workspace: &WeakEntity<Workspace>,
    terminal_id: RemoteTerminalId,
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
    store: &Entity<SessionStore>,
    id: SessionId,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<ExplorerPanel>, Subscription) {
    let panel = cx.new(|cx| ExplorerPanel::new(id, store.clone(), window, cx));
    let subscription = cx.subscribe(
        &panel,
        |this, _, event: &ExplorerPanelEvent, _| match event {
            ExplorerPanelEvent::Activated(id) => this.active_tab = Some(CenterTab::Explorer(*id)),
            ExplorerPanelEvent::Closed(id) => {
                this.explorers.remove(id);
                if this.active_tab == Some(CenterTab::Explorer(*id)) {
                    this.active_tab = None;
                }
            }
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
            .on_action(cx.listener(Self::on_delete_session))
            .on_action(cx.listener(Self::on_new_session_in_group))
            .on_action(cx.listener(Self::on_new_group))
            .on_action(cx.listener(Self::on_new_child_group))
            .on_action(cx.listener(Self::on_rename_group))
            .on_action(cx.listener(Self::on_delete_group))
            .on_action(cx.listener(Self::on_connect_session))
            .on_action(cx.listener(Self::on_disconnect_session))
            .on_action(cx.listener(Self::on_reconnect_terminal))
            .on_action(cx.listener(Self::on_open_explorer))
            .on_action(cx.listener(Self::on_close_terminal))
            .on_action(cx.listener(Self::on_close_explorer))
            .on_action(cx.listener(Self::on_close_local_terminal))
            .on_action(cx.listener(Self::on_close_active_tab))
            .on_action(cx.listener(Self::on_restart_local_terminal))
            .on_action(cx.listener(Self::on_copy_terminal))
            .on_action(cx.listener(Self::on_paste_terminal))
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
            .children(Root::render_notification_layer(window, cx))
    }
}
