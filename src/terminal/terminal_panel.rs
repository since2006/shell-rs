use gpui_kit::component::{
    Icon,
    button::Button,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    menu::PopupMenu,
};
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CloseTerminal, CopyTerminal, DuplicateSession, EditSession, OpenExplorer,
    PasteTerminal, ReconnectTerminal,
};
use crate::session::{SessionId, SessionStore};
use crate::shared::ClosableTabTitle;

use super::{
    RemoteTerminalId, SharedRemoteTerminalTransportProvider, TerminalLifecycle, TerminalPrompt,
    TerminalPromptReply, TerminalView, TerminalViewEvent,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPanelEvent {
    Activated(RemoteTerminalId, SessionId),
    Closed(RemoteTerminalId, SessionId),
    StatusChanged(RemoteTerminalId, SessionId),
    PromptRequested(RemoteTerminalId, SessionId, TerminalPrompt),
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
            cx.subscribe(&terminal, |this, _, event: &TerminalViewEvent, cx| {
                if let TerminalViewEvent::PromptRequested(prompt) = event {
                    cx.emit(TerminalPanelEvent::PromptRequested(
                        this.id,
                        this.session_id,
                        prompt.clone(),
                    ));
                }
            }),
        ];

        Self {
            id,
            session_id,
            store,
            terminal,
            remote_provider,
            tab_group: None,
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
        let name = self
            .store
            .read(cx)
            .session(self.session_id)
            .map(|session| session.name.clone())
            .unwrap_or_else(|| "终端".into());
        ClosableTabTitle::new(CatalogIcon::Terminal, name).closable(
            ("close-terminal", self.id.0),
            Box::new(CloseTerminal(self.id)),
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
                .tooltip("重连")
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
        let id = self.session_id;
        let terminal = self.terminal.read(cx);
        let has_selection = terminal.has_selection(cx);
        let can_paste = terminal.lifecycle(cx).accepts_input()
            && cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some();
        menu.menu_with_disabled("复制", Box::new(CopyTerminal), !has_selection)
            .menu_with_disabled("粘贴", Box::new(PasteTerminal), !can_paste)
            .separator()
            .menu("复制会话配置", Box::new(DuplicateSession(id)))
            .menu("编辑会话…", Box::new(EditSession(id)))
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.terminal.clone())
    }
}
