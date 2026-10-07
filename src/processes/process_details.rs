//! A process's details: the dialog a click on it in 进程管理 opens.

use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    h_flex,
    spinner::Spinner,
    tag::Tag,
    v_flex,
};
use gpui_kit::*;

use super::linux::{command_line, command_line_command};
use super::model::{ProcessDetails, format_started};
use crate::app::{CatalogIcon, EndProcess};
use crate::i18n::t;
use crate::shared::{format_bytes, format_duration, format_percent};
use crate::terminal::{TerminalView, exec_answer};

/// How wide the dialog is: two columns of a label and a value.
const DIALOG_WIDTH: f32 = 40.;
/// How long a copy button says it copied.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// Open the details of a process of the host of `terminal`, whose
/// connection reads its command line. `dispatch` is the workspace's focus
/// handle: the dialog's 结束进程 buttons close it and dispatch there.
pub fn open_process_dialog(
    details: ProcessDetails,
    terminal: WeakEntity<TerminalView>,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let view = cx.new(|cx| ProcessDetailsView::new(details, terminal, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let this = view.read(cx);
        let process = &this.details.process;
        let pid = process.pid;
        let title = v_flex()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().truncate().child(process.name.clone()))
                    .child(
                        Tag::secondary()
                            .small()
                            .rounded_full()
                            .child(process.state.label()),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "PID {pid} · {}",
                        process.user.as_deref().unwrap_or("—")
                    )),
            );
        let end = |id: &'static str, force: bool| {
            let dispatch = dispatch.clone();
            Button::new(id).on_click(move |_, window, cx| {
                // One dialog at a time: the confirmation takes its place.
                window.close_dialog(cx);
                dispatch.dispatch_action(&EndProcess { pid, force }, window, cx);
            })
        };
        let footer = DialogFooter::new()
            .justify_between()
            .child(copy_button(
                "process-copy-pid",
                t!("processes.details.copy_pid"),
                Some(pid.to_string()),
                this.copied == Some(Copied::Pid),
                Copied::Pid,
                &view,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        end("process-end", false)
                            .outline()
                            .icon(Icon::new(CatalogIcon::CircleStop))
                            .label(t!("processes.details.end")),
                    )
                    .child(
                        end("process-force-end", true)
                            .danger()
                            .icon(Icon::new(CatalogIcon::OctagonX))
                            .label(t!("processes.details.kill")),
                    ),
            );
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

/// Which copy button just copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Copied {
    Pid,
    Command,
}

/// The command line, read once the dialog opens: the list does not carry
/// it.
enum CommandLine {
    Reading,
    Known(String),
    /// Why there is none: 「读取失败：命令超时」.
    Unknown(String),
}

/// The dialog's body: the process as the list last read it, and its
/// command line.
pub struct ProcessDetailsView {
    details: ProcessDetails,
    command: CommandLine,
    copied: Option<Copied>,
    _reading: Task<()>,
    /// Puts a copy button's 已复制 back after a moment.
    copied_reset: Option<Task<()>>,
}

impl ProcessDetailsView {
    fn new(
        details: ProcessDetails,
        terminal: WeakEntity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let reply = terminal.upgrade().and_then(|view| {
            view.read(cx)
                .exec(command_line_command(details.process.pid), cx)
        });
        let reading = cx.spawn(async move |this, cx| {
            let command = match reply {
                None => CommandLine::Unknown(t!("tools.not_connected").into()),
                Some(reply) => match exec_answer(reply, cx).await {
                    None => CommandLine::Unknown(t!("tools.not_connected").into()),
                    Some(Err(error)) => {
                        CommandLine::Unknown(t!("tools.read_failed", error = error).into())
                    }
                    Some(Ok(output)) => match command_line(&output) {
                        Some(line) => CommandLine::Known(line),
                        None => CommandLine::Unknown(t!("processes.details.no_command").into()),
                    },
                },
            };
            this.update(cx, |this, cx| {
                this.command = command;
                cx.notify();
            })
            .ok();
        });
        Self {
            details,
            command: CommandLine::Reading,
            copied: None,
            _reading: reading,
            copied_reset: None,
        }
    }

