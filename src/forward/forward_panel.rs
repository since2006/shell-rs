use std::{cell::Cell, collections::HashSet, rc::Rc};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{ForwardManager, ForwardStatus};
use crate::app::{
    CatalogIcon, DeleteForward, EditForward, FORWARD_PANEL_CONTEXT, NewForward, NewHost,
    SelectNextForward, SelectPreviousForward, StartForward, StopForward, ToggleSelectedForward,
};
use crate::host::{ForwardId, ForwardKind, HostStore, matches_forward_query};
use crate::shared::{RowTooltip, RowTooltips};

/// The port-forwarding list the left dock shows in place of the hosts:
/// every rule, whether it is running, and the switch that starts or stops it.
///
/// The rules live in the shared `HostStore` and what each is doing in the
/// `ForwardManager`; this panel observes both and owns only its selection
/// and search.
pub struct ForwardPanel {
    store: Entity<HostStore>,
    manager: Entity<ForwardManager>,
    /// The workspace's focus handle: actions dispatched on it reach the
    /// workspace handlers whatever is focused, which after a dialog closes
    /// may be nothing at all.
    target: FocusHandle,
    search: Entity<InputState>,
    query: String,
    selected: Option<ForwardId>,
    /// The rules the store held at the last change, which is how a rule just
    /// created in the dialog gets selected without the dialog reporting back.
    known: HashSet<ForwardId>,
    /// The row a right click landed on, for the menu about to open; `None`
    /// for the blank space below the rows.
    menu_hit: Rc<Cell<Option<ForwardId>>>,
    /// Rows' tooltips: the rule in full, which the row may cut short or
    /// replace with what went wrong.
    row_tooltips: RowTooltips<ForwardId>,
    scroll_handle: ScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// One row of the list: a plain snapshot taken in `render`.
struct ForwardRow {
    id: ForwardId,
    kind: ForwardKind,
    title: SharedString,
    /// 「本地转发 · 8080 → db:3306 · web-01」.
    detail: SharedString,
    status: ForwardStatus,
    /// What last went wrong with one connection while the forward runs.
    problem: Option<SharedString>,
}

impl ForwardPanel {
    pub fn new(
        store: Entity<HostStore>,
        manager: Entity<ForwardManager>,
        target: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索端口转发")
                .clean_on_escape()
        });
        let known = store
            .read(cx)
            .forwards()
            .iter()
            .map(|rule| rule.id)
            .collect();
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.on_store_changed(cx)),
            cx.observe(&manager, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            manager,
            target,
            search,
            query: String::new(),
            selected: None,
            known,
            menu_hit: Rc::new(Cell::new(None)),
            row_tooltips: RowTooltips::new("forward-tooltip", cx),
            scroll_handle: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Select whatever rule the store just gained, and let go of a selection
    /// whose rule is gone.
    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let current: Vec<ForwardId> = self
            .store
            .read(cx)
            .forwards()
            .iter()
            .map(|rule| rule.id)
            .collect();
        if let Some(created) = current.iter().rev().find(|id| !self.known.contains(id)) {
            self.selected = Some(*created);
        }
        if self.selected.is_some_and(|id| !current.contains(&id)) {
            self.selected = None;
        }
        self.known = current.into_iter().collect();
        cx.notify();
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The rule the list has selected, if it is still there.
    pub fn selected(&self) -> Option<ForwardId> {
        self.selected
    }

    /// What the dock's title bar shows while this list is up.
    pub fn title(&self) -> impl IntoElement + use<> {
        h_flex()
            .gap_1()
            .child(Icon::new(CatalogIcon::ArrowLeftRight).small())
            .child("端口转发")
    }

    /// The dock's toolbar while this list is up.
    pub fn toolbar_buttons(&self) -> Vec<Button> {
        let target = self.target.clone();
        vec![
            Button::new("new-forward")
                .icon(IconName::Plus)
                .tooltip("新建端口转发…")
                .accessibility_label("新建端口转发…")
                .on_click(move |_, window, cx| target.dispatch_action(&NewForward, window, cx)),
        ]
    }

    /// The rows the search lets through, in list order.
    fn rows(&self, cx: &App) -> Vec<ForwardRow> {
        let store = self.store.read(cx);
        let manager = self.manager.read(cx);
        store
            .forwards()
            .iter()
            .filter_map(|rule| {
                let host = store
                    .host(rule.host)
                    .map(|host| host.name.clone())
                    .unwrap_or_default();
                matches_forward_query(rule, &host, &self.query).then(|| ForwardRow {
                    id: rule.id,
                    kind: rule.kind,
                    title: rule.title(),
                    detail: if rule.name.is_empty() {
                        format!("{} · {host}", rule.kind.label())
                    } else {
                        format!("{} · {} · {host}", rule.kind.label(), rule.summary())
                    }
                    .into(),
                    status: manager.status(rule.id),
                    problem: manager.problem(rule.id),
                })
            })
            .collect()
    }

    fn on_toggle_selected(
        &mut self,
        _: &ToggleSelectedForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.selected {
            let active = self.manager.read(cx).is_active(id);
            toggle(id, active, &self.target, window, cx);
        }
    }

    fn on_select_previous(
        &mut self,
        _: &SelectPreviousForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(false, cx);
    }

    fn on_select_next(&mut self, _: &SelectNextForward, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(true, cx);
    }

    /// Move the selection one visible row, stopping at either end. With
    /// nothing selected it lands on the first row going down and on the last
    /// going up.
    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let rows: Vec<ForwardId> = self.rows(cx).iter().map(|row| row.id).collect();
        let Some(last) = rows.len().checked_sub(1) else {
            return;
        };
        let current = self
            .selected
            .and_then(|id| rows.iter().position(|row| *row == id));
        let next = match (current, down) {
            (Some(ix), true) => (ix + 1).min(last),
            (Some(ix), false) => ix.saturating_sub(1),
            (None, true) => 0,
            (None, false) => last,
        };
        self.selected = Some(rows[next]);
        self.scroll_handle.scroll_to_item(next);
        cx.notify();
    }

    fn render_empty(&self, cx: &Context<Self>) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let store = self.store.read(cx);
        let target = self.target.clone();
        let (title, hint, button) = if !store.forwards().is_empty() {
            ("没有匹配的端口转发", None, None)
        } else if store.hosts().is_empty() {
            (
                "还没有主机",
                Some("端口转发经由一台主机的 SSH 连接，先新建主机。"),
                Some(
                    Button::new("forward-empty-new-host")
                        .small()
                        .icon(IconName::Plus)
                        .label("新建主机…")
                        .on_click(move |_, window, cx| {
                            target.dispatch_action(&NewHost, window, cx)
                        }),
                ),
            )
        } else {
            (
                "还没有端口转发",
                Some("把本机的端口和服务器那一侧的网络接起来。"),
                Some(
                    Button::new("forward-empty-new")
                        .small()
                        .icon(IconName::Plus)
                        .label("新建端口转发…")
                        .on_click(move |_, window, cx| {
                            target.dispatch_action(&NewForward, window, cx)
                        }),
                ),
            )
        };
        v_flex()
            .id("forward-empty")
            .test_support()
            .aria_label(title)
            .items_center()
            .gap_2()
            .px_4()
            .py_8()
            .text_color(muted)
            .child(Icon::new(CatalogIcon::ArrowLeftRight).large())
            .child(div().text_sm().child(title))
            .when_some(hint, |view, hint| {
                view.child(div().text_xs().text_center().child(hint))
            })
            .when_some(button, |view, button| {
                view.child(div().pt_2().child(button))
            })
            .into_any_element()
    }

    fn render_row(&self, row: ForwardRow, cx: &Context<Self>) -> ListItem {
        let theme = cx.theme();
        let id = row.id;
        let selected = self.selected == Some(id);
        let active = row.status.is_active();
        // The second line says what is wrong when something is; otherwise
        // what the rule is. A reason is never cut short: it may be long, and
        // it is the only place that spells it out.
        let (line, line_color, wraps) = match (&row.status, &row.problem) {
            (ForwardStatus::Failed(reason), _) => (reason.clone(), theme.danger, true),
            (status @ ForwardStatus::Reconnecting { .. }, _) => {
                (status.label(), theme.warning, true)
            }
            (_, Some(problem)) if active => (problem.clone(), theme.warning, true),
            _ => (row.detail.clone(), theme.muted_foreground, false),
        };
        let tooltip = self
            .row_tooltips
            .row(id, RowTooltip::new(row.detail.clone()));
        let toggle_tooltip = tooltip.clone();
        let status = row.status.clone();
        let target = self.target.clone();

        let item = ListItem::new(("forward-row", id.0))
            .w_full()
            .px_2()
            .py_1p5()
            .rounded(theme.radius)
            .confirmed(selected)
            .aria_selected(selected)
            .when(selected, |item| {
                item.bg(crate::app::host_tree_selection_color(theme))
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        // A fixed slot, so titles line up whatever the icon.
                        h_flex()
                            .flex_shrink_0()
                            .size_5()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(kind_icon(row.kind).small()),
                    )
                    .child(
                        // Zero wide, growing and clipped, rather than
                        // `flex_1`: the row's wrappers are sized by their
                        // content, and text that may grow counts at its
                        // full width there unless it is clipped. A long
                        // line would then push the switch out of the list.
                        v_flex()
                            .w_0()
                            .flex_grow(1.)
                            .overflow_x_hidden()
                            .gap_0p5()
                            .child(div().truncate().child(row.title))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(line_color)
                                    .map(|line| if wraps { line } else { line.truncate() })
                                    .child(line),
                            ),
                    ),
            )
            .suffix(move |_, cx| {
                let target = target.clone();
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .child(status_mark(id, &status, cx))
                    // The switch has a tooltip of its own.
                    .child(
                        toggle_tooltip.exclude(
                            div().id(("forward-toggle-area", id.0)).child(
                                Button::new(("forward-toggle", id.0))
                                    .ghost()
                                    .xsmall()
                                    .icon(if active {
                                        CatalogIcon::Square
                                    } else {
                                        CatalogIcon::Play
                                    })
                                    .tooltip(if active { "停止" } else { "启动" })
                                    .accessibility_label(if active { "停止" } else { "启动" })
                                    .on_click(move |_, window, cx| {
                                        // The row's own click must not see this one:
                                        // a double click here is two toggles, not a
                                        // third from the row as well.
                                        cx.stop_propagation();
                                        toggle(id, active, &target, window, cx);
                                    }),
                            ),
                        ),
                    )
            });
        tooltip
            .attach(item)
            // A right click selects the row, as in Finder and Explorer, so
            // the menu visibly belongs to it.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.menu_hit.set(Some(id));
                    this.selected = Some(id);
                    // The menu's commands are dispatched from what is
                    // focused, which after a dialog may be nothing.
                    this.focus_handle.focus(window, cx);
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.selected = Some(id);
                this.focus_handle.focus(window, cx);
                cx.notify();
                if event.click_count() == 2 {
                    let active = this.manager.read(cx).is_active(id);
                    toggle(id, active, &this.target, window, cx);
                }
            }))
    }
}

