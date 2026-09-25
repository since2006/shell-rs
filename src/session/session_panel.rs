use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::base::Tree as BaseTree;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    tree::{TreeEntry, TreeEvent, TreeState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CollapseAllGroups, ConnectGroup, ConnectSelected, ConnectSession, DeleteGroup,
    DeleteSession, DuplicateSession, EditSession, ExpandAllGroups, MoveSessionNode, NewChildGroup,
    NewGroup, NewSession, NewSessionInGroup, OpenExplorer, OpenSettings, RenameGroup,
    SESSION_PANEL_CONTEXT,
};

use crate::shared::HostMark;

use super::{
    GroupId, HostOs, NodeDrop, SessionId, SessionNode, SessionStore, matches_query,
    session_tree_items,
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
    drop_target: Rc<Cell<Option<(SessionNode, NodeDrop)>>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SessionPanel {
    pub fn new(store: Entity<SessionStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (expanded, known_groups, known_sessions, items) = {
            let read = store.read(cx);
            let expanded: HashSet<GroupId> = read
                .groups()
                .iter()
                .filter(|group| group.expanded)
                .map(|group| group.id)
                .collect();
            let items = session_tree_items(read.groups(), read.sessions(), "", &expanded);
            (
                expanded,
                read.groups().iter().map(|group| group.id).collect(),
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
            cx.subscribe(&tree_state, |this, _, event: &TreeEvent, cx| {
                if !this.query.trim().is_empty() {
                    return;
                }
                let (id, expanded) = match event {
                    TreeEvent::Expanded(id) => (id, true),
                    TreeEvent::Collapsed(id) => (id, false),
                };
                if let Some(SessionNode::Group(group)) = SessionNode::parse(id) {
                    if expanded {
                        this.expanded.insert(group);
                    } else {
                        this.expanded.remove(&group);
                    }
                    this.store.update(cx, |store, cx| {
                        store.set_group_expanded(group, expanded, cx);
                    });
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
            drop_target: Rc::new(Cell::new(None)),
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
        if let Some(SessionNode::Group(id)) = created
            && self
                .store
                .read(cx)
                .group(id)
                .is_some_and(|group| group.expanded)
        {
            self.expanded.insert(id);
        }
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

    /// Keep the moved row visible even when it was dropped into a closed group.
    pub fn reveal_node(&mut self, node: SessionNode, cx: &mut Context<Self>) {
        let ancestors = {
            let store = self.store.read(cx);
            let parent = match node {
                SessionNode::Group(id) => store.group(id).and_then(|group| group.parent),
                SessionNode::Session(id) => store.session(id).and_then(|session| session.group),
            };
            parent
                .map(|id| {
                    let mut chain = store.ancestor_groups(id);
                    chain.push(id);
                    chain
                })
                .unwrap_or_default()
        };
        self.expanded.extend(ancestors);
        match node {
            SessionNode::Group(id) => self.select_group(id, cx),
            SessionNode::Session(id) => self.select_session(id, cx),
        }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Refresh the visible tree after a bulk expansion change in the store.
    pub fn set_all_groups_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.expanded = if expanded {
            self.store
                .read(cx)
                .groups()
                .iter()
                .map(|group| group.id)
                .collect()
        } else {
            HashSet::new()
        };
        self.right_clicked.set(None);
        self.rebuild_tree(cx);
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
                if !self.query.trim().is_empty() {
                    return;
                }
                let expanded = if self.expanded.remove(&group) {
                    false
                } else {
                    self.expanded.insert(group);
                    true
                };
                self.store.update(cx, |store, cx| {
                    store.set_group_expanded(group, expanded, cx);
                });
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
        // Same reason: the marks in front of the rows are a snapshot too.
        let host_os: Rc<HashMap<SessionId, HostOs>> = Rc::new(
            self.store
                .read(cx)
                .sessions()
                .iter()
                .filter_map(|session| session.os.map(|os| (session.id, os)))
                .collect(),
        );
        let group_parents: Rc<HashMap<GroupId, Option<GroupId>>> = Rc::new(
            self.store
                .read(cx)
                .groups()
                .iter()
                .map(|group| (group.id, group.parent))
                .collect(),
        );
        let group_counts = Rc::new(group_host_counts(
            self.store.read(cx).sessions(),
            &group_parents,
        ));
        let clicked_row = self.right_clicked.clone();
        let clicked_blank = self.right_clicked.clone();
        let clicked_menu = self.right_clicked.clone();
        let drop_target = self.drop_target.clone();
        let drop_target_for_root = self.drop_target.clone();
        let drop_target_for_move = self.drop_target.clone();
        let can_reorder = self.query.trim().is_empty();
        let row_interactions = Rc::new(RowInteractions {
            group_parents,
            right_clicked: clicked_row,
            drop_target,
            can_reorder,
        });
        let tree_scroll = self.tree_state.read(cx).scroll_handle().clone();

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
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .size_full()
                            .px_1()
                            .child(
                                BaseTree::new(&self.tree_state)
                                    .item(move |_, entry, state, _, cx| {
                                        let node = SessionNode::parse(&entry.item().id);
                                        let selected = match row_interactions.right_clicked.get() {
                                            Some(right_clicked) => node == Some(right_clicked),
                                            None => state.is_selected(),
                                        };
                                        render_row(
                                            entry,
                                            selected,
                                            &group_counts,
                                            &host_os,
                                            &row_interactions,
                                            cx,
                                        )
                                        .disabled(entry.is_disabled())
                                        .into_any_element()
                                    })
                                    .list_style(
                                        StyleRefinement::default().flex_grow_1().size_full(),
                                    )
                                    .relative()
                                    .size_full(),
                            )
                            .vertical_scrollbar(&tree_scroll),
                    )
                    // The menu hangs off the container, not off the rows: a
                    // row's menu would be built while the virtualized list is
                    // being prepainted, and the focus it takes there lands in
                    // the middle of the frame, which trips gpui's "set_focus
                    // called more than once in a single frame" assertion.
                    // Capture runs before every bubble handler. Clear the
                    // context selection on any new click; a right-clicked row
                    // writes itself back before the menu is built.
                    .capture_any_mouse_down(move |event, _, _| {
                        if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                            clicked_blank.set(None);
                        }
                    })
                    .when(can_reorder, |view| {
                        view.on_drag_move(move |_: &DragMoveEvent<DraggedSessionNode>, _, _| {
                            drop_target_for_move.set(None);
                        })
                        .on_drop(
                            move |drag: &DraggedSessionNode, window, cx| {
                                if drop_target_for_root.get().is_none() {
                                    window.dispatch_action(
                                        Box::new(MoveSessionNode {
                                            source: drag.node,
                                            destination: NodeDrop::Root,
                                        }),
                                        cx,
                                    );
                                }
                                drop_target_for_root.set(None);
                            },
                        )
                    })
                    .context_menu(move |menu, _, _| build_context_menu(clicked_menu.get(), menu)),
            )
            .child(
                // Pinned under the tree, where desktop apps keep settings.
                h_flex()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().sidebar_border)
                    .child(
                        Button::new("open-settings")
                            .ghost()
                            .small()
                            .icon(Icon::new(CatalogIcon::Settings))
                            .label("设置")
                            .tooltip_with_action("打开设置", &OpenSettings, None)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(OpenSettings), cx)
                            }),
                    ),
            )
    }
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

struct RowInteractions {
    group_parents: Rc<HashMap<GroupId, Option<GroupId>>>,
    right_clicked: Rc<Cell<Option<SessionNode>>>,
    drop_target: Rc<Cell<Option<(SessionNode, NodeDrop)>>>,
    can_reorder: bool,
}

/// Count hosts in each group, including hosts in nested child groups.
fn group_host_counts(
    sessions: &[super::Session],
    parents: &HashMap<GroupId, Option<GroupId>>,
) -> HashMap<GroupId, usize> {
    let mut counts = HashMap::new();
    for session in sessions {
        let mut group = session.group;
        for _ in 0..parents.len() {
            let Some(id) = group else { break };
            *counts.entry(id).or_insert(0) += 1;
            group = parents.get(&id).copied().flatten();
        }
    }
    counts
}

fn render_row(
    entry: &TreeEntry,
    selected: bool,
    group_counts: &HashMap<GroupId, usize>,
    host_os: &HashMap<SessionId, HostOs>,
    interactions: &RowInteractions,
    cx: &mut App,
) -> ListItem {
    let RowInteractions {
        group_parents,
        right_clicked,
        drop_target,
        can_reorder,
    } = interactions;
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
            HostMark::new(
                ("session-os", id.0),
                item.label.clone(),
                host_os.get(&id).copied(),
            )
            .small()
            .into_any_element(),
            ("session-row", id.0).into(),
        ),
        None => (
            plain_mark(Icon::new(IconName::File), cx),
            item.id.clone().into(),
        ),
    };
    let session_id = node.and_then(SessionNode::session_id);

    ListItem::new(row_id)
        .w_full()
        .px_2()
        .rounded(cx.theme().radius)
        // Make selection stronger than hover against the sidebar background.
        .confirmed(selected)
        .when(selected, |row| {
            row.bg(crate::app::session_tree_selection_color(cx.theme()))
        })
        .pl(rems(0.75 + entry.depth() as f32))
        .child(h_flex().gap_2().child(mark).child(item.label.clone()))
        .when_some(node.and_then(SessionNode::group_id), |row, id| {
            let count = group_counts.get(&id).copied().unwrap_or(0);
            row.suffix(move |_, cx| {
                div()
                    .id(("group-count", id.0))
                    .test_support()
                    .aria_label(format!("{count} 台主机"))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                    .child(count.to_string())
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
        .when(*can_reorder, |row| {
            row.when_some(node, |row, node| {
                let drop_target_for_move = drop_target.clone();
                let drop_target_for_style = drop_target.clone();
                let drop_target_for_drop = drop_target.clone();
                let parents_for_move = group_parents.clone();
                let label = item.label.clone();
                row.on_drag(DraggedSessionNode { node, label }, |drag, _, _, cx| {
                    cx.new(|_| drag.clone())
                })
                .on_drag_move(move |event: &DragMoveEvent<DraggedSessionNode>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        let source = event.drag(cx).node;
                        let relative = (event.event.position.y - event.bounds.top())
                            / event.bounds.size.height;
                        let destination = match (source, node) {
                            (SessionNode::Session(_), SessionNode::Group(id)) => NodeDrop::Into(id),
                            (_, SessionNode::Group(id)) if relative > 0.25 && relative < 0.75 => {
                                NodeDrop::Into(id)
                            }
                            _ if relative < 0.5 => NodeDrop::Before(node),
                            _ => NodeDrop::After(node),
                        };
                        if valid_drop(source, destination, &parents_for_move) {
                            drop_target_for_move.set(Some((node, destination)));
                        }
                    }
                })
                .drag_over::<DraggedSessionNode>(move |style, drag, _, cx| {
                    if drag.node == node {
                        return style;
                    }
                    match drop_target_for_style.get() {
                        Some((target, NodeDrop::Before(_))) if target == node => {
                            style.border_t_1().border_color(cx.theme().primary)
                        }
                        Some((target, NodeDrop::After(_))) if target == node => {
                            style.border_b_1().border_color(cx.theme().primary)
                        }
                        Some((target, NodeDrop::Into(_))) if target == node => style
                            .bg(cx.theme().accent)
                            .border_1()
                            .border_color(cx.theme().primary),
                        _ => style,
                    }
                })
                .on_drop(move |drag: &DraggedSessionNode, window, cx| {
                    if let Some((target, destination)) = drop_target_for_drop.get()
                        && target == node
                        && drag.node != node
                    {
                        window.dispatch_action(
                            Box::new(MoveSessionNode {
                                source: drag.node,
                                destination,
                            }),
                            cx,
                        );
                    }
                    drop_target_for_drop.set(None);
                    cx.stop_propagation();
                })
            })
        })
}

#[derive(Clone)]
struct DraggedSessionNode {
    node: SessionNode,
    label: SharedString,
}

impl Render for DraggedSessionNode {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .child(self.label.clone())
    }
}

