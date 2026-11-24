use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use crate::shared::RightClickMenu as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    menu::PopupMenu,
    progress::Progress,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::linux::{self, Parsed};
use super::model::{
    Concern, Device, GpuProcess, Health, Reading, Vendor, format_memory, format_power,
    format_power_limit, format_process_memory, format_share, format_temperature, heat,
};
use crate::app::{CatalogIcon, EndProcess, RefreshGpu, ToggleAllGpuDevices, ToggleGpuDevice};
use crate::i18n::{t, tn};
use crate::shared::format_duration;
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// Time between readings, from the end of one to the start of the next: a
/// slow `npu-smi` slows the readings down rather than piling them up.
const INTERVAL: Duration = Duration::from_secs(2);
/// How long a terminal not connected is left before it is asked again.
const RETRY: Duration = Duration::from_secs(2);

/// GPU/NPU: the accelerators of the host of the SSH terminal in front, read
/// every two seconds while the panel shows. Laid out for a host with eight:
/// the figures over all of them at the top, then three lines a device, each
/// unfolding to its details, then the processes computing on them, which a
/// right click ends.
///
/// Each reading runs `linux::command` beside the shell, on the terminal's
/// own connection: no second login, nothing asked again. Nothing runs on
/// the host while the panel is hidden. What each terminal's host showed
/// last is kept, so coming back to a terminal shows its devices at once.
///
/// A reading that fails, or a tool that has to be stopped, stops the
/// readings until 刷新: a GPU fallen off the bus can leave `nvidia-smi`
/// stuck in the driver past being killed, and reading on would leave one
/// more stuck every two seconds.
pub struct GpuPanel {
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// The readings; dropping it stops them.
    sampling: Option<Task<()>>,
    /// Whether a reading 刷新 asked for is on its way: the button turns
    /// for that one, not for every reading.
    refreshing: bool,
    /// 全部展开 or 全部收起, the last one used; until then a host of one
    /// device shows it unfolded, of more folded.
    all_unfolded: Option<bool>,
    /// The devices folded the other way since, by key.
    toggled: HashSet<String>,
    /// The process a right click landed on, for the menu; `None` off every
    /// process.
    menu_hit: Rc<Cell<Option<u32>>>,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    focus_handle: FocusHandle,
}

#[derive(Default)]
struct HostState {
    reading: Option<Rc<Reading>>,
    problem: Option<Problem>,
    /// The readings stopped after a failure, until 刷新.
    paused: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Problem {
    /// The terminal is not connected, so there is no connection to read on.
    NotConnected,
    /// Neither `nvidia-smi` nor `npu-smi` is on the host.
    NoTools,
    /// The command did not run, or did not finish.
    Failed(String),
}

impl GpuPanel {
    pub fn new(dispatch: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            target: None,
            active: false,
            hosts: HashMap::new(),
            sampling: None,
            refreshing: false,
            all_unfolded: None,
            toggled: HashSet::new(),
            menu_hit: Rc::new(Cell::new(None)),
            dispatch,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Read `target`'s host while `active`, the panel showing it.
    pub fn set_target(&mut self, target: Option<ExecTarget>, active: bool, cx: &mut Context<Self>) {
        if self.target == target && self.active == active {
            return;
        }
        self.target = target;
        self.active = active;
        self.restart(None, cx);
        cx.notify();
    }

    /// Read the host now, and every two seconds from now: 刷新, which also
    /// takes up the readings after a failure stopped them.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.restart(None, cx);
        self.refreshing = self.sampling.is_some();
        cx.notify();
    }

    /// Read the host in a moment: a process just told to end takes that
    /// long to let go of its device.
    pub fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        self.restart(Some(Duration::from_secs(1)), cx);
    }

    /// Unfold a device's details, or fold them away again.
    pub fn toggle(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.toggled.remove(key) {
            self.toggled.insert(key.to_owned());
        }
        cx.notify();
    }

    /// 全部展开, or 全部收起 when every device is unfolded already.
    pub fn toggle_all(&mut self, cx: &mut Context<Self>) {
        let Some(reading) = self.reading() else {
            return;
        };
        let every = reading
            .devices
            .iter()
            .all(|device| self.unfolded(&reading, device));
        self.all_unfolded = Some(!every);
        self.toggled.clear();
        cx.notify();
    }

