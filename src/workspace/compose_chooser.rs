//! 选定的终端…: the terminals open now, tab by tab as the tab bar has them,
//! each checked to send 撰写窗格 to it. A split tab is listed by its name
//! with its terminals under it, and its own box checks them all; a
//! terminal not connected is listed, greyed with why, and cannot be
//! checked. While it is open, the list follows the terminals. A search
//! narrows it; 全选 checks what is listed.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::sending::{Candidate, ChooserTab};
use crate::app::{PaneContent, SetComposeTerminals};
use crate::i18n::t;
use crate::shared::commit_footer;

pub(super) struct ComposeChooser {
    tabs: Vec<ChooserTab>,
    checked: Vec<PaneContent>,
    search: Entity<InputState>,
    _subscription: Subscription,
}

impl ComposeChooser {
    /// The terminals as they are now. A terminal no longer connected, or
    /// gone, is no longer checked.
    pub(super) fn set_tabs(&mut self, tabs: Vec<ChooserTab>, cx: &mut Context<Self>) {
        if self.tabs == tabs {
            return;
        }
        self.tabs = tabs;
        self.keep_checked_live();
        cx.notify();
    }

    fn keep_checked_live(&mut self) {
        let live: Vec<PaneContent> = self.candidates().map(|candidate| candidate.pane).collect();
        self.checked.retain(|pane| live.contains(pane));
    }

    /// The terminals that take commands, in the order listed.
    fn candidates(&self) -> impl Iterator<Item = &Candidate> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.terminals.iter())
            .filter(|candidate| candidate.not_live.is_none())
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    /// The tabs the search leaves, each with the terminals it leaves: a
    /// tab whose name matches keeps all of its own.
    fn listed(&self, cx: &App) -> Vec<ChooserTab> {
        let query = self.query(cx);
        self.tabs
            .iter()
            .filter_map(|tab| {
                if query.is_empty() || tab.label.to_lowercase().contains(&query) {
                    return Some(tab.clone());
                }
                let terminals: Vec<Candidate> = tab
                    .terminals
                    .iter()
                    .filter(|candidate| candidate.name.to_lowercase().contains(&query))
                    .cloned()
                    .collect();
                (!terminals.is_empty()).then(|| ChooserTab {
                    terminals,
                    ..tab.clone()
                })
            })
            .collect()
    }

    /// The panes among `terminals` that can be checked.
    fn checkable(terminals: &[Candidate]) -> Vec<PaneContent> {
        terminals
            .iter()
            .filter(|candidate| candidate.not_live.is_none())
            .map(|candidate| candidate.pane)
            .collect()
    }

    fn all_checked(&self, panes: &[PaneContent]) -> bool {
        !panes.is_empty() && panes.iter().all(|pane| self.checked.contains(pane))
    }

    fn set(&mut self, panes: &[PaneContent], checked: bool, cx: &mut Context<Self>) {
        for pane in panes {
            self.checked.retain(|checked| checked != pane);
            if checked {
                self.checked.push(*pane);
            }
        }
        cx.notify();
    }

    /// The checked terminals in the order listed.
    fn result(&self) -> Vec<PaneContent> {
        self.candidates()
            .map(|candidate| candidate.pane)
            .filter(|pane| self.checked.contains(pane))
            .collect()
    }

    fn render_terminal(
        &self,
        candidate: &Candidate,
        indented: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pane = candidate.pane;
        let id = match pane {
            PaneContent::LocalTerminal(id) => ("compose-choose-local", id.0),
            PaneContent::Terminal(id) => ("compose-choose-remote", id.0),
            _ => unreachable!("only terminals are listed"),
        };
        h_flex()
            .gap_2()
            .when(indented, |row| row.pl_6())
            .child(
                Checkbox::new(id)
                    .checked(self.checked.contains(&pane))
                    .disabled(candidate.not_live.is_some())
                    .label(candidate.name.clone())
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set(&[pane], *checked, cx)
                    })),
            )
            .children(candidate.not_live.clone().map(|why| {
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(why)
            }))
            .into_any_element()
    }
}

impl Render for ComposeChooser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let listed = self.listed(cx);
        let everything: Vec<PaneContent> = listed
            .iter()
            .flat_map(|tab| Self::checkable(&tab.terminals))
            .collect();
        let all = self.all_checked(&everything);
        let nothing_open = self.tabs.is_empty();
        let rows: Vec<AnyElement> = listed
            .iter()
            .map(|tab| {
                if !tab.split {
                    let candidate = &tab.terminals[0];
                    return self
                        .render_terminal(candidate, false, cx)
                        .into_any_element();
                }
                let panes = Self::checkable(&tab.terminals);
                let checked = self.all_checked(&panes);
                v_flex()
                    .gap_1()
                    .child(
                        Checkbox::new(("compose-choose-tab", tab.tab.0))
                            .checked(checked)
                            .disabled(panes.is_empty())
                            .label(tab.label.clone())
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                this.set(&panes, *checked, cx)
                            })),
                    )
                    .children({
                        let mut rows = Vec::new();
                        for candidate in &tab.terminals {
                            rows.push(self.render_terminal(candidate, true, cx));
                        }
                        rows
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .id("compose-chooser")
            .gap_3()
            .child(
                Input::new(&self.search)
                    .id("compose-chooser-search")
                    .small(),
            )
            .child(
                Checkbox::new("compose-choose-all")
                    .checked(all)
                    .disabled(everything.is_empty())
                    .label(t!("workspace.compose.choose.all"))
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set(&everything, *checked, cx)
                    })),
            )
            .child(
                v_flex()
                    .id("compose-chooser-list")
                    .max_h(rems(22.))
                    .overflow_y_scroll()
                    .gap_2()
                    .when(rows.is_empty(), |list| {
                        list.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if nothing_open {
                                    t!("workspace.compose.choose.none")
                                } else {
                                    t!("workspace.compose.choose.no_match")
                                }),
                        )
                    })
                    .children(rows),
            )
    }
}

/// Open the chooser over the open terminals, `checked` checked. What it
/// leaves goes to the workspace through `target`. The workspace keeps the
/// chooser to give it the terminals as they change.
pub(super) fn open(
    tabs: Vec<ChooserTab>,
    checked: Vec<PaneContent>,
    target: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ComposeChooser> {
    let chooser = cx.new(|cx| {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(t!("workspace.compose.choose.search"))
        });
        let subscription = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut chooser = ComposeChooser {
            tabs,
            checked,
            search,
            _subscription: subscription,
        };
        chooser.keep_checked_live();
        chooser
    });
    let dialog_chooser = chooser.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("workspace.compose.choose.title"))
            .overlay_closable(false)
            .child(dialog_chooser.clone())
            .footer(commit_footer(
                "compose-choose-ok",
                t!("workspace.compose.choose.ok"),
            ))
            .on_ok({
                let chooser = dialog_chooser.clone();
                let target = target.clone();
                move |_, window, cx| {
                    let panes = chooser.read(cx).result();
                    let target = target.clone();
                    window.defer(cx, move |window, cx| {
                        target.dispatch_action(&SetComposeTerminals(panes), window, cx)
                    });
                    true
                }
            })
    });
    chooser
}
