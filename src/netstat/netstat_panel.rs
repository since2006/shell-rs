use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement as _,
    searchable_list::SearchableListItem,
    select::{Select, SelectEvent, SelectState},
    spinner::Spinner,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::linux::{self, Parsed};
use super::model::{
    Filter, Processes, ProtocolFilter, Role, Socket, SocketState, StateFilter, Table,
};
use crate::app::{CatalogIcon, RefreshConnections};
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// How long a terminal not connected is left before it is asked again.
const RETRY: Duration = Duration::from_secs(2);

/// 网络连接: every TCP and UDP socket of the host of the SSH terminal in
/// front, with the processes holding them, read when the panel comes on
/// screen and again on 刷新.
///
/// A list, not a gauge, so it holds still while it is read: what changes
/// underneath shows on the next refresh, not under the pointer. Each
/// reading runs `linux::command` beside the shell, on the terminal's own
/// connection: no second login, nothing asked again. What each terminal's
/// host showed last is kept, so coming back to a terminal shows its list at
/// once while it is read again. The search and the filters stay as they
/// are from one terminal to the next.
pub struct NetstatPanel {
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// The reading on its way; dropping it stops it.
    reading: Option<Task<()>>,
    /// Whether a reading is on its way.
    loading: bool,
    search: Entity<InputState>,
    protocol: Entity<SelectState<Vec<ProtocolFilter>>>,
    state: Entity<SelectState<Vec<StateFilter>>>,
    filter: Filter,
    scroll_handle: UniformListScrollHandle,
    /// The workspace's focus handle: 刷新 dispatches on it, so it works
    /// whatever is focused.
    dispatch: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct HostState {
    table: Option<Rc<Table>>,
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

impl SearchableListItem for ProtocolFilter {
    type Value = Self;

    fn title(&self) -> SharedString {
        self.label().into()
    }

    fn value(&self) -> &Self::Value {
        self
    }
}

impl SearchableListItem for StateFilter {
    type Value = Self;

    fn title(&self) -> SharedString {
        self.label().into()
    }

    fn value(&self) -> &Self::Value {
        self
    }
}

impl NetstatPanel {
    pub fn new(dispatch: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索地址、端口、状态、PID 或进程")
                .clean_on_escape()
        });
        let protocol = cx.new(|cx| {
            SelectState::new(
                ProtocolFilter::ALL.to_vec(),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let state = cx.new(|cx| {
            SelectState::new(
                StateFilter::ALL.to_vec(),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    this.set_filter(
                        Filter {
                            query,
                            ..this.filter.clone()
                        },
                        cx,
                    );
                }
            }),
            cx.subscribe(
                &protocol,
                |this, _, event: &SelectEvent<Vec<ProtocolFilter>>, cx| {
                    if let SelectEvent::Confirm(Some(protocol)) = event {
                        this.set_filter(
                            Filter {
                                protocol: *protocol,
                                ..this.filter.clone()
                            },
                            cx,
                        );
                    }
                },
            ),
            cx.subscribe(
                &state,
                |this, _, event: &SelectEvent<Vec<StateFilter>>, cx| {
                    if let SelectEvent::Confirm(Some(state)) = event {
                        this.set_filter(
                            Filter {
                                state: *state,
                                ..this.filter.clone()
                            },
                            cx,
                        );
                    }
                },
            ),
        ];
        Self {
            target: None,
            active: false,
            hosts: HashMap::new(),
            reading: None,
            loading: false,
            search,
            protocol,
            state,
            filter: Filter::default(),
            scroll_handle: UniformListScrollHandle::new(),
            dispatch,
            _subscriptions: subscriptions,
        }
    }

    /// Read `target`'s host when `active`, the panel showing it, and was
    /// not before.
    pub fn set_target(&mut self, target: Option<ExecTarget>, active: bool, cx: &mut Context<Self>) {
        if self.target == target && self.active == active {
            return;
        }
        self.target = target;
        self.active = active;
        self.read(cx);
        cx.notify();
    }

    /// Read the host again: 刷新.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.read(cx);
        cx.notify();
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.scroll_handle.scroll_to_item(0, ScrollStrategy::Top);
            cx.notify();
        }
    }