/// Use the render snapshot to keep impossible group drops from looking active.
fn valid_drop(
    source: SessionNode,
    destination: NodeDrop,
    parents: &HashMap<GroupId, Option<GroupId>>,
) -> bool {
    let target = match destination {
        NodeDrop::Before(target) | NodeDrop::After(target) => Some(target),
        _ => None,
    };
    if target == Some(source) {
        return false;
    }
    match source {
        SessionNode::Session(_) => match destination {
            NodeDrop::Before(SessionNode::Session(_))
            | NodeDrop::After(SessionNode::Session(_))
            | NodeDrop::Root => true,
            NodeDrop::Into(group) => parents.contains_key(&group),
            _ => false,
        },
        SessionNode::Group(id) => {
            let mut parent = match destination {
                NodeDrop::Before(SessionNode::Group(target))
                | NodeDrop::After(SessionNode::Group(target)) => match parents.get(&target) {
                    Some(parent) => *parent,
                    None => return false,
                },
                NodeDrop::Into(group) if parents.contains_key(&group) => Some(group),
                NodeDrop::Root => None,
                _ => return false,
            };
            for _ in 0..=parents.len() {
                let Some(current) = parent else {
                    return true;
                };
                if current == id {
                    return false;
                }
                parent = match parents.get(&current) {
                    Some(parent) => *parent,
                    None => return false,
                };
            }
            false
        }
    }
}

/// The menu of one session, wherever it is listed: the session tree and the
/// start page's recent sessions both build it here, so they cannot drift.
pub fn session_menu(menu: PopupMenu, id: SessionId) -> PopupMenu {
    menu.menu_with_icon(
        "连接",
        Icon::new(CatalogIcon::Plug),
        Box::new(ConnectSession(id)),
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
    )
}

fn build_context_menu(node: Option<SessionNode>, menu: PopupMenu) -> PopupMenu {
    match node {
        Some(SessionNode::Session(id)) => session_menu(menu, id),
        Some(SessionNode::Group(id)) => menu
            .menu_with_icon(
                "连接组内主机",
                Icon::new(CatalogIcon::Plug),
                Box::new(ConnectGroup(id)),
            )
            .separator()
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
                "展开全部分组",
                Icon::new(IconName::FolderOpen),
                Box::new(ExpandAllGroups),
            )
            .menu_with_icon(
                "折叠全部分组",
                Icon::new(IconName::Folder),
                Box::new(CollapseAllGroups),
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
