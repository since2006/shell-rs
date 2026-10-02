use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    group_box::{GroupBox, GroupBoxVariants as _},
    h_flex,
    progress::{Progress, ProgressCircle},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::MonitorDetail;
use super::linux::{self, Parsed, Parts};
use super::model::{
    DiskUsage, InterfaceRate, Load, Memory, Snapshot, SystemInfo, Tracker, bars_per_row,
    format_rate,
};
use crate::app::{CatalogIcon, ToggleMonitorDetail};
use crate::shared::{format_bytes, format_duration, format_percent};
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// Time between readings of the load. The second comes sooner: the CPU's
/// load and the network's rates need two, and the panel should not sit on
/// dashes.
const INTERVAL: Duration = Duration::from_secs(2);
const SECOND_READING: Duration = Duration::from_secs(1);
/// Disks are read this seldom: they fill slowly, and `df` is the slowest
/// part of a reading.
const DISK_INTERVAL: Duration = Duration::from_secs(30);
/// 系统监控: the CPU, memory, network and disks of the host of the SSH
/// terminal in front, read every two seconds while the panel shows.
///
/// Each reading runs `linux::command` beside the shell, on the terminal's
/// own connection: no second login, nothing asked again. What does not
/// change while connected is asked once per connection, the disks every
/// half minute, so most readings start no process on the host but the `sh`
/// running them. Nothing runs on the host while the panel is hidden. What
/// each terminal's host showed last is kept, so coming back to a terminal
/// shows its numbers at once.
pub struct MonitorPanel {
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// Every core's load, not only the first row of it.
    cores_unfolded: bool,
    /// How wide the row of per-core bars was last frame, which decides how
    /// many make the first row.
    cores_width: Rc<Cell<Option<Pixels>>>,
    /// Every interface, not only the main one.
    interfaces_unfolded: bool,
    /// The workspace's focus handle: the fold buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    sampling: Option<Task<()>>,
}

#[derive(Default)]
struct HostState {
    tracker: Tracker,
    snapshot: Option<Snapshot>,
    /// With the terminal's connection it was read on: a reconnect may land
    /// on another machine.
    system: Option<(u64, SystemInfo)>,
    /// With when they were read.
    disks: Option<(Instant, Vec<DiskUsage>)>,
    problem: Option<Problem>,
}

