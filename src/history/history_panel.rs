use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::bash::{self, Parsed};
use super::model::{Entry, History};
use crate::app::{CatalogIcon, CopyCommand, EnterCommand, RefreshHistory};
use crate::shared::command_tooltip;
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// How long a terminal not connected is left before it is asked again.
const RETRY: Duration = Duration::from_secs(2);

/// 历史命令: the commands bash kept in `~/.bash_history` on the host of the
/// SSH terminal in front, the newest first, each once. A click puts one on
/// the terminal's input line to edit; 执行 runs it there.
///
/// Read when the panel comes on screen and again on 刷新, beside the shell
/// on the terminal's own connection: no second login. bash writes the file
/// as a shell exits, so what this terminal ran shows up after it does,
/// which the panel says. What each terminal's host showed last is kept, so
/// coming back to a terminal shows its list at once while it is read
/// again. The search stays as it is from one terminal to the next.
pub struct HistoryPanel {
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// The reading on its way; dropping it stops it.
    reading: Option<Task<()>>,
    /// Whether a reading is on its way.
    loading: bool,
    search: Entity<InputState>,
    query: String,
    scroll_handle: UniformListScrollHandle,
    /// The command a right click landed on, for the list's menu; `None`
    /// off every command.
    menu_hit: Rc<RefCell<Option<String>>>,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct HostState {
    history: Option<Rc<History>>,
    problem: Option<Problem>,
}

#[derive(Clone, Debug, PartialEq)]
enum Problem {
    /// The terminal is not connected, so there is no connection to read on.
    NotConnected,
    /// No `~/.bash_history` to read.
    Missing,
    /// No `sh` to run the command: Windows.
    Unsupported,
    /// The command did not run, or did not finish.
    Failed(String),
}

impl HistoryPanel {
    pub fn new(dispatch: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("搜索命令")
                .clean_on_escape()
        });
        let subscriptions = vec![
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
            reading: None,
            loading: false,
            search,
            query: String::new(),
            scroll_handle: UniformListScrollHandle::new(),
            menu_hit: Rc::new(RefCell::new(None)),
            dispatch,
            focus_handle: cx.focus_handle(),
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

    /// Read the history again: 刷新.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.read(cx);
        cx.notify();
    }

