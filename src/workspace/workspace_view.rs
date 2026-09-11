use std::{collections::HashMap, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::{
    ActiveTheme as _, Root, Theme, ThemeMode, TitleBar,
    dock::{DockArea, DockEvent, DockLayout, DockPlacement, PanelId, TabGroup, panel_handle},
};
use gpui_kit::*;

use crate::app::{
    CloseActiveTab, CloseExplorer, CloseLocalTerminal, CloseTerminal, ConnectSession, CopyTerminal,
    DeleteSession, DisconnectSession, DuplicateSession, EditSession, FocusSearch, NewLocalTerminal,
    NewSession, OpenExplorer, PasteTerminal, ReconnectSession, RestartLocalTerminal,
    ToggleSessionPanel, ToggleTheme, ZoomIn, ZoomOut, ZoomReset,
};
use crate::explorer::{ExplorerPanel, ExplorerPanelEvent};
use crate::session::{
    ConnectionState, SessionId, SessionPanel, SessionStore, confirm_delete_session,
    open_session_dialog,
};
use crate::terminal::{
    LocalPtyTransportFactory, LocalTerminalId, LocalTerminalPanel, LocalTerminalPanelEvent,
    SharedTerminalTransportFactory, TerminalPanel, TerminalPanelEvent,
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
    Terminal(SessionId),
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
    terminals: HashMap<SessionId, Entity<TerminalPanel>>,
    explorers: HashMap<SessionId, Entity<ExplorerPanel>>,
    local_terminals: HashMap<LocalTerminalId, Entity<LocalTerminalPanel>>,
    local_terminal_factory: SharedTerminalTransportFactory,
    next_local_terminal_id: u64,
    /// The center tab displayed most recently; `CloseActiveTab` closes it.
    active_tab: Option<CenterTab>,
    /// Dispatch target for the title bar and start page: actions sent to it
    /// reach the workspace handlers whatever is focused.
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_with_local_terminal_factory(Arc::new(LocalPtyTransportFactory), window, cx)
    }

    /// Alternate constructor used by UI tests to avoid launching a real
    /// login shell while exercising workspace behavior.
    pub fn new_with_local_terminal_factory(
        local_terminal_factory: SharedTerminalTransportFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let store = cx.new(|_| SessionStore::seed());
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
        for id in connected {
            let (panel, subscription) = new_terminal_panel(&store, id, window, cx);
            center = center.panel_view(panel_handle(panel.clone()), cx);
            subscriptions.push(subscription);
            terminals.insert(id, panel);
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
            next_local_terminal_id: 1,
            active_tab: None,
            focus_handle,
            _subscriptions: subscriptions,
        }
    }

    /// The session store, for tests and for panels created later.
    pub fn store(&self) -> &Entity<SessionStore> {
        &self.store
    }

    pub fn terminal(&self, id: SessionId) -> Option<&Entity<TerminalPanel>> {
        self.terminals.get(&id)
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

    fn mark_connected(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.set_state(id, ConnectionState::Connected, cx);
        });
    }

    fn on_connect_session(
        &mut self,
        action: &ConnectSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        if let Some(panel) = self.terminals.get(&id).cloned() {
            let group = panel.read(cx).tab_group();
            self.activate_tab(group, panel.entity_id(), window, cx);
            return;
        }
        if self.store.read(cx).session(id).is_none() {
            return;
        }
        self.mark_connected(id, cx);
        let (panel, subscription) = new_terminal_panel(&self.store, id, window, cx);
        self._subscriptions.push(subscription);
        self.terminals.insert(id, panel.clone());
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
        self.mark_connected(id, cx);
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
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.store.update(cx, |store, cx| {
            store.set_state(id, ConnectionState::Disconnected, cx);
        });
        if let Some(terminal) = self.terminals.get(&id) {
            terminal.update(cx, |terminal, cx| terminal.disconnect(cx));
        }
    }

    fn on_reconnect_session(
        &mut self,
        action: &ReconnectSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        if self.store.read(cx).session(id).is_none() {
            return;
        }
        self.store.update(cx, |store, cx| {
            store.set_state(id, ConnectionState::Connecting, cx);
        });
        if let Some(terminal) = self.terminals.get(&id) {
            terminal.update(cx, |terminal, cx| terminal.reconnect(window, cx));
        }
        // The only asynchronous work in the mock: a short "connecting" state.
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            this.update_in(cx, |this, _, cx| {
                this.mark_connected(id, cx);
                if let Some(terminal) = this.terminals.get(&id) {
                    terminal.update(cx, |terminal, cx| terminal.append_line("已重新连接", cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn on_new_session(&mut self, _: &NewSession, window: &mut Window, cx: &mut Context<Self>) {
        open_session_dialog(None, self.store.clone(), window, cx);
    }

    fn on_edit_session(
        &mut self,
        action: &EditSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).session(action.0).is_some() {
            open_session_dialog(Some(action.0), self.store.clone(), window, cx);
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
        let closes_tabs = self.terminals.contains_key(&id) || self.explorers.contains_key(&id);
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
        if let Some(terminal) = self.terminals.remove(&id) {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(terminal, window, cx));
        }
        if let Some(explorer) = self.explorers.remove(&id) {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(explorer, window, cx));
        }
        self.store.update(cx, |store, cx| {
            store.remove(id, cx);
        });
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
    id: SessionId,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> (Entity<TerminalPanel>, Subscription) {
    let panel = cx.new(|cx| TerminalPanel::new(id, store.clone(), window, cx));
    let subscription = cx.subscribe(
        &panel,
        |this, _, event: &TerminalPanelEvent, cx| match event {
            TerminalPanelEvent::Activated(id) => this.active_tab = Some(CenterTab::Terminal(*id)),
            TerminalPanelEvent::Closed(id) => {
                this.terminals.remove(id);
                if this.active_tab == Some(CenterTab::Terminal(*id)) {
                    this.active_tab = None;
                }
            }
            TerminalPanelEvent::StatusChanged(_) => cx.notify(),
        },
    );
    (panel, subscription)
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
            .on_action(cx.listener(Self::on_connect_session))
            .on_action(cx.listener(Self::on_disconnect_session))
            .on_action(cx.listener(Self::on_reconnect_session))
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
