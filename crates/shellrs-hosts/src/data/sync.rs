//! What sync reads of the store and applies to it.
//!
//! Every record that syncs leaves as a `shellrs_sync::LocalRecord` with its
//! references turned into global ids (a host's `public_id`, a credential's
//! `keychain_id`, the other records' `sync_id`, a bookmark's id made from
//! its host and path) and comes back the same way. What only this device
//! keeps stays out, and a record applied keeps what it had of it: a host's
//! system and sidebar, a group's expansion, a forward's automatic start, a
//! key's file path, the local bookmarks. Serial hosts do not sync at all:
//! their port is this computer's.
//!
//! Secrets are read on a worker thread from where the export says they are
//! (`read_secrets`), and those a round brings are written there the same
//! way (`SecretWork`), never through the database. A keychain entry is read
//! only when its edit time is not the one the last round saw: the round has
//! the others already, and a keychain may ask the user for each entry it
//! hands out. Until one is written it
//! is pending, by its sync key in the database: a keychain that refused it
//! must not have it read back as deleted. A record applied keeps its edit
//! time; one that is new has 0.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};

use gpui_shared_string::SharedString;
use shellrs_sync::{
    self as sync, Kind, LocalRecord, LocalSnapshot, RecordData, RecordKey, SyncState,
};
use zeroize::Zeroizing;

use super::{BookmarkMeta, Failure, HostData, SecretError};
use crate::private_key::{read_small, write_key_file};
use crate::secrets::{SecretRef, SecretStore};
use crate::{
    AuthKind, BookmarkSide, Credential, CredentialDraft, CredentialId, CredentialKind,
    ForwardDraft, ForwardEndpoint, ForwardId, ForwardKind, ForwardRule, GroupDraft, GroupId, Host,
    HostDraft, HostGroup, HostId, Protocol, ProxyKind, ProxySettings, PublicId, Route, Snippet,
    SnippetCategory, SnippetCategoryId, SnippetDraft, SnippetId, SnippetScope, TelnetSettings,
};

/// What one round of sync takes from the store, on the UI thread.
#[derive(Clone)]
pub struct SyncExport {
    /// Every record that syncs, secrets aside.
    pub records: Vec<LocalRecord>,
    /// The secrets that sync, and where to read each.
    pub secrets: Vec<SecretSource>,
    /// The secrets a round brought that are not written yet.
    pub pending: Vec<RecordKey>,
    /// The store's `sync_revision` as the records were read: the round's
    /// result is applied only while it is still that.
    pub revision: u64,
}

impl SyncExport {
    /// What the round is handed, read on a worker thread: the records, the
    /// secrets from where they are (those changed since the round `state`
    /// records), and what the store does not hold as it should (a key file
    /// gone, a secret not written yet).
    pub fn local(&self, secrets: &dyn SecretStore, state: &SyncState) -> LocalSnapshot {
        let read = read_secrets(&self.secrets, &self.pending, state, secrets);
        let mut records = self.records.clone();
        records.extend(read.records);
        let mut unreadable = read.unreadable;
        unreadable.extend(self.pending.iter().cloned());
        LocalSnapshot {
            records,
            secrets_read: read.secrets_read,
            unreadable,
            unchanged: read.unchanged,
        }
    }
}

/// A secret that syncs, by its sync record, and where it is.
#[derive(Clone, Debug)]
pub struct SecretSource {
    pub key: RecordKey,
    pub edited_ms: u64,
    pub place: SecretPlace,
}

#[derive(Clone, Debug)]
pub enum SecretPlace {
    /// A keychain entry; for a password of an endpoint, its endpoint too.
    Keychain(SecretRef, Option<SecretEndpoint>),
    /// A key file ShellRS keeps.
    KeyFile(PathBuf),
}

/// Who signs in where with a password: what a password's record names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretEndpoint {
    pub user: String,
    pub host: String,
    pub port: u16,
}

/// The secrets of an export as read: the records to hand the round with
/// the others, and what could not be read.
pub struct SecretsRead {
    pub records: Vec<LocalRecord>,
    /// False when the keychain did not answer: then no secret is in
    /// `records`, and the round keeps them all as they were.
    pub secrets_read: bool,
    /// Key files that are not there or cannot be read.
    pub unreadable: Vec<RecordKey>,
    /// Keychain entries not read: unchanged since the last round saw them.
    pub unchanged: Vec<sync::Unchanged>,
}

/// Read the secrets of `sources`, on a worker thread: the keychain blocks
/// and may ask the user. An entry whose edit time is the one the round
/// `state` records saw is not read but named unchanged; one in `pending` is
/// not read at all, as it was never written.
pub fn read_secrets(
    sources: &[SecretSource],
    pending: &[RecordKey],
    state: &SyncState,
    secrets: &dyn SecretStore,
) -> SecretsRead {
    let unread = || SecretsRead {
        records: Vec::new(),
        secrets_read: false,
        unreadable: Vec::new(),
        unchanged: Vec::new(),
    };
    if !secrets.is_available() {
        return unread();
    }
    let mut records = Vec::new();
    let mut unreadable = Vec::new();
    let mut unchanged = Vec::new();
    for source in sources
        .iter()
        .filter(|source| !pending.contains(&source.key))
    {
        let value = match &source.place {
            SecretPlace::Keychain(..) if state.edit_seen(&source.key) == Some(source.edited_ms) => {
                unchanged.push(sync::Unchanged {
                    key: source.key.clone(),
                    edited_ms: source.edited_ms,
                });
                continue;
            }
            SecretPlace::Keychain(secret, _) => match secrets.get(secret) {
                Ok(value) => value,
                // A locked keychain looks like nothing saved, and nothing
                // saved would delete every secret everywhere.
                Err(_) => return unread(),
            },
            SecretPlace::KeyFile(path) => match read_small(path) {
                Some(text) => Some(text),
                None => {
                    unreadable.push(source.key.clone());
                    continue;
                }
            },
        };
        let Some(mut value) = value.filter(|value| !value.is_empty()) else {
            continue;
        };
        let value = sync::Secret::new(std::mem::take(&mut *value));
        let endpoint = |endpoint: &Option<SecretEndpoint>| {
            endpoint.as_ref().map(|endpoint| sync::EndpointSecret {
                user: endpoint.user.clone(),
                host: endpoint.host.clone(),
                port: endpoint.port,
                value: value.clone(),
            })
        };
        let data = match (&source.key.kind, &source.place) {
            (Kind::SecretPassword, SecretPlace::Keychain(_, at)) => {
                endpoint(at).map(RecordData::Password)
            }
            (Kind::SecretProxy, SecretPlace::Keychain(_, at)) => {
                endpoint(at).map(RecordData::ProxyPassword)
            }
            (Kind::SecretCredential, _) => Some(RecordData::CredentialPassword(value)),
            (Kind::SecretPassphrase, _) => Some(RecordData::Passphrase(value)),
            (Kind::SecretPrivateKey, _) => Some(RecordData::PrivateKey(value)),
            _ => None,
        };
        if let Some(data) = data {
            records.push(LocalRecord {
                id: source.key.id.clone(),
                data,
                edited_ms: source.edited_ms,
            });
        }
    }
    SecretsRead {
        records,
        secrets_read: true,
        unreadable,
        unchanged,
    }
}

/// Why a round's changes were not applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// The user changed something since the round read the records: run
    /// it again.
    Stale,
    /// The database refused them, in its words; nothing was applied.
    NotSaved(String),
}

/// The secrets a round brings, to write where they go on a worker thread,
/// each with its sync key.
#[derive(Default)]
pub struct SecretWork {
    keychain: Vec<(RecordKey, SecretRef, Option<Zeroizing<String>>)>,
    keys: Vec<KeyWrite>,
    /// Key files ShellRS kept that go, with their passphrases.
    forget: Vec<(RecordKey, PathBuf)>,
    /// Secrets with nowhere to go yet: a passphrase whose key has not come.
    waiting: Vec<RecordKey>,
}

/// A private key to write for a credential, with the passphrase to keep
/// under the file it lands in.
struct KeyWrite {
    credential: PublicId,
    replace: Option<PathBuf>,
    text: Zeroizing<String>,
    passphrase: Option<Zeroizing<String>>,
}

impl fmt::Debug for SecretWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretWork")
            .field("keychain", &self.keychain.len())
            .field("keys", &self.keys.len())
            .field("forget", &self.forget)
            .field("waiting", &self.waiting)
            .finish()
    }
}

/// What writing a round's secrets came to.
#[derive(Debug, Default)]
pub struct SecretsWritten {
    /// The key file each credential's private key went to.
    pub key_paths: Vec<(PublicId, SharedString)>,
    /// The secrets written, by their sync key: no longer pending.
    pub done: Vec<RecordKey>,
    /// What could not be kept; the next round brings it again.
    pub failures: Vec<Failure>,
}

