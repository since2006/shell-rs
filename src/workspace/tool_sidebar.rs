use gpui_kit::component::{
    ActiveTheme as _, Icon, Placement, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{CatalogIcon, DOCKER_ICON, ToggleTool, ToolKind};
use crate::docker::{Container, DockerObject, DockerPanel, ObjectSummary};
use crate::host::{HostId, HostOs, HostStore};
use crate::monitor::{MonitorDetail, MonitorPanel};
use crate::netstat::NetstatPanel;
use crate::processes::{Process, ProcessDetails, ProcessPanel, ProcessSort};
use crate::services::{Service, ServicePanel};
use crate::terminal::{ExecTarget, RemoteTerminalId, TerminalView};

impl ToolKind {
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Snippets => "命令片段",
            ToolKind::History => "历史命令",
            ToolKind::Docker => "Docker",
            ToolKind::Services => "系统服务",
            ToolKind::Processes => "进程管理",
            ToolKind::Connections => "网络连接",
            ToolKind::Monitor => "系统监控",
        }
    }

    /// Lucide glyphs, but for Docker, which goes by its whale: a box said
    /// nothing about Docker.
    fn icon(self) -> Icon {
        Icon::new(match self {
            ToolKind::Snippets => CatalogIcon::CodeXml,
            ToolKind::History => CatalogIcon::RotateCcwClock,
            ToolKind::Docker => return Icon::default().path(DOCKER_ICON),
            ToolKind::Services => CatalogIcon::ServerCog,
            ToolKind::Processes => CatalogIcon::ListFilter,
            ToolKind::Connections => CatalogIcon::Network,
            ToolKind::Monitor => CatalogIcon::Activity,
        })
    }

    /// Whether the tool works on a host running `os`. A host not identified
    /// yet gets every tool, and is told if one cannot work there.
    pub fn works_on(self, os: Option<HostOs>) -> bool {
        match self {
            ToolKind::Services
            | ToolKind::Processes
            | ToolKind::Connections
            | ToolKind::Monitor => os.is_none_or(HostOs::is_linux),
            ToolKind::Snippets | ToolKind::History => true,
            // Wherever `sh` runs `docker`: Linux, and a Mac with Docker Desktop.
            ToolKind::Docker => os != Some(HostOs::Windows),
        }
    }

    /// The id of the tool's button in the switch.
    fn button_id(self) -> &'static str {
        match self {
            ToolKind::Snippets => "tool-snippets",
            ToolKind::History => "tool-history",
            ToolKind::Docker => "tool-docker",
            ToolKind::Services => "tool-services",
            ToolKind::Processes => "tool-processes",
            ToolKind::Connections => "tool-connections",
            ToolKind::Monitor => "tool-monitor",
        }
    }

    /// What a tool still to come is going to show for the terminal in
    /// front of `host`, said in its place.
    fn about(self, host: &str) -> String {
        match self {
            ToolKind::Snippets => "常用的命令存在这里，点一下就发送到当前终端。".into(),
            ToolKind::History => format!("{host} 上执行过的命令，可以搜索、再次执行。"),
            ToolKind::Docker => format!("{host} 上的容器：状态、日志，启动和停止。"),
            ToolKind::Services => format!("{host} 上的 systemd 服务：启动、停止、日志。"),
            ToolKind::Processes => format!("{host} 上的进程：内存、CPU，结束进程。"),
            ToolKind::Connections => format!("{host} 上的 TCP、UDP 连接和监听端口。"),
            ToolKind::Monitor => format!("{host} 的 CPU、内存、网络和磁盘。"),
        }
    }
}

/// The SSH terminal the right sidebar works on.
#[derive(Clone, PartialEq)]
pub struct ToolTerminal {
    pub id: RemoteTerminalId,
    pub host: HostId,
    /// Runs the tools' commands on the terminal's own connection.
    pub view: WeakEntity<TerminalView>,
}

