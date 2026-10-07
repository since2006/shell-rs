//! A service's details: the dialog a click on it in 系统服务 opens.

use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    h_flex,
    spinner::Spinner,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::*;

use super::linux::{log_command, parse_status, status_command};
use super::model::{
    ActiveState, Service, ServiceCommand, ServiceKind, ServiceStatus, file_state_label,
    load_state_label, sub_state_label,
};
use super::service_panel::{command_icon, state_color};
use crate::app::{CatalogIcon, ControlService};
use crate::i18n::t;
use crate::shared::{format_bytes, soft_tag};
use crate::terminal::{ExecResult, TerminalView, exec_answer};

/// How wide the dialog is: two columns of a label and a value, and the
/// journal's lines.
const DIALOG_WIDTH: f32 = 44.;

/// Open the details of a service of the host of `terminal`, whose
/// connection reads its state and journal. `dispatch` is the workspace's
/// focus handle: the dialog's buttons close it and dispatch there.
pub fn open_service_dialog(
    service: Service,
    terminal: WeakEntity<TerminalView>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| ServiceDetailsView::new(service, terminal, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let this = view.read(cx);
        let service = this.current();
        let color = state_color(service.active, cx);
        let title = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().truncate().child(service.name.clone()))
                    .child(soft_tag(service.active.label(), color)),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(cx.theme().muted_foreground)
                    .child(service.description.clone()),
            );
        let button = |command: ServiceCommand| {
            let (dispatch, name) = (dispatch.clone(), service.name.clone());
            let button = Button::new(SharedString::from(format!(
                "service-dialog-{}",
                command.verb()
            )))
            .icon(command_icon(command))
            .label(command.label())
            .on_click(move |_, window, cx| {
                // One dialog at a time: a confirmation takes its place.
                window.close_dialog(cx);
                dispatch.dispatch_action(
                    &ControlService {
                        name: name.clone(),
                        command,
                    },
                    window,
                    cx,
                );
            });
            match command {
                ServiceCommand::Stop => button.danger(),
                ServiceCommand::Start => button.primary(),
                _ => button.outline(),
            }
        };
        let footer = DialogFooter::new()
            .justify_start()
            .children(service.commands().iter().map(|&command| button(command)))
            .children(service.boot_command().map(button));
        dialog
            .title(title)
            // Dialog geometry is an API boundary that takes `Pixels`; the
            // width follows the interface zoom through the rem.
            .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
            .max_h(window.viewport_size().height * 0.9)
            // Closed by its buttons or Escape, like every dialog.
            .overlay_closable(false)
            .child(view.clone())
            .footer(footer)
    });
}

/// Something read off the host for the dialog.
enum Read<T> {
    /// Not asked for yet: the journal until its tab is opened.
    NotYet,
    Reading,
    Known(T),
    /// Why not: 「读取失败：命令超时」.
    Unknown(String),
}

/// The dialog's body: the service's state in full, and its journal.
pub struct ServiceDetailsView {
    service: Service,
    terminal: WeakEntity<TerminalView>,
    status: Read<ServiceStatus>,
    journal: Read<String>,
    /// 0 the state, 1 the journal.
    tab: usize,
    journal_scroll: ScrollHandle,
    _status_task: Task<()>,
    _journal_task: Option<Task<()>>,
}

impl ServiceDetailsView {
    fn new(service: Service, terminal: WeakEntity<TerminalView>, cx: &mut Context<Self>) -> Self {
        let status = run(
            &terminal,
            status_command(&service.name),
            cx,
            |this, answer, _| {
                this.status = match answer {
                    Ok(output) => match parse_status(&output) {
                        Some(status) => Read::Known(status),
                        None => Read::Unknown(t!("services.details.unreadable").into()),
                    },
                    Err(why) => Read::Unknown(why),
                };
            },
        );
        Self {
            service,
            terminal,
            status: Read::Reading,
            journal: Read::NotYet,
            tab: 0,
            journal_scroll: ScrollHandle::new(),
            _status_task: status,
            _journal_task: None,
        }
    }

    /// The service as its state now says, which may have moved on since
    /// the list read it.
    fn current(&self) -> Service {
        let mut service = self.service.clone();
        if let Read::Known(status) = &self.status {
            service.active = ActiveState::parse(&status.active);
            service.file_state = status.file_state.clone();
            if !status.description.is_empty() {
                service.description = status.description.clone();
            }
        }
        service
    }

    fn select(&mut self, tab: usize, cx: &mut Context<Self>) {
        self.tab = tab;
        if tab == 1 && matches!(self.journal, Read::NotYet) {
            self.journal = Read::Reading;
            self._journal_task = Some(run(
                &self.terminal,
                log_command(&self.service.name),
                cx,
                |this, answer, _| {
                    this.journal = match answer {
                        Ok(output) if output.trim().is_empty() => {
                            Read::Unknown(t!("services.details.no_journal").into())
                        }
                        Ok(output) => Read::Known(output.trim_end().to_owned()),
                        Err(why) => Read::Unknown(why),
                    };
                    // The newest lines are the last.
                    this.journal_scroll.scroll_to_bottom();
                },
            ));
        }
        cx.notify();
    }