impl SecretWork {
    pub fn is_empty(&self) -> bool {
        self.keychain.is_empty() && self.keys.is_empty() && self.forget.is_empty()
    }

    /// Every secret this work is for, those waiting included.
    fn pending(&self) -> BTreeSet<RecordKey> {
        let mut keys: BTreeSet<RecordKey> =
            self.keychain.iter().map(|(key, ..)| key.clone()).collect();
        for write in &self.keys {
            let id = write.credential.to_string();
            keys.insert(RecordKey::new(Kind::SecretPrivateKey, id.clone()));
            if write.passphrase.is_some() {
                keys.insert(RecordKey::new(Kind::SecretPassphrase, id));
            }
        }
        keys.extend(self.forget.iter().map(|(key, _)| key.clone()));
        keys.extend(self.waiting.iter().cloned());
        keys
    }

    /// Write it all, on a worker thread. Key files go to `key_dir`.
    pub fn run(self, secrets: &dyn SecretStore, key_dir: Option<&Path>) -> SecretsWritten {
        let mut written = SecretsWritten::default();
        let mut failures = Vec::new();
        let mut fail = |error: SecretError| failures.push(Failure::SyncedSecret(error));
        for (key, secret, value) in self.keychain {
            let result = match value {
                Some(value) => secrets.set(&secret, &value),
                None => secrets.delete(&secret),
            };
            match result {
                Ok(()) => written.done.push(key),
                Err(error) => fail(SecretError::of(&error)),
            }
        }
        for key in self.keys {
            let Some(dir) = key_dir else {
                fail(SecretError::NoKeyDir);
                continue;
            };
            let id = key.credential.to_string();
            match write_key_file(dir, key.replace.as_deref(), &key.text) {
                Ok(path) => {
                    written
                        .done
                        .push(RecordKey::new(Kind::SecretPrivateKey, id.clone()));
                    if let Some(passphrase) = key.passphrase {
                        match secrets.set(&SecretRef::passphrase(&path), &passphrase) {
                            Ok(()) => written
                                .done
                                .push(RecordKey::new(Kind::SecretPassphrase, id)),
                            Err(error) => fail(SecretError::of(&error)),
                        }
                    }
                    written
                        .key_paths
                        .push((key.credential, path.display().to_string().into()));
                }
                Err(error) => fail(SecretError::KeyFile(error.to_string())),
            }
        }
        for (key, path) in self.forget {
            let removed = match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    fail(SecretError::KeyFile(error.to_string()));
                    false
                }
                _ => true,
            };
            match secrets.delete(&SecretRef::passphrase(&path)) {
                Ok(()) if removed => written.done.push(key),
                Ok(()) => {}
                Err(error) => fail(SecretError::of(&error)),
            }
        }
        written.failures = failures;
        written
    }
}

/// A database write a round's changes need, done once memory has them all.
enum Write {
    Pending(RecordKey, bool),
    Group(GroupId, bool),
    DeleteGroup(GroupId),
    Credential(CredentialId, bool),
    DeleteCredential(CredentialId),
    Host(HostId, bool),
    DeleteHost(HostId),
    Bookmark(HostId, String),
    DeleteBookmark(HostId, String),
    Forward(ForwardId, bool),
    DeleteForward(ForwardId),
    Category(SnippetCategoryId, bool),
    DeleteCategory(SnippetCategoryId),
    Snippet(SnippetId, bool),
    DeleteSnippet(SnippetId),
}

/// Memory as it was before a round was applied, to go back to.
struct Saved {
    groups: Vec<HostGroup>,
    hosts: Vec<Host>,
    credentials: Vec<Credential>,
    forwards: Vec<ForwardRule>,
    snippets: Vec<Snippet>,
    snippet_categories: Vec<SnippetCategory>,
    bookmarks: HashMap<(HostId, BookmarkSide), Vec<String>>,
    bookmark_meta: HashMap<(HostId, BookmarkSide, String), BookmarkMeta>,
    recent: Vec<HostId>,
    sync_pending: BTreeSet<RecordKey>,
    next: [u64; 6],
}

/// Which internal record each global id is, the new ones included.
#[derive(Default)]
struct Ids {
    groups: HashMap<String, GroupId>,
    hosts: HashMap<String, HostId>,
    credentials: HashMap<String, CredentialId>,
    forwards: HashMap<String, ForwardId>,
    categories: HashMap<String, SnippetCategoryId>,
    snippets: HashMap<String, SnippetId>,
}

impl HostData {
    /// The records that sync and where their secrets are, for a round.
    pub fn sync_export(&self) -> SyncExport {
        let group_id = |id: GroupId| self.group(id).map(|group| group.sync_id.to_string());
        let host_id = |id: HostId| self.synced_host(id).map(|host| host.public_id.to_string());
        let credential_id =
            |id: CredentialId| self.credential(id).map(|c| c.keychain_id.to_string());
        let mut records = Vec::new();
        for group in &self.groups {
            records.push(LocalRecord {
                id: group.sync_id.to_string(),
                data: RecordData::Group(sync::GroupData {
                    name: group.name.to_string(),
                    parent: group.parent.and_then(group_id),
                    sort_order: group.sort_order,
                }),
                edited_ms: group.edited_ms,
            });
        }
        for credential in &self.credentials {
            records.push(LocalRecord {
                id: credential.keychain_id.to_string(),
                data: RecordData::Credential(sync::CredentialData {
                    name: credential.name.to_string(),
                    kind: sync_credential_kind(credential.kind),
                    username: credential.user.to_string(),
                    sort_order: credential.sort_order,
                }),
                edited_ms: credential.edited_ms,
            });
        }
        for host in self.hosts.iter().filter(|host| !host.is_serial()) {
            let protocol = match &host.protocol {
                Protocol::Telnet(telnet) => sync::Protocol::Telnet(sync::TelnetData {
                    user_prompt: some_text(&telnet.user_prompt),
                    pass_prompt: some_text(&telnet.password_prompt),
                }),
                _ => sync::Protocol::Ssh,
            };
            let (auth, username, credential) = match host.credential.and_then(credential_id) {
                Some(credential) => (sync::Auth::Credential, None, Some(credential)),
                None => (
                    match host.auth {
                        AuthKind::Password => sync::Auth::Password,
                        AuthKind::NoPassword => sync::Auth::NoPassword,
                    },
                    Some(host.user.to_string()),
                    None,
                ),
            };
            let route = match &host.route {
                Route::Direct => sync::Route::Direct,
                Route::Jump(hops) => {
                    sync::Route::Jump(hops.iter().map(|hop| hop.and_then(host_id)).collect())
                }
                Route::Proxy(proxy) => sync::Route::Proxy(sync::ProxyData {
                    kind: match proxy.kind {
                        ProxyKind::Http => sync::ProxyKind::Http,
                        ProxyKind::Socks5 => sync::ProxyKind::Socks5,
                    },
                    host: proxy.host.to_string(),
                    port: proxy.port,
                    username: proxy.user.as_ref().map(ToString::to_string),
                }),
            };
            records.push(LocalRecord {
                id: host.public_id.to_string(),
                data: RecordData::Host(sync::HostData {
                    name: host.name.to_string(),
                    address: host.address.to_string(),
                    port: host.port,
                    group: host.group.and_then(group_id),
                    sort_order: host.sort_order,
                    protocol,
                    auth,
                    username,
                    credential,
                    route,
                    notes: host.notes.to_string(),
                }),
                edited_ms: host.edited_ms,
            });
        }
        for ((host, side), paths) in &self.bookmarks {
            let Some(public_id) = host_id(*host).filter(|_| *side == BookmarkSide::Remote) else {
                continue;
            };
            for path in paths {
                let meta = self.bookmark_meta(*host, *side, path);
                records.push(LocalRecord {
                    id: sync::bookmark_id(&public_id, path),
                    data: RecordData::Bookmark(sync::BookmarkData {
                        host: public_id.clone(),
                        path: path.clone(),
                        sort_order: meta.sort_order,
                    }),
                    edited_ms: meta.edited_ms,
                });
            }
        }
        for rule in &self.forwards {
            let Some(host) = host_id(rule.host) else {
                continue;
            };
            records.push(LocalRecord {
                id: rule.sync_id.to_string(),
                data: RecordData::Forward(sync::ForwardData {
                    host,
                    name: rule.name.to_string(),
                    kind: match rule.kind {
                        ForwardKind::Local => sync::ForwardKind::Local,
                        ForwardKind::Remote => sync::ForwardKind::Remote,
                        ForwardKind::Dynamic => sync::ForwardKind::Dynamic,
                    },
                    bind_host: rule.bind.host.to_string(),
                    bind_port: rule.bind.port,
                    target: rule.target.as_ref().map(|target| sync::Endpoint {
                        host: target.host.to_string(),
                        port: target.port,
                    }),
                    sort_order: rule.sort_order,
                }),
                edited_ms: rule.edited_ms,
            });
        }
        for category in &self.snippet_categories {
            records.push(LocalRecord {
                id: category.sync_id.to_string(),
                data: RecordData::SnippetCategory(sync::SnippetCategoryData {
                    name: category.name.to_string(),
                }),
                edited_ms: category.edited_ms,
            });
        }
        for snippet in &self.snippets {
            let scope = match snippet.scope {
                SnippetScope::All => sync::SnippetScope::All,
                SnippetScope::Group(group) => match group_id(group) {
                    Some(group) => sync::SnippetScope::Group(group),
                    None => continue,
                },
                // A serial host's snippets stay with it.
                SnippetScope::Host(host) => match host_id(host) {
                    Some(host) => sync::SnippetScope::Host(host),
                    None => continue,
                },
            };
            records.push(LocalRecord {
                id: snippet.sync_id.to_string(),
                data: RecordData::Snippet(sync::SnippetData {
                    category: snippet.category.and_then(|category| {
                        self.snippet_category(category)
                            .map(|category| category.sync_id.to_string())
                    }),
                    name: snippet.name.to_string(),
                    command: snippet.command.clone(),
                    scope,
                    run_on_click: snippet.run_on_click,
                }),
                edited_ms: snippet.edited_ms,
            });
        }
        SyncExport {
            records,
            secrets: self.secret_sources(),
            pending: self.sync_pending.iter().cloned().collect(),
            revision: self.sync_revision,
        }
    }

