//! Commands are submitted against a fixed identity snapshot, with explicit
//! per-target results. A partial report blocks ordinary submission until resolved.
use super::{
    Workspace,
    compose_pane::ComposePresentation,
    terminal_groups::{ComposeIdentity, Recipient},
};
use crate::app::{
    CenterTab, ComposeScope, EditComposeAsNew, RetryCompose, SendCompose, SendComposeInterrupt,
    SendComposeSnippet, ToggleComposePane,
};
use crate::i18n::{join_list, t};
use crate::terminal::{ComposePaneShown, TerminalInput};
use gpui_kit::component::dock::{DockLayout, DockPlacement, panel_handle};
use gpui_kit::*;

// Dock size APIs take Pixels. Minimum accommodates the header, editor and actions.
pub(super) const COMPOSE_HEIGHT: Pixels = px(104.);
const COMPOSE_MIN_HEIGHT: Pixels = px(96.);

pub(super) struct ComposeOutcome {
    identity: ComposeIdentity,
    name: SharedString,
    /// None means accepted by the terminal input path, not remotely executed.
    refusal: Option<SharedString>,
}

pub(super) struct ComposeReport {
    command: String,
    run: bool,
    /// Snippets leave an unrelated draft intact, including after a retry.
    clears_draft: bool,
    outcomes: Vec<ComposeOutcome>,
}
impl ComposeReport {
    fn submitted(&self) -> usize {
        self.outcomes.iter().filter(|r| r.refusal.is_none()).count()
    }
    fn partial(&self) -> bool {
        self.submitted() > 0 && self.submitted() < self.outcomes.len()
    }
    fn complete(&self) -> bool {
        !self.outcomes.is_empty() && self.submitted() == self.outcomes.len()
    }
    fn summary(&self) -> SharedString {
        t!(
            "workspace.compose.result",
            submitted = self.submitted(),
            refused = self.outcomes.len() - self.submitted()
        )
    }
    fn details(&self) -> Vec<SharedString> {
        self.outcomes
            .iter()
            .map(|outcome| match &outcome.refusal {
                Some(reason) => t!(
                    "workspace.compose.refused",
                    names = &outcome.name,
                    error = reason
                ),
                None => t!("workspace.compose.submitted_target", name = &outcome.name),
            })
            .collect()
    }
}

impl Workspace {
    pub(super) fn compose_presentation(&self, cx: &App) -> ComposePresentation {
        let recipients = self.compose_recipients(cx);
        let count = recipients.len();
        let local = recipients
            .iter()
            .filter(|r| matches!(r.identity.tab, CenterTab::LocalTerminal(_)))
            .count();
        let summary = match count {
            0 if self.compose_routing.scope == ComposeScope::Selected => {
                t!("workspace.compose.choose")
            }
            0 => t!("workspace.compose.empty"),
            1 if local == 1 => t!("workspace.compose.single_local", name = &recipients[0].name),
            1 => recipients[0].name.clone(),
            _ if local > 0 => t!(
                "workspace.compose.multiple_local",
                count = count,
                local = local
            ),
            _ => t!("workspace.compose.multiple", count = count),
        };
        let names: Vec<_> = recipients.iter().map(|r| r.name.clone()).collect();
        let report = self.compose_report.as_ref();
        let blocked = report.is_some_and(ComposeReport::partial);
        ComposePresentation {
            scope: self.compose_routing.scope,
            summary,
            names: join_list(&names).into(),
            count,
            keys: self.compose_routing.keys,
            targets_changed: self.compose_routing.targets_changed,
            available: count > 0 && !crate::app_lock::is_locked(cx),
            blocked,
            result: report
                .filter(|r| !r.complete() || r.outcomes.len() > 1)
                .map(ComposeReport::summary),
            result_command: report
                .filter(|r| !r.complete())
                .map(|r| r.command.clone().into()),
            details: report
                .filter(|r| !r.complete())
                .map(ComposeReport::details)
                .unwrap_or_default(),
            retry: report.is_some_and(|r| {
                r.partial()
                    && r.outcomes.iter().any(|outcome| {
                        outcome.refusal.is_some()
                            && self
                                .recipient(outcome.identity.tab, cx)
                                .is_some_and(|recipient| {
                                    recipient.identity == outcome.identity
                                        && recipient.view.read(cx).lifecycle(cx).accepts_input()
                                })
                    })
            }) && !crate::app_lock::is_locked(cx),
        }
    }

