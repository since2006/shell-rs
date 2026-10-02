use gpui_kit::component::{
    ActiveTheme as _, Icon, Placement, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{CatalogIcon, ToggleTool, ToolKind};
use crate::host::{HostId, HostStore};
use crate::terminal::{LocalTerminalId, RemoteTerminalId};

impl ToolKind {
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Snippets => "命令片段",
            ToolKind::History => "历史命令",
            ToolKind::Docker => "Docker",
            ToolKind::Monitor => "系统监控",
        }
    }

    fn icon(self) -> CatalogIcon {
        match self {
            ToolKind::Snippets => CatalogIcon::CodeXml,
            ToolKind::History => CatalogIcon::RotateCcwClock,
            ToolKind::Docker => CatalogIcon::Container,
            ToolKind::Monitor => CatalogIcon::Activity,
        }
    }

    /// The id of the tool's button in the switch.
    fn button_id(self) -> &'static str {
        match self {
            ToolKind::Snippets => "tool-snippets",
            ToolKind::History => "tool-history",
            ToolKind::Docker => "tool-docker",
            ToolKind::Monitor => "tool-monitor",
        }
    }

    /// What the tool is going to show for the terminal in front, said while
    /// it is a placeholder. `machine` names the terminal's machine, spaced
    /// for the Chinese after it: 「web-01 」 or 「本机」.
    fn about(self, machine: &str) -> String {
        match self {
            ToolKind::Snippets => "常用的命令存在这里，点一下就发送到当前终端。".into(),
            ToolKind::History => format!("{machine}上执行过的命令，可以搜索、再次执行。"),
            ToolKind::Docker => format!("{machine}上的容器：状态、日志，启动和停止。"),
            ToolKind::Monitor => format!("{machine}的 CPU、内存、网络和磁盘。"),
        }
    }
}

/// The terminal the right sidebar works on: a host's over SSH, or a local
/// one on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolTerminal {
    Remote(RemoteTerminalId, HostId),
    Local(LocalTerminalId),
}

impl ToolTerminal {
    /// The sidebar's id, after the terminal's, the way the terminals'
    /// own are `("terminal", id)` and `("local-terminal", id)`.
    fn sidebar_id(self) -> ElementId {
        match self {
            ToolTerminal::Remote(id, _) => ("tool-sidebar", id.0).into(),
            ToolTerminal::Local(id) => ("local-tool-sidebar", id.0).into(),
        }
    }
}

/// The right dock's one panel: the tool the switch at the window's edge
/// picked, working on the terminal in front, remote or local. It is there
/// only while one is; the workspace hides it, switch and all, with any
/// other tab.
///
/// One panel showing either tool, like the left dock's `Sidebar`, rather
/// than a panel per tool swapped in the dock: swapping takes the displaced
/// panel out of the dock and the focus with it, and two panels in one tab
/// group always draw a tab bar.
///
/// The tools are placeholders for now. Each real one is to be a feature
/// module's entity held here, alive while hidden so it keeps its state, and
/// told which terminal is in front the way the placeholders are.
pub struct ToolSidebar {
    tool: ToolKind,
    /// The terminal in front; `None` while the sidebar is hidden for
    /// another kind of tab.
    terminal: Option<ToolTerminal>,
    store: Entity<HostStore>,
    focus_handle: FocusHandle,
    _subscription: Subscription,
}

impl ToolSidebar {
    pub fn new(store: Entity<HostStore>, cx: &mut Context<Self>) -> Self {
        // A host renamed is named anew.
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            tool: ToolKind::default(),
            terminal: None,
            store,
            focus_handle: cx.focus_handle(),
            _subscription: subscription,
        }
    }

    pub fn tool(&self) -> ToolKind {
        self.tool
    }

    /// Work on the terminal now in front.
    pub fn set_terminal(&mut self, terminal: Option<ToolTerminal>, cx: &mut Context<Self>) {
        if self.terminal != terminal {
            self.terminal = terminal;
            cx.notify();
        }
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
        cx.notify();
    }

    fn render_placeholder(&self, terminal: ToolTerminal, cx: &App) -> impl IntoElement {
        let machine = match terminal {
            ToolTerminal::Remote(_, host) => {
                let store = self.store.read(cx);
                let name = store.host(host).map(|host| host.name.clone());
                format!("{} ", name.unwrap_or_default())
            }
            ToolTerminal::Local(_) => "本机".into(),
        };
        let about = self.tool.about(&machine);
        v_flex()
            .id("tool-placeholder")
            .test_support()
            .aria_label(about.clone())
            .items_center()
            .gap_2()
            .px_4()
            .py_8()
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(self.tool.icon()).large())
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
            .child(Icon::new(self.tool.icon()).small())
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
        let id = self
            .terminal
            .map_or_else(|| "tool-sidebar".into(), ToolTerminal::sidebar_id);
        v_flex()
            .id(id)
            .test_support()
            .aria_label(self.tool.label())
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .when_some(self.terminal, |sidebar, terminal| {
                sidebar.child(self.render_placeholder(terminal, cx))
            })
    }
}

/// The switch at the window's right edge: a button per tool, the one the
/// sidebar is showing marked, none while it is hidden.
///
/// The workspace draws it beside the dock area rather than in the right
/// dock, which goes off screen whole when it is hidden. The buttons do not
/// take the focus, so the terminal keeps the keyboard, and dispatch on
/// `target`, the workspace's focus handle, so they work whatever is focused.
pub fn render_tool_switch(
    showing: Option<ToolKind>,
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
        .children(ToolKind::ALL.into_iter().map(|tool| {
            let target = target.clone();
            Button::new(tool.button_id())
                .ghost()
                .icon(Icon::new(tool.icon()))
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
