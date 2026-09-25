//! The SFTP tab's transfer queue, laid out like WinSCP's: a toolbar, one
//! row per batch with the file in flight beneath the running one, and a
//! batch's item results when it is unfolded. The running batch's row and its
//! file row carry WinSCP's two progress bars, overall and current file.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    progress::Progress,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;

use super::{ExplorerPanel, FileSizeFormat, QueueEntry, QueueState, format_size, percent};
use crate::app::{CatalogIcon, ExplorerAction, ExplorerCommand, ExplorerDispatch as _};
use crate::sftp::{TransferDirection, TransferOutcome, TransferPhase};

// Column widths, which the layout takes in pixels.
const OPERATION: f32 = 56.;
const TRANSFERRED: f32 = 110.;
const TIME: f32 = 72.;
const SPEED: f32 = 90.;
const PROGRESS: f32 = 150.;

impl ExplorerPanel {
    pub(super) fn render_queue(&self, cx: &Context<Self>) -> impl IntoElement {
        let sid = self.id();
        let dispatch = self.dispatch.clone();
        let on = move |command: ExplorerCommand| {
            let dispatch = dispatch.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                dispatch.dispatch_explorer_action(
                    &ExplorerAction::new(sid, command.clone()),
                    window,
                    cx,
                )
            }
        };
        let theme = cx.theme();
        let queue = &self.queue;
        let head = queue.head().map(QueueEntry::state);
        let stopped = head == Some(QueueState::Stopped) && !self.engine_busy;
        let tool = |id: &'static str,
                    icon: Icon,
                    tip: &'static str,
                    enabled: bool,
                    command: ExplorerCommand| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon)
                .tooltip(tip)
                .disabled(!enabled)
                .on_click(on(command))
        };
        let unfinished = queue.unfinished_count();
        let size_format = cx
            .try_global::<FileSizeFormat>()
            .copied()
            .unwrap_or_default();

        v_flex()
            .id(("transfer-queue", sid.0))
            .test_support()
            .size_full()
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .child(div().text_sm().child(if unfinished > 0 {
                        format!("传输队列 ({unfinished})")
                    } else {
                        "传输队列".into()
                    }))
                    .child(div().flex_1())
                    .child(tool(
                        "resume-transfer",
                        Icon::new(CatalogIcon::Play),
                        "继续",
                        stopped,
                        ExplorerCommand::ResumeTransfer,
                    ))
                    .child(tool(
                        "cancel-transfer",
                        Icon::new(CatalogIcon::Square),
                        "停止",
                        head == Some(QueueState::Active),
                        ExplorerCommand::CancelTransfer,
                    ))
                    .child(tool(
                        "remove-transfer",
                        Icon::new(IconName::Close),
                        "移出队列",
                        queue.removable().is_some(),
                        ExplorerCommand::RemoveQueueEntry,
                    ))
                    .child(tool(
                        "discard-transfer",
                        Icon::new(CatalogIcon::Trash),
                        "丢弃续传进度",
                        stopped,
                        ExplorerCommand::DiscardTransfer,
                    ))
                    .child(tool(
                        "clear-finished-transfers",
                        Icon::new(CatalogIcon::Eraser),
                        "清除已完成",
                        queue.has_finished(),
                        ExplorerCommand::ClearFinishedTransfers,
                    )),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_0p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .bg(theme.muted)
                    .child(fixed(OPERATION).child("操作"))
                    .child(flexible().child("来源"))
                    .child(flexible().child("目标"))
                    .child(fixed(TRANSFERRED).justify_end().child("已传输"))
                    .child(fixed(TIME).justify_end().child("时间"))
                    .child(fixed(SPEED).justify_end().child("速度"))
                    .child(fixed(PROGRESS).pl_3().child("进度")),
            )
            .child(
                v_flex()
                    .id("transfer-queue-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(queue.entries().iter().map(|entry| {
                        let id = entry.id();
                        let is_head = queue.head().is_some_and(|head| head.id() == id);
                        let expanded = queue.is_expanded(id);
                        v_flex()
                            .child(batch_row(
                                entry,
                                Row {
                                    is_head,
                                    selected: queue.is_selected(id),
                                    expanded,
                                },
                                size_format,
                                (
                                    on(ExplorerCommand::SelectQueueEntry { id: id.0 }),
                                    on(ExplorerCommand::ToggleQueueEntry { id: id.0 }),
                                ),
                                cx,
                            ))
                            .children(file_row(entry, size_format, cx))
                            .when(expanded, |this| this.child(item_results(entry, cx)))
                    })),
            )
    }
}

