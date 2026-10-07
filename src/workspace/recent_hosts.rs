use std::{cell::Cell, rc::Rc};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::ListItem,
    menu::ContextMenuExt as _,
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, ConnectHost, ConnectSelected, NewHost, QuickConnect, RECENT_HOSTS_CONTEXT,
};
use crate::host::{HostId, HostOs, HostStore, host_menu};
use crate::shared::HostMark;

/// The center's empty state: shown in place of the tabs while none is open.
///
/// Lists the hosts most recently connected, newest first, so the next
/// connection is a double click away. Not a dock panel: the workspace's dock skin
/// lays it over the empty center, and it goes away as soon as a tab opens.
pub struct RecentHosts {
    store: Entity<HostStore>,
    /// The workspace's focus handle: actions dispatched on it reach the
    /// workspace handlers whatever is focused.
    target: FocusHandle,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    selected: Option<HostId>,
    /// The row a right click landed on, for the menu about to open. Blank
    /// space leaves it empty, and an empty menu does not open.
    menu_hit: Rc<Cell<Option<HostId>>>,
    _subscription: Subscription,
}

/// One row of the list: a plain snapshot taken from the store in `render`.
struct RecentRow {
    id: HostId,
    name: SharedString,
    address: String,
    group: Option<SharedString>,
    connected: bool,
    os: Option<HostOs>,
}

impl RecentHosts {
    pub fn new(store: Entity<HostStore>, target: FocusHandle, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            target,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            selected: None,
            menu_hit: Rc::new(Cell::new(None)),
            _subscription: subscription,
        }
    }

    /// A newly shown start page begins without a selected host.
    pub(super) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.selected.take().is_some() {
            cx.notify();
        }
    }

    fn rows(&self, cx: &App) -> Vec<RecentRow> {
        let store = self.store.read(cx);
        store
            .recent_hosts()
            .map(|host| RecentRow {
                id: host.id,
                name: host.name.clone(),
                address: host.endpoint(),
                // The full path, so two 数据库 groups under different
                // parents do not read the same here.
                group: host
                    .group
                    .map(|id| SharedString::from(store.group_path(id))),
                connected: host.state.is_connected(),
                os: host.os,
            })
            .collect()
    }

    fn render_header(&self, cx: &App) -> impl IntoElement {
        let target = self.target.clone();
        h_flex()
            .items_start()
            .justify_between()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::MEDIUM)
                            .child("最近连接"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("双击主机连接，或新建一台主机"),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("recent-quick-connect")
                            .primary()
                            .small()
                            .icon(IconName::Search)
                            .label("快速连接")
                            .on_click({
                                let target = target.clone();
                                move |_, window, cx| {
                                    target.dispatch_action(&QuickConnect, window, cx)
                                }
                            }),
                    )
                    .child(
                        Button::new("recent-new-host")
                            .small()
                            .icon(IconName::Plus)
                            .label("新建主机…")
                            .on_click(move |_, window, cx| {
                                target.dispatch_action(&NewHost, window, cx)
                            }),
                    ),
            )
    }

    fn render_empty(&self, cx: &App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .items_center()
            .gap_2()
            .py_8()
            .text_color(muted)
            .child(Icon::new(CatalogIcon::Server).large())
            .child(div().text_sm().child("还没有连接过的主机"))
    }

    fn on_connect_selected(
        &mut self,
        _: &ConnectSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.selected {
            self.target.dispatch_action(&ConnectHost(id), window, cx);
        }
    }

    /// The rows, with the host menu of the host tree. Like the tree's,
    /// the menu hangs off the list rather than off each row: capture clears
    /// the hit before a right-clicked row writes itself back, and the menu
    /// is built after both.
    fn render_list(&self, rows: Vec<RecentRow>, cx: &Context<Self>) -> impl IntoElement {
        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();
        v_flex()
            .id("recent-host-list")
            .gap_1()
            .capture_any_mouse_down(move |_, _, _| clear_hit.set(None))
            .children(rows.into_iter().map(|row| self.render_row(row, cx)))
            .context_menu({
                let store = self.store.clone();
                move |menu, _, cx| match menu_hit.get().and_then(|id| store.read(cx).host(id)) {
                    Some(host) => host_menu(menu, host),
                    None => menu,
                }
            })
    }

    fn render_row(&self, row: RecentRow, cx: &Context<Self>) -> ListItem {
        let muted = cx.theme().muted_foreground;
        let id = row.id;
        let selected = self.selected == Some(id);
        ListItem::new(("recent-host", id.0))
            .w_full()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .confirmed(selected)
            .when(selected, |item| {
                item.bg(crate::app::selected_row_color(cx.theme()))
            })
            .child(
                h_flex()
                    .gap_3()
                    .child(HostMark::new(
                        ("recent-host-os", id.0),
                        row.name.clone(),
                        row.os,
                    ))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::MEDIUM).child(row.name))
                                    .when_some(row.group, |row, group| {
                                        row.child(div().text_xs().text_color(muted).child(group))
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child(row.address),
                            ),
                    ),
            )
            .when(row.connected, |item| {
                item.suffix(|_, cx| {
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(
                            Icon::new(IconName::CircleCheck)
                                .xsmall()
                                .text_color(cx.theme().success),
                        )
                        .child("已连接")
                })
            })
            // A right click selects the row, as in Finder and Explorer, so
            // the menu visibly belongs to it.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.menu_hit.set(Some(id));
                    this.selected = Some(id);
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.selected = Some(id);
                this.focus_handle.focus(window, cx);
                cx.notify();
                if event.click_count() == 2 {
                    this.target.dispatch_action(&ConnectHost(id), window, cx);
                }
            }))
    }
}

impl Focusable for RecentHosts {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RecentHosts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);

        let page = div()
            .id("recent-host-page")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .flex()
            .flex_col()
            .items_center()
            .p_8()
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(40.))
                    .gap_6()
                    .child(self.render_header(cx))
                    .map(|page| {
                        if rows.is_empty() {
                            page.child(self.render_empty(cx))
                        } else {
                            page.child(self.render_list(rows, cx))
                        }
                    }),
            );
        // The bar goes on this, which does not scroll, rather than on the
        // page, which does: there it would scroll away with it.
        div()
            .id("recent-hosts")
            .test_support()
            .track_focus(&self.focus_handle)
            .key_context(RECENT_HOSTS_CONTEXT)
            .on_action(cx.listener(Self::on_connect_selected))
            .relative()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(page)
            .vertical_scrollbar(&self.scroll_handle)
    }
}