    /// A requested pane remains visible even with no available targets.
    pub(super) fn sync_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_compose_routing(cx);
        let show = self.settings().read(cx).settings().compose_pane;
        cx.set_global(ComposePaneShown(show));
        if self.dock_area.read(cx).has_dock(DockPlacement::Bottom) == show {
            self.hold_compose_height(window, cx);
            return;
        }
        if show {
            let pane = panel_handle(self.compose.clone());
            let height = self.compose_height;
            self.dock_area.update(cx, |area, cx| {
                area.set_dock(
                    DockPlacement::Bottom,
                    DockLayout::tabs().panel_view(pane, cx),
                    window,
                    cx,
                );
                area.set_dock_size(DockPlacement::Bottom, height, window, cx);
                area.set_dock_collapsible(DockPlacement::Bottom, false, window, cx);
            });
        } else {
            let focused = self
                .compose
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx);
            if let Some(height) = self.dock_area.read(cx).dock_size(DockPlacement::Bottom) {
                self.compose_height = height;
            }
            self.dock_area.update(cx, |area, cx| {
                area.remove_dock(DockPlacement::Bottom, window, cx)
            });
            if focused {
                self.focus_center(window, cx);
            }
        }
    }
    /// Undo a drag that took the pane below its lowest. Runs whenever the
    /// dock area changes, as the right sidebar's width is held.
    pub(super) fn hold_compose_height(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let area = self.dock_area.read(cx);
        let width = window.viewport_size().width
            - window.rem_size() * 3.
            - [DockPlacement::Left, DockPlacement::Right]
                .into_iter()
                .filter(|side| area.is_dock_open(*side))
                .filter_map(|side| area.dock_size(side))
                .sum::<Pixels>();
        let rows = if width < window.rem_size() * 42. {
            8.
        } else {
            6.
        };
        let result = if self.compose_presentation(cx).result.is_some() {
            1.75
        } else {
            0.
        };
        let minimum = COMPOSE_MIN_HEIGHT.max(window.rem_size() * (rows + result));
        let lower = self
            .dock_area
            .read(cx)
            .dock_size(DockPlacement::Bottom)
            .is_some_and(|height| height < minimum);
        if lower {
            self.dock_area.update(cx, |area, cx| {
                area.set_dock_size(DockPlacement::Bottom, minimum, window, cx);
            });
        }
    }

    /// Type in the pane once it is drawn. A tab the dock moved takes the
    /// focus as it becomes active, which its group tells it from a spawned
    /// task; this one comes after.
    pub(super) fn focus_compose_soon(&self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.compose.clone();
        cx.spawn_in(window, async move |_, cx| {
            _ = cx.update(|window, cx| pane.update(cx, |pane, cx| pane.focus(window, cx)));
        })
        .detach();
    }

    fn submit_composed(
        command: &str,
        run: bool,
        recipient: Recipient,
        cx: &mut App,
    ) -> ComposeOutcome {
        let refusal = recipient
            .view
            .update(cx, |view, cx| view.enter_command(command, run, false, cx))
            .err();
        ComposeOutcome {
            identity: recipient.identity,
            name: recipient.name,
            refusal,
        }
    }

    fn send_composed(
        &mut self,
        command: &str,
        run: bool,
        clears_draft: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::app_lock::is_locked(cx)
            || self
                .compose_report
                .as_ref()
                .is_some_and(ComposeReport::partial)
        {
            return;
        }
        self.sync_compose_routing(cx);
        if self.compose_routing.scope != ComposeScope::Current && command.trim().is_empty() {
            return;
        }
        // Collect once. Creating/reconnecting a terminal cannot enlarge this send.
        let recipients = self.compose_recipients(cx);
        if recipients.is_empty() {
            return;
        }
        let outcomes = recipients
            .into_iter()
            .map(|r| Self::submit_composed(command, run, r, cx))
            .collect();
        self.finish_compose_report(
            ComposeReport {
                command: command.to_owned(),
                run,
                clears_draft,
                outcomes,
            },
            window,
            cx,
        );
    }

    fn finish_compose_report(
        &mut self,
        report: ComposeReport,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if report.complete() && report.clears_draft {
            self.compose.update(cx, |pane, cx| pane.clear(window, cx));
        }
        self.compose_report = Some(report);
        self.sync_compose(window, cx);
        self.focus_compose_soon(window, cx);
    }

    pub(super) fn on_toggle_compose_pane(
        &mut self,
        _: &ToggleComposePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = !self.settings().read(cx).settings().compose_pane;
        self.settings().update(cx, |settings, cx| {
            settings.update(|s| s.compose_pane = open, cx)
        });
        self.sync_compose(window, cx);
        if open {
            self.focus_compose_soon(window, cx);
        } else {
            self.focus_center(window, cx);
        }
        cx.notify();
    }

    pub(super) fn on_send_compose(
        &mut self,
        _: &SendCompose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.compose.read(cx).text(cx);
        self.send_composed(text.trim_end_matches(['\r', '\n']), true, true, window, cx);
    }

    pub(super) fn on_send_compose_snippet(
        &mut self,
        action: &SendComposeSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::app_lock::is_locked(cx)
            || self
                .compose_report
                .as_ref()
                .is_some_and(ComposeReport::partial)
        {
            return;
        }
        let Some(snippet) = self.store.read(cx).snippet(action.0).cloned() else {
            return;
        };
        self.send_composed(&snippet.command, snippet.run_on_click, false, window, cx);
    }

    pub(super) fn on_retry_compose(
        &mut self,
        _: &RetryCompose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::app_lock::is_locked(cx) {
            return;
        }
        let Some(mut report) = self.compose_report.take() else {
            return;
        };
        if !report.partial() {
            self.compose_report = Some(report);
            return;
        }
        for outcome in &mut report.outcomes {
            if outcome.refusal.is_none() {
                continue;
            }
            let recipient = self
                .recipient(outcome.identity.tab, cx)
                .filter(|r| r.identity == outcome.identity);
            outcome.refusal = match recipient {
                Some(recipient) => {
                    Self::submit_composed(&report.command, report.run, recipient, cx).refusal
                }
                None => Some(t!("workspace.compose.connection_changed")),
            };
        }
        self.finish_compose_report(report, window, cx);
    }

    pub(super) fn on_edit_compose_as_new(
        &mut self,
        _: &EditComposeAsNew,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.compose_report = None;
        self.sync_compose(window, cx);
        self.focus_compose_soon(window, cx);
    }

    pub(super) fn on_send_compose_interrupt(
        &mut self,
        _: &SendComposeInterrupt,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::app_lock::is_locked(cx) {
            return;
        }
        self.sync_compose_routing(cx);
        for recipient in self.compose_recipients(cx) {
            recipient.view.update(cx, |view, cx| {
                view.receive_input(&TerminalInput::Bytes(vec![0x03]), cx);
            });
        }
        self.focus_compose_soon(window, cx);
    }
}
