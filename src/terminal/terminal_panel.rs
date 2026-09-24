use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::Button,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    menu::PopupMenu,
};
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CenterTab, CloseTerminal, ConnectSession, CopySessionHost, EditSession,
    OpenExplorer, ReconnectTerminal, RenameTerminal,
};
use crate::session::{HostOs, SessionId, SessionStore};
use crate::shared::{ClosableTabTitle, HostMark, close_tab_items};

use super::{
    LatencyLevel, RemoteTerminalId, SharedRemoteTerminalTransportProvider, TerminalLifecycle,
    TerminalPrompt, TerminalPromptReply, TerminalView, TerminalViewEvent,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPanelEvent {
    Activated(RemoteTerminalId, SessionId),
    Closed(RemoteTerminalId, SessionId),
    StatusChanged(RemoteTerminalId, SessionId),
    PromptRequested(RemoteTerminalId, SessionId, TerminalPrompt),
    HostOsDetected(SessionId, HostOs),
}

/// A remote-session Dock panel backed by the shared terminal engine. The
/// transport is created from the latest saved session on every connection.
pub struct TerminalPanel {
    id: RemoteTerminalId,
    session_id: SessionId,
    store: Entity<SessionStore>,
    terminal: Entity<TerminalView>,
    remote_provider: SharedRemoteTerminalTransportProvider,
    tab_group: Option<WeakEntity<TabGroup>>,
    /// A title the user gave this tab, to tell apart several connections to
    /// the same host. Lives as long as the tab, like the rest of the layout.
    custom_title: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl TerminalPanel {
    pub fn new(
        id: RemoteTerminalId,
        session_id: SessionId,
        store: Entity<SessionStore>,
        remote_provider: SharedRemoteTerminalTransportProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = store
            .read(cx)
            .session(session_id)
            .cloned()
            .expect("terminal sessions must exist in the store");
        let terminal = cx.new(|cx| {
            TerminalView::new(
                ("terminal", id.0),
                format!("{} 的终端", session.name),
                remote_provider.factory_for(&session),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&terminal, |this, _, cx| {
                cx.emit(TerminalPanelEvent::StatusChanged(this.id, this.session_id));
                cx.notify();
            }),
            cx.subscribe(
                &terminal,
                |this, _, event: &TerminalViewEvent, cx| match event {
                    TerminalViewEvent::PromptRequested(prompt) => {
                        cx.emit(TerminalPanelEvent::PromptRequested(
                            this.id,
                            this.session_id,
                            prompt.clone(),
                        ));
                    }
                    TerminalViewEvent::HostOsDetected(os) => {
                        cx.emit(TerminalPanelEvent::HostOsDetected(this.session_id, *os));
                    }
                    TerminalViewEvent::Changed => {}
                },
            ),
        ];

        Self {
            id,
            session_id,
            store,
            terminal,
            remote_provider,
            tab_group: None,
            custom_title: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn id(&self) -> RemoteTerminalId {
        self.id
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    /// The session's name, as the tab shows it without a title of its own.
    pub fn session_name(&self, cx: &App) -> SharedString {
        self.store
            .read(cx)
            .session(self.session_id)
            .map(|session| session.name.clone())
            .unwrap_or_else(|| "终端".into())
    }

    /// The tab's label: its own title when it has one, else the session name.
    pub fn title_text(&self, cx: &App) -> SharedString {
        self.custom_title
            .clone()
            .unwrap_or_else(|| self.session_name(cx))
    }

    /// Give the tab its own title, or `None` to follow the session name again.
    pub fn set_custom_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>) {
        self.custom_title = title;
        cx.notify();
    }

    fn tab_menu(&self, cx: &Context<Self>) -> TabMenu {
        TabMenu {
            id: self.id,
            session_id: self.session_id,
            host_is_ip: self
                .store
                .read(cx)
                .session(self.session_id)
                .is_some_and(|session| session.host_is_ip()),
            group: self.tab_group.clone(),
            panel: cx.entity_id(),
        }
    }

    pub fn terminal(&self) -> &Entity<TerminalView> {
        &self.terminal
    }

    pub fn lifecycle(&self, cx: &App) -> TerminalLifecycle {
        self.terminal.read(cx).lifecycle(cx)
    }

    pub fn append_line(&mut self, line: impl AsRef<str>, cx: &mut Context<Self>) {
        self.terminal.update(cx, |terminal, cx| {
            terminal.append_system_message(line.as_ref(), cx)
        });
    }

    pub fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.store.read(cx).session(self.session_id).cloned() else {
            return;
        };
        let factory = self.remote_provider.factory_for(&session);
        self.terminal.update(cx, |terminal, cx| {
            terminal.restart_with_factory(factory, cx)
        });
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
    }

    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.terminal
            .update(cx, |terminal, cx| terminal.stop("已断开连接", cx));
    }

    pub fn cancel_connection(&mut self, cx: &mut Context<Self>) {
        self.terminal
            .update(cx, |terminal, cx| terminal.stop("连接已取消", cx));
    }

    pub fn reply_to_prompt(&self, request_id: u64, reply: TerminalPromptReply, cx: &App) {
        self.terminal
            .read(cx)
            .reply_to_prompt(request_id, reply, cx);
    }
}

impl EventEmitter<PanelEvent> for TerminalPanel {}
impl EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.read(cx).focus_handle()
    }
}

impl BasePanel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "TerminalPanel"
    }

    /// Closing goes through `CloseTerminal`; see `ClosableTabTitle`. Saying
    /// no here also keeps the dock from adding a second 「关闭」 to the 「…」
    /// menu, beside the tab's own close commands.
    fn closable(&self, _: &App) -> bool {
        false
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !active {
            return;
        }
        let id = self.session_id;
        self.store
            .update(cx, |store, cx| store.set_active(Some(id), cx));
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
        cx.emit(TerminalPanelEvent::Activated(self.id, id));
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        self.terminal
            .update(cx, |terminal, cx| terminal.shutdown(cx));
        let id = self.session_id;
        cx.emit(TerminalPanelEvent::Closed(self.id, id));
    }
}

