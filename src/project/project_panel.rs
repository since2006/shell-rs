use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use crate::shared::RightClickMenu as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::ListItem,
    menu::PopupMenu,
    scroll::ScrollableElement as _,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::outline::{ProjectRow, project_rows};
use crate::app::{
    CatalogIcon, CloseSavedTerminal, DeleteSavedTerminal, DeleteTerminalGroup, MoveTerminalNode,
    NewSavedTerminal, NewTerminalGroup, OpenSavedTerminal, RenameSavedTerminal,
    RenameTerminalGroup, ToggleTerminalGroup,
};
use crate::host::{HostStore, SavedTerminalId, TerminalDrop, TerminalGroupId, TerminalNode};
use crate::i18n::t;

/// 项目: the right sidebar's tool beside local terminals. Local terminals
/// kept like hosts, in groups or at the top below them: a group's 「+」
/// opens a new one in it, and a click on one brings its tab to the front,
/// or opens it again in the folder it was last in. Rows are dragged into
/// order as the host tree's are. Its 新建 buttons are on the sidebar's
/// title bar, as the host list's are.
///
/// A group's heading is one line and a terminal two, its folder under its
/// name: the list measures each row, unlike a tree, whose rows are all as
/// tall as the first.
///
/// The groups and terminals are in the host store, which this panel
/// observes; which of them are open, the workspace tells it.
pub struct ProjectPanel {
    store: Entity<HostStore>,
    list: ListState,
    /// The rows the list was last told of.
    rows: Vec<ProjectRow>,
    /// The terminals with their tab open, and the one in front.
    open: HashSet<SavedTerminalId>,
    front: Option<SavedTerminalId>,
    /// What the shells or programs of open terminals title them, shown for
    /// the names they were given, as their tabs show it. The title a
    /// terminal had when its tab last closed shows until then.
    titles: HashMap<SavedTerminalId, SharedString>,
    /// Which row the last right click landed on, `None` for the blank space
    /// below the rows. Shared with the row renderer and the menu builder.
    right_clicked: Rc<Cell<Option<TerminalNode>>>,
    /// Where the row being dragged would land, as the rows under it work
    /// out.
    drop_target: Rc<Cell<Option<(TerminalNode, TerminalDrop)>>>,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ProjectPanel {
    pub fn new(store: Entity<HostStore>, dispatch: FocusHandle, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe(&store, |_, _, cx| cx.notify())];
        Self {
            store,
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            rows: Vec::new(),
            open: HashSet::new(),
            front: None,
            titles: HashMap::new(),
            right_clicked: Rc::new(Cell::new(None)),
            drop_target: Rc::new(Cell::new(None)),
            dispatch,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// The terminals with their tab open, and the one in front, `None` when
    /// the tab in front is none of them.
    pub fn set_open(
        &mut self,
        open: HashSet<SavedTerminalId>,
        front: Option<SavedTerminalId>,
        cx: &mut Context<Self>,
    ) {
        if self.open == open && self.front == front {
            return;
        }
        self.titles.retain(|id, _| open.contains(id));
        self.open = open;
        if self.front != front {
            self.front = front;
            // The one in front scrolled to, unless its group is folded.
            let rows = self.current_rows(cx);
            self.sync_list(&rows);
            if let Some(index) = rows.iter().position(
                |row| matches!(row, ProjectRow::Terminal { id, .. } if Some(*id) == front),
            ) {
                self.list.scroll_to_reveal_item(index);
            }
        }
        cx.notify();
    }

    /// What an open terminal's shell or a program titles it now, `None`
    /// when nothing does.
    pub fn set_title(
        &mut self,
        id: SavedTerminalId,
        title: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.titles.get(&id) == title.as_ref() {
            return;
        }
        match title {
            Some(title) => self.titles.insert(id, title),
            None => self.titles.remove(&id),
        };
        cx.notify();
    }

    fn current_rows(&self, cx: &App) -> Vec<ProjectRow> {
        let store = self.store.read(cx);
        project_rows(store.terminal_groups(), store.saved_terminals())
    }

    /// Tell the list about `rows` when they are not the ones it has.
    fn sync_list(&mut self, rows: &[ProjectRow]) {
        if self.rows != rows {
            self.list.splice(0..self.rows.len(), rows.len());
            self.rows = rows.to_vec();
        }
    }

    /// What shows before there is anything: what it is for, and the ways
    /// to start.
    fn render_empty(&self, cx: &App) -> AnyElement {
        let (terminal, group) = (self.dispatch.clone(), self.dispatch.clone());
        v_flex()
            .id("projects-empty")
            .test_support()
            .aria_label(t!("project.panel.empty"))
            .items_center()
            .gap_2()
            .px_4()
            .py_8()
            .child(div().text_sm().child(t!("project.panel.empty")))
            .child(
                div()
                    .text_xs()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("project.panel.empty_description")),
            )
            .child(
                h_flex()
                    .mt_2()
                    .gap_2()
                    .child(
                        Button::new("projects-empty-new-terminal")
                            .small()
                            .primary()
                            .icon(Icon::new(IconName::Plus))
                            .label(t!("project.menu.new_terminal"))
                            .on_click(move |_, window, cx| {
                                terminal.dispatch_action(&NewSavedTerminal(None), window, cx)
                            }),
                    )
                    .child(
                        Button::new("projects-empty-new-group")
                            .small()
                            .outline()
                            .icon(Icon::new(CatalogIcon::FolderPlus))
                            .label(t!("project.menu.new_group"))
                            .on_click(move |_, window, cx| {
                                group.dispatch_action(&NewTerminalGroup, window, cx)
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Focusable for ProjectPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProjectPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.current_rows(cx);
        self.sync_list(&rows);
        // Plain snapshots for the row renderer: render callbacks must not
        // read entities.
        let store = self.store.read(cx);
        let home = dirs::home_dir();
        let snapshot = Rc::new(Rows {
            rows,
            groups: store
                .terminal_groups()
                .iter()
                .map(|group| (group.id, group.name.clone()))
                .collect(),
            terminals: store
                .saved_terminals()
                .iter()
                .map(|terminal| {
                    let (short, directory) = match terminal.directory.as_deref() {
                        Some(directory) => (
                            short_path(directory, home.as_deref()).into(),
                            Some(directory.to_owned().into()),
                        ),
                        // The home folder, where it starts before it says.
                        None => ("~".into(), None),
                    };
                    // A name given by hand shows over the titles.
                    let title = self
                        .titles
                        .get(&terminal.id)
                        .or(terminal.title.as_ref())
                        .filter(|_| !terminal.renamed);
                    (
                        terminal.id,
                        TerminalOf {
                            name: title.unwrap_or(&terminal.name).clone(),
                            short,
                            directory,
                        },
                    )
                })
                .collect(),
            open: self.open.clone(),
            front: self.front,
            right_clicked: self.right_clicked.clone(),
            drop_target: self.drop_target.clone(),
            dispatch: self.dispatch.clone(),
        });
        let nothing = snapshot.rows.is_empty();
        let menu_rows = snapshot.clone();
        let (clicked_blank, drop_target, drop_target_for_top) = (
            self.right_clicked.clone(),
            self.drop_target.clone(),
            self.drop_target.clone(),
        );
        let dispatch = self.dispatch.clone();

        v_flex()
            .id("projects")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(
                div()
                    .id("project-tree")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .map(|tree| {
                        if nothing {
                            tree.child(self.render_empty(cx))
                        } else {
                            let rows = snapshot.clone();
                            tree.child(
                                gpui_kit::list(self.list.clone(), move |index, _, cx| {
                                    div()
                                        .px_1()
                                        .when(index == 0, |row| row.pt_1())
                                        .child(render_row(rows.rows[index], &rows, cx))
                                        .into_any_element()
                                })
                                .size_full(),
                            )
                        }
                    })
                    // The scrollbar goes on the box, which does not scroll;
                    // the menu hangs off the box too, not off the rows,
                    // which are drawn after layout (see the host tree).
                    .vertical_scrollbar(&self.list)
                    .capture_any_mouse_down(move |event, _, _| {
                        if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                            clicked_blank.set(None);
                        }
                    })
                    // Off every row, a terminal dragged lands last at the
                    // top: the rows under it say otherwise.
                    .on_drag_move(move |_: &DragMoveEvent<DraggedTerminalNode>, _, _| {
                        drop_target.set(None);
                    })
                    .on_drop(move |drag: &DraggedTerminalNode, window, cx| {
                        if drop_target_for_top.get().is_none()
                            && matches!(drag.node, TerminalNode::Terminal(_))
                        {
                            dispatch.dispatch_action(
                                &MoveTerminalNode {
                                    source: drag.node,
                                    destination: TerminalDrop::Top,
                                },
                                window,
                                cx,
                            );
                        }
                        drop_target_for_top.set(None);
                    })
                    // A right click off every row still focuses the panel,
                    // so the menu's commands have somewhere to be
                    // dispatched from.
                    .on_mouse_down(MouseButton::Right, {
                        let focus = self.focus_handle.clone();
                        move |_, window, cx| focus.focus(window, cx)
                    })
                    .context_menu(move |menu, _, _| {
                        build_context_menu(menu_rows.right_clicked.get(), &menu_rows.open, menu)
                    }),
            )
    }
}

/// What the rows are drawn from, taken in `render`.
struct Rows {
    rows: Vec<ProjectRow>,
    groups: HashMap<TerminalGroupId, SharedString>,
    terminals: HashMap<SavedTerminalId, TerminalOf>,
    open: HashSet<SavedTerminalId>,
    front: Option<SavedTerminalId>,
    right_clicked: Rc<Cell<Option<TerminalNode>>>,
    drop_target: Rc<Cell<Option<(TerminalNode, TerminalDrop)>>>,
    dispatch: FocusHandle,
}

/// A terminal's row as `render` took it.
struct TerminalOf {
    name: SharedString,
    /// The folder it was last in, the home folder written as `~`.
    short: SharedString,
    /// The whole path, `None` for the home folder before the shell said.
    directory: Option<SharedString>,
}

/// `directory` with the home folder at its start written as `~`.
fn short_path(directory: &str, home: Option<&std::path::Path>) -> String {
    let Some(home) = home.map(|home| home.display().to_string()) else {
        return directory.to_owned();
    };
    match directory.strip_prefix(&home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with(['/', '\\']) => format!("~{rest}"),
        _ => directory.to_owned(),
    }
}

fn render_row(row: ProjectRow, rows: &Rows, cx: &mut App) -> ListItem {
    let muted = cx.theme().muted_foreground;
    let node = match row {
        ProjectRow::Group { id, .. } => TerminalNode::Group(id),
        ProjectRow::Terminal { id, .. } => TerminalNode::Terminal(id),
    };
    let selected = match rows.right_clicked.get() {
        Some(clicked) => clicked == node,
        None => matches!(row, ProjectRow::Terminal { id, .. } if rows.front == Some(id)),
    };
    let item = ListItem::new(match row {
        ProjectRow::Group { id, .. } => ElementId::from(("terminal-group-row", id.0)),
        ProjectRow::Terminal { id, .. } => ElementId::from(("saved-terminal-row", id.0)),
    })
    .w_full()
    .px_2()
    .rounded(cx.theme().radius)
    // Make selection stronger than hover against the sidebar background.
    .confirmed(selected)
    .when(selected, |item| {
        item.bg(crate::app::selected_row_color(cx.theme()))
    })
    .on_mouse_down(MouseButton::Right, {
        let right_clicked = rows.right_clicked.clone();
        move |_, _, _| right_clicked.set(Some(node))
    });
    let label = match row {
        ProjectRow::Group { id, .. } => rows.groups.get(&id).cloned().unwrap_or_default(),
        ProjectRow::Terminal { id, .. } => rows
            .terminals
            .get(&id)
            .map(|terminal| terminal.name.clone())
            .unwrap_or_default(),
    };
    let item = draggable(item, node, label.clone(), rows);
    match row {
        ProjectRow::Group { id, folded } => {
            let (toggle, new_terminal) = (rows.dispatch.clone(), rows.dispatch.clone());
            item.h_7()
                .pl(rems(0.75))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(if folded {
                                IconName::Folder
                            } else {
                                IconName::FolderOpen
                            })
                            .small()
                            .text_color(muted),
                        )
                        .child(div().flex_1().w_0().truncate().child(label)),
                )
                .suffix(move |_, _| {
                    row_button(
                        Button::new(("new-saved-terminal", id.0))
                            .icon(Icon::new(IconName::Plus))
                            .tooltip(t!("project.menu.new_terminal"))
                            .accessibility_label(t!("project.menu.new_terminal")),
                        new_terminal.clone(),
                        NewSavedTerminal(Some(id)),
                    )
                })
                .on_click(move |_, window, cx| {
                    toggle.dispatch_action(&ToggleTerminalGroup(id), window, cx)
                })
        }
        ProjectRow::Terminal { id, in_group } => {
            let open = rows.open.contains(&id);
            let (short, directory) = rows
                .terminals
                .get(&id)
                .map(|terminal| (terminal.short.clone(), terminal.directory.clone()))
                .unwrap_or_else(|| ("~".into(), None));
            let (open_it, delete) = (rows.dispatch.clone(), rows.dispatch.clone());
            let hover = SharedString::from(format!("saved-terminal:{}", id.0));
            item.h(rems(2.75))
                .pl(rems(if in_group { 1.75 } else { 0.75 }))
                .group(hover.clone())
                .aria_label(label.clone())
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            // Coloured while its tab is open, as a host's tab
                            // is while connected.
                            div()
                                .id(("saved-terminal-mark", id.0))
                                .test_support()
                                .aria_label(if open {
                                    t!("project.panel.open")
                                } else {
                                    t!("project.panel.closed")
                                })
                                .flex_shrink_0()
                                .child(
                                    Icon::new(CatalogIcon::SquareTerminal)
                                        .small()
                                        .text_color(if open { cx.theme().success } else { muted }),
                                ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .w_0()
                                .child(div().truncate().child(label))
                                .child(div().truncate().text_xs().text_color(muted).child(short)),
                        ),
                )
                .when_some(directory, |item, directory| {
                    item.tooltip(move |window, cx| {
                        Tooltip::new(directory.clone()).build(window, cx)
                    })
                })
                // 删除 shows while the pointer is on the row; the menu has
                // it too, for the keyboard.
                .suffix(move |_, _| {
                    div()
                        .invisible()
                        .group_hover(hover.clone(), |style| style.visible())
                        .child(row_button(
                            Button::new(("delete-saved-terminal", id.0))
                                .icon(Icon::new(CatalogIcon::Trash))
                                .tooltip(t!("project.menu.delete"))
                                .accessibility_label(t!("project.menu.delete")),
                            delete.clone(),
                            DeleteSavedTerminal(id),
                        ))
                })
                .on_click(move |_, window, cx| {
                    open_it.dispatch_action(&OpenSavedTerminal(id), window, cx)
                })
        }
    }
}

/// A small button at a row's end, dispatching `action` on the workspace. A
/// press on it is not the row's, to drag, nor its click the row's.
fn row_button(
    button: Button,
    dispatch: FocusHandle,
    action: impl Action + Clone,
) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(button.ghost().xsmall().on_click(move |_, window, cx| {
            cx.stop_propagation();
            dispatch.dispatch_action(&action, window, cx)
        }))
}

