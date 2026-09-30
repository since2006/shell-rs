use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    time::Duration,
};

use gpui_kit::base::animation::{EffectTransition, ease_in_out_cubic, ease_out_cubic};
use gpui_kit::base::{
    Placement, TooltipOverlay, TooltipRequest, TooltipTransition, Tree as BaseTree,
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    tooltip::Tooltip,
    tree::{TreeEntry, TreeEvent, TreeState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CollapseAllGroups, ConnectGroup, ConnectSelected, ConnectSession, CopySessionId,
    DeleteGroup, DeleteSession, DuplicateSession, EditSession, ExpandAllGroups, MoveSessionNode,
    NewChildGroup, NewGroup, NewSession, NewSessionInGroup, OpenExplorer, OpenSettings,
    RenameGroup, SESSION_PANEL_CONTEXT,
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
    /// Session rows' tooltips, which open beside the row rather than under
    /// the pointer so they never cover the rows below.
    row_tooltip: Entity<TooltipOverlay>,
    /// The row the tooltip was last opened for. See `render_row`.
    row_tooltip_owner: Rc<Cell<Option<SessionId>>>,
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
            row_tooltip: cx.new(|_| TooltipOverlay::new().render_with(animate_row_tooltip)),
            row_tooltip_owner: Rc::new(Cell::new(None)),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Rebuild the tree, then put the cursor on whatever the store just
    /// gained so a freshly created group or session is visible and selected.
    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let created = self.take_created_node(cx);
        self.rebuild_tree(cx);
        if let Some(node) = created {
            self.select_node(node, cx);
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
            let ancestors = created
                .map(|node| groups_above(store, node))
                .unwrap_or_default();
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

    /// Select (and reveal) a row, for example a fresh duplicate or a group
    /// just created.
    pub fn select_node(&mut self, node: SessionNode, cx: &mut Context<Self>) {
        self.rebuild_tree(cx);
        let row_id = node.id();
        self.tree_state.update(cx, |state, cx| {
            state.reveal_item(&row_id, ScrollStrategy::Center, cx);
            let ix = state.index_of(&row_id);
            state.set_selected_index(ix, cx);
        });
    }

    /// Keep the moved row visible even when it was dropped into a closed group.
    pub fn reveal_node(&mut self, node: SessionNode, cx: &mut Context<Self>) {
        let ancestors = groups_above(self.store.read(cx), node);
        self.expanded.extend(ancestors);
        self.select_node(node, cx);
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

/// Every group a node sits in, at any depth: the folders that have to be
/// open for its row to show.
fn groups_above(store: &SessionStore, node: SessionNode) -> Vec<GroupId> {
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
        // Plain snapshots for the row renderer: render callbacks must not
        // read entities.
        let host_os: Rc<HashMap<SessionId, HostOs>> = Rc::new(
            self.store
                .read(cx)
                .sessions()
                .iter()
                .filter_map(|session| session.os.map(|os| (session.id, os)))
                .collect(),
        );
        let addresses: Rc<HashMap<SessionId, SharedString>> = Rc::new(
            self.store
                .read(cx)
                .sessions()
                .iter()
                .map(|session| (session.id, session.address().into()))
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
            addresses,
            tooltip: self.row_tooltip.clone(),
            tooltip_owner: self.row_tooltip_owner.clone(),
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
            .child(self.row_tooltip.clone())
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
    /// `user@host:port` of every session, for the row tooltips.
    addresses: Rc<HashMap<SessionId, SharedString>>,
    tooltip: Entity<TooltipOverlay>,
    tooltip_owner: Rc<Cell<Option<SessionId>>>,
}

/// A session row's tooltip: where the session logs in, as `user@host:port`.
///
/// Drawn in the theme's inverse, dark on the light theme and light on the
/// dark one, so it stands out against the sidebar and the terminal in both.
/// Only this tooltip: gpui-kit's others share the popover colour with menus.
fn session_tooltip(address: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |window, cx| {
        let address = address.clone();
        let (background, foreground) = (cx.theme().foreground, cx.theme().background);
        Tooltip::element(move |_, _| {
            div()
                .id("session-tooltip")
                .test_support()
                .aria_label(address.clone())
                .child(address.clone())
        })
        .bg(background)
        .border_color(background)
        .text_color(foreground)
        .build(window, cx)
    }
}

/// The row tooltip's motion, after gpui-kit's own tooltips: it eases out of
/// the row when it first opens, then follows the pointer from row to row.
fn animate_row_tooltip(
    view: AnyView,
    transition: TooltipTransition,
    _: &mut Window,
    _: &mut App,
) -> AnyElement {
    let tooltip = div().child(view);
    match transition {
        TooltipTransition::Enter { epoch } => EffectTransition::new(Duration::from_millis(150))
            .ease(ease_out_cubic)
            .slide_x(px(-4.), px(0.))
            .fade(0., 1.)
            .apply(
                tooltip,
                ElementId::NamedInteger("session-tooltip-enter".into(), epoch as u64),
            )
            .into_any_element(),
        TooltipTransition::Switch {
            epoch,
            previous,
            current,
        } => EffectTransition::new(Duration::from_millis(200))
            .ease(ease_in_out_cubic)
            .slide_y(previous.center().y - current.center().y, px(0.))
            .apply(
                tooltip,
                ElementId::NamedInteger("session-tooltip-move".into(), epoch as u64),
            )
            .into_any_element(),
    }
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
        addresses,
        tooltip,
        tooltip_owner,
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
            .without_tooltip()
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
        .when_some(session_id, |row, id| {
            let content = Rc::new(session_tooltip(
                addresses.get(&id).cloned().unwrap_or_default(),
            ));
            let bounds = Rc::new(Cell::new(Bounds::default()));
            let (bounds_for_prepaint, show, hide) =
                (bounds.clone(), tooltip.clone(), tooltip.clone());
            let (owner, owner_for_click) = (tooltip_owner.clone(), tooltip_owner.clone());
            // Measures the row for the tooltip. The insets matter: ListItem's
            // content box is not `relative`, so without them the canvas would
            // land below the content instead of over it.
            row.child(
                canvas(
                    move |row_bounds, _, _| bounds_for_prepaint.set(row_bounds),
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .on_hover(move |hovered, window, cx| {
                // Not while a row is being dragged over the others.
                if *hovered && !cx.has_active_drag() {
                    owner.set(Some(id));
                    let content = content.clone();
                    let request =
                        TooltipRequest::new(bounds.get(), move |window, cx| content(window, cx))
                            .placement(Placement::Right);
                    show.update(cx, |overlay, cx| overlay.request_show(request, window, cx));
                } else if owner.get() == Some(id) {
                    // Only the row showing the tooltip may put it away. Going
                    // down the list, the next row reports the pointer arriving
                    // before this one reports it leaving, and this row's hide
                    // would cancel the tooltip the next row just asked for.
                    owner.set(None);
                    show.update(cx, |overlay, cx| overlay.request_hide(window, cx));
                }
            })
            // Out of the way of the menu, the click and the drag.
            .on_any_mouse_down(move |_, _, cx| {
                owner_for_click.set(None);
                hide.update(cx, |overlay, cx| overlay.hide(cx));
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
        "复制 ID",
        Icon::new(CatalogIcon::ClipboardCopy),
        Box::new(CopySessionId(id)),
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