    /// Start reading the target's host over, or stop when there is nothing
    /// to show. A terminal not connected is asked again every little while,
    /// so its history comes once it connects.
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
                    view.read(cx).exec(bash::command(), cx)
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
            Some(Ok(output)) => match bash::parse(&output) {
                Parsed::History(history) => {
                    host.history = Some(Rc::new(history));
                    None
                }
                Parsed::Missing => {
                    host.history = None;
                    Some(Problem::Missing)
                }
                Parsed::Unsupported => Some(Problem::Unsupported),
            },
        };
        // Not connected, it keeps asking; the spinner stops all the same.
        self.loading = false;
        cx.notify();
    }

    fn render_history(
        &self,
        history: Rc<History>,
        problem: Option<Problem>,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let rows = Rc::new(history.matching(&self.query));
        let summary = format!("共 {} 条 · ~/.bash_history", history.entries().len());
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
                            .id("history-summary")
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
                        Button::new("history-refresh")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CatalogIcon::RefreshCw))
                            .loading(self.loading)
                            .tooltip("刷新")
                            .accessibility_label("刷新")
                            .on_click(move |_, window, cx| {
                                refresh.dispatch_action(&RefreshHistory, window, cx)
                            }),
                    ),
            )
            .when_some(problem, |header, problem| {
                header.child(render_problem(&problem, cx))
            })
            .when(history.truncated(), |header| {
                header.child(note(
                    "history-truncated",
                    format!("历史文件太长，只读了最后 {} KB。", bash::LIMIT / 1024),
                    cx,
                ))
            })
            // Why the command just run is not there.
            .child(note(
                "history-note",
                "这个终端里执行的命令，bash 退出后才会出现在这里。".into(),
                cx,
            ))
            .child(
                Input::new(&self.search)
                    .id("history-search")
                    .small()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            );

        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();
        // The scrollbar goes on the list's box, which does not scroll: on
        // the scrolled rows it would scroll away with them. The menu hangs
        // off the box too, not off the rows, which are drawn after layout
        // (see the host tree).
        let list = div()
            .id("history-list")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .map(|list| {
                if rows.is_empty() {
                    let empty = if self.query.trim().is_empty() {
                        "没有历史命令"
                    } else {
                        "没有符合条件的命令"
                    };
                    list.child(
                        div()
                            .id("history-empty")
                            .test_support()
                            .aria_label(empty)
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
                    let now = chrono::Utc::now().timestamp();
                    list.child(
                        uniform_list("history-entries", count, move |range, _, cx| {
                            range
                                .map(|index| {
                                    let entry = &history.entries()[rows[index]];
                                    div().px_3().pt_2().child(render_entry(
                                        entry,
                                        now,
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
                    clear_hit.borrow_mut().take();
                }
            })
            .context_menu(move |menu, _, _| build_context_menu(menu_hit.borrow().clone(), menu));
        v_flex().size_full().child(header).child(list)
    }
}

impl Focusable for HistoryPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self
            .target
            .as_ref()
            .and_then(|target| self.hosts.get(&target.terminal));
        let history = host.and_then(|host| host.history.clone());
        let problem = host.and_then(|host| host.problem.clone());
        v_flex()
            .id("history")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .map(|panel| match (history, problem) {
                // A list from before a failed reading still says something;
                // the failure goes above it.
                (Some(history), problem @ (None | Some(Problem::Failed(_)))) => {
                    panel.child(self.render_history(history, problem, cx))
                }
                (_, Some(problem)) => panel.child(div().p_3().child(render_problem(&problem, cx))),
                (None, None) => panel.child(
                    h_flex()
                        .id("history-loading")
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
            "终端没有连接。连接后这里显示主机上 bash 的历史命令。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Missing => (
            "这台主机上还没有 bash 的历史命令（~/.bash_history）。bash 退出时才会写入它。"
                .to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported => (
            "暂不支持读取这台主机的历史命令，只支持 bash 的 ~/.bash_history。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Failed(error) => (format!("读取失败：{error}"), cx.theme().danger),
    };
    div()
        .id("history-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

/// A line under the summary.
fn note(id: &'static str, text: String, cx: &App) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .aria_label(text.clone())
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn build_context_menu(hit: Option<String>, menu: PopupMenu) -> PopupMenu {
    let Some(command) = hit else {
        return menu;
    };
    menu.menu_with_icon(
        "输入到终端",
        Icon::new(CatalogIcon::SquareTerminal),
        Box::new(EnterCommand {
            command: command.clone(),
            run: false,
        }),
    )
    .menu_with_icon(
        "执行",
        Icon::new(CatalogIcon::Play),
        Box::new(EnterCommand {
            command: command.clone(),
            run: true,
        }),
    )
    .separator()
    .menu_with_icon(
        "复制",
        Icon::new(IconName::Copy),
        Box::new(CopyCommand(command)),
    )
}

/// A command's card: the command on one line, when it last ran and how
/// often below. Every card has the same lines, so they are all as tall as
/// the list takes them to be.
fn render_entry(
    entry: &Entry,
    now: i64,
    menu_hit: Rc<RefCell<Option<String>>>,
    focus: FocusHandle,
    dispatch: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let command = entry.command.clone();
    let line = entry.one_line();
    let summary = entry.summary(now);
    let id = SharedString::from(format!("history-entry:{command}"));
    let enter = EnterCommand {
        command: command.clone(),
        run: false,
    };
    let run = EnterCommand {
        command: command.clone(),
        run: true,
    };
    let tooltip = command_tooltip(&command, cx);
    let mono = cx.theme().mono_font_family.clone();
    h_flex()
        .id(id.clone())
        .test_support()
        .aria_label(format!("{line} · {summary}"))
        .gap_2()
        .py_2()
        .pl_3()
        .pr_2()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .text_color(cx.theme().group_box_foreground)
        // A click puts it on the terminal's line; the border says it can.
        .border_1()
        .border_color(cx.theme().tokens.group_box)
        .hover({
            let border = cx.theme().border;
            move |style| style.border_color(border)
        })
        .cursor_pointer()
        .on_click({
            let dispatch = dispatch.clone();
            move |_, window, cx| dispatch.dispatch_action(&enter, window, cx)
        })
        // A right click puts the menu on this command. The menu's commands
        // are dispatched from what is focused, which may be nothing.
        .on_mouse_down(MouseButton::Right, {
            let command = command.clone();
            move |_, window, cx| {
                menu_hit.replace(Some(command.clone()));
                focus.focus(window, cx);
            }
        })
        // The whole of a command too long for its line.
        .when_some(tooltip, |card, tooltip| card.tooltip(tooltip))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().truncate().font_family(mono).text_xs().child(line))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(summary),
                ),
        )
        .child(
            // As tall as the card's two lines allow: an easy target.
            Button::new(SharedString::from(format!("history-run:{command}")))
                .ghost()
                .icon(Icon::new(CatalogIcon::Play))
                .tooltip("执行")
                .accessibility_label("执行")
                .on_click(move |_, window, cx| {
                    // Not the card's click too, which would only put it on
                    // the line.
                    cx.stop_propagation();
                    dispatch.dispatch_action(&run, window, cx);
                }),
        )
}
