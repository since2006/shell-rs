//! What the Docker tool's details dialogs share: reading off the host, and
//! drawing what was read as sections under headings.

use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, spinner::Spinner, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::model::{DetailSection, SectionBody};
use crate::i18n::t;
use crate::terminal::{ExecResult, TerminalView, exec_answer};

/// Something read off the host for a dialog.
pub(super) enum Read<T> {
    /// Not asked for yet: a container's output until its tab is opened.
    NotYet,
    Reading,
    Known(T),
    /// Why not: 「读取失败：命令超时」.
    Unknown(String),
}

/// Run `command` on `terminal`'s connection, and hand the answer to
/// `record`: what it printed, or why it did not run.
pub(super) fn run<V: 'static>(
    terminal: &WeakEntity<TerminalView>,
    command: Option<String>,
    cx: &mut Context<V>,
    record: impl FnOnce(&mut V, Result<String, String>) + 'static,
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
            record(this, answer);
            cx.notify();
        })
        .ok();
    })
}

/// The sections one under another, each under its heading; 「—」 for one
/// with nothing in it. Their lines are found as 「{prefix}-{section}:{label}」
/// (「container-basics:name」), a text section as 「{prefix}-{section}」.
pub(super) fn render_sections(
    prefix: &'static str,
    sections: &[DetailSection],
    cx: &App,
) -> impl IntoElement {
    v_flex().gap_4().children(sections.iter().map(|section| {
        v_flex()
            .gap_1p5()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!(section.title)),
            )
            .child(render_body(prefix, section, cx))
    }))
}

fn render_body(prefix: &'static str, section: &DetailSection, cx: &App) -> AnyElement {
    if section.body.is_empty() {
        return div()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("—")
            .into_any_element();
    }
    let line_id = |key: &str| SharedString::from(format!("{prefix}-{}:{key}", section.id));
    let lines = |lines: Vec<AnyElement>| {
        v_flex()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .children(lines.into_iter().enumerate().map(|(index, line)| {
                div()
                    .when(index > 0, |line| {
                        line.border_t_1().border_color(cx.theme().border)
                    })
                    .child(line)
            }))
            .into_any_element()
    };
    match &section.body {
        SectionBody::Rows(rows) => lines(
            rows.iter()
                .map(|(label, value)| {
                    h_flex()
                        .id(line_id(label.id()))
                        .test_support()
                        .aria_label(value.clone())
                        .items_start()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .child(
                            div()
                                .w(rems(9.))
                                .flex_shrink_0()
                                .text_color(cx.theme().muted_foreground)
                                .child(label.text()),
                        )
                        // Long IDs and names wrap rather than go off the edge.
                        .child(div().flex_1().min_w_0().child(value.clone()))
                        .into_any_element()
                })
                .collect(),
        ),
        SectionBody::List(items) => lines(
            items
                .iter()
                .map(|item| {
                    div()
                        .id(line_id(item))
                        .test_support()
                        .aria_label(item.clone())
                        .px_3()
                        .py_2()
                        .text_sm()
                        .child(item.clone())
                        .into_any_element()
                })
                .collect(),
        ),
        SectionBody::Text(text) => {
            let text = text.join("\n");
            div()
                .id(SharedString::from(format!("{prefix}-{}", section.id)))
                .test_support()
                .aria_label(text.clone())
                .max_h(rems(12.))
                .overflow_y_scroll()
                .p_3()
                .rounded(cx.theme().radius)
                .bg(cx.theme().muted)
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .child(text)
                .into_any_element()
        }
    }
}

pub(super) fn reading(id: &'static str, cx: &App) -> AnyElement {
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

pub(super) fn message(id: &'static str, text: String, cx: &App) -> AnyElement {
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