impl HostState {
    /// What the next reading on `connection` asks for besides the load.
    fn parts_due(&self, connection: u64, now: Instant) -> Parts {
        let system = self
            .system
            .as_ref()
            .is_none_or(|(read_on, _)| *read_on != connection);
        let disks = system
            || self
                .disks
                .as_ref()
                .is_none_or(|(at, _)| now.saturating_duration_since(*at) >= DISK_INTERVAL);
        Parts { system, disks }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Problem {
    /// The terminal is not connected, so there is no connection to read on.
    NotConnected,
    /// Not Linux; `uname -s` when the host said it.
    Unsupported(Option<String>),
    /// The command did not run, or did not finish.
    Failed(String),
}

/// What one try at a reading came back with.
enum Outcome {
    Output(ExecResult),
    NotConnected,
}

impl MonitorPanel {
    pub fn new(dispatch: FocusHandle) -> Self {
        Self {
            target: None,
            active: false,
            hosts: HashMap::new(),
            cores_unfolded: false,
            cores_width: Rc::new(Cell::new(None)),
            interfaces_unfolded: false,
            dispatch,
            sampling: None,
        }
    }

    /// Unfold a folded part, or fold it away again. Stays so for every
    /// terminal's host.
    pub fn toggle(&mut self, detail: MonitorDetail, cx: &mut Context<Self>) {
        let unfolded = match detail {
            MonitorDetail::Cores => &mut self.cores_unfolded,
            MonitorDetail::Interfaces => &mut self.interfaces_unfolded,
        };
        *unfolded = !*unfolded;
        cx.notify();
    }

    /// Read `target`'s host while `active`, the panel showing it.
    pub fn set_target(&mut self, target: Option<ExecTarget>, active: bool, cx: &mut Context<Self>) {
        if self.target == target && self.active == active {
            return;
        }
        self.target = target;
        self.active = active;
        self.restart(cx);
        cx.notify();
    }

    /// Start reading the target's host over, or stop when there is nothing
    /// to show. Dropping the task stops it; a reading still on its way is
    /// answered to no one.
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.sampling = None;
        let Some(target) = self.target.clone().filter(|_| self.active) else {
            return;
        };
        self.sampling = Some(cx.spawn(async move |this, cx| {
            let mut first = true;
            loop {
                let request = this.update(cx, |this, cx| {
                    let view = target.view.upgrade()?;
                    let view = view.read(cx);
                    let connection = view.generation(cx);
                    let now = cx.background_executor().now();
                    let parts = this.hosts.get(&target.terminal).map_or(
                        Parts {
                            system: true,
                            disks: true,
                        },
                        |host| host.parts_due(connection, now),
                    );
                    let reply = view.exec(linux::command(parts), cx)?;
                    Some((reply, connection))
                });
                let Ok(request) = request else { break };
                let (outcome, connection) = match request {
                    None => (Outcome::NotConnected, 0),
                    Some((reply, connection)) => (
                        exec_answer(reply, cx)
                            .await
                            .map_or(Outcome::NotConnected, Outcome::Output),
                        connection,
                    ),
                };
                if this
                    .update(cx, |this, cx| {
                        this.record(target.terminal, connection, outcome, cx)
                    })
                    .is_err()
                {
                    break;
                }
                let pause = if first { SECOND_READING } else { INTERVAL };
                first = false;
                cx.background_executor().timer(pause).await;
            }
        }));
    }

    fn record(
        &mut self,
        terminal: RemoteTerminalId,
        connection: u64,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        let now = cx.background_executor().now();
        let host = self.hosts.entry(terminal).or_default();
        host.problem = match outcome {
            Outcome::NotConnected => Some(Problem::NotConnected),
            Outcome::Output(Err(error)) => Some(Problem::Failed(error)),
            Outcome::Output(Ok(output)) => match linux::parse(&output) {
                Parsed::Reading(mut reading) => {
                    if let Some(system) = reading.system.take() {
                        host.system = Some((connection, system));
                    }
                    if let Some(disks) = reading.disks.take() {
                        host.disks = Some((now, disks));
                    }
                    host.snapshot = Some(host.tracker.update(*reading, now));
                    None
                }
                Parsed::Unsupported(system) => Some(Problem::Unsupported(system)),
            },
        };
        cx.notify();
    }
}

impl Render for MonitorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self
            .target
            .as_ref()
            .and_then(|target| self.hosts.get(&target.terminal));
        let snapshot = host.and_then(|host| host.snapshot.as_ref());
        let system = host.and_then(|host| host.system.as_ref().map(|(_, system)| system));
        let disks = host.and_then(|host| host.disks.as_ref().map(|(_, disks)| disks.as_slice()));
        let problem = host.and_then(|host| host.problem.clone());
        let interfaces_unfolded = self.interfaces_unfolded;
        let cores = CoresLayout {
            unfolded: self.cores_unfolded,
            per_row: self
                .cores_width
                .get()
                .map(|width| bars_per_row(f32::from(width), f32::from(window.rem_size()))),
            width: self.cores_width.clone(),
            panel: cx.entity_id(),
        };
        let dispatch = &self.dispatch;
        // The scrollbar goes on a layer beside the scrolled content, which
        // `overflow_y_scrollbar` lays out: on the content itself it would
        // scroll away with it, and stop matching where the content is.
        let content = v_flex()
            .size_full()
            .p_3()
            .gap_3()
            .map(|panel| match (snapshot, problem) {
                // Numbers from before a failed reading still say something;
                // the failure goes above them.
                (Some(snapshot), problem @ (None | Some(Problem::Failed(_)))) => panel
                    .when_some(problem, |panel, problem| {
                        panel.child(render_problem(&problem, cx))
                    })
                    .when_some(system, |panel, system| {
                        panel.child(render_system(system, snapshot.uptime, cx))
                    })
                    .child(render_cpu(system, snapshot, &cores, dispatch, cx))
                    .child(render_memory(&snapshot.memory, cx))
                    .child(render_network(snapshot, interfaces_unfolded, dispatch, cx))
                    .when_some(disks, |panel, disks| panel.child(render_disks(disks, cx))),
                (_, Some(problem)) => panel.child(render_problem(&problem, cx)),
                (None, None) => panel.child(
                    h_flex()
                        .id("monitor-loading")
                        .test_support()
                        .gap_2()
                        .justify_center()
                        .py_8()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(Spinner::new().small())
                        .child("正在读取…"),
                ),
            })
            .overflow_y_scrollbar();
        div()
            .id("monitor")
            .test_support()
            .size_full()
            .child(content)
    }
}

