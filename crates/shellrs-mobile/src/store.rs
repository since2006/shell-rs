//! The saved hosts, groups and credentials: the hosts core's data
//! (`HostData`), held in memory and written through to the database in the
//! data folder, the desktop's own format.
//!
//! Each change is in memory at once and written to the database there and
//! then; what could not be kept comes as [`StoreEvent::Failed`], the change
//! standing all the same, as on the desktop. Passwords and passphrases go
//! to the app's keychain on the core's threads, never on the caller's.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use shellrs_hosts::{
    self as hosts, Change, CredentialId, GroupId, HostData, HostEvent, HostId, SecretJob,
};
use shellrs_ssh as ssh;
use tokio::runtime::Handle;
use zeroize::Zeroizing;

use crate::events::Events;
use crate::hosts::{
    Credential, CredentialDraft, CredentialKind, CredentialProblem, Dependents, Group, GroupDraft,
    Host, HostDraft, HostProblem, KeyAlgorithm, KeyChange, KeyProblem, NewKey, SecretChange,
    TreeDrop, TreeNode,
};
use crate::secrets::SecretRef;
use crate::task::{self, CallError};

/// What the store says, in order.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum StoreEvent {
    /// What the lists show has changed, here or through sync: read them
    /// again. Only the newest waits to be taken.
    Changed,
    /// Something could not be kept. The change stands in memory.
    Failed { failure: StoreFailure },
}

/// What could not be kept.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum StoreFailure {
    /// The database did not take `change`, in its own words.
    NotSaved { change: ChangeKind, detail: String },
    /// A key file nothing uses any more is still there.
    KeyNotDeleted { detail: String },
    /// The keychain did not save `secret`, or with `writing` false did not
    /// delete it.
    Secret {
        secret: SecretRef,
        writing: bool,
        problem: SecretProblem,
    },
    /// A secret sync brought is not kept; the next round brings it again.
    SyncedSecret { problem: SecretProblem },
}

/// Which change the database did not take.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ChangeKind {
    NewHost,
    SaveHost,
    DeleteHost,
    DuplicateHost,
    HostOrder,
    NewGroup,
    SaveGroup,
    DeleteGroup,
    GroupExpanded,
    MoveInTree,
    NewCredential,
    SaveCredential,
    DeleteCredential,
    /// The list of recent connections.
    Recent,
    /// What the app does not edit, such as the desktop's bookmarks and
    /// snippets, which sync brings along.
    Other,
}

/// Why a secret is not kept.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SecretProblem {
    /// There is no keychain now ([`crate::KeychainError::Unavailable`]).
    Unavailable,
    /// The keychain's own failure, in its words.
    Keychain { detail: String },
    /// A private key's file could not be written.
    KeyFile { detail: String },
}

