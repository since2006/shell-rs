//! Where 撰写窗格 sends: the current terminal, the connected terminals of
//! the tab in front, or terminals chosen by hand. To more than one it is
//! 命令广播, whose command bar the pane is; where the terminals are laid
//! out does not change for it. Each chosen terminal is chosen as the
//! connection it has: closed, disconnected or reconnected, it is no longer
//! chosen.

use gpui_kit::component::dock::DockPlacement;
use gpui_kit::*;

use super::Workspace;
use crate::app::{
    ChooseComposeTerminals, PaneContent, SendTarget, SetComposeTerminals, SetSendTarget,
    ToggleBroadcastKeys,
};
use crate::center::{LeadingControls, TabId};
use crate::shared::RenamableTab as _;
use crate::terminal::{TerminalInputEvent, TerminalView};

/// A terminal as one connection: the pane, and its connection's generation.
type Connection = (PaneContent, u64);

/// Where 撰写窗格 sends, and how.
#[derive(Default)]
pub(super) struct Sending {
    target: SendTarget,
    /// The terminals chosen for `SendTarget::Selected`, in the order shown.
    chosen: Vec<Connection>,
    /// Typing in one terminal it broadcasts to goes to the others as well.
    /// Off, only the pane broadcasts, so a password typed at one prompt
    /// stays there.
    keys: bool,
}

impl Sending {
    pub(super) fn target(&self) -> SendTarget {
        self.target
    }

    /// To more than the current terminal: 命令广播.
    pub(super) fn broadcasting(&self) -> bool {
        self.target != SendTarget::Current
    }

    pub(super) fn keys(&self) -> bool {
        self.keys
    }

    /// Back to the current terminal alone, keys unshared, as at launch.
    pub(super) fn send_to_current(&mut self) {
        *self = Self::default();
    }

    /// Forget the chosen terminals no longer connected as they were; with
    /// none left, send to the current terminal again.
    fn reconcile(&mut self, live: &[Connection]) {
        self.chosen.retain(|chosen| live.contains(chosen));
        if self.target == SendTarget::Selected && self.chosen.is_empty() {
            self.target = SendTarget::Current;
        }
    }

    fn choose(&mut self, panes: &[PaneContent], live: &[Connection]) {
        self.chosen = panes
            .iter()
            .filter_map(|pane| live.iter().find(|(live, _)| live == pane).copied())
            .collect();
        self.target = if self.chosen.is_empty() {
            SendTarget::Current
        } else {
            SendTarget::Selected
        };
    }

    /// The panes it sends to: `current`, the connected terminals among
    /// `tab`'s panes, or the chosen ones.
    fn recipients(
        &self,
        current: Option<PaneContent>,
        tab: &[PaneContent],
        live: &[Connection],
    ) -> Vec<PaneContent> {
        match self.target {
            SendTarget::Current => current.into_iter().collect(),
            SendTarget::Tab => tab
                .iter()
                .copied()
                .filter(|pane| live.iter().any(|(live, _)| live == pane))
                .collect(),
            SendTarget::Selected => self.chosen.iter().map(|(pane, _)| *pane).collect(),
        }
    }
}

/// A terminal as 撰写窗格 sends to it.
pub(super) struct Recipient {
    pub(super) view: Entity<TerminalView>,
    pub(super) name: SharedString,
}

/// An open terminal, as the chooser lists it.
#[derive(Clone, PartialEq)]
pub(super) struct Candidate {
    pub(super) pane: PaneContent,
    pub(super) name: SharedString,
    /// What keeps it from taking commands, while it is not connected.
    pub(super) not_live: Option<SharedString>,
}

/// A tab with terminals, as the chooser lists it: in the tab bar's order,
/// a split tab named, its terminals under it.
#[derive(Clone, PartialEq)]
pub(super) struct ChooserTab {
    pub(super) tab: TabId,
    pub(super) label: SharedString,
    /// It holds several panes: its terminals are listed under its name.
    pub(super) split: bool,
    pub(super) terminals: Vec<Candidate>,
}

impl Workspace {
    fn pane_terminal_view(&self, pane: PaneContent, cx: &App) -> Option<Entity<TerminalView>> {
        match pane {
            PaneContent::Terminal(id) => Some(self.terminals.get(&id)?.read(cx).terminal().clone()),
            PaneContent::LocalTerminal(id) => {
                Some(self.local_terminals.get(&id)?.read(cx).terminal().clone())
            }
            _ => None,
        }
    }

