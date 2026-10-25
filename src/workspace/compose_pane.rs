//! 撰写窗格: commands written and edited below the terminals, then run in
//! the current terminal, or broadcast to several: the terminals of the tab
//! in front, or those chosen by hand, which makes it the broadcast's
//! command bar. A panel of the dock's bottom dock, there while it shows.
//! Every control dispatches to the workspace, which owns where it sends.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    menu::DropdownMenu as _,
    switch::Switch,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, ChooseComposeTerminals, SendCompose, SendComposeInterrupt, SendComposeSnippet,
    SendTarget, SetSendTarget, ToggleBroadcastKeys, ToggleComposePane,
};
use crate::host::{HostStore, SnippetId};
use crate::i18n::{UiLocale, t, tn};

/// A category's name (none for the uncategorized) and its snippets.
type SnippetGroup = (Option<SharedString>, Vec<(SnippetId, SharedString)>);

/// Where the pane sends.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ComposeTarget {
    /// The current terminal, by its name.
    Terminal(SharedString),
    /// The connected terminals of the tab in front, this many.
    Tab(usize),
    /// The terminals chosen by hand, this many.
    Selected(usize),
}

impl ComposeTarget {
    fn kind(&self) -> SendTarget {
        match self {
            Self::Terminal(_) => SendTarget::Current,
            Self::Tab(_) => SendTarget::Tab,
            Self::Selected(_) => SendTarget::Selected,
        }
    }

    /// As the title tells it.
    fn label(&self) -> SharedString {
        match self {
            Self::Terminal(name) => name.clone(),
            Self::Tab(count) => tn!("workspace.compose.tab_target", *count),
            Self::Selected(count) => tn!("workspace.compose.selected_target", *count),
        }
    }
}

pub(super) struct ComposePane {
    input: Entity<TextareaState>,
    store: Entity<HostStore>,
    /// The workspace's: the pane's commands go there.
    dispatch: FocusHandle,
    target: ComposeTarget,
    /// 命令广播's 同步按键.
    keys: bool,
    _subscriptions: Vec<Subscription>,
}

impl ComposePane {
    pub(super) fn new(
        store: Entity<HostStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Enter sends, Shift+Enter starts a new line.
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .placeholder(t!("workspace.compose.placeholder"))
        });
        let subscriptions = vec![
            // The snippets menu lists what the store has.
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.input.update(cx, |input, cx| {
                    input.set_placeholder(t!("workspace.compose.placeholder"), window, cx)
                });
            }),
            // Through the window, which queues it: the handler reads the
            // text, so it must not run while the pane is being updated. The
            // text has the focus, inside the workspace. Shift+Enter tells of
            // the line it has just started.
            cx.subscribe_in(&input, window, |_, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    window.dispatch_action(Box::new(SendCompose), cx);
                }
            }),
        ];
        Self {
            input,
            store,
            dispatch,
            target: ComposeTarget::Selected(0),
            keys: false,
            _subscriptions: subscriptions,
        }
    }

    /// What is written.
    pub(super) fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// Empty the pane, once what it held went out.
    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    pub(super) fn set_target(&mut self, target: ComposeTarget, keys: bool, cx: &mut Context<Self>) {
        if self.target != target || self.keys != keys {
            self.target = target;
            self.keys = keys;
            cx.notify();
        }
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn broadcasting(&self) -> bool {
        self.target.kind() != SendTarget::Current
    }

    /// The snippets by category, the uncategorized first, as 命令片段
    /// lists them.
    fn snippet_groups(&self, cx: &App) -> Vec<SnippetGroup> {
        let store = self.store.read(cx);
        let of = |category| {
            store
                .snippets()
                .iter()
                .filter(|snippet| snippet.category == category)
                .map(|snippet| (snippet.id, snippet.name.clone()))
                .collect::<Vec<_>>()
        };
        std::iter::once((None, of(None)))
            .chain(
                store
                    .snippet_categories()
                    .iter()
                    .map(|category| (Some(category.name.clone()), of(Some(category.id)))),
            )
            .filter(|(_, snippets)| !snippets.is_empty())
            .collect()
    }
}

impl EventEmitter<PanelEvent> for ComposePane {}

impl Focusable for ComposePane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl BasePanel for ComposePane {
    fn panel_name(&self) -> &'static str {
        "ComposePane"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl ComposePane {
    /// The pane, and where it sends, which its menu changes: the current
    /// terminal, the tab's terminals, or terminals chosen by hand.
    fn render_title(&self, cx: &App) -> impl IntoElement {
        let target = self.target.label();
        let kind = self.target.kind();
        let title = t!("workspace.compose.title");
        let dispatch = self.dispatch.clone();
        h_flex()
            .id("compose-pane-title")
            .test_support()
            .aria_label(format!("{title} · {target}"))
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(Icon::new(CatalogIcon::SquarePen).small())
            .child(div().flex_shrink_0().child(title))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child("·"),
            )
            .child(
                div().min_w_0().child(
                    Button::new("compose-pane-target")
                        .xsmall()
                        .ghost()
                        .label(target.clone())
                        .accessibility_label(target)
                        .dropdown_caret(true)
                        .tooltip(t!("workspace.compose.target.tooltip"))
                        .dropdown_menu(move |menu, _, _| {
                            menu.action_context(dispatch.clone())
                                .menu_with_check(
                                    t!("workspace.compose.target.current"),
                                    kind == SendTarget::Current,
                                    Box::new(SetSendTarget(SendTarget::Current)),
                                )
                                .menu_with_check(
                                    t!("workspace.compose.target.tab"),
                                    kind == SendTarget::Tab,
                                    Box::new(SetSendTarget(SendTarget::Tab)),
                                )
                                .menu_with_check(
                                    t!("workspace.compose.target.selected"),
                                    kind == SendTarget::Selected,
                                    Box::new(ChooseComposeTerminals),
                                )
                        }),
                ),
            )
    }