/// The right dock's one panel: the tool the switch at the window's edge
/// picked, working on the SSH terminal in front. It is there only while one
/// is; the workspace hides it, switch and all, with any other tab.
///
/// One panel showing either tool, like the left dock's `Sidebar`, rather
/// than a panel per tool swapped in the dock: swapping takes the displaced
/// panel out of the dock and the focus with it, and two panels in one tab
/// group always draw a tab bar.
///
/// Each tool is a feature module's entity held here, alive while hidden so
/// it keeps its state, told which terminal is in front and whether it is on
/// screen; the tools still to come are placeholders.
pub struct ToolSidebar {
    tool: ToolKind,
    /// The terminal in front; `None` while the sidebar is hidden for
    /// another kind of tab.
    terminal: Option<ToolTerminal>,
    /// Whether the right dock is open.
    shown: bool,
    monitor: Entity<MonitorPanel>,
    docker: Entity<DockerPanel>,
    services: Entity<ServicePanel>,
    processes: Entity<ProcessPanel>,
    netstat: Entity<NetstatPanel>,
    store: Entity<HostStore>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl ToolSidebar {
    /// `dispatch` is the workspace's focus handle, which the tools' buttons
    /// dispatch on.
    pub fn new(
        store: Entity<HostStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // A host renamed is named anew.
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            tool: ToolKind::default(),
            terminal: None,
            shown: false,
            monitor: cx.new(|_| MonitorPanel::new(dispatch.clone())),
            docker: cx.new(|_| DockerPanel::new(dispatch.clone())),
            services: cx.new(|cx| ServicePanel::new(dispatch.clone(), window, cx)),
            processes: cx.new(|cx| ProcessPanel::new(dispatch.clone(), window, cx)),
            netstat: cx.new(|cx| NetstatPanel::new(dispatch, window, cx)),
            store,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    pub fn tool(&self) -> ToolKind {
        self.tool
    }

    /// Work on the SSH terminal now in front, and say whether the dock is
    /// open: a tool reads the host only while it is on screen.
    pub fn set_terminal(
        &mut self,
        terminal: Option<ToolTerminal>,
        shown: bool,
        cx: &mut Context<Self>,
    ) {
        if self.terminal != terminal || self.shown != shown {
            self.terminal = terminal;
            self.shown = shown;
            self.sync_tools(cx);
            cx.notify();
        }
    }

    pub fn toggle_monitor_detail(&mut self, detail: MonitorDetail, cx: &mut Context<Self>) {
        self.monitor
            .update(cx, |monitor, cx| monitor.toggle(detail, cx));
    }

    /// 网络连接's 刷新.
    pub fn refresh_connections(&mut self, cx: &mut Context<Self>) {
        self.netstat.update(cx, |netstat, cx| netstat.refresh(cx));
    }

    /// Docker's 刷新, and after a command.
    pub fn refresh_docker(&mut self, cx: &mut Context<Self>) {
        self.docker.update(cx, |docker, cx| docker.refresh(cx));
    }

    /// Fold a compose project away, or unfold it.
    pub fn toggle_docker_project(&mut self, name: &str, cx: &mut Context<Self>) {
        self.docker
            .update(cx, |docker, cx| docker.toggle_project(name, cx));
    }

    /// A container of the host in front, as Docker last showed it.
    pub fn container(&self, id: &str, cx: &App) -> Option<Container> {
        self.docker.read(cx).container(id)
    }

    /// A volume, an image or a network of the host in front, as Docker
    /// last showed it.
    pub fn docker_summary(
        &self,
        object: DockerObject,
        id: &str,
        cx: &App,
    ) -> Option<ObjectSummary> {
        self.docker.read(cx).summary_of(object, id)
    }

    /// 系统服务's 刷新, and after a command.
    pub fn refresh_services(&mut self, cx: &mut Context<Self>) {
        self.services
            .update(cx, |services, cx| services.refresh(cx));
    }

    /// A service of the host in front, as 系统服务 last showed it.
    pub fn service(&self, name: &str, cx: &App) -> Option<Service> {
        self.services.read(cx).service(name)
    }

    /// 进程管理's 刷新.
    pub fn refresh_processes(&mut self, cx: &mut Context<Self>) {
        self.processes
            .update(cx, |processes, cx| processes.refresh(cx));
    }

    /// Read the processes again in a moment, once one was told to end.
    pub fn refresh_processes_soon(&mut self, cx: &mut Context<Self>) {
        self.processes
            .update(cx, |processes, cx| processes.refresh_soon(cx));
    }

    /// 进程管理's sort buttons.
    pub fn sort_processes(&mut self, by: ProcessSort, cx: &mut Context<Self>) {
        self.processes
            .update(cx, |processes, cx| processes.sort(by, cx));
    }

    /// A process of the host in front, as 进程管理 last showed it.
    pub fn process(&self, pid: u32, cx: &App) -> Option<Process> {
        self.processes.read(cx).process(pid)
    }

    /// The same with its family, for its details.
    pub fn process_details(&self, pid: u32, cx: &App) -> Option<ProcessDetails> {
        self.processes.read(cx).details(pid)
    }

    /// Point each tool at the terminal, running only the one on screen.
    fn sync_tools(&mut self, cx: &mut Context<Self>) {
        let target = self.terminal.as_ref().map(|terminal| ExecTarget {
            terminal: terminal.id,
            view: terminal.view.clone(),
        });
        let showing = |tool| self.shown && self.tool == tool;
        let (monitor, netstat, processes, services, docker) = (
            showing(ToolKind::Monitor),
            showing(ToolKind::Connections),
            showing(ToolKind::Processes),
            showing(ToolKind::Services),
            showing(ToolKind::Docker),
        );
        self.docker
            .update(cx, |panel, cx| panel.set_target(target.clone(), docker, cx));
        self.services.update(cx, |panel, cx| {
            panel.set_target(target.clone(), services, cx)
        });
        self.monitor.update(cx, |panel, cx| {
            panel.set_target(target.clone(), monitor, cx)
        });
        self.processes.update(cx, |panel, cx| {
            panel.set_target(target.clone(), processes, cx)
        });
        self.netstat
            .update(cx, |panel, cx| panel.set_target(target, netstat, cx));
    }

    /// Show another tool. A focus inside the one going away moves to the
    /// sidebar itself: left on an element no longer drawn, it would take
    /// every window-level shortcut's dispatch path with it.
    pub fn show(&mut self, tool: ToolKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.tool == tool {
            return;
        }
        if self.focus_handle.contains_focused(window, cx) {
            window.focus(&self.focus_handle, cx);
        }
        self.tool = tool;
        self.sync_tools(cx);
        cx.notify();
    }

    fn render_placeholder(&self, host: HostId, cx: &App) -> impl IntoElement {
        let store = self.store.read(cx);
        let host = store.host(host).map(|host| host.name.clone());
        let about = self.tool.about(host.as_deref().unwrap_or_default());
        v_flex()
            .id("tool-placeholder")
            .test_support()
            .aria_label(about.clone())
            .items_center()
            .gap_2()
            .px_4()
            .py_8()
            .text_color(cx.theme().muted_foreground)
            .child(self.tool.icon().large())
            .child(div().text_sm().text_center().child(about))
            .child(div().text_xs().child("即将推出"))
    }
}

impl EventEmitter<PanelEvent> for ToolSidebar {}

impl Focusable for ToolSidebar {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for ToolSidebar {
    fn panel_name(&self) -> &'static str {
        "ToolSidebar"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for ToolSidebar {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(self.tool.icon().small())
            .child(self.tool.label())
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for ToolSidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Named after the terminal it works on.
        let id = self.terminal.as_ref().map_or(0, |terminal| terminal.id.0);
        v_flex()
            .id(("tool-sidebar", id))
            .test_support()
            .aria_label(self.tool.label())
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .when_some(self.terminal.as_ref(), |sidebar, terminal| {
                match self.tool {
                    ToolKind::Monitor => {
                        sidebar.child(div().flex_1().min_h_0().child(self.monitor.clone()))
                    }
                    ToolKind::Connections => {
                        sidebar.child(div().flex_1().min_h_0().child(self.netstat.clone()))
                    }
                    ToolKind::Processes => {
                        sidebar.child(div().flex_1().min_h_0().child(self.processes.clone()))
                    }
                    ToolKind::Services => {
                        sidebar.child(div().flex_1().min_h_0().child(self.services.clone()))
                    }
                    ToolKind::Docker => {
                        sidebar.child(div().flex_1().min_h_0().child(self.docker.clone()))
                    }
                    _ => sidebar.child(self.render_placeholder(terminal.host, cx)),
                }
            })
    }
}

/// The switch at the window's right edge: a button per tool `offered` for
/// the host in front, the one the sidebar is showing marked, none while it
/// is hidden.
///
/// The workspace draws it beside the dock area rather than in the right
/// dock, which goes off screen whole when it is hidden. The buttons do not
/// take the focus, so the terminal keeps the keyboard, and dispatch on
/// `target`, the workspace's focus handle, so they work whatever is focused.
pub fn render_tool_switch(
    showing: Option<ToolKind>,
    offered: &[ToolKind],
    target: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .id("tool-switch")
        .test_support()
        .flex_shrink_0()
        .h_full()
        .p_1()
        .gap_1()
        .bg(cx.theme().sidebar)
        .border_l_1()
        .border_color(cx.theme().sidebar_border)
        .children(offered.iter().map(|&tool| {
            let target = target.clone();
            Button::new(tool.button_id())
                .ghost()
                .icon(tool.icon())
                .selected(showing == Some(tool))
                // A switch: assistive technology hears it as pressed.
                .toggled(showing == Some(tool))
                .tooltip(tool.label())
                // Toward the window, not off its edge.
                .tooltip_placement(Placement::Left)
                .accessibility_label(tool.label())
                .on_click(move |_, window, cx| {
                    target.dispatch_action(&ToggleTool(tool), window, cx)
                })
        }))
}
