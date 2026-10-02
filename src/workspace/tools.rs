//! The right sidebar's part of `Workspace`: the tools for the SSH terminal
//! in front, and the switch that picks one.
//!
//! The sidebar goes with SSH terminals: with an SFTP tab, a local terminal,
//! the settings or the start page in front it is not there at all, switch
//! included. Whether it is open and which tool it shows stay as they were,
//! so the next SSH terminal brings it back the same. A tool that cannot work
//! on the host in front (the monitor on a host known not to run Linux) is
//! not offered there: no button, and no sidebar while it is the one picked.

use std::rc::Rc;

use gpui_kit::component::{
    WindowExt as _,
    dock::{DockArea, DockPlacement},
    notification::Notification,
};
use gpui_kit::*;

use crate::app::{
    CenterTab, ControlService, EndProcess, RefreshConnections, RefreshProcesses, RefreshServices,
    ShowProcess, ShowService, SortProcesses, ToggleMonitorDetail, ToggleTool, ToggleToolSidebar,
    ToolKind,
};
use crate::processes::{end_command, ended, open_process_dialog};
use crate::services::{ServiceCommand, control_command, controlled, open_service_dialog};
use crate::shared::confirm_danger;
use crate::terminal::exec_answer;

use super::{Workspace, tool_sidebar::ToolTerminal};

