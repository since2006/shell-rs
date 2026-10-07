//! Files for the editor and for previews: an SFTP tab reads them and writes
//! the remote ones back over its own connection, so neither ever logs in
//! again.
//!
//! A file is read before any editor tab or preview exists: one that cannot
//! be shown (too large, binary, not UTF-8) is explained here and never
//! flashes a tab.

use super::preview::{IMAGE_LIMIT, Preview, PreviewContent, PreviewKind, open_preview_dialog};
use super::{ExplorerPanel, ExplorerPanelEvent, format_size};
use crate::app::{ExplorerAction, ExplorerCommand, ExplorerDispatch as _};
use crate::host::ConnectionState;
use crate::i18n::t;
use crate::sftp::{
    EDIT_LIMIT, FileBytes, FileStamp, ReadFailure, RemotePath, SaveFailure, SftpCommand, TextFile,
    read_local_bytes, read_local_text,
};
use futures::channel::oneshot;
use gpui_kit::component::{WindowExt as _, dialog::DialogButtonProps, notification::Notification};
use gpui_kit::*;
use std::path::PathBuf;
use std::rc::Rc;

/// Where a file to edit lives: on the server of an SFTP tab, or here.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FileLocation {
    Remote(RemotePath),
    Local(PathBuf),
}

impl FileLocation {
    pub fn is_remote(&self) -> bool {
        matches!(self, FileLocation::Remote(_))
    }
    /// The file's own name.
    pub fn name(&self) -> String {
        match self {
            FileLocation::Remote(path) => path.file_name().to_string(),
            FileLocation::Local(path) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        }
    }
    /// The full path, as a pane shows paths.
    pub fn path(&self) -> String {
        match self {
            FileLocation::Remote(path) => path.to_string(),
            FileLocation::Local(path) => path.to_string_lossy().into_owned(),
        }
    }
    /// The directory the file is in, as a pane shows paths.
    pub fn directory(&self) -> String {
        match self {
            FileLocation::Remote(path) => path.parent().to_string(),
            FileLocation::Local(path) => path
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

/// A read or a write waiting for the worker, answered from `on_event`.
pub(super) enum PendingFile {
    Read(oneshot::Sender<Result<TextFile, ReadFailure>>),
    Bytes(oneshot::Sender<Result<FileBytes, ReadFailure>>),
    Write(oneshot::Sender<Result<FileStamp, SaveFailure>>),
}

/// Why a file is read: for the editor, or for a preview of its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Opening {
    Edit,
    Preview(PreviewKind),
}

/// A file as read for an `Opening`: text, or an image's bytes.
enum Opened {
    Text(TextFile),
    Bytes(FileBytes),
}

impl ExplorerPanel {
    /// The file `path` names on one side, or the one under that side's
    /// cursor; `None` for a directory or nothing.
    pub fn edit_target(&self, remote: bool, path: Option<&str>, cx: &App) -> Option<FileLocation> {
        let pane = self.pane(remote).read(cx);
        let path = match path {
            Some(path) => path.to_string(),
            None => {
                let entry = pane.cursor_entry(cx)?;
                if entry.is_dir() || entry.is_parent() {
                    return None;
                }
                pane.child_path_of(&entry.name)
            }
        };
        Some(if remote {
            FileLocation::Remote(RemotePath::new(path).ok()?)
        } else {
            FileLocation::Local(PathBuf::from(path))
        })
    }

    /// The file `path` names on one side, or the one under the cursor,
    /// when it can be previewed.
    pub fn preview_target(
        &self,
        remote: bool,
        path: Option<&str>,
        cx: &App,
    ) -> Option<(FileLocation, PreviewKind)> {
        let location = self.edit_target(remote, path, cx)?;
        let kind = PreviewKind::of(&location.name())?;
        Some((location, kind))
    }

    /// Read a file for a new editor tab or a preview. `EditFile` brings one
    /// for the editor to the workspace; a preview opens here; a refusal is
    /// explained here.
    pub(super) fn open_file(
        &mut self,
        location: FileLocation,
        opening: Opening,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.opening.insert(location.clone()) {
            return;
        }
        let read = match opening {
            Opening::Preview(PreviewKind::Image(_)) => self
                .read_bytes(&location, IMAGE_LIMIT, window, cx)
                .map(|read| cx.spawn(async move |_, _| read.await.map(Opened::Bytes))),
            Opening::Edit | Opening::Preview(PreviewKind::Markdown) => self
                .read_file(&location, window, cx)
                .map(|read| cx.spawn(async move |_, _| read.await.map(Opened::Text))),
        };
        let Some(read) = read else {
            self.opening.remove(&location);
            return;
        };
        let remote = location.is_remote();
        let name = location.name();
        self.pane(remote)
            .clone()
            .update(cx, |pane, cx| pane.begin_opening(name, cx));
        cx.spawn_in(window, async move |this, cx| {
            let result = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.opening.remove(&location);
                this.pane(remote)
                    .clone()
                    .update(cx, |pane, cx| pane.finish_opening(cx));
                match (opening, result) {
                    (Opening::Edit, Ok(Opened::Text(file))) => cx.emit(
                        ExplorerPanelEvent::EditFile(this.id(), location, Rc::new(file)),
                    ),
                    (Opening::Preview(kind), Ok(opened)) => {
                        this.show_preview(&location, kind, opened, window, cx)
                    }
                    (Opening::Edit, Ok(Opened::Bytes(_))) => {}
                    (_, Err(failure)) => this.refuse_open(&location, opening, failure, window, cx),
                }
            });
        })
        .detach();
    }

