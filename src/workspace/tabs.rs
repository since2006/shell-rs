//! The center's tabs from the keyboard: the next one, the previous one, or
//! one by its position; and rearranged by dragging tabs and pane headers.

use gpui_kit::*;

use std::rc::Rc;

use crate::app::{
    ActivatePane, ActivateTab, ChooseTabToJoin, CloseTab, CloseTabs, EqualizePanes, JoinTab,
    MergeTab, MovePane, MovePaneToNewTab, MoveTab, NextTab, PaneContent, PreviousTab, RenameTab,
    SwapPanes, SwitchToTab, ZoomPane,
};
use crate::center::TabNaming;
use crate::i18n::{t, tn};
use crate::shared::{confirm_danger, open_rename_tab_dialog};

use super::Workspace;

impl Workspace {
    pub(super) fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.center
            .update(cx, |center, cx| center.step(1, window, cx));
    }

    pub(super) fn on_previous_tab(
        &mut self,
        _: &PreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center
            .update(cx, |center, cx| center.step(-1, window, cx));
    }

    /// From 1; 9 is the last, as in browsers. A position past the last
    /// does nothing.
    pub(super) fn on_switch_to_tab(
        &mut self,
        action: &SwitchToTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center
            .update(cx, |center, cx| center.switch_to(action.0, window, cx));
    }

    pub(super) fn on_move_pane(
        &mut self,
        action: &MovePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center.update(cx, |center, cx| {
            center.move_pane(action.pane, action.onto, action.placement, window, cx);
        });
    }

    pub(super) fn on_merge_tab(
        &mut self,
        action: &MergeTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center.update(cx, |center, cx| {
            center.merge_tab(action.from, action.onto, action.placement, window, cx);
        });
    }

    pub(super) fn on_join_tab(
        &mut self,
        action: &JoinTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center.update(cx, |center, cx| {
            center.join_tab(action.pane, action.tab, action.placement, window, cx);
        });
    }

    pub(super) fn on_swap_panes(
        &mut self,
        action: &SwapPanes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center.update(cx, |center, cx| {
            center.swap_panes(action.pane, action.with, window, cx);
        });
    }

    pub(super) fn on_equalize_panes(
        &mut self,
        action: &EqualizePanes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center
            .update(cx, |center, cx| center.equalize(action.0, window, cx));
    }

    /// The menu opens where the pointer is: on the command that asked for
    /// it.
    pub(super) fn on_choose_tab_to_join(
        &mut self,
        action: &ChooseTabToJoin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = window.mouse_position();
        self.center.update(cx, |center, cx| {
            center.open_chooser(action.tab, action.placement, position, window, cx);
        });
    }

    /// At the place asked for, or right after the tab the pane leaves.
    pub(super) fn on_move_pane_to_new_tab(
        &mut self,
        action: &MovePaneToNewTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center.update(cx, |center, cx| {
            let Some(from) = center.tab_of(action.pane) else {
                return;
            };
            let ix = action.ix.unwrap_or_else(|| {
                center
                    .tab_ids()
                    .iter()
                    .position(|tab| *tab == from)
                    .map_or(usize::MAX, |ix| ix + 1)
            });
            center.split_off(action.pane, ix, window, cx);
        });
    }

    pub(super) fn on_move_tab(&mut self, action: &MoveTab, _: &mut Window, cx: &mut Context<Self>) {
        self.center
            .update(cx, |center, cx| center.move_tab(action.tab, action.ix, cx));
    }

    /// Close a tab with all its panes, asking first when it holds several.
    pub(super) fn on_close_tab(
        &mut self,
        action: &CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let center = self.center.read(cx);
        let panes = center.panes(action.0).to_vec();
        if panes.len() < 2 {
            self.close_panes(panes, window, cx);
            return;
        }
        let title = t!(
            "center.tab.close_confirm",
            name = center.tab_label(action.0, cx),
            count = panes.len()
        );
        self.confirm_closing(title, panes, window, cx);
    }

    /// The tabs a close command of a tab's menu takes, the tab it was opened
    /// on being `action.tab`'s. Asks first when one of them holds several
    /// panes.
    pub(super) fn on_close_tabs(
        &mut self,
        action: &CloseTabs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let center = self.center.read(cx);
        let tabs = center.tabs_in_scope(action.tab, action.scope);
        let several = tabs.iter().any(|tab| center.panes(*tab).len() > 1);
        let panes: Vec<PaneContent> = tabs
            .iter()
            .flat_map(|tab| center.panes(*tab).to_vec())
            .collect();
        if several {
            let title = tn!("center.tab.close_many_confirm", panes.len());
            self.confirm_closing(title, panes, window, cx);
        } else {
            self.close_panes(panes, window, cx);
        }
    }

    fn confirm_closing(
        &self,
        title: SharedString,
        panes: Vec<PaneContent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = cx.weak_entity();
        confirm_danger(
            title,
            Some(t!("center.tab.close_description")),
            t!("common.close"),
            Rc::new(move |window, cx| {
                _ = workspace.update(cx, |workspace, cx| {
                    workspace.close_panes(panes.clone(), window, cx)
                });
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_rename_tab(
        &mut self,
        action: &RenameTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let naming = cx.new(|_| TabNaming::new(self.center.downgrade(), action.0));
        open_rename_tab_dialog(naming, false, window, cx);
    }

    pub(super) fn on_zoom_pane(
        &mut self,
        action: &ZoomPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center
            .update(cx, |center, cx| center.toggle_zoom(action.0, window, cx));
    }

    pub(super) fn on_activate_tab(
        &mut self,
        action: &ActivateTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.center
            .update(cx, |center, cx| center.activate_tab(action.0, window, cx));
    }

    pub(super) fn on_activate_pane(
        &mut self,
        action: &ActivatePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_pane(action.0, window, cx);
    }
}