/// Why a call to the store failed: nothing changed.
#[derive(Debug, uniffi::Error)]
pub enum StoreError {
    /// No such host, group or credential, or no longer.
    NotFound,
    Host {
        problem: HostProblem,
    },
    Credential {
        problem: CredentialProblem,
    },
    /// The private key as pasted.
    Key {
        problem: KeyProblem,
    },
    /// A group needs a name.
    GroupName,
    /// The group a host or a group goes in is gone.
    GroupNotFound,
    /// A group cannot go into itself or a group in it.
    GroupIntoItself,
    /// The credential a host logs in with is gone.
    CredentialNotFound,
    /// A jump host is gone, or is no SSH host.
    JumpHostNotFound,
    /// A host cannot go through itself.
    JumpThroughItself,
    /// An SSH host other hosts go through, or port forwards go via, stays
    /// an SSH host.
    HasDependents,
    /// Only SSH hosts connect here.
    NotSsh,
    /// The private key's file could not be written, in the system's words.
    KeyNotSaved {
        detail: String,
    },
    /// The keychain did not read a password the change needed.
    Secret {
        problem: SecretProblem,
    },
    /// The core is gone.
    Closed,
    /// What should never happen: a bug in ShellRS.
    Internal {
        detail: String,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host { problem } => write!(f, "host: {problem:?}"),
            Self::Credential { problem } => write!(f, "credential: {problem:?}"),
            Self::Key { problem } => write!(f, "key: {problem}"),
            Self::KeyNotSaved { detail } => write!(f, "key not saved: {detail}"),
            Self::Secret { problem } => write!(f, "keychain: {problem:?}"),
            Self::Internal { detail } => write!(f, "internal error: {detail}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl CallError for StoreError {
    fn closed() -> Self {
        Self::Closed
    }

    fn internal(detail: String) -> Self {
        Self::Internal { detail }
    }
}

/// Which saved secret [`HostStore::saved_secret`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SavedSecret {
    /// A host's own password.
    HostPassword { host: u64 },
    /// The password of a host's proxy.
    ProxyPassword { host: u64 },
    /// A password credential's.
    CredentialPassword { credential: u64 },
    /// A key credential's passphrase.
    Passphrase { credential: u64 },
}

/// The data, and what its changes are passed on with.
pub(crate) struct Shared {
    data: Mutex<HostData>,
    events: Events<StoreEvent>,
    pub(crate) runtime: Handle,
    /// Set, with the data emptied, when the core goes.
    closed: AtomicBool,
}

impl Shared {
    pub(crate) fn new(data: HostData, runtime: Handle) -> Arc<Self> {
        Arc::new(Self {
            data: Mutex::new(data),
            events: Events::new(),
            runtime,
            closed: AtomicBool::new(false),
        })
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, HostData> {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run `f` with the data, then pass on what it did.
    pub(crate) fn with_data<R>(
        self: &Arc<Self>,
        f: impl FnOnce(&mut HostData) -> R,
    ) -> Result<R, StoreError> {
        let mut data = self.lock();
        if self.closed.load(Ordering::SeqCst) {
            return Err(StoreError::Closed);
        }
        let result = f(&mut data);
        self.settle(&mut data);
        Ok(result)
    }

    /// The keychain writes the data queued go to the core's threads, and
    /// what it did to the app's events.
    pub(crate) fn settle(self: &Arc<Self>, data: &mut HostData) {
        let jobs = data.take_secret_jobs();
        if !jobs.is_empty() {
            let shared = self.clone();
            self.runtime
                .spawn(async move { shared.run_secret_jobs(jobs).await });
        }
        self.pass_events(data);
    }

    /// Tell the app what could not be kept.
    pub(crate) fn report(&self, failure: StoreFailure) {
        self.events.push(StoreEvent::Failed { failure });
    }

    fn pass_events(&self, data: &mut HostData) {
        for event in data.take_events() {
            match event {
                HostEvent::Failed(failure) => self.report((&failure).into()),
                HostEvent::Changed => self.events.push_replacing(StoreEvent::Changed, |event| {
                    matches!(event, StoreEvent::Changed)
                }),
                // The app runs no port forwards.
                HostEvent::ForwardSettingsChanged(_) => {}
            }
        }
    }

    /// Write `jobs` to the keychain in order, then tell the data how they
    /// went: a secret saved is dated for sync only once it is in.
    pub(crate) async fn run_secret_jobs(self: &Arc<Self>, jobs: Vec<SecretJob>) {
        let outcomes = task::blocking(&self.runtime, move || {
            jobs.into_iter().map(SecretJob::run).collect::<Vec<_>>()
        })
        .await;
        let _ = self.with_data(|data| {
            for outcome in outcomes {
                data.secret_done(outcome);
            }
        });
    }

    /// The core is going: the database closes, and the app hears nothing
    /// more.
    pub(crate) fn close(&self) {
        let mut data = self.lock();
        self.closed.store(true, Ordering::SeqCst);
        *data = HostData::empty();
        self.events.finish(None);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// The saved hosts, groups and credentials, from [`crate::Core::store`].
/// Reading is quick. So is changing, but for saving a host or a credential,
/// which may read the keychain or encrypt a key and so is async.
#[derive(uniffi::Object)]
pub struct HostStore {
    pub(crate) shared: Arc<Shared>,
}

#[uniffi::export]
impl HostStore {
    /// The next event; `None` once the core is gone.
    pub async fn next_event(&self) -> Option<StoreEvent> {
        self.shared.events.next().await
    }

    /// Every host, each group's in the tree's order. Serial hosts stay on
    /// the computer they were made on and are not among them.
    pub fn hosts(&self) -> Vec<Host> {
        let data = self.shared.lock();
        let mut hosts: Vec<_> = data.hosts().iter().filter(|h| !h.is_serial()).collect();
        hosts.sort_by_key(|host| (host.sort_order, host.id));
        hosts.into_iter().map(Host::of).collect()
    }

    pub fn host(&self, id: u64) -> Option<Host> {
        saved_host(&self.shared.lock(), id).map(Host::of)
    }

    /// The hosts connected to lately, the latest first.
    pub fn recent_hosts(&self) -> Vec<Host> {
        let data = self.shared.lock();
        data.recent_hosts()
            .filter(|host| !host.is_serial())
            .map(Host::of)
            .collect()
    }

    /// Every group, each group's in the tree's order.
    pub fn groups(&self) -> Vec<Group> {
        let data = self.shared.lock();
        let mut groups: Vec<_> = data.groups().iter().collect();
        groups.sort_by_key(|group| (group.sort_order, group.id));
        groups.into_iter().map(Group::of).collect()
    }

    /// Every credential, in the list's order.
    pub fn credentials(&self) -> Vec<Credential> {
        let data = self.shared.lock();
        let mut credentials: Vec<_> = data.credentials().iter().collect();
        credentials.sort_by_key(|credential| (credential.sort_order, credential.id));
        credentials.into_iter().map(Credential::of).collect()
    }

    pub fn credential(&self, id: u64) -> Option<Credential> {
        self.shared
            .lock()
            .credential(CredentialId(id))
            .map(Credential::of)
    }

    /// The hosts logging in with `credential`: deleting it has them log in
    /// on their own.
    pub fn hosts_using(&self, credential: u64) -> Vec<u64> {
        self.shared
            .lock()
            .hosts_using(CredentialId(credential))
            .map(|host| host.id.0)
            .collect()
    }

    /// What deleting `node` takes with it, to say so before; nothing for
    /// one not there.
    pub fn dependents(&self, node: TreeNode) -> Dependents {
        let data = self.shared.lock();
        let (doomed, hosts, subgroups) = match node {
            TreeNode::Host { id } => match saved_host(&data, id) {
                Some(host) => (vec![host.id], 0, 0),
                None => return Dependents::default(),
            },
            TreeNode::Group { id } => {
                let id = GroupId(id);
                if data.group(id).is_none() {
                    return Dependents::default();
                }
                let doomed = data.hosts_under(id);
                let hosts = doomed.len();
                (doomed, hosts, data.descendant_groups(id).len())
            }
        };
        let forwards = doomed.iter().map(|id| data.forwards_of(*id).count()).sum();
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        Dependents {
            hosts: count(hosts),
            subgroups: count(subgroups),
            forwards: count(forwards),
            jump_users: count(data.jump_users(&doomed)),
        }
    }

    /// Save a new host, and its password and its proxy's as they say.
    pub async fn add_host(
        &self,
        draft: HostDraft,
        password: SecretChange,
        proxy_password: SecretChange,
    ) -> Result<u64, StoreError> {
        let shared = self.shared.clone();
        let (password, proxy_password) = (password.into(), proxy_password.into());
        task::run(&self.shared.runtime, async move {
            save_host(&shared, None, draft, password, proxy_password).await
        })
        .await
        .map(|id| id.0)
    }

    /// Change a host. What is connected stays connected as it was: the next
    /// connection logs in anew.
    pub async fn update_host(
        &self,
        id: u64,
        draft: HostDraft,
        password: SecretChange,
        proxy_password: SecretChange,
    ) -> Result<(), StoreError> {
        let shared = self.shared.clone();
        let (password, proxy_password) = (password.into(), proxy_password.into());
        task::run(&self.shared.runtime, async move {
            save_host(&shared, Some(HostId(id)), draft, password, proxy_password).await
        })
        .await
        .map(|_| ())
    }

    /// Delete a host. The hosts that went through it keep its place among
    /// their jump hosts, empty.
    pub fn delete_host(&self, id: u64) -> Result<(), StoreError> {
        self.shared.with_data(|data| {
            saved_host(data, id).ok_or(StoreError::NotFound)?;
            data.remove(HostId(id));
            Ok(())
        })?
    }

    /// A copy of a host, named `name`, just after it.
    pub fn duplicate_host(&self, id: u64, name: String) -> Result<u64, StoreError> {
        self.shared.with_data(|data| {
            saved_host(data, id).ok_or(StoreError::NotFound)?;
            data.duplicate(HostId(id), name.into())
                .map(|copy| copy.0)
                .ok_or(StoreError::NotFound)
        })?
    }

    /// Move a host or a group in the tree; whether it moved.
    pub fn move_node(&self, node: TreeNode, drop: TreeDrop) -> bool {
        self.shared
            .with_data(|data| data.move_node(node.into(), drop.into()))
            .unwrap_or(false)
    }

    pub fn add_group(&self, draft: GroupDraft) -> Result<u64, StoreError> {
        self.shared.with_data(|data| {
            let draft = checked_group(data, None, draft)?;
            Ok(data.insert_group(draft).0)
        })?
    }

    pub fn update_group(&self, id: u64, draft: GroupDraft) -> Result<(), StoreError> {
        self.shared.with_data(|data| {
            let id = GroupId(id);
            let draft = checked_group(data, Some(id), draft)?;
            match data.update_group(id, draft) {
                true => Ok(()),
                false => Err(StoreError::NotFound),
            }
        })?
    }

    /// Delete a group with every group and host in it; the hosts deleted.
    pub fn delete_group(&self, id: u64) -> Result<Vec<u64>, StoreError> {
        self.shared.with_data(|data| {
            let id = GroupId(id);
            data.group(id).ok_or(StoreError::NotFound)?;
            Ok(data
                .remove_group(id)
                .into_iter()
                .map(|host| host.0)
                .collect())
        })?
    }

    pub fn set_group_expanded(&self, id: u64, expanded: bool) {
        let _ = self
            .shared
            .with_data(|data| data.set_group_expanded(GroupId(id), expanded));
    }

    /// Save a new credential, with its key and its password as they say.
    pub async fn add_credential(
        &self,
        draft: CredentialDraft,
        key: KeyChange,
        password: SecretChange,
    ) -> Result<u64, StoreError> {
        let shared = self.shared.clone();
        let (key, password) = (NewSecrets::of(key), password.into());
        task::run(&self.shared.runtime, async move {
            save_credential(&shared, None, draft, key, password).await
        })
        .await
        .map(|id| id.0)
    }

    /// Change a credential. Every host using it takes its user; what is
    /// connected stays connected as it was.
    pub async fn update_credential(
        &self,
        id: u64,
        draft: CredentialDraft,
        key: KeyChange,
        password: SecretChange,
    ) -> Result<(), StoreError> {
        let shared = self.shared.clone();
        let (key, password) = (NewSecrets::of(key), password.into());
        task::run(&self.shared.runtime, async move {
            save_credential(&shared, Some(CredentialId(id)), draft, key, password).await
        })
        .await
        .map(|_| ())
    }

    /// Delete a credential. The hosts using it log in on their own again,
    /// as the same user, and are returned.
    pub fn delete_credential(&self, id: u64) -> Result<Vec<u64>, StoreError> {
        self.shared.with_data(|data| {
            let id = CredentialId(id);
            data.credential(id).ok_or(StoreError::NotFound)?;
            Ok(data
                .remove_credential(id)
                .into_iter()
                .map(|host| host.0)
                .collect())
        })?
    }

    /// A new key pair, for a key credential: an RSA key takes a while.
    pub async fn generate_key(&self, algorithm: KeyAlgorithm) -> Result<Arc<NewKey>, StoreError> {
        let runtime = self.shared.runtime.clone();
        task::run(&self.shared.runtime, async move {
            task::blocking(&runtime, move || {
                hosts::GeneratedKey::generate(algorithm.into())
                    .map(|key| Arc::new(NewKey { key }))
                    .map_err(|error| StoreError::Internal {
                        detail: format!("{error:#}"),
                    })
            })
            .await
        })
        .await
    }

    /// A key credential's public key, as a line of `authorized_keys`;
    /// `None` when it cannot be told without the passphrase.
    pub async fn public_key(&self, credential: u64) -> Option<String> {
        let path = self
            .shared
            .lock()
            .credential(CredentialId(credential))
            .and_then(|credential| credential.key_path.clone())?;
        let runtime = self.shared.runtime.clone();
        task::run::<_, StoreError>(&self.shared.runtime, async move {
            Ok(task::blocking(&runtime, move || {
                hosts::read_public_key(std::path::Path::new(path.as_ref()))
            })
            .await)
        })
        .await
        .ok()
        .flatten()
    }

    /// What is saved as `secret`, to show it or fill a form with it; `None`
    /// for none, or for a host or credential without such a secret.
    pub async fn saved_secret(&self, secret: SavedSecret) -> Result<Option<String>, StoreError> {
        let (secrets, secret) = {
            let data = self.shared.lock();
            (data.secrets(), secret_ref(&data, secret))
        };
        let Some(secret) = secret else {
            return Ok(None);
        };
        let runtime = self.shared.runtime.clone();
        task::run(&self.shared.runtime, async move {
            task::blocking(&runtime, move || secrets.get(&secret))
                .await
                .map(|value| value.map(|value| value.to_string()))
                .map_err(|error| StoreError::Secret {
                    problem: (&hosts::SecretError::of(&error)).into(),
                })
        })
        .await
    }
}

/// A saved host the app sees: not a serial one.
fn saved_host(data: &HostData, id: u64) -> Option<&hosts::Host> {
    data.saved_host(HostId(id)).filter(|host| !host.is_serial())
}

/// A password or passphrase as saving changes it, kept out of sight.
pub(crate) enum NewSecret {
    Keep,
    Clear,
    Set(Zeroizing<String>),
}

impl From<SecretChange> for NewSecret {
    fn from(change: SecretChange) -> Self {
        match change {
            SecretChange::Keep => Self::Keep,
            SecretChange::Clear => Self::Clear,
            SecretChange::Set { value } => Self::Set(Zeroizing::new(value)),
        }
    }
}

impl NewSecret {
    /// A secret given, or none: an empty one is none.
    fn given(value: Option<Zeroizing<String>>) -> Self {
        match value.filter(|value| !value.is_empty()) {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        }
    }

    /// Queue the change of `secret` with the data.
    fn save(self, data: &mut HostData, secret: ssh::SecretRef) {
        match self {
            Self::Keep => {}
            Self::Clear => data.save_secret(secret, None),
            Self::Set(value) => data.save_secret(secret, Some(value.to_string())),
        }
    }
}

/// A key credential's key as saving changes it.
enum NewSecrets {
    Keep {
        passphrase: NewSecret,
    },
    Text {
        text: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
    },
    Generated {
        key: Arc<NewKey>,
        passphrase: Option<Zeroizing<String>>,
    },
}

impl NewSecrets {
    fn of(change: KeyChange) -> Self {
        match change {
            KeyChange::Keep { passphrase } => Self::Keep {
                passphrase: passphrase.into(),
            },
            KeyChange::Text { text, passphrase } => Self::Text {
                text: Zeroizing::new(text),
                passphrase: passphrase.map(Zeroizing::new),
            },
            KeyChange::Generated { key, passphrase } => Self::Generated {
                key,
                passphrase: passphrase.map(Zeroizing::new),
            },
        }
    }
}

/// Save a host, new or not, then its secrets. A password left as it is
/// moves with the host to the address, port or user it moves to, as the
/// desktop's form, which fills it in, does.
async fn save_host(
    shared: &Arc<Shared>,
    id: Option<HostId>,
    draft: HostDraft,
    password: NewSecret,
    proxy_password: NewSecret,
) -> Result<HostId, StoreError> {
    let draft = draft.core();
    // The passwords left as they are whose host moves away from them.
    let (secrets, moves) = {
        let data = shared.lock();
        let new = checked_host(&data, id, draft.clone())?;
        let old = id.and_then(|id| data.saved_host(id));
        let moved = |change: &NewSecret, from, to| match (change, from, to) {
            (NewSecret::Keep, Some(from), Some(to)) if from != to => Some(from),
            _ => None,
        };
        let moves = [
            moved(&password, old.and_then(own_password), own_password_of(&new)),
            moved(
                &proxy_password,
                old.and_then(|host| proxy_secret(&host.route)),
                proxy_secret(&new.route),
            ),
        ];
        (data.secrets(), moves)
    };
    let [password, proxy_password] = if moves.iter().any(Option::is_some) {
        let runtime = shared.runtime.clone();
        let read = task::blocking(&runtime, move || {
            let read = |from: Option<ssh::SecretRef>| {
                from.map(|from| secrets.get(&from))
                    .transpose()
                    .map(Option::flatten)
            };
            let [password, proxy] = moves;
            anyhow::Ok([read(password)?, read(proxy)?])
        })
        .await
        .map_err(|error| StoreError::Secret {
            problem: (&hosts::SecretError::of(&error)).into(),
        })?;
        let carry = |change, read: Option<Zeroizing<String>>| match (change, read) {
            (NewSecret::Keep, Some(value)) => NewSecret::Set(value),
            (change, _) => change,
        };
        let [read_password, read_proxy] = read;
        [
            carry(password, read_password),
            carry(proxy_password, read_proxy),
        ]
    } else {
        [password, proxy_password]
    };
    let (id, jobs) = {
        let mut data = shared.lock();
        if shared.is_closed() {
            return Err(StoreError::Closed);
        }
        // Checked again: the data may have changed meanwhile.
        let draft = checked_host(&data, id, draft)?;
        let (own, proxy) = (own_password_of(&draft), proxy_secret(&draft.route));
        let id = match id {
            Some(id) => match data.update(id, draft) {
                true => id,
                false => return Err(StoreError::NotFound),
            },
            None => data.insert(draft),
        };
        // After `update`, which drops the password of the endpoint left.
        if let Some(secret) = own {
            password.save(&mut data, secret);
        }
        if let Some(secret) = proxy {
            proxy_password.save(&mut data, secret);
        }
        let jobs = data.take_secret_jobs();
        shared.settle(&mut data);
        (id, jobs)
    };
    shared.run_secret_jobs(jobs).await;
    Ok(id)
}

/// `draft` checked against the data, for host `id` or a new one.
fn checked_host(
    data: &HostData,
    id: Option<HostId>,
    draft: hosts::HostDraft,
) -> Result<hosts::HostDraft, StoreError> {
    let old = match id {
        Some(id) => Some(saved_host(data, id.0).ok_or(StoreError::NotFound)?),
        None => None,
    };
    let draft = draft.validated().map_err(|error| StoreError::Host {
        problem: error.into(),
    })?;
    if draft.group.is_some_and(|group| data.group(group).is_none()) {
        return Err(StoreError::GroupNotFound);
    }
    if draft
        .credential
        .is_some_and(|credential| data.credential(credential).is_none())
    {
        return Err(StoreError::CredentialNotFound);
    }
    for hop in draft.route.jump_hosts() {
        if Some(hop) == id {
            return Err(StoreError::JumpThroughItself);
        }
        if data.saved_ssh_host(hop).is_none() {
            return Err(StoreError::JumpHostNotFound);
        }
    }
    if let Some(old) = old
        && old.is_ssh()
        && !draft.protocol.is_ssh()
        && data.ssh_dependents(old.id) != (0, 0)
    {
        return Err(StoreError::HasDependents);
    }
    Ok(draft)
}

/// Where a host keeps its own password: not when it logs in with a
/// credential or with none.
fn own_password(host: &hosts::Host) -> Option<ssh::SecretRef> {
    (host.credential.is_none() && host.auth == hosts::AuthKind::Password)
        .then(|| host.password_secret())
}

fn own_password_of(draft: &hosts::HostDraft) -> Option<ssh::SecretRef> {
    (draft.credential.is_none() && draft.auth == hosts::AuthKind::Password)
        .then(|| draft.password_secret())
}

fn proxy_secret(route: &hosts::Route) -> Option<ssh::SecretRef> {
    match route {
        hosts::Route::Proxy(proxy) => proxy.password_secret(),
        _ => None,
    }
}

fn checked_group(
    data: &HostData,
    id: Option<GroupId>,
    draft: GroupDraft,
) -> Result<hosts::GroupDraft, StoreError> {
    if id.is_some_and(|id| data.group(id).is_none()) {
        return Err(StoreError::NotFound);
    }
    let name = draft.name.trim();
    if name.is_empty() {
        return Err(StoreError::GroupName);
    }
    let parent = draft.parent.map(GroupId);
    if let Some(parent) = parent {
        if data.group(parent).is_none() {
            return Err(StoreError::GroupNotFound);
        }
        if let Some(id) = id
            && (parent == id || data.descendant_groups(id).contains(&parent))
        {
            return Err(StoreError::GroupIntoItself);
        }
    }
    Ok(hosts::GroupDraft::new(name.to_owned(), parent))
}

/// Save a credential, new or not: its key's file first, then the
/// credential, then its secrets.
async fn save_credential(
    shared: &Arc<Shared>,
    id: Option<CredentialId>,
    draft: CredentialDraft,
    key: NewSecrets,
    password: NewSecret,
) -> Result<CredentialId, StoreError> {
    let kind: hosts::CredentialKind = draft.kind.into();
    let is_key = draft.kind == CredentialKind::Key;
    // The key's text and its passphrase, a generated key's encrypted with
    // it, which takes a while on purpose.
    let (text, passphrase) = match key {
        _ if !is_key => (None, NewSecret::Keep),
        NewSecrets::Keep { passphrase } => (None, passphrase),
        NewSecrets::Text { text, passphrase } => {
            let key = hosts::PastedKey::parse(&text).map_err(|error| StoreError::Key {
                problem: error.into(),
            })?;
            // Only an encrypted key has a passphrase to keep.
            let passphrase = passphrase.filter(|_| key.is_encrypted());
            (
                Some(Zeroizing::new(key.text().to_owned())),
                NewSecret::given(passphrase),
            )
        }
        NewSecrets::Generated { key, passphrase } => {
            let comment = draft.name.trim().to_owned();
            let secret = passphrase.clone().unwrap_or_default();
            let runtime = shared.runtime.clone();
            let text = task::blocking(&runtime, move || key.key.encode(&comment, &secret))
                .await
                .map_err(|error| StoreError::Internal {
                    detail: format!("{error:#}"),
                })?;
            (Some(text), NewSecret::given(passphrase))
        }
    };
    let (id, jobs) = {
        let mut data = shared.lock();
        if shared.is_closed() {
            return Err(StoreError::Closed);
        }
        let existing = match id {
            Some(id) => Some(data.credential(id).ok_or(StoreError::NotFound)?.clone()),
            None => None,
        };
        let key_path = match (&text, &existing) {
            // A stand-in, never kept: the file is written once the rest
            // is known to be right.
            (Some(_), _) => Some("(new key)".into()),
            (None, Some(existing)) if is_key => existing.key_path.clone(),
            (None, _) => None,
        };
        let mut checked = hosts::CredentialDraft::new(draft.name, kind, draft.user);
        if let Some(path) = key_path {
            checked = checked.with_key_path(path);
        }
        let mut checked = checked
            .validated()
            .map_err(|error| StoreError::Credential {
                problem: error.into(),
            })?;
        if let Some(text) = &text {
            let path =
                data.save_private_key(id, text)
                    .map_err(|error| StoreError::KeyNotSaved {
                        detail: error.to_string(),
                    })?;
            checked.key_path = Some(path);
        }
        let key_path = checked.key_path.clone();
        let id = match id {
            Some(id) => match data.update_credential(id, checked) {
                true => id,
                false => return Err(StoreError::NotFound),
            },
            None => data.insert_credential(checked),
        };
        if kind == hosts::CredentialKind::Password
            && let Some(credential) = data.credential(id)
        {
            let secret = credential.password_secret();
            password.save(&mut data, secret);
        }
        if let Some(path) = key_path {
            passphrase.save(&mut data, ssh::SecretRef::passphrase(path.as_ref()));
        }
        let jobs = data.take_secret_jobs();
        shared.settle(&mut data);
        (id, jobs)
    };
    shared.run_secret_jobs(jobs).await;
    Ok(id)
}

/// Where `secret` is kept, if the host or credential has one such.
fn secret_ref(data: &HostData, secret: SavedSecret) -> Option<ssh::SecretRef> {
    match secret {
        SavedSecret::HostPassword { host } => saved_host(data, host).and_then(own_password),
        SavedSecret::ProxyPassword { host } => {
            saved_host(data, host).and_then(|host| proxy_secret(&host.route))
        }
        SavedSecret::CredentialPassword { credential } => data
            .credential(CredentialId(credential))
            .filter(|credential| credential.kind == hosts::CredentialKind::Password)
            .map(|credential| credential.password_secret()),
        SavedSecret::Passphrase { credential } => data
            .credential(CredentialId(credential))
            .and_then(|credential| credential.key_path.as_deref())
            .map(ssh::SecretRef::passphrase),
    }
}

impl From<&hosts::Failure> for StoreFailure {
    fn from(failure: &hosts::Failure) -> Self {
        match failure {
            hosts::Failure::NotSaved { change, error } => Self::NotSaved {
                change: (*change).into(),
                detail: error.clone(),
            },
            hosts::Failure::KeyNotDeleted { error } => Self::KeyNotDeleted {
                detail: error.clone(),
            },
            hosts::Failure::Secret {
                secret,
                writing,
                error,
            } => Self::Secret {
                secret: secret.into(),
                writing: *writing,
                problem: error.into(),
            },
            hosts::Failure::SyncedSecret(error) => Self::SyncedSecret {
                problem: error.into(),
            },
        }
    }
}

impl From<&hosts::SecretError> for SecretProblem {
    fn from(error: &hosts::SecretError) -> Self {
        match error {
            hosts::SecretError::Unavailable => Self::Unavailable,
            hosts::SecretError::Keychain(detail) => Self::Keychain {
                detail: detail.clone(),
            },
            hosts::SecretError::NoKeyDir => Self::KeyFile {
                detail: "there is no folder to keep private keys in".into(),
            },
            hosts::SecretError::KeyFile(detail) => Self::KeyFile {
                detail: detail.clone(),
            },
        }
    }
}

impl From<Change> for ChangeKind {
    fn from(change: Change) -> Self {
        match change {
            Change::NewHost => Self::NewHost,
            Change::SaveHost => Self::SaveHost,
            Change::DeleteHost => Self::DeleteHost,
            Change::DuplicateHost => Self::DuplicateHost,
            Change::HostOrder => Self::HostOrder,
            Change::NewGroup => Self::NewGroup,
            Change::SaveGroup => Self::SaveGroup,
            Change::DeleteGroup => Self::DeleteGroup,
            Change::GroupExpanded | Change::AllGroupsExpanded => Self::GroupExpanded,
            Change::MoveHostNode => Self::MoveInTree,
            Change::NewCredential => Self::NewCredential,
            Change::SaveCredential => Self::SaveCredential,
            Change::DeleteCredential => Self::DeleteCredential,
            Change::Recent => Self::Recent,
            Change::AddBookmark
            | Change::DeleteBookmark
            | Change::BookmarkOrder
            | Change::NewForward
            | Change::SaveForward
            | Change::DeleteForward
            | Change::HostOs
            | Change::SidebarTool
            | Change::NewSnippet
            | Change::SaveSnippet
            | Change::DeleteSnippet
            | Change::NewSnippetCategory
            | Change::RenameSnippetCategory
            | Change::DeleteSnippetCategory
            | Change::NewTerminalGroup
            | Change::SaveTerminalGroup
            | Change::DeleteTerminalGroup
            | Change::NewSavedTerminal
            | Change::SaveSavedTerminal
            | Change::DeleteSavedTerminal
            | Change::MoveTerminalNode
            | Change::SavedTerminalTabs
            | Change::Batch => Self::Other,
        }
    }
}