    fn reading(&self) -> Option<Rc<Reading>> {
        let target = self.target.as_ref()?;
        self.hosts.get(&target.terminal)?.reading.clone()
    }

    /// A process of the host on screen, as the panel last showed it: its
    /// program's name and its owner, for the question before ending it.
    pub fn process(&self, pid: u32) -> Option<(String, Option<String>)> {
        let reading = self.reading()?;
        let process = reading.processes.iter().find(|p| p.pid == pid)?;
        let program = process.name.rsplit('/').next().unwrap_or(&process.name);
        Some((program.to_owned(), process.user.clone()))
    }

    /// Start reading the target's host over, after `delay` if any, or stop
    /// when there is nothing to show. Dropping the task stops it; a reading
    /// still on its way is answered to no one.
    fn restart(&mut self, delay: Option<Duration>, cx: &mut Context<Self>) {
        self.sampling = None;
        self.refreshing = false;
        let Some(target) = self.target.clone().filter(|_| self.active) else {
            return;
        };
        if let Some(host) = self.hosts.get_mut(&target.terminal) {
            host.paused = false;
        }
        self.sampling = Some(cx.spawn(async move |this, cx| {
            if let Some(delay) = delay {
                cx.background_executor().timer(delay).await;
            }
            loop {
                let request = this.update(cx, |_, cx| {
                    target.view.upgrade()?.read(cx).exec(linux::command(), cx)
                });
                let Ok(request) = request else { break };
                let answer = match request {
                    None => None,
                    Some(reply) => exec_answer(reply, cx).await,
                };
                let Ok(Some(pause)) =
                    this.update(cx, |this, cx| this.record(target.terminal, answer, cx))
                else {
                    break;
                };
                cx.background_executor().timer(pause).await;
            }
        }));
    }

    /// Keep what a reading came back with; how long until the next, `None`
    /// for no next.
    fn record(
        &mut self,
        terminal: RemoteTerminalId,
        answer: Option<ExecResult>,
        cx: &mut Context<Self>,
    ) -> Option<Duration> {
        self.refreshing = false;
        cx.notify();
        let host = self.hosts.entry(terminal).or_default();
        let output = match answer {
            None => {
                host.problem = Some(Problem::NotConnected);
                return Some(RETRY);
            }
            Some(Err(error)) => {
                host.problem = Some(Problem::Failed(error));
                host.paused = true;
                return None;
            }
            Some(Ok(output)) => output,
        };
        match linux::parse(&output) {
            Parsed::Reading(reading) => {
                host.paused = reading.timed_out;
                host.reading = Some(Rc::new(*reading));
                host.problem = None;
                (!host.paused).then_some(INTERVAL)
            }
            // No tool turns up by itself: showing the panel again looks
            // again.
            Parsed::NoTools => {
                host.reading = None;
                host.problem = Some(Problem::NoTools);
                None
            }
        }
    }

    fn unfolded(&self, reading: &Reading, device: &Device) -> bool {
        let all = self.all_unfolded.unwrap_or(reading.devices.len() == 1);
        all != self.toggled.contains(&device.key)
    }

