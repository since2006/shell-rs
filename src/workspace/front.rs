//! Which pane is in front. A tab holding one pane shows just it; a tab
//! split into several shows them all, and the one in front is the one the
//! user works in: the focus decides, not only a tab switch. The status bar
//! and the right sidebar follow it.

use gpui_kit::*;

use crate::app::PaneContent;

use super::Workspace;

impl Workspace {
    /// The panes on screen: the tab in front's, or only its zoomed one.
    pub(super) fn visible_panes(&self, cx: &App) -> Vec<PaneContent> {
        self.center.read(cx).visible_panes(cx)
    }

    /// The focus went into `tab`: in a split, clicking into the other half
    /// brings that tab to the front without any tab being switched to.
    /// Does what the tab's own activation does.
    pub(super) fn focus_entered(
        &mut self,
        tab: PaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center
            .update(cx, |center, cx| center.focus_entered(tab, cx));
        if self.active_pane == Some(tab) {
            return;
        }
        let open = match tab {
            PaneContent::Terminal(id) => self.terminals.contains_key(&id),
            PaneContent::Explorer(id) => self.explorers.contains_key(&id),
            PaneContent::Editor(id) => self.editors.contains_key(&id),
            PaneContent::LocalTerminal(_) | PaneContent::Settings => true,
        };
        if !open {
            return;
        }
        self.set_active_pane(Some(tab), window, cx);
        cx.notify();
    }

    /// After the center changed (another tab in front, a pane closed or
    /// moved): a pane in front that no longer shows gives way to the one
    /// with the focus, or to its tab's current pane; then the right sidebar
    /// follows.
    pub(super) fn sync_front(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible_panes(cx);
        if let Some(tab) = self.active_pane
            && !visible.contains(&tab)
        {
            self.active_pane = visible
                .iter()
                .copied()
                .find(|tab| {
                    self.pane_focus_handle(*tab, cx)
                        .is_some_and(|focus| focus.contains_focused(window, cx))
                })
                .or_else(|| self.center.read(cx).current_pane());
        }
        self.sync_tool_sidebar(window, cx);
    }
}

/// Bring `tab` to the front whenever the focus goes into it. Registered with
/// every center tab, held for as long as the workspace.
pub(super) fn follow_focus(
    tab: PaneContent,
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