/// Start a stopped forward, stop a running one.
fn toggle(id: ForwardId, active: bool, target: &FocusHandle, window: &mut Window, cx: &mut App) {
    if active {
        target.dispatch_action(&StopForward(id), window, cx);
    } else {
        target.dispatch_action(&StartForward(id), window, cx);
    }
}

/// The icon of a kind of forward, the same in the list and in the dialog:
/// an arrow into this machine's port, an arrow out of it, the wide world.
pub(super) fn kind_icon(kind: ForwardKind) -> Icon {
    match kind {
        ForwardKind::Local => Icon::new(CatalogIcon::ArrowRightToLine),
        ForwardKind::Remote => Icon::new(CatalogIcon::ArrowLeftToLine),
        ForwardKind::Dynamic => Icon::new(IconName::Globe),
    }
}

/// What a rule is doing, as a mark in a slot of fixed width: a spinner while
/// it is getting there, a tick with the number of connections while it runs,
/// an alert once it failed. The words are in `aria_label` and beside it.
fn status_mark(id: ForwardId, status: &ForwardStatus, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let mark = match status {
        ForwardStatus::Stopped => None,
        ForwardStatus::Connecting => Some(Spinner::new().xsmall().into_any_element()),
        ForwardStatus::Reconnecting { .. } => Some(
            Spinner::new()
                .xsmall()
                .color(theme.warning)
                .into_any_element(),
        ),
        ForwardStatus::Running { .. } => Some(
            Icon::new(IconName::CircleCheck)
                .xsmall()
                .text_color(theme.success)
                .into_any_element(),
        ),
        ForwardStatus::Failed(_) => Some(
            Icon::new(CatalogIcon::CircleAlert)
                .xsmall()
                .text_color(theme.danger)
                .into_any_element(),
        ),
    };
    let connections = match status {
        ForwardStatus::Running { connections } if *connections > 0 => Some(*connections),
        _ => None,
    };
    h_flex()
        .id(("forward-status", id.0))
        .test_support()
        .aria_label(status.label())
        .gap_1()
        .text_xs()
        .text_color(theme.muted_foreground)
        .when_some(connections, |view, connections| {
            view.child(connections.to_string())
        })
        .child(h_flex().size_4().justify_center().children(mark))
}