    /// The terminals that take input, each as its connection, tab by tab.
    fn live_terminals(&self, cx: &App) -> Vec<Connection> {
        self.center
            .read(cx)
            .all_panes()
            .into_iter()
            .filter_map(|pane| {
                let view = self.pane_terminal_view(pane, cx)?;
                let view = view.read(cx);
                view.lifecycle(cx)
                    .accepts_input()
                    .then(|| (pane, view.generation(cx)))
            })
            .collect()
    }

    /// A terminal, by its view and the name its tab shows.
    pub(super) fn recipient(&self, pane: PaneContent, cx: &App) -> Option<Recipient> {
        match pane {
            PaneContent::Terminal(id) => {
                let panel = self.terminals.get(&id)?.read(cx);
                Some(Recipient {
                    view: panel.terminal().clone(),
                    name: panel.tab_title(cx),
                })
            }
            PaneContent::LocalTerminal(id) => {
                let panel = self.local_terminals.get(&id)?.read(cx);
                Some(Recipient {
                    view: panel.terminal().clone(),
                    name: panel.display_title(cx),
                })
            }
            _ => None,
        }
    }

    /// The panes 撰写窗格 sends to now.
    pub(super) fn send_panes(&self, cx: &App) -> Vec<PaneContent> {
        let center = self.center.read(cx);
        let tab = center
            .active_tab()
            .map(|tab| center.panes(tab).to_vec())
            .unwrap_or_default();
        let current = self.tool_target(cx).map(|target| target.tab());
        self.sending
            .recipients(current, &tab, &self.live_terminals(cx))
    }

    /// The open terminals, tab by tab, for the chooser.
    pub(super) fn chooser_tabs(&self, cx: &App) -> Vec<ChooserTab> {
        let center = self.center.read(cx);
        center
            .tab_ids()
            .into_iter()
            .filter_map(|tab| {
                let panes = center.panes(tab);
                let terminals: Vec<Candidate> = panes
                    .iter()
                    .filter_map(|&pane| {
                        let name = self.recipient(pane, cx)?.name;
                        let view = self.pane_terminal_view(pane, cx)?;
                        let lifecycle = view.read(cx).lifecycle(cx);
                        Some(Candidate {
                            pane,
                            name,
                            not_live: (!lifecycle.accepts_input()).then(|| lifecycle.label()),
                        })
                    })
                    .collect();
                (!terminals.is_empty()).then(|| ChooserTab {
                    tab,
                    label: center.tab_label(tab, cx),
                    split: panes.len() > 1,
                    terminals,
                })
            })
            .collect()
    }

    /// The terminals the chooser starts out checked: those chosen, or
    /// while it sends elsewhere, those it sends to now.
    pub(super) fn chooser_checked(&self, cx: &App) -> Vec<PaneContent> {
        match self.sending.target {
            SendTarget::Selected => self.sending.chosen.iter().map(|(pane, _)| *pane).collect(),
            _ => self.send_panes(cx),
        }
    }

    /// Keep the sending in step with the terminals, and the tab bar's
    /// controls with the sidebars and 撰写窗格.
    pub(super) fn sync_sending(&mut self, cx: &mut Context<Self>) {
        let live = self.live_terminals(cx);
        self.sending.reconcile(&live);
        self.sync_tab_bar_controls(cx);
        // The chooser, while open, lists the terminals as they are.
        if let Some(chooser) = self.compose_chooser.as_ref().and_then(WeakEntity::upgrade) {
            let tabs = self.chooser_tabs(cx);
            chooser.update(cx, |chooser, cx| chooser.set_tabs(tabs, cx));
        }
    }

    pub(super) fn sync_tab_bar_controls(&mut self, cx: &mut Context<Self>) {
        let area = self.dock_area.read(cx);
        let controls = LeadingControls {
            hosts_open: area
                .is_dock_collapsible(DockPlacement::Left)
                .then(|| area.is_dock_open(DockPlacement::Left)),
            compose_shown: area.has_dock(DockPlacement::Bottom),
        };
        self.center
            .update(cx, |center, cx| center.set_controls(controls, cx));
    }

    /// What was typed in one terminal, to the others 撰写窗格 broadcasts
    /// to, when keys are shared and it is one of them. Only while the pane
    /// shows: keys must not go elsewhere unseen, a password least of all.
    pub(super) fn broadcast_input(
        &mut self,
        source: PaneContent,
        event: &TerminalInputEvent,
        cx: &mut Context<Self>,
    ) {
        // Every keystroke comes through here: the cheap checks first.
        if !self.sending.broadcasting()
            || !self.sending.keys
            || !self.dock_area.read(cx).has_dock(DockPlacement::Bottom)
        {
            return;
        }
        let live = self.live_terminals(cx);
        if !live.contains(&(source, event.generation())) {
            return;
        }
        let panes = self.send_panes(cx);
        if !panes.contains(&source) {
            return;
        }
        // receive_input never emits Input, so recipients cannot form a loop.
        let recipients: Vec<_> = panes
            .into_iter()
            .filter(|pane| *pane != source)
            .filter_map(|pane| self.pane_terminal_view(pane, cx))
            .collect();
        for view in recipients {
            view.update(cx, |view, cx| view.receive_input(event.input(), cx));
        }
    }

