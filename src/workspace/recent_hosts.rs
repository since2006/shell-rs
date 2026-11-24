use std::{cell::Cell, rc::Rc};

use crate::shared::RightClickMenu as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::ListItem,
    scroll::ScrollableElement as _,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, ConnectHost, ConnectSelected, NewHost, OpenPromotion, QuickConnect,
    RECENT_HOSTS_CONTEXT,
};
use crate::host::{HostId, HostOs, HostStore, ProtocolKind, host_menu};
use crate::i18n::t;
use crate::promotion::{PromotionView, Promotions};
use crate::shared::HostMark;

/// The center's empty state: shown in place of the tabs while none is open.
///
/// Lists the hosts most recently connected, newest first, so the next
/// connection is a double click away. Not a dock panel: the workspace's dock skin
/// lays it over the empty center, and it goes away as soon as a tab opens.
/// The title and its buttons stay at the top and the promotions at the
/// bottom; only the list scrolls between them.
pub struct RecentHosts {
    store: Entity<HostStore>,
    promotions: Entity<Promotions>,
    /// The workspace's focus handle: actions dispatched on it reach the
    /// workspace handlers whatever is focused.
    target: FocusHandle,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    selected: Option<HostId>,
    /// The row a right click landed on, for the menu about to open. Blank
    /// space leaves it empty, and an empty menu does not open.
    menu_hit: Rc<Cell<Option<HostId>>>,
    /// One for each promotion shown, in order, so the keyboard reaches them.
    promotion_focus: Vec<FocusHandle>,
    _subscriptions: [Subscription; 2],
}

/// One row of the list: a plain snapshot taken from the store in `render`.
struct RecentRow {
    id: HostId,
    name: SharedString,
    address: String,
    group: Option<SharedString>,
    os: Option<HostOs>,
    protocol: ProtocolKind,
    /// The host's own address, which colours a mark without a brand.
    host_address: SharedString,
}

