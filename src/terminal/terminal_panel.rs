use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{
    Icon, IconName, Sizable as _,
    button::Button,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    menu::PopupMenu,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CenterTab, CloseTerminal, ConnectHost, CopyHostAddress, CopyHostId,
    DisconnectTerminal, EditHost, OpenExplorer, ReconnectTerminal, RenameTerminal, SendBreak,
    SendCommands,
};
use crate::connection::{ConnectionPrompt, ConnectionPromptReply};
use crate::host::{ConnectionState, Host, HostConnection, HostId, HostOs, HostStore, ProtocolKind};
use crate::i18n::t;
use crate::shared::{ClosableTabTitle, HostMark, LatencyLabel, RenamableTab, close_tab_items};

use super::{
    HostTerminalTransports, RemoteTerminalId, ReportedDirectory, SessionLogIdentity, TerminalEvent,
    TerminalLifecycle, TerminalMenuItems, TerminalNotice, TerminalStatus, TerminalView,
    ZmodemDirection, ZmodemReply,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPanelEvent {
    Input(RemoteTerminalId, super::TerminalInputEvent),
    Activated(RemoteTerminalId, HostId),
    Closed(RemoteTerminalId, HostId),
    StatusChanged(RemoteTerminalId, HostId),
    /// Connected, again after each reconnection.
    Connected(RemoteTerminalId, HostId),
    PromptRequested(RemoteTerminalId, HostId, ConnectionPrompt),
    HostOsDetected(HostId, HostOs),
    Notice(RemoteTerminalId, TerminalNotice),
    /// The shell reported another folder; the right sidebar's 文件 follows.
    DirectoryChanged(RemoteTerminalId, ReportedDirectory),
    /// `rz` or `sz` started: the user chooses, and the answer goes to
    /// [`TerminalPanel::zmodem_reply`].
    ZmodemAsk(RemoteTerminalId, ZmodemDirection),
    /// Files dropped on the terminal, for [`TerminalPanel::upload_dropped`].
    FilesDropped(RemoteTerminalId, Vec<PathBuf>),
}

/// A remote-host Dock panel backed by the shared terminal engine: an SSH
/// host's, a Telnet host's or a serial host's.
pub struct TerminalPanel {
    id: RemoteTerminalId,
    host_id: HostId,
    store: Entity<HostStore>,
    terminal: Entity<TerminalView>,
    transports: HostTerminalTransports,
    /// The host as it was when the tab opened: what the tab shows, what
    /// it offers (only SSH has SFTP, only a serial port no address to copy)
    /// and what it connects with, every reconnection included. Editing or
    /// deleting the host leaves it alone: a new tab takes the new settings.
    /// Only the system this tab's own connections find is written in.
    host: Host,
    connection: HostConnection,
    /// Opened from a link (外部连接): nothing beside the terminal.
    external: bool,
    tab_group: Option<WeakEntity<TabGroup>>,
    broadcast: Option<super::BroadcastSelection>,
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
        transports: HostTerminalTransports,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (host, connection, external) = {
            let store = store.read(cx);
            let host = store
                .host(host_id)
                .cloned()
                .expect("terminal hosts must exist in the store");
            let connection = store.connection_of(&host);
            (host, connection, store.is_external(host_id))
        };
        let protocol = host.kind();
        let identity = SessionLogIdentity {
            name: host.name.clone(),
            address: host.address.clone(),
        };
        let terminal = cx.new(|cx| {
            let name = host.name.clone();
            let mut terminal = TerminalView::new(
                ("terminal", id.0),
                move |_| t!("terminal.remote.label", host = name),
                identity,
                transports.factory_for(&connection),
                window,
                cx,
            );
            terminal.set_menu_items(remote_menu_items(
                id,
                host_id,
                protocol,
                host.address_is_ip(),
                store.clone(),
            ));
            terminal.set_ended_actions(ReconnectTerminal(id), CloseTerminal(id), cx);
            terminal.take_dropped_files();
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
                    TerminalEvent::Input(input) => {
                        cx.emit(TerminalPanelEvent::Input(this.id, input.clone()));
                    }
                    TerminalEvent::Started => {
                        cx.emit(TerminalPanelEvent::Connected(this.id, this.host_id));
                    }
                    TerminalEvent::PromptRequested(prompt) => {
                        cx.emit(TerminalPanelEvent::PromptRequested(
                            this.id,
                            this.host_id,
                            prompt.clone(),
                        ));
                    }
                    TerminalEvent::HostOsDetected(os) => {
                        this.host.os = Some(*os);
                        cx.emit(TerminalPanelEvent::HostOsDetected(this.host_id, *os));
                    }
                    TerminalEvent::Notice(notice) => {
                        cx.emit(TerminalPanelEvent::Notice(this.id, notice.clone()));
                    }
                    TerminalEvent::DirectoryChanged(directory) => {
                        cx.emit(TerminalPanelEvent::DirectoryChanged(
                            this.id,
                            directory.clone(),
                        ));
                    }
                    TerminalEvent::ZmodemAsk(direction) => {
                        cx.emit(TerminalPanelEvent::ZmodemAsk(this.id, *direction));
                    }
                    TerminalEvent::FilesDropped(files) => {
                        cx.emit(TerminalPanelEvent::FilesDropped(this.id, files.clone()));
                    }
                    // Only a local transport tells.
                    TerminalEvent::WorkingDirectory(_) => {}
                },
            ),
        ];

        Self {
            id,
            host_id,
            store,
            terminal,
            transports,
            host,
            connection,
            external,
            tab_group: None,
            broadcast: None,
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

    /// The host as the tab took it (see the field).
    pub fn host(&self) -> &Host {
        &self.host
    }

    pub fn connection(&self) -> &HostConnection {
        &self.connection
    }

    /// Whether only the terminal runs on its connection: a link's, a
    /// Telnet host's or a serial host's.
    pub fn shell_only(&self) -> bool {
        self.external || !self.host.is_ssh()
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    fn tab_menu(&self, cx: &Context<Self>) -> TabMenu {
        let store = self.store.read(cx);
        TabMenu {
            id: self.id,
            host_id: self.host_id,
            host_is_ip: self.host.address_is_ip(),
            protocol: self.host.kind(),
            running: self.lifecycle(cx) == TerminalLifecycle::Running,
            host_exists: store.host(self.host_id).is_some(),
            host_is_ssh: store.host(self.host_id).is_some_and(|host| host.is_ssh()),
            editable: !store.is_temporary(self.host_id),
            group: self.tab_group.clone(),
            panel: cx.entity_id(),
        }
    }

    pub fn terminal(&self) -> &Entity<TerminalView> {
        &self.terminal
    }

    pub(crate) fn set_broadcast_selection(
        &mut self,
        selection: Option<super::BroadcastSelection>,
        cx: &mut Context<Self>,
    ) {
        if self.broadcast != selection {
            self.broadcast = selection;
            cx.notify();
        }
    }

    pub fn lifecycle(&self, cx: &App) -> TerminalLifecycle {
        self.terminal.read(cx).lifecycle(cx)
    }

    /// Where this tab's own connection stands.
    pub fn connection_state(&self, cx: &App) -> ConnectionState {
        match self.lifecycle(cx) {
            TerminalLifecycle::Starting => ConnectionState::Connecting,
            TerminalLifecycle::Running => ConnectionState::Connected,
            TerminalLifecycle::Exited { .. }
            | TerminalLifecycle::Failed(_)
            | TerminalLifecycle::Closing => ConnectionState::Disconnected,
        }
    }

    /// The terminal's state and size, for the window's status bar.
    pub fn status(&self, cx: &App) -> TerminalStatus {
        self.terminal.read(cx).status(cx)
    }

    /// Connect again as the tab first did.
    pub fn reconnect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let factory = self.transports.factory_for(&self.connection);
        self.terminal.update(cx, |terminal, cx| {
            terminal.restart_with_factory(factory, cx)
        });
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
    }

    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |terminal, cx| {
            terminal.stop(&t!("terminal.notice.disconnected"), cx)
        });
    }

    /// What the user chose for the transfer `rz` or `sz` started.
    pub fn zmodem_reply(&self, reply: ZmodemReply, cx: &App) {
        self.terminal.read(cx).zmodem_reply(reply, cx);
    }

    /// Upload `files`, dropped on the terminal, with ZMODEM: `rz` typed for
    /// them. Says why not.
    pub fn upload_dropped(
        &mut self,
        files: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        self.terminal
            .update(cx, |terminal, cx| terminal.upload_dropped(files, cx))
    }

    /// 发送 Break, for a Telnet or serial host's terminal.
    pub fn send_break(&self, cx: &App) {
        if self.host.kind().sends_break() {
            self.terminal.read(cx).send_break(cx);
        }
    }

    pub fn cancel_connection(&mut self, cx: &mut Context<Self>) {
        self.terminal.update(cx, |terminal, cx| {
            terminal.stop(&t!("terminal.notice.cancelled"), cx)
        });
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
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
        cx.emit(TerminalPanelEvent::Activated(self.id, self.host_id));
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
        let (address, os) = (self.host.address.clone(), self.host.os);
        let tab_menu = self.tab_menu(cx);
        let state = self.connection_state(cx);
        let mark = super::BroadcastSelection::lead(
            self.broadcast.clone(),
            CenterTab::Terminal(id),
            self.tab_title(cx),
            HostMark::new(
                ("terminal-tab-os", id.0),
                self.default_title(cx),
                address,
                os,
            )
            .protocol(self.host.kind())
            .connection_state(state)
            .small(),
        );
        ClosableTabTitle::new(("terminal-tab", id.0), mark, self.tab_title(cx))
            .connection_state(state)
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

    fn toolbar_buttons(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<Vec<Button>> {
        let host_id = self.host_id;
        let terminal_id = self.id;
        let reconnect_dispatch = self.focus_handle(cx);
        // SFTP is a new connection to the host as it is now: none once it
        // is gone or no longer an SSH host.
        let offers_sftp = self
            .store
            .read(cx)
            .host(self.host_id)
            .is_some_and(|host| host.is_ssh());
        let sftp = offers_sftp.then(|| {
            Button::new(("sftp", terminal_id.0))
                .icon(Icon::new(CatalogIcon::FolderTree))
                .label("SFTP")
                .tooltip(t!("terminal.remote.sftp_tooltip"))
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(OpenExplorer(host_id)), cx)
                })
        });
        let reconnect = Button::new(("reconnect", terminal_id.0))
            .icon(Icon::new(CatalogIcon::RefreshCw))
            .tooltip(t!("terminal.remote.reconnect"))
            .on_click(move |_, window, cx| {
                reconnect_dispatch.dispatch_action(&ReconnectTerminal(terminal_id), window, cx)
            });
        Some(sftp.into_iter().chain([reconnect]).collect())
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