    /// Read a whole text file. `None` when the remote side is not connected,
    /// which this tab has already told the user about.
    pub fn read_file(
        &mut self,
        location: &FileLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<TextFile, ReadFailure>>> {
        match location {
            FileLocation::Local(path) => {
                let (provider, path) = (self.local_provider.clone(), path.clone());
                Some(cx.background_spawn(async move { read_local_text(provider.as_ref(), &path) }))
            }
            FileLocation::Remote(path) => {
                if !self.reachable(window, cx) {
                    return None;
                }
                let (sender, receiver) = oneshot::channel();
                let request_id = self.next_file_request(PendingFile::Read(sender));
                self.send(SftpCommand::ReadFile {
                    request_id,
                    path: path.clone(),
                });
                Some(cx.spawn(async move |_, _| {
                    receiver.await.unwrap_or_else(|_| {
                        Err(ReadFailure::Failed(
                            t!("explorer.file.connection_lost").into(),
                        ))
                    })
                }))
            }
        }
    }

    /// Read a whole file as it is, up to `limit` bytes. `None` when the
    /// remote side is not connected, which this tab has already said.
    pub(super) fn read_bytes(
        &mut self,
        location: &FileLocation,
        limit: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<FileBytes, ReadFailure>>> {
        match location {
            FileLocation::Local(path) => {
                let (provider, path) = (self.local_provider.clone(), path.clone());
                Some(cx.background_spawn(async move {
                    read_local_bytes(provider.as_ref(), &path, limit)
                }))
            }
            FileLocation::Remote(path) => {
                if !self.reachable(window, cx) {
                    return None;
                }
                let (sender, receiver) = oneshot::channel();
                let request_id = self.next_file_request(PendingFile::Bytes(sender));
                self.send(SftpCommand::ReadBytes {
                    request_id,
                    path: path.clone(),
                    limit,
                });
                Some(cx.spawn(async move |_, _| {
                    receiver.await.unwrap_or_else(|_| {
                        Err(ReadFailure::Failed(
                            t!("explorer.file.connection_lost").into(),
                        ))
                    })
                }))
            }
        }
    }

    /// Write bytes over a remote file in place. `None` when not connected,
    /// which this tab has already told the user about.
    pub fn write_remote_file(
        &mut self,
        path: RemotePath,
        bytes: Vec<u8>,
        expected: Option<FileStamp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Task<Result<FileStamp, SaveFailure>>> {
        if !self.reachable(window, cx) {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        let request_id = self.next_file_request(PendingFile::Write(sender));
        self.send(SftpCommand::WriteFile {
            request_id,
            path,
            bytes,
            expected,
        });
        Some(cx.spawn(async move |_, _| {
            // The connection dropped before the answer: the write may have
            // stopped halfway.
            receiver.await.unwrap_or_else(|_| {
                Err(SaveFailure::Interrupted(
                    t!("explorer.file.connection_lost").into(),
                ))
            })
        }))
    }

    /// Re-read a pane that shows the directory `location` is in, so its
    /// size and time follow a save.
    pub fn reload_if_showing(
        &mut self,
        location: &FileLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = location.is_remote();
        if remote && self.connection_state() != ConnectionState::Connected {
            return;
        }
        if self.pane(remote).read(cx).path() == location.directory() {
            self.reload(remote, window, cx);
        }
    }

    pub(super) fn finish_file_read(&mut self, id: u64, result: Result<TextFile, ReadFailure>) {
        if let Some(PendingFile::Read(sender)) = self.files.remove(&id) {
            let _ = sender.send(result);
        }
    }

    pub(super) fn finish_bytes_read(&mut self, id: u64, result: Result<FileBytes, ReadFailure>) {
        if let Some(PendingFile::Bytes(sender)) = self.files.remove(&id) {
            let _ = sender.send(result);
        }
    }

    pub(super) fn finish_file_write(&mut self, id: u64, result: Result<FileStamp, SaveFailure>) {
        if let Some(PendingFile::Write(sender)) = self.files.remove(&id) {
            let _ = sender.send(result);
        }
    }

    /// The remote side is connected; otherwise say why and offer to
    /// reconnect.
    fn reachable(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.connection_state() {
            ConnectionState::Connected => true,
            ConnectionState::Disconnected => {
                self.offer_reconnect(window, cx);
                false
            }
            ConnectionState::Connecting => {
                window.push_notification(Notification::warning(t!("explorer.file.connecting")), cx);
                false
            }
        }
    }

    fn next_file_request(&mut self, pending: PendingFile) -> u64 {
        self.next_operation += 1;
        self.files.insert(self.next_operation, pending);
        self.next_operation
    }

    /// The preview dialog for a file just read.
    fn show_preview(
        &mut self,
        location: &FileLocation,
        kind: PreviewKind,
        opened: Opened,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = location.is_remote();
        let (content, size) = match (kind, opened) {
            (PreviewKind::Image(format), Opened::Bytes(bytes)) => {
                let size = bytes.len() as u64;
                let image = Image::from_bytes(format, bytes.into_inner());
                (PreviewContent::Image(image), size)
            }
            (PreviewKind::Markdown, Opened::Text(file)) => {
                let size = file.stamp().size();
                let (text, _, _) = file.into_parts();
                (PreviewContent::Markdown(text.into()), size)
            }
            _ => return,
        };
        let id = self.id();
        let download = self.download_action(location, cx);
        let edit = (kind == PreviewKind::Markdown).then(|| {
            ExplorerAction::new(
                id,
                ExplorerCommand::Edit {
                    remote,
                    path: Some(location.path()),
                },
            )
        });
        open_preview_dialog(
            Preview {
                name: location.name(),
                size,
                content,
                download: download.map(|command| ExplorerAction::new(id, command)),
                edit,
                dispatch: self.dispatch.clone(),
            },
            window,
            cx,
        );
    }

    /// 下载… for a remote file, into the local pane's directory.
    fn download_action(&self, location: &FileLocation, cx: &App) -> Option<ExplorerCommand> {
        match location {
            FileLocation::Remote(path) => Some(ExplorerCommand::DownloadPaths {
                paths: vec![path.to_string()],
                target: self.local().read(cx).path(),
            }),
            FileLocation::Local(_) => None,
        }
    }

    /// Why a file was not opened. Too large, binary or not UTF-8 is a
    /// decision to make (a remote file can still be downloaded); anything
    /// else is an error.
    fn refuse_open(
        &mut self,
        location: &FileLocation,
        opening: Opening,
        failure: ReadFailure,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = location.name();
        let editing = opening == Opening::Edit;
        let (title, limit) = match opening {
            Opening::Edit => (t!("explorer.file.edit_refused", name = name), EDIT_LIMIT),
            Opening::Preview(PreviewKind::Image(_)) => (
                t!("explorer.file.preview_refused", name = name),
                IMAGE_LIMIT,
            ),
            Opening::Preview(PreviewKind::Markdown) => {
                (t!("explorer.file.preview_refused", name = name), EDIT_LIMIT)
            }
        };
        let reason = match &failure {
            ReadFailure::TooLarge(size) => {
                let (size, limit) = (format_size(*size), format_size(limit));
                if editing {
                    t!(
                        "explorer.file.too_large_for_editor",
                        size = size,
                        limit = limit
                    )
                } else {
                    t!(
                        "explorer.file.too_large_for_preview",
                        size = size,
                        limit = limit
                    )
                }
            }
            ReadFailure::NotText => t!("explorer.file.not_text"),
            ReadFailure::NotFile => t!("explorer.file.not_file"),
            ReadFailure::Failed(message) => {
                window.push_notification(
                    Notification::error(message.clone())
                        .title(t!("explorer.file.open_failed", name = name)),
                    cx,
                );
                return;
            }
        };
        let download = self.download_action(location, cx);
        let description = if download.is_some() {
            t!("explorer.file.download_instead", reason = reason)
        } else {
            reason
        };
        let (dispatch, id) = (self.dispatch.clone(), self.id());
        window.open_alert_dialog(cx, move |alert, _, _| {
            let alert = alert.title(title.clone()).description(description.clone());
            match download.clone() {
                Some(download) => alert
                    .button_props(
                        DialogButtonProps::default()
                            .ok_text(t!("explorer.file.download"))
                            .cancel_text(t!("common.cancel")),
                    )
                    .show_cancel(true)
                    .on_ok({
                        let dispatch = dispatch.clone();
                        move |_, window, cx| {
                            dispatch.dispatch_explorer_action(
                                &ExplorerAction::new(id, download.clone()),
                                window,
                                cx,
                            );
                            true
                        }
                    }),
                None => alert.button_props(DialogButtonProps::default().ok_text(t!("explorer.ok"))),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::FileLocation;
    use crate::sftp::RemotePath;
    use std::path::PathBuf;

    #[test]
    fn a_location_names_its_file_and_directory_as_a_pane_does() {
        let remote = FileLocation::Remote(RemotePath::new("/etc/nginx/nginx.conf").unwrap());
        assert_eq!(remote.name(), "nginx.conf");
        assert_eq!(remote.directory(), "/etc/nginx");
        let top = FileLocation::Remote(RemotePath::new("/hosts").unwrap());
        assert_eq!(top.directory(), "/");
        let local = FileLocation::Local(PathBuf::from("/Users/me/notes.md"));
        assert_eq!(local.name(), "notes.md");
        assert_eq!(local.directory(), "/Users/me");
        assert_eq!(local.path(), "/Users/me/notes.md");
    }
}
