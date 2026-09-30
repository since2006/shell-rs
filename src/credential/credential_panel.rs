use std::{cell::Cell, collections::HashSet, rc::Rc};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CREDENTIAL_PANEL_CONTEXT, CatalogIcon, DeleteCredential, EditCredential,
    EditSelectedCredential, NewCredential, SelectNextCredential, SelectPreviousCredential,
};
use crate::session::{CredentialId, CredentialKind, SessionStore, matches_credential_query};

/// The credential list the left dock shows in place of the sessions: every
/// saved login and how many hosts use it.
///
/// The credentials live in the shared `SessionStore`; this panel observes it
/// and owns only its selection and search.
pub struct CredentialPanel {
    store: Entity<SessionStore>,
    /// The workspace's focus handle: actions dispatched on it reach the
    /// workspace handlers whatever is focused, which after a dialog closes
    /// may be nothing at all.
    target: FocusHandle,
    search: Entity<InputState>,
    query: String,
    selected: Option<CredentialId>,
    /// The credentials the store held at the last change, which is how one
    /// just created in the dialog gets selected without the dialog reporting
    /// back.
    known: HashSet<CredentialId>,
    /// The row a right click landed on, for the menu about to open; `None`
    /// for the blank space below the rows.
    menu_hit: Rc<Cell<Option<CredentialId>>>,
    scroll_handle: ScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// One row of the list: a plain snapshot taken in `render`.
struct CredentialRow {
    id: CredentialId,
    kind: CredentialKind,
    name: SharedString,
    /// 「root · 密码 · 3 台主机」.
    detail: SharedString,
    /// The detail with the whole key path, for the tooltip.
    tooltip: SharedString,
}

impl CredentialPanel {
    pub fn new(
        store: Entity<SessionStore>,
        target: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索凭据")
                .clean_on_escape()
        });
        let known = store
            .read(cx)
            .credentials()
            .iter()
            .map(|credential| credential.id)
            .collect();
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.on_store_changed(cx)),
            cx.subscribe(&search, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            target,
            search,
            query: String::new(),
            selected: None,
            known,
            menu_hit: Rc::new(Cell::new(None)),
            scroll_handle: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Select whatever credential the store just gained, and let go of a
    /// selection whose credential is gone.
    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let current: Vec<CredentialId> = self
            .store
            .read(cx)
            .credentials()
            .iter()
            .map(|credential| credential.id)
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

    /// The credential the list has selected, if it is still there.
    pub fn selected(&self) -> Option<CredentialId> {
        self.selected
    }

    /// What the dock's title bar shows while this list is up.
    pub fn title(&self) -> impl IntoElement + use<> {
        h_flex()
            .gap_1()
            .child(Icon::new(CatalogIcon::KeyRound).small())
            .child("凭据")
    }

    /// The dock's toolbar while this list is up.
    pub fn toolbar_buttons(&self) -> Vec<Button> {
        let target = self.target.clone();
        vec![
            Button::new("new-credential")
                .icon(IconName::Plus)
                .tooltip("新建凭据…")
                .accessibility_label("新建凭据…")
                .on_click(move |_, window, cx| target.dispatch_action(&NewCredential, window, cx)),
        ]
    }

    /// The rows the search lets through, in list order.
    fn rows(&self, cx: &App) -> Vec<CredentialRow> {
        let store = self.store.read(cx);
        store
            .credentials()
            .iter()
            .filter(|credential| matches_credential_query(credential, &self.query))
            .map(|credential| {
                let hosts = store.sessions_using(credential.id).count();
                let summary = credential.summary();
                let detail = match hosts {
                    0 => summary,
                    hosts => format!("{summary} · {hosts} 台主机"),
                };
                let tooltip = match credential.key_path.as_deref() {
                    Some(path) => format!("{} · {path}", credential.user),
                    None => detail.clone(),
                };
                CredentialRow {
                    id: credential.id,
                    kind: credential.kind,
                    name: credential.name.clone(),
                    detail: detail.into(),
                    tooltip: tooltip.into(),
                }
            })
            .collect()
    }

    fn on_edit_selected(
        &mut self,
        _: &EditSelectedCredential,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.selected {
            self.target.dispatch_action(&EditCredential(id), window, cx);
        }
    }

    fn on_select_previous(
        &mut self,
        _: &SelectPreviousCredential,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(false, cx);
    }

    fn on_select_next(&mut self, _: &SelectNextCredential, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(true, cx);
    }

    /// Move the selection one visible row, stopping at either end. With
    /// nothing selected it lands on the first row going down and on the last
    /// going up.
    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let rows: Vec<CredentialId> = self.rows(cx).iter().map(|row| row.id).collect();
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
        let target = self.target.clone();
        let (title, hint, button) = if !self.store.read(cx).credentials().is_empty() {
            ("没有匹配的凭据", None, None)
        } else {
            (
                "还没有凭据",
                Some("保存一套用户名和密码、私钥或 SSH Agent，让多台主机共用。"),
                Some(
                    Button::new("credential-empty-new")
                        .small()
                        .icon(IconName::Plus)
                        .label("新建凭据…")
                        .on_click(move |_, window, cx| {
                            target.dispatch_action(&NewCredential, window, cx)
                        }),
                ),
            )
        };
        v_flex()
            .id("credential-empty")
            .test_support()
            .aria_label(title)
            .items_center()
            .gap_2()
            .px_4()
            .py_8()
            .text_color(muted)
            .child(Icon::new(CatalogIcon::KeyRound).large())
            .child(div().text_sm().child(title))
            .when_some(hint, |view, hint| {
                view.child(div().text_xs().text_center().child(hint))
            })
            .when_some(button, |view, button| {
                view.child(div().pt_2().child(button))
            })
            .into_any_element()
    }

    fn render_row(&self, row: CredentialRow, cx: &Context<Self>) -> ListItem {
        let theme = cx.theme();
        let id = row.id;
        let selected = self.selected == Some(id);
        let tooltip = row.tooltip.clone();

        ListItem::new(("credential-row", id.0))
            .w_full()
            .px_2()
            .py_1p5()
            .rounded(theme.radius)
            .confirmed(selected)
            .aria_selected(selected)
            .when(selected, |item| {
                item.bg(crate::app::session_tree_selection_color(theme))
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(
                        // A fixed slot, so names line up whatever the icon.
                        h_flex()
                            .flex_shrink_0()
                            .size_5()
                            .justify_center()
                            .text_color(theme.muted_foreground)
                            .child(kind_icon(row.kind).small()),
                    )
                    .child(
                        // Zero wide, growing and clipped, like the forward
                        // list's: text that may grow otherwise counts at
                        // its full width in the row's content-sized
                        // wrappers.
                        v_flex()
                            .w_0()
                            .flex_grow(1.)
                            .overflow_x_hidden()
                            .gap_0p5()
                            .child(div().truncate().child(row.name))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(row.detail),
                            ),
                    ),
            )
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
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
                    this.target.dispatch_action(&EditCredential(id), window, cx);
                }
            }))
    }
}