    fn render_reading(
        &self,
        reading: &Reading,
        problem: Option<&Problem>,
        paused: bool,
        cx: &App,
    ) -> impl IntoElement {
        let mut summary = tn!("gpu.panel.summary", reading.devices.len()).to_string();
        if let Some(uptime) = reading.uptime {
            summary += " · ";
            summary += &t!("gpu.panel.uptime", duration = format_duration(uptime));
        }
        let refresh = self.dispatch.clone();
        let model = reading.model().map(str::to_owned);
        let drivers = (!reading.drivers.is_empty())
            .then(|| t!("gpu.panel.driver", version = reading.drivers.join(", ")));
        let about = match (model, drivers) {
            (Some(model), Some(drivers)) => Some(format!("{model} · {drivers}")),
            (Some(model), None) => Some(model),
            (None, drivers) => drivers.map(Into::into),
        };
        let refresh_tooltip = t!("gpu.panel.refresh", seconds = INTERVAL.as_secs());
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("gpu-model")
                                    .test_support()
                                    .when_some(about.clone(), |line, about| line.aria_label(about))
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .children(about),
                            )
                            .child(
                                Button::new("gpu-refresh")
                                    .ghost()
                                    .xsmall()
                                    .icon(Icon::new(CatalogIcon::RefreshCw))
                                    // Turning for 刷新 alone: every two
                                    // seconds would be a light always on.
                                    .loading(self.refreshing)
                                    .tooltip(refresh_tooltip.clone())
                                    .accessibility_label(refresh_tooltip)
                                    .on_click(move |_, window, cx| {
                                        refresh.dispatch_action(&RefreshGpu, window, cx)
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("gpu-summary")
                            .test_support()
                            .aria_label(summary.clone())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(summary),
                    ),
            )
            .when(paused, |panel| {
                let text = t!("gpu.panel.paused");
                panel.child(
                    div()
                        .id("gpu-paused")
                        .test_support()
                        .aria_label(text.clone())
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(text),
                )
            })
            .when_some(problem, |panel, problem| {
                panel.child(render_problem(problem, cx))
            })
            .when(!reading.errors.is_empty(), |panel| {
                let text = reading.errors.join("\n");
                panel.child(
                    div()
                        .id("gpu-errors")
                        .test_support()
                        .aria_label(text.clone())
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(text),
                )
            })
            .map(|panel| {
                if reading.devices.is_empty() {
                    if !reading.errors.is_empty() {
                        return panel;
                    }
                    let text = t!("gpu.panel.no_devices");
                    return panel.child(
                        div()
                            .id("gpu-no-devices")
                            .test_support()
                            .aria_label(text.clone())
                            .py_4()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(text),
                    );
                }
                panel
                    .child(render_overview(reading, cx))
                    .child(self.render_devices(reading, cx))
                    .child(self.render_processes(reading, cx))
            })
    }

    fn render_devices(&self, reading: &Reading, cx: &App) -> impl IntoElement {
        let every = reading
            .devices
            .iter()
            .all(|device| self.unfolded(reading, device));
        let (label, icon) = if every {
            (
                t!("gpu.devices.fold_all"),
                Icon::new(CatalogIcon::ChevronsDownUp),
            )
        } else {
            (
                t!("gpu.devices.unfold_all"),
                Icon::new(IconName::ChevronsUpDown),
            )
        };
        let dispatch = self.dispatch.clone();
        v_flex()
            .id("gpu-devices")
            .test_support()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("gpu.devices.title")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(reading.devices.len().to_string()),
                    )
                    // With one device there is nothing to do all at once.
                    .when(reading.devices.len() > 1, |header| {
                        header.child(
                            Button::new("gpu-devices-fold")
                                .ghost()
                                .xsmall()
                                .icon(icon)
                                .label(label)
                                .on_click(move |_, window, cx| {
                                    dispatch.dispatch_action(&ToggleAllGpuDevices, window, cx)
                                }),
                        )
                    }),
            )
            // One model for all of them is in the header already.
            .children(reading.devices.iter().enumerate().map(|(index, device)| {
                render_device(
                    index,
                    device,
                    reading.model().is_none(),
                    self.unfolded(reading, device),
                    &self.dispatch,
                    cx,
                )
            }))
    }

    fn render_processes(&self, reading: &Reading, cx: &App) -> impl IntoElement {
        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();
        v_flex()
            .id("gpu-processes")
            .test_support()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(t!("gpu.processes.title")),
                    )
                    .child(
                        div()
                            .id("gpu-processes-count")
                            .test_support()
                            .aria_label(reading.processes.len().to_string())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(reading.processes.len().to_string()),
                    ),
            )
            .map(|list| {
                if reading.processes.is_empty() {
                    let text = t!("gpu.processes.empty");
                    list.child(
                        div()
                            .id("gpu-no-processes")
                            .test_support()
                            .aria_label(text.clone())
                            .py_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(text),
                    )
                } else {
                    list.children(reading.processes.iter().map(|process| {
                        let device = process
                            .device
                            .and_then(|index| reading.devices.get(index))
                            .map_or("—", |device| device.label.as_str());
                        render_process(
                            process,
                            device,
                            self.menu_hit.clone(),
                            self.focus_handle.clone(),
                            cx,
                        )
                    }))
                }
            })
            // The menu hangs off the list, not off the rows, as in 进程管理.
            .capture_any_mouse_down(move |event, _, _| {
                if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                    clear_hit.set(None);
                }
            })
            .context_menu(move |menu, _, _| build_context_menu(menu_hit.get(), menu))
    }
}