    /// A saved host that syncs: not a serial one.
    fn synced_host(&self, id: HostId) -> Option<&Host> {
        self.saved_host(id).filter(|host| !host.is_serial())
    }

    /// Where each secret that syncs is: the passwords the hosts that sync
    /// log in and go through proxies with, and the credentials' secrets.
    /// A key credential's are synced only for a key ShellRS keeps: a key
    /// file of the user's is this computer's.
    fn secret_sources(&self) -> Vec<SecretSource> {
        let mut sources: Vec<SecretSource> = Vec::new();
        let mut add = |kind: Kind, id: String, place: SecretPlace| {
            let key = RecordKey::new(kind, id);
            if sources.iter().any(|source| source.key == key) {
                return;
            }
            let edited_ms = self
                .secret_edits
                .get(&(key.kind.to_string(), key.id.clone()))
                .copied()
                .unwrap_or_default();
            sources.push(SecretSource {
                key,
                edited_ms,
                place,
            });
        };
        for host in self.hosts.iter().filter(|host| !host.is_serial()) {
            for secret in self.connection_of(host).password_entries() {
                let (kind, endpoint) = match &secret {
                    SecretRef::Password { user, host, port } => {
                        (Kind::SecretPassword, endpoint_of(user, host, *port))
                    }
                    SecretRef::Proxy { user, host, port } => {
                        (Kind::SecretProxy, endpoint_of(user, host, *port))
                    }
                    // A credential's are its own, below.
                    _ => continue,
                };
                let id = sync::endpoint_secret_id(&endpoint.user, &endpoint.host, endpoint.port);
                add(kind, id, SecretPlace::Keychain(secret, Some(endpoint)));
            }
        }
        for credential in &self.credentials {
            let id = credential.keychain_id.to_string();
            match (credential.kind, credential.key_path.as_deref()) {
                (CredentialKind::Password, _) => add(
                    Kind::SecretCredential,
                    id,
                    SecretPlace::Keychain(credential.password_secret(), None),
                ),
                (CredentialKind::Key, Some(path)) if self.keeps_key(path) => {
                    add(
                        Kind::SecretPrivateKey,
                        id.clone(),
                        SecretPlace::KeyFile(PathBuf::from(path)),
                    );
                    add(
                        Kind::SecretPassphrase,
                        id,
                        SecretPlace::Keychain(SecretRef::passphrase(path), None),
                    );
                }
                _ => {}
            }
        }
        sources
    }

    /// Record that the user saved `secret` (or deleted it, `saved` false),
    /// for sync to tell their edit. Either way the secret is theirs now, no
    /// longer one a round has still to write, and its new edit time has the
    /// next round read it: a deletion too, or it would not be seen.
    pub(super) fn note_secret_edit(&mut self, secret: &SecretRef) {
        let keys: Vec<(Kind, String)> = match secret {
            SecretRef::Password { user, host, port } => {
                vec![(
                    Kind::SecretPassword,
                    sync::endpoint_secret_id(user, host, *port),
                )]
            }
            SecretRef::Proxy { user, host, port } => {
                vec![(
                    Kind::SecretProxy,
                    sync::endpoint_secret_id(user, host, *port),
                )]
            }
            SecretRef::Credential { keychain_id } => {
                vec![(Kind::SecretCredential, keychain_id.clone())]
            }
            SecretRef::Passphrase { key_path } => self
                .credentials
                .iter()
                .filter(|credential| credential.key_path.as_deref() == Some(key_path.as_str()))
                .map(|credential| (Kind::SecretPassphrase, credential.keychain_id.to_string()))
                .collect(),
            SecretRef::Temporary { .. } | SecretRef::SyncKeyring | SecretRef::SyncLogin => {
                Vec::new()
            }
        };
        for (kind, id) in keys {
            self.stamp_secret(kind, id);
        }
    }

    /// Note that a round's secret is still to be written, or no longer, in
    /// memory and in the database.
    fn set_pending(&mut self, key: &RecordKey, pending: bool) {
        let changed = if pending {
            self.sync_pending.insert(key.clone())
        } else {
            self.sync_pending.remove(key)
        };
        if changed && let Some(database) = &self.database {
            // Left pending, it is written again next round: harmless.
            let _ = database.set_sync_pending(key.kind.as_str(), &key.id, pending);
        }
    }

    /// Date an edit of the secret of record `kind`/`id`, in memory and in
    /// the database.
    pub(super) fn stamp_secret(&mut self, kind: Kind, id: String) {
        let edited = self
            .secret_edits
            .entry((kind.to_string(), id.clone()))
            .or_default();
        *edited = super::edited_after(*edited);
        let edited_ms = *edited;
        self.sync_revision += 1;
        self.set_pending(&RecordKey::new(kind.clone(), id.clone()), false);
        if let Some(database) = &self.database {
            // An edit time not saved costs one secret syncing late, and
            // only after a restart: nothing to tell the user about.
            let _ = database.set_secret_edit(kind.as_str(), &id, edited_ms);
        }
    }

    /// The sync state as last saved, `None` while sync is not set up.
    pub fn sync_state(&self) -> Option<Vec<u8>> {
        self.sync_state.clone()
    }

    /// Keep `state`, as a push returned it. Kept in memory even when the
    /// database refuses it, in its words: the round's result says so.
    pub fn save_sync_state(&mut self, state: &[u8]) -> Result<(), String> {
        self.sync_state = Some(state.to_vec());
        match &self.database {
            Some(database) => database
                .set_sync_state(state)
                .map_err(|error| error.to_string()),
            None => Ok(()),
        }
    }

    /// Forget the sync state, as turning sync off does, and the secrets a
    /// round brought that were still to be written.
    pub fn clear_sync_state(&mut self) {
        self.sync_state = None;
        self.sync_pending.clear();
        if let Some(database) = &self.database {
            let _ = database.clear_sync_state();
        }
    }

