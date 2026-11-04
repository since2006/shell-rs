//! Compose routing is owned by Workspace. Selection never changes Dock layout.
//! Manual membership is a terminal and connection generation, never a host.

use gpui_kit::component::dock::{DockPlacement, PaneRef};
use gpui_kit::*;

use super::Workspace;
use crate::app::{
    CenterTab, ComposeScope, SetComposeScope, SetComposeTarget, ToggleComposeKeySync,
};
use crate::shared::RenamableTab as _;
use crate::terminal::{ComposeSelection, TerminalInputEvent, TerminalView};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ComposeIdentity {
    pub(super) tab: CenterTab,
    pub(super) generation: u64,
}

#[derive(Default)]
pub(super) struct ComposeRouting {
    pub(super) scope: ComposeScope,
    selected: Vec<ComposeIdentity>,
    pub(super) keys: bool,
    effective: Vec<ComposeIdentity>,
    pub(super) targets_changed: bool,
}

impl ComposeRouting {
    fn reconcile(&mut self, available: &[ComposeIdentity], current: Option<ComposeIdentity>) {
        self.selected.retain(|target| available.contains(target));
        let effective = match self.scope {
            ComposeScope::Current => current
                .into_iter()
                .filter(|id| available.contains(id))
                .collect(),
            ComposeScope::Window => available.to_vec(),
            ComposeScope::Selected => self.selected.clone(),
        };
        // Hash-map iteration, renaming, or dragging cannot change membership.
        let changed = self.effective.len() != effective.len()
            || self.effective.iter().any(|id| !effective.contains(id));
        if changed && self.keys {
            self.keys = false;
            self.targets_changed = true;
        }
        self.effective = effective;
    }

    fn set_scope(&mut self, scope: ComposeScope) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.keys = false;
        self.targets_changed = false;
    }
}

pub(super) struct Recipient {
    pub(super) identity: ComposeIdentity,
    pub(super) view: Entity<TerminalView>,
    pub(super) name: SharedString,
}

impl Workspace {
    fn terminal_view_for_tab(&self, tab: CenterTab, cx: &App) -> Option<Entity<TerminalView>> {
        match tab {
            CenterTab::Terminal(id) => Some(self.terminals.get(&id)?.read(cx).terminal().clone()),
            CenterTab::LocalTerminal(id) => {
                Some(self.local_terminals.get(&id)?.read(cx).terminal().clone())
            }
            _ => None,
        }
    }

    fn compose_candidates(&self, cx: &App) -> Vec<ComposeIdentity> {
        let mut tabs: Vec<_> = self
            .terminals
            .keys()
            .copied()
            .map(CenterTab::Terminal)
            .chain(
                self.local_terminals
                    .keys()
                    .copied()
                    .map(CenterTab::LocalTerminal),
            )
            .collect();
        tabs.sort_by_key(|tab| match tab {
            CenterTab::Terminal(id) => (0, id.0),
            CenterTab::LocalTerminal(id) => (1, id.0),
            _ => unreachable!(),
        });
        tabs.into_iter()
            .filter_map(|tab| {
                let view = self.terminal_view_for_tab(tab, cx)?;
                let view = view.read(cx);
                view.lifecycle(cx).accepts_input().then(|| ComposeIdentity {
                    tab,
                    generation: view.generation(cx),
                })
            })
            .collect()
    }

    pub(super) fn recipient(&self, tab: CenterTab, cx: &App) -> Option<Recipient> {
        let (view, name) = match tab {
            CenterTab::Terminal(id) => {
                let panel = self.terminals.get(&id)?.read(cx);
                (panel.terminal().clone(), panel.tab_title(cx))
            }
            CenterTab::LocalTerminal(id) => {
                let panel = self.local_terminals.get(&id)?.read(cx);
                (panel.terminal().clone(), panel.display_title(cx))
            }
            _ => return None,
        };
        let identity = ComposeIdentity {
            tab,
            generation: view.read(cx).generation(cx),
        };
        Some(Recipient {
            identity,
            view,
            name,
        })
    }

    /// One immutable snapshot serves summary, commands, interrupts and key sync.
    pub(super) fn compose_recipients(&self, cx: &App) -> Vec<Recipient> {
        self.compose_routing
            .effective
            .iter()
            .filter_map(|identity| {
                self.recipient(identity.tab, cx)
                    .filter(|recipient| recipient.identity == *identity)
            })
            .collect()
    }

