//! Controlled compose presentation. Workspace owns scope, submission and sync;
//! this retained view owns only the draft and its editing/focus behavior.
use crate::app::{
    CatalogIcon, ComposeScope, EditComposeAsNew, RetryCompose, SendCompose, SendComposeInterrupt,
    SendComposeSnippet, SetComposeScope, ToggleComposeKeySync, ToggleComposePane,
};
use crate::host::{HostStore, SnippetId};
use crate::i18n::{UiLocale, t};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    menu::DropdownMenu as _,
    popover::Popover,
    switch::Switch,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

type SnippetGroup = (Option<SharedString>, Vec<(SnippetId, SharedString)>);

#[derive(Clone, Default, PartialEq)]
pub(super) struct ComposePresentation {
    pub(super) scope: ComposeScope,
    pub(super) summary: SharedString,
    pub(super) names: SharedString,
    pub(super) count: usize,
    pub(super) available: bool,
    pub(super) keys: bool,
    pub(super) targets_changed: bool,
    pub(super) blocked: bool,
    pub(super) result: Option<SharedString>,
    pub(super) result_command: Option<SharedString>,
    pub(super) details: Vec<SharedString>,
    pub(super) retry: bool,
}

pub(super) struct ComposePane {
    input: Entity<TextareaState>,
    store: Entity<HostStore>,
    dispatch: FocusHandle,
    presentation: ComposePresentation,
    _subscriptions: Vec<Subscription>,
}
impl ComposePane {
    pub(super) fn new(
        store: Entity<HostStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .placeholder(t!("workspace.compose.placeholder"))
        });
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&input, |_, _, cx| cx.notify()),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.input.update(cx, |input, cx| {
                    input.set_placeholder(t!("workspace.compose.placeholder"), window, cx)
                });
            }),
            // Do not submit an IME candidate that is still marked.
            // Queue the action so Workspace reads the draft after this update.
            cx.subscribe_in(
                &input,
                window,
                |_, input, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = event
                        && input
                            .update(cx, |input, cx| input.marked_text_range(window, cx))
                            .is_none()
                    {
                        window.dispatch_action(Box::new(SendCompose), cx);
                    }
                },
            ),
        ];
        Self {
            input,
            store,
            dispatch,
            presentation: Default::default(),
            _subscriptions: subscriptions,
        }
    }
    pub(super) fn text(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }
    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_text("", window, cx);
    }
    pub(super) fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value(text.to_owned(), window, cx));
    }
    pub(super) fn set_presentation(
        &mut self,
        presentation: ComposePresentation,
        cx: &mut Context<Self>,
    ) {
        if self.presentation != presentation {
            self.presentation = presentation;
            cx.notify();
        }
    }
    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
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

