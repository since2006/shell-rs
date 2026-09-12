use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, ThemeStyled as _,
    button::Button,
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    tooltip::Tooltip,
    tree::{TreeEntry, TreeEvent, TreeState, tree},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, ConnectSelected, ConnectSession, DeleteGroup, DeleteSession, DisconnectSession,
    DuplicateSession, EditSession, NewChildGroup, NewGroup, NewSession, NewSessionInGroup,
    OpenExplorer, RenameGroup, SESSION_PANEL_CONTEXT,
};

use super::{
    GroupId, HostOs, SessionId, SessionNode, SessionStore, matches_query, session_tree_items,
};

/// The left dock panel: a searchable, grouped tree of sessions.
///
/// Owns the tree and search state; the session data lives in the shared
/// `SessionStore`, which this panel observes.
pub struct SessionPanel {
    store: Entity<SessionStore>,
    tree_state: Entity<TreeState>,
    search: Entity<InputState>,
    query: String,
    expanded: HashSet<GroupId>,
    /// What the store held the last time the tree was rebuilt. Comparing
    /// against these is how a group or session created in a dialog gets
    /// revealed without the dialog having to report back to the panel.
    known_groups: HashSet<GroupId>,
    known_sessions: HashSet<SessionId>,
    /// Which node the last right-click landed on, `None` for the blank space
    /// below the rows. Shared with the row renderer and the context menu
    /// builder, both of which run outside this entity.
    right_clicked: Rc<Cell<Option<SessionNode>>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SessionPanel {
    pub fn new(store: Entity<SessionStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (expanded, known_groups, known_sessions, items) = {
            let read = store.read(cx);
            let expanded: HashSet<GroupId> = read.groups().iter().map(|g| g.id).collect();
            let items = session_tree_items(read.groups(), read.sessions(), "", &expanded);
            (
                expanded.clone(),
                expanded,
                read.sessions().iter().map(|s| s.id).collect(),
                items,
            )
        };
        let tree_state = cx.new(|cx| TreeState::new(cx).items(items));
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索会话")
                .clean_on_escape()
        });

        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.on_store_changed(cx)),
            cx.subscribe_in(
                &search,
                window,
                |this, state, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.query = state.read(cx).value().to_string();
                        this.rebuild_tree(cx);
                    }
                    InputEvent::PressEnter { .. } => this.connect_first_match(window, cx),
                    _ => {}
                },
            ),
            cx.subscribe(&tree_state, |this, _, event: &TreeEvent, _| match event {
                TreeEvent::Expanded(id) => {
                    if let Some(SessionNode::Group(group)) = SessionNode::parse(id) {
                        this.expanded.insert(group);
                    }
                }
                TreeEvent::Collapsed(id) => {
                    if let Some(SessionNode::Group(group)) = SessionNode::parse(id) {
                        this.expanded.remove(&group);
                    }
                }
            }),
        ];

        Self {
            store,
            tree_state,
            search,
            query: String::new(),
            expanded,
            known_groups,
            known_sessions,
            right_clicked: Rc::new(Cell::new(None)),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Rebuild the tree, then put the cursor on whatever the store just
    /// gained so a freshly created group or session is visible and selected.
    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let created = self.take_created_node(cx);
        self.rebuild_tree(cx);
        match created {
            Some(SessionNode::Group(id)) => self.select_group(id, cx),
            Some(SessionNode::Session(id)) => self.select_session(id, cx),
            None => {}
        }
    }

    /// The node the store gained since the last rebuild, if any, with the
    /// folders above it opened so it can be scrolled to.
    fn take_created_node(&mut self, cx: &mut Context<Self>) -> Option<SessionNode> {
        let (created, ancestors, groups, sessions) = {
            let store = self.store.read(cx);
            let created = store
                .groups()
                .iter()
                .rev()
                .find(|group| !self.known_groups.contains(&group.id))
                .map(|group| SessionNode::Group(group.id))
                .or_else(|| {
                    store
                        .sessions()
                        .iter()
                        .rev()
                        .find(|session| !self.known_sessions.contains(&session.id))
                        .map(|session| SessionNode::Session(session.id))
                });
            let ancestors = match created {
                Some(SessionNode::Group(id)) => store.ancestor_groups(id),
                Some(SessionNode::Session(id)) => store
                    .session(id)
                    .and_then(|session| session.group)
                    .map(|group| {
                        let mut chain = store.ancestor_groups(group);
                        chain.push(group);
                        chain
                    })
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            (
                created,
                ancestors,
                store.groups().iter().map(|g| g.id).collect(),
                store.sessions().iter().map(|s| s.id).collect(),
            )
        };
        self.known_groups = groups;
        self.known_sessions = sessions;
        self.expanded.extend(ancestors);
        created
    }

    /// Rebuild the tree from the store, keeping the selection by id.
    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let items = {
            let store = self.store.read(cx);
            session_tree_items(
                store.groups(),
                store.sessions(),
                &self.query,
                &self.expanded,
            )
        };
        let selected_id = self
            .tree_state
            .read(cx)
            .selected_item()
            .map(|item| item.id.clone());
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items, cx);
            let ix = selected_id.and_then(|id| state.index_of(&id));
            state.set_selected_index(ix, cx);
        });
        cx.notify();
    }

    /// Select (and reveal) a session row, for example a fresh duplicate.
    pub fn select_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.rebuild_tree(cx);
        let row_id = SessionNode::Session(id).id();
        self.tree_state.update(cx, |state, cx| {
            state.reveal_item(&row_id, ScrollStrategy::Center, cx);
            let ix = state.index_of(&row_id);
            state.set_selected_index(ix, cx);
        });
    }

    /// Select (and reveal) a group row, for example one just created.
    pub fn select_group(&mut self, id: GroupId, cx: &mut Context<Self>) {
        self.rebuild_tree(cx);
        let row_id = SessionNode::Group(id).id();
        self.tree_state.update(cx, |state, cx| {
            state.reveal_item(&row_id, ScrollStrategy::Center, cx);
            let ix = state.index_of(&row_id);
            state.set_selected_index(ix, cx);
        });
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The node currently selected in the tree.
    pub fn selected_node(&self, cx: &App) -> Option<SessionNode> {
        self.tree_state
            .read(cx)
            .selected_item()
            .and_then(|item| SessionNode::parse(&item.id))
    }

    fn connect_first_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = self
            .store
            .read(cx)
            .sessions()
            .iter()
            .find(|session| matches_query(session, &self.query))
            .map(|session| session.id);
        if let Some(id) = first {
            window.dispatch_action(Box::new(ConnectSession(id)), cx);
        }
    }

    fn on_connect_selected(
        &mut self,
        _: &ConnectSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_node(cx) {
            Some(SessionNode::Session(id)) => {
                window.dispatch_action(Box::new(ConnectSession(id)), cx);
            }
            Some(SessionNode::Group(group)) => {
                if !self.expanded.remove(&group) {
                    self.expanded.insert(group);
                }
                self.rebuild_tree(cx);
            }
            None => {}
        }
    }
}

