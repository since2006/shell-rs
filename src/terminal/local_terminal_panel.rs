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

use crate::app::{CatalogIcon, CenterTab, CloseLocalTerminal, RestartLocalTerminal};
use crate::i18n::t;
use crate::shared::{ClosableTabTitle, close_tab_items};

use super::{
    LocalTerminalId, SharedTerminalTransportFactory, TerminalEvent, TerminalLifecycle,
    TerminalNotice, TerminalStatus, TerminalView,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalTerminalPanelEvent {
    Activated(LocalTerminalId),
    Closed(LocalTerminalId),
    StatusChanged(LocalTerminalId),
    Notice(LocalTerminalId, TerminalNotice),
}

/// A local login-shell Dock tab backed by the shared terminal view.
pub struct LocalTerminalPanel {
    id: LocalTerminalId,
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
        let terminal = cx.new(|cx| {
            TerminalView::new(
                ("local-terminal", id.0),
                move |_| default_title(id),
                factory,
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.observe(&terminal, move |_, _, cx| {
                cx.emit(LocalTerminalPanelEvent::StatusChanged(id));
                cx.notify();
            }),
            cx.subscribe(&terminal, move |_, _, event: &TerminalEvent, cx| {
                if let TerminalEvent::Notice(notice) = event {
                    cx.emit(LocalTerminalPanelEvent::Notice(id, notice.clone()));
                }
            }),
        ];

        Self {
            id,
            terminal,
            tab_group: None,
            _subscriptions: subscriptions,
        }
    }

    /// 「本地终端 1」: what a notification calls it, whatever title the
    /// shell gives the tab.
    pub fn default_title(&self) -> SharedString {
        default_title(self.id)
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

    fn display_title(&self, cx: &App) -> SharedString {
        self.terminal
            .read(cx)
            .title(cx)
            .map_or_else(|| self.default_title(), SharedString::from)
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

    /// Closing goes through `CloseLocalTerminal`; see `ClosableTabTitle`.
    /// Saying no here also keeps the dock from adding a second 「关闭」 to the
    /// 「…」 menu, beside the tab's own close commands.
    fn closable(&self, _: &App) -> bool {
        false
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
        let (id, group, panel) = (self.id, self.tab_group.clone(), cx.entity_id());
        ClosableTabTitle::new(
            ("local-terminal-tab", self.id.0),
            Icon::new(CatalogIcon::Terminal).small(),
            self.display_title(cx),
        )
        .closable(
            ("close-local-terminal", self.id.0),
            Box::new(CloseLocalTerminal(self.id)),
        )
        .context_menu(move |menu, _, cx| tab_menu(menu, id, group.clone(), panel, cx))
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        let id = self.id;
        Some(vec![
            Button::new(("restart-local-terminal-toolbar", id.0))
                .icon(Icon::new(CatalogIcon::RefreshCw))
                .tooltip(t!("terminal.local.restart"))
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
        tab_menu(menu, self.id, self.tab_group.clone(), cx.entity_id(), cx)
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
                                .label(t!("terminal.local.restart"))
                                .on_click(move |_, window, cx| {
                                    window.dispatch_action(Box::new(RestartLocalTerminal(id)), cx)
                                }),
                        ),
                )
            })
    }
}

/// 「本地终端 1」, in the interface language.
fn default_title(id: LocalTerminalId) -> SharedString {
    t!("terminal.local.title", number = id.0)
}

/// The commands of a local terminal tab, shared by its context menu and the
/// tab bar's 「…」 menu.
fn tab_menu(
    menu: PopupMenu,
    id: LocalTerminalId,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    cx: &App,
) -> PopupMenu {
    let menu = menu
        .menu_with_icon(
            t!("terminal.local.restart"),
            Icon::new(CatalogIcon::RefreshCw),
            Box::new(RestartLocalTerminal(id)),
        )
        .separator();
    close_tab_items(menu, CenterTab::LocalTerminal(id), group, panel, cx)
}