/// Let a row be dragged, and dropped on, as the host tree's rows are: a
/// group among the groups, a terminal among the terminals or onto a group,
/// last in it.
fn draggable(item: ListItem, node: TerminalNode, label: SharedString, rows: &Rows) -> ListItem {
    let (drop_target_for_move, drop_target_for_style, drop_target_for_drop) = (
        rows.drop_target.clone(),
        rows.drop_target.clone(),
        rows.drop_target.clone(),
    );
    let dispatch = rows.dispatch.clone();
    item.on_drag(DraggedTerminalNode { node, label }, |drag, _, _, cx| {
        cx.new(|_| drag.clone())
    })
    .on_drag_move(move |event: &DragMoveEvent<DraggedTerminalNode>, _, cx| {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let source = event.drag(cx).node;
        if source == node {
            return;
        }
        let upper = (event.event.position.y - event.bounds.top()) / event.bounds.size.height < 0.5;
        let destination = match (source, node) {
            (TerminalNode::Terminal(_), TerminalNode::Group(group)) => TerminalDrop::Into(group),
            (TerminalNode::Group(_), TerminalNode::Group(_))
            | (TerminalNode::Terminal(_), TerminalNode::Terminal(_)) => {
                if upper {
                    TerminalDrop::Before(node)
                } else {
                    TerminalDrop::After(node)
                }
            }
            (TerminalNode::Group(_), TerminalNode::Terminal(_)) => return,
        };
        drop_target_for_move.set(Some((node, destination)));
    })
    .drag_over::<DraggedTerminalNode>(move |style, drag, _, cx| {
        if drag.node == node {
            return style;
        }
        match drop_target_for_style.get() {
            Some((target, TerminalDrop::Before(_))) if target == node => {
                style.border_t_1().border_color(cx.theme().primary)
            }
            Some((target, TerminalDrop::After(_))) if target == node => {
                style.border_b_1().border_color(cx.theme().primary)
            }
            Some((target, TerminalDrop::Into(_))) if target == node => style
                .bg(cx.theme().accent)
                .border_1()
                .border_color(cx.theme().primary),
            _ => style,
        }
    })
    .on_drop(move |drag: &DraggedTerminalNode, window, cx| {
        if let Some((target, destination)) = drop_target_for_drop.get()
            && target == node
            && drag.node != node
        {
            dispatch.dispatch_action(
                &MoveTerminalNode {
                    source: drag.node,
                    destination,
                },
                window,
                cx,
            );
        }
        drop_target_for_drop.set(None);
        cx.stop_propagation();
    })
}