impl EventEmitter<PanelEvent> for SessionPanel {}

impl Focusable for SessionPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for SessionPanel {
    fn panel_name(&self) -> &'static str {
        "SessionPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for SessionPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(CatalogIcon::Server).small())
            .child("会话")
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        Some(vec![
            Button::new("new-group")
                .icon(Icon::new(CatalogIcon::FolderPlus))
                .tooltip("新建分组…")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NewGroup), cx)),
            Button::new("new-session-panel")
                .icon(IconName::Plus)
                .tooltip("新建会话…")
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NewSession), cx)),
        ])
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for SessionPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A plain snapshot for the row renderer: render callbacks must not
        // read entities.
        let connected: Rc<HashSet<SessionId>> = Rc::new(
            self.store
                .read(cx)
                .sessions()
                .iter()
                .filter(|session| session.state.is_connected())
                .map(|session| session.id)
                .collect(),
        );
        let connected_for_menu = connected.clone();
        // Same reason: the marks in front of the rows are a snapshot too.
        let host_os: Rc<HashMap<SessionId, HostOs>> = Rc::new(
            self.store
                .read(cx)
                .sessions()
                .iter()
                .filter_map(|session| session.os.map(|os| (session.id, os)))
                .collect(),
        );
        let clicked_row = self.right_clicked.clone();
        let clicked_blank = self.right_clicked.clone();
        let clicked_menu = self.right_clicked.clone();

        v_flex()
            .id("session-panel")
            .key_context(SESSION_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_connect_selected))
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                div().p_2().child(
                    Input::new(&self.search)
                        .id("session-search")
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                ),
            )
            .child(
                div()
                    .id("session-tree")
                    .flex_1()
                    .min_h_0()
                    .child(
                        tree(&self.tree_state, move |_, entry, _, _, cx| {
                            render_row(entry, &connected, &host_os, &clicked_row, cx)
                        })
                        .px_1(),
                    )
                    // The menu hangs off the container, not off the rows: a
                    // row's menu would be built while the virtualized list is
                    // being prepainted, and the focus it takes there lands in
                    // the middle of the frame, which trips gpui's "set_focus
                    // called more than once in a single frame" assertion.
                    // Capture runs before every bubble handler, so this clears
                    // the target and the row that was hit writes itself back;
                    // a click on blank space leaves it cleared.
                    .capture_any_mouse_down(move |event, _, _| {
                        if event.button == MouseButton::Right {
                            clicked_blank.set(None);
                        }
                    })
                    .context_menu(move |menu, _, _| {
                        build_context_menu(clicked_menu.get(), &connected_for_menu, menu)
                    }),
            )
    }
}