impl Panel for TerminalPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let os = self
            .store
            .read(cx)
            .session(self.session_id)
            .and_then(|session| session.os);
        let mark = HostMark::new(("terminal-tab-os", id.0), self.session_name(cx), os).small();
        let tab_menu = self.tab_menu(cx);
        ClosableTabTitle::new(("terminal-tab", id.0), mark, self.title_text(cx))
            .closable(("close-terminal", id.0), Box::new(CloseTerminal(id)))
            .context_menu(move |menu, _, cx| tab_menu.build(menu, cx))
    }

    /// The connection's latest round trip, beside the toolbar. Shown only
    /// while the shell runs, so a dropped connection leaves no stale number.
    fn title_suffix(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let terminal = self.terminal.read(cx);
        if terminal.lifecycle(cx) != TerminalLifecycle::Running {
            return None;
        }
        let latency = terminal.latency(cx)?;
        let color = match latency.level() {
            LatencyLevel::Good => cx.theme().success,
            LatencyLevel::Fair => cx.theme().warning,
            LatencyLevel::Poor => cx.theme().danger,
        };
        let label = latency.label();
        Some(
            div()
                .id(("terminal-latency", self.id.0))
                .test_support()
                .aria_label(label.clone())
                // Keeps the buttons beside it still as the digits change.
                .min_w_12()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_right()
                .text_color(color)
                .child(label),
        )
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        let session_id = self.session_id;
        let terminal_id = self.id;
        Some(vec![
            Button::new(("sftp", terminal_id.0))
                .icon(Icon::new(CatalogIcon::FolderTree))
                .label("SFTP")
                .tooltip("打开 SFTP 文件浏览")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenExplorer(session_id)), cx)
                }),
            Button::new(("reconnect", terminal_id.0))
                .icon(Icon::new(CatalogIcon::RefreshCw))
                .tooltip("重新连接")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(ReconnectTerminal(terminal_id)), cx)
                }),
        ])
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        self.tab_menu(cx).build(menu, cx)
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

/// The commands of one remote terminal tab, as a snapshot taken while the tab
/// renders. The tab's context menu and the tab bar's 「…」 menu both build
/// from it, so the two always list the same commands.
#[derive(Clone)]
struct TabMenu {
    id: RemoteTerminalId,
    session_id: SessionId,
    host_is_ip: bool,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
}

impl TabMenu {
    fn build(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let (id, session_id) = (self.id, self.session_id);
        let copy_host = if self.host_is_ip {
            "复制 IP 地址"
        } else {
            "复制主机名"
        };
        let menu = menu
            .menu_with_icon(
                "重命名标签…",
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameTerminal(id)),
            )
            .menu_with_icon(
                "在新标签页中连接",
                Icon::new(CatalogIcon::Plug),
                Box::new(ConnectSession(session_id)),
            )
            .menu_with_icon(
                "打开 SFTP",
                Icon::new(CatalogIcon::FolderTree),
                Box::new(OpenExplorer(session_id)),
            )
            .menu_with_icon(
                copy_host,
                Icon::new(IconName::Copy),
                Box::new(CopySessionHost(session_id)),
            )
            .menu_with_icon(
                "重新连接",
                Icon::new(CatalogIcon::RefreshCw),
                Box::new(ReconnectTerminal(id)),
            )
            .menu_with_icon(
                "编辑会话…",
                Icon::new(CatalogIcon::Pencil),
                Box::new(EditSession(session_id)),
            )
            .separator();
        close_tab_items(
            menu,
            CenterTab::Terminal(id),
            self.group.clone(),
            self.panel,
            cx,
        )
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.terminal.clone())
    }
}