    /// Apply what a round says the app should change, with `state` to keep,
    /// all in one transaction: either all of it lands, in memory and in the
    /// database, or none does. `export` is what the round read; if the user
    /// changed something since, the round is stale and nothing is applied.
    /// The secrets are returned to write on a worker thread.
    pub fn apply_sync(
        &mut self,
        changes: &[sync::Change],
        state: &[u8],
        export: &SyncExport,
    ) -> Result<SecretWork, ApplyError> {
        if export.revision != self.sync_revision {
            return Err(ApplyError::Stale);
        }
        let saved = self.saved();
        let ids = self.ids_for(changes);
        let mut writes = Vec::new();
        let mut work = SecretWork::default();
        let mut passphrases = Vec::new();
        for change in changes {
            self.apply_change(
                change,
                &ids,
                export,
                &mut writes,
                &mut work,
                &mut passphrases,
            );
        }
        // A passphrase goes under its key's file: one the round brings, or
        // the one there. A credential with neither waits for its key.
        for (credential, passphrase) in passphrases {
            if let Some(key) = work
                .keys
                .iter_mut()
                .find(|key| key.credential == credential)
            {
                key.passphrase = Some(passphrase);
            } else if let Some(path) = self
                .credentials
                .iter()
                .find(|c| c.keychain_id == credential)
                .and_then(|c| c.key_path.as_deref())
            {
                work.keychain.push((
                    RecordKey::new(Kind::SecretPassphrase, credential.to_string()),
                    SecretRef::passphrase(path),
                    Some(passphrase),
                ));
            } else {
                work.waiting.push(RecordKey::new(
                    Kind::SecretPassphrase,
                    credential.to_string(),
                ));
            }
        }
        // What the work is for stays pending until written; the round's
        // other secrets need nothing more.
        let pending = work.pending();
        for key in changes.iter().map(sync::Change::key) {
            if key.kind.is_secret() {
                let now = pending.contains(&key);
                if now {
                    self.sync_pending.insert(key.clone());
                } else {
                    self.sync_pending.remove(&key);
                }
                writes.push(Write::Pending(key, now));
            }
        }
        if let Some(database) = &self.database {
            let result = database.all_or_nothing(|db| {
                db.defer_foreign_keys()?;
                for write in &writes {
                    self.write(db, write)?;
                }
                db.set_sync_state(state)
            });
            if let Err(error) = result {
                self.restore(saved);
                return Err(ApplyError::NotSaved(error.to_string()));
            }
        }
        self.sync_state = Some(state.to_vec());
        self.changed();
        Ok(work)
    }

    /// Take what writing a round's secrets came to: the credentials whose
    /// private key it wrote point at its file (this device's own path, no
    /// edit), and what it wrote is no longer pending.
    pub fn adopt_synced_secrets(&mut self, written: &SecretsWritten) {
        for key in &written.done {
            self.set_pending(key, false);
        }
        for (credential, path) in &written.key_paths {
            let Some(credential) = self
                .credentials
                .iter_mut()
                .find(|c| &c.keychain_id == credential)
            else {
                continue;
            };
            credential.key_path = Some(path.clone());
            let credential = credential.clone();
            self.persist(super::Change::SaveCredential, |db| {
                db.update_credential(&credential)
            });
        }
        for failure in &written.failures {
            self.report(failure.clone());
        }
        self.changed();
    }

    fn saved(&self) -> Saved {
        Saved {
            groups: self.groups.clone(),
            hosts: self.hosts.clone(),
            credentials: self.credentials.clone(),
            forwards: self.forwards.clone(),
            snippets: self.snippets.clone(),
            snippet_categories: self.snippet_categories.clone(),
            bookmarks: self.bookmarks.clone(),
            bookmark_meta: self.bookmark_meta.clone(),
            recent: self.recent.clone(),
            sync_pending: self.sync_pending.clone(),
            next: [
                self.next_group_id,
                self.next_host_id,
                self.next_credential_id,
                self.next_forward_id,
                self.next_snippet_category_id,
                self.next_snippet_id,
            ],
        }
    }

    fn restore(&mut self, saved: Saved) {
        self.groups = saved.groups;
        self.hosts = saved.hosts;
        self.credentials = saved.credentials;
        self.forwards = saved.forwards;
        self.snippets = saved.snippets;
        self.snippet_categories = saved.snippet_categories;
        self.bookmarks = saved.bookmarks;
        self.bookmark_meta = saved.bookmark_meta;
        self.recent = saved.recent;
        self.sync_pending = saved.sync_pending;
        [
            self.next_group_id,
            self.next_host_id,
            self.next_credential_id,
            self.next_forward_id,
            self.next_snippet_category_id,
            self.next_snippet_id,
        ] = saved.next;
    }

    /// Every global id the store knows, and an internal id for each record
    /// `changes` brings that it does not: a host may jump through one that
    /// comes later in the same round.
    fn ids_for(&mut self, changes: &[sync::Change]) -> Ids {
        let mut ids = Ids::default();
        for group in &self.groups {
            ids.groups.insert(group.sync_id.to_string(), group.id);
        }
        for host in &self.hosts {
            ids.hosts.insert(host.public_id.to_string(), host.id);
        }
        for credential in &self.credentials {
            ids.credentials
                .insert(credential.keychain_id.to_string(), credential.id);
        }
        for rule in &self.forwards {
            ids.forwards.insert(rule.sync_id.to_string(), rule.id);
        }
        for category in &self.snippet_categories {
            ids.categories
                .insert(category.sync_id.to_string(), category.id);
        }
        for snippet in &self.snippets {
            ids.snippets.insert(snippet.sync_id.to_string(), snippet.id);
        }
        for change in changes {
            let sync::Change::Upsert { id, data } = change else {
                continue;
            };
            let id = id.clone();
            match data {
                RecordData::Group(_) if !ids.groups.contains_key(&id) => {
                    ids.groups.insert(id, GroupId(self.next_group_id));
                    self.next_group_id += 1;
                }
                RecordData::Host(_) if !ids.hosts.contains_key(&id) => {
                    ids.hosts.insert(id, HostId(self.next_host_id));
                    self.next_host_id += 1;
                }
                RecordData::Credential(_) if !ids.credentials.contains_key(&id) => {
                    ids.credentials
                        .insert(id, CredentialId(self.next_credential_id));
                    self.next_credential_id += 1;
                }
                RecordData::Forward(_) if !ids.forwards.contains_key(&id) => {
                    ids.forwards.insert(id, ForwardId(self.next_forward_id));
                    self.next_forward_id += 1;
                }
                RecordData::SnippetCategory(_) if !ids.categories.contains_key(&id) => {
                    ids.categories
                        .insert(id, SnippetCategoryId(self.next_snippet_category_id));
                    self.next_snippet_category_id += 1;
                }
                RecordData::Snippet(_) if !ids.snippets.contains_key(&id) => {
                    ids.snippets.insert(id, SnippetId(self.next_snippet_id));
                    self.next_snippet_id += 1;
                }
                _ => {}
            }
        }
        ids
    }

    fn apply_change(
        &mut self,
        change: &sync::Change,
        ids: &Ids,
        export: &SyncExport,
        writes: &mut Vec<Write>,
        work: &mut SecretWork,
        passphrases: &mut Vec<(PublicId, Zeroizing<String>)>,
    ) {
        match change {
            sync::Change::Upsert { id, data } => {
                self.apply_upsert(id, data, ids, writes, work, passphrases)
            }
            sync::Change::Delete { kind, id } => {
                self.apply_delete(kind, id, ids, export, writes, work)
            }
        }
    }