/// What a remote terminal adds to its context menu: copying the host's
/// address and ID below 复制, and the connection's commands at the bottom.
/// Only an SSH host has SFTP to open, and a serial host no address to copy;
/// a deleted host has neither.
fn remote_menu_items(
    id: RemoteTerminalId,
    host_id: HostId,
    protocol: ProtocolKind,
    address_is_ip: bool,
    store: Entity<HostStore>,
) -> TerminalMenuItems {
    let copy = Rc::new(move |menu: PopupMenu, cx: &App| {
        // Named after what the address is, as in the tab's menu.
        let copy_host = if address_is_ip {
            t!("terminal.remote.copy_ip")
        } else {
            t!("terminal.remote.copy_host_name")
        };
        let exists = store.read(cx).host(host_id).is_some();
        menu.when(protocol.has_address() && exists, |menu| {
            menu.menu_with_icon(
                copy_host,
                Icon::new(CatalogIcon::ClipboardCopy),
                Box::new(CopyHostAddress(host_id)),
            )
        })
        .menu_with_icon(
            t!("terminal.remote.copy_id"),
            Icon::new(CatalogIcon::ClipboardCopy),
            Box::new(CopyHostId(host_id)),
        )
    });
    let commands = Rc::new(move |menu: PopupMenu, lifecycle: &TerminalLifecycle| {
        let connected = matches!(
            lifecycle,
            TerminalLifecycle::Starting | TerminalLifecycle::Running
        );
        menu.when(protocol.is_ssh(), |menu| {
            menu.menu_with_icon(
                t!("terminal.remote.open_sftp"),
                Icon::new(CatalogIcon::FolderTree),
                Box::new(OpenExplorer(host_id)),
            )
        })
        .menu_with_icon_and_disabled(
            t!("terminal.menu.send_commands"),
            Icon::new(CatalogIcon::Send),
            Box::new(SendCommands(id)),
            !connected,
        )
        .menu_with_icon(
            t!("terminal.remote.reconnect"),
            Icon::new(CatalogIcon::RefreshCw),
            Box::new(ReconnectTerminal(id)),
        )
        .menu_with_icon_and_disabled(
            t!("terminal.remote.disconnect"),
            Icon::new(CatalogIcon::Unplug),
            Box::new(DisconnectTerminal(id)),
            !connected,
        )
    });
    TerminalMenuItems { copy, commands }
}

