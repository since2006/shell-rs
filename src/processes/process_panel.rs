use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu},
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
    Order, Process, ProcessDetails, ProcessSort, ProcessState, Snapshot, Tracker, format_started,
};
use crate::app::{CatalogIcon, EndProcess, RefreshProcesses, ShowProcess, SortProcesses};
use crate::i18n::{UiLocale, t, tn};
use crate::shared::{format_bytes, format_percent};
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// Time between readings. The second comes sooner: a CPU share needs two,
/// and the list should not sit on dashes.
const INTERVAL: Duration = Duration::from_secs(15);
const SECOND_READING: Duration = Duration::from_secs(2);
/// How long a terminal not connected is left before it is asked again.
const RETRY: Duration = Duration::from_secs(2);

/// 进程管理: the processes of the host of the SSH terminal in front, with
/// their memory and CPU, read every 15 seconds while the panel shows; a
/// right click on one ends it.
///
/// Each reading runs `linux::command` beside the shell, on the terminal's
/// own connection: no second login, nothing asked again. Nothing runs on
/// the host while the panel is hidden. What each terminal's host showed
/// last is kept, so coming back to a terminal shows its list at once. The
/// search and the order stay as they are from one terminal to the next.
pub struct ProcessPanel {
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// The readings; dropping it stops them.
    sampling: Option<Task<()>>,
    /// Whether a reading is on its way.
    loading: bool,
    search: Entity<InputState>,
    query: String,
    order: Order,
    scroll_handle: UniformListScrollHandle,
    /// The process a right click landed on, for the list's menu; `None`
    /// off every process.
    menu_hit: Rc<Cell<Option<u32>>>,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct HostState {
    tracker: Tracker,
    /// The terminal's connection the tracker's last reading came over: a
    /// reconnect may land on another machine.
    connection: Option<u64>,
    snapshot: Option<Rc<Snapshot>>,
    problem: Option<Problem>,
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

impl ProcessPanel {
    pub fn new(dispatch: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("processes.panel.search"))
                .clean_on_escape()
        });
        let subscriptions = vec![
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.search.update(cx, |search, cx| {
                    search.set_placeholder(t!("processes.panel.search"), window, cx)
                });
            }),
            cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = input.read(cx).value().to_string();
                    this.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
            }),
        ];
        Self {
            target: None,
            active: false,
            hosts: HashMap::new(),
            sampling: None,
            loading: false,
            search,
            query: String::new(),
            order: Order::default(),
            scroll_handle: UniformListScrollHandle::new(),
            menu_hit: Rc::new(Cell::new(None)),
            dispatch,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
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

    /// Read the host now, and every 15 seconds from now: 刷新.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.restart(None, cx);
        cx.notify();
    }

    /// Read the host in a moment: a process just told to end takes that
    /// long to go.
    pub fn refresh_soon(&mut self, cx: &mut Context<Self>) {
        self.restart(Some(Duration::from_secs(1)), cx);
    }

    /// Sort by `by`, the most first; by the same again, the other way.
    pub fn sort(&mut self, by: ProcessSort, cx: &mut Context<Self>) {
        self.order = self.order.toggle(by);
        self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// A process of the host on screen, as the list last showed it.
    pub fn process(&self, pid: u32) -> Option<Process> {
        self.snapshot()?.process(pid).cloned()
    }

    /// A process of the host on screen with its family, for its details.
    pub fn details(&self, pid: u32) -> Option<ProcessDetails> {
        self.snapshot()?.details(pid)
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        let target = self.target.as_ref()?;
        self.hosts.get(&target.terminal)?.snapshot.as_deref()
    }

    /// Start reading the target's host over, after `delay` if any, or stop
    /// when there is nothing to show. Dropping the task stops it; a reading
    /// still on its way is answered to no one.
    fn restart(&mut self, delay: Option<Duration>, cx: &mut Context<Self>) {
        self.sampling = None;
        self.loading = false;
        let Some(target) = self.target.clone().filter(|_| self.active) else {
            return;
        };
        self.sampling = Some(cx.spawn(async move |this, cx| {
            if let Some(delay) = delay {
                cx.background_executor().timer(delay).await;
            }
            loop {
                let request = this.update(cx, |this, cx| {
                    let view = target.view.upgrade()?;
                    let view = view.read(cx);
                    let reply = view.exec(linux::command(), cx)?;
                    let connection = view.generation(cx);
                    this.loading = true;
                    cx.notify();
                    Some((reply, connection))
                });
                let Ok(request) = request else { break };
                let (answer, connection) = match request {
                    None => (None, 0),
                    Some((reply, connection)) => (exec_answer(reply, cx).await, connection),
                };
                let Ok(pause) = this.update(cx, |this, cx| {
                    this.record(target.terminal, connection, answer, cx)
                }) else {
                    break;
                };
                cx.background_executor().timer(pause).await;
            }
        }));
    }

    /// Keep what a reading came back with; how long until the next.
    fn record(
        &mut self,
        terminal: RemoteTerminalId,
        connection: u64,
        answer: Option<ExecResult>,
        cx: &mut Context<Self>,
    ) -> Duration {
        self.loading = false;
        cx.notify();
        let host = self.hosts.entry(terminal).or_default();
        let output = match answer {
            None => {
                host.problem = Some(Problem::NotConnected);
                return RETRY;
            }
            Some(Err(error)) => {
                host.problem = Some(Problem::Failed(error));
                return INTERVAL;
            }
            Some(Ok(output)) => output,
        };
        match linux::parse(&output) {
            Parsed::Reading(reading) => {
                if host.connection != Some(connection) {
                    host.tracker = Tracker::default();
                    host.connection = Some(connection);
                }
                let now = chrono::Utc::now().timestamp();
                let snapshot = host.tracker.update(*reading, now);
                let shares = snapshot
                    .processes()
                    .iter()
                    .any(|process| process.cpu.is_some());
                host.snapshot = Some(Rc::new(snapshot));
                host.problem = None;
                if shares { INTERVAL } else { SECOND_READING }
            }
            Parsed::Unsupported(system) => {
                host.problem = Some(Problem::Unsupported(system));
                INTERVAL
            }
        }
    }

    fn render_list(
        &self,
        snapshot: Rc<Snapshot>,
        problem: Option<Problem>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let rows = Rc::new(snapshot.listed(&self.query, self.order));
        let summary = tn!(
            "processes.panel.summary",
            snapshot.processes().len(),
            seconds = INTERVAL.as_secs()
        );
        let refresh = self.dispatch.clone();
        let header = v_flex()
            .flex_shrink_0()
            .px_3()
            .pt_3()
            .pb_2()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("processes-summary")
                            .test_support()
                            .aria_label(summary.clone())
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(summary),
                    )
                    .child(
                        Button::new("processes-refresh")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CatalogIcon::RefreshCw))
                            .loading(self.loading)
                            .tooltip(t!("tools.refresh"))
                            .accessibility_label(t!("tools.refresh"))
                            .on_click(move |_, window, cx| {
                                refresh.dispatch_action(&RefreshProcesses, window, cx)
                            }),
                    ),
            )
            .when_some(problem, |header, problem| {
                header.child(render_problem(&problem, cx))
            })
            .when(snapshot.truncated(), |header| {
                let text = t!("processes.panel.truncated", limit = linux::LIMIT);
                header.child(
                    div()
                        .id("processes-truncated")
                        .test_support()
                        .aria_label(text.clone())
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(text),
                )
            })
            .child(
                Input::new(&self.search)
                    .id("processes-search")
                    .small()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(sort_button(
                        "processes-sort-memory",
                        IconName::MemoryStick,
                        ProcessSort::Memory,
                        self.order,
                        &self.dispatch,
                    ))
                    .child(sort_button(
                        "processes-sort-cpu",
                        IconName::Cpu,
                        ProcessSort::Cpu,
                        self.order,
                        &self.dispatch,
                    ))
                    .child(div().flex_1())
                    .child({
                        let count = rows.len().to_string();
                        div()
                            .id("processes-count")
                            .test_support()
                            .aria_label(count.clone())
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_color(cx.theme().muted_foreground)
                            .child(count)
                    }),
            );

        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();
        // The scrollbar goes on the list's box, which does not scroll: on
        // the scrolled rows it would scroll away with them. The menu hangs
        // off the box too, not off the rows, which are drawn after layout
        // (see the host tree).
        let list = div()
            .id("processes-list")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .map(|list| {
                if rows.is_empty() {
                    let empty = if self.query.trim().is_empty() {
                        t!("processes.panel.empty")
                    } else {
                        t!("processes.panel.no_match")
                    };
                    list.child(
                        div()
                            .id("processes-empty")
                            .test_support()
                            .aria_label(empty.clone())
                            .py_8()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty),
                    )
                } else {
                    let count = rows.len();
                    let (hit, focus, dispatch) = (
                        self.menu_hit.clone(),
                        self.focus_handle.clone(),
                        self.dispatch.clone(),
                    );
                    list.child(
                        uniform_list("processes-rows", count, move |range, _, cx| {
                            range
                                .map(|index| {
                                    let process = &snapshot.processes()[rows[index]];
                                    div().px_3().pt_2().child(render_process(
                                        process,
                                        hit.clone(),
                                        focus.clone(),
                                        dispatch.clone(),
                                        cx,
                                    ))
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .pb_3()
                        .track_scroll(&self.scroll_handle),
                    )
                }
            })
            .vertical_scrollbar(&self.scroll_handle)
            .capture_any_mouse_down(move |event, _, _| {
                if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                    clear_hit.set(None);
                }
            })
            .context_menu(move |menu, _, _| build_context_menu(menu_hit.get(), menu));
        v_flex().size_full().child(header).child(list)
    }
}