fn scope_label(scope: ComposeScope) -> SharedString {
    match scope {
        ComposeScope::Current => t!("workspace.compose.scope_current"),
        ComposeScope::Window => t!("workspace.compose.scope_window"),
        ComposeScope::Selected => t!("workspace.compose.scope_selected"),
    }
}
impl ComposePane {
    fn render_scope(&self, cx: &App) -> impl IntoElement {
        let scope = self.presentation.scope;
        let focus = self.input.read(cx).focus_handle(cx);
        Button::new("compose-scope")
            .small()
            .ghost()
            .label(scope_label(scope))
            .icon(IconName::ChevronDown)
            .accessibility_label(t!(
                "workspace.compose.scope_label",
                scope = scope_label(scope)
            ))
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.action_context(focus.clone());
                for choice in [
                    ComposeScope::Current,
                    ComposeScope::Window,
                    ComposeScope::Selected,
                ] {
                    menu = menu.menu_with_check(
                        scope_label(choice),
                        scope == choice,
                        Box::new(SetComposeScope::new(choice)),
                    );
                }
                menu
            })
    }
    fn render_toolbar(&self, cx: &App) -> impl IntoElement {
        h_flex()
            .id("compose-toolbar")
            .test_support()
            .flex_shrink_0()
            .flex_wrap()
            .gap_1()
            .child(
                h_flex()
                    .flex_1()
                    .flex_basis(rems(18.))
                    .min_w_0()
                    .gap_1()
                    .child(self.render_scope(cx))
                    .child(self.render_summary(cx)),
            )
            .child(self.render_controls(cx))
    }
    fn render_summary(&self, cx: &App) -> impl IntoElement {
        let p = &self.presentation;
        let tooltip = if p.names.is_empty() {
            p.summary.clone()
        } else {
            p.names.clone()
        };
        h_flex()
            .gap_1()
            .min_w_0()
            .flex_1()
            .text_color(if p.count > 1 || p.keys {
                cx.theme().warning
            } else {
                cx.theme().muted_foreground
            })
            .when(p.count > 1, |row| {
                row.child(Icon::new(IconName::TriangleAlert).small())
            })
            .child(
                div()
                    .id("compose-pane-target")
                    .test_support()
                    .aria_label(p.summary.clone())
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .child(p.summary.clone())
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
            )
    }
    fn render_controls(&self, cx: &App) -> impl IntoElement {
        let groups = Rc::new(self.snippet_groups(cx));
        let p = &self.presentation;
        let no_snippets = groups.is_empty();
        let input_focus = self.input.read(cx).focus_handle(cx);
        let send = |action: &'static dyn Action| {
            let dispatch = self.dispatch.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                dispatch.dispatch_action(action, window, cx)
            }
        };
        let keys_dispatch = self.dispatch.clone();
        let blank = p.scope != ComposeScope::Current && self.text(cx).trim().is_empty();
        let close = self.dispatch.clone();
        let key_tip = if p.targets_changed {
            t!("workspace.compose.keys_changed")
        } else {
            t!("workspace.compose.keys_tooltip")
        };
        h_flex()
            .gap_1()
            .flex_shrink_0()
            .child(
                Button::new("compose-snippets")
                    .small()
                    .ghost()
                    .icon(Icon::new(CatalogIcon::CodeXml))
                    .label(t!("workspace.compose.snippets"))
                    .tooltip(if no_snippets {
                        t!("workspace.compose.no_snippets")
                    } else {
                        t!("workspace.compose.snippets_tooltip")
                    })
                    .disabled(no_snippets || !p.available || p.blocked)
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
            .when(p.count >= 2, |row| {
                row.child(
                    div()
                        .id("compose-keys-tip")
                        .flex_shrink_0()
                        .tooltip(move |window, cx| Tooltip::new(key_tip.clone()).build(window, cx))
                        .child(
                            Switch::new("compose-keys")
                                .small()
                                .checked(p.keys)
                                .color(cx.theme().warning)
                                .accessibility_label(if p.keys {
                                    t!("workspace.compose.keys_active", count = p.count)
                                } else {
                                    t!("workspace.compose.keys")
                                })
                                .label(t!("workspace.compose.keys"))
                                .disabled(!p.available)
                                .on_change(move |_, window, cx| {
                                    keys_dispatch.dispatch_action(&ToggleComposeKeySync, window, cx)
                                }),
                        ),
                )
            })
            .child(
                Button::new("compose-interrupt")
                    .small()
                    .ghost()
                    .label(t!("workspace.compose.interrupt"))
                    .tooltip(t!("workspace.compose.interrupt_tooltip", count = p.count))
                    .disabled(!p.available)
                    .on_click(send(&SendComposeInterrupt)),
            )
            .child(
                Button::new("compose-send")
                    .small()
                    .primary()
                    .icon(Icon::new(CatalogIcon::Send))
                    .label(t!("workspace.compose.send"))
                    .tooltip(t!("workspace.compose.send_tooltip"))
                    .disabled(!p.available || p.blocked || blank)
                    .on_click(send(&SendCompose)),
            )
            .child(
                Button::new("compose-close")
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(t!("workspace.compose.close"))
                    .accessibility_label(t!("workspace.compose.close"))
                    .on_click(move |_, window, cx| {
                        close.dispatch_action(&ToggleComposePane, window, cx)
                    }),
            )
    }
    fn render_result(&self) -> impl IntoElement {
        let p = &self.presentation;
        let retry = self.dispatch.clone();
        let edit = self.dispatch.clone();
        let details = p.details.clone();
        let command = p.result_command.clone();
        h_flex()
            .gap_1()
            .min_w_0()
            .flex_shrink_0()
            .when_some(p.result.clone(), |row, result| {
                row.child(
                    div()
                        .id("compose-result")
                        .test_support()
                        .aria_label(result.clone())
                        .min_w_0()
                        .truncate()
                        .child(result),
                )
                .when(!details.is_empty(), |row| {
                    row.child(
                        Popover::new("compose-result-popover")
                            .anchor(Anchor::TopLeft)
                            .trigger(
                                Button::new("compose-result-expand")
                                    .xsmall()
                                    .ghost()
                                    .label(t!("workspace.compose.details")),
                            )
                            .content(move |_, _, cx| {
                                v_flex()
                                    .id("compose-result-details")
                                    .test_support()
                                    .w_80()
                                    .max_h_64()
                                    .overflow_y_scroll()
                                    .gap_1()
                                    .text_sm()
                                    .text_color(cx.theme().foreground)
                                    .when_some(command.clone(), |body, command| {
                                        body.child(div().child(t!(
                                            "workspace.compose.result_command",
                                            command = command
                                        )))
                                    })
                                    .children(
                                        details.iter().cloned().map(|detail| div().child(detail)),
                                    )
                            }),
                    )
                })
                .when(p.blocked, |row| {
                    row.child(
                        Button::new("compose-retry")
                            .xsmall()
                            .ghost()
                            .label(t!("workspace.compose.retry"))
                            .disabled(!p.retry)
                            .on_click(move |_, window, cx| {
                                retry.dispatch_action(&RetryCompose, window, cx)
                            }),
                    )
                    .child(
                        Button::new("compose-edit-new")
                            .xsmall()
                            .ghost()
                            .label(t!("workspace.compose.edit_new"))
                            .on_click(move |_, window, cx| {
                                edit.dispatch_action(&EditComposeAsNew, window, cx)
                            }),
                    )
                })
            })
    }
}
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
        let p = &self.presentation;
        v_flex()
            .id("compose-pane")
            .test_support()
            .size_full()
            .min_h_0()
            .text_sm()
            .bg(cx.theme().background)
            .px_2()
            .py_1()
            .gap_1()
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .id("compose-body")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .child(
                        Textarea::new(&self.input)
                            .h(relative(1.))
                            .text_sm()
                            .readonly(p.blocked)
                            .aria_label(t!("workspace.compose.title"))
                            .font_family(cx.theme().mono_font_family.clone()),
                    ),
            )
            .when(p.result.is_some(), |pane| pane.child(self.render_result()))
    }
}

