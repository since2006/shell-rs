use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    spinner::Spinner,
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::linux::{self, Parsed};
use super::model::{
    Container, ContainerCommand, ContainerState, DockerObject, DockerRow, DockerTab, DockerTable,
    ObjectSummary, Project,
};
use super::object_details::tone_color;
use crate::app::{
    CatalogIcon, ControlContainers, RefreshDocker, RemoveDockerObject, ShowDockerObject,
    ToggleDockerProject,
};
use crate::shared::{count_tabs, soft_tag, tinted};
use crate::terminal::{ExecResult, ExecTarget, RemoteTerminalId, exec_answer};

/// How long a terminal not connected is left before it is asked again.
const RETRY: Duration = Duration::from_secs(2);

/// Docker: the containers of the host of the SSH terminal in front, by
/// compose project, and its volumes, images and networks, read when the
/// panel comes on screen, on 刷新 and after a command. Containers start,
/// stop and restart, a project's all at once, and open for their details
/// and output; what nothing uses can be removed.
///
/// Each reading runs `linux::command` beside the shell, on the terminal's
/// own connection. What each terminal's host showed last is kept, so
/// coming back to a terminal shows its list at once while it is read
/// again. The tab stays as it is from one terminal to the next.
pub struct DockerPanel {
    target: Option<ExecTarget>,
    active: bool,
    hosts: HashMap<RemoteTerminalId, HostState>,
    /// The reading on its way; dropping it stops it.
    reading: Option<Task<()>>,
    /// Whether a reading is on its way.
    loading: bool,
    tab: DockerTab,
    /// Compose projects folded or unfolded by hand, by name. The rest are
    /// as [`Project::unfolded_at_first`] says.
    unfolded: HashMap<String, bool>,
    /// The list's lines differ in height (a project with its containers,
    /// a heading, a container), so it is a list that measures them.
    list: ListState,
    /// The lines the list was last told about.
    rows: Vec<DockerRow>,
    /// The tab or the terminal changed: the list goes back to its top.
    back_to_top: bool,
    /// The workspace's focus handle: the panel's buttons dispatch on it, so
    /// they work whatever is focused.
    dispatch: FocusHandle,
}

#[derive(Default)]
struct HostState {
    table: Option<Rc<DockerTable>>,
    problem: Option<Problem>,
}

#[derive(Clone, Debug, PartialEq)]
enum Problem {
    /// The terminal is not connected, so there is no connection to read on.
    NotConnected,
    /// No `docker` on the host.
    Missing,
    /// Not a shell that runs `sh` (cmd.exe).
    Unsupported,
    /// Docker would not answer: why.
    Unreachable(String),
    /// The command did not run, or did not finish.
    Failed(String),
}

impl DockerPanel {
    pub fn new(dispatch: FocusHandle) -> Self {
        Self {
            target: None,
            active: false,
            hosts: HashMap::new(),
            reading: None,
            loading: false,
            tab: DockerTab::default(),
            unfolded: HashMap::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            rows: Vec::new(),
            back_to_top: false,
            dispatch,
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

    /// Fold a compose project away, or unfold it.
    pub fn toggle_project(&mut self, name: &str, cx: &mut Context<Self>) {
        let unfolded = self
            .table()
            .and_then(|table| {
                table
                    .projects()
                    .iter()
                    .find(|project| project.name == name)
                    .map(|project| self.is_unfolded(project))
            })
            .unwrap_or(false);
        self.unfolded.insert(name.to_owned(), !unfolded);
        cx.notify();
    }

    /// A container of the host on screen, as the list last showed it.
    pub fn container(&self, id: &str) -> Option<Container> {
        self.table()?.container(id).cloned()
    }

    /// A volume, an image or a network of the host on screen, as its card
    /// last showed it.
    pub fn summary_of(&self, object: DockerObject, id: &str) -> Option<ObjectSummary> {
        self.table()?.summary_of(object, id)
    }

    fn table(&self) -> Option<&DockerTable> {
        let target = self.target.as_ref()?;
        self.hosts.get(&target.terminal)?.table.as_deref()
    }

    fn is_unfolded(&self, project: &Project) -> bool {
        self.unfolded
            .get(&project.name)
            .copied()
            .unwrap_or_else(|| project.unfolded_at_first())
    }

    fn show(&mut self, tab: DockerTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
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
                Parsed::Unreachable(why) => Some(Problem::Unreachable(why)),
                Parsed::Missing(_) => Some(Problem::Missing),
                Parsed::Unsupported => Some(Problem::Unsupported),
            },
        };
        self.loading = false;
        cx.notify();
    }

