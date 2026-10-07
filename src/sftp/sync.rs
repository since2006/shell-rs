//! What the external CLI's `sync --delete` removes before it copies:
//! whatever is in the remote folder and not in the local one, or is there
//! as another kind (a folder where the local one has a file, a link where
//! it has a folder). Links are removed as links, never followed.

use std::{collections::HashMap, path::Path};

use anyhow::Result;

use crate::i18n::t;

use super::{
    EntryKind, RemotePath, TransferChoice, TransferDetail, TransferQuestionKind,
    client::{RemoteFs, is_network_error},
    control::{Cancelled, TransferControl},
    model::local_metadata,
    operations::delete_tree,
};

/// What the pruning did: how many entries it removed (a folder with all
/// in it counts once), and what it could not remove.
#[derive(Debug, Default)]
pub(super) struct Pruned {
    pub deleted: usize,
    pub failures: Vec<TransferDetail>,
}

/// Remove from the folder `remote` what the local folder `local` does not
/// have, all the way down. A folder that cannot be read here is left alone
/// there: not being able to see what it holds says nothing about what
/// belongs in its copy. What a transfer leaves behind while it runs (a
/// `.filepart`, a `.shellrs-…backup`) stays too: it may be another
/// transfer's, or the only copy of a file being replaced.
pub(super) async fn prune<F: RemoteFs>(
    fs: &F,
    local: &Path,
    remote: &RemotePath,
    control: &TransferControl,
) -> Result<Pruned> {
    let mut pruned = Pruned::default();
    let mut stack = vec![(local.to_path_buf(), remote.clone())];
    while let Some((local, remote)) = stack.pop() {
        control.check()?;
        let Ok(here) = local_kinds(&local).await else {
            continue;
        };
        let there = match control.run(fs.read_dir(&remote)).await {
            Ok(there) => there,
            Err(error) if gives_up(&error) => return Err(error),
            Err(error) => {
                fail(&mut pruned, &remote, &error, control).await?;
                continue;
            }
        };
        for entry in there {
            let name = entry.name();
            let path = remote.join(name)?;
            let kind = entry.metadata().kind();
            match here.get(name) {
                Some(Some(local_kind)) if *local_kind == kind => {
                    if kind == EntryKind::Directory {
                        stack.push((local.join(name), path));
                    }
                }
                // There, but what it is cannot be told: left alone.
                Some(None) => {}
                None if is_transfer_leftover(name) => {}
                _ => loop {
                    match control.run(delete_tree(fs, &path)).await {
                        Ok(()) => {
                            pruned.deleted += 1;
                            break;
                        }
                        Err(error) if gives_up(&error) => return Err(error),
                        Err(error) => {
                            if !fail(&mut pruned, &path, &error, control).await? {
                                break;
                            }
                        }
                    }
                },
            }
        }
    }
    Ok(pruned)
}

/// A failure that ends the sync rather than one entry: cancelled, or the
/// connection gone.
fn gives_up(error: &anyhow::Error) -> bool {
    error.is::<Cancelled>() || is_network_error(error)
}

/// Say what could not be done, as an upload says why a file failed;
/// whether to try again.
async fn fail(
    pruned: &mut Pruned,
    path: &RemotePath,
    error: &anyhow::Error,
    control: &TransferControl,
) -> Result<bool> {
    let message = t!("sftp.sync.delete_failed", error = format!("{error:#}"));
    let answer = control
        .ask(TransferQuestionKind::Error, path.as_str(), &message)
        .await?;
    if answer.choice() == TransferChoice::Retry {
        return Ok(true);
    }
    pruned.failures.push(TransferDetail::failed(
        path.to_string(),
        format!("{error:#}"),
    ));
    Ok(false)
}

/// What a local folder holds, by name, links not followed; `None` for an
/// entry whose kind cannot be read.
async fn local_kinds(dir: &Path) -> std::io::Result<HashMap<String, Option<EntryKind>>> {
    let mut kinds = HashMap::new();
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        // A name that is not UTF-8 cannot be copied either; what is there
        // under some name is left alone rather than taken for missing.
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let kind = tokio::fs::symlink_metadata(entry.path())
            .await
            .ok()
            .map(|metadata| local_metadata(&metadata).kind());
        kinds.insert(name, kind);
    }
    Ok(kinds)
}

/// The files an upload writes beside its target while it runs.
fn is_transfer_leftover(name: &str) -> bool {
    name.ends_with(".filepart") || (name.starts_with(".shellrs-") && name.ends_with(".backup"))
}
