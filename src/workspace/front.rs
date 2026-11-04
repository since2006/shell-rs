//! Which center tab is in front. Without a split it is the tab shown; with
//! the center split, several show at once and the one in front is the one
//! the user works in: the focus decides, not only a tab switch. The status
//! bar and the right sidebar follow it.

use gpui_kit::component::{
    WindowExt as _,
    dock::{DockPlacement, PaneRef, Panel, PanelId},
};
use gpui_kit::*;

use crate::app::CenterTab;

use super::Workspace;

impl Workspace {
    /// The center tabs on screen: the tab each group shows, or only the
    /// zoomed group's.
    pub(super) fn visible_center_tabs(&self, cx: &App) -> Vec<CenterTab> {
        let area = self.dock_area.read(cx);
        let Some(tree) = area.layout(DockPlacement::Center) else {
            return Vec::new();
        };
        let zoomed = area.zoomed_group();
        let mut shown: Vec<PanelId> = Vec::new();
        tree.root().walk(&mut |node| {
            if let PaneRef::Tabs { panels, active_ix } = node.kind()
                && zoomed.is_none_or(|zoomed| zoomed == node.id())
                && let Some(panel) = panels.get(active_ix).or(panels.last())
            {
                shown.push(*panel);
            }
        });
        shown
            .into_iter()
            .filter_map(|panel| self.center_tab_for_panel(panel))
            .collect()
    }

    /// The focus went into `tab`: in a split, clicking into the other half
    /// brings that tab to the front without any tab being switched to.
    /// Does what the tab's own activation does.
    pub(super) fn focus_entered(
        &mut self,
        tab: CenterTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The callback is deferred. Another focus change or removal may
        // already have superseded it before it reaches the workspace.
        if self.active_tab == Some(tab)
            || !self.visible_center_tabs(cx).contains(&tab)
            || !self
                .tab_focus_handle(tab, cx)
                .is_some_and(|focus| focus.contains_focused(window, cx))
        {
            return;
        }
        self.set_active_tab(Some(tab), window, cx);
        cx.notify();
    }

    /// A group's activation may be delivered after another group took focus.
    /// Keep the actual focused tab in front, ignoring departed senders.
    pub(super) fn panel_activated(
        &mut self,
        tab: CenterTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible = self.visible_center_tabs(cx);
        if !visible.contains(&tab) {
            return;
        }
        let front = visible
            .iter()
            .copied()
            .find(|candidate| {
                self.tab_focus_handle(*candidate, cx)
                    .is_some_and(|focus| focus.contains_focused(window, cx))
            })
            .unwrap_or(tab);
        self.set_active_tab(Some(front), window, cx);
    }

    /// After the center's layout changed (a tab dragged into another group,
    /// a group closed): a tab in front that no longer shows gives way to the
    /// one with the focus, or to one that shows; then the right sidebar
    /// follows.
    pub(super) fn sync_front(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible_center_tabs(cx);
        let focused = visible.iter().copied().find(|tab| {
            self.tab_focus_handle(*tab, cx)
                .is_some_and(|focus| focus.contains_focused(window, cx))
        });
        let front = focused
            .or(self.active_tab.filter(|tab| visible.contains(tab)))
            .or_else(|| self.recent_visible_tab(&visible))
            .or_else(|| visible.first().copied());
        if self.active_tab != front {
            self.set_active_tab(front, window, cx);
        } else {
            self.sync_tool_sidebar(window, cx);
        }
    }

    fn recent_visible_tab(&self, visible: &[CenterTab]) -> Option<CenterTab> {
        self.active_tab_history
            .iter()
            .rev()
            .copied()
            .find(|tab| visible.contains(tab))
    }

