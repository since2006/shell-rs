//! The update dialog: how far the newer ShellRS's download is, and
//! restarting into it. What it changes is not listed here: a button opens
//! the website's changelog.
//!
//! Opened from the title bar button and from 设置 › 关于. What a restart
//! interrupts is written here, above the button, so restarting needs no
//! second dialog on top of this one.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::DialogFooter,
    progress::Progress,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{CatalogIcon, DownloadUpdate, OpenChangelog, OpenDownloadPage, RestartToUpdate};

use super::status::{RestartImpact, UpdateStep, percent, restart_note};
use super::updater::{Phase, Stage, UpdateSnapshot, Updater};

/// The dialog's width, in rems.
const DIALOG_WIDTH: f32 = 32.;

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
        let versions = match snapshot
            .release
            .as_ref()
            .and_then(|release| release.published_on())
        {
            Some(day) => format!("当前版本 {} · 新版本发布于 {day}", snapshot.current),
            None => format!("当前版本 {}", snapshot.current),
        };
        v_flex()
            .id("update-dialog")
            .test_support()
            .gap_3()
            .text_sm()
            .child(
                v_flex()
                    .gap_1()
                    .when(snapshot.phase == Phase::Ready, |lines| {
                        lines.child(
                            div()
                                .id("update-ready")
                                .test_support()
                                .child("新版本已下载，重启 ShellRS 即可完成更新。"),
                        )
                    })
                    .child(div().text_color(muted).child(versions)),
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

/// Where to read what changed, and the one step that applies now. The
/// dialog closes by its close button or Escape.
fn footer(snapshot: &UpdateSnapshot, dispatch: FocusHandle) -> DialogFooter {
    let action: Option<(&'static str, &'static str, Box<dyn Action>)> =
        snapshot.step().map(|step| -> (_, _, Box<dyn Action>) {
            match step {
                UpdateStep::Restart => {
                    ("restart-to-update", "重启并安装", Box::new(RestartToUpdate))
                }
                UpdateStep::DownloadPage => (
                    "open-download-page",
                    "前往下载页",
                    Box::new(OpenDownloadPage),
                ),
                UpdateStep::Download { retry } => (
                    "download-update",
                    if retry { "重试" } else { "下载" },
                    Box::new(DownloadUpdate),
                ),
            }
        });
    let changelog = dispatch.clone();
    DialogFooter::new()
        .child(
            Button::new("open-changelog")
                .icon(CatalogIcon::ExternalLink)
                .label("查看更新内容")
                .on_click(move |_, window, cx| {
                    changelog.dispatch_action(&OpenChangelog, window, cx);
                }),
        )
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
