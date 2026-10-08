//! The workspace's share of the external CLI: coming forward when ShellRS
//! is opened again, the changes to hosts and credentials a `shellrs`
//! command asks for, and the `exec --terminal` runs. The request threads
//! cannot reach the store or the terminals; the changes and the runs wait
//! in the server until they are taken up here.

use std::time::Duration;

use gpui_kit::*;

use crate::analytics::Counter;
use crate::cli::{
    CliChange, CliError, CliUse, CredentialDeleted, CredentialFields, ErrorCode, HostDeleted,
    HostFields, Reply,
};
use crate::cli::{
    CredentialPlan, GroupPlan, SecretChange, credential_details, credential_secrets,
    find_credential, find_host, find_saved_host, host_details, host_info, host_secrets,
    plan_credential, plan_host, with_saved_credential_secrets, with_saved_passwords,
};
use crate::host::{CredentialId, GroupDraft, GroupId, HostId, HostStore};
use crate::i18n::t;
use crate::secrets::{SecretRef, SharedSecretStore};
use crate::terminal::{RunFailure, RunHandle, TerminalLifecycle, TerminalPanel};

use super::workspace_view::Workspace;

/// How often the app looks for what the CLI server has heard.
const CLI_POLL: Duration = Duration::from_millis(100);

impl Workspace {
    /// Bring the window forward whenever ShellRS is opened while it is
    /// already running, and make the changes the CLI asks for. The second
    /// copy of ShellRS only passes the word on and exits: two of them on
    /// one data directory would each keep their own copy of the hosts and
    /// write over the other's changes.
    ///
    /// Asked on a timer, like every other worker: the thread that hears the
    /// request never wakes the window itself. A change is answered once
    /// made, the server's snapshot already showing it, and only then is the
    /// next one taken.
    pub(super) fn serve_cli(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CLI_POLL).await;
                let taken = this.update_in(cx, |this, window, cx| {
                    let server = this.cli_server.as_ref()?;
                    let links = server.take_activation();
                    let change = server.take_change();
                    for usage in server.take_usage() {
                        this.count(match usage {
                            CliUse::Exec => Counter::CliExec,
                            CliUse::ExecTerminal => Counter::CliExecTerminal,
                            CliUse::Upload => Counter::CliUpload,
                            CliUse::Download => Counter::CliDownload,
                            CliUse::Sync => Counter::CliSync,
                            CliUse::Hosts => Counter::CliHosts,
                            CliUse::Credentials => Counter::CliCredentials,
                        });
                    }
                    if let Some(links) = links {
                        crate::app::bring_forward(window, cx);
                        for link in links {
                            this.open_link(link, window, cx);
                        }
                    }
                    change.map(|(change, reply)| (this.apply_cli_change(change, window, cx), reply))
                });
                match taken {
                    Ok(Some((outcome, reply))) => reply.send(outcome.await),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        })
        .detach();
    }

    /// Hand each `exec --terminal` run the CLI server has heard to its
    /// host's terminal. Asked on a timer of its own: the poller above waits
    /// for each change to be made, and a run must not wait behind a
    /// keychain write.
    pub(super) fn serve_cli_runs(&self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CLI_POLL).await;
                let served = this.update(cx, |this, cx| {
                    let Some(server) = this.cli_server.as_ref() else {
                        return;
                    };
                    let runs: Vec<_> = std::iter::from_fn(|| server.take_run()).collect();
                    for (host, run) in runs {
                        if let Err(failure) = this.start_cli_run(&host, run.clone(), cx) {
                            run.fail(failure);
                        }
                    }
                });
                if served.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Type an `exec --terminal` run into the terminal of the host the CLI
    /// calls `host`: the one the right sidebar follows when it is this
    /// host's, else the host's newest connected one.
    pub fn start_cli_run(&self, host: &str, run: RunHandle, cx: &App) -> Result<(), RunFailure> {
        let host = find_host(self.store.read(cx), host)
            .map_err(|_| RunFailure::HostNotFound)?
            .id;
        let connected = |panel: &&Entity<TerminalPanel>| {
            matches!(panel.read(cx).lifecycle(cx), TerminalLifecycle::Running)
        };
        let panel = self
            .tool_terminal(cx)
            .filter(|terminal| terminal.host == host)
            .and_then(|terminal| self.terminals.get(&terminal.id))
            .filter(connected)
            .or_else(|| {
                self.terminals
                    .values()
                    .filter(|panel| panel.read(cx).host_id() == host)
                    .filter(connected)
                    .max_by_key(|panel| panel.read(cx).id().0)
            });
        match panel {
            Some(panel) => panel.read(cx).terminal().read(cx).run_command(run, cx),
            None if self.terminal(host, cx).is_some() => Err(RunFailure::NotConnected),
            None => Err(RunFailure::NoTerminal),
        }
    }

    /// Make a change a `shellrs` command asks for, checked as the host and
    /// credential forms check theirs. Its outcome is the reply the command
    /// prints; on success, the host or credential as it is now.
    pub fn apply_cli_change(
        &mut self,
        change: CliChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<Reply, CliError>> {
        let store = self.store.read(cx);
        match change {
            CliChange::CreateHost(fields) => self.save_host_from_cli(None, fields, cx),
            CliChange::UpdateHost { host, fields } => match find_saved_host(store, &host) {
                Ok(id) => self.save_host_from_cli(Some(id), fields, cx),
                Err(error) => Task::ready(Err(error)),
            },
            CliChange::DeleteHost { host, force } => {
                Task::ready(self.delete_host_from_cli(&host, force, window, cx))
            }
            CliChange::CreateCredential(fields) => self.save_credential_from_cli(None, fields, cx),
            CliChange::UpdateCredential { credential, fields } => {
                match find_credential(store, &credential) {
                    Ok(credential) => {
                        let id = credential.id;
                        self.save_credential_from_cli(Some(id), fields, cx)
                    }
                    Err(error) => Task::ready(Err(error)),
                }
            }
            CliChange::DeleteCredential { credential } => {
                Task::ready(self.delete_credential_from_cli(&credential, cx))
            }
        }
    }

    fn save_host_from_cli(
        &mut self,
        existing: Option<HostId>,
        fields: HostFields,
        cx: &mut Context<Self>,
    ) -> Task<Result<Reply, CliError>> {
        let plan = match plan_host(fields, existing, self.store.read(cx)) {
            Ok(plan) => plan,
            Err(error) => return Task::ready(Err(error)),
        };
        let store = self.store.clone();
        let secrets = store.read(cx).secrets();
        cx.spawn(async move |_, cx| {
            // Read what moves with the host before the change, after which
            // the store may delete the entry it moves from.
            let password = secret_value(plan.password, &secrets, cx).await;
            let proxy_password = secret_value(plan.proxy_password, &secrets, cx).await;
            let (id, mut failures, writes) = store.update(cx, |store, cx| {
                let (id, failures) = store.capture_failures(cx, |store, cx| {
                    let mut draft = plan.draft;
                    draft.group = place_group(store, plan.group, cx);
                    match existing {
                        Some(id) => {
                            store.update(id, draft, cx);
                            id
                        }
                        None => store.insert(draft, cx),
                    }
                });
                let writes: Vec<_> = [password, proxy_password]
                    .into_iter()
                    .flatten()
                    .map(|(secret, value)| store.write_secret(secret, value, cx))
                    .collect();
                (id, failures, writes)
            });
            for write in writes {
                if let Err(error) = write.await {
                    failures.push(error);
                }
            }
            if !failures.is_empty() {
                return Err(save_failed(failures));
            }
            let (details, refs) = store
                .read_with(cx, |store, _| {
                    store
                        .host(id)
                        .map(|host| (host_details(host, store), host_secrets(host, store)))
                })
                .ok_or_else(|| {
                    CliError::new(ErrorCode::HostNotFound, t!("cli.change.host_gone"))
                })?;
            let details = cx
                .background_executor()
                .spawn(async move {
                    with_saved_passwords(details, &refs, |secret| is_saved(&secrets, secret))
                })
                .await;
            Ok(Reply::Host(details))
        })
    }

    /// Delete a saved host as the host list does, closing its tabs; a
    /// host with tabs open only with `force`, since nobody is asked.
    fn delete_host_from_cli(
        &mut self,
        host: &str,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Reply, CliError> {
        let store = self.store.read(cx);
        let id = find_saved_host(store, host)?;
        let Some(host) = store.host(id) else {
            return Err(CliError::new(
                ErrorCode::HostNotFound,
                t!("cli.change.host_missing"),
            ));
        };
        let deleted = HostDeleted {
            host: host_details(host, store),
            forwards: store.forwards_of(id).count() as u64,
            jump_users: store.jump_users(&[id]) as u64,
        };
        if !force && self.has_tabs(id, cx) {
            return Err(CliError::new(
                ErrorCode::HostInUse,
                t!("cli.change.host_in_use", name = deleted.host.name),
            ));
        }
        self.close_host_tabs(id, window, cx);
        let (_, failures) = self.store.update(cx, |store, cx| {
            store.capture_failures(cx, |store, cx| store.remove(id, cx))
        });
        if !failures.is_empty() {
            return Err(save_failed(failures));
        }
        Ok(Reply::HostDeleted(deleted))
    }

    fn save_credential_from_cli(
        &mut self,
        existing: Option<CredentialId>,
        fields: CredentialFields,
        cx: &mut Context<Self>,
    ) -> Task<Result<Reply, CliError>> {
        let plan = match plan_credential(fields, existing, self.store.read(cx)) {
            Ok(plan) => plan,
            Err(error) => return Task::ready(Err(error)),
        };
        let store = self.store.clone();
        let secrets = store.read(cx).secrets();
        let saved = store.update(cx, |store, cx| save_credential(store, existing, plan, cx));
        let (id, mut failures, writes) = match saved {
            Ok(saved) => saved,
            Err(error) => return Task::ready(Err(error)),
        };
        cx.spawn(async move |_, cx| {
            for write in writes {
                if let Err(error) = write.await {
                    failures.push(error);
                }
            }
            if !failures.is_empty() {
                return Err(save_failed(failures));
            }
            let (details, refs) = store
                .read_with(cx, |store, _| {
                    store.credential(id).map(|credential| {
                        (
                            credential_details(credential, store),
                            credential_secrets(credential),
                        )
                    })
                })
                .ok_or_else(credential_gone)?;
            let details = cx
                .background_executor()
                .spawn(async move {
                    with_saved_credential_secrets(details, &refs, |secret| {
                        is_saved(&secrets, secret)
                    })
                })
                .await;
            Ok(Reply::Credential(details))
        })
    }

    /// Delete a credential as the credential list does; the hosts using it
    /// log in on their own from now on, and are listed in the reply.
    fn delete_credential_from_cli(
        &mut self,
        credential: &str,
        cx: &mut Context<Self>,
    ) -> Result<Reply, CliError> {
        let store = self.store.read(cx);
        let found = find_credential(store, credential)?;
        let (id, details) = (found.id, credential_details(found, store));
        let (released, failures) = self.store.update(cx, |store, cx| {
            store.capture_failures(cx, |store, cx| store.remove_credential(id, cx))
        });
        if !failures.is_empty() {
            return Err(save_failed(failures));
        }
        let store = self.store.read(cx);
        Ok(Reply::CredentialDeleted(CredentialDeleted {
            credential: details,
            released: released
                .iter()
                .filter_map(|id| store.host(*id))
                .map(|host| host_info(host, store))
                .collect(),
        }))
    }
}

/// The group a host goes into, made first when it is new.
fn place_group(
    store: &mut HostStore,
    plan: GroupPlan,
    cx: &mut Context<HostStore>,
) -> Option<GroupId> {
    match plan {
        GroupPlan::Existing(group) => group,
        GroupPlan::Create { parent, names } => names.into_iter().fold(parent, |parent, name| {
            Some(store.insert_group(GroupDraft::new(name, parent), cx))
        }),
    }
}

/// Write the key file, then the credential, then start writing its
/// secrets: the order the credential form keeps, so the credential never
/// names a key file that is not there.
fn save_credential(
    store: &mut HostStore,
    existing: Option<CredentialId>,
    plan: CredentialPlan,
    cx: &mut Context<HostStore>,
) -> Result<SavedCredential, CliError> {
    let mut draft = plan.draft;
    if let Some(pasted) = &plan.private_key {
        let path = store
            .save_private_key(existing, pasted.text())
            .map_err(|error| {
                CliError::new(
                    ErrorCode::SaveFailed,
                    t!("cli.change.key_not_saved", error = error),
                )
            })?;
        draft = draft.with_key_path(path);
    }
    let (id, failures) = store.capture_failures(cx, |store, cx| match existing {
        Some(id) => {
            store.update_credential(id, draft, cx);
            id
        }
        None => store.insert_credential(draft, cx),
    });
    let Some(credential) = store.credential(id).cloned() else {
        return Err(credential_gone());
    };
    let mut writes = Vec::new();
    if let Some(value) = plan.password.value() {
        writes.push(store.write_secret(credential.password_secret(), value, cx));
    }
    if let (Some(path), Some(value)) = (credential.key_path.as_deref(), plan.passphrase.value()) {
        writes.push(store.write_secret(SecretRef::passphrase(path), value, cx));
    }
    Ok((id, failures, writes))
}

type SavedCredential = (
    CredentialId,
    Vec<SharedString>,
    Vec<Task<Result<(), SharedString>>>,
);

/// What to write to `secret`, `None` for nothing and `Some(None)` to
/// delete it: a value moving with the host is read from where it was.
async fn secret_value(
    change: Option<(SecretRef, SecretChange)>,
    secrets: &SharedSecretStore,
    cx: &mut AsyncApp,
) -> Option<(SecretRef, Option<String>)> {
    let (secret, change) = change?;
    let value = match change {
        SecretChange::Carry(from) => {
            let secrets = secrets.clone();
            let value = cx
                .background_executor()
                .spawn(async move { secrets.get(&from).ok().flatten() })
                .await?;
            Some(value.to_string())
        }
        change => change.value()?,
    };
    Some((secret, value))
}

fn is_saved(secrets: &SharedSecretStore, secret: &SecretRef) -> bool {
    secrets.get(secret).is_ok_and(|value| value.is_some())
}

fn credential_gone() -> CliError {
    CliError::new(
        ErrorCode::CredentialNotFound,
        t!("cli.change.credential_gone"),
    )
}

/// Every failure, one after the other.
fn save_failed(failures: Vec<SharedString>) -> CliError {
    let message = failures
        .into_iter()
        .reduce(|before, next| t!("cli.change.failures", before = before, next = next))
        .unwrap_or_default();
    CliError::new(ErrorCode::SaveFailed, message)
}
