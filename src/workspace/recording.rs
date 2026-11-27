//! 录制当前终端 (issue #121): the file a recording of the active terminal
//! goes into, chosen in the system's save panel, and what became of it.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use super::Workspace;
use crate::app::{ExportTerminal, RecordTerminal, StopRecordingTerminal};
use crate::i18n::t;
use crate::terminal::{export_file_name, export_text_bytes, recording_file_name};

/// How often the window asks whether a recording ended.
const ENDED_POLL: Duration = Duration::from_millis(200);

impl Workspace {
    /// Ask where the active terminal's recording goes, offered as
    /// `ShellRS-<date>-<time>.log` in the folder the last one went into
    /// (Downloads at first), and start it there.
    pub(super) fn on_record_terminal(
        &mut self,
        _: &RecordTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.active_terminal(cx) else {
            return;
        };
        if terminal.read(cx).is_recording(cx) {
            return;
        }
        let folder = self
            .recording_folder
            .clone()
            .unwrap_or_else(|| self.local_directory_provider.downloads());
        let name = recording_file_name(chrono::Local::now().naive_local());
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        let terminal = terminal.downgrade();
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = chosen.await.ok().and_then(Result::ok).flatten() else {
                return;
            };
            // A network folder may take its time: not on the window's
            // thread.
            let created = cx
                .background_spawn({
                    let path = path.clone();
                    async move { File::create(path) }
                })
                .await;
            let started = this
                .update_in(cx, |this, window, cx| {
                    this.recording_folder = path.parent().map(Path::to_path_buf);
                    let started = match created {
                        Ok(file) => match terminal.update(cx, |terminal, cx| {
                            terminal.start_recording(path.clone(), file, cx)
                        }) {
                            Ok(started) => started,
                            // Its tab closed meanwhile: nothing to record.
                            Err(_) => {
                                let _ = std::fs::remove_file(&path);
                                return None;
                            }
                        },
                        Err(error) => Err(error),
                    };
                    match started {
                        Ok(started) => Some(started),
                        Err(error) => {
                            window.push_notification(
                                Notification::error(t!(
                                    "workspace.recording.not_started",
                                    path = shown(&path),
                                    error = error
                                )),
                                cx,
                            );
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            let Some((id, outcome)) = started else {
                return;
            };
            // Once it is all written: stopped, the tab closed, or the
            // file refused.
            let ended = loop {
                cx.background_executor().timer(ENDED_POLL).await;
                match outcome.try_recv() {
                    Ok(ended) => break ended,
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        break Err(io::ErrorKind::Interrupted.into());
                    }
                }
            };
            this.update_in(cx, |_, window, cx| {
                let notification = match ended {
                    Ok(()) => {
                        Notification::success(t!("workspace.recording.saved", path = shown(&path)))
                    }
                    Err(error) => {
                        _ = terminal.update(cx, |terminal, cx| terminal.recording_failed(id, cx));
                        Notification::error(t!(
                            "workspace.recording.stopped",
                            path = shown(&path),
                            error = error
                        ))
                    }
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn on_stop_recording_terminal(
        &mut self,
        _: &StopRecordingTerminal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = self.active_terminal(cx) {
            terminal.update(cx, |terminal, cx| terminal.stop_recording(cx));
        }
    }

    /// Export the active terminal's scrollback and current screen as plain text
    /// to a file chosen in the save panel, offered as
    /// `ShellRS-<date>-<time>.txt` in the folder the last recording or export
    /// went into (Downloads at first).
    pub(super) fn on_export_terminal(
        &mut self,
        _: &ExportTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.active_terminal(cx) else {
            return;
        };
        // Snapshot the terminal content immediately so the exported file
        // matches what the user saw when invoking Export.
        let text = terminal.read(cx).all_text(cx);
        let folder = self
            .recording_folder
            .clone()
            .unwrap_or_else(|| self.local_directory_provider.downloads());
        let name = export_file_name(chrono::Local::now().naive_local());
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = chosen.await.ok().and_then(Result::ok).flatten() else {
                return;
            };
            let bytes = export_text_bytes(&text);
            let written = cx
                .background_spawn({
                    let path = path.clone();
                    async move { std::fs::write(&path, bytes) }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.recording_folder = path.parent().map(Path::to_path_buf);
                let notification = match written {
                    Ok(()) => {
                        Notification::success(t!("workspace.export.saved", path = shown(&path)))
                    }
                    Err(error) => Notification::error(t!(
                        "workspace.export.failed",
                        path = shown(&path),
                        error = error
                    )),
                };
                window.push_notification(notification, cx);
            })
            .ok();
        })
        .detach();
    }
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}