    fn apply_upsert(
        &mut self,
        id: &str,
        data: &RecordData,
        ids: &Ids,
        writes: &mut Vec<Write>,
        work: &mut SecretWork,
        passphrases: &mut Vec<(PublicId, Zeroizing<String>)>,
    ) {
        let group = |id: &Option<String>| id.as_ref().and_then(|id| ids.groups.get(id)).copied();
        let host = |id: &str| ids.hosts.get(id).copied();
        match data {
            RecordData::Group(data) => {
                let Some(&group_id) = ids.groups.get(id) else {
                    return;
                };
                let parent = group(&data.parent);
                let mut fresh =
                    HostGroup::new(group_id, GroupDraft::new(data.name.as_str(), parent));
                fresh.sort_order = data.sort_order;
                fresh.sync_id = PublicId::from_stored(id.to_owned());
                let existing = self.groups.iter_mut().find(|g| g.id == group_id);
                let is_new = existing.is_none();
                match existing {
                    Some(old) => {
                        fresh.expanded = old.expanded;
                        fresh.edited_ms = old.edited_ms;
                        *old = fresh;
                    }
                    None => self.groups.push(fresh),
                }
                writes.push(Write::Group(group_id, is_new));
            }
            RecordData::Credential(data) => {
                let Some(&credential_id) = ids.credentials.get(id) else {
                    return;
                };
                let kind = app_credential_kind(data.kind);
                let mut draft =
                    CredentialDraft::new(data.name.as_str(), kind, data.username.as_str());
                let old = self
                    .credentials
                    .iter()
                    .find(|c| c.id == credential_id)
                    .cloned();
                // A key credential keeps its file; one that is no longer a
                // key credential lets go of a key ShellRS kept.
                match old.as_ref().and_then(|old| old.key_path.clone()) {
                    Some(path) if kind == CredentialKind::Key => draft.key_path = Some(path),
                    Some(path) if self.keeps_key(&path) && self.key_file_users(&path) == 1 => {
                        work.forget.push((
                            RecordKey::new(Kind::SecretPrivateKey, id),
                            PathBuf::from(path.as_ref()),
                        ));
                    }
                    _ => {}
                }
                let mut fresh = Credential::new(credential_id, draft);
                fresh.keychain_id = PublicId::from_stored(id.to_owned());
                fresh.sort_order = data.sort_order;
                fresh.edited_ms = old.as_ref().map_or(0, |old| old.edited_ms);
                let user = fresh.user.clone();
                match self.credentials.iter_mut().find(|c| c.id == credential_id) {
                    Some(slot) => *slot = fresh,
                    None => self.credentials.push(fresh),
                }
                // The hosts using it keep a copy of its user name.
                for host in self.hosts.iter_mut().chain(&mut self.temporary) {
                    if host.credential == Some(credential_id) {
                        host.user = user.clone();
                    }
                }
                writes.push(Write::Credential(credential_id, old.is_none()));
            }
            RecordData::Host(data) => {
                let Some(host_id) = host(id) else {
                    return;
                };
                let credential = data
                    .credential
                    .as_ref()
                    .and_then(|id| ids.credentials.get(id))
                    .and_then(|id| self.credential(*id));
                let user = match credential {
                    Some(credential) => credential.user.clone(),
                    None => data.username.clone().unwrap_or_default().into(),
                };
                let auth = match data.auth {
                    sync::Auth::NoPassword => AuthKind::NoPassword,
                    _ => AuthKind::Password,
                };
                let mut draft = HostDraft::new(
                    data.name.as_str(),
                    data.address.as_str(),
                    data.port,
                    user,
                    auth,
                    group(&data.group),
                )
                .with_notes(data.notes.as_str())
                .with_route(match &data.route {
                    sync::Route::Direct => Route::Direct,
                    sync::Route::Jump(hops) => Route::Jump(
                        hops.iter()
                            .map(|hop| hop.as_deref().and_then(host))
                            .collect(),
                    ),
                    sync::Route::Proxy(proxy) => Route::Proxy(
                        ProxySettings::new(
                            match proxy.kind {
                                sync::ProxyKind::Http => ProxyKind::Http,
                                sync::ProxyKind::Socks5 => ProxyKind::Socks5,
                            },
                            proxy.host.as_str(),
                            proxy.port,
                        )
                        .with_user(proxy.username.clone().unwrap_or_default()),
                    ),
                });
                draft.credential = credential.map(|credential| credential.id);
                if let sync::Protocol::Telnet(telnet) = &data.protocol {
                    draft.protocol = Protocol::Telnet(TelnetSettings {
                        user_prompt: telnet.user_prompt.clone().unwrap_or_default().into(),
                        password_prompt: telnet.pass_prompt.clone().unwrap_or_default().into(),
                    });
                }
                let mut fresh = Host::new(host_id, draft);
                fresh.public_id = PublicId::from_stored(id.to_owned());
                fresh.sort_order = data.sort_order;
                let is_new = match self.hosts.iter_mut().find(|h| h.id == host_id) {
                    Some(old) => {
                        fresh.os = old.os;
                        fresh.sidebar_tool = old.sidebar_tool.take();
                        fresh.edited_ms = old.edited_ms;
                        *old = fresh;
                        false
                    }
                    None => {
                        self.hosts.push(fresh);
                        true
                    }
                };
                writes.push(Write::Host(host_id, is_new));
            }
            RecordData::Bookmark(data) => {
                let Some(host_id) = host(&data.host) else {
                    return;
                };
                let side = BookmarkSide::Remote;
                let list = self.bookmarks.entry((host_id, side)).or_default();
                if !list.contains(&data.path) {
                    list.push(data.path.clone());
                }
                let meta = self
                    .bookmark_meta
                    .entry((host_id, side, data.path.clone()))
                    .or_default();
                meta.sort_order = data.sort_order;
                self.sort_bookmarks(host_id, side);
                writes.push(Write::Bookmark(host_id, data.path.clone()));
            }
            RecordData::Forward(data) => {
                let (Some(&rule_id), Some(host_id)) = (ids.forwards.get(id), host(&data.host))
                else {
                    return;
                };
                let kind = match data.kind {
                    sync::ForwardKind::Local => ForwardKind::Local,
                    sync::ForwardKind::Remote => ForwardKind::Remote,
                    sync::ForwardKind::Dynamic => ForwardKind::Dynamic,
                };
                let old = self
                    .forwards
                    .iter()
                    .find(|rule| rule.id == rule_id)
                    .cloned();
                let draft = ForwardDraft::new(
                    kind,
                    host_id,
                    ForwardEndpoint::new(data.bind_host.as_str(), data.bind_port),
                    data.target
                        .as_ref()
                        .map(|target| ForwardEndpoint::new(target.host.as_str(), target.port)),
                )
                .with_name(data.name.as_str())
                .with_auto_start(old.as_ref().is_some_and(|old| old.auto_start));
                let mut fresh = ForwardRule::new(rule_id, draft);
                fresh.sync_id = PublicId::from_stored(id.to_owned());
                fresh.sort_order = data.sort_order;
                fresh.edited_ms = old.as_ref().map_or(0, |old| old.edited_ms);
                match self.forwards.iter_mut().find(|rule| rule.id == rule_id) {
                    Some(slot) => *slot = fresh,
                    None => self.forwards.push(fresh),
                }
                self.forwards.sort_by_key(|rule| (rule.sort_order, rule.id));
                writes.push(Write::Forward(rule_id, old.is_none()));
            }
            RecordData::SnippetCategory(data) => {
                let Some(&category_id) = ids.categories.get(id) else {
                    return;
                };
                let mut fresh = SnippetCategory::new(category_id, data.name.as_str());
                fresh.sync_id = PublicId::from_stored(id.to_owned());
                let is_new = match self
                    .snippet_categories
                    .iter_mut()
                    .find(|category| category.id == category_id)
                {
                    Some(old) => {
                        fresh.edited_ms = old.edited_ms;
                        *old = fresh;
                        false
                    }
                    None => {
                        self.snippet_categories.push(fresh);
                        true
                    }
                };
                writes.push(Write::Category(category_id, is_new));
            }
            RecordData::Snippet(data) => {
                let Some(&snippet_id) = ids.snippets.get(id) else {
                    return;
                };
                let category = data
                    .category
                    .as_ref()
                    .and_then(|id| ids.categories.get(id))
                    .copied();
                let mut draft =
                    SnippetDraft::new(data.name.as_str(), data.command.clone(), category)
                        .with_run_on_click(data.run_on_click);
                draft.scope = match &data.scope {
                    sync::SnippetScope::All => SnippetScope::All,
                    sync::SnippetScope::Group(id) => match ids.groups.get(id) {
                        Some(group) => SnippetScope::Group(*group),
                        None => return,
                    },
                    sync::SnippetScope::Host(id) => match host(id) {
                        Some(host) => SnippetScope::Host(host),
                        None => return,
                    },
                };
                let mut fresh = Snippet::new(snippet_id, draft);
                fresh.sync_id = PublicId::from_stored(id.to_owned());
                let is_new = match self.snippets.iter_mut().find(|s| s.id == snippet_id) {
                    Some(old) => {
                        fresh.edited_ms = old.edited_ms;
                        *old = fresh;
                        false
                    }
                    None => {
                        self.snippets.push(fresh);
                        true
                    }
                };
                writes.push(Write::Snippet(snippet_id, is_new));
            }
            RecordData::Password(endpoint) => work.keychain.push((
                RecordKey::new(Kind::SecretPassword, id),
                SecretRef::password(&endpoint.user, &endpoint.host, endpoint.port),
                Some(Zeroizing::new(endpoint.value.expose().to_owned())),
            )),
            RecordData::ProxyPassword(endpoint) => work.keychain.push((
                RecordKey::new(Kind::SecretProxy, id),
                SecretRef::proxy(&endpoint.user, &endpoint.host, endpoint.port),
                Some(Zeroizing::new(endpoint.value.expose().to_owned())),
            )),
            RecordData::CredentialPassword(value) => work.keychain.push((
                RecordKey::new(Kind::SecretCredential, id),
                SecretRef::credential(id),
                Some(Zeroizing::new(value.expose().to_owned())),
            )),
            RecordData::Passphrase(value) => passphrases.push((
                PublicId::from_stored(id.to_owned()),
                Zeroizing::new(value.expose().to_owned()),
            )),
            RecordData::PrivateKey(text) => {
                let Some(credential) = ids.credentials.get(id).and_then(|id| self.credential(*id))
                else {
                    return;
                };
                let replace = credential
                    .key_path
                    .as_deref()
                    .filter(|path| self.keeps_key(path) && self.key_file_users(path) == 1)
                    .map(PathBuf::from);
                work.keys.push(KeyWrite {
                    credential: credential.keychain_id.clone(),
                    replace,
                    text: Zeroizing::new(text.expose().to_owned()),
                    passphrase: None,
                });
            }
        }
    }

