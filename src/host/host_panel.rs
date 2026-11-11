use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, VirtualListScrollHandle,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    tree::TreeItem,
    v_flex, v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, CollapseAllGroups, CollapseSelectedGroup, ConnectGroup, ConnectHost,
    ConnectSelected, CopyHostAddress, CopyHostId, DeleteGroup, DeleteHost, DuplicateHost, EditHost,
    ExpandAllGroups, ExpandSelectedGroup, HOST_PANEL_CONTEXT, HOST_TREE_CONTEXT, ImportKnownHosts,
    ImportSshConfig, MoveHostNode, NewChildGroup, NewGroup, NewHost, NewHostInGroup, OpenExplorer,
    RenameGroup, SelectNextHostRow, SelectPreviousHostRow, ToggleHostDetails,
};

use crate::i18n::{UiLocale, t, tn};
use crate::shared::{HostMark, RowTooltip, RowTooltips};

use super::{
    GroupId, Host, HostId, HostNode, HostOs, HostStore, NodeDrop, ProtocolKind,
    first_matching_host, host_tree_items,
};

/// 显示主机详情: whether the host tree's rows show each host's address and
/// notes under its name. Off until the user turns it on with the button
/// beside the search field; kept in the settings and set from them like
/// `ShowHiddenFiles`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShowHostDetails(pub bool);

impl Global for ShowHostDetails {}

impl ShowHostDetails {
    pub fn get(cx: &App) -> bool {
        cx.try_global::<Self>().is_some_and(|details| details.0)
    }
}

