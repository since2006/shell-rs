//! The right sidebar's part of `Workspace`: the tools for the terminal in
//! front, and the switch that picks one.
//!
//! The sidebar goes with terminals: with an SFTP tab, the settings or the
//! start page in front it is not there at all, switch included. Beside an
//! SSH terminal it has the host's tools; beside a local terminal 项目 alone.
//! With the center split it goes with the terminal in front last, as long
//! as that one is on screen, and names an SSH terminal's host; it hides once
//! no terminal shows. Whether it is open and which tool it shows are the
//! host's: the host keeps them, a saved one across launches, and its next
//! terminal in front brings the sidebar back the same, while another host's
//! has it as that host left it. Local terminals share one such choice, kept
//! in the settings. A tool that cannot work on the host in front (the
//! monitor on a host known not to run Linux) is not offered there: no
//! button, and no sidebar while it is the one the host keeps.

use std::rc::Rc;

use gpui_kit::component::{
    WindowExt as _,
    dock::{DockArea, DockPlacement},
    notification::Notification,
};
use gpui_kit::*;

use crate::analytics::Counter;
use crate::app::{
    CenterTab, ControlContainers, ControlService, CopyCommand, EndProcess, EnterCommand,
    RefreshConnections, RefreshDocker, RefreshGpu, RefreshHistory, RefreshProcesses,
    RefreshServices, RemoveDockerObject, ShowDockerObject, ShowProcess, ShowService, SortProcesses,
    ToggleAllGpuDevices, ToggleDockerProject, ToggleGpuDevice, ToggleMonitorDetail, ToggleTool,
    ToggleToolSidebar, ToolKind,
};
use crate::docker::{
    self, ContainerCommand, ContainerSubject, DockerObject, open_container_dialog,
    open_object_dialog,
};
use crate::host::{HostId, HostOs};
use crate::i18n::t;
use crate::processes::{end_command, ended, open_process_dialog};
use crate::services::{ServiceCommand, control_command, controlled, open_service_dialog};
use crate::shared::confirm_danger;
use crate::terminal::{LocalTerminalId, RemoteTerminalId, exec_answer};

use super::{
    Workspace,
    tool_sidebar::{SidebarTarget, ToolSidebar, ToolTerminal},
};

/// A terminal the right sidebar can go with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToolTarget {
    Remote(RemoteTerminalId),
    Local(LocalTerminalId),
}

impl ToolTarget {
    fn of(tab: CenterTab) -> Option<Self> {
        match tab {
            CenterTab::Terminal(id) => Some(Self::Remote(id)),
            CenterTab::LocalTerminal(id) => Some(Self::Local(id)),
            _ => None,
        }
    }

    pub(super) fn tab(self) -> CenterTab {
        match self {
            Self::Remote(id) => CenterTab::Terminal(id),
            Self::Local(id) => CenterTab::LocalTerminal(id),
        }
    }
}

/// How wide the right sidebar opens, and the narrowest it can be dragged:
/// the monitor's and the connections' cards are laid out for this width.
pub(super) const TOOL_SIDEBAR_WIDTH: Pixels = px(320.);

impl Workspace {
    pub(super) fn on_kubernetes_action(
        &mut self,
        action: &crate::app::KubernetesAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .settings()
            .read(cx)
            .settings()
            .sidebar_tools
            .shows(ToolKind::Kubernetes)
        {
            return;
        }
        if let crate::kubernetes::KubernetesCommand::CopyValue(value) = &action.0 {
            cx.write_to_clipboard(ClipboardItem::new_string(value.clone()));
            window.push_notification(Notification::success(t!("k8s.copied")), cx);
            return;
        }
        self.tools.update(cx, |tools, cx| {
            tools.kubernetes_command(&action.0, window, cx)
        });
    }
    /// The terminal the right sidebar goes with: the one in front last
    /// while it is on screen, else another that is.
    pub(super) fn tool_target(&self, cx: &App) -> Option<ToolTarget> {
        let visible = self.visible_center_tabs(cx);
        self.tool_target_choice
            .filter(|target| visible.contains(&target.tab()))
            .or_else(|| visible.iter().copied().find_map(ToolTarget::of))
    }

