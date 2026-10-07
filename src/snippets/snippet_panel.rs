use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::model::{SnippetRow, rows};
use crate::app::{
    CatalogIcon, CopyCommand, DeleteSnippet, DeleteSnippetCategory, EditSnippet, EnterCommand,
    NewSnippet, NewSnippetCategory, NewSnippetIn, RenameSnippetCategory, ToggleSnippetCategory,
};
use crate::host::{HostStore, Snippet, SnippetCategoryId, SnippetId};
use crate::i18n::{UiLocale, t, tn};
use crate::shared::{command_tooltip, one_line};

/// 命令片段: the commands kept for every host, in categories one level
/// deep. A click puts one on the input line of the SSH terminal in front to
/// edit; 执行 runs it there.
///
/// The snippets are in the host store, so they are the same whichever
/// terminal is in front, and nothing is read from the host. Which
/// categories are folded and the search stay as they are from one terminal
/// to the next; the folds are not saved.
pub struct SnippetPanel {
    store: Entity<HostStore>,
    search: Entity<InputState>,
    query: String,
    folded: HashSet<Option<SnippetCategoryId>>,
    list: ListState,
    /// The lines the list was last told about.
    rows: Vec<SnippetRow>,
    /// The search changed: back to the top.
    back_to_top: bool,
    /// What a right click landed on, for the list's menu; `None` off every
    /// line.
    menu_hit: Rc<RefCell<Option<MenuHit>>>,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum MenuHit {
    Snippet(SnippetId),
    Category(Option<SnippetCategoryId>),
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
            cx.observe(&store, |_, _, cx| cx.notify()),
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
        Self {
            store,
            search,
            query: String::new(),
            folded: HashSet::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            rows: Vec::new(),
            back_to_top: false,
            menu_hit: Rc::new(RefCell::new(None)),
            dispatch,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Fold a category away, or unfold it; `None` is 未分类.
    pub fn toggle_category(&mut self, id: Option<SnippetCategoryId>, cx: &mut Context<Self>) {
        if !self.folded.remove(&id) {
            self.folded.insert(id);
        }
        cx.notify();
    }

    /// Tell the list about `rows` when they are not the ones it has; back
    /// at the top when the search changed.
    fn sync_list(&mut self, rows: &[SnippetRow]) {
        if std::mem::take(&mut self.back_to_top) {
            self.list.reset(rows.len());
        } else if self.rows != rows {
            self.list.splice(0..self.rows.len(), rows.len());
        }
        self.rows = rows.to_vec();
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

impl Focusable for SnippetPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SnippetPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let snippets: Rc<Vec<Snippet>> = Rc::new(store.snippets().to_vec());
        let names: Rc<Vec<(SnippetCategoryId, SharedString)>> = Rc::new(
            store
                .snippet_categories()
                .iter()
                .map(|category| (category.id, category.name.clone()))
                .collect(),
        );
        let rows = rows(
            store.snippet_categories(),
            &snippets,
            &self.query,
            &self.folded,
        );
        let nothing = snippets.is_empty() && names.is_empty();
        self.sync_list(&rows);
        let header = self.render_header(snippets.len(), names.len(), cx);

        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();
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
                    let rows = Rc::new(rows);
                    let (hit, focus, dispatch) = (
                        self.menu_hit.clone(),
                        self.focus_handle.clone(),
                        self.dispatch.clone(),
                    );
                    list.child(
                        gpui_kit::list(self.list.clone(), move |index, _, cx| {
                            let last = index + 1 == rows.len();
                            match rows[index] {
                                SnippetRow::Category { id, count, folded } => {
                                    let name = id
                                        .and_then(|id| names.iter().find(|(each, _)| *each == id))
                                        .map_or_else(
                                            || t!("snippets.uncategorized"),
                                            |(_, name)| name.clone(),
                                        );
                                    render_category(
                                        Heading {
                                            id,
                                            name,
                                            count,
                                            folded,
                                            last,
                                        },
                                        hit.clone(),
                                        focus.clone(),
                                        dispatch.clone(),
                                        cx,
                                    )
                                    .into_any_element()
                                }
                                SnippetRow::Snippet(id) => {
                                    let Some(snippet) =
                                        snippets.iter().find(|snippet| snippet.id == id)
                                    else {
                                        return div().into_any_element();
                                    };
                                    div()
                                        .px_3()
                                        .pt_2()
                                        // The last one clears the bottom.
                                        .when(last, |row| row.pb_3())
                                        .child(render_snippet(
                                            snippet,
                                            hit.clone(),
                                            focus.clone(),
                                            dispatch.clone(),
                                            cx,
                                        ))
                                        .into_any_element()
                                }
                            }
                        })
                        .size_full(),
                    )
                }
            })
            .vertical_scrollbar(&self.list)
            .capture_any_mouse_down(move |event, _, _| {
                if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                    clear_hit.replace(None);
                }
            })
            // A right click off every line still focuses the panel, so the
            // menu's commands have somewhere to be dispatched from.
            .on_mouse_down(MouseButton::Right, {
                let focus = self.focus_handle.clone();
                move |_, window, cx| focus.focus(window, cx)
            })
            .context_menu({
                let store = self.store.clone();
                move |menu, _, cx| build_context_menu(*menu_hit.borrow(), store.read(cx), menu)
            });
        v_flex()
            .id("snippets")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(header)
            .child(list)
    }
}