/// The host list of the left dock: a searchable, grouped tree of hosts.
/// The workspace's sidebar shows it, and borrows its title and toolbar for
/// the dock's title bar while it does.
///
/// Owns the tree's rows, selection, folds and search; the host data lives
/// in the shared `HostStore`, which this panel observes.
///
/// One line a row, the host's name, until 显示主机详情 adds its address and
/// notes under it: rows then differ in height, so the tree is a virtual
/// list of measured rows rather than gpui-kit's `Tree`, whose rows must all
/// be as tall as the first.
pub struct HostPanel {
    store: Entity<HostStore>,
    search: Entity<InputState>,
    query: String,
    expanded: HashSet<GroupId>,
    /// Groups folded while searching, which otherwise opens every group it
    /// lists. Only for that search: forgotten when it changes, and not
    /// saved.
    folded_in_search: HashSet<GroupId>,
    /// The tree's rows, depth first, as the store and the folds last made
    /// them.
    rows: Rc<Vec<TreeRow>>,
    /// The row selected, by what it stands for, so the selection stays with
    /// its group or host while rows come and go around it.
    selected: Option<HostNode>,
    /// 显示主机详情, as the settings last said.
    details: bool,
    scroll_handle: VirtualListScrollHandle,
    /// What the store held the last time the tree was rebuilt. Comparing
    /// against these is how a group or host created in a dialog gets
    /// revealed without the dialog having to report back to the panel.
    known_groups: HashSet<GroupId>,
    seen_hosts: HashSet<HostId>,
    /// Which node the last right-click landed on, `None` for the blank space
    /// below the rows. Shared with the row renderer and the context menu
    /// builder, both of which run outside this entity.
    right_clicked: Rc<Cell<Option<HostNode>>>,
    drop_target: Rc<Cell<Option<(HostNode, NodeDrop)>>>,
    /// Host rows' tooltips: where the host logs in, and its notes.
    row_tooltips: RowTooltips<HostId>,
    /// The tree's own, so the arrow keys move through the tree and not
    /// through the search field.
    tree_focus: FocusHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

/// One line of the tree: a group or a host, as deep as it sits.
#[derive(Clone, Debug, PartialEq)]
struct TreeRow {
    node: HostNode,
    label: SharedString,
    depth: usize,
    /// A group whose rows show under it.
    expanded: bool,
}

impl HostPanel {
    pub fn new(store: Entity<HostStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (expanded, known_groups, seen_hosts) = {
            let read = store.read(cx);
            (
                read.groups()
                    .iter()
                    .filter(|group| group.expanded)
                    .map(|group| group.id)
                    .collect(),
                read.groups().iter().map(|group| group.id).collect(),
                read.hosts().iter().map(|s| s.id).collect(),
            )
        };
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.panel.search"))
                .clean_on_escape()
        });

        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.on_store_changed(cx)),
            cx.observe_global::<ShowHostDetails>(|this, cx| {
                this.set_details(ShowHostDetails::get(cx), cx)
            }),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.search.update(cx, |search, cx| {
                    search.set_placeholder(t!("host.panel.search"), window, cx)
                });
            }),
            cx.subscribe_in(
                &search,
                window,
                |this, state, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.query = state.read(cx).value().to_string();
                        this.folded_in_search.clear();
                        this.rebuild_tree(cx);
                    }
                    InputEvent::PressEnter { .. } => this.connect_first_match(window, cx),
                    _ => {}
                },
            ),
        ];

        let mut panel = Self {
            store,
            search,
            query: String::new(),
            expanded,
            folded_in_search: HashSet::new(),
            rows: Rc::default(),
            selected: None,
            details: ShowHostDetails::get(cx),
            scroll_handle: VirtualListScrollHandle::new(),
            known_groups,
            seen_hosts,
            right_clicked: Rc::new(Cell::new(None)),
            drop_target: Rc::new(Cell::new(None)),
            row_tooltips: RowTooltips::new("host-tooltip", cx),
            tree_focus: cx.focus_handle(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        panel.rows = Rc::new(panel.tree_rows(cx));
        panel
    }

    /// Rebuild the tree, then put the cursor on whatever the store just
    /// gained so a freshly created group or host is visible and selected.
    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let created = self.take_created_node(cx);
        self.rebuild_tree(cx);
        if let Some(node) = created {
            self.select_node(node, cx);
        }
    }

    /// The node the store gained since the last rebuild, if any, with the
    /// folders above it opened so it can be scrolled to.
    fn take_created_node(&mut self, cx: &mut Context<Self>) -> Option<HostNode> {
        let (created, ancestors, groups, hosts) = {
            let store = self.store.read(cx);
            let created = store
                .groups()
                .iter()
                .rev()
                .find(|group| !self.known_groups.contains(&group.id))
                .map(|group| HostNode::Group(group.id))
                .or_else(|| {
                    store
                        .hosts()
                        .iter()
                        .rev()
                        .find(|host| !self.seen_hosts.contains(&host.id))
                        .map(|host| HostNode::Host(host.id))
                });
            let ancestors = created
                .map(|node| groups_above(store, node))
                .unwrap_or_default();
            (
                created,
                ancestors,
                store.groups().iter().map(|g| g.id).collect(),
                store.hosts().iter().map(|s| s.id).collect(),
            )
        };
        self.known_groups = groups;
        self.seen_hosts = hosts;
        self.expanded.extend(ancestors);
        if let Some(HostNode::Group(id)) = created
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

    /// The rows the store and the folds make now.
    fn tree_rows(&self, cx: &App) -> Vec<TreeRow> {
        let store = self.store.read(cx);
        let items = host_tree_items(store.groups(), store.hosts(), &self.query, &self.expanded);
        let mut rows = Vec::new();
        flatten(&items, 0, &self.folded_in_search, &mut rows);
        rows
    }

    /// Rebuild the tree from the store. The selection stays on its node
    /// while that still shows.
    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        self.rows = Rc::new(self.tree_rows(cx));
        if self
            .selected
            .is_some_and(|node| self.row_index(node).is_none())
        {
            self.selected = None;
        }
        cx.notify();
    }

    fn row_index(&self, node: HostNode) -> Option<usize> {
        self.rows.iter().position(|row| row.node == node)
    }

    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    /// Select (and reveal) a row, for example a fresh duplicate or a group
    /// just created.
    pub fn select_node(&mut self, node: HostNode, cx: &mut Context<Self>) {
        self.rebuild_tree(cx);
        self.selected = None;
        if let Some(ix) = self.row_index(node) {
            self.selected = Some(node);
            self.scroll_handle
                .scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    /// Keep the moved row visible even when it was dropped into a closed group.
    pub fn reveal_node(&mut self, node: HostNode, cx: &mut Context<Self>) {
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
            self.folded_in_search.clear();
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

    /// The rows change height under the scroll position: the selection is
    /// kept in sight.
    fn set_details(&mut self, details: bool, cx: &mut Context<Self>) {
        if self.details == details {
            return;
        }
        self.details = details;
        if let Some(ix) = self.selected.and_then(|node| self.row_index(node)) {
            self.scroll_handle
                .scroll_to_item(ix, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Fold a group away or unfold it, as a click on it or Enter does: saved
    /// with the group, except while searching, where it lasts as long as
    /// the search.
    fn toggle_group(&mut self, group: GroupId, cx: &mut Context<Self>) {
        if self.searching() {
            if !self.folded_in_search.remove(&group) {
                self.folded_in_search.insert(group);
            }
        } else {
            let expanded = !self.expanded.remove(&group);
            if expanded {
                self.expanded.insert(group);
            }
            self.store.update(cx, |store, cx| {
                store.set_group_expanded(group, expanded, cx);
            });
        }
        self.rebuild_tree(cx);
    }

    /// A press on a row selects it; on a group it also folds or unfolds it.
    fn press_row(&mut self, node: HostNode, cx: &mut Context<Self>) {
        self.selected = Some(node);
        match node {
            HostNode::Group(group) => self.toggle_group(group, cx),
            HostNode::Host(_) => cx.notify(),
        }
    }

    fn connect_first_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let first = first_matching_host(store.groups(), store.hosts(), &self.query);
        if let Some(id) = first {
            window.dispatch_action(Box::new(ConnectHost(id)), cx);
        }
    }

    fn on_connect_selected(
        &mut self,
        _: &ConnectSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected {
            Some(HostNode::Host(id)) => {
                window.dispatch_action(Box::new(ConnectHost(id)), cx);
            }
            Some(HostNode::Group(group)) => self.toggle_group(group, cx),
            None => {}
        }
    }

    fn on_select_previous(
        &mut self,
        _: &SelectPreviousHostRow,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_selection(false, cx);
    }

    fn on_select_next(&mut self, _: &SelectNextHostRow, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(true, cx);
    }

    /// Move the selection one row, round from either end to the other. With
    /// nothing selected it lands on the first row going down and on the last
    /// going up.
    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        self.right_clicked.set(None);
        let Some(last) = self.rows.len().checked_sub(1) else {
            return;
        };
        let current = self.selected.and_then(|node| self.row_index(node));
        let next = match (current, down) {
            (Some(ix), true) if ix < last => ix + 1,
            (_, true) => 0,
            (Some(ix), false) => ix.checked_sub(1).unwrap_or(last),
            (None, false) => last,
        };
        self.selected = Some(self.rows[next].node);
        self.scroll_handle
            .scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn on_collapse_selected(
        &mut self,
        _: &CollapseSelectedGroup,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fold_selected(false, cx);
    }

    fn on_expand_selected(
        &mut self,
        _: &ExpandSelectedGroup,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fold_selected(true, cx);
    }

    /// Unfold the selected group, or fold it away; nothing when it already
    /// is, or a host is selected.
    fn fold_selected(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.right_clicked.set(None);
        let Some(HostNode::Group(group)) = self.selected else {
            return;
        };
        let shown = self
            .row_index(HostNode::Group(group))
            .is_some_and(|ix| self.rows[ix].expanded);
        if shown != expanded {
            self.toggle_group(group, cx);
        }
    }
}

/// The rows of `items` and of every open group among them, depth first.
fn flatten(items: &[TreeItem], depth: usize, folded: &HashSet<GroupId>, rows: &mut Vec<TreeRow>) {
    for item in items {
        let Some(node) = HostNode::parse(&item.id) else {
            continue;
        };
        let expanded =
            item.is_expanded() && node.group_id().is_none_or(|group| !folded.contains(&group));
        rows.push(TreeRow {
            node,
            label: item.label.clone(),
            depth,
            expanded,
        });
        if expanded {
            flatten(&item.children, depth + 1, folded, rows);
        }
    }
}

/// Every group a node sits in, at any depth: the folders that have to be
/// open for its row to show.
fn groups_above(store: &HostStore, node: HostNode) -> Vec<GroupId> {
    let parent = match node {
        HostNode::Group(id) => store.group(id).and_then(|group| group.parent),
        HostNode::Host(id) => store.host(id).and_then(|host| host.group),
    };
    parent
        .map(|id| {
            let mut chain = store.ancestor_groups(id);
            chain.push(id);
            chain
        })
        .unwrap_or_default()
}

impl EventEmitter<PanelEvent> for HostPanel {}

impl Focusable for HostPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for HostPanel {
    fn panel_name(&self) -> &'static str {
        "HostPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for HostPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(CatalogIcon::Server).small())
            .child(t!("host.panel.title"))
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        // A new host first: it is what the list is mostly added to.
        Some(vec![
            Button::new("new-host-panel")
                .icon(IconName::Plus)
                .tooltip(t!("host.menu.new_host"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NewHost), cx)),
            Button::new("new-group")
                .icon(Icon::new(CatalogIcon::FolderPlus))
                .tooltip(t!("host.menu.new_group"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NewGroup), cx)),
        ])
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for HostPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let details = self.details;
        // Plain snapshots for the row renderer: render callbacks must not
        // read entities.
        let (hosts, group_parents, group_counts) = {
            let store = self.store.read(cx);
            let hosts: HashMap<HostId, HostOf> = store
                .hosts()
                .iter()
                .map(|host| (host.id, HostOf::new(host, details)))
                .collect();
            let group_parents: Rc<HashMap<GroupId, Option<GroupId>>> = Rc::new(
                store
                    .groups()
                    .iter()
                    .map(|group| (group.id, group.parent))
                    .collect(),
            );
            let group_counts = group_host_counts(store.hosts(), &group_parents);
            (hosts, group_parents, group_counts)
        };
        let can_reorder = !self.searching();
        let tree = Rc::new(TreeSnapshot {
            rows: self.rows.clone(),
            hosts,
            group_counts,
            group_parents,
            selected: self.selected,
            can_reorder,
            right_clicked: self.right_clicked.clone(),
            drop_target: self.drop_target.clone(),
            row_tooltips: self.row_tooltips.clone(),
        });
        let heights = RowHeights::measure(window, cx);
        let sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            tree.rows
                .iter()
                .map(|row| size(px(0.), heights.of(tree.lines(row))))
                .collect(),
        );
        let clicked_blank = self.right_clicked.clone();
        let clicked_menu = self.right_clicked.clone();
        let drop_target_for_root = self.drop_target.clone();
        let drop_target_for_move = self.drop_target.clone();
        let rows = v_virtual_list(cx.entity(), "host-tree-rows", sizes.clone(), {
            let tree = tree.clone();
            move |_, range, _, cx| {
                range
                    .filter_map(|ix| {
                        let row = tree.rows.get(ix)?;
                        Some(render_row(row, &tree, sizes[ix].height, cx))
                    })
                    .collect::<Vec<_>>()
            }
        })
        .track_scroll(&self.scroll_handle);

        v_flex()
            .id("host-panel")
            .key_context(HOST_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_connect_selected))
            .on_action(cx.listener(Self::on_select_previous))
            .on_action(cx.listener(Self::on_select_next))
            .on_action(cx.listener(Self::on_collapse_selected))
            .on_action(cx.listener(Self::on_expand_selected))
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .id("host-search")
                                .small()
                                .cleanable(true)
                                .prefix(Icon::new(IconName::Search).small()),
                        ),
                    )
                    .child(
                        Button::new("toggle-host-details")
                            .ghost()
                            .small()
                            .icon(Icon::new(CatalogIcon::LayoutList))
                            .selected(details)
                            // A switch: assistive technology hears it as
                            // pressed.
                            .toggled(details)
                            .tooltip(t!("host.panel.show_details"))
                            .accessibility_label(t!("host.panel.show_details"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleHostDetails), cx)
                            }),
                    ),
            )
            .child(
                div()
                    .id("host-tree")
                    .test_support()
                    .role(Role::Tree)
                    .key_context(HOST_TREE_CONTEXT)
                    .track_focus(&self.tree_focus)
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .size_full()
                            .px_1()
                            .child(rows)
                            .vertical_scrollbar(&self.scroll_handle),
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
                        view.on_drag_move(move |_: &DragMoveEvent<DraggedHostNode>, _, _| {
                            drop_target_for_move.set(None);
                        })
                        .on_drop(
                            move |drag: &DraggedHostNode, window, cx| {
                                if drop_target_for_root.get().is_none() {
                                    window.dispatch_action(
                                        Box::new(MoveHostNode {
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
                    .context_menu({
                        let store = self.store.clone();
                        move |menu, _, cx| {
                            build_context_menu(clicked_menu.get(), store.read(cx), menu)
                        }
                    }),
            )
            .child(self.row_tooltips.overlay())
    }
}

/// A row that is not a host still has to reserve the badge's width, or the
/// labels of groups and hosts would not line up.
fn plain_mark(icon: Icon, cx: &App) -> AnyElement {
    h_flex()
        .flex_shrink_0()
        .size_5()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .child(icon.small())
        .into_any_element()
}

/// What the rows are drawn from, taken in `render`.
struct TreeSnapshot {
    rows: Rc<Vec<TreeRow>>,
    hosts: HashMap<HostId, HostOf>,
    group_counts: HashMap<GroupId, usize>,
    group_parents: Rc<HashMap<GroupId, Option<GroupId>>>,
    selected: Option<HostNode>,
    /// Not while searching: the rows shown are not all there is.
    can_reorder: bool,
    right_clicked: Rc<Cell<Option<HostNode>>>,
    drop_target: Rc<Cell<Option<(HostNode, NodeDrop)>>>,
    row_tooltips: RowTooltips<HostId>,
}

impl TreeSnapshot {
    /// How many lines `row` takes: a group's and a host's name, and under a
    /// host's the details it has, while they show.
    fn lines(&self, row: &TreeRow) -> usize {
        let details = row
            .node
            .host_id()
            .and_then(|id| self.hosts.get(&id))
            .map_or(0, |host| host.details.len());
        1 + details
    }
}

/// Count hosts in each group, including hosts in nested child groups.
fn group_host_counts(
    hosts: &[super::Host],
    parents: &HashMap<GroupId, Option<GroupId>>,
) -> HashMap<GroupId, usize> {
    let mut counts = HashMap::new();
    for host in hosts {
        let mut group = host.group;
        for _ in 0..parents.len() {
            let Some(id) = group else { break };
            *counts.entry(id).or_insert(0) += 1;
            group = parents.get(&id).copied().flatten();
        }
    }
    counts
}

/// What a host row is drawn from, taken in `render`.
struct HostOf {
    os: Option<HostOs>,
    protocol: ProtocolKind,
    address: SharedString,
    /// What the row's tooltip says: where the host logs in, and its notes.
    /// None while the row shows its details, which say it already.
    tooltip: Option<RowTooltip>,
    /// 显示主机详情's lines under the name: where the host is, then its
    /// notes if it has any. Empty while the details are hidden.
    details: Vec<SharedString>,
}

impl HostOf {
    fn new(host: &Host, details: bool) -> Self {
        let (tooltip, details) = if details {
            let lines = std::iter::once(location(host))
                .chain(note_line(&host.notes))
                .collect();
            (None, lines)
        } else {
            let tooltip = RowTooltip::new(host.endpoint()).note(host.notes.clone());
            (Some(tooltip), Vec::new())
        };
        Self {
            os: host.os,
            protocol: host.kind(),
            address: host.address.clone(),
            tooltip,
            details,
        }
    }
}

/// Where a host logs in, as its details say: `user@address:port` as
/// everywhere else (a Telnet host's without the user), with an IPv6 address
/// in brackets so the port stands apart from it; a serial host's device and
/// line settings.
fn location(host: &Host) -> SharedString {
    if !host.kind().has_address() {
        return host.endpoint().into();
    }
    let user = if host.is_ssh() && !host.user.is_empty() {
        format!("{}@", host.user)
    } else {
        String::new()
    };
    if host.address.contains(':') {
        format!("{user}[{}]:{}", host.address, host.port).into()
    } else {
        format!("{user}{}:{}", host.address, host.port).into()
    }
}

/// A host's notes on the one line its details give them, their lines one
/// after the other; `None` for notes that say nothing.
fn note_line(notes: &str) -> Option<SharedString> {
    let line = notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    (!line.is_empty()).then(|| line.into())
}

/// The ids of a host's detail lines, in their order.
const DETAIL_IDS: [&str; 2] = ["host-address", "host-note"];

/// How tall a row of one, two and three lines is (a group or a host alone,
/// a host with its address, and with its notes too), measured from rows
/// built as the tree builds its own: the list gives every row a slot of its
/// height, so the two must agree. Measured each frame, so they follow the
/// interface's font size.
#[derive(Clone, Copy)]
struct RowHeights([Pixels; 3]);

impl RowHeights {
    fn measure(window: &mut Window, cx: &mut App) -> Self {
        let available = size(AvailableSpace::MinContent, AvailableSpace::MinContent);
        let sample = SharedString::from("M");
        let mut heights = [px(0.); 3];
        for (details, height) in heights.iter_mut().enumerate() {
            let lines = (0..details)
                .map(|line| {
                    let id = ElementId::from(("host-row-sample-detail", line as u64));
                    (id, sample.clone())
                })
                .collect();
            let content = row_content(
                plain_mark(Icon::new(IconName::Folder), cx),
                sample.clone(),
                lines,
                cx,
            );
            *height = row_frame(("host-row-sample", details as u64), 0, cx)
                .child(content)
                .into_any_element()
                .layout_as_root(available, window, cx)
                .height;
        }
        Self(heights)
    }

    fn of(self, lines: usize) -> Pixels {
        self.0[lines.clamp(1, self.0.len()) - 1]
    }
}

/// The box a row is drawn in, a sample one's too.
fn row_frame(id: impl Into<ElementId>, depth: usize, cx: &App) -> ListItem {
    ListItem::new(id)
        .w_full()
        .px_2()
        .rounded(cx.theme().radius)
        .pl(rems(0.75 + depth as f32))
}

/// What a row shows: its mark and name, and under the name a host's
/// details, a line each, cut short where the sidebar ends.
fn row_content(
    mark: AnyElement,
    label: SharedString,
    details: Vec<(ElementId, SharedString)>,
    cx: &App,
) -> Div {
    v_flex()
        .min_w_0()
        .child(
            h_flex()
                .gap_2()
                .child(mark)
                .child(div().min_w_0().truncate().child(label)),
        )
        .children(details.into_iter().map(|(id, line)| {
            h_flex()
                .gap_2()
                // Under the name, clear of the mark.
                .child(div().flex_shrink_0().w_5())
                .child(
                    div()
                        .id(id)
                        .test_support()
                        .aria_label(line.clone())
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(line),
                )
        }))
}

fn render_row(
    row: &TreeRow,
    tree: &TreeSnapshot,
    height: Pixels,
    cx: &mut Context<HostPanel>,
) -> ListItem {
    let TreeSnapshot {
        hosts,
        group_counts,
        group_parents,
        selected,
        can_reorder,
        right_clicked,
        drop_target,
        row_tooltips,
        ..
    } = tree;
    let node = row.node;
    let selected = match right_clicked.get() {
        Some(right_clicked) => node == right_clicked,
        None => *selected == Some(node),
    };
    let (mark, row_id, details): (AnyElement, ElementId, Vec<(ElementId, SharedString)>) =
        match node {
            HostNode::Group(id) => (
                plain_mark(
                    Icon::new(if row.expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    }),
                    cx,
                ),
                ("group-row", id.0).into(),
                Vec::new(),
            ),
            HostNode::Host(id) => {
                let host = hosts.get(&id);
                let mark = HostMark::new(
                    ("host-os", id.0),
                    row.label.clone(),
                    host.map(|host| host.address.clone()).unwrap_or_default(),
                    host.and_then(|host| host.os),
                )
                .protocol(host.map(|host| host.protocol).unwrap_or_default())
                .small()
                .without_tooltip()
                .into_any_element();
                let details = host
                    .map(|host| {
                        DETAIL_IDS
                            .iter()
                            .zip(&host.details)
                            .map(|(name, line)| ((*name, id.0).into(), line.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                (mark, ("host-row", id.0).into(), details)
            }
        };
    let host_id = node.host_id();
    let label = row.label.clone();
    let expanded = row.expanded;

    row_frame(row_id, row.depth, cx)
        // Exactly the slot the list gives it.
        .h(height)
        .role(Role::TreeItem)
        .aria_label(label.clone())
        .aria_selected(selected)
        .when(node.group_id().is_some(), |item| {
            item.aria_expanded(expanded)
        })
        // Make selection stronger than hover against the sidebar background.
        .confirmed(selected)
        .when(selected, |item| {
            item.bg(crate::app::selected_row_color(cx.theme()))
        })
        .child(row_content(mark, label.clone(), details, cx))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| this.press_row(node, cx)),
        )
        .on_mouse_down(MouseButton::Right, {
            let right_clicked = right_clicked.clone();
            move |_, _, _| right_clicked.set(Some(node))
        })
        .when_some(node.group_id(), |item, id| {
            let count = group_counts.get(&id).copied().unwrap_or(0);
            item.suffix(move |_, cx| {
                div()
                    .id(("group-count", id.0))
                    .test_support()
                    .aria_label(tn!("host.count.hosts", count))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                    .child(count.to_string())
            })
        })
        .when_some(host_id, |item, id| {
            item.on_click(move |event: &ClickEvent, window, cx| {
                if event.click_count() == 2 {
                    window.dispatch_action(Box::new(ConnectHost(id)), cx);
                }
            })
        })
        .when_some(
            host_id.and_then(|id| Some((id, hosts.get(&id)?.tooltip.clone()?))),
            |item, (id, tooltip)| row_tooltips.row(id, tooltip).attach(item),
        )
        .when(*can_reorder, |item| {
            let drop_target_for_move = drop_target.clone();
            let drop_target_for_style = drop_target.clone();
            let drop_target_for_drop = drop_target.clone();
            let parents_for_move = group_parents.clone();
            item.on_drag(DraggedHostNode { node, label }, |drag, _, _, cx| {
                cx.new(|_| drag.clone())
            })
            .on_drag_move(move |event: &DragMoveEvent<DraggedHostNode>, _, cx| {
                if event.bounds.contains(&event.event.position) {
                    let source = event.drag(cx).node;
                    let relative =
                        (event.event.position.y - event.bounds.top()) / event.bounds.size.height;
                    let destination = match (source, node) {
                        (HostNode::Host(_), HostNode::Group(id)) => NodeDrop::Into(id),
                        (_, HostNode::Group(id)) if relative > 0.25 && relative < 0.75 => {
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
            .drag_over::<DraggedHostNode>(move |style, drag, _, cx| {
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
            .on_drop(move |drag: &DraggedHostNode, window, cx| {
                if let Some((target, destination)) = drop_target_for_drop.get()
                    && target == node
                    && drag.node != node
                {
                    window.dispatch_action(
                        Box::new(MoveHostNode {
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
}

#[derive(Clone)]
struct DraggedHostNode {
    node: HostNode,
    label: SharedString,
}

impl Render for DraggedHostNode {
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
    source: HostNode,
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
        HostNode::Host(_) => match destination {
            NodeDrop::Before(HostNode::Host(_))
            | NodeDrop::After(HostNode::Host(_))
            | NodeDrop::Root => true,
            NodeDrop::Into(group) => parents.contains_key(&group),
            _ => false,
        },
        HostNode::Group(id) => {
            let mut parent = match destination {
                NodeDrop::Before(HostNode::Group(target))
                | NodeDrop::After(HostNode::Group(target)) => match parents.get(&target) {
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

/// The menu of one host, wherever it is listed: the host tree and the
/// start page's recent hosts both build it here, so they cannot drift.
/// Built as it opens, from the host as it is then.
///
/// Only an SSH host has SFTP, and a serial host has no address to copy.
pub fn host_menu(menu: PopupMenu, host: &Host) -> PopupMenu {
    let id = host.id;
    let ssh = host.is_ssh();
    let has_address = host.kind().has_address();
    // Named after what the address is, as in the tabs' menus.
    let copy_address = if host.address_is_ip() {
        t!("host.menu.copy_ip")
    } else {
        t!("host.menu.copy_host_name")
    };
    menu.menu_with_icon(
        t!("host.menu.connect"),
        Icon::new(CatalogIcon::Plug),
        Box::new(ConnectHost(id)),
    )
    .when(ssh, |menu| {
        menu.menu_with_icon(
            t!("host.menu.open_sftp"),
            Icon::new(CatalogIcon::FolderTree),
            Box::new(OpenExplorer(id)),
        )
    })
    .separator()
    .menu_with_icon(
        t!("host.menu.edit_host"),
        Icon::new(CatalogIcon::Pencil),
        Box::new(EditHost(id)),
    )
    .menu_with_icon(
        t!("host.menu.duplicate"),
        Icon::new(CatalogIcon::CopyPlus),
        Box::new(DuplicateHost(id)),
    )
    .separator()
    .when(has_address, |menu| {
        menu.menu_with_icon(
            copy_address,
            Icon::new(CatalogIcon::ClipboardCopy),
            Box::new(CopyHostAddress(id)),
        )
    })
    .menu_with_icon(
        t!("host.menu.copy_id"),
        Icon::new(CatalogIcon::ClipboardCopy),
        Box::new(CopyHostId(id)),
    )
    .separator()
    .menu_with_icon(
        t!("common.delete"),
        Icon::new(CatalogIcon::Trash),
        Box::new(DeleteHost(id)),
    )
}

fn build_context_menu(node: Option<HostNode>, store: &HostStore, menu: PopupMenu) -> PopupMenu {
    match node {
        Some(HostNode::Host(id)) => match store.host(id) {
            Some(host) => host_menu(menu, host),
            None => menu,
        },
        Some(HostNode::Group(id)) => menu
            .menu_with_icon(
                t!("host.menu.connect_group"),
                Icon::new(CatalogIcon::Plug),
                Box::new(ConnectGroup(id)),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.new_host"),
                Icon::new(IconName::Plus),
                Box::new(NewHostInGroup(id)),
            )
            .menu_with_icon(
                t!("host.menu.new_subgroup"),
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewChildGroup(id)),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.expand_all"),
                Icon::new(IconName::FolderOpen),
                Box::new(ExpandAllGroups),
            )
            .menu_with_icon(
                t!("host.menu.collapse_all"),
                Icon::new(IconName::Folder),
                Box::new(CollapseAllGroups),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.rename_group"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameGroup(id)),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.delete_group"),
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteGroup(id)),
            ),
        None => import_menu_items(
            menu.menu_with_icon(
                t!("host.menu.new_host"),
                Icon::new(IconName::Plus),
                Box::new(NewHost),
            )
            .menu_with_icon(
                t!("host.menu.new_group"),
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewGroup),
            )
            .separator(),
        ),
    }
}

/// The import commands, one per source: under the blank space's menu and
/// in the dock's 「…」 menu while the host list shows.
pub fn import_menu_items(menu: PopupMenu) -> PopupMenu {
    menu.menu_with_icon(
        t!("host.menu.import_ssh_config"),
        Icon::new(CatalogIcon::Import),
        Box::new(ImportSshConfig),
    )
    .menu_with_icon(
        t!("host.menu.import_known_hosts"),
        Icon::new(CatalogIcon::Import),
        Box::new(ImportKnownHosts),
    )
}

#[cfg(test)]
mod tests {
    use super::{Host, HostStore, location, note_line};
    use crate::host::{AuthKind, HostDraft};

    fn host_at(address: &str, port: u16) -> Host {
        let mut store = HostStore::empty();
        let id = store.insert_unnotified(HostDraft::new(
            "host",
            address,
            port,
            "root",
            AuthKind::NoPassword,
            None,
        ));
        store.host(id).unwrap().clone()
    }

    #[test]
    fn the_details_give_the_user_address_and_port() {
        assert_eq!(location(&host_at("10.0.1.12", 22)), "root@10.0.1.12:22");
        assert_eq!(
            location(&host_at("db.example.test", 2222)),
            "root@db.example.test:2222"
        );
        // The port stands apart from an IPv6 address.
        assert_eq!(location(&host_at("fe80::1", 22)), "root@[fe80::1]:22");
    }

    #[test]
    fn notes_go_on_one_line_without_their_blank_lines() {
        assert_eq!(
            note_line("机房 A\n\n  负责人：张三  \n").as_deref(),
            Some("机房 A · 负责人：张三")
        );
        assert_eq!(note_line("只有一行").as_deref(), Some("只有一行"));
        assert_eq!(note_line(""), None);
        assert_eq!(note_line(" \n\t\n"), None);
    }
}