    /// The SSH terminal the right sidebar works on: none while it goes with
    /// a local terminal.
    pub(super) fn tool_terminal(&self, cx: &App) -> Option<ToolTerminal> {
        let ToolTarget::Remote(id) = self.tool_target(cx)? else {
            return None;
        };
        let panel = self.terminals.get(&id)?.read(cx);
        Some(ToolTerminal {
            id,
            host: panel.host_id(),
            name: panel.host().name.clone(),
            os: panel.host().os,
            shell_only: panel.shell_only(),
            view: panel.terminal().downgrade(),
        })
    }

    /// What the sidebar goes with, as it shows it.
    fn sidebar_target(&self, cx: &App) -> Option<SidebarTarget> {
        match self.tool_target(cx)? {
            ToolTarget::Remote(_) => self.tool_terminal(cx).map(SidebarTarget::Remote),
            ToolTarget::Local(id) => self
                .local_terminals
                .contains_key(&id)
                .then_some(SidebarTarget::Local(id)),
        }
    }

    /// The tools the switch offers for the terminal in front; none without
    /// one. Those 侧栏功能 leaves on: beside a local terminal 项目; beside an
    /// SSH terminal those that work on the host, where an external
    /// connection (a bastion host's link) and a serial host, which have no
    /// channel beside the terminal, get only those that run nothing beside
    /// it.
    pub(super) fn offered_tools(&self, cx: &App) -> Vec<ToolKind> {
        let shown = self.settings().read(cx).settings().sidebar_tools;
        match self.sidebar_target(cx) {
            None => Vec::new(),
            Some(SidebarTarget::Local(_)) => ToolKind::ALL
                .into_iter()
                .filter(|tool| tool.is_local() && shown.shows(*tool))
                .collect(),
            Some(SidebarTarget::Remote(terminal)) => {
                let (os, shell_only) = (terminal.os, terminal.shell_only);
                ToolKind::ALL
                    .into_iter()
                    .filter(|tool| {
                        !tool.is_local()
                            && shown.shows(*tool)
                            && tool.works_on(os)
                            && !(shell_only && tool.runs_commands())
                    })
                    .collect()
            }
        }
    }

    /// GPU/NPU's 刷新: read the host's devices again, and go on reading.
    pub(super) fn on_refresh_gpu(
        &mut self,
        _: &RefreshGpu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools.update(cx, |tools, cx| tools.refresh_gpu(cx));
    }

    /// GPU/NPU's 全部展开 and 全部收起.
    pub(super) fn on_toggle_all_gpu_devices(
        &mut self,
        _: &ToggleAllGpuDevices,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools
            .update(cx, |tools, cx| tools.toggle_all_gpu_devices(cx));
    }

    /// A click on a device of GPU/NPU: its details, or no more.
    pub(super) fn on_toggle_gpu_device(
        &mut self,
        action: &ToggleGpuDevice,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = action.0.clone();
        self.tools
            .update(cx, |tools, cx| tools.toggle_gpu_device(&key, cx));
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
        if let Some(target) = tab.and_then(ToolTarget::of) {
            self.tool_target_choice = Some(target);
        }
        if let Some(CenterTab::LocalTerminal(id)) = tab
            && self.saved_terminals.contains_key(&id)
        {
            self.front_saved_terminal = Some(id);
        }
        self.sync_tool_sidebar(window, cx);
        self.sync_saved_terminals(cx);
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
        let Some(target) = self.sidebar_target(cx) else {
            return;
        };
        if !self.offered_tools(cx).contains(&tool) {
            return;
        }
        if self.tool_showing(cx) == Some(tool) {
            self.remember_target_tool(&target, None, cx);
        } else {
            self.remember_target_tool(&target, Some(tool), cx);
            self.count_tool(tool);
        }
        self.sync_tool_sidebar(window, cx);
    }