impl Focusable for GpuPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GpuPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self
            .target
            .as_ref()
            .and_then(|target| self.hosts.get(&target.terminal));
        let reading = host.and_then(|host| host.reading.clone());
        let problem = host.and_then(|host| host.problem.clone());
        let paused = host.is_some_and(|host| host.paused);
        // The scrollbar goes on a layer beside the scrolled content, which
        // `overflow_y_scrollbar` lays out (see the system monitor).
        let content = v_flex()
            .size_full()
            .p_3()
            .map(|panel| match (reading, problem) {
                // Devices from before a failed reading still say something;
                // the failure goes above them.
                (Some(reading), problem @ (None | Some(Problem::Failed(_)))) => {
                    panel.child(self.render_reading(&reading, problem.as_ref(), paused, cx))
                }
                (_, Some(problem)) => panel.child(render_problem(&problem, cx)),
                (None, None) => panel.child(
                    h_flex()
                        .id("gpu-loading")
                        .test_support()
                        .gap_2()
                        .justify_center()
                        .py_8()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(Spinner::new().small())
                        .child(t!("tools.reading")),
                ),
            })
            .overflow_y_scrollbar();
        div()
            .id("gpu")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(content)
    }
}

fn render_problem(problem: &Problem, cx: &App) -> impl IntoElement {
    let (text, color) = match problem {
        Problem::NotConnected => (t!("gpu.panel.not_connected"), cx.theme().muted_foreground),
        Problem::NoTools => (t!("gpu.panel.no_tools"), cx.theme().muted_foreground),
        Problem::Failed(error) => (t!("tools.read_failed", error = error), cx.theme().danger),
    };
    div()
        .id("gpu-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

/// Numbers that change every reading: monospaced, so they hold still.
fn numeric(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .font_family(cx.theme().mono_font_family.clone())
        .child(text.into())
}

fn concern_color(concern: Concern, cx: &App) -> Option<Hsla> {
    match concern {
        Concern::None => None,
        Concern::Warning => Some(cx.theme().warning),
        Concern::Critical => Some(cx.theme().danger),
    }
}

/// The four figures over every device, two by two. The memory reads as a
/// share, its amount under it: a host's hundreds of GB do not fit a half.
fn render_overview(reading: &Reading, cx: &App) -> impl IntoElement {
    let summary = reading.summary();
    let field = |id: &'static str,
                 label: SharedString,
                 value: String,
                 color: Option<Hsla>,
                 under: Option<(&'static str, String)>| {
        v_flex()
            .min_w_0()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                numeric(value.clone(), cx)
                    .id(id)
                    .test_support()
                    .aria_label(value)
                    .truncate()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when_some(color, |value, color| value.text_color(color)),
            )
            .when_some(under, |field, (id, text)| {
                field.child(
                    numeric(text.clone(), cx)
                        .id(id)
                        .test_support()
                        .aria_label(text)
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground),
                )
            })
    };
    GroupBox::new().id("gpu-overview").fill().child(
        div()
            .grid()
            .grid_cols(2)
            .gap_x_4()
            .gap_y_3()
            .child(field(
                "gpu-utilization",
                t!("gpu.overview.utilization"),
                format_share(summary.utilization),
                None,
                None,
            ))
            .child(field(
                "gpu-memory",
                t!("gpu.overview.memory"),
                format_share(summary.memory_percent()),
                None,
                Some((
                    "gpu-memory-amount",
                    format_memory(summary.memory_used, summary.memory_total),
                )),
            ))
            .child(field(
                "gpu-temperature",
                t!("gpu.overview.temperature"),
                format_temperature(summary.temperature),
                summary
                    .temperature
                    .and_then(|celsius| concern_color(heat(celsius), cx)),
                None,
            ))
            .child(field(
                "gpu-power",
                t!("gpu.overview.power"),
                format_power(summary.power),
                None,
                None,
            )),
    )
}