    fn render_status(&self, cx: &App) -> AnyElement {
        let status = match &self.status {
            Read::Known(status) => status,
            Read::Unknown(why) => return message("service-status-unknown", why.clone(), cx),
            Read::Reading | Read::NotYet => return reading("service-status-reading", cx),
        };
        let active = ActiveState::parse(&status.active);
        let number = |value: Option<u64>| value.map_or("—".to_owned(), |value| value.to_string());
        let fields: [(&'static str, String); 10] = [
            ("services.field.load", load_state_label(&status.load).into()),
            (
                "services.field.active",
                format!("{} / {}", active.label(), sub_state_label(&status.sub)),
            ),
            (
                "services.field.boot",
                file_state_label(&status.file_state).map_or("—".into(), Into::into),
            ),
            (
                "services.field.main_pid",
                status.main_pid.map_or("—".into(), |pid| pid.to_string()),
            ),
            (
                "services.field.memory",
                status.memory.map_or("—".into(), format_bytes),
            ),
            ("services.field.tasks", number(status.tasks)),
            ("services.field.restarts", number(status.restarts)),
            (
                "services.field.exit_status",
                status
                    .exit_status
                    .map_or("—".into(), |status| status.to_string()),
            ),
            (
                "services.field.started",
                status.started.clone().unwrap_or("—".into()),
            ),
            (
                "services.field.stopped",
                // When it last went down; while it runs, that is history.
                status
                    .stopped
                    .clone()
                    .filter(|_| active.kind() != ServiceKind::Running)
                    .unwrap_or("—".into()),
            ),
        ];
        v_flex()
            .child(
                div().grid().grid_cols(2).gap_x_8().children(
                    fields
                        .into_iter()
                        .map(|(label, value)| field(label, value, cx)),
                ),
            )
            .child(field(
                "services.field.unit_file",
                status.path.clone().unwrap_or("—".into()),
                cx,
            ))
            .into_any_element()
    }

    fn render_journal(&self, cx: &App) -> AnyElement {
        match &self.journal {
            Read::Known(journal) => div()
                .id("service-journal")
                .test_support()
                .aria_label(journal.clone())
                .h(rems(20.))
                .overflow_y_scroll()
                .track_scroll(&self.journal_scroll)
                .p_3()
                .rounded(cx.theme().radius)
                .bg(cx.theme().muted)
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .child(journal.clone())
                .into_any_element(),
            Read::Unknown(why) => message("service-journal-unknown", why.clone(), cx),
            Read::Reading | Read::NotYet => reading("service-journal-reading", cx),
        }
    }
}

/// Run `command` on `terminal`'s connection, and hand the answer to
/// `record`: what it printed, or why it did not run.
fn run(
    terminal: &WeakEntity<TerminalView>,
    command: Option<String>,
    cx: &mut Context<ServiceDetailsView>,
    record: impl FnOnce(
        &mut ServiceDetailsView,
        Result<String, String>,
        &mut Context<ServiceDetailsView>,
    ) + 'static,
) -> Task<()> {
    let reply = command.and_then(|command| terminal.upgrade()?.read(cx).exec(command, cx));
    cx.spawn(async move |this, cx| {
        let answer: ExecResult = match reply {
            None => Err(t!("tools.not_connected").into()),
            Some(reply) => match exec_answer(reply, cx).await {
                None => Err(t!("tools.not_connected").into()),
                Some(Err(error)) => Err(t!("tools.read_failed", error = error).into()),
                Some(Ok(output)) => Ok(output),
            },
        };
        this.update(cx, |this, cx| {
            record(this, answer, cx);
            cx.notify();
        })
        .ok();
    })
}

/// A label, by its text's key, and its value, over a line. The line is
/// found by the last part of the key: 「service-field:main_pid」.
fn field(label: &'static str, value: String, cx: &App) -> impl IntoElement {
    let id = label.rsplit('.').next().unwrap_or(label);
    h_flex()
        .id(SharedString::from(format!("service-field:{id}")))
        .test_support()
        .aria_label(value.clone())
        .min_w_0()
        .gap_3()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border)
        .text_sm()
        .child(
            div()
                .w(rems(6.5))
                .flex_shrink_0()
                .text_color(cx.theme().muted_foreground)
                .child(t!(label)),
        )
        .child(div().flex_1().min_w_0().truncate().child(value))
}

fn reading(id: &'static str, cx: &App) -> AnyElement {
    h_flex()
        .id(id)
        .test_support()
        .gap_2()
        .py_8()
        .justify_center()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(Spinner::new().small())
        .child(t!("tools.reading"))
        .into_any_element()
}

fn message(id: &'static str, text: String, cx: &App) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .aria_label(text.clone())
        .py_8()
        .text_sm()
        .text_center()
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

impl Render for ServiceDetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("service-details")
            .test_support()
            .gap_3()
            .child(
                div().id("service-tabs").test_support().child(
                    TabBar::new("service-tab-bar")
                        .underline()
                        .small()
                        .selected_index(self.tab)
                        .on_click(cx.listener(|this, index: &usize, _, cx| this.select(*index, cx)))
                        .child(
                            Tab::new()
                                .icon(Icon::new(CatalogIcon::CircleDot))
                                .label(t!("services.details.tab_status")),
                        )
                        .child(
                            Tab::new()
                                .icon(Icon::new(CatalogIcon::FileText))
                                .label(t!("services.details.tab_journal")),
                        ),
                ),
            )
            .child(if self.tab == 0 {
                self.render_status(cx)
            } else {
                self.render_journal(cx)
            })
    }
}