    /// The right sidebar's tool as the terminal in front had it last,
    /// `None` while hidden: its host's, or every local terminal's.
    fn target_tool(&self, target: &SidebarTarget, cx: &App) -> Option<ToolKind> {
        match target {
            SidebarTarget::Remote(terminal) => self.host_tool(terminal.host, cx),
            SidebarTarget::Local(_) => self.settings().read(cx).settings().local_sidebar_tool,
        }
    }

    /// Keep the right sidebar's tool for the terminal in front: for its
    /// host, or for every local terminal.
    fn remember_target_tool(
        &mut self,
        target: &SidebarTarget,
        tool: Option<ToolKind>,
        cx: &mut Context<Self>,
    ) {
        match target {
            SidebarTarget::Remote(terminal) => self.remember_host_tool(terminal.host, tool, cx),
            SidebarTarget::Local(_) => self.settings().update(cx, |settings, cx| {
                settings.update(|settings| settings.local_sidebar_tool = tool, cx)
            }),
        }
    }

    /// The right sidebar's tool as the host's terminals last had it, `None`
    /// while hidden.
    fn host_tool(&self, host: HostId, cx: &App) -> Option<ToolKind> {
        match self.store.read(cx).host(host) {
            Some(host) => host.sidebar_tool.as_deref().and_then(ToolKind::from_key),
            None => self.deleted_host_tools.get(&host).copied(),
        }
    }

    /// Keep the right sidebar's tool for the host's terminals: on the host,
    /// or here for one deleted while its terminals are open.
    fn remember_host_tool(&mut self, host: HostId, tool: Option<ToolKind>, cx: &mut Context<Self>) {
        if self.store.read(cx).host(host).is_some() {
            self.store.update(cx, |store, cx| {
                store.set_host_sidebar_tool(host, tool.map(|tool| tool.key().into()), cx)
            });
        } else if let Some(tool) = tool {
            self.deleted_host_tools.insert(host, tool);
        } else {
            self.deleted_host_tools.remove(&host);
        }
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

    /// 历史命令's 刷新: read the host's bash history again.
    pub(super) fn on_refresh_history(
        &mut self,
        _: &RefreshHistory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools.update(cx, |tools, cx| tools.refresh_history(cx));
    }

    /// A command of 历史命令 or 命令片段 onto the input line of the SSH
    /// terminal in front, run with `run`; the terminal takes the keyboard,
    /// to edit it or carry on. Why not, when it cannot, is a notification.
    ///
    /// It takes the place of what is typed there, but on Windows, whose
    /// shells do not read Ctrl-E and Ctrl-U as line editing.
    pub(super) fn on_enter_command(
        &mut self,
        action: &EnterCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let Some(view) = terminal.view.upgrade() else {
            return;
        };
        let replace = terminal.os != Some(HostOs::Windows);
        let EnterCommand { command, run } = action;
        match view.update(cx, |view, cx| {
            view.enter_command(command, *run, replace, cx)
        }) {
            Ok(()) => {
                let focus = view.read(cx).focus_handle();
                window.focus(&focus, cx);
                // Only the snippets and the history put commands in.
                self.count(match self.tools.read(cx).tool() {
                    ToolKind::History => Counter::CommandHistory,
                    _ => Counter::CommandSnippet,
                });
            }
            Err(why) => {
                let message = if *run {
                    t!("tools.command.run_failed", error = why)
                } else {
                    t!("tools.command.enter_failed", error = why)
                };
                window.push_notification(Notification::error(message), cx);
            }
        }
    }

    /// 历史命令's 复制.
    pub(super) fn on_copy_command(
        &mut self,
        action: &CopyCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(action.0.clone()));
        window.push_notification(Notification::success(t!("tools.command.copied")), cx);
    }

