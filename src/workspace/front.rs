//! Which center tab is in front. Without a split it is the tab shown; with
//! the center split, several show at once and the one in front is the one
//! the user works in: the focus decides, not only a tab switch. The status
//! bar and the right sidebar follow it.

use gpui_kit::component::dock::{DockPlacement, PaneRef, PanelId};
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
        if self.active_tab == Some(tab) {
            return;
        }
        // The host list marks the host of the tab in front.
        let host = match tab {
            CenterTab::Terminal(id) => match self.terminals.get(&id) {
                Some(panel) => Some(panel.read(cx).host_id()),
                None => return,
            },
            CenterTab::Explorer(id) => match self.explorers.get(&id) {
                Some(panel) => Some(panel.read(cx).host_id()),
                None => return,
            },
            CenterTab::Editor(id) => match self.editors.get(&id) {
                Some(editor) => editor.read(cx).host_id(),
                None => return,
            },
            CenterTab::LocalTerminal(_) | CenterTab::Settings => None,
        };
        self.store
            .update(cx, |store, cx| store.set_active(host, cx));
        self.set_active_tab(Some(tab), window, cx);
        cx.notify();
    }

    /// After the center's layout changed (a tab dragged into another group,
    /// a group closed): a tab in front that no longer shows gives way to the
    /// one with the focus, or to one that shows; then the right sidebar
    /// follows.
    pub(super) fn sync_front(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible_center_tabs(cx);
        if let Some(tab) = self.active_tab
            && !visible.contains(&tab)
        {
            self.active_tab = visible
                .iter()
                .copied()
                .find(|tab| {
                    self.tab_focus_handle(*tab, cx)
                        .is_some_and(|focus| focus.contains_focused(window, cx))
                })
                .or_else(|| visible.first().copied());
        }
        self.sync_tool_sidebar(window, cx);
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