    pub(super) fn sync_compose_routing(&mut self, cx: &mut Context<Self>) {
        let available = self.compose_candidates(cx);
        let current = self
            .tool_target(cx)
            .and_then(|target| available.iter().find(|id| id.tab == target.tab()).copied());
        self.compose_routing.reconcile(&available, current);
        let requested = self.settings().read(cx).settings().compose_pane;
        // Dock zoom hides the bottom pane too: visible controls must accompany
        // live input forwarding, even while the visibility preference stays on.
        let shown = requested && self.dock_area.read(cx).zoomed_group().is_none();
        if !shown || crate::app_lock::is_locked(cx) {
            self.compose_routing.keys = false;
        }
        let manual = shown && self.compose_routing.scope == ComposeScope::Selected;
        let selection = |tab| {
            let selected = self.compose_routing.selected.iter().any(|id| id.tab == tab);
            manual.then(|| {
                ComposeSelection::new(
                    selected,
                    available.iter().any(|id| id.tab == tab),
                    self.focus_handle.clone(),
                )
            })
        };
        for (id, panel) in &self.terminals {
            let state = selection(CenterTab::Terminal(*id));
            panel.update(cx, |panel, cx| panel.set_compose_selection(state, cx));
        }
        for (id, panel) in &self.local_terminals {
            let state = selection(CenterTab::LocalTerminal(*id));
            panel.update(cx, |panel, cx| panel.set_compose_selection(state, cx));
        }
        let area = self.dock_area.read(cx);
        let mut groups = Vec::new();
        if let Some(tree) = area.layout(DockPlacement::Center) {
            tree.root().walk(&mut |node| {
                if !matches!(node.kind(), PaneRef::Tabs { .. }) {
                    return;
                }
                groups.push(node.id());
            });
        }
        self.skin.set_center_groups(groups, cx);
        let presentation = self.compose_presentation(cx);
        self.compose
            .update(cx, |pane, cx| pane.set_presentation(presentation, cx));
    }

    pub(super) fn sync_compose_input(
        &mut self,
        source: CenterTab,
        event: &TerminalInputEvent,
        cx: &mut Context<Self>,
    ) {
        if !self.compose_routing.keys {
            return;
        }
        self.sync_compose_routing(cx);
        if !self.compose_routing.keys
            || !self.compose_routing.effective.contains(&ComposeIdentity {
                tab: source,
                generation: event.generation(),
            })
        {
            return;
        }
        for recipient in self
            .compose_recipients(cx)
            .into_iter()
            .filter(|r| r.identity.tab != source)
        {
            // receive_input never emits Input: each terminal encodes its own modes once.
            recipient.view.update(cx, |view, cx| {
                view.receive_input(event.input(), cx);
            });
        }
    }

    pub(super) fn on_set_compose_scope(
        &mut self,
        action: &SetComposeScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_compose_routing(cx);
        self.compose_routing.set_scope(action.scope());
        self.sync_compose(window, cx);
    }

    pub(super) fn on_set_compose_target(
        &mut self,
        action: &SetComposeTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.compose_routing.scope != ComposeScope::Selected
            || !self.settings().read(cx).settings().compose_pane
        {
            return;
        }
        self.compose_routing.keys = false;
        self.compose_routing.targets_changed = false;
        self.compose_routing
            .selected
            .retain(|id| id.tab != action.tab());
        if action.is_selected()
            && let Some(id) = self
                .compose_candidates(cx)
                .into_iter()
                .find(|id| id.tab == action.tab())
        {
            self.compose_routing.selected.push(id);
        }
        self.sync_compose(window, cx);
        // No activation, focus change, reconnect or Dock operation here.
    }

    pub(super) fn on_toggle_compose_key_sync(
        &mut self,
        _: &ToggleComposeKeySync,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_compose_routing(cx);
        if self.compose_routing.effective.len() >= 2
            && self.settings().read(cx).settings().compose_pane
            && !crate::app_lock::is_locked(cx)
        {
            self.compose_routing.keys = !self.compose_routing.keys;
            self.compose_routing.targets_changed = false;
        }
        self.sync_compose(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::{ComposeIdentity, ComposeRouting};
    use crate::{
        app::{CenterTab, ComposeScope},
        terminal::RemoteTerminalId,
    };
    fn id(n: u64, generation: u64) -> ComposeIdentity {
        ComposeIdentity {
            tab: CenterTab::Terminal(RemoteTerminalId(n)),
            generation,
        }
    }
    #[test]
    fn selected_never_falls_back_or_rejoins_after_reconnect() {
        let (a, b) = (id(1, 1), id(2, 1));
        let mut routing = ComposeRouting::default();
        routing.reconcile(&[a, b], Some(b));
        routing.set_scope(ComposeScope::Selected);
        assert!(routing.selected.is_empty());
        routing.selected.push(b);
        routing.reconcile(&[a, b], Some(a));
        assert_eq!(routing.effective, vec![b]);
        routing.reconcile(&[a, id(2, 2)], Some(a));
        assert!(routing.effective.is_empty());
        routing.reconcile(&[a, b], Some(a));
        assert!(routing.effective.is_empty());
    }
    #[test]
    fn window_is_dynamic_and_membership_changes_disable_keys() {
        let (a, b, c) = (id(1, 1), id(2, 1), id(3, 1));
        let mut routing = ComposeRouting {
            scope: ComposeScope::Window,
            ..Default::default()
        };
        routing.reconcile(&[a, b], Some(a));
        routing.keys = true;
        routing.reconcile(&[b, a], Some(b));
        assert!(routing.keys);
        routing.reconcile(&[a, b, c], Some(a));
        assert_eq!(routing.effective, vec![a, b, c]);
        assert!(!routing.keys);
        assert!(routing.targets_changed);
        routing.set_scope(ComposeScope::Selected);
        assert!(routing.selected.is_empty());
        routing.selected.push(b);
        routing.set_scope(ComposeScope::Current);
        routing.reconcile(&[a, b, c], Some(a));
        routing.set_scope(ComposeScope::Selected);
        routing.reconcile(&[a, b, c], Some(a));
        assert_eq!(routing.effective, vec![b]);
    }
}