    /// What goes with sending: the snippets, Ctrl+C, while broadcasting
    /// whether keys are shared, and 发送.
    fn render_controls(&self, cx: &App) -> impl IntoElement {
        let groups = Rc::new(self.snippet_groups(cx));
        let no_snippets = groups.is_empty();
        // Picking a snippet hands the keyboard back to the text.
        let input_focus = self.input.read(cx).focus_handle(cx);
        let send = |action: &'static dyn Action, dispatch: &FocusHandle| {
            let dispatch = dispatch.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                dispatch.dispatch_action(action, window, cx)
            }
        };
        let keys_dispatch = self.dispatch.clone();
        let broadcasting = self.broadcasting();
        h_flex()
            .flex_shrink_0()
            .gap_1()
            .child(
                Button::new("compose-snippets")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(CatalogIcon::CodeXml))
                    .label(t!("workspace.compose.snippets"))
                    .tooltip(if no_snippets {
                        t!("workspace.compose.no_snippets")
                    } else {
                        t!("workspace.compose.snippets_tooltip")
                    })
                    .disabled(no_snippets)
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu
                            .action_context(input_focus.clone())
                            .max_h(px(420.))
                            .scrollable(true);
                        for (ix, (category, snippets)) in groups.iter().enumerate() {
                            if ix > 0 {
                                menu = menu.separator();
                            }
                            if let Some(name) = category {
                                menu = menu.label(name.clone());
                            }
                            for (id, name) in snippets {
                                menu = menu.menu(name.clone(), Box::new(SendComposeSnippet(*id)));
                            }
                        }
                        menu
                    }),
            )
            .when(broadcasting, |row| {
                // A switch, like 目录跟随's: the knob shows the state. The
                // tooltip goes around it, label included: the switch's
                // own shows over the knob alone.
                row.child(
                    div()
                        .id("broadcast-keys-tip")
                        .flex_shrink_0()
                        .tooltip(|window, cx| {
                            Tooltip::new(t!("workspace.compose.keys_tooltip")).build(window, cx)
                        })
                        .child(
                            Switch::new("broadcast-keys")
                                .xsmall()
                                .checked(self.keys)
                                .label(t!("workspace.compose.keys"))
                                .on_click(move |_, window, cx| {
                                    keys_dispatch.dispatch_action(&ToggleBroadcastKeys, window, cx)
                                }),
                        ),
                )
            })
            .child(
                Button::new("compose-interrupt")
                    .xsmall()
                    .ghost()
                    .label("Ctrl+C")
                    .tooltip(t!("workspace.compose.interrupt"))
                    .on_click(send(&SendComposeInterrupt, &self.dispatch)),
            )
            .child(
                Button::new("compose-send")
                    .xsmall()
                    .primary()
                    .icon(Icon::new(CatalogIcon::Send))
                    .label(if broadcasting {
                        t!("workspace.compose.send_all")
                    } else {
                        t!("workspace.compose.send")
                    })
                    .tooltip(t!("workspace.compose.send_tooltip"))
                    .on_click(send(&SendCompose, &self.dispatch)),
            )
    }

    /// × hides the pane, as its switch does; a broadcast stays chosen.
    fn render_close(&self) -> impl IntoElement {
        let dispatch = self.dispatch.clone();
        Button::new("compose-close")
            .xsmall()
            .ghost()
            .tab_stop(false)
            .icon(IconName::Close)
            .tooltip(t!("workspace.compose.close"))
            .on_click(move |_, window, cx| dispatch.dispatch_action(&ToggleComposePane, window, cx))
    }
}

/// The dock draws no title bar over the pane (see `WorkspaceTabBar`): its
/// own has no 「…」 menu, which would hold nothing here.
impl Panel for ComposePane {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        t!("workspace.compose.title")
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for ComposePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let broadcasting = self.broadcasting();
        v_flex()
            .id("compose-pane")
            .test_support()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .child(
                // Laid out as the dock's own title bars are, which it sizes
                // in pixels: the panes beside it line up.
                h_flex()
                    .id("compose-pane-bar")
                    .flex_shrink_0()
                    .h(px(30.))
                    .py_2()
                    .pl_3()
                    .pr_2()
                    .gap_2()
                    // Broadcasting, the bar takes the warning color: what
                    // is sent here goes to several terminals at once.
                    .when(broadcasting, |bar| {
                        bar.bg(cx.theme().warning.opacity(0.15))
                            .border_b_1()
                            .border_color(cx.theme().warning.opacity(0.5))
                    })
                    .child(self.render_title(cx))
                    .child(self.render_controls(cx))
                    .child(self.render_close()),
            )
            .child(
                div().flex_1().min_h_0().px_2().pb_2().child(
                    Textarea::new(&self.input)
                        .h(relative(1.))
                        .text_sm()
                        .aria_label(t!("workspace.compose.title"))
                        .font_family(cx.theme().mono_font_family.clone()),
                ),
            )
    }
}