    pub(super) fn on_set_send_target(
        &mut self,
        action: &SetSendTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action.0 == SendTarget::Selected && self.sending.chosen.is_empty() {
            self.on_choose_compose_terminals(&ChooseComposeTerminals, window, cx);
            return;
        }
        self.sending.target = action.0;
        self.after_sending_changed(window, cx);
    }

    pub(super) fn on_choose_compose_terminals(
        &mut self,
        _: &ChooseComposeTerminals,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.chooser_tabs(cx);
        let checked = self.chooser_checked(cx);
        let chooser =
            super::compose_chooser::open(tabs, checked, self.focus_handle.clone(), window, cx);
        self.compose_chooser = Some(chooser.downgrade());
    }

    pub(super) fn on_set_compose_terminals(
        &mut self,
        action: &SetComposeTerminals,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let live = self.live_terminals(cx);
        self.sending.choose(&action.0, &live);
        self.after_sending_changed(window, cx);
    }

    pub(super) fn on_toggle_broadcast_keys(
        &mut self,
        _: &ToggleBroadcastKeys,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sending.keys = !self.sending.keys;
        self.sync_compose(window, cx);
    }

    /// Sending to several, the pane is their command bar: it shows, and
    /// has the keyboard to take the command. Back to the current terminal,
    /// the keyboard goes back there.
    fn after_sending_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_sending(cx);
        if self.sending.broadcasting() && !self.settings().read(cx).settings().compose_pane {
            self.settings().update(cx, |settings, cx| {
                settings.update(|s| s.compose_pane = true, cx)
            });
        }
        self.sync_compose(window, cx);
        if self.sending.broadcasting() {
            self.focus_compose_soon(window, cx);
        } else {
            self.focus_center(window, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{SendTarget, Sending};
    use crate::{
        app::PaneContent,
        terminal::{LocalTerminalId, RemoteTerminalId},
    };

    const A: PaneContent = PaneContent::Terminal(RemoteTerminalId(1));
    const B: PaneContent = PaneContent::Terminal(RemoteTerminalId(2));
    const LOCAL: PaneContent = PaneContent::LocalTerminal(LocalTerminalId(1));
    const SFTP: PaneContent = PaneContent::Explorer(crate::explorer::ExplorerId(1));

    #[test]
    fn the_current_terminal_by_default() {
        let sending = Sending::default();
        assert!(!sending.broadcasting());
        assert_eq!(sending.recipients(Some(A), &[A, B], &[(A, 1), (B, 1)]), [A]);
        assert!(sending.recipients(None, &[A], &[(A, 1)]).is_empty());
    }

    #[test]
    fn the_tabs_connected_terminals_in_reading_order() {
        let sending = Sending {
            target: SendTarget::Tab,
            ..Sending::default()
        };
        assert!(sending.broadcasting());
        // Not its SFTP pane, not a terminal that is not connected.
        let tab = [B, SFTP, A, LOCAL];
        assert_eq!(
            sending.recipients(Some(A), &tab, &[(A, 1), (LOCAL, 3)]),
            [A, LOCAL]
        );
    }

    #[test]
    fn chosen_terminals_are_chosen_as_their_connections() {
        let mut sending = Sending::default();
        sending.choose(&[B, A, SFTP], &[(A, 1), (B, 1)]);
        assert_eq!(sending.target(), SendTarget::Selected);
        assert_eq!(sending.recipients(None, &[], &[]), [B, A]);
        // B reconnected: no longer chosen.
        sending.reconcile(&[(A, 1), (B, 2)]);
        assert_eq!(sending.recipients(None, &[], &[]), [A]);
        // A closed: none left, back to the current terminal.
        sending.reconcile(&[(B, 2)]);
        assert_eq!(sending.target(), SendTarget::Current);
    }

    #[test]
    fn choosing_none_goes_back_to_the_current_terminal() {
        let mut sending = Sending {
            target: SendTarget::Selected,
            chosen: vec![(A, 1)],
            keys: true,
        };
        sending.choose(&[], &[(A, 1)]);
        assert_eq!(sending.target(), SendTarget::Current);
        assert!(sending.keys());
    }
}
