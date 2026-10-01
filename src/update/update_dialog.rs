//! The update dialog: what the newer ShellRS brings, how far its download
//! is, and restarting into it.
//!
//! Opened from the title bar button and from 设置 › 关于. What a restart
//! interrupts is written here, above the button, so restarting needs no
//! second dialog on top of this one.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogClose, DialogFooter},
    progress::Progress,
    text::TextView,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{DownloadUpdate, OpenDownloadPage, RestartToUpdate};

use super::status::{RestartImpact, percent, restart_note};
use super::updater::{Phase, Stage, UpdateSnapshot, Updater};

/// The dialog's width, in rems.
const DIALOG_WIDTH: f32 = 32.;
/// The notes scroll beyond this height, in rems.
const NOTES_HEIGHT: f32 = 18.;

/// Counts what a restart would interrupt, at the time the dialog draws.
pub type ImpactCounter = Rc<dyn Fn(&App) -> RestartImpact>;

/// The dialog's body. It observes the updater so the progress moves while
/// the dialog is open.
pub struct UpdateDialog {
    updater: Entity<Updater>,
    impact: ImpactCounter,
    _observer: Subscription,
}

impl UpdateDialog {
    fn new(updater: Entity<Updater>, impact: ImpactCounter, cx: &mut Context<Self>) -> Self {
        Self {
            _observer: cx.observe(&updater, |_, _, cx| cx.notify()),
            updater,
            impact,
        }
    }
}

impl Render for UpdateDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.updater.read(cx).snapshot();
        let impact = (self.impact)(cx);
        let muted = cx.theme().muted_foreground;
        let release = snapshot.release.clone();
        v_flex()
            .id("update-dialog")
            .test_support()
            .gap_3()
            .text_sm()
            .child(
                div()
                    .text_color(muted)
                    .child(match (&release, &snapshot.manual) {
                        (Some(release), _) => match release.published_on() {
                            Some(day) => format!("当前版本 {} · 发布于 {day}", snapshot.current),
                            None => format!("当前版本 {}", snapshot.current),
                        },
                        _ => format!("当前版本 {}", snapshot.current),
                    }),
            )
            .children(
                release
                    .as_ref()
                    .filter(|release| !release.notes.trim().is_empty())
                    .map(|release| {
                        div()
                            .id("update-notes")
                            .test_support()
                            .aria_label(release.notes.clone())
                            .max_h(rems(NOTES_HEIGHT))
                            .overflow_y_scroll()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_3()
                            .child(
                                TextView::markdown(
                                    SharedString::from(format!("update-notes-{}", release.version)),
                                    release.notes.clone(),
                                )
                                .selectable(true),
                            )
                    }),
            )
            .when_some(progress(&snapshot), |body, (label, value)| {
                body.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .id("update-progress")
                                .test_support()
                                .aria_label(label.clone())
                                .text_color(muted)
                                .child(label),
                        )
                        .child(Progress::new("update-progress-bar").value(value).small()),
                )
            })
            .when_some(problem(&snapshot), |body, problem| {
                body.child(
                    div()
                        .id("update-problem")
                        .test_support()
                        .aria_label(problem.clone())
                        .text_color(cx.theme().danger)
                        .child(problem),
                )
            })
            .when_some(
                (snapshot.phase == Phase::Ready)
                    .then(|| restart_note(impact))
                    .flatten(),
                |body, note| {
                    body.child(
                        div()
                            .id("update-restart-note")
                            .test_support()
                            .aria_label(note.clone())
                            .text_color(cx.theme().warning)
                            .child(note),
                    )
                },
            )
    }
}

/// The progress line and bar while downloading or checking the package.
fn progress(snapshot: &UpdateSnapshot) -> Option<(String, f32)> {
    match snapshot.phase {
        Phase::Downloading { done, total } => {
            let percent = percent(done, total);
            Some((format!("正在下载 · {percent}%"), percent as f32))
        }
        Phase::Verifying => Some(("正在校验…".into(), 100.)),
        _ => None,
    }
}

/// Why the last step failed, or why this copy cannot install the update.
fn problem(snapshot: &UpdateSnapshot) -> Option<String> {
    match &snapshot.phase {
        Phase::Failed { stage, error } => Some(match stage {
            Stage::Check => format!("检查失败：{error}"),
            Stage::Download => format!("下载失败：{error}"),
            Stage::Install => format!("安装失败：{error}"),
        }),
        _ if snapshot.manual.is_some() => {
            Some("这个版本需要从官网下载安装，无法在 ShellRS 中直接更新。".into())
        }
        _ => snapshot.unsupported.as_ref().map(|reason| reason.reason()),
    }
}

/// Open the dialog for the version the updater has found. `dispatch` is
/// the workspace's focus handle, which handles the buttons' actions.
pub fn open_update_dialog(
    updater: Entity<Updater>,
    impact: ImpactCounter,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let body = cx.new(|cx| UpdateDialog::new(updater.clone(), impact, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let snapshot = updater.read(cx).snapshot();
        let title = match snapshot.offered_version() {
            Some(version) => format!("ShellRS {version}"),
            None => "ShellRS 更新".into(),
        };
        dialog
            .title(title)
            // Dialog geometry is an API boundary that takes `Pixels`; the
            // width follows the interface zoom through the rem.
            .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
            // Closed by its buttons or Escape, not by a click beside it.
            .overlay_closable(false)
            .child(body.clone())
            .footer(footer(&snapshot, dispatch.clone()))
    });
}

/// 稍后, and the one step that applies now.
fn footer(snapshot: &UpdateSnapshot, dispatch: FocusHandle) -> DialogFooter {
    let action: Option<(&'static str, &'static str, Box<dyn Action>)> =
        if snapshot.phase == Phase::Ready {
            Some(("restart-to-update", "重启更新", Box::new(RestartToUpdate)))
        } else if snapshot.needs_download_page() {
            Some((
                "open-download-page",
                "前往下载页",
                Box::new(OpenDownloadPage),
            ))
        } else if snapshot.can_download() {
            let label = if matches!(snapshot.phase, Phase::Failed { .. }) {
                "重试"
            } else {
                "下载"
            };
            Some(("download-update", label, Box::new(DownloadUpdate)))
        } else {
            None
        };
    let close = if action.is_some() { "稍后" } else { "关闭" };
    DialogFooter::new()
        .child(DialogClose::new().trigger(move |button| button.label(close)))
        .children(action.map(|(id, label, action)| {
            let dispatch = dispatch.clone();
            Button::new(id)
                .primary()
                .label(label)
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_action(action.as_ref(), window, cx);
                })
        }))
}