    /// Tell the list about `rows` when they are not the ones it has; back
    /// at the top when the tab or the terminal changed.
    fn sync_list(&mut self, rows: &[DockerRow]) {
        if std::mem::take(&mut self.back_to_top) {
            self.list.reset(rows.len());
        } else if self.rows != rows {
            self.list.splice(0..self.rows.len(), rows.len());
        }
        self.rows = rows.to_vec();
    }

    fn render_table(
        &mut self,
        table: Rc<DockerTable>,
        problem: Option<Problem>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows = table.rows(self.tab, |project| self.is_unfolded(project));
        self.sync_list(&rows);
        let summary = table.summary();
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
                            .id("docker-summary")
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
                        Button::new("docker-refresh")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(CatalogIcon::RefreshCw))
                            .loading(self.loading)
                            .tooltip("刷新")
                            .accessibility_label("刷新")
                            .on_click(move |_, window, cx| {
                                refresh.dispatch_action(&RefreshDocker, window, cx)
                            }),
                    ),
            )
            .when_some(problem, |header, problem| {
                header.child(render_problem(&problem, cx))
            })
            .child(count_tabs(
                "docker-tabs",
                DockerTab::ALL
                    .iter()
                    .map(|tab| tab.label())
                    .zip(table.counts()),
                DockerTab::ALL
                    .iter()
                    .position(|tab| *tab == self.tab)
                    .unwrap_or(0),
                cx.listener(|this, index: &usize, _, cx| this.show(DockerTab::ALL[*index], cx)),
            ));

        let empty = match self.tab {
            DockerTab::Containers => "没有容器",
            DockerTab::Volumes => "没有卷",
            DockerTab::Images => "没有镜像",
            DockerTab::Networks => "没有网络",
        };
        // The scrollbar goes on the list's box, which does not scroll: on
        // the scrolled lines it would scroll away with them.
        let list = div()
            .id("docker-list")
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
                            .id("docker-empty")
                            .test_support()
                            .aria_label(empty)
                            .py_8()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty),
                    )
                } else {
                    let rows = Rc::new(rows);
                    let dispatch = self.dispatch.clone();
                    list.child(
                        gpui_kit::list(self.list.clone(), move |index, _, cx| {
                            let last = index + 1 == rows.len();
                            let row = match &rows[index] {
                                DockerRow::Project { index, unfolded } => render_project(
                                    &table,
                                    &table.projects()[*index],
                                    *unfolded,
                                    &dispatch,
                                    cx,
                                )
                                .into_any_element(),
                                DockerRow::Standalone => div()
                                    .px_1()
                                    .pt_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("独立容器")
                                    .into_any_element(),
                                DockerRow::Container(index) => render_container(
                                    &table.containers()[*index],
                                    false,
                                    &dispatch,
                                    cx,
                                )
                                .into_any_element(),
                                DockerRow::Volume(index) => {
                                    let volume = &table.volumes()[*index];
                                    render_object(
                                        format!("docker-volume:{}", volume.name).into(),
                                        volume.summary(),
                                        &dispatch,
                                        cx,
                                    )
                                    .into_any_element()
                                }
                                DockerRow::Image(index) => {
                                    let image = &table.images()[*index];
                                    render_object(
                                        format!("docker-image:{}", image.id).into(),
                                        image.summary(),
                                        &dispatch,
                                        cx,
                                    )
                                    .into_any_element()
                                }
                                DockerRow::Network(index) => {
                                    let network = &table.networks()[*index];
                                    render_object(
                                        format!("docker-network:{}", network.name).into(),
                                        network.summary(),
                                        &dispatch,
                                        cx,
                                    )
                                    .into_any_element()
                                }
                            };
                            div()
                                .px_3()
                                .pt_2()
                                .when(last, |row| row.pb_3())
                                .child(row)
                                .into_any_element()
                        })
                        .size_full(),
                    )
                }
            })
            .vertical_scrollbar(&self.list);
        v_flex().size_full().child(header).child(list)
    }
}

impl Render for DockerPanel {
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
                self.render_table(table, problem, cx).into_any_element()
            }
            (_, Some(problem)) => div()
                .p_3()
                .child(render_problem(&problem, cx))
                .into_any_element(),
            (None, None) => h_flex()
                .id("docker-loading")
                .test_support()
                .gap_2()
                .justify_center()
                .py_8()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Spinner::new().small())
                .child("正在读取…")
                .into_any_element(),
        };
        v_flex().id("docker").test_support().size_full().child(body)
    }
}

