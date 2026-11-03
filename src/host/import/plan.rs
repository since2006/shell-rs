//! What an import adds: the rows the dialog lists, with their defaults,
//! checked as a whole before anything is written, then written in one go.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui_kit::{Context, SharedString};

use crate::i18n::t;

use super::super::{
    AuthKind, CredentialDraft, CredentialId, CredentialKind, DEFAULT_USER, ErrorMessage as _,
    GroupPathError, HostDraft, HostDraftError, HostStore, read_small,
};
use super::{ImportCandidate, ImportScan};

/// How an imported host logs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAuth {
    NoPassword,
    /// A password, asked for at each connection.
    Password,
    /// A saved credential.
    Credential(CredentialId),
    /// A key credential made by the import for the row's identity file.
    NewKey,
}

/// One host the import dialog lists: what the source gave and what the
/// user made of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRow {
    pub candidate: ImportCandidate,
    pub selected: bool,
    pub name: String,
    /// A group path such as `生产/华东`; empty for no group.
    pub group: String,
    pub user: String,
    pub auth: RowAuth,
    /// Whether a saved SSH host is already at this address and port. Such a
    /// row is shown but never imported.
    pub existing: bool,
}

impl ImportRow {
    pub fn will_import(&self) -> bool {
        self.selected && !self.existing
    }
}

/// The rows a scan gives, with their defaults: hosts already saved are
/// left out of the selection, and a host with a key file of its own logs
/// in with a key credential, else without a password.
pub fn import_rows(scan: &ImportScan, store: &HostStore) -> Vec<ImportRow> {
    scan.candidates
        .iter()
        .map(|candidate| {
            let existing = is_saved(store, &candidate.address, candidate.port);
            ImportRow {
                candidate: candidate.clone(),
                selected: !existing,
                name: candidate.name.clone(),
                group: String::new(),
                user: candidate.user.clone(),
                auth: default_auth(candidate, store),
                existing,
            }
        })
        .collect()
}

/// Bring rows up to date with a store that changed while the dialog was
/// open: a host saved meanwhile makes its row existing, and a credential
/// deleted meanwhile leaves its rows without a password.
pub fn refresh_rows(rows: &mut [ImportRow], store: &HostStore) {
    for row in rows {
        let existing = is_saved(store, &row.candidate.address, row.candidate.port);
        if existing && !row.existing {
            row.selected = false;
        }
        row.existing = existing;
        if let RowAuth::Credential(id) = row.auth
            && store.credential(id).is_none()
        {
            row.auth = RowAuth::NoPassword;
        }
    }
}

/// Whether a saved SSH host is at this address (any case) and port.
fn is_saved(store: &HostStore, address: &str, port: u16) -> bool {
    store.hosts().iter().any(|host| {
        host.is_ssh() && host.port == port && host.address.eq_ignore_ascii_case(address)
    })
}

fn default_auth(candidate: &ImportCandidate, store: &HostStore) -> RowAuth {
    match &candidate.identity {
        Some(path) => match saved_key(store, path, &candidate.user) {
            Some(id) => RowAuth::Credential(id),
            None => RowAuth::NewKey,
        },
        None => RowAuth::NoPassword,
    }
}

/// A saved key credential for this file and user: one naming the file,
/// or one keeping a copy of it, as importing it makes.
fn saved_key(store: &HostStore, path: &Path, user: &str) -> Option<CredentialId> {
    let same_key = |key: &str| {
        Path::new(key) == path
            || (store.keeps_key_file(key)
                && read_small(Path::new(key)).is_some_and(|kept| Some(kept) == read_small(path)))
    };
    store
        .credentials()
        .iter()
        .find(|credential| {
            credential.kind == CredentialKind::Key
                && credential.user.as_ref() == user
                && credential.key_path.as_deref().is_some_and(same_key)
        })
        .map(|credential| credential.id)
}

/// The user a row logs in as: an empty one is [`DEFAULT_USER`].
fn login_user(user: &str) -> String {
    match user.trim() {
        "" => DEFAULT_USER.to_string(),
        user => user.to_string(),
    }
}

/// Everything an import writes, checked.
#[derive(Debug)]
pub struct ImportPlan {
    entries: Vec<Entry>,
    credentials: Vec<CredentialDraft>,
}

#[derive(Debug)]
struct Entry {
    draft: HostDraft,
    group: String,
    login: Login,
}

#[derive(Debug)]
enum Login {
    Own,
    Saved(CredentialId),
    /// The credential at this index of the plan's new ones.
    New(usize),
}

/// Why an import cannot go ahead, naming the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    Group { name: String, error: GroupPathError },
    Host { name: String, error: HostDraftError },
}