#[cfg(test)]
mod tests {
    use super::ComposePane;
    use crate::{app::SendCompose, host::HostStore};
    use gpui_kit::component::{
        Root,
        input::{Enter, InputEvent},
    };
    use gpui_kit::{AppContext, EntityInputHandler, Focusable, TestAppContext, px, size};
    use std::{cell::Cell, rc::Rc};

    #[gpui_kit::test]
    fn compose_ime_confirmation_does_not_dispatch_send(cx: &mut TestAppContext) {
        cx.update(crate::app::init);
        let mut pane = None;
        let handle = cx.open_window(size(px(800.), px(400.)), |window, cx| {
            let dispatch = cx.focus_handle();
            let view =
                cx.new(|cx| ComposePane::new(cx.new(|_| HostStore::empty()), dispatch, window, cx));
            pane = Some(view.clone());
            Root::new(view, window, cx)
        });
        let pane = pane.unwrap();
        let sent = Rc::new(Cell::new(0));
        let count = sent.clone();
        cx.update(|cx| {
            cx.on_action(move |_: &SendCompose, _| count.set(count.get() + 1));
        });
        cx.update_window(handle.into(), |_, window, cx| {
            let input = pane.read(cx).input.clone();
            window.focus(&input.read(cx).focus_handle(cx), cx);
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "中文", Some(0..2), window, cx)
            });
            input.update(cx, |_, cx| {
                cx.emit(InputEvent::PressEnter {
                    shift: false,
                    secondary: false,
                })
            });
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(sent.get(), 0);
        cx.update_window(handle.into(), |_, window, cx| {
            let input = pane.read(cx).input.clone();
            input.update(cx, |input, cx| input.unmark_text(window, cx));
            window.dispatch_action(
                Box::new(Enter {
                    shift: false,
                    secondary: false,
                }),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(sent.get(), 1);
    }
}
