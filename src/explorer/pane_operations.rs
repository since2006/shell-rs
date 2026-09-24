//! Running a confirmed file operation on one pane: remote ones through the
//! SFTP worker, local ones on the background executor. Either way the pane
//! stays busy until the result arrives, then re-reads its directory.

use super::{ExplorerPanel, LoadIntent, NewEntryKind};
use crate::session::ConnectionState;
use crate::sftp::{
    LocalDirectoryProvider, PermissionEdit, RemoteOperation, RemotePath, SftpCommand,
};
use anyhow::Result;
use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;
use std::path::{Path, PathBuf};

/// A file operation the user confirmed, with names relative to the pane's
/// current directory.
#[derive(Clone, Debug)]
pub(super) enum PaneOperation {
    Delete(Vec<String>),
    Rename {
        from: String,
        to: String,
    },
    Create {
        kind: NewEntryKind,
        name: String,
    },
    Permissions {
        names: Vec<String>,
        edit: PermissionEdit,
        recursive: bool,
        add_x_to_dirs: bool,
    },
}

/// What to do when an operation answers.
pub(super) struct PendingOperation {
    remote: bool,
    failure: &'static str,
    select: Option<String>,
}

impl PaneOperation {
    fn failure_title(&self) -> &'static str {
        match self {
            PaneOperation::Delete(_) => "删除失败",
            PaneOperation::Rename { .. } => "重命名失败",
            PaneOperation::Create { .. } => "新建失败",
            PaneOperation::Permissions { .. } => "修改权限失败",
        }
    }

    /// The row to select after the directory is read again.
    fn result_name(&self) -> Option<String> {
        match self {
            PaneOperation::Rename { to, .. } => Some(to.clone()),
            PaneOperation::Create { name, .. } => Some(name.clone()),
            _ => None,
        }
    }

    fn to_remote(&self, directory: &str) -> Result<RemoteOperation> {
        let directory = RemotePath::new(directory)?;
        let paths = |names: &[String]| -> Result<Vec<RemotePath>> {
            names.iter().map(|name| directory.join(name)).collect()
        };
        Ok(match self {
            PaneOperation::Delete(names) => RemoteOperation::Delete {
                paths: paths(names)?,
            },
            PaneOperation::Rename { from, to } => RemoteOperation::Rename {
                from: directory.join(from)?,
                to: directory.join(to)?,
            },
            PaneOperation::Create {
                kind: NewEntryKind::Folder,
                name,
            } => RemoteOperation::CreateDirectory {
                path: directory.join(name)?,
            },
            PaneOperation::Create {
                kind: NewEntryKind::File,
                name,
            } => RemoteOperation::CreateFile {
                path: directory.join(name)?,
            },
            PaneOperation::Permissions {
                names,
                edit,
                recursive,
                add_x_to_dirs,
            } => RemoteOperation::SetPermissions {
                paths: paths(names)?,
                edit: *edit,
                recursive: *recursive,
                add_x_to_dirs: *add_x_to_dirs,
            },
        })
    }

    /// Blocking; runs on the background executor.
    fn run_local(&self, provider: &dyn LocalDirectoryProvider, directory: &Path) -> Result<()> {
        let paths = |names: &[String]| -> Vec<PathBuf> {
            names.iter().map(|name| directory.join(name)).collect()
        };
        match self {
            PaneOperation::Delete(names) => provider.trash(&paths(names)),
            PaneOperation::Rename { from, to } => {
                provider.rename(&directory.join(from), &directory.join(to))
            }
            PaneOperation::Create {
                kind: NewEntryKind::Folder,
                name,
            } => provider.create_dir(&directory.join(name)),
            PaneOperation::Create {
                kind: NewEntryKind::File,
                name,
            } => provider.create_file(&directory.join(name)),
            PaneOperation::Permissions {
                names,
                edit,
                recursive,
                add_x_to_dirs,
            } => provider.set_permissions(&paths(names), *edit, *recursive, *add_x_to_dirs),
        }
    }
}

impl ExplorerPanel {
    /// Whether a pane accepts file commands now: its side is reachable and
    /// no other operation of its is running.
    pub(super) fn can_modify(&self, remote: bool, cx: &App) -> bool {
        (!remote || self.connection_state() == ConnectionState::Connected)
            && !self.pane(remote).read(cx).is_busy()
    }

    pub(super) fn start_operation(
        &mut self,
        remote: bool,
        operation: PaneOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_modify(remote, cx) {
            return;
        }
        let pane = self.pane(remote).clone();
        let directory = pane.read(cx).path();
        self.next_operation += 1;
        let id = self.next_operation;
        self.operations.insert(
            id,
            PendingOperation {
                remote,
                failure: operation.failure_title(),
                select: operation.result_name(),
            },
        );
        pane.update(cx, |pane, cx| pane.set_busy(true, cx));
        if remote {
            match operation.to_remote(&directory) {
                Ok(operation) => self.send(SftpCommand::Operate {
                    request_id: id,
                    operation,
                }),
                Err(error) => self.finish_operation(id, Err(format!("{error:#}")), window, cx),
            }
        } else {
            let provider = self.local_provider.clone();
            cx.spawn_in(window, async move |this, cx| {
                let result = cx
                    .background_spawn(async move {
                        operation
                            .run_local(provider.as_ref(), Path::new(&directory))
                            .map_err(|error| format!("{error:#}"))
                    })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.finish_operation(id, result, window, cx)
                });
            })
            .detach();
        }
    }

    pub(super) fn finish_operation(
        &mut self,
        id: u64,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.operations.remove(&id) else {
            return;
        };
        let pane = self.pane(pending.remote).clone();
        pane.update(cx, |pane, cx| {
            pane.set_busy(false, cx);
            if let (Ok(()), Some(name)) = (&result, pending.select) {
                pane.select_after_load(name);
            }
        });
        if let Err(message) = result {
            window.push_notification(Notification::error(message).title(pending.failure), cx);
        }
        // Re-read even after a failure: a delete may have removed part of a tree.
        if !pending.remote || self.connection_state() == ConnectionState::Connected {
            let path = pane.read(cx).path();
            self.navigate(pending.remote, path, LoadIntent::Reload, window, cx);
        }
    }

    /// The connection dropped: remote operations will not answer.
    pub(super) fn abandon_remote_operations(&mut self, cx: &mut Context<Self>) {
        self.operations.retain(|_, pending| !pending.remote);
        self.remote.update(cx, |pane, cx| pane.set_busy(false, cx));
    }
}