/// The commands of one remote terminal tab, as a snapshot taken while the tab
/// renders. The tab's context menu and the tab bar's 「…」 menu both build
/// from it, so the two always list the same commands.
#[derive(Clone)]
struct TabMenu {
    id: RemoteTerminalId,
    host_id: HostId,
    host_is_ip: bool,
    /// What the host is: an SSH host's tab has SFTP, a Telnet or serial
    /// host's 发送 Break, and all but a serial host's the address.
    protocol: ProtocolKind,
    /// Whether the terminal is connected, which a break needs.
    running: bool,
    /// The host is still there: deleted, it has nothing to connect to,
    /// open SFTP on, copy or edit.
    host_exists: bool,
    /// It is an SSH host now, which a new SFTP connection needs whatever
    /// the tab's own protocol.
    host_is_ssh: bool,
    /// A saved host; a temporary one has nothing to edit.
    editable: bool,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
}

impl RenamableTab for TerminalPanel {
    /// The host's name when the tab opened, as the tab shows it without a
    /// title of its own.
    fn default_title(&self, _: &App) -> SharedString {
        self.host.name.clone()
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
            t!("terminal.remote.copy_ip")
        } else {
            t!("terminal.remote.copy_host_name")
        };
        let exists = self.host_exists;
        let mut menu = menu
            .menu_with_icon(
                t!("terminal.remote.rename"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameTerminal(id)),
            )
            .when(exists, |menu| {
                menu.menu_with_icon(
                    t!("terminal.remote.connect_in_new_tab"),
                    Icon::new(CatalogIcon::Plug),
                    Box::new(ConnectHost(host_id)),
                )
            });
        let protocol = self.protocol;
        menu = menu
            .when(protocol.sends_break(), |menu| {
                menu.menu_with_icon_and_disabled(
                    t!("terminal.remote.send_break"),
                    Icon::new(CatalogIcon::Zap),
                    Box::new(SendBreak(id)),
                    !self.running,
                )
            })
            .when(self.host_is_ssh, |menu| {
                menu.menu_with_icon(
                    t!("terminal.remote.open_sftp"),
                    Icon::new(CatalogIcon::FolderTree),
                    Box::new(OpenExplorer(host_id)),
                )
            })
            .when(protocol.has_address() && exists, |menu| {
                menu.menu_with_icon(
                    copy_host,
                    Icon::new(IconName::Copy),
                    Box::new(CopyHostAddress(host_id)),
                )
            });
        menu = menu.menu_with_icon(
            t!("terminal.remote.reconnect"),
            Icon::new(CatalogIcon::RefreshCw),
            Box::new(ReconnectTerminal(id)),
        );
        if self.editable && exists {
            menu = menu.menu_with_icon(
                t!("terminal.remote.edit_host"),
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
