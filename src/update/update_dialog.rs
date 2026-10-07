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
use crate::i18n::t;

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
            Some(day) => t!(
                "update.dialog.versions_released",
                current = snapshot.current,
                day = day
            ),
            None => t!("update.dialog.current_version", current = snapshot.current),
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
                                .child(t!("update.dialog.ready")),
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
fn progress(snapshot: &UpdateSnapshot) -> Option<(SharedString, f32)> {
    match snapshot.phase {
        Phase::Downloading { done, total } => {
            let percent = percent(done, total);
            Some((
                t!("update.dialog.downloading", percent = percent),
                percent as f32,
            ))
        }
        Phase::Verifying => Some((t!("update.dialog.verifying"), 100.)),
        _ => None,
    }
}

/// Why the last step failed, or why this copy cannot install the update.
fn problem(snapshot: &UpdateSnapshot) -> Option<SharedString> {
    match &snapshot.phase {
        Phase::Failed { stage, error } => Some(match stage {
            Stage::Check => t!("update.dialog.check_failed", error = error),
            Stage::Download => t!("update.dialog.download_failed", error = error),
            Stage::Install => t!("update.dialog.install_failed", error = error),
        }),
        _ if snapshot.manual.is_some() => Some(t!("update.dialog.manual")),
        _ => snapshot
            .unsupported
            .as_ref()
            .map(|reason| reason.reason().into()),
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
            Some(version) => SharedString::from(format!("ShellRS {version}")),
            None => t!("update.dialog.title"),
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
    let action: Option<(&'static str, SharedString, Box<dyn Action>)> =
        snapshot.step().map(|step| -> (_, _, Box<dyn Action>) {
            match step {
                UpdateStep::Restart => (
                    "restart-to-update",
                    t!("update.dialog.restart"),
                    Box::new(RestartToUpdate),
                ),
                UpdateStep::DownloadPage => (
                    "open-download-page",
                    t!("update.dialog.download_page"),
                    Box::new(OpenDownloadPage),
                ),
                UpdateStep::Download { retry } => (
                    "download-update",
                    if retry {
                        t!("update.dialog.retry")
                    } else {
                        t!("update.dialog.download")
                    },
                    Box::new(DownloadUpdate),
                ),
            }
        });
    let changelog = dispatch.clone();
    DialogFooter::new()
        .child(
            Button::new("open-changelog")
                .icon(CatalogIcon::ExternalLink)
                .label(t!("update.dialog.whats_new"))
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
