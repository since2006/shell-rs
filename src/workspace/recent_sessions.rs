use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, button::Button, h_flex, list::ListItem,
    scroll::ScrollableElement as _, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{CatalogIcon, ConnectSession, NewSession};
use crate::session::{HostOs, SessionId, SessionStore};
use crate::shared::HostMark;

/// The center's empty state: shown in place of the tabs while none is open.
///
/// Lists the sessions most recently connected, newest first, so the next
/// connection is one click away. Not a dock panel: the workspace's dock skin
/// lays it over the empty center, and it goes away as soon as a tab opens.
pub struct RecentSessions {
    store: Entity<SessionStore>,
    /// The workspace's focus handle: actions dispatched on it reach the
    /// workspace handlers whatever is focused.
    target: FocusHandle,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    _subscription: Subscription,
}

/// One row of the list: a plain snapshot taken from the store in `render`.
struct RecentRow {
    id: SessionId,
    name: SharedString,
    address: String,
    group: Option<SharedString>,
    connected: bool,
    os: Option<HostOs>,
}

impl RecentSessions {
    pub fn new(store: Entity<SessionStore>, target: FocusHandle, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            target,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            _subscription: subscription,
        }
    }

    fn rows(&self, cx: &App) -> Vec<RecentRow> {
        let store = self.store.read(cx);
        store
            .recent_sessions()
            .map(|session| RecentRow {
                id: session.id,
                name: session.name.clone(),
                address: session.address(),
                // The full path, so two 数据库 groups under different
                // parents do not read the same here.
                group: session
                    .group
                    .map(|id| SharedString::from(store.group_path(id))),
                connected: session.state.is_connected(),
                os: session.os,
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
                            .child("选择一个会话继续，或新建一个会话"),
                    ),
            )
            .child(
                Button::new("recent-new-session")
                    .icon(IconName::Plus)
                    .label("新建会话…")
                    .on_click(move |_, window, cx| target.dispatch_action(&NewSession, window, cx)),
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
            .child(div().text_sm().child("还没有连接过的会话"))
            .child(div().text_xs().child("双击左侧的会话即可连接"))
    }

    fn render_row(&self, row: RecentRow, cx: &App) -> ListItem {
        let muted = cx.theme().muted_foreground;
        let target = self.target.clone();
        let id = row.id;
        ListItem::new(("recent-session", id.0))
            .w_full()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .child(
                h_flex()
                    .gap_3()
                    .child(HostMark::new(
                        ("recent-session-os", id.0),
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
            .on_click(move |_, window, cx| target.dispatch_action(&ConnectSession(id), window, cx))
    }
}

impl Focusable for RecentSessions {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RecentSessions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);

        div()
            .id("recent-sessions")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle)
            .vertical_scrollbar(&self.scroll_handle)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
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
                            page.child(
                                v_flex()
                                    .gap_1()
                                    .children(rows.into_iter().map(|row| self.render_row(row, cx))),
                            )
                        }
                    }),
            )
    }
}