    /// Remove a center panel only after its close confirmation has succeeded.
    /// The Dock chooses the remaining tab in the source group; if that group
    /// disappears, use the most recently used tab still on screen. Native
    /// group activation runs as a task, so hand focus over behind that task.
    pub(super) fn remove_center_panel<P: Panel>(
        &mut self,
        panel: Entity<P>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = PanelId::from(panel.entity_id());
        let Some(closed) = self.center_tab_for_panel(id) else {
            return;
        };
        let Some((group, _)) = self.center_tab_location(closed, cx) else {
            return;
        };
        let source_panels: Vec<_> = group
            .read(cx)
            .panels()
            .iter()
            .map(|view| view.panel_id(cx))
            .collect();
        let source_focus: Vec<_> = group
            .read(cx)
            .panels()
            .iter()
            .map(|view| view.focus_handle(cx))
            .collect();
        let previous_focus = window.focused(cx);
        let held_focus = panel.read(cx).focus_handle(cx).contains_focused(window, cx);
        // A batch may already have removed the focused panel before this
        // removal runs. Its queued task is superseded, but its need to hand
        // focus over must survive through the last removal in the batch.
        let inherited_handoff =
            previous_focus.is_some() && previous_focus == self.pending_close_focus;
        if held_focus {
            self.pending_close_focus = previous_focus.clone();
        } else if !inherited_handoff {
            self.pending_close_focus = None;
        }
        let previous_front = self.active_tab;
        self.close_focus_generation = self.close_focus_generation.wrapping_add(1);
        let generation = self.close_focus_generation;
        let interaction = self.skin.interaction_generation();
        self.dock_area
            .update(cx, |area, cx| area.remove_panel(panel, window, cx));
        if self.dock_area.read(cx).panel(id).is_some() {
            return;
        }
        let remaining: Vec<_> = self
            .dock_area
            .read(cx)
            .layout(DockPlacement::Center)
            .map(|tree| tree.panels().collect())
            .unwrap_or_default();
        let live_tabs: Vec<_> = remaining
            .iter()
            .filter_map(|id| self.center_tab_for_panel(*id))
            .collect();
        self.active_tab_history
            .retain(|tab| live_tabs.contains(tab));
        let visible = self.visible_center_tabs(cx);
        let successor = visible
            .iter()
            .copied()
            .find(|tab| {
                self.center_tab_location(*tab, cx)
                    .is_some_and(|(_, id)| source_panels.contains(&id))
            })
            .or_else(|| self.recent_visible_tab(&visible))
            .or_else(|| visible.first().copied());
        let target = if held_focus || inherited_handoff || previous_front == Some(closed) {
            successor
        } else {
            previous_front
                .filter(|tab| visible.contains(tab))
                .or(successor)
        };
        let handoff = held_focus || inherited_handoff || previous_focus.is_none();
        cx.spawn_in(window, async move |this, cx| {
            _ = this.update_in(cx, |this, window, cx| {
                if this.close_focus_generation != generation {
                    return;
                }
                this.pending_close_focus = None;
                if this.skin.interaction_generation() != interaction
                    || window.has_active_dialog(cx)
                    || window.has_active_sheet(cx)
                    || crate::app_lock::is_locked(cx)
                {
                    return;
                }
                let current = window.focused(cx);
                // Preserve a newer focus choice outside the removed group.
                if current.is_some()
                    && current != previous_focus
                    && !source_focus
                        .iter()
                        .any(|focus| focus.contains_focused(window, cx))
                {
                    return;
                }
                if let Some(target) = target {
                    if !this.visible_center_tabs(cx).contains(&target) {
                        return;
                    }
                    if handoff {
                        // A panel knows its input target: for SFTP this is
                        // the last file list, not its enclosing focus handle.
                        let view = this
                            .center_tab_location(target, cx)
                            .and_then(|(_, id)| this.dock_area.read(cx).panel(id).cloned());
                        if let Some(view) = view {
                            view.set_active(true, window, cx);
                        }
                    } else if let Some(focus) = &previous_focus {
                        window.focus(focus, cx);
                    }
                    this.set_active_tab(Some(target), window, cx);
                } else if this.dock_area.read(cx).is_empty(DockPlacement::Center, cx) {
                    let focus = this.recent.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                    this.set_active_tab(None, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// Bring `tab` to the front whenever the focus goes into it. Registered with
/// every center tab, held for as long as the workspace.
pub(super) fn follow_focus(
    tab: CenterTab,
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Subscription {
    cx.on_focus_in(focus, window, move |_, window, cx| {
        // Focus listeners run in the frame's focus phase, where a change
        // would not be drawn until something else asks for a frame.
        cx.defer_in(window, move |this, window, cx| {
            this.focus_entered(tab, window, cx)
        });
    })
}