    fn copy(&mut self, which: Copied, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(which);
        self.copied_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

/// 「复制命令」, which says 「已复制」 for a moment once it has.
fn copy_button(
    id: &'static str,
    label: SharedString,
    text: Option<String>,
    copied: bool,
    which: Copied,
    view: &Entity<ProcessDetailsView>,
) -> Button {
    let view = view.clone();
    Button::new(id)
        .outline()
        .icon(Icon::new(if copied {
            IconName::Check
        } else {
            IconName::Copy
        }))
        .label(if copied {
            t!("processes.details.copied")
        } else {
            label
        })
        .disabled(text.is_none())
        .on_click(move |_, _, cx| {
            if let Some(text) = text.clone() {
                view.update(cx, |view, cx| view.copy(which, text, cx));
            }
        })
}

impl Render for ProcessDetailsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let details = &self.details;
        let process = &details.process;
        let info = &process.info;
        // Each label by its text's key; its line is found by the key's
        // last part: 「process-field:parent」.
        let none = || t!("processes.details.none").to_string();
        let fields: [(&'static str, String); 16] = [
            ("processes.field.pid", process.pid.to_string()),
            (
                "processes.field.parent",
                details.parent.clone().unwrap_or_else(none),
            ),
            (
                "processes.field.user",
                process.user.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "processes.field.state",
                format!("{} ({})", process.state.label(), info.letters),
            ),
            ("processes.field.started", format_started(process.started)),
            ("processes.field.running", format_duration(process.running)),
            (
                "processes.field.terminal",
                info.terminal.clone().unwrap_or_else(none),
            ),
            ("processes.field.priority", info.priority.to_string()),
            ("processes.field.nice", info.nice.to_string()),
            (
                "processes.field.cpu",
                process.cpu.map_or("—".into(), format_percent),
            ),
            (
                "processes.field.cpu_time",
                format_duration(process.cpu_time),
            ),
            (
                "processes.field.memory",
                format!(
                    "{} ({})",
                    format_bytes(process.memory),
                    format_percent(process.memory_percent)
                ),
            ),
            (
                "processes.field.virtual_memory",
                format_bytes(info.virtual_memory),
            ),
            ("processes.field.threads", info.threads.to_string()),
            ("processes.field.children", details.children.to_string()),
            (
                "processes.field.descendants",
                details.descendants.to_string(),
            ),
        ];
        let heading = |text: SharedString| {
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(text)
        };
        let command_text = match &self.command {
            CommandLine::Known(line) => Some(line.clone()),
            _ => None,
        };
        let view = cx.entity();
        v_flex()
            .id("process-details")
            .test_support()
            .gap_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(heading(t!("processes.details.info")))
                    .child(
                        div()
                            .grid()
                            .grid_cols(2)
                            .gap_x_8()
                            .children(fields.into_iter().map(|(label, value)| {
                                let id = label.rsplit('.').next().unwrap_or(label);
                                h_flex()
                                    .id(SharedString::from(format!("process-field:{id}")))
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
                                            .w(rems(8.5))
                                            .flex_shrink_0()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!(label)),
                                    )
                                    .child(div().flex_1().min_w_0().truncate().child(value))
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(heading(t!("processes.details.command")))
                            .child(
                                copy_button(
                                    "process-copy-command",
                                    t!("processes.details.copy_command"),
                                    command_text,
                                    self.copied == Some(Copied::Command),
                                    Copied::Command,
                                    &view,
                                )
                                .ghost()
                                .small(),
                            ),
                    )
                    .child(match &self.command {
                        CommandLine::Reading => h_flex()
                            .id("process-command-reading")
                            .test_support()
                            .gap_2()
                            .p_3()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(Spinner::new().small())
                            .child(t!("tools.reading"))
                            .into_any_element(),
                        CommandLine::Known(line) => div()
                            .id("process-command")
                            .test_support()
                            .aria_label(line.clone())
                            .max_h(rems(10.))
                            .overflow_y_scroll()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().muted)
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .child(line.clone())
                            .into_any_element(),
                        CommandLine::Unknown(why) => div()
                            .id("process-command-unknown")
                            .test_support()
                            .aria_label(why.clone())
                            .p_3()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(why.clone())
                            .into_any_element(),
                    }),
            )
    }
}