impl Focusable for ProcessPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProcessPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self
            .target
            .as_ref()
            .and_then(|target| self.hosts.get(&target.terminal));
        let snapshot = host.and_then(|host| host.snapshot.clone());
        let problem = host.and_then(|host| host.problem.clone());
        v_flex()
            .id("processes")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .map(|panel| match (snapshot, problem) {
                // A list from before a failed reading still says something;
                // the failure goes above it.
                (Some(snapshot), problem @ (None | Some(Problem::Failed(_)))) => {
                    panel.child(self.render_list(snapshot, problem, cx))
                }
                (_, Some(problem)) => panel.child(div().p_3().child(render_problem(&problem, cx))),
                (None, None) => panel.child(
                    h_flex()
                        .id("processes-loading")
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
    }
}

fn render_problem(problem: &Problem, cx: &App) -> impl IntoElement {
    let (text, color) = match problem {
        Problem::NotConnected => (
            t!("processes.panel.not_connected"),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(Some(system)) => (
            t!("processes.panel.unsupported_system", system = system),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(None) => (
            t!("processes.panel.unsupported"),
            cx.theme().muted_foreground,
        ),
        Problem::Failed(error) => (t!("tools.read_failed", error = error), cx.theme().danger),
    };
    div()
        .id("processes-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

/// 「内存 ↓」: sorts by `by`, the most first, and the other way round when
/// it already does; the arrow says which way it does.
fn sort_button(
    id: &'static str,
    icon: IconName,
    by: ProcessSort,
    order: Order,
    dispatch: &FocusHandle,
) -> impl IntoElement {
    let active = order.by == by;
    let label: SharedString = match by {
        ProcessSort::Memory => t!("processes.memory"),
        ProcessSort::Cpu => "CPU".into(),
    };
    let tooltip = match (by, active && order.descending) {
        (ProcessSort::Memory, true) => t!("processes.sort.memory_ascending"),
        (ProcessSort::Memory, false) => t!("processes.sort.memory_descending"),
        (ProcessSort::Cpu, true) => t!("processes.sort.cpu_ascending"),
        (ProcessSort::Cpu, false) => t!("processes.sort.cpu_descending"),
    };
    let dispatch = dispatch.clone();
    Button::new(id)
        .ghost()
        .small()
        .icon(Icon::new(icon))
        .label(label)
        .when(active, |button| {
            button.child(Icon::new(if order.descending {
                IconName::ArrowDown
            } else {
                IconName::ArrowUp
            }))
        })
        .selected(active)
        .toggled(active)
        .tooltip(tooltip)
        .on_click(move |_, window, cx| dispatch.dispatch_action(&SortProcesses(by), window, cx))
}

fn build_context_menu(hit: Option<u32>, menu: PopupMenu) -> PopupMenu {
    let Some(pid) = hit else {
        return menu;
    };
    menu.menu_with_icon(
        t!("tools.menu.details"),
        Icon::new(IconName::Info),
        Box::new(ShowProcess(pid)),
    )
    .separator()
    .menu_with_icon(
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

/// What a state looks like: green running, yellow stuck or paused, red a
/// zombie, the rest (most processes, asleep) quiet.
fn state_color(state: ProcessState, cx: &App) -> Hsla {
    match state {
        ProcessState::Running => cx.theme().success,
        ProcessState::Uninterruptible | ProcessState::Stopped => cx.theme().warning,
        ProcessState::Zombie => cx.theme().danger,
        ProcessState::Sleeping | ProcessState::Idle | ProcessState::Other => {
            cx.theme().muted_foreground
        }
    }
}

/// A process's card. Every card has the same lines, so they are all as
/// tall as the list takes them to be.
fn render_process(
    process: &Process,
    menu_hit: Rc<Cell<Option<u32>>>,
    focus: FocusHandle,
    dispatch: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let pid = process.pid;
    let user = process.user.clone().unwrap_or_else(|| "—".into());
    let started = format_started(process.started);
    let memory = format!(
        "{} · {}",
        format_bytes(process.memory),
        format_percent(process.memory_percent)
    );
    let cpu = process.cpu.map_or("—".into(), format_percent);
    let label = t!(
        "processes.card.label",
        name = process.name,
        pid = pid,
        state = process.state.label(),
        user = user,
        memory = memory,
        cpu = cpu
    );
    let state = process.state.label();
    let numeric = |text: String| {
        div()
            .font_family(cx.theme().mono_font_family.clone())
            .child(text)
    };
    v_flex()
        .id(("process", pid as u64))
        .test_support()
        .aria_label(label)
        .gap_1()
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .text_color(cx.theme().group_box_foreground)
        // A click opens its details; the border says it can.
        .border_1()
        .border_color(cx.theme().tokens.group_box)
        .hover({
            let border = cx.theme().border;
            move |style| style.border_color(border)
        })
        .cursor_pointer()
        .on_click(move |_, window, cx| dispatch.dispatch_action(&ShowProcess(pid), window, cx))
        // A right click puts the menu on this process. The menu's commands
        // are dispatched from what is focused, which may be nothing.
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            menu_hit.set(Some(pid));
            focus.focus(window, cx);
        })
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .id(("process-state", pid as u64))
                        .flex_shrink_0()
                        .size_2()
                        .rounded_full()
                        .bg(state_color(process.state, cx))
                        .tooltip(move |window, cx| Tooltip::new(state.clone()).build(window, cx)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(process.name.clone()),
                )
                .child(
                    numeric(format!("PID {pid}"))
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground),
                ),
        )
        .child(
            div()
                .truncate()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{user} · {started}")),
        )
        .child(
            h_flex()
                .gap_2()
                .text_xs()
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("processes.memory")),
                        )
                        .child(numeric(memory).text_color(cx.theme().info)),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .child(div().text_color(cx.theme().muted_foreground).child("CPU"))
                        .child(numeric(cpu).text_color(cx.theme().warning)),
                ),
        )
        .child(
            Progress::new(SharedString::from(format!("process-memory-{pid}")))
                .small()
                .value(process.memory_percent)
                .color(cx.theme().info),
        )
}
