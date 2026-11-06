use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::model::{SnippetRow, rows};
use crate::app::{
    CatalogIcon, CopyCommand, DeleteSnippet, DeleteSnippetCategory, EditSnippet, EnterCommand,
    MoveSnippetNode, NewSnippet, NewSnippetCategory, NewSnippetIn, NewSnippetSubcategory,
    RenameSnippetCategory, ToggleSnippetCategory,
};
use crate::host::{HostStore, SnippetCategoryId, SnippetDrop, SnippetId, SnippetNode};
use crate::i18n::{UiLocale, t, tn};
use crate::shared::{RowTooltip, RowTooltips, one_line};

/// 命令片段: the commands kept for every host, in categories that nest as
/// host groups do. A click on one puts it on the input line of the terminal
/// in front, an SSH terminal or a local one, to edit, or runs it there when
/// the snippet says so.
///
/// One line a row, the name alone: the command shows beside the row while
/// the pointer is on it. Rows are dragged into order and into categories as
/// the host tree's are, while not searching.
///
/// The snippets are in the host store, so they are the same whichever
/// terminal is in front, and nothing is read from the host. Which
/// categories are folded and the search stay as they are from one terminal
/// to the next; the folds are not saved.
pub struct SnippetPanel {
    store: Entity<HostStore>,
    search: Entity<InputState>,
    query: String,
    folded: HashSet<SnippetCategoryId>,
    list: ListState,
    /// The lines the list was last told about.
    rows: Vec<SnippetRow>,
    /// The search changed: back to the top.
    back_to_top: bool,
    /// What the store held when it last changed: what it gains since, a
    /// category or snippet just made, is unfolded to and scrolled to.
    known: HashSet<SnippetNode>,
    /// The row to scroll to once the list has it.
    reveal: Option<SnippetNode>,
    /// What a right click landed on, for the list's menu; `None` off every
    /// row. Shared with the row renderer and the menu builder.
    right_clicked: Rc<Cell<Option<SnippetNode>>>,
    /// Where the row being dragged would land, as the rows under it work
    /// out.
    drop_target: Rc<Cell<Option<(SnippetNode, SnippetDrop)>>>,
    /// The snippets' rows' tooltips: their commands.
    row_tooltips: RowTooltips<SnippetId>,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SnippetPanel {
    pub fn new(
        store: Entity<HostStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("snippets.panel.search"))
                .clean_on_escape()
        });
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.on_store_changed(cx)),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.search.update(cx, |search, cx| {
                    search.set_placeholder(t!("snippets.panel.search"), window, cx)
                });
            }),
            cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = input.read(cx).value().to_string();
                    this.back_to_top = true;
                    cx.notify();
                }
            }),
        ];
        let known = nodes(store.read(cx));
        Self {
            store,
            search,
            query: String::new(),
            folded: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            rows: Vec::new(),
            back_to_top: false,
            known,
            reveal: None,
            right_clicked: Rc::new(Cell::new(None)),
            drop_target: Rc::new(Cell::new(None)),
            row_tooltips: RowTooltips::new("snippet-tooltip", cx).on_the_left(),
            dispatch,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Fold a category away, or unfold it.
    pub fn toggle_category(&mut self, id: SnippetCategoryId, cx: &mut Context<Self>) {
        if !self.folded.remove(&id) {
            self.folded.insert(id);
        }
        cx.notify();
    }

    /// Unfold the categories `node` is in, and scroll to it: one just
    /// made, or dropped into a folded category.
    pub fn reveal(&mut self, node: SnippetNode, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let category = match node {
            SnippetNode::Category(id) => store.snippet_category(id).and_then(|c| c.parent),
            SnippetNode::Snippet(id) => store.snippet(id).and_then(|s| s.category),
        };
        if let Some(category) = category {
            self.folded.remove(&category);
            for above in store.ancestor_snippet_categories(category) {
                self.folded.remove(&above);
            }
        }
        self.reveal = Some(node);
        cx.notify();
    }

    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let now = nodes(self.store.read(cx));
        let made = now
            .difference(&self.known)
            .copied()
            .max_by_key(|node| match node {
                // The newest, a category before a snippet made with it.
                SnippetNode::Category(id) => (1, id.0),
                SnippetNode::Snippet(id) => (0, id.0),
            });
        self.folded
            .retain(|id| now.contains(&SnippetNode::Category(*id)));
        self.known = now;
        match made {
            Some(node) => self.reveal(node, cx),
            None => cx.notify(),
        }
    }

    /// Tell the list about `rows` when they are not the ones it has; back
    /// at the top when the search changed, or at the row to reveal.
    fn sync_list(&mut self, rows: &[SnippetRow]) {
        if std::mem::take(&mut self.back_to_top) {
            self.list.reset(rows.len());
        } else if self.rows != rows {
            self.list.splice(0..self.rows.len(), rows.len());
        }
        self.rows = rows.to_vec();
        if let Some(node) = self.reveal.take()
            && let Some(index) = rows.iter().position(|row| node_of(*row) == node)
        {
            self.list.scroll_to_reveal_item(index);
        }
    }

    fn render_header(&self, snippets: usize, categories: usize, cx: &App) -> impl IntoElement {
        let summary: SharedString = if categories == 0 {
            tn!("snippets.panel.count", snippets)
        } else {
            format!(
                "{} · {}",
                tn!("snippets.panel.count", snippets),
                tn!("snippets.panel.categories", categories)
            )
            .into()
        };
        let (new_category, new_snippet) = (self.dispatch.clone(), self.dispatch.clone());
        v_flex()
            .flex_shrink_0()
            .px_3()
            .pt_3()
            .pb_2()
            .gap_2()
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .id("snippets-summary")
                            .test_support()
                            .aria_label(summary.clone())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(summary),
                    )
                    .child(
                        Button::new("snippets-new")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::Plus))
                            .tooltip(t!("snippets.panel.new_snippet"))
                            .accessibility_label(t!("snippets.panel.new_snippet"))
                            .on_click(move |_, window, cx| {
                                new_snippet.dispatch_action(&NewSnippet, window, cx)
                            }),
                    )
                    .child(
                        Button::new("snippets-new-category")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CatalogIcon::FolderPlus))
                            .tooltip(t!("snippets.panel.new_category"))
                            .accessibility_label(t!("snippets.panel.new_category"))
                            .on_click(move |_, window, cx| {
                                new_category.dispatch_action(&NewSnippetCategory, window, cx)
                            }),
                    ),
            )
            .child(
                Input::new(&self.search)
                    .id("snippets-search")
                    .small()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
    }

    /// What shows before there is any snippet: what they are for, and the
    /// way to make one.
    fn render_empty(&self, cx: &App) -> AnyElement {
        let dispatch = self.dispatch.clone();
        v_flex()
            .id("snippets-empty")
            .test_support()
            .aria_label(t!("snippets.panel.empty"))
            .items_center()
            .gap_2()
            .px_4()
            .py_8()
            .child(div().text_sm().child(t!("snippets.panel.empty")))
            .child(
                div()
                    .text_xs()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("snippets.panel.empty_description")),
            )
            .child(
                Button::new("snippets-empty-new")
                    .small()
                    .primary()
                    .mt_2()
                    .icon(Icon::new(IconName::Plus))
                    .label(t!("snippets.panel.new_snippet"))
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_action(&NewSnippet, window, cx)
                    }),
            )
            .into_any_element()
    }
}