    fn apply_delete(
        &mut self,
        kind: &Kind,
        id: &str,
        ids: &Ids,
        export: &SyncExport,
        writes: &mut Vec<Write>,
        work: &mut SecretWork,
    ) {
        match kind {
            Kind::Group => {
                if let Some(&group_id) = ids.groups.get(id) {
                    self.groups.retain(|group| group.id != group_id);
                    writes.push(Write::DeleteGroup(group_id));
                }
            }
            Kind::Credential => {
                if let Some(&credential_id) = ids.credentials.get(id) {
                    if let Some(index) = self.credentials.iter().position(|c| c.id == credential_id)
                    {
                        let auth = self.credentials.remove(index).kind.without_credential();
                        // The round moved its hosts off it first; any left
                        // log in on their own, as the database makes them.
                        for host in self.hosts.iter_mut().chain(&mut self.temporary) {
                            if host.credential == Some(credential_id) {
                                host.credential = None;
                                host.auth = auth;
                            }
                        }
                    }
                    writes.push(Write::DeleteCredential(credential_id));
                }
            }
            Kind::Host => {
                if let Some(&host_id) = ids.hosts.get(id) {
                    self.forget_host(host_id);
                    writes.push(Write::DeleteHost(host_id));
                }
            }
            Kind::Bookmark => {
                let found = self.bookmarks.iter().find_map(|((host, side), paths)| {
                    let public_id = self.synced_host(*host)?.public_id.to_string();
                    (*side == BookmarkSide::Remote)
                        .then(|| {
                            paths
                                .iter()
                                .find(|path| sync::bookmark_id(&public_id, path) == id)
                                .map(|path| (*host, path.clone()))
                        })
                        .flatten()
                });
                if let Some((host, path)) = found {
                    if let Some(list) = self.bookmarks.get_mut(&(host, BookmarkSide::Remote)) {
                        list.retain(|existing| *existing != path);
                    }
                    self.bookmark_meta
                        .remove(&(host, BookmarkSide::Remote, path.clone()));
                    writes.push(Write::DeleteBookmark(host, path));
                }
            }
            Kind::Forward => {
                if let Some(&rule_id) = ids.forwards.get(id) {
                    self.forwards.retain(|rule| rule.id != rule_id);
                    writes.push(Write::DeleteForward(rule_id));
                }
            }
            Kind::SnippetCategory => {
                if let Some(&category_id) = ids.categories.get(id) {
                    self.snippet_categories.retain(|c| c.id != category_id);
                    self.snippets
                        .retain(|snippet| snippet.category != Some(category_id));
                    writes.push(Write::DeleteCategory(category_id));
                }
            }
            Kind::Snippet => {
                if let Some(&snippet_id) = ids.snippets.get(id) {
                    self.snippets.retain(|snippet| snippet.id != snippet_id);
                    writes.push(Write::DeleteSnippet(snippet_id));
                }
            }
            Kind::SecretPassword | Kind::SecretProxy => {
                let source = export
                    .secrets
                    .iter()
                    .find(|source| source.key.kind == *kind && source.key.id == id);
                if let Some(SecretPlace::Keychain(secret, _)) = source.map(|source| &source.place) {
                    work.keychain
                        .push((RecordKey::new(kind.clone(), id), secret.clone(), None));
                }
            }
            Kind::SecretCredential => work.keychain.push((
                RecordKey::new(Kind::SecretCredential, id),
                SecretRef::credential(id),
                None,
            )),
            Kind::SecretPassphrase | Kind::SecretPrivateKey => {
                let Some(credential) = ids
                    .credentials
                    .get(id)
                    .and_then(|id| self.credentials.iter_mut().find(|c| c.id == *id))
                else {
                    return;
                };
                let Some(path) = credential.key_path.clone() else {
                    return;
                };
                if *kind == Kind::SecretPassphrase {
                    work.keychain.push((
                        RecordKey::new(Kind::SecretPassphrase, id),
                        SecretRef::passphrase(path.as_ref()),
                        None,
                    ));
                    return;
                }
                // The key went: the credential has no file until another
                // comes, and one ShellRS kept goes too.
                credential.key_path = None;
                let credential_id = credential.id;
                if self.keeps_key(&path) && self.key_file_users(&path) == 0 {
                    work.forget.push((
                        RecordKey::new(Kind::SecretPrivateKey, id),
                        PathBuf::from(path.as_ref()),
                    ));
                }
                writes.push(Write::Credential(credential_id, false));
            }
            Kind::Secret(_) | Kind::Other(_) => {}
        }
    }

    /// Take a host away as the database's foreign keys do, with no edit of
    /// anything: the round deals with what pointed at it.
    fn forget_host(&mut self, id: HostId) {
        self.hosts.retain(|host| host.id != id);
        for host in &mut self.hosts {
            if let Route::Jump(hops) = &mut host.route {
                for hop in hops.iter_mut().filter(|hop| **hop == Some(id)) {
                    *hop = None;
                }
            }
        }
        self.recent.retain(|recent| *recent != id);
        self.bookmarks.retain(|(host, _), _| *host != id);
        self.bookmark_meta.retain(|(host, _, _), _| *host != id);
        self.forwards.retain(|rule| rule.host != id);
        self.snippets
            .retain(|snippet| snippet.scope != SnippetScope::Host(id));
    }

    /// Put a pane's bookmarks in their `sort_order`.
    fn sort_bookmarks(&mut self, host: HostId, side: BookmarkSide) {
        let meta = &self.bookmark_meta;
        if let Some(list) = self.bookmarks.get_mut(&(host, side)) {
            list.sort_by_key(|path| {
                let order = meta
                    .get(&(host, side, path.clone()))
                    .map_or(0, |meta| meta.sort_order);
                (order, path.clone())
            });
        }
    }

    fn write(&self, db: &crate::HostDatabase, write: &Write) -> rusqlite::Result<()> {
        match write {
            Write::Pending(key, pending) => {
                db.set_sync_pending(key.kind.as_str(), &key.id, *pending)
            }
            Write::Group(id, new) => match self.group(*id) {
                Some(group) if *new => db.insert_group(group),
                Some(group) => db.update_group(group),
                None => Ok(()),
            },
            Write::DeleteGroup(id) => db.remove_group(*id),
            Write::Credential(id, new) => match self.credential(*id) {
                Some(credential) if *new => db.insert_credential(credential),
                Some(credential) => db.update_credential(credential),
                None => Ok(()),
            },
            Write::DeleteCredential(id) => db.remove_credential(*id, AuthKind::NoPassword, 0),
            Write::Host(id, new) => match self.saved_host(*id) {
                Some(host) if *new => db.insert_host(host),
                Some(host) => db.update_host(host),
                None => Ok(()),
            },
            Write::DeleteHost(id) => db.remove_host(*id),
            Write::Bookmark(host, path) => {
                let meta = self.bookmark_meta(*host, BookmarkSide::Remote, path);
                db.insert_bookmark(
                    *host,
                    BookmarkSide::Remote,
                    path,
                    meta.sort_order,
                    meta.edited_ms,
                )
            }
            Write::DeleteBookmark(host, path) => {
                db.remove_bookmark(*host, BookmarkSide::Remote, path)
            }
            Write::Forward(id, new) => match self.forward(*id) {
                Some(rule) if *new => db.insert_forward(rule),
                Some(rule) => db.update_forward(rule),
                None => Ok(()),
            },
            Write::DeleteForward(id) => db.remove_forward(*id),
            Write::Category(id, new) => match self.snippet_category(*id) {
                Some(category) if *new => db.insert_snippet_category(category),
                Some(category) => db.update_snippet_category(category),
                None => Ok(()),
            },
            Write::DeleteCategory(id) => db.remove_snippet_category(*id),
            Write::Snippet(id, new) => match self.snippet(*id) {
                Some(snippet) if *new => db.insert_snippet(snippet),
                Some(snippet) => db.update_snippet(snippet),
                None => Ok(()),
            },
            Write::DeleteSnippet(id) => db.remove_snippet(*id),
        }
    }
}

/// Text that is empty as `None`, as a Telnet prompt is.
fn some_text(text: &SharedString) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

fn endpoint_of(user: &str, host: &str, port: u16) -> SecretEndpoint {
    SecretEndpoint {
        user: user.to_owned(),
        host: host.to_owned(),
        port,
    }
}

fn sync_credential_kind(kind: CredentialKind) -> sync::CredentialKind {
    match kind {
        CredentialKind::Password => sync::CredentialKind::Password,
        CredentialKind::Key => sync::CredentialKind::Key,
        CredentialKind::Agent => sync::CredentialKind::Agent,
    }
}

