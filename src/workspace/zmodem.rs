//! ZMODEM in a terminal: the files or the folder the user chooses when
//! `rz` or `sz` starts on the other end, and files dropped on a remote
//! terminal.

use std::path::PathBuf;

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use super::Workspace;
use crate::analytics::Counter;
use crate::app::PaneContent;
use crate::i18n::t;
use crate::terminal::{RemoteTerminalId, ZmodemDirection, ZmodemReply};

impl Workspace {
    /// `rz` or `sz` started in the terminal of `tab`: the system's picker
    /// for the files to upload, or the folder to download into. Choosing
    /// nothing cancels the transfer.
    pub(super) fn ask_zmodem(
        &mut self,
        tab: PaneContent,
        direction: ZmodemDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = match direction {
            ZmodemDirection::Upload => PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some(t!("workspace.zmodem.choose_files")),
            },
            ZmodemDirection::Download => PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: Some(t!("workspace.zmodem.choose_folder")),
            },
        };
        let chosen = cx.prompt_for_paths(options);
        cx.spawn_in(window, async move |this, cx| {
            let chosen = chosen
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .unwrap_or_default();
            this.update(cx, |this, cx| {
                let reply = match (direction, chosen.first()) {
                    (_, None) => ZmodemReply::Declined,
                    (ZmodemDirection::Upload, Some(_)) => {
                        this.count(Counter::ZmodemUpload);
                        ZmodemReply::Files(chosen)
                    }
                    (ZmodemDirection::Download, Some(folder)) => {
                        this.count(Counter::ZmodemDownload);
                        ZmodemReply::Folder(folder.clone())
                    }
                };
                this.reply_zmodem(tab, reply, cx);
            })
            .ok();
        })
        .detach();
    }

    fn reply_zmodem(&self, tab: PaneContent, reply: ZmodemReply, cx: &App) {
        match tab {
            PaneContent::Terminal(id) => {
                if let Some(panel) = self.terminals.get(&id) {
                    panel.read(cx).zmodem_reply(reply, cx);
                }
            }
            PaneContent::LocalTerminal(id) => {
                if let Some(panel) = self.local_terminals.get(&id) {
                    panel.read(cx).zmodem_reply(reply, cx);
                }
            }
            _ => {}
        }
    }

    /// Files dropped on a remote terminal: uploaded by the `rz` typed for
    /// them. Folders are left out, which ZMODEM cannot send.
    pub(super) fn upload_dropped(
        &mut self,
        id: RemoteTerminalId,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.terminals.get(&id).cloned() else {
            return;
        };
        let files: Vec<PathBuf> = paths
            .iter()
            .filter(|path| path.is_file())
            .cloned()
            .collect();
        let uploaded = if files.is_empty() {
            Err(t!("workspace.zmodem.files_only"))
        } else {
            panel.update(cx, |panel, cx| panel.upload_dropped(files, cx))
        };
        match uploaded {
            Ok(()) => self.count(Counter::ZmodemUpload),
            Err(message) => window.push_notification(Notification::warning(message), cx),
        }
    }
}
