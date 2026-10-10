//! One-shot remote file operations: delete, rename, create, change
//! permissions. Trees are walked with an explicit stack and symbolic links
//! are never followed.

use super::{EntryKind, RemoteOperation, RemotePath, client::RemoteFs};
use crate::i18n::t;
use anyhow::{Result, bail};

pub(crate) async fn run<F: RemoteFs>(fs: &F, operation: &RemoteOperation) -> Result<()> {
    match operation {
        RemoteOperation::Delete { paths } => {
            for path in paths {
                delete_tree(fs, path).await?;
            }
            Ok(())
        }
        RemoteOperation::Rename { from, to } => {
            if fs.metadata(to).await?.is_some() {
                bail!(t!("sftp.error.exists", name = to.file_name()));
            }
            fs.rename(from, to).await
        }
        RemoteOperation::CreateDirectory { path } => {
            if fs.metadata(path).await?.is_some() {
                bail!(t!("sftp.error.exists", name = path.file_name()));
            }
            fs.mkdir(path).await
        }
        RemoteOperation::CreateFile { path } => {
            let handle = fs.open(path, true).await?;
            fs.close(&handle).await
        }
        RemoteOperation::SetPermissions {
            paths,
            edit,
            recursive,
            add_x_to_dirs,
        } => {
            // Parents first, so a change that grants access lets the walk in.
            let mut stack: Vec<RemotePath> = paths.iter().rev().cloned().collect();
            while let Some(path) = stack.pop() {
                let Some(metadata) = fs.metadata(&path).await? else {
                    continue;
                };
                let is_dir = metadata.kind() == EntryKind::Directory;
                if metadata.kind() == EntryKind::Symlink {
                    continue;
                }
                if let Some(mode) = metadata.permissions() {
                    let next = edit.apply(mode, is_dir, *add_x_to_dirs);
                    if next != mode {
                        fs.set_permissions(&path, next & 0o7777).await?;
                    }
                }
                if *recursive && is_dir {
                    let mut children = fs.read_dir(&path).await?;
                    children.sort_by(|a, b| b.name().cmp(a.name()));
                    for child in children {
                        stack.push(path.join(child.name())?);
                    }
                }
            }
            Ok(())
        }
    }
}

/// Delete a path and, for a directory, everything under it: children before
/// their directory. A link is removed as a link.
pub(super) async fn delete_tree<F: RemoteFs>(fs: &F, root: &RemotePath) -> Result<()> {
    let mut stack = vec![(root.clone(), false)];
    while let Some((path, emptied)) = stack.pop() {
        if emptied {
            fs.rmdir(&path).await?;
            continue;
        }
        match fs.metadata(&path).await? {
            None => {}
            Some(metadata) if metadata.kind() == EntryKind::Directory => {
                stack.push((path.clone(), true));
                for child in fs.read_dir(&path).await? {
                    stack.push((path.join(child.name())?, false));
                }
            }
            Some(_) => fs.remove(&path).await?,
        }
    }
    Ok(())
}
