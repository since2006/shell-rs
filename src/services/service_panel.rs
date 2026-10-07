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
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::linux::{self, Parsed};
use super::model::{
    ActiveState, Service, ServiceCommand, ServiceFilter, ServiceRow, ServiceTable, file_state_label,
};
use crate::app::{CatalogIcon, ControlService, RefreshServices, ShowService};
use crate::i18n::{UiLocale, t};
use crate::shared::{soft_tag, tinted};
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// How long a terminal not connected is left before it is asked again.
const RETRY: Duration = Duration::from_secs(2);

/// 系统服务: the systemd services of the host of the SSH terminal in front,
/// the administrator's own first, read when the panel comes on screen, on
/// 刷新 and after a command; each can be started, stopped, restarted and
/// opened for its details and journal.
///
/// Each reading runs `linux::command` beside the shell, on the terminal's
/// own connection. Services change seldom and a reading starts a handful
/// of `systemctl`s, so the list is not read on a timer. What each
/// terminal's host showed last is kept, so coming back to a terminal shows
/// its list at once while it is read again. The tab and the search stay as
/// they are from one terminal to the next.
pub struct ServicePanel {
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// The reading on its way; dropping it stops it.
    reading: Option<Task<()>>,
    /// Whether a reading is on its way.
    loading: bool,
    search: Entity<InputState>,
    query: String,
    filter: ServiceFilter,
    /// The list's lines differ in height (headings and cards), so it is a
    /// list that measures them rather than a uniform one.
    list: ListState,
    /// The lines the list was last told about.
    rows: Vec<ServiceRow>,
    /// The tab or the search changed: the list goes back to its top.
    back_to_top: bool,
    /// The service a right click landed on, for the list's menu; `None`
    /// off every service.
    menu_hit: Rc<RefCell<Option<Service>>>,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

#[derive(Default)]
struct HostState {
    table: Option<Rc<ServiceTable>>,
    problem: Option<Problem>,
}

#[derive(Clone, Debug, PartialEq)]
enum Problem {
    /// The terminal is not connected, so there is no connection to read on.
    NotConnected,
    /// Not a host systemd runs; `uname -s` when the host said it.
    Unsupported(Option<String>),
    /// The command did not run, or did not finish.
    Failed(String),
}

impl ServicePanel {
    pub fn new(dispatch: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("services.panel.search"))
                .clean_on_escape()
        });
        let subscriptions = vec![
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.search.update(cx, |search, cx| {
                    search.set_placeholder(t!("services.panel.search"), window, cx)
                });
            }),
            cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = input.read(cx).value().to_string();
                    this.back_to_top = true;
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
            filter: ServiceFilter::default(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            rows: Vec::new(),
            back_to_top: false,
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
        if self.target != target {
            self.back_to_top = true;
        }
        self.target = target;
        self.active = active;
        self.read(cx);
        cx.notify();
    }

    /// Read the host again: 刷新, and after a command.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.read(cx);
        cx.notify();
    }

    /// A service of the host on screen, as the list last showed it.
    pub fn service(&self, name: &str) -> Option<Service> {
        let target = self.target.as_ref()?;
        self.hosts
            .get(&target.terminal)?
            .table
            .as_ref()?
            .service(name)
            .cloned()
    }

    fn show(&mut self, filter: ServiceFilter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.back_to_top = true;
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
        self.loading = false;
        cx.notify();
    }

    /// Tell the list about `rows` when they are not the ones it has; back
    /// at the top when the tab or the search changed.
    fn sync_list(&mut self, rows: &[ServiceRow]) {
        if std::mem::take(&mut self.back_to_top) {
            self.list.reset(rows.len());
        } else if self.rows != rows {
            self.list.splice(0..self.rows.len(), rows.len());
        }
        self.rows = rows.to_vec();
    }

    fn render_list(
        &mut self,
        table: Rc<ServiceTable>,
        problem: Option<Problem>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows = table.rows(self.filter, &self.query);
        self.sync_list(&rows);
        let counts = table.counts(&self.query);
        let summary = table.summary();
        let refresh = self.dispatch.clone();
        let header =
            v_flex()
                .flex_shrink_0()
                .px_3()
                .pt_3()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .id("services-summary")
                                .test_support()
                                .aria_label(summary.clone())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(if table.degraded() {
                                    cx.theme().warning
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(summary),
                        )
                        .child(
                            Button::new("services-refresh")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(CatalogIcon::RefreshCw))
                                .loading(self.loading)
                                .tooltip(t!("tools.refresh"))
                                .accessibility_label(t!("tools.refresh"))
                                .on_click(move |_, window, cx| {
                                    refresh.dispatch_action(&RefreshServices, window, cx)
                                }),
                        ),
                )
                .when_some(problem, |header, problem| {
                    header.child(render_problem(&problem, cx))
                })
                .child(
                    Input::new(&self.search)
                        .id("services-search")
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                )
                .child(
                    // gpui-kit's underlined tabs, each as wide as its label and
                    // count: 「已停止 202」 does not fit an equal share of the
                    // panel, and where they all do not fit the bar scrolls
                    // sideways rather than cut a label short (the user's
                    // choice; Docker's shorter labels share the width).
                    div().id("services-tabs").test_support().child(
                        TabBar::new("services-tab-bar")
                            .underline()
                            .small()
                            .selected_index(
                                ServiceFilter::ALL
                                    .iter()
                                    .position(|filter| *filter == self.filter)
                                    .unwrap_or(0),
                            )
                            .on_click(cx.listener(|this, index: &usize, _, cx| {
                                this.show(ServiceFilter::ALL[*index], cx)
                            }))
                            .children(ServiceFilter::ALL.iter().zip(counts).map(
                                |(filter, count)| {
                                    Tab::new()
                                        .label(filter.label())
                                        .aria_label(format!("{} {count}", filter.label()))
                                        // Beside the label: the suffix would sit
                                        // outside the label's box.
                                        .child(
                                            div()
                                                .ml_1()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(count.to_string()),
                                        )
                                },
                            )),
                    ),
                );

        let clear_hit = self.menu_hit.clone();
        let menu_hit = self.menu_hit.clone();
        let empty = if self.query.trim().is_empty() {
            t!("services.panel.empty")
        } else {
            t!("services.panel.no_match")
        };
        // The scrollbar goes on the list's box, which does not scroll: on
        // the scrolled lines it would scroll away with them. The menu hangs
        // off the box too, not off the cards, which are drawn after layout
        // (see the host tree).
        let list = div()
            .id("services-list")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .map(|list| {
                if rows.is_empty() {
                    list.child(
                        div()
                            .id("services-empty")
                            .test_support()
                            .aria_label(empty.clone())
                            .py_8()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty),
                    )
                } else {
                    let rows = Rc::new(rows);
                    let (hit, focus, dispatch) = (
                        self.menu_hit.clone(),
                        self.focus_handle.clone(),
                        self.dispatch.clone(),
                    );
                    list.child(
                        gpui_kit::list(self.list.clone(), move |index, _, cx| {
                            match rows[index] {
                                ServiceRow::Group { custom, count } => {
                                    render_group(custom, count, cx).into_any_element()
                                }
                                ServiceRow::Service(service) => div()
                                    .px_3()
                                    .pt_2()
                                    // The last one clears the bottom.
                                    .when(index + 1 == rows.len(), |row| row.pb_3())
                                    .child(render_service(
                                        &table.services()[service],
                                        hit.clone(),
                                        focus.clone(),
                                        dispatch.clone(),
                                        cx,
                                    ))
                                    .into_any_element(),
                            }
                        })
                        .size_full(),
                    )
                }
            })
            .vertical_scrollbar(&self.list)
            .capture_any_mouse_down(move |event, _, _| {
                if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                    clear_hit.replace(None);
                }
            })
            .context_menu(move |menu, _, _| build_context_menu(menu_hit.borrow().as_ref(), menu));
        v_flex().size_full().child(header).child(list)
    }
}