fn app_credential_kind(kind: sync::CredentialKind) -> CredentialKind {
    match kind {
        sync::CredentialKind::Password => CredentialKind::Password,
        sync::CredentialKind::Key => CredentialKind::Key,
        sync::CredentialKind::Agent => CredentialKind::Agent,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use shellrs_sync::{
        Capabilities, DeviceId, DeviceInfo, FixedEnv, KdfParams, Keyring, MemoryStore, Pushed,
        RoundOptions, SyncState, create_vault, pull, push,
    };

    use super::*;
    use crate::secrets::{InMemorySecretStore, SharedSecretStore};
    use crate::{HostDatabase, SerialSettings};

    /// One device: its store, keychain, key folder and sync state.
    struct Device {
        store: HostData,
        secrets: Arc<InMemorySecretStore>,
        data: tempfile::TempDir,
        keys_dir: PathBuf,
        state: SyncState,
        env: FixedEnv,
        /// The keychain refuses what a round brings, as one locked
        /// meanwhile does.
        refuse_writes: bool,
    }

    impl Device {
        fn new(name: char) -> Self {
            let data = tempfile::tempdir().unwrap();
            let keys_dir = data.path().join("keys");
            let secrets = Arc::new(InMemorySecretStore::default());
            let database = HostDatabase::open(&data.path().join("shellrs.db")).unwrap();
            let store = HostData::load(database)
                .unwrap()
                .with_secrets(secrets.clone() as SharedSecretStore)
                .with_key_dir(&keys_dir);
            let id = DeviceId::parse(&name.to_string().repeat(16)).unwrap();
            Self {
                store,
                secrets,
                data,
                keys_dir,
                state: SyncState::new(id),
                env: FixedEnv::new(1_800_000_000_000, name as u64),
                refuse_writes: false,
            }
        }

        /// A round, as the sync service runs it.
        fn sync(&mut self, vault: &MemoryStore, keys: &Keyring) -> usize {
            self.env.advance(1000);
            let export = self.store.sync_export();
            let local = export.local(&*self.secrets, &self.state);
            let pulled = pull(
                vault,
                keys,
                &self.state,
                &local,
                &Capabilities::ALL,
                &RoundOptions::default(),
                &mut self.env,
            )
            .unwrap();
            let state = pulled.state.to_bytes();
            let work = self
                .store
                .apply_sync(&pulled.changes, &state, &export)
                .unwrap();
            let written = if self.refuse_writes {
                work.run(&Locked, Some(&self.keys_dir))
            } else {
                work.run(&*self.secrets, Some(&self.keys_dir))
            };
            assert!(
                self.refuse_writes || written.failures.is_empty(),
                "{:?}",
                written.failures
            );
            self.store.adopt_synced_secrets(&written);
            self.state = pulled.state;
            let info = DeviceInfo {
                name: "test".into(),
                platform: "test".into(),
                app_version: "0.0.0".into(),
            };
            if let Pushed::Uploaded { state } =
                push(vault, keys, &info, &self.state, &mut self.env).unwrap()
            {
                self.state = state;
            }
            pulled.changes.len()
        }

        /// What the device holds that syncs, as records, secrets included,
        /// and with the edit times left out.
        fn held(&self) -> Vec<(String, String)> {
            let export = self.store.sync_export();
            let read = read_secrets(&export.secrets, &[], &nothing_seen(), &*self.secrets);
            let mut held: Vec<(String, String)> = export
                .records
                .into_iter()
                .chain(read.records)
                .map(|record| {
                    let shown = match &record.data {
                        RecordData::Password(e) | RecordData::ProxyPassword(e) => {
                            format!("{}@{}:{} {}", e.user, e.host, e.port, e.value.expose())
                        }
                        RecordData::CredentialPassword(s)
                        | RecordData::Passphrase(s)
                        | RecordData::PrivateKey(s) => s.expose().to_owned(),
                        other => format!("{other:?}"),
                    };
                    (format!("{}/{}", record.data.kind(), record.id), shown)
                })
                .collect();
            held.sort();
            held
        }
    }

    /// Run the keychain writes the changes queued, as the app does on a
    /// background thread.
    fn run_secret_jobs(store: &mut HostData) {
        for job in store.take_secret_jobs() {
            let outcome = job.run();
            store.secret_done(outcome);
        }
    }

    fn vault() -> (MemoryStore, Keyring) {
        let store = MemoryStore::new();
        let fast = KdfParams {
            memory_kib: 8,
            passes: 1,
            parallelism: 1,
        };
        let keys = create_vault(&store, "correct horse", fast, &mut FixedEnv::new(0, 7)).unwrap();
        (store, keys)
    }

    /// A device's worth of everything that syncs, and a serial host that
    /// does not.
    fn fill(device: &mut Device) {
        let store = &mut device.store;
        {
            let ops = store.insert_group(GroupDraft::new("ops", None));
            let db = store.insert_group(GroupDraft::new("db", Some(ops)));
            let deploy = store.insert_credential(CredentialDraft::new(
                "deploy",
                CredentialKind::Password,
                "deploy",
            ));
            let path = store.save_private_key(None, "-----KEY-----\n").unwrap();
            let key = store.insert_credential(
                CredentialDraft::new("key", CredentialKind::Key, "admin").with_key_path(path),
            );
            let bastion = store.insert(HostDraft::new(
                "bastion",
                "bastion.example",
                22,
                "root",
                AuthKind::Password,
                Some(ops),
            ));
            let web = store.insert(
                HostDraft::new("web", "10.0.0.1", 22, "", AuthKind::Password, Some(db))
                    .with_credential(key)
                    .with_route(Route::Jump(vec![Some(bastion)]))
                    .with_notes("behind the bastion"),
            );
            let mut switch = HostDraft::telnet("switch", "10.0.0.9", 23, None);
            switch.credential = Some(deploy);
            if let Protocol::Telnet(telnet) = &mut switch.protocol {
                telnet.user_prompt = "login:".into();
            }
            store.insert(switch);
            store.insert(
                HostDraft::new("proxied", "10.0.0.2", 22, "me", AuthKind::Password, None)
                    .with_route(Route::Proxy(
                        ProxySettings::new(ProxyKind::Socks5, "proxy", 1080).with_user("p"),
                    )),
            );
            store.insert(HostDraft::serial(
                "console",
                "/dev/ttyUSB0",
                SerialSettings::default(),
                None,
            ));
            store.add_bookmark(web, BookmarkSide::Remote, "/var/log");
            store.add_bookmark(web, BookmarkSide::Local, "/Users/me");
            store.insert_forward(
                ForwardDraft::new(
                    ForwardKind::Local,
                    web,
                    ForwardEndpoint::new("127.0.0.1", 5432),
                    Some(ForwardEndpoint::new("db.internal", 5432)),
                )
                .with_name("db")
                .with_auto_start(true),
            );
            let docker = store.insert_snippet_category("Docker");
            let mut ps = SnippetDraft::new("ps", "docker ps", Some(docker));
            ps.scope = SnippetScope::Group(ops);
            store.insert_snippet(ps);
            for (secret, value) in [
                (
                    SecretRef::password("root", "bastion.example", 22),
                    "pw-bastion",
                ),
                (SecretRef::proxy("p", "proxy", 1080), "pw-proxy"),
                (SecretRef::password("me", "10.0.0.2", 22), "pw-me"),
                (
                    store.credential(deploy).unwrap().password_secret(),
                    "pw-deploy",
                ),
                (
                    SecretRef::passphrase(
                        store
                            .credential(key)
                            .unwrap()
                            .key_path
                            .as_ref()
                            .unwrap()
                            .as_ref(),
                    ),
                    "pass-key",
                ),
            ] {
                store.save_secret(secret, Some(value.into()));
            }
        }
        run_secret_jobs(store);
    }

    #[test]
    fn two_devices_end_up_holding_the_same() {
        let (vault, keys) = vault();
        let mut a = Device::new('a');
        let mut b = Device::new('b');
        fill(&mut a);
        a.sync(&vault, &keys);
        b.sync(&vault, &keys);
        assert_eq!(b.held(), a.held());

        {
            let store = &b.store;
            // The serial host stayed on its computer; this device's own of
            // the rest came as new.
            assert!(store.hosts().iter().all(|host| !host.is_serial()));
            assert!(store.forwards().iter().all(|rule| !rule.auto_start));
            let web = store.hosts().iter().find(|h| h.name == "web").unwrap();
            assert!(store.bookmarks(web.id, BookmarkSide::Local).is_empty());
            assert_eq!(store.bookmarks(web.id, BookmarkSide::Remote), ["/var/log"]);
            assert_eq!(web.edited_ms, 0);
            // The key came as a file of its own, its passphrase under it.
            let key = store
                .credentials()
                .iter()
                .find(|c| c.name == "key")
                .unwrap();
            let path = key.key_path.as_deref().unwrap();
            assert!(path.starts_with(&b.keys_dir.display().to_string()));
            assert_eq!(std::fs::read_to_string(path).unwrap(), "-----KEY-----\n");
        }

        // An edit on one, and a deletion, reach the other.
        {
            let store = &mut b.store;
            let web = store.hosts().iter().find(|h| h.name == "web").unwrap().id;
            let mut renamed = store.host(web).unwrap().draft();
            renamed.name = "web-01".into();
            store.update(web, renamed);
            let rule = store.forwards()[0].id;
            store.remove_forward(rule);
        }
        b.sync(&vault, &keys);
        a.sync(&vault, &keys);
        assert_eq!(b.held(), a.held());
        assert!(a.store.hosts().iter().any(|h| h.name == "web-01"));
        assert!(a.store.forwards().is_empty());

        // Then a round changes nothing on either: what was applied is what
        // goes out again, exactly.
        assert_eq!(a.sync(&vault, &keys), 0);
        assert_eq!(b.sync(&vault, &keys), 0);
    }

    #[test]
    fn a_round_read_before_an_edit_is_not_applied() {
        let mut device = Device::new('a');
        let export = device.store.sync_export();
        device.store.insert_group(GroupDraft::new("new", None));
        let result = device.store.apply_sync(&[], b"state", &export);
        assert_eq!(result.unwrap_err(), ApplyError::Stale);
        assert_eq!(device.store.sync_state(), None);
    }

    #[test]
    fn what_the_database_refuses_is_applied_nowhere() {
        let data = tempfile::tempdir().unwrap();
        let path = data.path().join("shellrs.db");
        let mut store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        // A database that refuses every new host.
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER no_hosts BEFORE INSERT ON hosts \
                 BEGIN SELECT RAISE(ABORT, 'no'); END;",
            )
            .unwrap();
        let group = sync::Change::Upsert {
            id: "g1aaaaaaaaaaaaaa".into(),
            data: RecordData::Group(sync::GroupData {
                name: "ops".into(),
                parent: None,
                sort_order: 0,
            }),
        };
        let host = sync::Change::Upsert {
            id: "h1aaaaaaaaaaaaaa".into(),
            data: RecordData::Host(sync::HostData {
                name: "web".into(),
                address: "10.0.0.1".into(),
                port: 22,
                group: Some("g1aaaaaaaaaaaaaa".into()),
                sort_order: 0,
                protocol: sync::Protocol::Ssh,
                auth: sync::Auth::Password,
                username: Some("root".into()),
                credential: None,
                route: sync::Route::Direct,
                notes: String::new(),
            }),
        };
        let export = store.sync_export();
        let result = store.apply_sync(&[group, host], b"state", &export);
        assert!(matches!(result, Err(ApplyError::NotSaved(_))));
        assert!(store.groups().is_empty() && store.hosts().is_empty());
        assert_eq!(store.sync_state(), None);
        let reopened = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        assert!(reopened.groups().is_empty());
    }

    /// A state that has seen nothing yet: every secret is read.
    fn nothing_seen() -> SyncState {
        SyncState::new(DeviceId::parse("zzzzzzzzzzzzzzzz").unwrap())
    }

    /// A keychain that counts what it hands out.
    struct Counting {
        keychain: Arc<InMemorySecretStore>,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl Counting {
        fn reads(&self) -> usize {
            self.reads.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl SecretStore for Counting {
        fn get(&self, secret: &SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.keychain.get(secret)
        }
        fn set(&self, secret: &SecretRef, value: &str) -> anyhow::Result<()> {
            self.keychain.set(secret, value)
        }
        fn delete(&self, secret: &SecretRef) -> anyhow::Result<()> {
            self.keychain.delete(secret)
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    /// After the first round the keychain is read for what the user changed
    /// since, a deletion too, and for a password never saved, which it does
    /// not ask about: the round has the rest.
    #[test]
    fn a_round_reads_the_keychain_for_what_changed_only() {
        let (vault, keys) = vault();
        let mut device = Device::new('a');
        fill(&mut device);
        device.sync(&vault, &keys);
        let read = |device: &Device| {
            let counting = Counting {
                keychain: device.secrets.clone(),
                reads: Default::default(),
            };
            let export = device.store.sync_export();
            let local = export.local(&counting, &device.state);
            (counting.reads(), local)
        };
        let (reads, local) = read(&device);
        // The web host's password, never saved.
        assert_eq!(reads, 1);
        assert_eq!(local.unchanged.len(), 5);
        // Of the secrets, only the key file is read: no keychain asks.
        let read_secrets: Vec<Kind> = local
            .records
            .iter()
            .map(|record| record.data.kind())
            .filter(Kind::is_secret)
            .collect();
        assert_eq!(read_secrets, [Kind::SecretPrivateKey]);

        let bastion = SecretRef::password("root", "bastion.example", 22);
        let proxy = SecretRef::proxy("p", "proxy", 1080);
        device
            .store
            .save_secret(bastion.clone(), Some("pw-new".into()));
        device.store.save_secret(proxy.clone(), None);
        run_secret_jobs(&mut device.store);
        let (reads, local) = read(&device);
        assert_eq!(reads, 3);
        assert_eq!(local.unchanged.len(), 3);
        assert!(local.records.iter().any(|record| matches!(
            &record.data,
            RecordData::Password(endpoint) if endpoint.value.expose() == "pw-new"
        )));

        // The edits sync, and the round after reads nothing it has: the
        // deleted password is looked for like one never saved.
        device.sync(&vault, &keys);
        let (reads, local) = read(&device);
        assert_eq!(reads, 2);
        assert_eq!(local.unchanged.len(), 4);
    }

    /// A keychain that does not open.
    struct Locked;

    impl SecretStore for Locked {
        fn get(&self, _: &SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
            Err(anyhow::anyhow!("locked"))
        }
        fn set(&self, _: &SecretRef, _: &str) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("locked"))
        }
        fn delete(&self, _: &SecretRef) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("locked"))
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    /// Secrets a round brought that the keychain refused stay pending, a
    /// restart too, so no round takes them for deleted; they are written
    /// once it takes them.
    #[test]
    fn a_secret_the_keychain_refused_is_written_later() {
        let (vault, keys) = vault();
        let mut a = Device::new('a');
        let mut b = Device::new('b');
        fill(&mut a);
        a.sync(&vault, &keys);
        b.refuse_writes = true;
        b.sync(&vault, &keys);
        let pending = |b: &Device| b.store.sync_export().pending.len();
        // Every password and the passphrase; the key file went in.
        assert_eq!(pending(&b), 5);
        let reopened =
            HostData::load(HostDatabase::open(&b.data.path().join("shellrs.db")).unwrap()).unwrap();
        assert_eq!(reopened.sync_export().pending.len(), 5);
        assert_eq!(b.sync(&vault, &keys), 5);
        assert_eq!(pending(&b), 5);

        b.refuse_writes = false;
        b.sync(&vault, &keys);
        assert_eq!(pending(&b), 0);
        a.sync(&vault, &keys);
        assert_eq!(b.held(), a.held());
        assert_eq!(a.sync(&vault, &keys), 0);
        assert_eq!(b.sync(&vault, &keys), 0);
    }

    /// The user's own edit of a pending secret is theirs: no longer the
    /// round's to write.
    #[test]
    fn an_edit_of_a_pending_secret_is_the_users() {
        let (vault, keys) = vault();
        let mut a = Device::new('a');
        let mut b = Device::new('b');
        fill(&mut a);
        a.sync(&vault, &keys);
        b.refuse_writes = true;
        b.sync(&vault, &keys);
        b.refuse_writes = false;
        b.store.save_secret(
            SecretRef::password("root", "bastion.example", 22),
            Some("pw-mine".into()),
        );
        run_secret_jobs(&mut b.store);
        assert_eq!(b.store.sync_export().pending.len(), 4);
        b.sync(&vault, &keys);
        a.sync(&vault, &keys);
        assert_eq!(b.held(), a.held());
        let mine = a
            .secrets
            .get(&SecretRef::password("root", "bastion.example", 22))
            .unwrap();
        assert_eq!(mine.as_deref().map(String::as_str), Some("pw-mine"));
    }

    #[test]
    fn a_locked_keychain_gives_no_secrets_rather_than_none() {
        let mut device = Device::new('a');
        fill(&mut device);
        let export = device.store.sync_export();
        assert!(!export.secrets.is_empty());
        let read = read_secrets(&export.secrets, &[], &nothing_seen(), &Locked);
        assert!(!read.secrets_read);
        assert!(read.records.is_empty());

        let read = read_secrets(&export.secrets, &[], &nothing_seen(), &*device.secrets);
        assert!(read.secrets_read);
        // All but the web host's own password, never saved: it signs in
        // with a key.
        assert_eq!(read.records.len(), export.secrets.len() - 1);
    }
}