/// A line of a device: what is measured, its bar and its figure. The name
/// and the figure have widths of their own, so the bars of every line start
/// and end together.
fn bar(
    id: SharedString,
    label: SharedString,
    percent: Option<f32>,
    text: String,
    color: Hsla,
    cx: &App,
) -> Div {
    h_flex()
        .pl_4()
        .gap_2()
        .text_xs()
        .child(
            div()
                .w_12()
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(
            div().flex_1().min_w_8().child(
                Progress::new(SharedString::from(format!("{id}-bar")))
                    .small()
                    .value(percent.unwrap_or(0.))
                    .color(color),
            ),
        )
        .child(
            numeric(text.clone(), cx)
                .id(id)
                .test_support()
                .aria_label(text)
                .min_w_24()
                .flex_shrink_0()
                .text_right(),
        )
}

/// A device in three lines: its label, model, heat and power, then how busy
/// it is, then its memory. A click unfolds its details.
fn render_device(
    index: usize,
    device: &Device,
    named: bool,
    unfolded: bool,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let utilization = format_share(device.utilization);
    let memory = format_memory(device.memory_used, device.memory_total);
    let temperature = format_temperature(device.temperature);
    let power = format_power(device.power);
    let label = t!(
        "gpu.device.label",
        device = device.label,
        name = device.name,
        utilization = utilization,
        memory = memory,
        temperature = temperature,
        power = power
    );
    let health = health_text(&device.health);
    let health_color = concern_color(device.health.concern(), cx);
    let heat_color = device
        .temperature
        .and_then(|celsius| concern_color(heat(celsius), cx));
    let key = device.key.clone();
    let dispatch = dispatch.clone();
    let muted = cx.theme().muted_foreground;
    v_flex()
        .id(("gpu-device", index as u64))
        .test_support()
        .aria_label(label)
        .gap_1p5()
        .p_2()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .text_color(cx.theme().group_box_foreground)
        // A click unfolds it; the border says it can.
        .border_1()
        .border_color(cx.theme().tokens.group_box)
        .hover({
            let border = cx.theme().border;
            move |style| style.border_color(border)
        })
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            dispatch.dispatch_action(&ToggleGpuDevice(key.clone()), window, cx)
        })
        .child(
            h_flex()
                .gap_1p5()
                .text_sm()
                .child(
                    Icon::new(if unfolded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall()
                    .text_color(muted),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .font_weight(FontWeight::MEDIUM)
                        .child(device.label.clone()),
                )
                .when_some(health_color, |line, color| {
                    let health = health.clone();
                    line.child(
                        div()
                            .id(("gpu-device-health", index as u64))
                            .test_support()
                            .aria_label(health.clone())
                            .flex_shrink_0()
                            .size_2()
                            .rounded_full()
                            .bg(color)
                            .tooltip(move |window, cx| {
                                Tooltip::new(health.clone()).build(window, cx)
                            }),
                    )
                })
                // Cut short if it must, whole when pointed at.
                .child(div().flex_1().min_w_0().when(named, |cell| {
                    let name = SharedString::from(device.name.clone());
                    cell.child(
                        div()
                            .id(("gpu-device-name", index as u64))
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(name.clone())
                            .tooltip(move |window, cx| {
                                Tooltip::new(name.clone()).build(window, cx)
                            }),
                    )
                }))
                .child(
                    numeric(temperature.clone(), cx)
                        .id(("gpu-device-temperature", index as u64))
                        .test_support()
                        .aria_label(temperature)
                        .flex_shrink_0()
                        .text_xs()
                        .when_some(heat_color, |text, color| text.text_color(color)),
                )
                .child(
                    numeric(power, cx)
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(muted),
                ),
        )
        .child(bar(
            SharedString::from(format!("gpu-device-utilization-{index}")),
            t!("gpu.devices.utilization"),
            device.utilization,
            utilization,
            cx.theme().success,
            cx,
        ))
        .child(bar(
            SharedString::from(format!("gpu-device-memory-{index}")),
            t!("gpu.devices.memory"),
            device.memory_percent(),
            memory,
            cx.theme().info,
            cx,
        ))
        .when(unfolded, |row| {
            row.child(render_details(index, device, health, cx))
        })
}