/// Every category and snippet the store holds, as rows.
fn nodes(store: &HostStore) -> HashSet<SnippetNode> {
    store
        .snippet_categories()
        .iter()
        .map(|category| SnippetNode::Category(category.id))
        .chain(
            store
                .snippets()
                .iter()
                .map(|snippet| SnippetNode::Snippet(snippet.id)),
        )
        .collect()
}

fn node_of(row: SnippetRow) -> SnippetNode {
    match row {
        SnippetRow::Category { id, .. } => SnippetNode::Category(id),
        SnippetRow::Snippet { id, .. } => SnippetNode::Snippet(id),
    }
}

impl Focusable for SnippetPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SnippetPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let rows = rows(
            store.snippet_categories(),
            store.snippets(),
            &self.query,
            &self.folded,
        );
        // Plain snapshots for the row renderer: render callbacks must not
        // read entities.
        let snapshot = Rc::new(Rows {
            rows: rows.clone(),
            categories: store
                .snippet_categories()
                .iter()
                .map(|category| (category.id, (category.name.clone(), category.parent)))
                .collect(),
            snippets: store
                .snippets()
                .iter()
                .map(|snippet| {
                    let of = SnippetOf {
                        name: snippet.name.clone(),
                        command: snippet.command.clone(),
                        runs: snippet.run_on_click,
                    };
                    (snippet.id, of)
                })
                .collect(),
            can_reorder: self.query.trim().is_empty(),
            right_clicked: self.right_clicked.clone(),
            drop_target: self.drop_target.clone(),
            row_tooltips: self.row_tooltips.clone(),
            focus: self.focus_handle.clone(),
            dispatch: self.dispatch.clone(),
        });
        let (snippets, categories) = (snapshot.snippets.len(), snapshot.categories.len());
        let nothing = snippets == 0 && categories == 0;
        self.sync_list(&rows);
        let header = self.render_header(snippets, categories, cx);

        let clicked_blank = self.right_clicked.clone();
        let menu_hit = self.right_clicked.clone();
        let (drop_target, drop_target_for_top) =
            (self.drop_target.clone(), self.drop_target.clone());
        let dispatch = self.dispatch.clone();
        let can_reorder = snapshot.can_reorder;
        // The scrollbar goes on the list's box, which does not scroll: on
        // the scrolled lines it would scroll away with them. The menu hangs
        // off the box too, not off the lines, which are drawn after layout
        // (see the host tree).
        let list = div()
            .id("snippets-list")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .map(|list| {
                if nothing {
                    list.child(self.render_empty(cx))
                } else if rows.is_empty() {
                    let empty = t!("snippets.panel.no_match");
                    list.child(
                        div()
                            .id("snippets-no-match")
                            .test_support()
                            .aria_label(empty.clone())
                            .py_8()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty),
                    )
                } else {
                    let rows = snapshot.clone();
                    list.child(
                        gpui_kit::list(self.list.clone(), move |index, _, cx| {
                            let last = index + 1 == rows.rows.len();
                            div()
                                .px_1()
                                .when(index == 0, |row| row.pt_1())
                                // The last one clears the bottom.
                                .when(last, |row| row.pb_2())
                                .child(render_row(rows.rows[index], &rows, cx))
                                .into_any_element()
                        })
                        .size_full(),
                    )
                }
            })
            .vertical_scrollbar(&self.list)
            .capture_any_mouse_down(move |event, _, _| {
                if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                    clicked_blank.set(None);
                }
            })
            // Off every row, a row dragged goes last at the top: the rows
            // under it say otherwise.
            .when(can_reorder, |list| {
                list.on_drag_move(move |_: &DragMoveEvent<DraggedSnippetNode>, _, _| {
                    drop_target.set(None);
                })
                .on_drop(move |drag: &DraggedSnippetNode, window, cx| {
                    if drop_target_for_top.get().is_none() {
                        dispatch.dispatch_action(
                            &MoveSnippetNode {
                                source: drag.node,
                                destination: SnippetDrop::Top,
                            },
                            window,
                            cx,
                        );
                    }
                    drop_target_for_top.set(None);
                })
            })
            // A right click off every row still focuses the panel, so the
            // menu's commands have somewhere to be dispatched from.
            .on_mouse_down(MouseButton::Right, {
                let focus = self.focus_handle.clone();
                move |_, window, cx| focus.focus(window, cx)
            })
            .context_menu({
                let store = self.store.clone();
                move |menu, _, cx| build_context_menu(menu_hit.get(), store.read(cx), menu)
            });
        v_flex()
            .id("snippets")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(header)
            .child(list)
            .child(self.row_tooltips.overlay())
    }
}

