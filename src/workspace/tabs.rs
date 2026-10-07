//! Moving between the center's tabs from the keyboard: the next one, the
//! previous one, or one by its position, within the current tab's group.

use gpui_kit::component::dock::TabGroup;
use gpui_kit::*;

use crate::app::{NextTab, PreviousTab, SwitchToTab};

use super::Workspace;

impl Workspace {
    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_tab(1, window, cx);
    }

    pub(super) fn on_previous_tab(
        &mut self,
        _: &PreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_tab(-1, window, cx);
    }

    /// From 1; 9 is the last, as in browsers. A position past the last
    /// does nothing.
    pub(super) fn on_switch_to_tab(
        &mut self,
        action: &SwitchToTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((group, _)) = self.current_tab_group(cx) else {
            return;
        };
        group.update(cx, |group, cx| {
            let count = group.panels().len();
            let ix = if action.0 >= 9 {
                count.checked_sub(1)
            } else {
                action.0.checked_sub(1).filter(|ix| *ix < count)
            };
            if let Some(ix) = ix {
                group.select_tab(ix, window, cx);
            }
        });
    }

    /// The tab `step` places away from the current one, around the ends.
    fn step_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((group, ix)) = self.current_tab_group(cx) else {
            return;
        };
        group.update(cx, |group, cx| {
            let count = group.panels().len() as isize;
            if count > 1 {
                let next = (ix as isize + step).rem_euclid(count) as usize;
                group.select_tab(next, window, cx);
            }
        });
    }

    /// The group holding the current center tab, and where in it the tab
    /// is. Its `select_tab` activates the panel, which reports back to
    /// `set_active_tab`.
    fn current_tab_group(&self, cx: &App) -> Option<(Entity<TabGroup>, usize)> {
        let (group, panel) = self.center_tab_location(self.active_tab?, cx)?;
        let ix = group
            .read(cx)
            .panels()
            .iter()
            .position(|candidate| candidate.panel_id(cx) == panel)?;
        Some((group, ix))
    }
}