fn render_problem(problem: &Problem, cx: &App) -> impl IntoElement {
    let (text, color) = match problem {
        Problem::NotConnected => (
            "终端没有连接。连接后这里显示主机上的容器。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Missing => (
            "这台主机没有安装 Docker。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Unsupported => (
            "暂不支持这台主机上的 Docker。".to_string(),
            cx.theme().muted_foreground,
        ),
        Problem::Unreachable(why) => (format!("无法读取 Docker：{why}"), cx.theme().danger),
        Problem::Failed(error) => (format!("读取失败：{error}"), cx.theme().danger),
    };
    div()
        .id("docker-message")
        .test_support()
        .aria_label(text.clone())
        .text_sm()
        .text_color(color)
        .child(text)
}

/// What a container's state looks like: green up, yellow on its way or
/// paused, red dead, the rest quiet.
pub(super) fn state_color(state: ContainerState, cx: &App) -> Hsla {
    match state {
        ContainerState::Running => cx.theme().success,
        ContainerState::Paused | ContainerState::Restarting | ContainerState::Removing => {
            cx.theme().warning
        }
        ContainerState::Dead => cx.theme().danger,
        ContainerState::Created | ContainerState::Exited => cx.theme().muted_foreground,
    }
}

fn dot(color: Hsla) -> Div {
    div().flex_shrink_0().size_2().rounded_full().bg(color)
}

/// Stop or start, then restart: what a container or a project offers,
/// stop while something in it is up.
fn command_buttons(
    id: &str,
    subject: String,
    ids: Vec<String>,
    up: bool,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let button = |command: ContainerCommand| {
        let (dispatch, subject, ids) = (dispatch.clone(), subject.clone(), ids.clone());
        let button = Button::new(SharedString::from(format!(
            "docker-{}:{id}",
            command.verb()
        )))
        .small()
        .icon(Icon::new(match command {
            ContainerCommand::Start => CatalogIcon::Play,
            ContainerCommand::Stop => CatalogIcon::Square,
            ContainerCommand::Restart => CatalogIcon::RotateCw,
        }))
        .tooltip(command.label())
        .accessibility_label(command.label())
        .on_click(move |_, window, cx| {
            // Not the line's click too.
            cx.stop_propagation();
            dispatch.dispatch_action(
                &ControlContainers {
                    subject: subject.clone(),
                    ids: ids.clone(),
                    command,
                },
                window,
                cx,
            )
        });
        match command {
            ContainerCommand::Stop => button.custom(tinted(cx.theme().danger, cx)),
            ContainerCommand::Start => button.custom(tinted(cx.theme().success, cx)),
            ContainerCommand::Restart => button.ghost(),
        }
    };
    h_flex()
        .flex_shrink_0()
        .gap_1()
        .child(button(if up {
            ContainerCommand::Stop
        } else {
            ContainerCommand::Start
        }))
        .child(button(ContainerCommand::Restart))
}

/// A compose project's card: a line for the project, a click on which
/// folds and unfolds it, and its containers under it when unfolded.
fn render_project(
    table: &DockerTable,
    project: &Project,
    unfolded: bool,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let total = project.containers.len();
    let color = match project.running {
        0 => cx.theme().muted_foreground,
        running if running == total => cx.theme().success,
        _ => cx.theme().warning,
    };
    let ids: Vec<String> = project
        .containers
        .iter()
        .map(|index| table.containers()[*index].id.clone())
        .collect();
    let toggle = dispatch.clone();
    let name = project.name.clone();
    v_flex()
        .id(SharedString::from(format!(
            "docker-project:{}",
            project.name
        )))
        .test_support()
        .aria_label(format!(
            "{} · {}/{total} 运行中",
            project.name, project.running
        ))
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .border_1()
        .border_color(cx.theme().border)
        .child(
            h_flex()
                .id(SharedString::from(format!(
                    "docker-project-line:{}",
                    project.name
                )))
                .test_support()
                .gap_2()
                .p_3()
                .cursor_pointer()
                .on_click(move |_, window, cx| {
                    toggle.dispatch_action(&ToggleDockerProject(name.clone()), window, cx)
                })
                .child(
                    Icon::new(if unfolded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .small()
                    .text_color(cx.theme().muted_foreground),
                )
                .child(dot(color))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(project.name.clone()),
                                )
                                .child(
                                    Tag::secondary()
                                        .small()
                                        .rounded_full()
                                        .child(format!("{}/{total}", project.running)),
                                ),
                        )
                        .when_some(project.working_dir.clone(), |column, directory| {
                            column.child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(directory),
                            )
                        }),
                )
                .child(command_buttons(
                    &format!("project:{}", project.name),
                    format!("项目“{}”", project.name),
                    ids,
                    project.running > 0,
                    dispatch,
                    cx,
                )),
        )
        .when(unfolded, |card| {
            card.children(project.containers.iter().map(|index| {
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pl_6()
                    .child(render_container(
                        &table.containers()[*index],
                        true,
                        dispatch,
                        cx,
                    ))
            }))
        })
}