impl ImportError {
    pub fn message(&self) -> SharedString {
        match self {
            ImportError::Group {
                name,
                error: GroupPathError::EmptyLevel { path },
            } => t!(
                "host.import.error.group_empty_level",
                name = name,
                path = path
            ),
            ImportError::Group {
                name,
                error: GroupPathError::Ambiguous { count, path },
            } => t!(
                "host.import.error.group_ambiguous",
                name = name,
                count = count,
                path = path
            ),
            ImportError::Host { name, error } => {
                t!(
                    "host.import.error.host",
                    name = name,
                    error = error.message()
                )
            }
        }
    }
}

/// Check the rows to import and work out what to write: nothing is
/// written when any of them is wrong. A key file shared by rows logging in
/// as the same user gets one credential, an existing one when there is.
pub fn plan_import(rows: &[ImportRow], store: &HostStore) -> Result<ImportPlan, ImportError> {
    let mut entries = Vec::new();
    let mut credentials = Vec::new();
    let mut new_keys: HashMap<(PathBuf, String), usize> = HashMap::new();
    for row in rows.iter().filter(|row| row.will_import()) {
        let candidate = &row.candidate;
        if is_saved(store, &candidate.address, candidate.port) {
            continue;
        }
        let shown_name = match row.name.trim() {
            "" => candidate.address.clone(),
            name => name.to_string(),
        };
        store
            .resolve_group_path(&row.group)
            .map_err(|error| ImportError::Group {
                name: shown_name.clone(),
                error,
            })?;
        let user = login_user(&row.user);
        let (auth, login) = match row.auth {
            RowAuth::NoPassword => (AuthKind::NoPassword, Login::Own),
            RowAuth::Password => (AuthKind::Password, Login::Own),
            RowAuth::Credential(id) if store.credential(id).is_some() => {
                (AuthKind::default(), Login::Saved(id))
            }
            RowAuth::Credential(_) => (AuthKind::NoPassword, Login::Own),
            RowAuth::NewKey => match &candidate.identity {
                Some(path) => match saved_key(store, path, &user) {
                    Some(id) => (AuthKind::default(), Login::Saved(id)),
                    None => {
                        let index =
                            *new_keys
                                .entry((path.clone(), user.clone()))
                                .or_insert_with(|| {
                                    credentials.push(key_credential(path, &user));
                                    credentials.len() - 1
                                });
                        (AuthKind::default(), Login::New(index))
                    }
                },
                None => (AuthKind::NoPassword, Login::Own),
            },
        };
        let draft = HostDraft::new(
            row.name.trim().to_string(),
            candidate.address.clone(),
            candidate.port,
            user,
            auth,
            None,
        )
        .validated()
        .map_err(|error| ImportError::Host {
            name: shown_name,
            error,
        })?;
        entries.push(Entry {
            draft,
            group: row.group.clone(),
            login,
        });
    }
    Ok(ImportPlan {
        entries,
        credentials,
    })
}

/// A key credential for a file, named after it.
fn key_credential(path: &Path, user: &str) -> CredentialDraft {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    CredentialDraft::new(name, CredentialKind::Key, user.to_string())
        .with_key_path(path.to_string_lossy().into_owned())
}

/// What an import wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub hosts: usize,
    pub credentials: usize,
    /// The writes that did not reach the database, already reported as
    /// `PersistFailed`.
    pub failures: Vec<SharedString>,
}

