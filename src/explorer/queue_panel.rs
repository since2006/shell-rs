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
use crate::i18n::t;
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
                    tip: SharedString,
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
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(if unfinished > 0 {
                                t!("explorer.queue.title_count", unfinished = unfinished)
                            } else {
                                t!("explorer.queue.title")
                            }),
                    )
                    .child(div().flex_1())
                    .child(tool(
                        "resume-transfer",
                        Icon::new(CatalogIcon::Play),
                        t!("explorer.queue.resume"),
                        stopped,
                        ExplorerCommand::ResumeTransfer,
                    ))
                    .child(tool(
                        "cancel-transfer",
                        Icon::new(CatalogIcon::Square),
                        t!("explorer.queue.stop"),
                        head == Some(QueueState::Active),
                        ExplorerCommand::CancelTransfer,
                    ))
                    .child(tool(
                        "remove-transfer",
                        Icon::new(IconName::Close),
                        t!("explorer.queue.remove"),
                        queue.removable().is_some(),
                        ExplorerCommand::RemoveQueueEntry,
                    ))
                    .child(tool(
                        "discard-transfer",
                        Icon::new(CatalogIcon::Trash),
                        t!("explorer.queue.discard"),
                        stopped,
                        ExplorerCommand::DiscardTransfer,
                    ))
                    .child(tool(
                        "clear-finished-transfers",
                        Icon::new(CatalogIcon::Eraser),
                        t!("explorer.queue.clear_finished"),
                        queue.has_finished(),
                        ExplorerCommand::ClearFinishedTransfers,
                    )),
            )
            // The sidebar's batches stack their parts on lines of their own,
            // with no columns to title.
            .when(!self.is_sidebar(), |queue| {
                queue.child(
                    h_flex()
                        .px_2()
                        .py_0p5()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .bg(theme.muted)
                        .child(fixed(OPERATION).child(t!("explorer.queue.column.operation")))
                        .child(flexible().child(t!("explorer.queue.column.source")))
                        .child(flexible().child(t!("explorer.queue.column.target")))
                        .child(
                            fixed(TRANSFERRED)
                                .justify_end()
                                .child(t!("explorer.queue.column.transferred")),
                        )
                        .child(
                            fixed(TIME)
                                .justify_end()
                                .child(t!("explorer.queue.column.time")),
                        )
                        .child(
                            fixed(SPEED)
                                .justify_end()
                                .child(t!("explorer.queue.column.speed")),
                        )
                        .child(
                            fixed(PROGRESS)
                                .pl_3()
                                .child(t!("explorer.queue.column.progress")),
                        ),
                )
            })
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
                        let row = Row {
                            is_head,
                            selected: queue.is_selected(id),
                            expanded,
                        };
                        let clicks = (
                            on(ExplorerCommand::SelectQueueEntry { id: id.0 }),
                            on(ExplorerCommand::ToggleQueueEntry { id: id.0 }),
                        );
                        let compact = self.is_sidebar();
                        v_flex()
                            .map(|this| {
                                if compact {
                                    this.child(compact_batch_row(entry, row, clicks, cx))
                                } else {
                                    this.child(batch_row(entry, row, size_format, clicks, cx))
                                        .children(file_row(entry, size_format, cx))
                                }
                            })
                            .when(expanded, |this| {
                                this.child(item_results(entry, compact, cx))
                            })
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
                            t!("explorer.queue.hide_results")
                        } else {
                            t!("explorer.queue.show_results")
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

/// A batch in the right sidebar's narrow queue: what it copies, where to,
/// where it stands and the file in flight, each on a line of its own
/// rather than in columns off the edge.
fn compact_batch_row(
    entry: &QueueEntry,
    row: Row,
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
    let status_id: ElementId = if row.is_head {
        "transfer-status".into()
    } else {
        ("queue-status", id).into()
    };
    let current = progress
        .filter(|progress| state == QueueState::Active && !progress.current_source().is_empty())
        .map(|progress| progress.current_source().to_string());
    v_flex()
        .id(("queue-entry", id))
        .on_click(select)
        .test_support()
        .aria_selected(row.selected)
        .aria_label(status.clone())
        .px_2()
        .py_1()
        .gap_0p5()
        .min_w_0()
        .text_xs()
        .when(state.is_finished(), |this| {
            this.text_color(theme.muted_foreground)
        })
        .when(row.selected, |this| this.bg(theme.list_active))
        .when(!row.selected, |this| {
            this.hover(|style| style.bg(theme.list_hover))
        })
        .child(
            h_flex()
                .gap_1()
                .min_w_0()
                .text_sm()
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
                            t!("explorer.queue.hide_results")
                        } else {
                            t!("explorer.queue.show_results")
                        })
                        .on_click(toggle),
                )
                .child(
                    Icon::new(direction)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(flexible().child(entry.job().source_label())),
        )
        .child(
            flexible()
                .text_color(theme.muted_foreground)
                .child(entry.job().target().to_string()),
        )
        .child(
            v_flex()
                .id(status_id)
                .test_support()
                .aria_label(status.clone())
                .gap_0p5()
                .min_w_0()
                .when(transferring, |this| {
                    this.child(
                        Progress::new(("queue-bar", id))
                            .value(progress.map_or(0.0, |progress| progress.fraction()) * 100.)
                            .small(),
                    )
                })
                // A long reason wraps rather than going off the edge.
                .child(div().min_w_0().child(status)),
        )
        .when_some(current, |this, current| {
            this.child(
                flexible()
                    .id(("queue-file", id))
                    .test_support()
                    .aria_label(current.clone())
                    .text_color(theme.muted_foreground)
                    .child(current),
            )
        })
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
fn item_results(
    entry: &QueueEntry,
    compact: bool,
    cx: &Context<ExplorerPanel>,
) -> impl IntoElement {
    let theme = cx.theme();
    let (muted, success, danger) = (theme.muted_foreground, theme.success, theme.danger);
    let details = entry
        .progress()
        .map(|progress| progress.details())
        .unwrap_or_default();
    let upload = entry.job().direction() == TransferDirection::Upload;
    v_flex()
        .id(("transfer-detail-list", entry.id().0))
        .test_support()
        .when(!compact, |this| this.pl(px(OPERATION)))
        .min_w_0()
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
                        t!("explorer.queue.not_started")
                    } else if upload {
                        t!("explorer.queue.nothing_uploaded")
                    } else {
                        t!("explorer.queue.nothing_downloaded")
                    }),
            )
        })
        .children(details.iter().rev().map(|detail| {
            let (icon, color, outcome) = match detail.outcome() {
                TransferOutcome::Done => (
                    Icon::new(IconName::CircleCheck),
                    success,
                    t!("explorer.queue.outcome.done"),
                ),
                TransferOutcome::Skipped => (
                    Icon::new(CatalogIcon::CircleMinus),
                    muted,
                    t!("explorer.queue.outcome.skipped"),
                ),
                TransferOutcome::Failed => (
                    Icon::new(IconName::CircleX),
                    danger,
                    t!("explorer.queue.outcome.failed"),
                ),
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
                            this.child(
                                div()
                                    .text_color(danger)
                                    .child(crate::sftp::describe_reason(reason)),
                            )
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