/// What the rows are drawn from, taken in `render`.
struct Rows {
    rows: Vec<SnippetRow>,
    /// Every category's name and the category it is in.
    categories: HashMap<SnippetCategoryId, (SharedString, Option<SnippetCategoryId>)>,
    snippets: HashMap<SnippetId, SnippetOf>,
    /// Not while searching: the rows shown are not all there is.
    can_reorder: bool,
    right_clicked: Rc<Cell<Option<SnippetNode>>>,
    drop_target: Rc<Cell<Option<(SnippetNode, SnippetDrop)>>>,
    row_tooltips: RowTooltips<SnippetId>,
    focus: FocusHandle,
    dispatch: FocusHandle,
}

/// A snippet's row as `render` took it.
struct SnippetOf {
    name: SharedString,
    command: String,
    /// A click runs it, rather than only typing it.
    runs: bool,
}

fn render_row(row: SnippetRow, rows: &Rows, cx: &mut App) -> AnyElement {
    let node = node_of(row);
    let (id, depth): (ElementId, usize) = match row {
        SnippetRow::Category { id, depth, .. } => (
            SharedString::from(format!("snippet-category:{}", id.0)).into(),
            depth,
        ),
        SnippetRow::Snippet { id, depth } => (("snippet", id.0).into(), depth),
    };
    // Only while its menu is open: a click acts at once, and leaves
    // nothing selected.
    let selected = rows.right_clicked.get() == Some(node);
    let item = ListItem::new(id)
        .w_full()
        .h_7()
        .px_2()
        .pl(rems(0.75 + depth as f32))
        .rounded(cx.theme().radius)
        // Make selection stronger than hover against the sidebar background.
        .confirmed(selected)
        .when(selected, |item| {
            item.bg(crate::app::selected_row_color(cx.theme()))
        })
        // A right click puts the menu on this row. The menu's commands are
        // dispatched from what is focused, which may be nothing.
        .on_mouse_down(MouseButton::Right, {
            let (right_clicked, focus) = (rows.right_clicked.clone(), rows.focus.clone());
            move |_, window, cx| {
                right_clicked.set(Some(node));
                focus.focus(window, cx);
            }
        });
    let label = match row {
        SnippetRow::Category { id, .. } => rows
            .categories
            .get(&id)
            .map(|(name, _)| name.clone())
            .unwrap_or_default(),
        SnippetRow::Snippet { id, .. } => rows
            .snippets
            .get(&id)
            .map(|snippet| snippet.name.clone())
            .unwrap_or_default(),
    };
    let item = if rows.can_reorder {
        draggable(item, node, label.clone(), rows)
    } else {
        item
    };
    let muted = cx.theme().muted_foreground;
    match row {
        SnippetRow::Category {
            id, count, folded, ..
        } => {
            let toggle = rows.dispatch.clone();
            item.aria_label(format!("{label} {count}"))
                .aria_expanded(!folded)
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
                .suffix(move |_, cx| {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground.opacity(0.7))
                        .child(count.to_string())
                })
                .on_click(move |_, window, cx| {
                    toggle.dispatch_action(&ToggleSnippetCategory(id), window, cx)
                })
                .into_any_element()
        }
        SnippetRow::Snippet { id, .. } => match rows.snippets.get(&id) {
            Some(snippet) => render_snippet(item, id, snippet, rows, cx),
            None => item.into_any_element(),
        },
    }
}