fn health_text(health: &Health) -> SharedString {
    match health {
        Health::Ok => t!("gpu.health.ok"),
        Health::EccErrors(count) => tn!("gpu.health.ecc", *count as usize),
        Health::Reported(word) => word.clone().into(),
        Health::Unknown => "—".into(),
    }
}

/// What a device's row unfolds to: what it is and how it is.
fn render_details(
    index: usize,
    device: &Device,
    health: SharedString,
    cx: &App,
) -> impl IntoElement {
    // Each with a name for the tests, which read the values by it.
    let mut fields: Vec<(&'static str, SharedString, String)> =
        vec![("model", t!("gpu.detail.model"), device.name.clone())];
    if let Some(uuid) = &device.uuid {
        fields.push(("uuid", "UUID".into(), uuid.clone()));
    }
    if let Some(bus_id) = &device.bus_id {
        fields.push(("bus", t!("gpu.detail.bus"), bus_id.clone()));
    }
    if let Some(pstate) = &device.pstate {
        fields.push(("pstate", t!("gpu.detail.pstate"), pstate.clone()));
    }
    fields.push((
        "power",
        t!("gpu.detail.power"),
        format_power_limit(device.power, device.power_limit),
    ));
    if let Some(fan) = device.fan {
        fields.push(("fan", t!("gpu.detail.fan"), format_share(Some(fan))));
    }
    if device.vendor == Vendor::Nvidia || device.health != Health::Unknown {
        fields.push(("health", t!("gpu.detail.health"), health.to_string()));
    }
    v_flex()
        .id(("gpu-device-details", index as u64))
        .test_support()
        .pl_4()
        .pt_1()
        .gap_1()
        .text_xs()
        .children(fields.into_iter().map(|(name, label, value)| {
            h_flex()
                .gap_2()
                .child(
                    div()
                        .w_20()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("gpu-detail:{index}:{name}")))
                        .test_support()
                        .aria_label(value.clone())
                        .flex_1()
                        .min_w_0()
                        .font_family(cx.theme().mono_font_family.clone())
                        .child(value),
                )
        }))
}

/// A process's card: its command line and memory, then its device, PID
/// and owner. A right click puts the menu on it.
fn render_process(
    process: &GpuProcess,
    device: &str,
    menu_hit: Rc<Cell<Option<u32>>>,
    focus: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let pid = process.pid;
    let user = process.user.clone().unwrap_or_else(|| "—".into());
    let memory = format_process_memory(process.memory);
    let label = t!(
        "gpu.process.label",
        title = process.title(),
        device = device,
        pid = pid,
        user = user,
        memory = memory
    );
    v_flex()
        .id(("gpu-process", pid as u64))
        .test_support()
        .aria_label(label)
        .gap_0p5()
        .p_2()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .text_color(cx.theme().group_box_foreground)
        // The menu's commands are dispatched from what is focused, which
        // may be nothing.
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            menu_hit.set(Some(pid));
            focus.focus(window, cx);
        })
        .child(
            h_flex()
                .gap_2()
                // A command line rarely fits: whole when pointed at.
                .child({
                    let title = SharedString::from(process.title().to_owned());
                    div()
                        .id(("gpu-process-title", pid as u64))
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(title.clone())
                        .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                })
                .child(
                    numeric(memory, cx)
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().info),
                ),
        )
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{device} · PID {pid} · {user}")),
        )
}

fn build_context_menu(hit: Option<u32>, menu: PopupMenu) -> PopupMenu {
    let Some(pid) = hit else {
        return menu;
    };
    menu.menu_with_icon(
        t!("processes.menu.end"),
        Icon::new(CatalogIcon::CircleStop),
        Box::new(EndProcess { pid, force: false }),
    )
    .menu_with_icon(
        t!("processes.menu.kill"),
        Icon::new(CatalogIcon::OctagonX),
        Box::new(EndProcess { pid, force: true }),
    )
}