    /// Docker's 刷新: read the host's Docker again.
    pub(super) fn on_refresh_docker(
        &mut self,
        _: &RefreshDocker,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools.update(cx, |tools, cx| tools.refresh_docker(cx));
    }

    /// A compose project's line: fold it away, or unfold it.
    pub(super) fn on_toggle_docker_project(
        &mut self,
        action: &ToggleDockerProject,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tools
            .update(cx, |tools, cx| tools.toggle_docker_project(&action.0, cx));
    }

    /// A container's details and output, or a volume's, an image's or a
    /// network's details.
    pub(super) fn on_show_docker_object(
        &mut self,
        action: &ShowDockerObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let tools = self.tools.read(cx);
        let dispatch = self.focus_handle.clone();
        if action.object == DockerObject::Container {
            if let Some(container) = tools.container(&action.id, cx) {
                open_container_dialog(container, terminal.view, dispatch, window, cx);
            }
        } else if let Some(summary) = tools.docker_summary(action.object, &action.id, cx) {
            open_object_dialog(summary, terminal.view, dispatch, window, cx);
        }
    }

    /// Start, stop or restart containers, asking first before stopping or
    /// restarting them; how it went is a notification, and the list is
    /// read again.
    pub(super) fn on_control_containers(
        &mut self,
        action: &ControlContainers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let ControlContainers {
            subject,
            ids,
            command,
        } = action.clone();
        let workspace = cx.entity().downgrade();
        let run = Rc::new({
            let subject = subject.clone();
            move |window: &mut Window, cx: &mut App| {
                let failed = subject.clone();
                run_on_host(
                    HostCommand {
                        script: docker::control_command(&ids, command),
                        outcome: docker::done,
                        done: Some(containers_done(&subject, command)),
                        failed: Box::new(move |why| containers_failed(&failed, command, why)),
                        then: ToolSidebar::refresh_docker,
                    },
                    &terminal,
                    workspace.clone(),
                    window,
                    cx,
                )
            }
        });
        let (title, description) = match (&subject, command) {
            (_, ContainerCommand::Start) => return run(window, cx),
            (ContainerSubject::Container(name), ContainerCommand::Stop) => (
                t!("tools.container.stop.title", name = name),
                t!("tools.container.stop.description"),
            ),
            (ContainerSubject::Project(name), ContainerCommand::Stop) => (
                t!("tools.project.stop.title", name = name),
                t!("tools.project.stop.description"),
            ),
            (ContainerSubject::Container(name), ContainerCommand::Restart) => (
                t!("tools.container.restart.title", name = name),
                t!("tools.container.restart.description"),
            ),
            (ContainerSubject::Project(name), ContainerCommand::Restart) => (
                t!("tools.project.restart.title", name = name),
                t!("tools.project.restart.description"),
            ),
        };
        confirm_danger(title, Some(description), command.label(), run, window, cx);
    }