/// Write a plan: the credentials, then each host in row order, making its
/// groups as they are first needed, all in one transaction.
pub fn apply_import(
    plan: ImportPlan,
    store: &mut HostStore,
    cx: &mut Context<HostStore>,
) -> Imported {
    let ImportPlan {
        entries,
        credentials,
    } = plan;
    let mut not_copied = Vec::new();
    // Each key file is copied among ShellRS's own keys, which is what
    // syncs; one that cannot be is used where it is.
    let credentials: Vec<CredentialDraft> = credentials
        .into_iter()
        .map(|draft| {
            let Some(path) = draft.key_path.clone() else {
                return draft;
            };
            match store.keep_key_file(None, &path) {
                Ok(Some(kept)) => draft.with_key_path(kept),
                Ok(None) => draft,
                Err(error) => {
                    not_copied.push(t!("host.import.key_not_copied", error = error.message()));
                    draft
                }
            }
        })
        .collect();
    let (credentials, mut failures) = store.capture_failures(cx, |store, cx| {
        store.in_one_transaction(cx, |store, cx| {
            let made: Vec<CredentialId> = credentials
                .into_iter()
                .map(|draft| store.insert_credential(draft, cx))
                .collect();
            for entry in &entries {
                let mut draft = entry.draft.clone();
                draft.group = match store.resolve_group_path(&entry.group) {
                    Ok(plan) => store.place_group(plan, cx),
                    Err(_) => None,
                };
                draft = match entry.login {
                    Login::Own => draft,
                    Login::Saved(id) => draft.with_credential(id),
                    Login::New(index) => draft.with_credential(made[index]),
                };
                store.insert(draft, cx);
            }
            made.len()
        })
    });
    failures.extend(not_copied);
    Imported {
        hosts: entries.len(),
        credentials,
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{GroupDraft, GroupPlan, HostDatabase};
    use super::*;

    fn candidate(name: &str, address: &str, identity: Option<&str>) -> ImportCandidate {
        ImportCandidate {
            name: name.into(),
            address: address.into(),
            port: 22,
            user: "deploy".into(),
            identity: identity.map(PathBuf::from),
        }
    }

    fn scan(candidates: Vec<ImportCandidate>) -> ImportScan {
        ImportScan {
            candidates,
            hashed: 0,
        }
    }

    #[test]
    fn rows_default_to_importing_what_is_new_the_way_it_logs_in() {
        let mut store = HostStore::empty();
        store.insert_unnotified(HostDraft::new(
            "old",
            "WEB.example.com",
            22,
            "someone",
            AuthKind::Password,
            None,
        ));
        let saved = store.insert_credential_unnotified(
            CredentialDraft::new("ops", CredentialKind::Key, "deploy").with_key_path("/k/ops"),
        );
        let rows = import_rows(
            &scan(vec![
                candidate("web", "web.example.com", None),
                candidate("app", "10.0.0.2", Some("/k/ops")),
                candidate("db", "10.0.0.3", Some("/k/db")),
                candidate("cache", "10.0.0.4", None),
            ]),
            &store,
        );
        let summary: Vec<(bool, bool, RowAuth)> = rows
            .iter()
            .map(|row| (row.existing, row.selected, row.auth))
            .collect();
        assert_eq!(
            summary,
            [
                (true, false, RowAuth::NoPassword),
                (false, true, RowAuth::Credential(saved)),
                (false, true, RowAuth::NewKey),
                (false, true, RowAuth::NoPassword),
            ]
        );
    }

    #[test]
    fn a_store_that_changed_meanwhile_updates_the_rows() {
        let mut store = HostStore::empty();
        let saved = store.insert_credential_unnotified(
            CredentialDraft::new("ops", CredentialKind::Key, "deploy").with_key_path("/k/ops"),
        );
        let mut rows = import_rows(
            &scan(vec![
                candidate("web", "10.0.0.1", None),
                candidate("app", "10.0.0.2", Some("/k/ops")),
            ]),
            &store,
        );
        store.insert_unnotified(HostDraft::new(
            "web",
            "10.0.0.1",
            22,
            "x",
            AuthKind::Password,
            None,
        ));
        store.remove_credential_unnotified(saved);
        refresh_rows(&mut rows, &store);
        assert!(rows[0].existing && !rows[0].selected);
        assert_eq!(rows[1].auth, RowAuth::NoPassword);
    }

    #[test]
    fn a_key_file_gets_one_credential_per_user_and_reuses_a_saved_one() {
        let mut store = HostStore::empty();
        let saved = store.insert_credential_unnotified(
            CredentialDraft::new("ops", CredentialKind::Key, "admin").with_key_path("/k/shared"),
        );
        let mut rows = import_rows(
            &scan(vec![
                candidate("a", "10.0.0.1", Some("/k/shared")),
                candidate("b", "10.0.0.2", Some("/k/shared")),
                candidate("c", "10.0.0.3", Some("/k/shared")),
                candidate("d", "10.0.0.4", Some("/k/shared")),
            ]),
            &store,
        );
        // Edited to the saved credential's user: that one is used.
        rows[2].user = " admin ".into();
        // Emptied: logs in as root, with a key of its own.
        rows[3].user = String::new();
        let plan = plan_import(&rows, &store).unwrap();
        assert_eq!(plan.credentials.len(), 2);
        assert_eq!(plan.credentials[0].user.as_ref(), "deploy");
        assert_eq!(plan.credentials[0].name.as_ref(), "shared");
        assert_eq!(plan.credentials[1].user.as_ref(), "root");
        let logins: Vec<String> = plan
            .entries
            .iter()
            .map(|entry| format!("{:?}", entry.login))
            .collect();
        assert_eq!(
            logins,
            [
                "New(0)",
                "New(0)",
                format!("Saved({saved:?})").as_str(),
                "New(1)"
            ]
        );
    }

    #[test]
    fn a_wrong_group_path_names_its_row_and_nothing_is_planned() {
        let mut store = HostStore::empty();
        store.insert_group_unnotified(GroupDraft::new("生产", None));
        store.insert_group_unnotified(GroupDraft::new("生产", None));
        let mut rows = import_rows(
            &scan(vec![
                candidate("web", "10.0.0.1", None),
                candidate("", "10.0.0.2", None),
            ]),
            &store,
        );
        rows[0].group = "a//b".into();
        let error = plan_import(&rows, &store).unwrap_err();
        assert_eq!(
            error,
            ImportError::Group {
                name: "web".into(),
                error: GroupPathError::EmptyLevel {
                    path: "a//b".into()
                }
            }
        );
        rows[0].group.clear();
        rows[1].group = "生产".into();
        let error = plan_import(&rows, &store).unwrap_err();
        assert!(matches!(
            &error,
            ImportError::Group { name, .. } if name == "10.0.0.2"
        ));
        assert!(error.message().contains("10.0.0.2"));
    }

    #[test]
    fn only_selected_new_rows_are_planned() {
        let store = HostStore::empty();
        let mut rows = import_rows(
            &scan(vec![
                candidate("web", "10.0.0.1", None),
                candidate("db", "10.0.0.2", None),
            ]),
            &store,
        );
        rows[1].selected = false;
        assert_eq!(plan_import(&rows, &store).unwrap().entries.len(), 1);
        rows[0].existing = true;
        assert_eq!(plan_import(&rows, &store).unwrap().entries.len(), 0);
    }

    #[gpui_kit::test]
    fn an_import_makes_each_new_group_once_and_its_credentials(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;

        let store = cx.new(|_| HostStore::load(HostDatabase::in_memory().unwrap()).unwrap());
        let imported = store.update(cx, |store, cx| {
            let production = store.insert_group(GroupDraft::new("生产", None), cx);
            let mut rows = import_rows(
                &scan(vec![
                    candidate("web", "10.0.0.1", Some("/k/deploy")),
                    candidate("", "10.0.0.2", Some("/k/deploy")),
                    candidate("db", "10.0.0.3", None),
                ]),
                store,
            );
            rows[0].group = "生产/华东".into();
            rows[1].group = " 生产 / 华东 ".into();
            rows[2].group = "开发".into();
            let plan = plan_import(&rows, store).unwrap();
            let imported = apply_import(plan, store, cx);
            assert_eq!(
                store
                    .resolve_group_path("生产/华东")
                    .map(|plan| match plan {
                        GroupPlan::Existing(Some(group)) => store.group(group).map(|g| g.parent),
                        _ => None,
                    }),
                Ok(Some(Some(production)))
            );
            imported
        });
        assert_eq!(
            imported,
            Imported {
                hosts: 3,
                credentials: 1,
                failures: Vec::new()
            }
        );
        store.read_with(cx, |store, _| {
            assert_eq!(store.groups().len(), 3);
            let hosts: Vec<(String, String, String)> = store
                .hosts()
                .iter()
                .map(|host| {
                    (
                        host.name.to_string(),
                        host.group
                            .map(|group| store.group_path(group))
                            .unwrap_or_default(),
                        format!("{:?}{:?}", host.auth, host.credential.is_some()),
                    )
                })
                .collect();
            assert_eq!(
                hosts,
                [
                    ("web".into(), "生产 / 华东".into(), "Passwordtrue".into()),
                    (
                        "10.0.0.2".into(),
                        "生产 / 华东".into(),
                        "Passwordtrue".into()
                    ),
                    ("db".into(), "开发".into(), "NoPasswordfalse".into()),
                ]
            );
            let credential = &store.credentials()[0];
            assert_eq!(credential.key_path.as_deref(), Some("/k/deploy"));
            assert_eq!(credential.user.as_ref(), "deploy");
        });
    }

    /// Imported keys are copied among ShellRS's own, which is what syncs,
    /// and importing again knows the copy for the file.
    #[gpui_kit::test]
    fn an_imported_key_is_copied_and_known_again(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;

        let data = tempfile::tempdir().unwrap();
        let key = data.path().join("id_deploy");
        std::fs::write(&key, "the key").unwrap();
        let identity = key.display().to_string();
        let store = cx.new(|_| HostStore::empty().with_key_dir(data.path().join("keys")));
        store.update(cx, |store, cx| {
            let rows = import_rows(
                &scan(vec![candidate("web", "10.0.0.1", Some(&identity))]),
                store,
            );
            let imported = apply_import(plan_import(&rows, store).unwrap(), store, cx);
            assert_eq!(imported.credentials, 1);
            let kept = store.credentials()[0].key_path.clone().unwrap();
            assert!(store.keeps_key_file(&kept), "{kept}");
            assert_eq!(std::fs::read_to_string(kept.as_ref()).unwrap(), "the key");

            let rows = import_rows(
                &scan(vec![candidate("api", "10.0.0.2", Some(&identity))]),
                store,
            );
            let imported = apply_import(plan_import(&rows, store).unwrap(), store, cx);
            assert_eq!(imported.credentials, 0, "the copy is the same key");
            assert_eq!(store.credentials().len(), 1);
        });
    }
}
