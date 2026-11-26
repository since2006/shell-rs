//! 终端 → 侧栏功能: which tools the right sidebar offers, as two rows of
//! their icons, the tools it shows above and the rest below. A click on an
//! icon, or dragging it to the other row, moves it across; pointing at one
//! says what the tool does.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    setting::{SettingGroup, SettingItem},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::ToolKind;
use crate::i18n::t;

use super::SettingsStore;
use super::settings_panel::keywords;

/// How far a press may move and still be a click: GPUI's own drag
/// threshold, which is private.
const DRAG_THRESHOLD: f64 = 2.;

/// A tool's icon on its way to the other row, and the row it left.
#[derive(Clone)]
struct DraggedTool {
    tool: ToolKind,
    shown: bool,
}

impl Render for DraggedTool {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        face(self.tool, cx)
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .shadow_md()
    }
}

/// The group: its one item is both rows, with 恢复默认 for them alone; the
/// page's 重置 puts them back too.
pub(super) fn sidebar_tools_group(store: &Entity<SettingsStore>) -> SettingGroup {
    let (reader, dirty, reset) = (store.clone(), store.clone(), store.clone());
    let item = SettingItem::render(move |_, _, cx| {
        let tools = reader.read(cx).settings().sidebar_tools;
        let (shown, hidden): (Vec<ToolKind>, Vec<ToolKind>) = ToolKind::ALL
            .into_iter()
            .partition(|tool| tools.shows(*tool));
        let restore = reader.clone();
        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(t!("settings.sidebar_tools.shown")),
                    )
                    .child(
                        Button::new("sidebar-tools-reset")
                            .ghost()
                            .xsmall()
                            .label(t!("settings.sidebar_tools.reset"))
                            .disabled(tools.is_default())
                            .on_click(move |_, _, cx| {
                                restore.update(cx, |store, cx| {
                                    store.update(|settings| settings.sidebar_tools.reset(), cx)
                                });
                            }),
                    ),
            )
            .child(row(true, &shown, &reader, cx))
            .child(
                div()
                    .pt_2()
                    .text_sm()
                    .child(t!("settings.sidebar_tools.hidden")),
            )
            .child(row(false, &hidden, &reader, cx))
    })
    .keywords(
        keywords(t!("settings.sidebar_tools.keywords"))
            .into_iter()
            .chain(ToolKind::ALL.iter().map(|tool| tool.label())),
    )
    .on_reset(
        move |cx| !dirty.read(cx).settings().sidebar_tools.is_default(),
        move |_, cx| {
            reset.update(cx, |store, cx| {
                store.update(|settings| settings.sidebar_tools.reset(), cx)
            });
        },
    );
    SettingGroup::new()
        .title(t!("settings.sidebar_tools.title"))
        .description(t!("settings.sidebar_tools.description"))
        .item(item)
}

/// Offer `tool`, or not: written to the settings at once, as every field
/// of the page is.
fn show(store: &Entity<SettingsStore>, tool: ToolKind, shown: bool, cx: &mut App) {
    store.update(cx, |store, cx| {
        store.update(|settings| settings.sidebar_tools.set(tool, shown), cx)
    });
}

/// A row: the tools that are `shown`, or those that are not, in the
/// switch's order; where an icon from the other row lands, lit up while
/// one is over it.
fn row(
    shown: bool,
    tools: &[ToolKind],
    store: &Entity<SettingsStore>,
    cx: &App,
) -> impl IntoElement {
    let landing = store.clone();
    let empty = if shown {
        t!("settings.sidebar_tools.shown_empty")
    } else {
        t!("settings.sidebar_tools.hidden_empty")
    };
    div()
        .id(if shown {
            "sidebar-tools-shown"
        } else {
            "sidebar-tools-hidden"
        })
        .test_support()
        .w_full()
        .min_h_20()
        .p_1p5()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_1()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        // The tools not shown wait in a box of their own.
        .when(!shown, |row| row.border_dashed())
        .drag_over::<DraggedTool>(move |style, dragged, _, cx| {
            if dragged.shown == shown {
                style
            } else {
                style.border_color(cx.theme().primary).bg(cx.theme().muted)
            }
        })
        .on_drop(move |dragged: &DraggedTool, _, cx| {
            if dragged.shown != shown {
                show(&landing, dragged.tool, shown, cx);
            }
        })
        .map(|row| {
            if tools.is_empty() {
                row.child(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(empty),
                )
            } else {
                row.children(tools.iter().map(|tool| tile(*tool, shown, store, cx)))
            }
        })
}

/// What a tool looks like in a row and on its way: its icon, as the switch
/// shows it, over its name.
fn face(tool: ToolKind, cx: &App) -> Div {
    v_flex()
        .w_20()
        .py_2()
        .px_1()
        .gap_1()
        .items_center()
        .child(tool.icon().text_color(cx.theme().foreground))
        .child(
            div()
                .w_full()
                .text_center()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(tool.label()),
        )
}

/// A tool in its row: a click moves it to the other, as does dragging it
/// there; pointing at it says what it does and what a click does.
fn tile(tool: ToolKind, shown: bool, store: &Entity<SettingsStore>, cx: &App) -> impl IntoElement {
    let toggle = store.clone();
    let hover = cx.theme().accent;
    face(tool, cx)
        .id(SharedString::from(format!("sidebar-tool-{}", tool.key())))
        .test_support()
        .aria_label(tool.label())
        .rounded(cx.theme().radius)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .tooltip(move |window, cx| {
            Tooltip::element(move |_, cx| {
                v_flex()
                    .max_w_72()
                    .gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child(tool.label()))
                    .child(div().text_sm().child(description(tool)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(if shown {
                                t!("settings.sidebar_tools.click_to_hide")
                            } else {
                                t!("settings.sidebar_tools.click_to_show")
                            }),
                    )
            })
            .build(window, cx)
        })
        // A quick flick lets go in the frame it starts, and still clicks:
        // the drop has moved it already.
        .on_click(move |event, _, cx| {
            let flicked = match event {
                ClickEvent::Mouse(click) => {
                    (click.up.position - click.down.position).magnitude() > DRAG_THRESHOLD
                }
                _ => false,
            };
            if !flicked {
                show(&toggle, tool, !shown, cx);
            }
        })
        .on_drag(DraggedTool { tool, shown }, |dragged, _, _, cx| {
            cx.new(|_| dragged.clone())
        })
}

/// What a tool of the right sidebar is for, in a line.
pub(super) fn description(tool: ToolKind) -> SharedString {
    match tool {
        ToolKind::Snippets => t!("settings.sidebar_tools.snippets"),
        ToolKind::History => t!("settings.sidebar_tools.history"),
        ToolKind::Files => t!("settings.sidebar_tools.files"),
        ToolKind::Assistant => t!("settings.sidebar_tools.assistant"),
        ToolKind::Tmux => t!("settings.sidebar_tools.tmux"),
        ToolKind::Docker => t!("settings.sidebar_tools.docker"),
        ToolKind::Kubernetes => t!("settings.sidebar_tools.kubernetes"),
        ToolKind::Services => t!("settings.sidebar_tools.services"),
        ToolKind::Processes => t!("settings.sidebar_tools.processes"),
        ToolKind::Connections => t!("settings.sidebar_tools.connections"),
        ToolKind::Monitor => t!("settings.sidebar_tools.monitor"),
        ToolKind::Gpu => t!("settings.sidebar_tools.gpu"),
        ToolKind::Projects => t!("settings.sidebar_tools.projects"),
    }
}