/// The list's menu, for what a right click landed on: a snippet, a
/// category's heading, or nothing. Built as it opens, from the store as it
/// is then.
fn build_context_menu(hit: Option<MenuHit>, store: &HostStore, menu: PopupMenu) -> PopupMenu {
    let new_snippet = |menu: PopupMenu, action: Box<dyn Action>| {
        menu.menu_with_icon(
            t!("snippets.menu.new_snippet"),
            Icon::new(IconName::Plus),
            action,
        )
    };
    match hit {
        None => new_snippet(menu, Box::new(NewSnippet)).menu_with_icon(
            t!("snippets.menu.new_category"),
            Icon::new(CatalogIcon::FolderPlus),
            Box::new(NewSnippetCategory),
        ),
        Some(MenuHit::Category(None)) => new_snippet(menu, Box::new(NewSnippet)),
        Some(MenuHit::Category(Some(id))) => new_snippet(menu, Box::new(NewSnippetIn(id)))
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
        Some(MenuHit::Snippet(id)) => {
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

/// What a category's heading shows.
struct Heading {
    /// `None` is 未分类.
    id: Option<SnippetCategoryId>,
    name: SharedString,
    count: usize,
    folded: bool,
    /// The list's last line, which clears the bottom.
    last: bool,
}

/// A category's heading: a click folds it away or unfolds it.
fn render_category(
    heading: Heading,
    menu_hit: Rc<RefCell<Option<MenuHit>>>,
    focus: FocusHandle,
    dispatch: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let Heading {
        id,
        name,
        count,
        folded,
        last,
    } = heading;
    let key = id.map_or("none".to_string(), |id| id.0.to_string());
    h_flex()
        .id(SharedString::from(format!("snippet-category:{key}")))
        .test_support()
        .aria_label(format!("{name} {count}"))
        .aria_expanded(!folded)
        .px_3()
        .pt_3()
        .pb_1()
        .when(last, |line| line.pb_3())
        .gap_1p5()
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            dispatch.dispatch_action(&ToggleSnippetCategory(id), window, cx)
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            menu_hit.replace(Some(MenuHit::Category(id)));
            focus.focus(window, cx);
        })
        .child(
            Icon::new(if folded {
                IconName::ChevronRight
            } else {
                IconName::ChevronDown
            })
            .small()
            .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(name),
        )
        .child(
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(count.to_string()),
        )
}

/// A snippet's card: its name, and its command on one line below.
///
/// What a click does is not the same for every snippet, so the pointer on
/// a card shows it at the right: ⚡ for one a click runs, ▶ 执行 for one a
/// click only types, which runs it. Out of the way otherwise, the slot
/// keeps its room, so nothing moves as it shows.
fn render_snippet(
    snippet: &Snippet,
    menu_hit: Rc<RefCell<Option<MenuHit>>>,
    focus: FocusHandle,
    dispatch: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let id = snippet.id;
    let line = one_line(&snippet.command);
    let runs = snippet.run_on_click;
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
    let tooltip = command_tooltip(&snippet.command, cx);
    let card = SharedString::from(format!("snippet-card:{}", id.0));
    h_flex()
        .id(("snippet", id.0))
        .test_support()
        .group(card.clone())
        .aria_label(if runs {
            format!(
                "{} · {line} · {}",
                snippet.name,
                t!("snippets.card.runs_on_click")
            )
        } else {
            format!("{} · {line}", snippet.name)
        })
        .gap_2()
        .py_2()
        .pl_3()
        .pr_2()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .text_color(cx.theme().group_box_foreground)
        // A click types it, or runs it; the border says it does something.
        .border_1()
        .border_color(cx.theme().tokens.group_box)
        .hover({
            let border = cx.theme().border;
            move |style| style.border_color(border)
        })
        .cursor_pointer()
        .on_click({
            let dispatch = dispatch.clone();
            move |_, window, cx| dispatch.dispatch_action(&click, window, cx)
        })
        // A right click puts the menu on this snippet. The menu's commands
        // are dispatched from what is focused, which may be nothing.
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            menu_hit.replace(Some(MenuHit::Snippet(id)));
            focus.focus(window, cx);
        })
        // The whole of a command too long for its line.
        .when_some(tooltip, |card, tooltip| card.tooltip(tooltip))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(snippet.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(line),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .invisible()
                .group_hover(card, |style| style.visible())
                .child(
                    // As tall as the card's two lines allow: an easy target.
                    Button::new(("snippet-run", id.0))
                        .ghost()
                        .map(|button| {
                            if runs {
                                button
                                    .icon(
                                        Icon::new(CatalogIcon::Zap).text_color(cx.theme().warning),
                                    )
                                    .tooltip(t!("snippets.card.runs_on_click"))
                            } else {
                                button
                                    .icon(Icon::new(CatalogIcon::Play))
                                    .tooltip(t!("tools.run"))
                            }
                        })
                        .accessibility_label(t!("tools.run"))
                        .on_click(move |_, window, cx| {
                            // Not the card's click too, which would run it
                            // again or only type it.
                            cx.stop_propagation();
                            dispatch.dispatch_action(&run, window, cx);
                        }),
                ),
        )
}
