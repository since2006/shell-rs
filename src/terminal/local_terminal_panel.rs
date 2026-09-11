use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::Button,
    dock::{BasePanel, Panel, PanelEvent, TabGroup},
    h_flex,
    menu::PopupMenu,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CloseLocalTerminal, CopyTerminal, PasteTerminal, RestartLocalTerminal,
};
use crate::shared::ClosableTabTitle;

use super::{
    LocalTerminalId, SharedTerminalTransportFactory, TerminalLifecycle, TerminalStatus,
    TerminalView,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalTerminalPanelEvent {
    Activated(LocalTerminalId),
    Closed(LocalTerminalId),
    StatusChanged(LocalTerminalId),
}

/// A local login-shell Dock tab backed by the shared terminal view.
pub struct LocalTerminalPanel {
    id: LocalTerminalId,
    default_title: String,
    terminal: Entity<TerminalView>,
    tab_group: Option<WeakEntity<TabGroup>>,
    _subscriptions: Vec<Subscription>,
}

impl LocalTerminalPanel {
    pub fn new(
        id: LocalTerminalId,
        factory: SharedTerminalTransportFactory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_title = format!("本地终端 {}", id.0);
        let terminal = cx.new(|cx| {
            TerminalView::new(
                ("local-terminal", id.0),
                default_title.clone(),
                factory,
                window,
                cx,
            )
        });
        let subscriptions = vec![cx.observe(&terminal, move |_, _, cx| {
            cx.emit(LocalTerminalPanelEvent::StatusChanged(id));
            cx.notify();
        })];

        Self {
            id,
            default_title,
            terminal,
            tab_group: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn id(&self) -> LocalTerminalId {
        self.id
    }

    pub fn tab_group(&self) -> Option<WeakEntity<TabGroup>> {
        self.tab_group.clone()
    }

    pub fn terminal(&self) -> &Entity<TerminalView> {
        &self.terminal
    }

    pub fn status(&self, cx: &App) -> TerminalStatus {
        self.terminal.read(cx).status(cx)
    }

    pub fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminal
            .update(cx, |terminal, cx| terminal.restart(cx));
        let focus = self.terminal.read(cx).focus_handle();
        window.focus(&focus, cx);
    }

    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        self.terminal
            .update(cx, |terminal, cx| terminal.copy_selection(cx))
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) -> bool {
        self.terminal
            .update(cx, |terminal, cx| terminal.paste_clipboard(cx))
    }

    fn display_title(&self, cx: &App) -> String {
        self.terminal
            .read(cx)
            .title(cx)
            .unwrap_or_else(|| self.default_title.clone())
    }
}

impl EventEmitter<PanelEvent> for LocalTerminalPanel {}
impl EventEmitter<LocalTerminalPanelEvent> for LocalTerminalPanel {}

impl Focusable for LocalTerminalPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.read(cx).focus_handle()
    }
}

impl BasePanel for LocalTerminalPanel {
    fn panel_name(&self) -> &'static str {
        "LocalTerminalPanel"
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            let focus = self.terminal.read(cx).focus_handle();
            window.focus(&focus, cx);
            cx.emit(LocalTerminalPanelEvent::Activated(self.id));
        }
    }

    fn on_added_to(&mut self, group: WeakEntity<TabGroup>, _: &mut Window, _: &mut Context<Self>) {
        self.tab_group = Some(group);
    }

    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.tab_group = None;
        self.terminal
            .update(cx, |terminal, cx| terminal.shutdown(cx));
        cx.emit(LocalTerminalPanelEvent::Closed(self.id));
    }
}

impl Panel for LocalTerminalPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ClosableTabTitle::new(CatalogIcon::Terminal, self.display_title(cx)).closable(
            ("close-local-terminal", self.id.0),
            Box::new(CloseLocalTerminal(self.id)),
        )
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        let id = self.id;
        Some(vec![
            Button::new(("restart-local-terminal-toolbar", id.0))
                .icon(Icon::new(CatalogIcon::RefreshCw))
                .tooltip("重新启动")
                .on_click(move |_, window, cx| {
                    window.dispatch_action(Box::new(RestartLocalTerminal(id)), cx)
                }),
        ])
    }

    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
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
            .menu("重新启动", Box::new(RestartLocalTerminal(self.id)))
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for LocalTerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lifecycle = self.terminal.read(cx).lifecycle(cx);
        let show_restart = matches!(
            lifecycle,
            TerminalLifecycle::Exited { .. } | TerminalLifecycle::Failed(_)
        );
        let id = self.id;

        v_flex()
            .size_full()
            .child(div().flex_1().min_h_0().child(self.terminal.clone()))
            .when(show_restart, |column| {
                column.child(
                    h_flex()
                        .id(("local-terminal-exit", id.0))
                        .test_support()
                        .justify_between()
                        .px_3()
                        .py_2()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted)
                        .text_sm()
                        .child(lifecycle.to_string())
                        .child(
                            Button::new(("restart-local-terminal", id.0))
                                .small()
                                .icon(Icon::new(CatalogIcon::RefreshCw))
                                .label("重新启动")
                                .on_click(move |_, window, cx| {
                                    window.dispatch_action(Box::new(RestartLocalTerminal(id)), cx)
                                }),
                        ),
                )
            })
    }
}