/// How a batch's row stands in the list.
struct Row {
    /// The batch the engine has, whose status the tests wait on.
    is_head: bool,
    selected: bool,
    expanded: bool,
}

/// A batch: what it copies where, how much has gone, and where it stands.
/// `(select, toggle)`: clicking the row, and its unfold button.
fn batch_row(
    entry: &QueueEntry,
    row: Row,
    size_format: FileSizeFormat,
    (select, toggle): (
        impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
        impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ),
    cx: &Context<ExplorerPanel>,
) -> impl IntoElement {
    let theme = cx.theme();
    let id = entry.id().0;
    let state = entry.state();
    let progress = entry.progress();
    let transferring = state == QueueState::Active
        && progress.is_some_and(|progress| progress.phase() == TransferPhase::Transferring);
    let direction = match entry.job().direction() {
        TransferDirection::Upload => CatalogIcon::Upload,
        TransferDirection::Download => CatalogIcon::Download,
    };
    let status = entry.status();
    // What the tests wait on: the head's status, whatever it is.
    let status_id: ElementId = if row.is_head {
        "transfer-status".into()
    } else {
        ("queue-status", id).into()
    };
    h_flex()
        .id(("queue-entry", id))
        .on_click(select)
        .test_support()
        .aria_selected(row.selected)
        .aria_label(status.clone())
        .px_2()
        .py_0p5()
        .text_sm()
        .when(state.is_finished(), |this| {
            this.text_color(theme.muted_foreground)
        })
        .when(row.selected, |this| this.bg(theme.list_active))
        .when(!row.selected, |this| {
            this.hover(|style| style.bg(theme.list_hover))
        })
        .child(
            fixed(OPERATION)
                .gap_1()
                .child(
                    Button::new(("queue-expand", id))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(if row.expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        }))
                        .tooltip(if row.expanded {
                            "收起逐项结果"
                        } else {
                            "展开逐项结果"
                        })
                        .on_click(toggle),
                )
                .child(
                    Icon::new(direction)
                        .small()
                        .text_color(theme.muted_foreground),
                ),
        )
        .child(flexible().child(entry.job().source_label()))
        .child(flexible().child(entry.job().target().to_string()))
        .child(
            fixed(TRANSFERRED)
                .justify_end()
                .children(progress.map(|progress| size_format.format(progress.completed_bytes()))),
        )
        .child(
            fixed(TIME).justify_end().children(
                progress
                    .filter(|_| transferring)
                    .and_then(|progress| progress.remaining())
                    .map(clock),
            ),
        )
        .child(
            fixed(SPEED).justify_end().children(
                progress
                    .filter(|progress| transferring && progress.bytes_per_second() > 0)
                    .map(|progress| format!("{}/s", format_size(progress.bytes_per_second()))),
            ),
        )
        .child(fixed(PROGRESS).pl_3().child(bar_with_text(
            status_id,
            ("queue-bar", id),
            transferring.then(|| progress.map_or(0.0, |progress| progress.fraction())),
            status,
        )))
}

