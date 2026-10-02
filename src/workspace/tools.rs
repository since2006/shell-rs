//! The right sidebar's part of `Workspace`: the tools for the terminal in
//! front, and the switch that picks one.
//!
//! The sidebar goes with terminals, remote or local: with an SFTP tab, the
//! settings or the start page in front it is not there at all, switch
//! included. Whether it is open and which tool it shows stay as they were,
//! so the next terminal brings it back the same.

use gpui_kit::component::dock::{DockArea, DockPlacement};
use gpui_kit::*;

use crate::app::{CenterTab, ToggleTool, ToggleToolSidebar, ToolKind};

use super::{Workspace, tool_sidebar::ToolTerminal};

impl Workspace {
    /// The terminal in front, which the right sidebar works on.
    pub(super) fn tool_terminal(&self, cx: &App) -> Option<ToolTerminal> {
        match self.active_tab? {
            CenterTab::Terminal(id) => {
                let host = self.terminals.get(&id)?.read(cx).host_id();
                Some(ToolTerminal::Remote(id, host))
            }
            CenterTab::LocalTerminal(id) => self
                .local_terminals
                .contains_key(&id)
                .then_some(ToolTerminal::Local(id)),
            CenterTab::Explorer(_) | CenterTab::Settings => None,
        }
    }

    /// The tool the right sidebar is showing, `None` while it is hidden.
    pub(super) fn tool_showing(&self, cx: &App) -> Option<ToolKind> {
        self.dock_area
            .read(cx)
            .is_dock_open(DockPlacement::Right)
            .then(|| self.tools.read(cx).tool())
    }

    /// Record the center tab in front, and bring the right sidebar in line
    /// with it.
    pub(super) fn set_active_tab(
        &mut self,
        tab: Option<CenterTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_tab = tab;
        self.sync_tool_sidebar(window, cx);
    }

    /// The switch's buttons: show that tool, or hide the sidebar when it is
    /// the one showing.
    pub(super) fn on_toggle_tool(
        &mut self,
        action: &ToggleTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tool_terminal(cx).is_none() {
            return;
        }
        let tool = action.0;
        if self.tool_showing(cx) == Some(tool) {
            self.tool_sidebar_wanted = false;
        } else {
            self.tools
                .update(cx, |tools, cx| tools.show(tool, window, cx));
            self.tool_sidebar_wanted = true;
        }
        self.sync_tool_sidebar(window, cx);
    }

    /// Without a terminal in front there is nothing to show or hide.
    pub(super) fn on_toggle_tool_sidebar(
        &mut self,
        _: &ToggleToolSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tool_terminal(cx).is_none() {
            return;
        }
        self.tool_sidebar_wanted = !self.tool_sidebar_wanted;
        self.sync_tool_sidebar(window, cx);
    }

    /// Point the sidebar at the terminal in front, and open it if one is and
    /// it is wanted, close it otherwise.
    fn sync_tool_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = self.tool_terminal(cx);
        self.tools
            .update(cx, |tools, cx| tools.set_terminal(terminal, cx));
        let open = self.tool_sidebar_wanted && terminal.is_some();
        if self.dock_area.read(cx).is_dock_open(DockPlacement::Right) != open {
            // A focus inside the sidebar would go off screen with it.
            let focus_inside = self
                .tools
                .read(cx)
                .focus_handle(cx)
                .contains_focused(window, cx);
            self.dock_area.update(cx, |area, cx| {
                set_right_dock_open(area, open, window, cx);
            });
            if !open && focus_inside {
                self.focus_center(window, cx);
            }
        }
        // The switch comes and goes with the terminal, and marks the tool.
        cx.notify();
    }
}

/// Open or close the right dock.
///
/// The switch is the dock's only control, so the dock is not collapsible:
/// a collapsible dock gets a second one, gpui-kit's collapse button at the
/// end of the center's tab bar. A dock that is not collapsible refuses to
/// close, so it is made collapsible just long enough to close it.
pub(super) fn set_right_dock_open(
    area: &mut DockArea,
    open: bool,
    window: &mut Window,
    cx: &mut Context<DockArea>,
) {
    if area.is_dock_open(DockPlacement::Right) == open {
        return;
    }
    area.set_dock_collapsible(DockPlacement::Right, true, window, cx);
    area.toggle_dock(DockPlacement::Right, window, cx);
    area.set_dock_collapsible(DockPlacement::Right, false, window, cx);
}