impl RecentHosts {
    pub fn new(
        store: Entity<HostStore>,
        promotions: Entity<Promotions>,
        target: FocusHandle,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = [
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&promotions, |_, _, cx| cx.notify()),
        ];
        Self {
            store,
            promotions,
            target,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            selected: None,
            menu_hit: Rc::new(Cell::new(None)),
            promotion_focus: Vec::new(),
            _subscriptions: subscriptions,
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
                os: host.os,
                protocol: host.kind(),
                host_address: host.address.clone(),
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
                            .child(t!("workspace.recent.title")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("workspace.recent.hint")),
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
                            .label(t!("workspace.recent.find_host"))
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
                            .label(t!("workspace.recent.new_host"))
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
            .child(div().text_sm().child(t!("workspace.recent.empty")))
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
                    .child(
                        HostMark::new(
                            ("recent-host-os", id.0),
                            row.name.clone(),
                            row.host_address.clone(),
                            row.os,
                        )
                        .protocol(row.protocol),
                    )
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

impl RecentHosts {
    /// Up to two banners, then up to four text links, two to a row, below
    /// a line that the list scrolls under. One alone in its row takes the
    /// left column, the size it has beside another.
    fn render_promotions(
        &mut self,
        view: PromotionView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = view.banners.len() + view.links.len();
        while self.promotion_focus.len() < count {
            self.promotion_focus.push(cx.focus_handle());
        }
        let theme = cx.theme();
        let dark = theme.is_dark();
        let mut focus = self.promotion_focus.iter();
        let banners: Vec<_> = view
            .banners
            .into_iter()
            .enumerate()
            .zip(focus.by_ref())
            .map(|((ix, banner), focus)| {
                promotion_link(
                    ("promotion-banner", ix),
                    banner.alt,
                    banner.url,
                    focus,
                    &self.target,
                )
                .w_full()
                .aspect_ratio(3.)
                .overflow_hidden()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .hover(|style| style.opacity(0.9))
                .active(|style| style.opacity(0.8))
                .focus_visible(|style| style.border_color(theme.ring))
                .child(
                    img(ImageSource::Render(banner.image))
                        .size_full()
                        .rounded(theme.radius_lg)
                        .object_fit(ObjectFit::Cover),
                )
                .test_support()
            })
            .collect();
        let links: Vec<_> = view
            .links
            .into_iter()
            .enumerate()
            .zip(focus)
            .map(|((ix, link), focus)| {
                let color = link
                    .color(dark)
                    .map(|color| Hsla::from(rgb(color)))
                    // The body's color, as the hosts' names: the link
                    // blue stands out of the neutral interface.
                    .unwrap_or(theme.foreground);
                promotion_link(
                    ("promotion-link", ix),
                    link.text.clone(),
                    link.url,
                    focus,
                    &self.target,
                )
                .min_w_0()
                .px_1()
                .rounded(theme.radius)
                .border_1()
                .border_color(transparent_black())
                // Underlined only when pointed at or focused: the area says what
                // it holds, the line says which one opens.
                .hover(|style| style.opacity(0.8).underline())
                .focus_visible(|style| style.border_color(theme.ring).underline())
                .child(
                    h_flex()
                        .gap_1p5()
                        .min_w_0()
                        // A size above the body's, to be seen beside the
                        // pictures.
                        .text_lg()
                        .text_color(color)
                        .when_some(link.icon, |row, icon| {
                            row.child(Icon::new(icon).text_color(color))
                        })
                        .child(div().min_w_0().truncate().child(link.text)),
                )
                .test_support()
            })
            .collect();
        div()
            .id("start-promotions")
            .test_support()
            .flex_none()
            .w_full()
            .child(
                band(
                    v_flex()
                        .gap_3()
                        .pt_4()
                        .border_t_1()
                        .border_color(theme.border)
                        .when(!banners.is_empty(), |area| {
                            area.child(div().grid().grid_cols(2).gap_3().children(banners))
                        })
                        .when(!links.is_empty(), |area| {
                            area.child(
                                div()
                                    .grid()
                                    .grid_cols(2)
                                    .gap_x_4()
                                    .gap_y_2()
                                    .children(links),
                            )
                        }),
                )
                .pb_6(),
            )
    }
}

/// A promotion the pointer and the keyboard open alike (Enter or Space
/// once focused, as GPUI clicks a focused element), through the
/// workspace's handler; its label also its tooltip, as a banner shows no
/// text and a link may be cut short. The caller adds `test_support` last,
/// after its look.
fn promotion_link(
    id: impl Into<ElementId>,
    label: SharedString,
    url: SharedString,
    focus: &FocusHandle,
    target: &FocusHandle,
) -> Stateful<Div> {
    let target = target.clone();
    let tip = label.clone();
    div()
        .id(id)
        .role(Role::Link)
        .aria_label(label)
        .track_focus(focus)
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(move |_, window, cx| {
            target.dispatch_action(&OpenPromotion { url: url.clone() }, window, cx)
        })
}

/// A strip across the page whose content keeps to its column, so the
/// title, the list and the promotions line up.
fn band(content: impl IntoElement) -> Div {
    div()
        .w_full()
        .flex()
        .justify_center()
        .px_8()
        .child(div().w_full().max_w(rems(40.)).child(content))
}

impl Focusable for RecentHosts {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RecentHosts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        let promotions = self.promotions.read(cx).view(crate::i18n::locale());
        let list = if rows.is_empty() {
            self.render_empty(cx).into_any_element()
        } else {
            self.render_list(rows, cx).into_any_element()
        };
        let hosts = v_flex()
            .id("recent-hosts")
            .test_support()
            .track_focus(&self.focus_handle)
            .key_context(RECENT_HOSTS_CONTEXT)
            .on_action(cx.listener(Self::on_connect_selected))
            .flex_1()
            .min_h_0()
            .w_full()
            .child(band(self.render_header(cx)).flex_none().pt_8().pb_6())
            .child(
                // The bar goes on this, which does not scroll, rather than
                // on the rows, which do: there it would scroll away with
                // them.
                div()
                    .id("recent-host-scroll")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(
                        div()
                            .id("recent-host-rows")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .child(band(list).pb_6()),
                    )
                    .vertical_scrollbar(&self.scroll_handle),
            );
        // The promotions beside the list's key context, not in it: Enter on
        // one opens it rather than connecting the selected host.
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(hosts)
            .when(!promotions.is_empty(), |page| {
                page.child(self.render_promotions(promotions, cx))
            })
    }
}