/// The file in flight, beneath its running batch, with its own bar.
fn file_row(
    entry: &QueueEntry,
    size_format: FileSizeFormat,
    cx: &Context<ExplorerPanel>,
) -> Option<impl IntoElement> {
    let progress = entry.progress()?;
    if entry.state() != QueueState::Active || progress.current_source().is_empty() {
        return None;
    }
    let id = entry.id().0;
    let fraction = progress.current_fraction();
    Some(
        h_flex()
            .id(("queue-file", id))
            .test_support()
            .aria_label(progress.current_source().to_string())
            .px_2()
            .py_0p5()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(fixed(OPERATION))
            .child(flexible().child(progress.current_source().to_string()))
            .child(flexible())
            .child(
                fixed(TRANSFERRED)
                    .justify_end()
                    .child(size_format.format(progress.current_bytes())),
            )
            .child(fixed(TIME))
            .child(fixed(SPEED))
            .child(fixed(PROGRESS).pl_3().child(bar_with_text(
                ("queue-file-status", id),
                ("queue-file-bar", id),
                Some(fraction),
                percent(fraction),
            ))),
    )
}

/// How each item of an unfolded batch ended, the latest on top. Items are
/// listed as they end, so until one does, it says so instead.
fn item_results(entry: &QueueEntry, cx: &Context<ExplorerPanel>) -> impl IntoElement {
    let theme = cx.theme();
    let (muted, success, danger) = (theme.muted_foreground, theme.success, theme.danger);
    let details = entry
        .progress()
        .map(|progress| progress.details())
        .unwrap_or_default();
    let verb = entry.job().direction().verb();
    v_flex()
        .id(("transfer-detail-list", entry.id().0))
        .test_support()
        .pl(px(OPERATION))
        .py_0p5()
        .text_xs()
        .when(details.is_empty(), |this| {
            this.child(
                div()
                    .id("transfer-detail-empty")
                    .test_support()
                    .px_2()
                    .py_0p5()
                    .text_color(muted)
                    .child(if entry.state() == QueueState::Pending {
                        "还没有开始".to_string()
                    } else {
                        format!("还没有{verb}完的项目，每完成一项会列在这里")
                    }),
            )
        })
        .children(details.iter().rev().map(|detail| {
            let (icon, color, outcome) = match detail.outcome() {
                TransferOutcome::Done => (Icon::new(IconName::CircleCheck), success, "完成"),
                TransferOutcome::Skipped => (Icon::new(CatalogIcon::CircleMinus), muted, "已跳过"),
                TransferOutcome::Failed => (Icon::new(IconName::CircleX), danger, "失败"),
            };
            h_flex()
                .id(ElementId::Name(
                    format!("transfer-detail:{}", detail.path()).into(),
                ))
                .test_support()
                .aria_label(format!("{outcome} {}", detail.path()))
                .items_start()
                .gap_2()
                .px_2()
                .py_0p5()
                .child(icon.xsmall().text_color(color))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(detail.path().to_string())
                        .when_some(detail.reason(), |this, reason| {
                            this.child(div().text_color(danger).child(reason.to_string()))
                        }),
                )
        }))
}

/// A progress bar, when there is one, and the words beside it.
fn bar_with_text(
    id: impl Into<ElementId>,
    bar: impl Into<ElementId>,
    fraction: Option<f32>,
    text: String,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .test_support()
        .aria_label(text.clone())
        .w_full()
        .gap_2()
        .when_some(fraction, |this, fraction| {
            this.child(
                div()
                    .flex_1()
                    .child(Progress::new(bar).value(fraction * 100.).small()),
            )
        })
        .child(div().flex_shrink_0().child(text))
}

fn fixed(width: f32) -> Div {
    h_flex().w(px(width)).flex_shrink_0()
}

fn flexible() -> Div {
    div().flex_1().min_w_0().truncate()
}

/// `0:00:03`, as WinSCP writes times.
fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::clock;

    #[test]
    fn times_read_as_hours_minutes_seconds() {
        assert_eq!(clock(Duration::from_secs(3)), "0:00:03");
        assert_eq!(clock(Duration::from_millis(3_725_900)), "1:02:05");
    }
}