/// How wide the right sidebar opens, and the narrowest it can be dragged:
/// the monitor's and the connections' cards are laid out for this width.
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

    /// 网络连接's 刷新: read the host's sockets again.
    pub(super) fn on_refresh_connections(
        &mut self,
        _: &RefreshConnections,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools
            .update(cx, |tools, cx| tools.refresh_connections(cx));
    }

    /// 进程管理's 刷新: read the host's processes again.
    pub(super) fn on_refresh_processes(
        &mut self,
        _: &RefreshProcesses,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools
            .update(cx, |tools, cx| tools.refresh_processes(cx));
    }

    /// 进程管理's sort buttons.
    pub(super) fn on_sort_processes(
        &mut self,
        action: &SortProcesses,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let by = action.0;
        self.tools
            .update(cx, |tools, cx| tools.sort_processes(by, cx));
    }

    /// 系统服务's 刷新: read the host's services again.
    pub(super) fn on_refresh_services(
        &mut self,
        _: &RefreshServices,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools
            .update(cx, |tools, cx| tools.refresh_services(cx));
    }

    /// A service's details: its state in full and its journal.
    pub(super) fn on_show_service(
        &mut self,
        action: &ShowService,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let Some(service) = self.tools.read(cx).service(&action.0, cx) else {
            return;
        };
        open_service_dialog(
            service,
            terminal.view,
            self.focus_handle.clone(),
            window,
            cx,
        );
    }

    /// Run a command on a service over the SSH terminal's own connection,
    /// asking first before it stops or restarts one; how it went is a
    /// notification, and the list is read again.
    pub(super) fn on_control_service(
        &mut self,
        action: &ControlService,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let ControlService { name, command } = action.clone();
        let workspace = cx.entity().downgrade();
        let run = Rc::new({
            let name = name.clone();
            move |window: &mut Window, cx: &mut App| {
                let reply = control_command(&name, command)
                    .and_then(|script| terminal.view.upgrade()?.read(cx).exec(script, cx));
                let Some(reply) = reply else {
                    window.push_notification(
                        Notification::error(format!(
                            "无法{} {name}：终端没有连接",
                            command.label()
                        )),
                        cx,
                    );
                    return;
                };
                let (workspace, name) = (workspace.clone(), name.clone());
                window
                    .spawn(cx, async move |cx| {
                        let outcome = match exec_answer(reply, cx).await {
                            None => Err("终端没有连接".to_string()),
                            Some(Err(error)) => Err(error),
                            Some(Ok(output)) => controlled(&output),
                        };
                        cx.update(|window, cx| {
                            let notification = match outcome {
                                Ok(()) => {
                                    Notification::success(format!("{name} {}", command.done()))
                                }
                                Err(why) => Notification::error(format!(
                                    "无法{} {name}：{why}",
                                    command.label()
                                )),
                            };
                            window.push_notification(notification, cx);
                            workspace
                                .update(cx, |this, cx| {
                                    this.tools
                                        .update(cx, |tools, cx| tools.refresh_services(cx))
                                })
                                .ok();
                        })
                        .ok();
                    })
                    .detach();
            }
        });
        // Stopping and restarting take something away, if only for a
        // moment; starting and the boot settings do not.
        let ssh = matches!(name.as_str(), "ssh.service" | "sshd.service");
        let (title, description) = match command {
            ServiceCommand::Stop => (
                format!("停止服务“{name}”？"),
                if ssh {
                    "这是 SSH 服务：停止后新的连接都连不上这台主机，直到它再次启动。"
                } else {
                    "停止后它提供的功能就不可用了，直到再次启动。"
                },
            ),
            ServiceCommand::Restart => (
                format!("重启服务“{name}”？"),
                "服务会先停止再启动，中间短暂不可用。",
            ),
            _ => return run(window, cx),
        };
        confirm_danger(
            title.into(),
            Some(description.into()),
            command.label(),
            run,
            window,
            cx,
        );
    }

    /// A process's details, from 进程管理's list and its command line.
    pub(super) fn on_show_process(
        &mut self,
        action: &ShowProcess,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let Some(details) = self.tools.read(cx).process_details(action.0, cx) else {
            return;
        };
        open_process_dialog(
            details,
            terminal.view,
            self.focus_handle.clone(),
            window,
            cx,
        );
    }

    /// Ask, naming the process, then send it SIGTERM, or SIGKILL when
    /// forced, over the SSH terminal's own connection. How that went is a
    /// notification, and the list is read again a moment later.
    pub(super) fn on_end_process(
        &mut self,
        action: &EndProcess,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let EndProcess { pid, force } = *action;
        let Some(process) = self.tools.read(cx).process(pid, cx) else {
            return;
        };
        let name = process.name;
        let user = process.user.unwrap_or_else(|| "—".into());
        let (title, description, verb) = if force {
            (
                format!("强制结束进程“{name}”？"),
                format!(
                    "PID {pid}，用户 {user}。进程会立即被终止（SIGKILL），来不及保存数据；                     只在「结束进程」不起作用时使用。"
                ),
                "强制结束",
            )
        } else {
            (
                format!("结束进程“{name}”？"),
                format!(
                    "PID {pid}，用户 {user}。进程会收到结束信号（SIGTERM），可以先做完收尾再退出。"
                ),
                "结束进程",
            )
        };
        let workspace = cx.entity().downgrade();
        confirm_danger(
            title.into(),
            Some(description.into()),
            verb,
            Rc::new(move |window, cx| {
                let Some(view) = terminal.view.upgrade() else {
                    return;
                };
                let Some(reply) = view.read(cx).exec(end_command(pid, force), cx) else {
                    window.push_notification(
                        Notification::error(format!("无法结束 {name}（PID {pid}）：终端没有连接")),
                        cx,
                    );
                    return;
                };
                let (workspace, name) = (workspace.clone(), name.clone());
                window
                    .spawn(cx, async move |cx| {
                        let outcome = match exec_answer(reply, cx).await {
                            None => Err("终端没有连接".to_string()),
                            Some(Err(error)) => Err(error),
                            Some(Ok(output)) => ended(&output),
                        };
                        cx.update(|window, cx| {
                            let notification = match outcome {
                                Ok(()) if force => {
                                    Notification::success(format!("已强制结束 {name}（PID {pid}）"))
                                }
                                Ok(()) => Notification::success(format!(
                                    "已向 {name}（PID {pid}）发送结束信号"
                                )),
                                Err(why) => Notification::error(format!(
                                    "无法结束 {name}（PID {pid}）：{why}"
                                )),
                            };
                            window.push_notification(notification, cx);
                            workspace
                                .update(cx, |this, cx| {
                                    this.tools
                                        .update(cx, |tools, cx| tools.refresh_processes_soon(cx))
                                })
                                .ok();
                        })
                        .ok();
                    })
                    .detach();
            }),
            window,
            cx,
        );
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