    /// Ask, then remove a container, an image, a volume or a network.
    pub(super) fn on_remove_docker_object(
        &mut self,
        action: &RemoveDockerObject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.tool_terminal(cx) else {
            return;
        };
        let RemoveDockerObject { object, id, name } = action.clone();
        let (title, description) = match object {
            DockerObject::Container => (
                t!("tools.remove.container.title", name = name),
                t!("tools.remove.container.description"),
            ),
            DockerObject::Image => (
                t!("tools.remove.image.title", name = name),
                t!("tools.remove.image.description"),
            ),
            DockerObject::Volume => (
                t!("tools.remove.volume.title", name = name),
                t!("tools.remove.volume.description"),
            ),
            DockerObject::Network => (
                t!("tools.remove.network.title", name = name),
                t!("tools.remove.network.description"),
            ),
        };
        let workspace = cx.entity().downgrade();
        confirm_danger(
            title,
            Some(description),
            t!("common.delete"),
            Rc::new(move |window, cx| {
                let failed = name.clone();
                run_on_host(
                    HostCommand {
                        script: docker::remove_command(object, &id),
                        outcome: docker::done,
                        done: Some(removed(object, &name)),
                        failed: Box::new(move |why| not_removed(object, &failed, why)),
                        then: ToolSidebar::refresh_docker,
                    },
                    &terminal,
                    workspace.clone(),
                    window,
                    cx,
                )
            }),
            window,
            cx,
        );
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
                let failed = name.clone();
                run_on_host(
                    HostCommand {
                        script: control_command(&name, command),
                        outcome: controlled,
                        done: Some(service_done(&name, command)),
                        failed: Box::new(move |why| service_failed(&failed, command, why)),
                        then: ToolSidebar::refresh_services,
                    },
                    &terminal,
                    workspace.clone(),
                    window,
                    cx,
                )
            }
        });
        // Stopping and restarting take something away, if only for a
        // moment; starting and the boot settings do not.
        let ssh = matches!(name.as_str(), "ssh.service" | "sshd.service");
        let (title, description) = match command {
            ServiceCommand::Stop => (
                t!("tools.service.stop.title", name = name),
                if ssh {
                    t!("tools.service.stop.description_ssh")
                } else {
                    t!("tools.service.stop.description")
                },
            ),
            ServiceCommand::Restart => (
                t!("tools.service.restart.title", name = name),
                t!("tools.service.restart.description"),
            ),
            _ => return run(window, cx),
        };
        confirm_danger(title, Some(description), command.label(), run, window, cx);
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
        let Some((name, user)) = self.tools.read(cx).process_label(pid, cx) else {
            return;
        };
        let user = user.unwrap_or_else(|| "—".into());
        let (title, description, verb) = if force {
            (
                t!("tools.process.kill.title", name = name),
                t!("tools.process.kill.description", pid = pid, user = user),
                t!("tools.process.kill.confirm"),
            )
        } else {
            (
                t!("tools.process.end.title", name = name),
                t!("tools.process.end.description", pid = pid, user = user),
                t!("tools.process.end.confirm"),
            )
        };
        let workspace = cx.entity().downgrade();
        confirm_danger(
            title,
            Some(description),
            verb,
            Rc::new(move |window, cx| {
                let Some(view) = terminal.view.upgrade() else {
                    return;
                };
                let Some(reply) = view.read(cx).exec(end_command(pid, force), cx) else {
                    window.push_notification(
                        Notification::error(t!(
                            "tools.process.end_failed",
                            name = name,
                            pid = pid,
                            error = t!("tools.not_connected")
                        )),
                        cx,
                    );
                    return;
                };
                let (workspace, name) = (workspace.clone(), name.clone());
                window
                    .spawn(cx, async move |cx| {
                        let outcome = match exec_answer(reply, cx).await {
                            None => Err(t!("tools.not_connected").to_string()),
                            Some(Err(error)) => Err(error),
                            Some(Ok(output)) => ended(&output),
                        };
                        cx.update(|window, cx| {
                            let notification = match outcome {
                                Ok(()) if force => Notification::success(t!(
                                    "tools.process.killed",
                                    name = name,
                                    pid = pid
                                )),
                                Ok(()) => Notification::success(t!(
                                    "tools.process.ended",
                                    name = name,
                                    pid = pid
                                )),
                                Err(why) => Notification::error(t!(
                                    "tools.process.end_failed",
                                    name = name,
                                    pid = pid,
                                    error = why
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
    /// offer when that one does not work on this terminal. Without a
    /// terminal in front there is nothing to show or hide.
    pub(super) fn on_toggle_tool_sidebar(
        &mut self,
        _: &ToggleToolSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offered = self.offered_tools(cx);
        let (Some(target), Some(&first)) = (self.sidebar_target(cx), offered.first()) else {
            return;
        };
        let tool = if self.dock_area.read(cx).is_dock_open(DockPlacement::Right) {
            None
        } else {
            let last = self.tools.read(cx).tool();
            Some(if offered.contains(&last) { last } else { first })
        };
        self.remember_target_tool(&target, tool, cx);
        self.sync_tool_sidebar(window, cx);
    }

    /// Point the sidebar at the terminal in front, and open it on the tool
    /// its host keeps (every local terminal's, for a local one) if one is
    /// and the tool works there; close it otherwise. Also when the host
    /// turns out to run something else.
    pub(super) fn sync_tool_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.sidebar_target(cx);
        let tool = target
            .as_ref()
            .and_then(|target| self.target_tool(target, cx))
            .filter(|tool| self.offered_tools(cx).contains(tool));
        if let Some(tool) = tool {
            self.tools
                .update(cx, |tools, cx| tools.show(tool, window, cx));
        }
        let open = tool.is_some();
        // A terminal's 文件 is made the first time it is shown, and kept.
        // 目录跟随's check goes first: the terminal's channels open one at a
        // time, so it is answered before 文件's opens and 文件 starts out in
        // the terminal's folder.
        let files = match &target {
            Some(SidebarTarget::Remote(terminal))
                if open && self.tools.read(cx).tool() == ToolKind::Files =>
            {
                self.check_directory_follow(terminal.id, window, cx);
                self.ensure_file_sidebar(terminal.id, window, cx)
            }
            _ => None,
        };
        self.tools.update(cx, |tools, cx| {
            tools.set_files(files, window, cx);
            tools.set_target(target, open, cx);
        });
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
        // 撰写窗格 sends to the same terminal.
        self.sync_compose(window, cx);
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

/// A command a tool runs on the host, and what follows it.
pub(super) struct HostCommand {
    /// `None` when what it names cannot go into a command.
    pub script: Option<String>,
    /// How it went, from what it printed.
    pub outcome: fn(&str) -> Result<(), String>,
    /// The notification when it went: 「nginx.service 已停止」; none when
    /// what it did shows by itself.
    pub done: Option<SharedString>,
    /// The notification when it did not, from why: 「无法停止
    /// nginx.service：…」.
    pub failed: Box<dyn FnOnce(&str) -> SharedString>,
    /// What the sidebar does after: read the tool's list again.
    pub then: fn(&mut ToolSidebar, &mut Context<ToolSidebar>),
}

/// Run `command` on the SSH terminal's own connection; how it went is a
/// notification, and then the sidebar does what follows.
pub(super) fn run_on_host(
    command: HostCommand,
    terminal: &ToolTerminal,
    workspace: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut App,
) {
    let HostCommand {
        script,
        outcome,
        done,
        failed,
        then,
    } = command;
    let reply = script.and_then(|script| terminal.view.upgrade()?.read(cx).exec(script, cx));
    let Some(reply) = reply else {
        window.push_notification(Notification::error(failed(&t!("tools.not_connected"))), cx);
        return;
    };
    window
        .spawn(cx, async move |cx| {
            let result = match exec_answer(reply, cx).await {
                None => Err(t!("tools.not_connected").to_string()),
                Some(Err(error)) => Err(error),
                Some(Ok(output)) => outcome(&output),
            };
            cx.update(|window, cx| {
                let notification = match result {
                    Ok(()) => done.map(Notification::success),
                    Err(why) => Some(Notification::error(failed(&why))),
                };
                if let Some(notification) = notification {
                    window.push_notification(notification, cx);
                }
                workspace
                    .update(cx, |this, cx| this.tools.update(cx, then))
                    .ok();
            })
            .ok();
        })
        .detach();
}

/// The notification when containers did as `command` said.
fn containers_done(subject: &ContainerSubject, command: ContainerCommand) -> SharedString {
    match (subject, command) {
        (ContainerSubject::Container(name), ContainerCommand::Start) => {
            t!("tools.container.start.done", name = name)
        }
        (ContainerSubject::Container(name), ContainerCommand::Stop) => {
            t!("tools.container.stop.done", name = name)
        }
        (ContainerSubject::Container(name), ContainerCommand::Restart) => {
            t!("tools.container.restart.done", name = name)
        }
        (ContainerSubject::Project(name), ContainerCommand::Start) => {
            t!("tools.project.start.done", name = name)
        }
        (ContainerSubject::Project(name), ContainerCommand::Stop) => {
            t!("tools.project.stop.done", name = name)
        }
        (ContainerSubject::Project(name), ContainerCommand::Restart) => {
            t!("tools.project.restart.done", name = name)
        }
    }
}

/// The notification when they did not, and why.
fn containers_failed(
    subject: &ContainerSubject,
    command: ContainerCommand,
    why: &str,
) -> SharedString {
    match (subject, command) {
        (ContainerSubject::Container(name), ContainerCommand::Start) => {
            t!("tools.container.start.failed", name = name, error = why)
        }
        (ContainerSubject::Container(name), ContainerCommand::Stop) => {
            t!("tools.container.stop.failed", name = name, error = why)
        }
        (ContainerSubject::Container(name), ContainerCommand::Restart) => {
            t!("tools.container.restart.failed", name = name, error = why)
        }
        (ContainerSubject::Project(name), ContainerCommand::Start) => {
            t!("tools.project.start.failed", name = name, error = why)
        }
        (ContainerSubject::Project(name), ContainerCommand::Stop) => {
            t!("tools.project.stop.failed", name = name, error = why)
        }
        (ContainerSubject::Project(name), ContainerCommand::Restart) => {
            t!("tools.project.restart.failed", name = name, error = why)
        }
    }
}

/// The notification when a container, an image, a volume or a network went.
fn removed(object: DockerObject, name: &str) -> SharedString {
    match object {
        DockerObject::Container => t!("tools.remove.container.done", name = name),
        DockerObject::Image => t!("tools.remove.image.done", name = name),
        DockerObject::Volume => t!("tools.remove.volume.done", name = name),
        DockerObject::Network => t!("tools.remove.network.done", name = name),
    }
}

/// The notification when it did not, and why.
fn not_removed(object: DockerObject, name: &str, why: &str) -> SharedString {
    match object {
        DockerObject::Container => t!("tools.remove.container.failed", name = name, error = why),
        DockerObject::Image => t!("tools.remove.image.failed", name = name, error = why),
        DockerObject::Volume => t!("tools.remove.volume.failed", name = name, error = why),
        DockerObject::Network => t!("tools.remove.network.failed", name = name, error = why),
    }
}

/// The notification when a service did as `command` said.
fn service_done(name: &str, command: ServiceCommand) -> SharedString {
    match command {
        ServiceCommand::Start => t!("tools.service.start.done", name = name),
        ServiceCommand::Stop => t!("tools.service.stop.done", name = name),
        ServiceCommand::Restart => t!("tools.service.restart.done", name = name),
        ServiceCommand::Enable => t!("tools.service.enable.done", name = name),
        ServiceCommand::Disable => t!("tools.service.disable.done", name = name),
    }
}

/// The notification when it did not, and why.
fn service_failed(name: &str, command: ServiceCommand, why: &str) -> SharedString {
    match command {
        ServiceCommand::Start => t!("tools.service.start.failed", name = name, error = why),
        ServiceCommand::Stop => t!("tools.service.stop.failed", name = name, error = why),
        ServiceCommand::Restart => t!("tools.service.restart.failed", name = name, error = why),
        ServiceCommand::Enable => t!("tools.service.enable.failed", name = name, error = why),
        ServiceCommand::Disable => t!("tools.service.disable.failed", name = name, error = why),
    }
}