/// A snippet's row: its name. The pointer on it shows its command beside
/// the row, and at the row's end what a click does: ⚡ for one a click
/// runs, ▶ for one a click only types, which runs it. Out of the way
/// otherwise, the button keeps its room, so nothing moves as it shows.
fn render_snippet(
    item: ListItem,
    id: SnippetId,
    snippet: &SnippetOf,
    rows: &Rows,
    cx: &App,
) -> AnyElement {
    let runs = snippet.runs;
    let line = one_line(&snippet.command);
    // A click runs it when the snippet says so, and only types it
    // otherwise.
    let click = EnterCommand {
        command: snippet.command.clone(),
        run: runs,
    };
    let run = EnterCommand {
        command: snippet.command.clone(),
        run: true,
    };
    let tooltip = rows
        .row_tooltips
        .row(id, RowTooltip::command(snippet.command.clone()));
    let hover = SharedString::from(format!("snippet-row:{}", id.0));
    let (dispatch, run_dispatch) = (rows.dispatch.clone(), rows.dispatch.clone());
    let button_tooltip = tooltip.clone();
    let item = item
        .group(hover.clone())
        .aria_label(if runs {
            format!(
                "{} · {line} · {}",
                snippet.name,
                t!("snippets.card.runs_on_click")
            )
        } else {
            format!("{} · {line}", snippet.name)
        })
        .child(
            h_flex()
                .gap_2()
                .child(
                    Icon::new(CatalogIcon::Terminal)
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(div().flex_1().w_0().truncate().child(snippet.name.clone())),
        )
        .suffix(move |_, cx| {
            let button = Button::new(("snippet-run", id.0))
                .ghost()
                .xsmall()
                .map(|button| {
                    if runs {
                        button
                            .icon(Icon::new(CatalogIcon::Zap).text_color(cx.theme().warning))
                            .tooltip(t!("snippets.card.runs_on_click"))
                    } else {
                        button
                            .icon(Icon::new(CatalogIcon::Play))
                            .tooltip(t!("tools.run"))
                    }
                })
                .accessibility_label(t!("tools.run"))
                .on_click({
                    let (dispatch, run) = (run_dispatch.clone(), run.clone());
                    move |_, window, cx| {
                        // Not the row's click too, which would run it again
                        // or only type it.
                        cx.stop_propagation();
                        dispatch.dispatch_action(&run, window, cx);
                    }
                });
            // A press on it is not the row's, to drag.
            button_tooltip.exclude(
                div()
                    .id(("snippet-run-slot", id.0))
                    .flex_shrink_0()
                    .invisible()
                    .group_hover(hover.clone(), |style| style.visible())
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(button),
            )
        })
        .on_click(move |_, window, cx| dispatch.dispatch_action(&click, window, cx));
    // Around the whole row, indent and all: opening on its left, the
    // tooltip would otherwise cover the indent.
    tooltip
        .attach(
            div()
                .id(("snippet-tip", id.0))
                .relative()
                .w_full()
                .child(item),
        )
        .into_any_element()
}

/// Let a row be dragged, and dropped on, as the host tree's rows are: a
/// category beside another or, onto its middle, into it; a snippet beside
/// another or onto a category, last in it.
fn draggable(item: ListItem, node: SnippetNode, label: SharedString, rows: &Rows) -> ListItem {
    let (drop_target_for_move, drop_target_for_style, drop_target_for_drop) = (
        rows.drop_target.clone(),
        rows.drop_target.clone(),
        rows.drop_target.clone(),
    );
    let parents: Rc<HashMap<SnippetCategoryId, Option<SnippetCategoryId>>> = Rc::new(
        rows.categories
            .iter()
            .map(|(id, (_, parent))| (*id, *parent))
            .collect(),
    );
    let dispatch = rows.dispatch.clone();
    item.on_drag(DraggedSnippetNode { node, label }, |drag, _, _, cx| {
        cx.new(|_| drag.clone())
    })
    .on_drag_move(move |event: &DragMoveEvent<DraggedSnippetNode>, _, cx| {
        if !event.bounds.contains(&event.event.position) {
            return;
        }
        let source = event.drag(cx).node;
        let relative = (event.event.position.y - event.bounds.top()) / event.bounds.size.height;
        let destination = match (source, node) {
            (SnippetNode::Snippet(_), SnippetNode::Category(id)) => SnippetDrop::Into(id),
            (SnippetNode::Category(_), SnippetNode::Category(id))
                if relative > 0.25 && relative < 0.75 =>
            {
                SnippetDrop::Into(id)
            }
            _ if relative < 0.5 => SnippetDrop::Before(node),
            _ => SnippetDrop::After(node),
        };
        if valid_drop(source, destination, &parents) {
            drop_target_for_move.set(Some((node, destination)));
        }
    })
    .drag_over::<DraggedSnippetNode>(move |style, drag, _, cx| {
        if drag.node == node {
            return style;
        }
        match drop_target_for_style.get() {
            Some((target, SnippetDrop::Before(_))) if target == node => {
                style.border_t_1().border_color(cx.theme().primary)
            }
            Some((target, SnippetDrop::After(_))) if target == node => {
                style.border_b_1().border_color(cx.theme().primary)
            }
            Some((target, SnippetDrop::Into(_))) if target == node => style
                .bg(cx.theme().accent)
                .border_1()
                .border_color(cx.theme().primary),
            _ => style,
        }
    })
    .on_drop(move |drag: &DraggedSnippetNode, window, cx| {
        if let Some((target, destination)) = drop_target_for_drop.get()
            && target == node
            && drag.node != node
        {
            dispatch.dispatch_action(
                &MoveSnippetNode {
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

/// Whether `source` may land at `destination`, from the render's snapshot:
/// a row beside one of its own kind, a snippet into any category, and a
/// category never into itself or a category inside it. What may not shows
/// no place to land.
fn valid_drop(
    source: SnippetNode,
    destination: SnippetDrop,
    parents: &HashMap<SnippetCategoryId, Option<SnippetCategoryId>>,
) -> bool {
    match (source, destination) {
        (_, SnippetDrop::Before(target) | SnippetDrop::After(target)) if target == source => false,
        (SnippetNode::Snippet(_), SnippetDrop::Before(target) | SnippetDrop::After(target)) => {
            matches!(target, SnippetNode::Snippet(_))
        }
        (SnippetNode::Snippet(_), SnippetDrop::Into(category)) => parents.contains_key(&category),
        (_, SnippetDrop::Top) => true,
        (SnippetNode::Category(id), destination) => {
            let mut parent = match destination {
                SnippetDrop::Before(SnippetNode::Category(target))
                | SnippetDrop::After(SnippetNode::Category(target)) => match parents.get(&target) {
                    Some(parent) => *parent,
                    None => return false,
                },
                SnippetDrop::Into(category) if parents.contains_key(&category) => Some(category),
                _ => return false,
            };
            for _ in 0..=parents.len() {
                let Some(current) = parent else {
                    return true;
                };
                if current == id {
                    return false;
                }
                parent = parents.get(&current).copied().flatten();
            }
            false
        }
    }
}

/// What follows the pointer while a row is dragged: its name.
#[derive(Clone)]
struct DraggedSnippetNode {
    node: SnippetNode,
    label: SharedString,
}

impl Render for DraggedSnippetNode {
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

/// The list's menu, for what a right click landed on: a snippet, a
/// category, or nothing. Built as it opens, from the store as it is then.
fn build_context_menu(hit: Option<SnippetNode>, store: &HostStore, menu: PopupMenu) -> PopupMenu {
    match hit {
        None => menu
            .menu_with_icon(
                t!("snippets.menu.new_snippet"),
                Icon::new(IconName::Plus),
                Box::new(NewSnippet),
            )
            .menu_with_icon(
                t!("snippets.menu.new_category"),
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewSnippetCategory),
            ),
        Some(SnippetNode::Category(id)) => menu
            .menu_with_icon(
                t!("snippets.menu.new_snippet"),
                Icon::new(IconName::Plus),
                Box::new(NewSnippetIn(id)),
            )
            .menu_with_icon(
                t!("snippets.menu.new_subcategory"),
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewSnippetSubcategory(id)),
            )
            .separator()
            .menu_with_icon(
                t!("snippets.menu.rename_category"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameSnippetCategory(id)),
            )
            .separator()
            .menu_with_icon(
                t!("snippets.menu.delete_category"),
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteSnippetCategory(id)),
            ),
        Some(SnippetNode::Snippet(id)) => {
            let Some(snippet) = store.snippet(id) else {
                return menu;
            };
            let command = snippet.command.clone();
            menu.menu_with_icon(
                t!("tools.menu.insert"),
                Icon::new(CatalogIcon::SquareTerminal),
                Box::new(EnterCommand {
                    command: command.clone(),
                    run: false,
                }),
            )
            .menu_with_icon(
                t!("tools.run"),
                Icon::new(CatalogIcon::Play),
                Box::new(EnterCommand {
                    command: command.clone(),
                    run: true,
                }),
            )
            .menu_with_icon(
                t!("snippets.menu.copy"),
                Icon::new(IconName::Copy),
                Box::new(CopyCommand(command)),
            )
            .separator()
            .menu_with_icon(
                t!("snippets.menu.edit"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(EditSnippet(id)),
            )
            .menu_with_icon(
                t!("snippets.menu.delete"),
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteSnippet(id)),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    // Imported one by one: `use gpui_kit::*` above would shadow `#[test]`.
    use std::collections::HashMap;

    use super::{SnippetCategoryId, SnippetDrop, SnippetId, SnippetNode, valid_drop};

    #[test]
    fn a_category_never_lands_inside_itself() {
        let (ops, logs, nginx) = (
            SnippetCategoryId(1),
            SnippetCategoryId(2),
            SnippetCategoryId(3),
        );
        let parents = HashMap::from([(ops, None), (logs, Some(ops)), (nginx, Some(logs))]);
        let category = SnippetNode::Category(ops);
        assert!(!valid_drop(category, SnippetDrop::Into(ops), &parents));
        assert!(!valid_drop(category, SnippetDrop::Into(nginx), &parents));
        assert!(!valid_drop(
            category,
            SnippetDrop::Before(SnippetNode::Category(nginx)),
            &parents
        ));
        assert!(valid_drop(
            SnippetNode::Category(nginx),
            SnippetDrop::Before(SnippetNode::Category(ops)),
            &parents
        ));
        assert!(valid_drop(category, SnippetDrop::Top, &parents));
        // A category goes beside categories only.
        let snippet = SnippetNode::Snippet(SnippetId(1));
        assert!(!valid_drop(category, SnippetDrop::After(snippet), &parents));
        assert!(!valid_drop(snippet, SnippetDrop::After(category), &parents));
        assert!(valid_drop(snippet, SnippetDrop::Into(nginx), &parents));
        assert!(!valid_drop(snippet, SnippetDrop::Before(snippet), &parents));
    }
}