    /// Start reading the target's host over, or stop when there is nothing
    /// to show. A terminal not connected is asked again every little while,
    /// so its list comes once it connects.
    fn read(&mut self, cx: &mut Context<Self>) {
        self.reading = None;
        self.loading = false;
        let Some(target) = self.target.clone().filter(|_| self.active) else {
            return;
        };
        self.loading = true;
        self.reading = Some(cx.spawn(async move |this, cx| {
            loop {
                let reply = this.update(cx, |_, cx| {
                    let view = target.view.upgrade()?;
                    view.read(cx).exec(linux::command(), cx)
                });
                let Ok(reply) = reply else { return };
                let answer = match reply {
                    Some(reply) => exec_answer(reply, cx).await,
                    None => None,
                };
                let connected = answer.is_some();
                if this
                    .update(cx, |this, cx| this.record(target.terminal, answer, cx))
                    .is_err()
                    || connected
                {
                    return;
                }
                cx.background_executor().timer(RETRY).await;
            }
        }));
    }

    fn record(
        &mut self,
        terminal: RemoteTerminalId,
        answer: Option<ExecResult>,
        cx: &mut Context<Self>,
    ) {
        let host = self.hosts.entry(terminal).or_default();
        host.problem = match answer {
            None => Some(Problem::NotConnected),
            Some(Err(error)) => Some(Problem::Failed(error)),
            Some(Ok(output)) => match linux::parse(&output) {
                Parsed::Table(table) => {
                    host.table = Some(Rc::new(table));
                    None
                }
                Parsed::Unsupported(system) => Some(Problem::Unsupported(system)),
            },
        };
        // Not connected, it keeps asking; the spinner stops all the same.
        self.loading = false;
        cx.notify();
    }

