use std::rc::Rc;

use gpui_kit::component::{
    Icon, IconName, Sizable as _,
    button::Button,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    menu::PopupMenu,
};
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CenterTab, CloseTerminal, ConnectHost, CopyHostAddress, DisconnectTerminal,
    EditHost, OpenExplorer, ReconnectTerminal, RenameTerminal,
};
use crate::connection::{ConnectionPrompt, ConnectionPromptReply};
use crate::host::{HostId, HostOs, HostStore};
use crate::shared::{ClosableTabTitle, HostMark, LatencyLabel, RenamableTab, close_tab_items};

use super::{
    RemoteTerminalId, SharedRemoteTerminalTransportProvider, TerminalEvent, TerminalLifecycle,
    TerminalMenuItems, TerminalStatus, TerminalView,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPanelEvent {
    Activated(RemoteTerminalId, HostId),
    Closed(RemoteTerminalId, HostId),
    StatusChanged(RemoteTerminalId, HostId),
    PromptRequested(RemoteTerminalId, HostId, ConnectionPrompt),
    HostOsDetected(HostId, HostOs),
}

/// A remote-host Dock panel backed by the shared terminal engine. The
/// transport is created from the latest saved host on every connection.
pub struct TerminalPanel {
    id: RemoteTerminalId,
    host_id: HostId,
    store: Entity<HostStore>,
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
        host_id: HostId,
        store: Entity<HostStore>,
        remote_provider: SharedRemoteTerminalTransportProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (host, login) = {
            let store = store.read(cx);
            let host = store
                .host(host_id)
                .cloned()
                .expect("terminal hosts must exist in the store");
            let login = store.login_of(&host);
            (host, login)
        };
        let terminal = cx.new(|cx| {
            let mut terminal = TerminalView::new(
                ("terminal", id.0),
                format!("{} 的终端", host.name),
                remote_provider.factory_for(&login),
                window,
                cx,
            );
            terminal.set_menu_items(connection_menu_items(id, host_id));
            terminal
        });
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&terminal, |this, _, cx| {
                cx.emit(TerminalPanelEvent::StatusChanged(this.id, this.host_id));
                cx.notify();
            }),
            cx.subscribe(
                &terminal,
                |this, _, event: &TerminalEvent, cx| match event {
                    TerminalEvent::PromptRequested(prompt) => {
                        cx.emit(TerminalPanelEvent::PromptRequested(
                            this.id,
                            this.host_id,
                            prompt.clone(),
                        ));
                    }
                    TerminalEvent::HostOsDetected(os) => {
                        cx.emit(TerminalPanelEvent::HostOsDetected(this.host_id, *os));
                    }
                },
            ),
        ];

        Self {
            id,
            host_id,
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

    pub fn host_id(&self) -> HostId {
        self.host_id
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    fn tab_menu(&self, cx: &Context<Self>) -> TabMenu {
        let store = self.store.read(cx);
        TabMenu {
            id: self.id,
            host_id: self.host_id,
            host_is_ip: store
                .host(self.host_id)
                .is_some_and(|host| host.address_is_ip()),
            editable: !store.is_temporary(self.host_id),
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

    /// The terminal's state and size, for the window's status bar.
    pub fn status(&self, cx: &App) -> TerminalStatus {
        self.terminal.read(cx).status(cx)
    }

    pub fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(login) = self.store.read(cx).login(self.host_id) else {
            return;
        };
        let factory = self.remote_provider.factory_for(&login);
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

    pub fn reply_to_prompt(&self, request_id: u64, reply: ConnectionPromptReply, cx: &App) {
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
        let id = self.host_id;
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
        let id = self.host_id;
        cx.emit(TerminalPanelEvent::Closed(self.id, id));
    }
}

impl Panel for TerminalPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id;
        let os = self
            .store
            .read(cx)
            .host(self.host_id)
            .and_then(|host| host.os);
        let mark = HostMark::new(("terminal-tab-os", id.0), self.default_title(cx), os).small();
        let tab_menu = self.tab_menu(cx);
        ClosableTabTitle::new(("terminal-tab", id.0), mark, self.tab_title(cx))
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
        Some(LatencyLabel::new(("terminal-latency", self.id.0), latency))
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        let host_id = self.host_id;
        let terminal_id = self.id;
        Some(vec![
            Button::new(("sftp", terminal_id.0))
                .icon(Icon::new(CatalogIcon::FolderTree))
                .label("SFTP")
                .tooltip("打开 SFTP 文件浏览")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenExplorer(host_id)), cx)
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

/// The connection commands at the bottom of a remote terminal's context menu.
fn connection_menu_items(id: RemoteTerminalId, host_id: HostId) -> TerminalMenuItems {
    Rc::new(move |menu, lifecycle| {
        let connected = matches!(
            lifecycle,
            TerminalLifecycle::Starting | TerminalLifecycle::Running
        );
        menu.menu_with_icon(
            "打开 SFTP",
            Icon::new(CatalogIcon::FolderTree),
            Box::new(OpenExplorer(host_id)),
        )
        .menu_with_icon(
            "重新连接",
            Icon::new(CatalogIcon::RefreshCw),
            Box::new(ReconnectTerminal(id)),
        )
        .menu_with_icon_and_disabled(
            "断开连接",
            Icon::new(CatalogIcon::Unplug),
            Box::new(DisconnectTerminal(id)),
            !connected,
        )
    })
}

/// The commands of one remote terminal tab, as a snapshot taken while the tab
/// renders. The tab's context menu and the tab bar's 「…」 menu both build
/// from it, so the two always list the same commands.
#[derive(Clone)]
struct TabMenu {
    id: RemoteTerminalId,
    host_id: HostId,
    host_is_ip: bool,
    /// A saved host; a temporary one has nothing to edit.
    editable: bool,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
}

impl RenamableTab for TerminalPanel {
    /// The host's name, as the tab shows it without a title of its own.
    fn default_title(&self, cx: &App) -> SharedString {
        self.store
            .read(cx)
            .host(self.host_id)
            .map(|host| host.name.clone())
            .unwrap_or_else(|| "终端".into())
    }
    /// The tab's label: its own title when it has one, else the host name.
    fn tab_title(&self, cx: &App) -> SharedString {
        self.custom_title
            .clone()
            .unwrap_or_else(|| self.default_title(cx))
    }
    fn set_custom_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>) {
        self.custom_title = title;
        cx.notify();
    }
}

impl TabMenu {
    fn build(&self, menu: PopupMenu, cx: &App) -> PopupMenu {
        let (id, host_id) = (self.id, self.host_id);
        let copy_host = if self.host_is_ip {
            "复制 IP 地址"
        } else {
            "复制主机名"
        };
        let mut menu = menu
            .menu_with_icon(
                "重命名标签…",
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameTerminal(id)),
            )
            .menu_with_icon(
                "在新标签页中连接",
                Icon::new(CatalogIcon::Plug),
                Box::new(ConnectHost(host_id)),
            )
            .menu_with_icon(
                "打开 SFTP",
                Icon::new(CatalogIcon::FolderTree),
                Box::new(OpenExplorer(host_id)),
            )
            .menu_with_icon(
                copy_host,
                Icon::new(IconName::Copy),
                Box::new(CopyHostAddress(host_id)),
            )
            .menu_with_icon(
                "重新连接",
                Icon::new(CatalogIcon::RefreshCw),
                Box::new(ReconnectTerminal(id)),
            );
        if self.editable {
            menu = menu.menu_with_icon(
                "编辑主机…",
                Icon::new(CatalogIcon::Pencil),
                Box::new(EditHost(host_id)),
            );
        }
        let menu = menu.separator();
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