/// The menu of one rule, or of the blank space under the rows. Built when
/// it opens, so it can ask what the rule is doing right now.
fn build_context_menu(
    hit: Option<ForwardId>,
    manager: &ForwardManager,
    menu: PopupMenu,
) -> PopupMenu {
    let Some(id) = hit else {
        return menu.menu_with_icon(
            "新建端口转发…",
            Icon::new(IconName::Plus),
            Box::new(NewForward),
        );
    };
    let menu = if manager.is_active(id) {
        menu.menu_with_icon(
            "停止",
            Icon::new(CatalogIcon::Square),
            Box::new(StopForward(id)),
        )
    } else {
        menu.menu_with_icon(
            "启动",
            Icon::new(CatalogIcon::Play),
            Box::new(StartForward(id)),
        )
    };
    menu.separator()
        .menu_with_icon(
            "编辑端口转发…",
            Icon::new(CatalogIcon::Pencil),
            Box::new(EditForward(id)),
        )
        .separator()
        .menu_with_icon(
            "删除",
            Icon::new(CatalogIcon::Trash),
            Box::new(DeleteForward(id)),
        )
}

impl Focusable for ForwardPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ForwardPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();
        let manager = self.manager.clone();

        v_flex()
            .id("forward-panel")
            .key_context(FORWARD_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_toggle_selected))
            .on_action(cx.listener(Self::on_select_previous))
            .on_action(cx.listener(Self::on_select_next))
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                div().p_2().child(
                    Input::new(&self.search)
                        .id("forward-search")
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                ),
            )
            .child(
                // The menu hangs off the list, not off the rows, like the
                // host tree's: capture clears the hit before a
                // right-clicked row writes itself back, and the menu is
                // built after both.
                div()
                    .id("forward-list")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("forward-rows")
                            .size_full()
                            .px_1()
                            .gap_0p5()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .map(|list| {
                                if rows.is_empty() {
                                    list.child(self.render_empty(cx))
                                } else {
                                    list.children(
                                        rows.into_iter().map(|row| self.render_row(row, cx)),
                                    )
                                }
                            }),
                    )
                    // On the list, which does not scroll, rather than on
                    // the rows, which do: there the bar would scroll away
                    // with them.
                    .vertical_scrollbar(&self.scroll_handle)
                    .capture_any_mouse_down(move |event, _, _| {
                        if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                            clear_hit.set(None);
                        }
                    })
                    .context_menu(move |menu, _, cx| {
                        build_context_menu(menu_hit.get(), manager.read(cx), menu)
                    }),
            )
            .child(self.row_tooltips.overlay())
    }
}