impl Focusable for ServicePanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ServicePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let host = self
            .target
            .as_ref()
            .and_then(|target| self.hosts.get(&target.terminal));
        let table = host.and_then(|host| host.table.clone());
        let problem = host.and_then(|host| host.problem.clone());
        let body = match (table, problem) {
            // A list from before a failed reading still says something;
            // the failure goes above it.
            (Some(table), problem @ (None | Some(Problem::Failed(_)))) => {
                self.render_list(table, problem, cx).into_any_element()
            }
            (_, Some(problem)) => div()
                .p_3()
                .child(render_problem(&problem, cx))
                .into_any_element(),
            (None, None) => h_flex()
                .id("services-loading")
                .test_support()
                .gap_2()
                .justify_center()
                .py_8()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Spinner::new().small())
                .child(t!("tools.reading"))
                .into_any_element(),
        };
        v_flex()
            .id("services")
            .test_support()
            .track_focus(&self.focus_handle)
            .size_full()
            .child(body)
    }
}

fn render_problem(problem: &Problem, cx: &App) -> impl IntoElement {
    let (text, color) = match problem {
        Problem::NotConnected => (
            t!("services.panel.not_connected"),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(Some(system)) if system == "Linux" => {
            (t!("services.panel.no_systemd"), cx.theme().muted_foreground)
        }
        Problem::Unsupported(Some(system)) => (
            t!("services.panel.unsupported_system", system = system),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported(None) => (
            t!("services.panel.unsupported"),
            cx.theme().muted_foreground,
        ),
        Problem::Failed(error) => (t!("tools.read_failed", error = error), cx.theme().danger),
    };
    div()
        .id("services-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

fn build_context_menu(hit: Option<&Service>, menu: PopupMenu) -> PopupMenu {
    let Some(service) = hit else {
        return menu;
    };
    let action = |command: ServiceCommand| -> Box<dyn Action> {
        Box::new(ControlService {
            name: service.name.clone(),
            command,
        })
    };
    let menu = menu
        .menu_with_icon(
            t!("tools.menu.details"),
            Icon::new(IconName::Info),
            Box::new(ShowService(service.name.clone())),
        )
        .separator();
    let menu = service.commands().iter().fold(menu, |menu, &command| {
        menu.menu_with_icon(command.label(), command_icon(command), action(command))
    });
    match service.boot_command() {
        Some(command) => {
            menu.separator()
                .menu_with_icon(command.label(), command_icon(command), action(command))
        }
        None => menu,
    }
}

pub(super) fn command_icon(command: ServiceCommand) -> Icon {
    Icon::new(match command {
        ServiceCommand::Start => CatalogIcon::Play,
        ServiceCommand::Stop => CatalogIcon::Square,
        ServiceCommand::Restart => CatalogIcon::RotateCw,
        ServiceCommand::Enable => CatalogIcon::CircleCheck,
        ServiceCommand::Disable => CatalogIcon::Ban,
    })
}

/// What a state looks like: green running, red failed, yellow on its way
/// up or down, the rest quiet.
pub(super) fn state_color(state: ActiveState, cx: &App) -> Hsla {
    match state {
        ActiveState::Active | ActiveState::Reloading => cx.theme().success,
        ActiveState::Failed => cx.theme().danger,
        ActiveState::Activating | ActiveState::Deactivating => cx.theme().warning,
        ActiveState::Inactive | ActiveState::Other => cx.theme().muted_foreground,
    }
}

/// 「自定义服务 3」.
fn render_group(custom: bool, count: usize, cx: &App) -> impl IntoElement {
    let label = if custom {
        t!("services.group.custom")
    } else {
        t!("services.group.system")
    };
    h_flex()
        .id(if custom {
            "services-group:custom"
        } else {
            "services-group:system"
        })
        .test_support()
        .aria_label(format!("{label} {count}"))
        .px_4()
        .pt_4()
        .pb_1()
        .gap_2()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(label),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(count.to_string()),
        )
}

/// A button on a card: the command in its own tint for starting and
/// stopping, quiet for the rest.
fn command_button(
    service: &Service,
    command: ServiceCommand,
    dispatch: &FocusHandle,
    cx: &App,
) -> Button {
    let button = Button::new(SharedString::from(format!(
        "service-{}:{}",
        command.verb(),
        service.name
    )))
    .small()
    .icon(command_icon(command))
    .tooltip(command.label())
    .accessibility_label(command.label());
    let button = match command {
        ServiceCommand::Stop => button.custom(tinted(cx.theme().danger, cx)),
        ServiceCommand::Start => button.custom(tinted(cx.theme().success, cx)),
        _ => button.ghost(),
    };
    let (dispatch, name) = (dispatch.clone(), service.name.clone());
    button.on_click(move |_, window, cx| {
        // Not the card's click too, which opens the details.
        cx.stop_propagation();
        dispatch.dispatch_action(
            &ControlService {
                name: name.clone(),
                command,
            },
            window,
            cx,
        )
    })
}

/// A service's card. A click opens its details.
fn render_service(
    service: &Service,
    menu_hit: Rc<RefCell<Option<Service>>>,
    focus: FocusHandle,
    dispatch: FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let color = state_color(service.active, cx);
    let boot = file_state_label(&service.file_state);
    let label = [
        Some(SharedString::from(service.name.clone())),
        Some(SharedString::from(service.description.clone())).filter(|text| !text.is_empty()),
        Some(service.active.label()),
        boot.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let name = service.name.clone();
    let open = dispatch.clone();
    let hit = service.clone();
    v_flex()
        .id(SharedString::from(format!("service:{}", service.name)))
        .test_support()
        .aria_label(label)
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .text_color(cx.theme().group_box_foreground)
        .border_1()
        .border_color(cx.theme().tokens.group_box)
        .hover({
            let border = cx.theme().border;
            move |style| style.border_color(border)
        })
        .cursor_pointer()
        .on_click(move |_, window, cx| open.dispatch_action(&ShowService(name.clone()), window, cx))
        // A right click puts the menu on this service. The menu's commands
        // are dispatched from what is focused, which may be nothing.
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            menu_hit.replace(Some(hit.clone()));
            focus.focus(window, cx);
        })
        .child(
            h_flex()
                .gap_3()
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_shrink_0().size_2().rounded_full().bg(color))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(service.name.clone()),
                                ),
                        )
                        .child(
                            // Under the name, past the dot.
                            div()
                                .pl(rems(1.))
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if service.description.is_empty() {
                                    t!("services.panel.no_description")
                                } else {
                                    service.description.clone().into()
                                }),
                        )
                        .child(
                            h_flex()
                                .pl(rems(1.))
                                .pt_0p5()
                                .gap_1()
                                .child(soft_tag(service.active.label(), color))
                                .when_some(boot, |tags, boot| {
                                    let tint = if service.file_state.starts_with("enabled") {
                                        cx.theme().success
                                    } else {
                                        cx.theme().muted_foreground
                                    };
                                    tags.child(soft_tag(boot, tint))
                                }),
                        ),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .children(
                            service
                                .commands()
                                .iter()
                                .map(|&command| command_button(service, command, &dispatch, cx)),
                        )
                        .child({
                            let (dispatch, name) = (dispatch.clone(), service.name.clone());
                            Button::new(SharedString::from(format!(
                                "service-more:{}",
                                service.name
                            )))
                            .ghost()
                            .small()
                            .icon(IconName::Ellipsis)
                            .tooltip(t!("tools.details_and_logs"))
                            .accessibility_label(t!("tools.details_and_logs"))
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                dispatch.dispatch_action(&ShowService(name.clone()), window, cx)
                            })
                        }),
                ),
        )
}