fn render_problem(problem: &Problem, cx: &App) -> impl IntoElement {
    let (text, color) = match problem {
        Problem::NotConnected => (
            "终端没有连接。连接后这里显示主机的 CPU、内存、网络和磁盘。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(Some(system)) => (
            format!("暂不支持 {system} 的监控，目前只支持 Linux 主机。"),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(None) => (
            "暂不支持这台主机的监控，目前只支持 Linux 主机。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Failed(error) => (format!("读取失败：{error}"), cx.theme().danger),
    };
    div()
        .id("monitor-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

/// A card: its icon and title, with `meta` at the end of the title row.
fn card(
    id: &'static str,
    icon: impl Into<Icon>,
    title: &'static str,
    meta: Option<AnyElement>,
    cx: &App,
) -> GroupBox {
    GroupBox::new().id(id).fill().child(
        h_flex()
            .gap_2()
            .child(icon.into().small().text_color(cx.theme().muted_foreground))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .children(meta),
    )
}

/// A card's quiet note at the end of its title row: 「1 核」.
fn note(text: String, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// In place of the note, when the card folds something away: 「8 核 ⌄」.
fn unfold_button(
    id: &'static str,
    label: String,
    detail: MonitorDetail,
    unfolded: bool,
    tooltip: &'static str,
    dispatch: &FocusHandle,
) -> AnyElement {
    let dispatch = dispatch.clone();
    Button::new(id)
        .ghost()
        .xsmall()
        .label(label)
        .child(Icon::new(if unfolded {
            IconName::ChevronUp
        } else {
            IconName::ChevronDown
        }))
        .toggled(unfolded)
        .tooltip(tooltip)
        .on_click(move |_, window, cx| {
            dispatch.dispatch_action(&ToggleMonitorDetail(detail), window, cx)
        })
        .into_any_element()
}

fn load_color(percent: f32, cx: &App) -> Hsla {
    match Load::of(percent) {
        Load::Normal => cx.theme().success,
        Load::High => cx.theme().warning,
        Load::Critical => cx.theme().danger,
    }
}

/// Numbers that change every reading: monospaced, so they hold still.
fn numeric(text: impl Into<SharedString>, cx: &App) -> Div {
    div()
        .font_family(cx.theme().mono_font_family.clone())
        .child(text.into())
}

fn render_system(system: &SystemInfo, uptime: Option<Duration>, cx: &App) -> impl IntoElement {
    let field = |id: &'static str, label: &'static str, value: String| {
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
                div()
                    .id(id)
                    .test_support()
                    .aria_label(value.clone())
                    .text_sm()
                    .truncate()
                    .child(value),
            )
    };
    card("monitor-system", CatalogIcon::Monitor, "系统", None, cx).child(
        div()
            .grid()
            .grid_cols(2)
            .gap_x_4()
            .gap_y_3()
            .child(field("monitor-host", "主机", system.host_name.clone()))
            .child(field("monitor-arch", "架构", system.arch.clone()))
            .child(field("monitor-os", "系统", system.os.clone()))
            .child(field(
                "monitor-uptime",
                "运行时长",
                uptime.map(format_duration).unwrap_or_default(),
            )),
    )
}

/// A ring with the share in its middle, as big as the eye needs to catch a
/// change across the room.
fn gauge(id: &'static str, percent: Option<f32>, cx: &App) -> impl IntoElement {
    let color = load_color(percent.unwrap_or(0.), cx);
    ProgressCircle::new(id)
        .value(percent.unwrap_or(0.))
        .color(color)
        .size_16()
        .child(
            v_flex()
                .items_center()
                .text_color(color)
                .child(
                    numeric(
                        percent.map_or("—".into(), |p| format!("{}", p.round() as u32)),
                        cx,
                    )
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD),
                )
                .child(div().text_xs().child("%")),
        )
}

/// A labelled bar: the label and the share above it.
fn meter(
    id: &'static str,
    label: &'static str,
    percent: Option<f32>,
    cx: &App,
) -> impl IntoElement {
    let color = load_color(percent.unwrap_or(0.), cx);
    let text = percent.map_or("—".into(), format_percent);
    v_flex()
        .gap_1p5()
        .child(
            h_flex().justify_between().text_sm().child(label).child(
                numeric(text.clone(), cx)
                    .id(id)
                    .test_support()
                    .aria_label(text)
                    .text_color(color)
                    .font_weight(FontWeight::SEMIBOLD),
            ),
        )
        .child(
            Progress::new(SharedString::from(format!("{id}-bar")))
                .small()
                .value(percent.unwrap_or(0.))
                .color(color),
        )
}

/// How the per-core bars lay out this frame.
struct CoresLayout {
    /// Every row, not only the first.
    unfolded: bool,
    /// How many bars make a row, from last frame's width; `None` until the
    /// row has been laid out once.
    per_row: Option<usize>,
    /// Where the row records its width for the next frame.
    width: Rc<Cell<Option<Pixels>>>,
    /// The panel to redraw when that width changes.
    panel: EntityId,
}

fn render_cpu(
    system: Option<&SystemInfo>,
    snapshot: &Snapshot,
    layout: &CoresLayout,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let model = system
        .map(|system| system.cpu_model.clone())
        .filter(|model| !model.is_empty());
    let total = snapshot.cpu.as_ref().map(|load| load.total);
    let cores = snapshot
        .cpu
        .as_ref()
        .map(|load| load.cores.as_slice())
        .filter(|cores| cores.len() > 1);
    // A host whose cores fill more than the one row shown can unfold the
    // rest; most hosts fit, and have no button.
    let more = layout
        .per_row
        .filter(|per_row| cores.is_some_and(|cores| cores.len() > *per_row));
    let count = format!("{} 核", snapshot.cores);
    let meta = if more.is_some() {
        unfold_button(
            "monitor-cores-toggle",
            count,
            MonitorDetail::Cores,
            layout.unfolded,
            if layout.unfolded {
                "收起更多核的占用"
            } else {
                "显示更多核的占用"
            },
            dispatch,
        )
    } else {
        note(count, cx)
    };
    let shown = cores.map(|cores| match more {
        Some(per_row) if !layout.unfolded => &cores[..per_row],
        _ => cores,
    });
    card("monitor-cpu", IconName::Cpu, "CPU", Some(meta), cx)
        .when_some(model, |card, model| {
            card.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(model),
            )
        })
        .child(
            h_flex()
                .gap_4()
                .child(gauge("monitor-cpu-gauge", total, cx))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_3()
                        .child(meter("monitor-cpu-usage", "平均使用率", total, cx))
                        .when_some(shown, |column, shown| {
                            column.child(render_cores(shown, layout, cx))
                        }),
                ),
        )
}

/// The cores' loads as a column each, wrapping onto more rows the more
/// there are: 64 take three rows at the sidebar's usual width. Folded, only
/// the first row; it measures its width for the next frame to know how many
/// make one. Pointing at a bar tells its core's number and share.
fn render_cores(cores: &[f32], layout: &CoresLayout, cx: &App) -> impl IntoElement {
    let label = cores
        .iter()
        .enumerate()
        .map(|(index, load)| format!("核 {index}：{}", format_percent(*load)))
        .collect::<Vec<_>>()
        .join("，");
    let (width, panel) = (layout.width.clone(), layout.panel);
    h_flex()
        .id("monitor-cpu-cores")
        .test_support()
        .aria_label(label)
        .relative()
        .w_full()
        .flex_wrap()
        .gap_0p5()
        // Before the first measurement, a row's height keeps the rest out.
        .when(!layout.unfolded, |row| {
            row.max_h(rems(1.5)).overflow_hidden()
        })
        .children(cores.iter().enumerate().map(|(index, load)| {
            let color = load_color(*load, cx);
            let tooltip: SharedString = format!("核 {index}：{}", format_percent(*load)).into();
            div()
                .id(("monitor-core", index as u64))
                .test_support()
                .relative()
                .w(rems(0.375))
                .h(rems(1.5))
                .rounded_sm()
                .bg(color.opacity(0.2))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .w_full()
                        .h(relative(load.clamp(0., 100.) / 100.))
                        .rounded_sm()
                        .bg(color),
                )
        }))
        .child(
            canvas(
                move |bounds, window, _| {
                    if width.get() != Some(bounds.size.width) {
                        width.set(Some(bounds.size.width));
                        window.on_next_frame(move |_, cx| cx.notify(panel));
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

/// 「421.02 MB / 973.25 MB」, the total quieter.
fn amount(id: &'static str, used: u64, total: u64, cx: &App) -> impl IntoElement {
    let (used, total) = (format_bytes(used), format_bytes(total));
    h_flex()
        .id(id)
        .test_support()
        .aria_label(format!("{used} / {total}"))
        .gap_1()
        .text_xs()
        .child(numeric(used, cx))
        .child(numeric(format!("/ {total}"), cx).text_color(cx.theme().muted_foreground))
}

fn render_memory(memory: &Memory, cx: &App) -> impl IntoElement {
    card("monitor-memory", IconName::MemoryStick, "内存", None, cx).child(
        h_flex()
            .gap_4()
            .child(gauge("monitor-memory-gauge", Some(memory.percent()), cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(meter(
                                "monitor-memory-usage",
                                "物理内存",
                                Some(memory.percent()),
                                cx,
                            ))
                            .child(amount(
                                "monitor-memory-amount",
                                memory.used,
                                memory.total,
                                cx,
                            )),
                    )
                    .when(memory.swap_total > 0, |column| {
                        column.child(
                            v_flex()
                                .gap_1p5()
                                .child(meter(
                                    "monitor-swap-usage",
                                    "交换空间",
                                    Some(memory.swap_percent()),
                                    cx,
                                ))
                                .child(amount(
                                    "monitor-swap-amount",
                                    memory.swap_used,
                                    memory.swap_total,
                                    cx,
                                )),
                        )
                    }),
            ),
    )
}

fn render_network(
    snapshot: &Snapshot,
    unfolded: bool,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let busy = snapshot.busy_interfaces();
    let meta = (busy.len() > 1).then(|| {
        unfold_button(
            "monitor-interfaces-toggle",
            format!("{} 个网卡", busy.len()),
            MonitorDetail::Interfaces,
            unfolded,
            if unfolded {
                "只显示主网卡"
            } else {
                "显示全部网卡"
            },
            dispatch,
        )
    });
    let shown: Vec<&InterfaceRate> = if unfolded {
        busy
    } else {
        snapshot.main_interface().into_iter().collect()
    };
    let rate = |rate: Option<f64>| rate.map_or("—".into(), format_rate);
    card("monitor-network", IconName::Network, "网络", meta, cx)
        .when(shown.is_empty(), |card| {
            card.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("没有在用的网卡"),
            )
        })
        .children(shown.into_iter().map(|interface| {
            let (up, down) = (rate(interface.transmitted), rate(interface.received));
            h_flex()
                .id(SharedString::from(format!(
                    "monitor-interface:{}",
                    interface.name
                )))
                .test_support()
                .aria_label(format!("上传 {up}，下载 {down}"))
                .gap_3()
                .text_sm()
                .child(
                    numeric(interface.name.clone(), cx)
                        .flex_1()
                        .min_w_0()
                        .truncate(),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_3()
                        .text_xs()
                        .child(direction(IconName::ArrowUp, up, cx))
                        .child(direction(IconName::ArrowDown, down, cx)),
                )
        }))
}

fn direction(icon: IconName, rate: String, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_0p5()
        .child(
            Icon::new(icon)
                .xsmall()
                .text_color(cx.theme().muted_foreground),
        )
        .child(numeric(rate, cx))
}

fn render_disks(disks: &[DiskUsage], cx: &App) -> impl IntoElement {
    card("monitor-disks", IconName::HardDrive, "磁盘", None, cx)
        .when(disks.is_empty(), |card| {
            card.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("没有读到磁盘"),
            )
        })
        .children(disks.iter().map(|disk| {
            let percent = disk.percent();
            let color = load_color(percent, cx);
            let (used, total) = (format_bytes(disk.used), format_bytes(disk.total));
            v_flex()
                .id(SharedString::from(format!("monitor-disk:{}", disk.mount)))
                .test_support()
                .aria_label(format!("{}，{used} / {total}", format_percent(percent)))
                .gap_1p5()
                .child(
                    h_flex()
                        .gap_3()
                        .text_sm()
                        .child(
                            numeric(disk.mount.clone(), cx)
                                .flex_1()
                                .min_w_0()
                                .truncate(),
                        )
                        .child(
                            numeric(format_percent(percent), cx)
                                .flex_shrink_0()
                                .text_color(color)
                                .font_weight(FontWeight::SEMIBOLD),
                        ),
                )
                .child(
                    Progress::new(SharedString::from(format!(
                        "monitor-disk-bar:{}",
                        disk.mount
                    )))
                    .small()
                    .value(percent)
                    .color(color),
                )
                .child(
                    numeric(format!("{used} / {total}"), cx)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground),
                )
        }))
}