/// A container's line: in its project's card, or on its own. A click
/// opens its details.
fn render_container(
    container: &Container,
    in_project: bool,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let detail = if container.ports.is_empty() {
        container.image.clone()
    } else {
        container.ports.clone()
    };
    let (open, more) = (dispatch.clone(), dispatch.clone());
    let (id, more_id) = (container.id.clone(), container.id.clone());
    h_flex()
        .id(SharedString::from(format!(
            "docker-container:{}",
            container.name
        )))
        .test_support()
        .aria_label(format!(
            "{} · {} · {detail}",
            container.name,
            container.state.label()
        ))
        .gap_2()
        .p_3()
        .rounded(cx.theme().radius)
        .cursor_pointer()
        .when(!in_project, |line| {
            line.hover({
                let hover = cx.theme().list_hover;
                move |style| style.bg(hover)
            })
        })
        .on_click(move |_, window, cx| {
            open.dispatch_action(
                &ShowDockerObject {
                    object: DockerObject::Container,
                    id: id.clone(),
                },
                window,
                cx,
            )
        })
        .child(dot(state_color(container.state, cx)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(container.name.clone()),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(detail),
                ),
        )
        .child(command_buttons(
            &container.name,
            format!("容器“{}”", container.name),
            vec![container.id.clone()],
            container.state.is_up(),
            dispatch,
            cx,
        ))
        .child(
            Button::new(SharedString::from(format!(
                "docker-more:{}",
                container.name
            )))
            .ghost()
            .small()
            .icon(IconName::Ellipsis)
            .tooltip("详情和日志")
            .accessibility_label("详情和日志")
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                more.dispatch_action(
                    &ShowDockerObject {
                        object: DockerObject::Container,
                        id: more_id.clone(),
                    },
                    window,
                    cx,
                )
            }),
        )
}

/// A card for a volume, an image or a network: its name, a line about
/// it, a tag, and the button that removes it when nothing uses it. A click
/// opens its details.
fn render_object(
    card: SharedString,
    summary: ObjectSummary,
    dispatch: &FocusHandle,
    cx: &App,
) -> impl IntoElement {
    let label = [
        Some(summary.name.clone()),
        Some(summary.detail.clone()),
        summary.tag.clone().map(|(tag, _)| tag),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let (open, remove) = (dispatch.clone(), dispatch.clone());
    let show = ShowDockerObject {
        object: summary.object,
        id: summary.id.clone(),
    };
    let removal = RemoveDockerObject {
        object: summary.object,
        id: summary.id.clone(),
        name: summary.name.clone(),
    };
    h_flex()
        .id(card.clone())
        .test_support()
        .aria_label(label)
        .gap_2()
        .p_3()
        .rounded(cx.theme().radius)
        .bg(cx.theme().tokens.group_box)
        .border_1()
        .border_color(cx.theme().tokens.group_box)
        .hover({
            let border = cx.theme().border;
            move |style| style.border_color(border)
        })
        .cursor_pointer()
        .on_click(move |_, window, cx| open.dispatch_action(&show, window, cx))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            // 「registry.ap-…/hello-world:latest」: a long
                            // registry's name goes, the image's stays.
                            div()
                                .min_w_0()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis_middle()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(summary.name.clone()),
                        )
                        .when_some(summary.tag.clone(), |line, (tag, tone)| {
                            line.child(soft_tag(tag, tone_color(tone, cx)).flex_shrink_0())
                        }),
                )
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(summary.detail.clone()),
                ),
        )
        .child(
            Button::new(SharedString::from(format!("{card}-remove")))
                .ghost()
                .small()
                .icon(Icon::new(CatalogIcon::Trash))
                .tooltip(summary.removable.err().unwrap_or("删除"))
                .accessibility_label("删除")
                .disabled(summary.removable.is_err())
                .on_click(move |_, window, cx| {
                    // Not the card's click too, which opens the details.
                    cx.stop_propagation();
                    remove.dispatch_action(&removal, window, cx)
                }),
        )
}