/// The mark in front of a session: the host's operating system on a badge in
/// that project's own colour, or the first character of the session name until
/// a probe succeeds.
///
/// The badge borrows `Avatar`'s treatment — a bordered circle at the theme's
/// radius — so identity marks look the same wherever the product shows one.
/// The border is what keeps a black or white brand readable against either
/// theme.
fn session_mark(id: SessionId, label: &SharedString, os: Option<HostOs>, cx: &App) -> AnyElement {
    let (background, ink, glyph, description) = match os {
        Some(os) => {
            let (background, ink) = match (os.brand_color(), os.brand_foreground()) {
                (Some(background), Some(ink)) => (background.into(), ink.into()),
                // A monochrome mark follows the theme, which is also how
                // Apple's own guidance draws it.
                _ => (cx.theme().foreground, cx.theme().background),
            };
            (
                background,
                ink,
                Icon::default()
                    .path(os.icon_path())
                    .xsmall()
                    .into_any_element(),
                SharedString::from(os.label()),
            )
        }
        None => (
            cx.theme().muted,
            cx.theme().muted_foreground,
            div()
                .text_xs()
                .child(
                    label
                        .chars()
                        .next()
                        .map(|character| character.to_uppercase().to_string())
                        .unwrap_or_default(),
                )
                .into_any_element(),
            SharedString::from("未探测到系统"),
        ),
    };
    let tooltip = description.clone();
    h_flex()
        .id(("session-os", id.0))
        .test_support()
        .aria_label(description)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .flex_shrink_0()
        .size_5()
        .justify_center()
        .rounded_full_style(cx)
        .border_1()
        .border_color(cx.theme().border)
        .bg(background)
        .text_color(ink)
        .child(glyph)
        .into_any_element()
}

/// A row that is not a session still has to reserve the badge's width, or the
/// labels of groups and sessions would not line up.
fn plain_mark(icon: Icon, cx: &App) -> AnyElement {
    h_flex()
        .flex_shrink_0()
        .size_5()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .child(icon.small())
        .into_any_element()
}

fn render_row(
    entry: &TreeEntry,
    connected: &HashSet<SessionId>,
    host_os: &HashMap<SessionId, HostOs>,
    right_clicked: &Rc<Cell<Option<SessionNode>>>,
    cx: &mut App,
) -> ListItem {
    let item = entry.item();
    let node = SessionNode::parse(&item.id);
    let (mark, row_id): (AnyElement, ElementId) = match node {
        Some(SessionNode::Group(id)) => (
            plain_mark(
                Icon::new(if entry.is_expanded() {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                }),
                cx,
            ),
            ("group-row", id.0).into(),
        ),
        Some(SessionNode::Session(id)) => (
            session_mark(id, &item.label, host_os.get(&id).copied(), cx),
            ("session-row", id.0).into(),
        ),
        None => (
            plain_mark(Icon::new(IconName::File), cx),
            item.id.clone().into(),
        ),
    };
    let session_id = node.and_then(SessionNode::session_id);
    let is_connected = session_id.is_some_and(|id| connected.contains(&id));

    ListItem::new(row_id)
        .w_full()
        .px_2()
        .rounded(cx.theme().radius)
        .pl(rems(0.75 + entry.depth() as f32))
        .child(h_flex().gap_2().child(mark).child(item.label.clone()))
        .when(is_connected, |row| {
            row.suffix(|_, cx| {
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
        .when_some(session_id, |row, id| {
            row.on_click(move |event: &ClickEvent, window, cx| {
                if event.click_count() == 2 {
                    window.dispatch_action(Box::new(ConnectSession(id)), cx);
                }
            })
        })
        .when_some(node, |row, node| {
            let right_clicked = right_clicked.clone();
            row.on_mouse_down(MouseButton::Right, move |_, _, _| {
                right_clicked.set(Some(node));
            })
        })
}

fn build_context_menu(
    node: Option<SessionNode>,
    connected: &HashSet<SessionId>,
    menu: PopupMenu,
) -> PopupMenu {
    match node {
        Some(SessionNode::Session(id)) => menu
            .menu_with_icon(
                "连接",
                Icon::new(CatalogIcon::Plug),
                Box::new(ConnectSession(id)),
            )
            .menu_with_disabled(
                "断开",
                Box::new(DisconnectSession(id)),
                !connected.contains(&id),
            )
            .menu_with_icon(
                "打开 SFTP",
                Icon::new(CatalogIcon::FolderTree),
                Box::new(OpenExplorer(id)),
            )
            .separator()
            .menu_with_icon(
                "编辑会话…",
                Icon::new(CatalogIcon::Pencil),
                Box::new(EditSession(id)),
            )
            .menu_with_icon(
                "复制",
                Icon::new(IconName::Copy),
                Box::new(DuplicateSession(id)),
            )
            .separator()
            .menu_with_icon(
                "删除",
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteSession(id)),
            ),
        Some(SessionNode::Group(id)) => menu
            .menu_with_icon(
                "新建会话…",
                Icon::new(IconName::Plus),
                Box::new(NewSessionInGroup(id)),
            )
            .menu_with_icon(
                "新建子分组…",
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewChildGroup(id)),
            )
            .separator()
            .menu_with_icon(
                "重命名分组…",
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameGroup(id)),
            )
            .separator()
            .menu_with_icon(
                "删除分组",
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteGroup(id)),
            ),
        None => menu
            .menu_with_icon("新建会话…", Icon::new(IconName::Plus), Box::new(NewSession))
            .menu_with_icon(
                "新建分组…",
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewGroup),
            ),
    }
}