    fn render_table(
        &self,
        table: Rc<Table>,
        problem: Option<Problem>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let rows = Rc::new(table.filtered(&self.filter));
        let summary = table.counts().summary();
        let dispatch = self.dispatch.clone();
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
                            .id("netstat-summary")
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
                        Button::new("netstat-refresh")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CatalogIcon::RefreshCw))
                            .loading(self.loading)
                            .tooltip("刷新")
                            .accessibility_label("刷新")
                            .on_click(move |_, window, cx| {
                                dispatch.dispatch_action(&RefreshConnections, window, cx)
                            }),
                    ),
            )
            .when_some(problem, |header, problem| {
                header.child(render_problem(&problem, cx))
            })
            .when(table.truncated(), |header| {
                header.child(note(
                    "netstat-truncated",
                    format!("连接太多，只列出了前 {} 条。", linux::LIMIT),
                    cx.theme().warning,
                ))
            })
            .map(|header| {
                let text = match table.processes() {
                    Processes::All => return header,
                    Processes::Own => "当前用户不是 root，只能看到自己的进程。",
                    Processes::None => "这台主机没有 ss 命令，看不到占用连接的进程。",
                };
                header.child(note(
                    "netstat-processes",
                    text.into(),
                    cx.theme().muted_foreground,
                ))
            })
            .child(
                Input::new(&self.search)
                    .id("netstat-search")
                    .small()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .child(
                h_flex()
                    .gap_2()
                    // Each in a box of its own, or it covers the whole row.
                    .child(
                        div().flex_1().min_w_0().child(
                            Select::new(&self.protocol)
                                .id("netstat-protocol")
                                .small()
                                .accessibility_label("协议"),
                        ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Select::new(&self.state)
                                .id("netstat-state")
                                .small()
                                .accessibility_label("状态"),
                        ),
                    ),
            )
            .when(self.filter.is_active(), |header| {
                let text = format!("筛选出 {} 条", rows.len());
                header.child(
                    div()
                        .id("netstat-matches")
                        .test_support()
                        .aria_label(text.clone())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                )
            });

        let empty = if self.filter.is_active() {
            "没有符合条件的连接"
        } else {
            "没有网络连接"
        };
        // The scrollbar goes on the list's box, which does not scroll: on
        // the scrolled rows it would scroll away with them.
        let list = div()
            .id("netstat-list")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .map(|list| {
                if rows.is_empty() {
                    list.child(
                        div()
                            .id("netstat-empty")
                            .test_support()
                            .aria_label(empty)
                            .py_8()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty),
                    )
                } else {
                    // Thousands of sockets on a busy server: only those in
                    // sight are drawn.
                    let count = rows.len();
                    list.child(
                        uniform_list("netstat-sockets", count, move |range, _, cx| {
                            range
                                .map(|index| {
                                    div()
                                        .px_3()
                                        .pt_2()
                                        .child(render_socket(&table.sockets()[rows[index]], cx))
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .pb_3()
                        .track_scroll(&self.scroll_handle),
                    )
                }
            })
            .vertical_scrollbar(&self.scroll_handle);
        v_flex().size_full().child(header).child(list)
    }
}

impl Render for NetstatPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self
            .target
            .as_ref()
            .and_then(|target| self.hosts.get(&target.terminal));
        let table = host.and_then(|host| host.table.clone());
        let problem = host.and_then(|host| host.problem.clone());
        v_flex()
            .id("netstat")
            .test_support()
            .size_full()
            .map(|panel| match (table, problem) {
                // A list from before a failed reading still says something;
                // the failure goes above it.
                (Some(table), problem @ (None | Some(Problem::Failed(_)))) => {
                    panel.child(self.render_table(table, problem, cx))
                }
                (_, Some(problem)) => panel.child(div().p_3().child(render_problem(&problem, cx))),
                (None, None) => panel.child(
                    h_flex()
                        .id("netstat-loading")
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
    }
}

fn render_problem(problem: &Problem, cx: &App) -> impl IntoElement {
    let (text, color) = match problem {
        Problem::NotConnected => (
            "终端没有连接。连接后这里显示主机的网络连接。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(Some(system)) => (
            format!("暂不支持查看 {system} 的网络连接，目前只支持 Linux 主机。"),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(None) => (
            "暂不支持查看这台主机的网络连接，目前只支持 Linux 主机。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Failed(error) => (format!("读取失败：{error}"), cx.theme().danger),
    };
    div()
        .id("netstat-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

/// A line under the summary: 「连接太多，只列出了前 3000 条。」.
fn note(id: &'static str, text: String, color: Hsla) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .aria_label(text.clone())
        .text_xs()
        .text_color(color)
        .child(text)
}

/// What a state looks like: green for what listens, blue for what is
/// connected, yellow for CLOSE_WAIT, which piles up when a program does not
/// close what its peer has; the rest quiet.
fn state_color(state: SocketState, cx: &App) -> Hsla {
    match state {
        SocketState::Listen => cx.theme().success,
        SocketState::Established => cx.theme().info,
        SocketState::CloseWait => cx.theme().warning,
        _ => cx.theme().muted_foreground,
    }
}

fn role_icon(role: Role) -> CatalogIcon {
    match role {
        Role::Listening => CatalogIcon::RadioTower,
        Role::Inbound => CatalogIcon::ArrowDownLeft,
        Role::Outbound => CatalogIcon::ArrowUpRight,
    }
}

/// A socket's card. Every card has the same lines, so they are all as tall
/// as the list takes them to be.
fn render_socket(socket: &Socket, cx: &App) -> impl IntoElement {
    let color = state_color(socket.state, cx);
    let process = socket.process_label();
    let label = format!(
        "{} {} · 本地 {} · 远端 {} · {} · {}",
        socket.protocol.label(),
        socket.state.label(),
        socket.local,
        socket.peer,
        process.as_deref().unwrap_or("—"),
        socket.user.as_deref().unwrap_or("—"),
    );
    let address = |name: &'static str, value: &str| {
        h_flex()
            .gap_2()
            .text_xs()
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(value.to_owned()),
            )
    };
    v_flex()
        .id(SharedString::from(format!(
            "netstat-socket:{} {} {}",
            socket.protocol.label().to_lowercase(),
            socket.local,
            socket.peer
        )))
        .test_support()
        .aria_label(label)
        .gap_1()
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .text_color(cx.theme().group_box_foreground)
        .child(
            h_flex()
                .gap_1p5()
                .child(Icon::new(role_icon(socket.role)).small().text_color(color))
                .child(Tag::secondary().small().child(socket.protocol.label()))
                .child(
                    Tag::custom(color.opacity(0.12), color, color.opacity(0.3))
                        .small()
                        .child(socket.state.label()),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(socket.role.label()),
                ),
        )
        .child(address("本地", &socket.local))
        .child(address("远端", &socket.peer))
        .child(
            h_flex()
                .gap_2()
                .mt_1()
                .pt_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_xs()
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child("进程"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .when(process.is_none(), |text| {
                            text.text_color(cx.theme().muted_foreground)
                        })
                        .child(process.unwrap_or_else(|| "—".into())),
                )
                .when_some(socket.user.clone(), |row, user| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .child(user),
                    )
                }),
        )
}
