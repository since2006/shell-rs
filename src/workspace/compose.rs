//! 撰写窗格 in the workspace: the bottom dock holds it while it shows, and
//! what it holds goes where `Sending` says: the current terminal, the one
//! the right sidebar goes with, or several (命令广播).

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dock::{DockLayout, DockPlacement, panel_handle};
use gpui_kit::component::notification::Notification;
use gpui_kit::*;

use super::Workspace;
use super::compose_pane::ComposeTarget;
use super::sending::Recipient;
use crate::app::{
    SendCompose, SendComposeInterrupt, SendComposeSnippet, SendTarget, ToggleComposePane,
};
use crate::i18n::{join_list, t};
use crate::terminal::{ComposePaneShown, TerminalInput};

/// The lowest it can be dragged, title bar included: its title bar and a
/// line or two, and the least a dock is kept at anyway. Dock geometry is an
/// API boundary that takes `Pixels`.
const COMPOSE_MIN_HEIGHT: Pixels = px(100.);
/// How tall it opens until dragged: as low as it goes, out of the
/// terminals' way; dragged taller, it opens so from then on.
const COMPOSE_HEIGHT: Pixels = COMPOSE_MIN_HEIGHT;
/// How long a dragged height settles before the settings file is written.
const HEIGHT_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// How tall the pane opens: as last dragged, kept in the settings, but
/// never below its lowest.
pub(super) fn compose_height(saved: Option<f32>) -> Pixels {
    saved.map_or(COMPOSE_HEIGHT, px).max(COMPOSE_MIN_HEIGHT)
}

impl Workspace {
    /// Where the pane sends.
    fn compose_recipients(&self, cx: &App) -> Vec<Recipient> {
        self.send_panes(cx)
            .into_iter()
            .filter_map(|pane| self.recipient(pane, cx))
            .collect()
    }

    /// Where the pane sends, as its title tells: the current terminal by
    /// name, none without a terminal on screen; or how many it broadcasts
    /// to.
    fn compose_target(&self, cx: &App) -> Option<ComposeTarget> {
        match self.sending.target() {
            SendTarget::Current => {
                let target = self.tool_target(cx)?;
                let recipient = self.recipient(target.tab(), cx)?;
                Some(ComposeTarget::Terminal(recipient.name))
            }
            SendTarget::Tab => Some(ComposeTarget::Tab(self.send_panes(cx).len())),
            SendTarget::Selected => Some(ComposeTarget::Selected(self.send_panes(cx).len())),
        }
    }

    /// Show the pane while it is switched on and has somewhere to send;
    /// tell it where it sends.
    pub(super) fn sync_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.compose_target(cx);
        let show = self.settings().read(cx).settings().compose_pane && target.is_some();
        if let Some(target) = target {
            let keys = self.sending.keys();
            self.compose
                .update(cx, |pane, cx| pane.set_target(target, keys, cx));
        }
        cx.set_global(ComposePaneShown(show));
        if self.dock_area.read(cx).has_dock(DockPlacement::Bottom) == show {
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
                // Its × and the shortcut are its controls, not gpui-kit's
                // collapse button.
                area.set_dock_collapsible(DockPlacement::Bottom, false, window, cx);
            });
        } else {
            // The dock goes away, and the pane with it: what it holds stays
            // for next time, and so does how tall it was.
            let focus_inside = self
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
            if focus_inside {
                self.focus_center(window, cx);
            }
        }
    }

    /// Undo a drag that took the pane below its lowest, and keep the
    /// height it was dragged to for the next launch, once the drag
    /// settles. Runs whenever the dock area changes, as the right
    /// sidebar's width is held.
    pub(super) fn hold_compose_height(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(height) = self.dock_area.read(cx).dock_size(DockPlacement::Bottom) else {
            return;
        };
        if height < COMPOSE_MIN_HEIGHT {
            self.dock_area.update(cx, |area, cx| {
                area.set_dock_size(DockPlacement::Bottom, COMPOSE_MIN_HEIGHT, window, cx);
            });
            return;
        }
        if height == self.compose_height {
            return;
        }
        self.compose_height = height;
        let settings = self.settings().clone();
        self.compose_height_save = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(HEIGHT_SAVE_DELAY).await;
            settings.update(cx, |settings, cx| {
                settings.update(|s| s.compose_height = Some(f32::from(height)), cx)
            });
        }));
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

    /// Put `command` on the input line of each terminal the pane sends to
    /// and with `run` run it. After whatever is typed there, not in its
    /// place as 命令片段 does: as Xshell's and SecureCRT's send to all, and
    /// clearing with Ctrl-E Ctrl-U rings bash's bell on an empty line and
    /// means other things to network devices. Says which terminals could not
    /// take it, and returns whether any did.
    fn send_composed(
        &mut self,
        command: &str,
        run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.sync_sending(cx);
        let mut sent = false;
        // Why not, with the terminals it applies to, in the order met.
        let mut refused: Vec<(SharedString, Vec<SharedString>)> = Vec::new();
        for recipient in self.compose_recipients(cx) {
            match recipient
                .view
                .update(cx, |view, cx| view.enter_command(command, run, false, cx))
            {
                Ok(()) => sent = true,
                Err(why) => match refused.iter_mut().find(|(reason, _)| *reason == why) {
                    Some((_, names)) => names.push(recipient.name),
                    None => refused.push((why, vec![recipient.name])),
                },
            }
        }
        for (why, names) in refused {
            let message = t!(
                "workspace.compose.refused",
                names = join_list(&names),
                error = why
            );
            window.push_notification(Notification::error(message), cx);
        }
        sent
    }

    /// Show or hide the pane, whatever it sends to. Hidden, it goes back to
    /// the current terminal, keys unshared: it opens again sending to one
    /// terminal, never to several left chosen and forgotten.
    pub(super) fn on_toggle_compose_pane(
        &mut self,
        _: &ToggleComposePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Nothing to write to: no terminal on screen.
        if self.compose_target(cx).is_none() {
            return;
        }
        let open = !self.settings().read(cx).settings().compose_pane;
        self.settings().update(cx, |settings, cx| {
            settings.update(|s| s.compose_pane = open, cx)
        });
        if !open {
            self.sending.send_to_current();
            self.sync_sending(cx);
        }
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
        // A last line ended would run once more, empty.
        let command = text.trim_end_matches(['\r', '\n']);
        // What could not be sent anywhere stays, to send again.
        if self.send_composed(command, true, window, cx) {
            self.compose.update(cx, |pane, cx| pane.clear(window, cx));
        }
    }

    pub(super) fn on_send_compose_snippet(
        &mut self,
        action: &SendComposeSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(snippet) = self.store.read(cx).snippet(action.0).cloned() else {
            return;
        };
        self.send_composed(&snippet.command, snippet.run_on_click, window, cx);
    }

    pub(super) fn on_send_compose_interrupt(
        &mut self,
        _: &SendComposeInterrupt,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_sending(cx);
        for recipient in self.compose_recipients(cx) {
            recipient.view.update(cx, |view, cx| {
                view.receive_input(&TerminalInput::Bytes(vec![0x03]), cx);
            });
        }
    }
}