/// What follows the pointer while a row is dragged: its name.
#[derive(Clone)]
struct DraggedTerminalNode {
    node: TerminalNode,
    label: SharedString,
}

impl Render for DraggedTerminalNode {
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

/// The list's menu, for what a right click landed on: a group, a terminal,
/// or nothing.
fn build_context_menu(
    hit: Option<TerminalNode>,
    open: &HashSet<SavedTerminalId>,
    menu: PopupMenu,
) -> PopupMenu {
    match hit {
        None => menu
            .menu_with_icon(
                t!("project.menu.new_terminal"),
                Icon::new(IconName::Plus),
                Box::new(NewSavedTerminal(None)),
            )
            .menu_with_icon(
                t!("project.menu.new_group"),
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewTerminalGroup),
            ),
        Some(TerminalNode::Group(id)) => menu
            .menu_with_icon(
                t!("project.menu.new_terminal"),
                Icon::new(IconName::Plus),
                Box::new(NewSavedTerminal(Some(id))),
            )
            .menu_with_icon(
                t!("project.menu.rename"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameTerminalGroup(id)),
            )
            .separator()
            .menu_with_icon(
                t!("project.menu.delete"),
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteTerminalGroup(id)),
            ),
        Some(TerminalNode::Terminal(id)) => {
            let menu = if open.contains(&id) {
                menu.menu_with_icon(
                    t!("project.menu.close"),
                    Icon::new(IconName::Close),
                    Box::new(CloseSavedTerminal(id)),
                )
            } else {
                menu.menu_with_icon(
                    t!("project.menu.open"),
                    Icon::new(CatalogIcon::SquareTerminal),
                    Box::new(OpenSavedTerminal(id)),
                )
            };
            menu.menu_with_icon(
                t!("project.menu.rename"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameSavedTerminal(id)),
            )
            .separator()
            .menu_with_icon(
                t!("project.menu.delete"),
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteSavedTerminal(id)),
            )
        }
    }
}

// Unix paths: Windows has its own home folder.
#[cfg(all(test, unix))]
mod tests {
    // Imported one by one: `use gpui_kit::*` above would shadow `#[test]`.
    use std::path::Path;

    use super::short_path;

    #[test]
    fn the_home_folder_is_written_as_a_tilde() {
        let home = Some(Path::new("/Users/me"));
        assert_eq!(short_path("/Users/me", home), "~");
        assert_eq!(short_path("/Users/me/code/app", home), "~/code/app");
        assert_eq!(short_path("/Users/meg/app", home), "/Users/meg/app");
        assert_eq!(short_path("/srv/app", None), "/srv/app");
    }
}