/// The icon of a kind of credential, the same in the list and wherever else
/// one is named: a masked field, a key file, the agent holding the user's
/// keys.
pub fn kind_icon(kind: CredentialKind) -> Icon {
    match kind {
        CredentialKind::Password => Icon::new(CatalogIcon::RectangleEllipsis),
        CredentialKind::Key => Icon::new(CatalogIcon::FileKey),
        CredentialKind::Agent => Icon::new(CatalogIcon::UserRoundKey),
    }
}

/// The menu of one credential, or of the blank space under the rows.
fn build_context_menu(hit: Option<CredentialId>, menu: PopupMenu) -> PopupMenu {
    let Some(id) = hit else {
        return menu.menu_with_icon(
            "新建凭据…",
            Icon::new(IconName::Plus),
            Box::new(NewCredential),
        );
    };
    menu.menu_with_icon(
        "编辑凭据…",
        Icon::new(CatalogIcon::Pencil),
        Box::new(EditCredential(id)),
    )
    .separator()
    .menu_with_icon(
        "删除",
        Icon::new(CatalogIcon::Trash),
        Box::new(DeleteCredential(id)),
    )
}

impl Focusable for CredentialPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CredentialPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows(cx);
        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();

        v_flex()
            .id("credential-panel")
            .key_context(CREDENTIAL_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_edit_selected))
            .on_action(cx.listener(Self::on_select_previous))
            .on_action(cx.listener(Self::on_select_next))
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                div().p_2().child(
                    Input::new(&self.search)
                        .id("credential-search")
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                ),
            )
            .child(
                // The menu hangs off the list, not off the rows, like the
                // session tree's and the forward list's.
                div()
                    .id("credential-list")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_flex()
                            .id("credential-rows")
                            .size_full()
                            .px_1()
                            .gap_0p5()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .vertical_scrollbar(&self.scroll_handle)
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
                    .capture_any_mouse_down(move |event, _, _| {
                        if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                            clear_hit.set(None);
                        }
                    })
                    .context_menu(move |menu, _, _| build_context_menu(menu_hit.get(), menu)),
            )
    }
}
