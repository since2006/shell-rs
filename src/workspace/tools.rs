//! The right sidebar's part of `Workspace`: the tools for the SSH terminal
//! in front, and the switch that picks one.
//!
//! The sidebar goes with SSH terminals: with an SFTP tab, a local terminal,
//! the settings or the start page in front it is not there at all, switch
//! included. Whether it is open and which tool it shows stay as they were,
//! so the next SSH terminal brings it back the same. A tool that cannot work
//! on the host in front (the monitor on a host known not to run Linux) is
//! not offered there: no button, and no sidebar while it is the one picked.

use gpui_kit::component::dock::{DockArea, DockPlacement};
use gpui_kit::*;

use crate::app::{CenterTab, ToggleMonitorDetail, ToggleTool, ToggleToolSidebar, ToolKind};

use super::{Workspace, tool_sidebar::ToolTerminal};

/// How wide the right sidebar opens, and the narrowest it can be dragged:
/// the monitor's cards are laid out for this width.
pub(super) const TOOL_SIDEBAR_WIDTH: Pixels = px(320.);

impl Workspace {
    /// The SSH terminal in front, which the right sidebar works on.
    pub(super) fn tool_terminal(&self, cx: &App) -> Option<ToolTerminal> {
        let CenterTab::Terminal(id) = self.active_tab? else {
            return None;
        };
        let panel = self.terminals.get(&id)?.read(cx);
        Some(ToolTerminal {
            id,
            host: panel.host_id(),
            view: panel.terminal().downgrade(),
        })
    }

    /// The tools the switch offers for the SSH terminal in front; none
    /// without one.
    pub(super) fn offered_tools(&self, cx: &App) -> Vec<ToolKind> {
        let Some(terminal) = self.tool_terminal(cx) else {
            return Vec::new();
        };
        let os = self
            .store
            .read(cx)
            .host(terminal.host)
            .and_then(|host| host.os);
        ToolKind::ALL
            .into_iter()
            .filter(|tool| tool.works_on(os))
            .collect()
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
        let tool = action.0;
        if !self.offered_tools(cx).contains(&tool) {
            return;
        }
        if self.tool_showing(cx) == Some(tool) {
            self.tool_sidebar_wanted = false;
        } else {
            self.tools
                .update(cx, |tools, cx| tools.show(tool, window, cx));
            self.tool_sidebar_wanted = true;
        }
        self.sync_tool_sidebar(window, cx);
    }

    /// Undo a drag that took the right sidebar below its narrowest. Runs
    /// whenever the dock area changes; the dock has no minimum of its own
    /// to set, and its drag callback is not ours to wrap. Set back within
    /// the same update, so no frame shows it narrower.
    pub(super) fn hold_tool_sidebar_width(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let narrower = self
            .dock_area
            .read(cx)
            .dock_size(DockPlacement::Right)
            .is_some_and(|width| width < TOOL_SIDEBAR_WIDTH);
        if narrower {
            self.dock_area.update(cx, |area, cx| {
                area.set_dock_size(DockPlacement::Right, TOOL_SIDEBAR_WIDTH, window, cx);
            });
        }
    }

    /// The system monitor's fold buttons.
    pub(super) fn on_toggle_monitor_detail(
        &mut self,
        action: &ToggleMonitorDetail,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let detail = action.0;
        self.tools
            .update(cx, |tools, cx| tools.toggle_monitor_detail(detail, cx));
    }

    /// Hide the sidebar, or show the tool shown last; the first tool on
    /// offer when that one does not work on this host. Without an SSH
    /// terminal in front there is nothing to show or hide.
    pub(super) fn on_toggle_tool_sidebar(
        &mut self,
        _: &ToggleToolSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offered = self.offered_tools(cx);
        let Some(&first) = offered.first() else {
            return;
        };
        if self.dock_area.read(cx).is_dock_open(DockPlacement::Right) {
            self.tool_sidebar_wanted = false;
        } else {
            if !offered.contains(&self.tools.read(cx).tool()) {
                self.tools
                    .update(cx, |tools, cx| tools.show(first, window, cx));
            }
            self.tool_sidebar_wanted = true;
        }
        self.sync_tool_sidebar(window, cx);
    }

    /// Point the sidebar at the SSH terminal in front, and open it if one is,
    /// its tool works on the host and the sidebar is wanted; close it
    /// otherwise. Also when the host turns out to run something else.
    pub(super) fn sync_tool_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = self.tool_terminal(cx);
        let offered = self.offered_tools(cx).contains(&self.tools.read(cx).tool());
        let open = self.tool_sidebar_wanted && terminal.is_some() && offered;
        self.tools
            .update(cx, |tools, cx| tools.set_terminal(terminal, open, cx));
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
