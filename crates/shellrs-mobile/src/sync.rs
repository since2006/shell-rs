//! 同步: the hosts and the rest kept alike on the user's devices, through
//! a folder, a WebDAV server or an S3 bucket, as on the desktop and with
//! it. The hosts core runs each round; when to run one is the app's to say
//! (see the README).

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use shellrs_hosts::HostData;
use shellrs_hosts::secrets::{SecretStore, SharedSecretStore};
use shellrs_hosts::sync::{
    self as hosts_sync, Blocking, JobError, RoundEnd, RoundHost, SyncServices, run_round, worker,
};
use shellrs_sync::{
    DeviceId, JoinMode, RemoteDevice, RoundOptions, StoreError, SyncError, SyncState, SyncStore,
};
use tokio::sync::Mutex as Turn;
use zeroize::Zeroizing;

use crate::store::{SecretProblem, Shared};
use crate::task::{self, CallError};

/// Where sync goes through.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncLocation {
    /// A folder on the device, which a cloud drive may be syncing.
    Folder { path: String },
    /// A folder on a WebDAV server (坚果云, Nextcloud and the like). Its
    /// password is kept in the keychain.
    WebDav { url: String, username: String },
    /// A folder in an S3 bucket, or one of a service that speaks its API
    /// (R2, MinIO, OSS, COS and the like), which the user made. Its secret
    /// key is kept in the keychain.
    S3 {
        /// The service's address, such as
        /// `https://s3.us-east-1.amazonaws.com`.
        endpoint: String,
        region: String,
        bucket: String,
        /// The folder in the bucket; empty for its top.
        prefix: String,
        /// The bucket in the address's path rather than its host.
        path_style: bool,
        access_key: String,
    },
}

/// Where sync stands.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncStatus {
    /// `None` while sync is off.
    pub location: Option<SyncLocation>,
    /// This device's id in sync, once a round has gone through: the one
    /// to leave out of what [`SyncService::remove_device`] is offered.
    pub device_id: Option<String>,
    /// The other devices, as the last round this run found them.
    pub devices: Vec<SyncDevice>,
    /// A round or a job runs.
    pub busy: bool,
    /// Something changed since the last round that went through this run,
    /// or none has: worth a round before the app goes to the background.
    pub has_changes: bool,
}

/// A device in sync.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncDevice {
    pub id: String,
    /// What it calls itself, such as the computer's name.
    pub name: String,
    /// `macos`, `windows`, `linux`, `ios` or `android`.
    pub platform: String,
    pub app_version: String,
    /// When it last synced, by its own clock: milliseconds since the epoch.
    pub last_write_ms: u64,
}

/// How a round went, when it did not fail.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncOutcome {
    /// It went through: the store has what the others had (see its
    /// [`crate::StoreEvent::Changed`]), and they will have what it had.
    Synced { devices: Vec<SyncDevice> },
    /// Only the user can settle what stopped it. Leave automatic rounds
    /// until they have.
    Attention { attention: Attention },
}

/// What only the user can settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Attention {
    /// The sync password was changed on another device, or the keychain
    /// lost the keys: [`SyncService::join`] again with the new one, not
    /// fresh.
    NeedsPassword,
    /// Sync was reset on another device: [`SyncService::join`] it fresh, or
    /// turn sync off.
    VaultReplaced,
    /// This device was removed from sync on another one: sync with
    /// `accept_removed`, or turn sync off.
    Removed,
    /// The round would delete `count` of the `of` records synced: sync
    /// with `allow_mass_delete`, or leave sync waiting.
    MassDelete { count: u32, of: u32 },
    /// The WebDAV server or the bucket refused the login, or the keychain
    /// lost it: [`SyncService::change_login`].
    LoginRefused,
}

/// What is wrong with a sync password as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PasswordProblem {
    Missing,
    /// A new one shorter than [`min_sync_password_length`].
    TooShort,
    /// A new one typed differently the second time.
    Mismatch,
}

/// What is wrong with a login as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Error)]
pub enum LoginProblem {
    UrlInvalid,
    UsernameMissing,
    PasswordMissing,
    EndpointInvalid,
    BucketInvalid,
    AccessKeyMissing,
    SecretKeyMissing,
}

impl std::fmt::Display for LoginProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for LoginProblem {}

/// What the storage said.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum StorageProblem {
    /// No such file.
    NotFound,
    /// A file changed under a write.
    PreconditionFailed,
    TooLarge,
    /// It did not answer in time.
    Timeout,
    /// It refused the login.
    Unauthorized,
    /// It is unreachable or failed, for now, in its words.
    Unavailable {
        detail: String,
    },
    /// Anything else, such as a file the system would not write.
    Io {
        detail: String,
    },
}

/// Why a round or a job failed.
#[derive(Debug, uniffi::Error)]
pub enum SyncFailure {
    /// There is no keychain to keep sync's keys in.
    Unavailable,
    /// Sync is off.
    NotSetUp,
    /// A job runs: one at a time.
    InProgress,
    /// Every try met a change made meanwhile, here or on another device:
    /// the next round will do.
    Busy,
    /// The keychain has no sync keys, or keys it cannot read.
    NoKeyring,
    /// The keychain has no login for the server or the bucket.
    NoLogin,
    Keychain {
        problem: SecretProblem,
    },
    /// The database refused a round's changes, in its words: nothing was
    /// applied.
    NotSaved {
        detail: String,
    },
    /// The database refused the state a round left, in its words.
    StateNotSaved {
        detail: String,
    },
    Storage {
        problem: StorageProblem,
    },
    /// Sync is not set up there: never, or it was reset.
    VaultMissing,
    /// Sync is set up there already, maybe just now by another device.
    VaultExists,
    /// A newer ShellRS set sync up there: update the app.
    VaultTooNew {
        version: u32,
    },
    /// What is there is not sync's, or is damaged.
    BadVault {
        detail: String,
    },
    WrongPassword,
    NeedsPassword,
    VaultReplaced,
    /// What the last round left cannot be read back.
    StateUnreadable {
        detail: String,
    },
    MassDelete {
        count: u32,
        of: u32,
    },
    /// Another device writes a newer format: this one reads but must not
    /// write until the app is updated.
    ReadOnly {
        min_reader: u32,
    },
    /// The system's random source failed.
    Random,
    Password {
        problem: PasswordProblem,
    },
    /// [`SyncService::change_login`] for a place sync does not go
    /// through.
    OtherPlace,
    /// [`SyncService::remove_device`] for no device id.
    NoSuchDevice,
    /// The job went through, but the app will not remember where sync
    /// goes when it starts again: the file did not take it, in the
    /// system's words.
    LocationNotSaved {
        detail: String,
    },
    /// The core is gone.
    Closed,
    /// What should never happen: a bug in ShellRS.
    Internal {
        detail: String,
    },
}

impl std::fmt::Display for SyncFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for SyncFailure {}

impl CallError for SyncFailure {
    fn closed() -> Self {
        Self::Closed
    }

    fn internal(detail: String) -> Self {
        Self::Internal { detail }
    }
}

/// A login of a WebDAV server or an S3 bucket, checked: what
/// [`SyncService::create`] and the rest take. It carries the password or
/// the secret key, so it never prints.
#[derive(uniffi::Record)]
pub struct CheckedLogin {
    pub location: SyncLocation,
    pub login: String,
}

/// An S3 bucket's fields as typed.
#[derive(uniffi::Record)]
pub struct S3Login {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub path_style: bool,
    pub access_key: String,
    pub secret_key: String,
}

/// The shortest new sync password, in characters.
#[uniffi::export]
pub fn min_sync_password_length() -> u32 {
    hosts_sync::MIN_PASSWORD as u32
}

/// What is wrong with a new sync password, typed twice; `None` for
/// nothing.
#[uniffi::export]
pub fn check_new_sync_password(password: String, repeat: String) -> Option<PasswordProblem> {
    hosts_sync::check_new_password(&password, &repeat)
        .err()
        .map(Into::into)
}

/// What is wrong with the password of a sync there already; `None` for
/// nothing.
#[uniffi::export]
pub fn check_sync_password(password: String) -> Option<PasswordProblem> {
    hosts_sync::check_password(&password).err().map(Into::into)
}

/// A strong sync password to offer: twenty letters and digits that cannot
/// be mistaken for one another, in groups of five.
#[uniffi::export]
pub fn random_sync_password() -> String {
    hosts_sync::random_password().to_string()
}

/// A WebDAV server's fields as typed, checked. The password is taken as
/// typed, spaces and all.
#[uniffi::export]
pub fn check_webdav_login(
    url: String,
    username: String,
    password: String,
) -> Result<CheckedLogin, LoginProblem> {
    let password = Zeroizing::new(password);
    hosts_sync::WebDavForm {
        url: &url,
        username: &username,
        password: &password,
    }
    .login()
    .map(CheckedLogin::of)
    .map_err(Into::into)
}

/// An S3 bucket's fields as typed, checked.
#[uniffi::export]
pub fn check_s3_login(login: S3Login) -> Result<CheckedLogin, LoginProblem> {
    let secret_key = Zeroizing::new(login.secret_key);
    hosts_sync::S3Form {
        endpoint: &login.endpoint,
        region: &login.region,
        bucket: &login.bucket,
        prefix: &login.prefix,
        path_style: login.path_style,
        access_key: &login.access_key,
        secret_key: &secret_key,
    }
    .login()
    .map(CheckedLogin::of)
    .map_err(Into::into)
}

impl CheckedLogin {
    fn of((location, login): (hosts_sync::SyncLocation, Zeroizing<String>)) -> Self {
        Self {
            location: location.into(),
            login: login.to_string(),
        }
    }
}

/// Sync, from [`crate::Core::sync`]. One round or job runs at a time: a
/// round waits for the one before, a job fails with
/// [`SyncFailure::InProgress`].
#[derive(uniffi::Object)]
pub struct SyncService {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    store: Arc<Shared>,
    services: SyncServices,
    /// Where the location is kept, beside the database.
    location_file: PathBuf,
    memory: Mutex<Memory>,
    turn: Arc<Turn<()>>,
}

/// What sync knows this run.
struct Memory {
    location: Option<hosts_sync::SyncLocation>,
    devices: Vec<RemoteDevice>,
    /// The store's revision the last round that went through had read.
    synced_revision: Option<u64>,
}

impl Inner {
    /// Sync over `store`, at the location kept in `location_file`.
    pub(crate) fn new(store: Arc<Shared>, services: SyncServices, location_file: PathBuf) -> Self {
        // One unreadable is sync off: the user sets it up again.
        let location = std::fs::read(&location_file)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self {
            store,
            services,
            location_file,
            memory: Mutex::new(Memory {
                location,
                devices: Vec::new(),
                synced_revision: None,
            }),
            turn: Arc::new(Turn::new(())),
        }
    }

    fn memory(&self) -> MutexGuard<'_, Memory> {
        self.memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn location(&self) -> Result<hosts_sync::SyncLocation, SyncFailure> {
        self.memory().location.clone().ok_or(SyncFailure::NotSetUp)
    }

    /// The keychain, if there is one to sync with.
    fn secrets(&self) -> Result<SharedSecretStore, SyncFailure> {
        let secrets = self.store.with_data(|data| data.secrets())?;
        match secrets.is_available() {
            true => Ok(secrets),
            false => Err(SyncFailure::Unavailable),
        }
    }

    /// Keep where sync goes through, or with `None` that it is off.
    fn keep_location(&self, location: Option<hosts_sync::SyncLocation>) -> Result<(), SyncFailure> {
        let kept = match &location {
            Some(location) => serde_json::to_vec_pretty(location)
                .map_err(std::io::Error::other)
                .and_then(|bytes| write_replacing(&self.location_file, &bytes)),
            None => match std::fs::remove_file(&self.location_file) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            },
        };
        self.memory().location = location;
        kept.map_err(|error| SyncFailure::LocationNotSaved {
            detail: error.to_string(),
        })
    }

    /// Sync goes through `location` now: set up, joined, or signed in to
    /// anew. `fresh` forgets what this device synced before.
    fn joined(&self, location: hosts_sync::SyncLocation, fresh: bool) -> Result<(), SyncFailure> {
        if fresh {
            self.store.with_data(HostData::clear_sync_state)?;
            let mut memory = self.memory();
            memory.devices.clear();
            memory.synced_revision = None;
        }
        self.keep_location(Some(location))
    }

    /// Run `work` alone, on a thread for it, with the storage at
    /// `location`, opened with `login` or the one the keychain keeps.
    async fn job<R: Send + 'static>(
        self: &Arc<Self>,
        location: hosts_sync::SyncLocation,
        login: Option<Zeroizing<String>>,
        work: impl FnOnce(&dyn SyncStore, &dyn SecretStore, &SyncServices) -> Result<R, JobError>
        + Send
        + 'static,
    ) -> Result<R, SyncFailure> {
        self.job_with(Some((location, login)), move |remote, secrets, services| {
            work(remote.ok_or(JobError::Unavailable)?, secrets, services)
        })
        .await
    }

    /// As [`Self::job`], the storage opened only when `open` says where:
    /// turning sync off needs none unless it deletes a file.
    async fn job_with<R: Send + 'static>(
        self: &Arc<Self>,
        open: Option<(hosts_sync::SyncLocation, Option<Zeroizing<String>>)>,
        work: impl FnOnce(
            Option<&dyn SyncStore>,
            &dyn SecretStore,
            &SyncServices,
        ) -> Result<R, JobError>
        + Send
        + 'static,
    ) -> Result<R, SyncFailure> {
        let _turn = self
            .turn
            .clone()
            .try_lock_owned()
            .map_err(|_| SyncFailure::InProgress)?;
        let secrets = self.secrets()?;
        let services = self.services.clone();
        task::blocking(&self.store.runtime, move || {
            let remote = match open {
                Some((location, login)) => Some(worker::open(
                    &services,
                    &location,
                    login.as_deref().map(String::as_str),
                    &*secrets,
                )?),
                None => None,
            };
            work(remote.as_deref(), &*secrets, &services)
        })
        .await
        .map_err(SyncFailure::from)
    }

    async fn round(
        self: &Arc<Self>,
        allow_mass_delete: bool,
        accept_removed: bool,
    ) -> Result<SyncOutcome, SyncFailure> {
        let _turn = self.turn.clone().lock_owned().await;
        // As it is once the round before has ended.
        let location = self.location()?;
        self.secrets()?;
        let host = MobileRound {
            store: self.store.clone(),
        };
        let opts = RoundOptions {
            allow_mass_delete,
            join: JoinMode::Merge,
        };
        let ended = run_round(&host, &self.services, &location, opts, accept_removed).await;
        match ended {
            RoundEnd::Synced { revision, devices } => {
                let mut memory = self.memory();
                memory.synced_revision = Some(revision);
                memory.devices = devices;
                Ok(SyncOutcome::Synced {
                    devices: memory.devices.iter().map(SyncDevice::of).collect(),
                })
            }
            RoundEnd::Attention(attention) => Ok(SyncOutcome::Attention {
                attention: attention.into(),
            }),
            // The data went from under it.
            RoundEnd::Failed(JobError::Unavailable) if self.store.is_closed() => {
                Err(SyncFailure::Closed)
            }
            RoundEnd::Failed(error) => Err(error.into()),
        }
    }
}

#[uniffi::export]
impl SyncService {
    pub fn status(&self) -> SyncStatus {
        let inner = &self.inner;
        let (revision, state) = {
            let data = inner.store.lock();
            (data.sync_revision(), data.sync_state())
        };
        let device_id = state
            .and_then(|bytes| SyncState::from_bytes(&bytes).ok())
            .map(|state| state.device().as_str().to_owned());
        let memory = inner.memory();
        SyncStatus {
            location: memory.location.clone().map(Into::into),
            device_id,
            devices: memory.devices.iter().map(SyncDevice::of).collect(),
            busy: inner.turn.try_lock().is_err(),
            has_changes: memory.location.is_some() && memory.synced_revision != Some(revision),
        }
    }

    /// One round now, then the outcome. Run one when the app starts or
    /// comes to the foreground, a few seconds after the user stops
    /// editing, and before it goes to the background; not on a timer.
    /// `allow_mass_delete` and `accept_removed` are the user's answers to
    /// [`Attention::MassDelete`] and [`Attention::Removed`].
    pub async fn sync_now(
        &self,
        allow_mass_delete: bool,
        accept_removed: bool,
    ) -> Result<SyncOutcome, SyncFailure> {
        let inner = self.inner.clone();
        task::run(&self.inner.store.runtime, async move {
            inner.round(allow_mass_delete, accept_removed).await
        })
        .await
    }

    /// Whether sync is set up at `location` already: to join it, or to
    /// set it up. A server's or a bucket's opens with `login`.
    pub async fn inspect(
        &self,
        location: SyncLocation,
        login: Option<String>,
    ) -> Result<bool, SyncFailure> {
        let inner = self.inner.clone();
        let login = login.map(Zeroizing::new);
        task::run(&self.inner.store.runtime, async move {
            inner
                .job(location.into(), login, |remote, _, _| {
                    worker::inspect(remote)
                })
                .await
        })
        .await
    }

    /// Write, read back and delete a file at `location`, keeping nothing:
    /// all sync asks of it. Beside a round as well.
    pub async fn test_connection(
        &self,
        location: SyncLocation,
        login: Option<String>,
    ) -> Result<(), SyncFailure> {
        let inner = self.inner.clone();
        let login = login.map(Zeroizing::new);
        let location: hosts_sync::SyncLocation = location.into();
        task::run(&self.inner.store.runtime, async move {
            let secrets = inner.secrets()?;
            let services = inner.services.clone();
            let probe = format!("probe-{:016x}.tmp", rand::random::<u64>());
            task::blocking(&inner.store.runtime, move || {
                let login = login.as_deref().map(String::as_str);
                let remote = worker::open(&services, &location, login, &*secrets)?;
                worker::probe(&*remote, &probe)
            })
            .await
            .map_err(SyncFailure::from)
        })
        .await
    }

    /// Set sync up at `location` with a new sync password, at least
    /// [`min_sync_password_length`] characters. A server's or a bucket's
    /// `login` is kept in the keychain. Then [`Self::sync_now`].
    pub async fn create(
        &self,
        location: SyncLocation,
        login: Option<String>,
        password: String,
    ) -> Result<(), SyncFailure> {
        let password = Zeroizing::new(password);
        if let Err(problem) = hosts_sync::check_new_password(&password, &password) {
            return Err(SyncFailure::Password {
                problem: problem.into(),
            });
        }
        let inner = self.inner.clone();
        let login = login.map(Zeroizing::new);
        let location: hosts_sync::SyncLocation = location.into();
        task::run(&self.inner.store.runtime, async move {
            let kept = login.clone();
            inner
                .job(location.clone(), login, move |remote, secrets, services| {
                    worker::create(remote, secrets, &password, services.kdf)?;
                    worker::keep_login(secrets, kept.as_deref().map(String::as_str))
                })
                .await?;
            inner.joined(location, true)
        })
        .await
    }

    /// Join the sync at `location` with its password, keeping a server's
    /// or a bucket's `login`. `fresh` forgets what this device synced
    /// before: its first time, or sync was reset; a password changed
    /// elsewhere keeps it. Then [`Self::sync_now`].
    pub async fn join(
        &self,
        location: SyncLocation,
        login: Option<String>,
        password: String,
        fresh: bool,
    ) -> Result<(), SyncFailure> {
        let password = Zeroizing::new(password);
        if let Err(problem) = hosts_sync::check_password(&password) {
            return Err(SyncFailure::Password {
                problem: problem.into(),
            });
        }
        let inner = self.inner.clone();
        let login = login.map(Zeroizing::new);
        let location: hosts_sync::SyncLocation = location.into();
        task::run(&self.inner.store.runtime, async move {
            let kept = login.clone();
            inner
                .job(location.clone(), login, move |remote, secrets, _| {
                    worker::join(remote, secrets, &password)?;
                    worker::keep_login(secrets, kept.as_deref().map(String::as_str))
                })
                .await?;
            inner.joined(location, fresh)
        })
        .await
    }

    /// Sign in anew to the server or bucket sync goes through: `location`
    /// is the same place, maybe as another user or with another key, and
    /// `login` must get in before it is kept. Then [`Self::sync_now`].
    pub async fn change_login(
        &self,
        location: SyncLocation,
        login: String,
    ) -> Result<(), SyncFailure> {
        let inner = self.inner.clone();
        let login = Zeroizing::new(login);
        let location: hosts_sync::SyncLocation = location.into();
        task::run(&self.inner.store.runtime, async move {
            let now = inner.location()?;
            if !(now.signs_in() && now.same_place(&location)) {
                return Err(SyncFailure::OtherPlace);
            }
            let kept = login.clone();
            inner
                .job(location.clone(), Some(login), move |remote, secrets, _| {
                    // The sync there is the one this device joined: the
                    // round tells, as for any other.
                    if !worker::inspect(remote)? {
                        return Err(SyncError::VaultMissing.into());
                    }
                    worker::keep_login(secrets, Some(kept.as_str()))
                })
                .await?;
            inner.joined(location, false)
        })
        .await
    }

    /// 更换同步密码, and the data key with it: what the other devices hold
    /// opens nothing newer. They are asked for the new one. Then
    /// [`Self::sync_now`].
    pub async fn change_password(&self, password: String) -> Result<(), SyncFailure> {
        let password = Zeroizing::new(password);
        if let Err(problem) = hosts_sync::check_new_password(&password, &password) {
            return Err(SyncFailure::Password {
                problem: problem.into(),
            });
        }
        let inner = self.inner.clone();
        task::run(&self.inner.store.runtime, async move {
            let location = inner.location()?;
            inner
                .job(location, None, move |remote, secrets, services| {
                    worker::new_password(remote, secrets, &password, services.kdf)
                })
                .await
        })
        .await
    }

    /// Remove a device from sync: its file goes, and it leaves the list
    /// until it syncs again.
    pub async fn remove_device(&self, id: String) -> Result<(), SyncFailure> {
        let device = DeviceId::parse(&id).ok_or(SyncFailure::NoSuchDevice)?;
        let inner = self.inner.clone();
        task::run(&self.inner.store.runtime, async move {
            let location = inner.location()?;
            let gone = device.clone();
            inner
                .job(location, None, move |remote, _, _| {
                    worker::remove_device(remote, &device)
                })
                .await?;
            inner.memory().devices.retain(|device| device.id != gone);
            Ok(())
        })
        .await
    }

    /// 关闭同步: the data here stays as it is; sync's keys, the login and
    /// what this device synced go, and with `delete_file` its file at the
    /// location too.
    pub async fn turn_off(&self, delete_file: bool) -> Result<(), SyncFailure> {
        let inner = self.inner.clone();
        task::run(&self.inner.store.runtime, async move {
            let Ok(location) = inner.location() else {
                return Ok(());
            };
            let state = inner.store.with_data(|data| data.sync_state())?;
            inner
                .job_with(
                    delete_file.then_some((location, None)),
                    move |remote, secrets, _| {
                        if let Some(remote) = remote
                            && let Some(state) =
                                state.and_then(|bytes| SyncState::from_bytes(&bytes).ok())
                        {
                            worker::remove_device(remote, state.device())?;
                        }
                        // Left in the keychain, they open nothing new once
                        // the next set-up writes over them.
                        let _ = worker::forget_keyring(secrets);
                        let _ = worker::keep_login(secrets, None);
                        Ok(())
                    },
                )
                .await?;
            inner.store.with_data(HostData::clear_sync_state)?;
            {
                let mut memory = inner.memory();
                memory.devices.clear();
                memory.synced_revision = None;
            }
            inner.keep_location(None)
        })
        .await
    }
}

/// A round's way to the data: briefly, and settled after each use so what
/// could not be kept is said as for any change.
struct MobileRound {
    store: Arc<Shared>,
}

impl RoundHost for MobileRound {
    fn with_data<R>(&self, f: impl FnOnce(&mut HostData) -> R) -> Option<R> {
        self.store.with_data(f).ok()
    }

    fn background<R: Send + 'static>(&self, work: Blocking<R>) -> impl Future<Output = R> + Send {
        let runtime = self.store.runtime.clone();
        async move { task::blocking(&runtime, work).await }
    }
}

/// Write `bytes` to `path` whole: to a file beside it first, then in its
/// place.
fn write_replacing(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)
}

impl SyncDevice {
    fn of(device: &RemoteDevice) -> Self {
        Self {
            id: device.id.as_str().to_owned(),
            name: device.info.name.clone(),
            platform: device.info.platform.clone(),
            app_version: device.info.app_version.clone(),
            last_write_ms: device.last_write_ms,
        }
    }
}

impl From<SyncLocation> for hosts_sync::SyncLocation {
    fn from(location: SyncLocation) -> Self {
        match location {
            SyncLocation::Folder { path } => Self::Folder { path: path.into() },
            SyncLocation::WebDav { url, username } => Self::WebDav { url, username },
            SyncLocation::S3 {
                endpoint,
                region,
                bucket,
                prefix,
                path_style,
                access_key,
            } => Self::S3 {
                endpoint,
                region,
                bucket,
                prefix,
                path_style,
                access_key,
            },
        }
    }
}

impl From<hosts_sync::SyncLocation> for SyncLocation {
    fn from(location: hosts_sync::SyncLocation) -> Self {
        match location {
            hosts_sync::SyncLocation::Folder { path } => Self::Folder {
                path: path.display().to_string(),
            },
            hosts_sync::SyncLocation::WebDav { url, username } => Self::WebDav { url, username },
            hosts_sync::SyncLocation::S3 {
                endpoint,
                region,
                bucket,
                prefix,
                path_style,
                access_key,
            } => Self::S3 {
                endpoint,
                region,
                bucket,
                prefix,
                path_style,
                access_key,
            },
        }
    }
}

impl From<hosts_sync::Attention> for Attention {
    fn from(attention: hosts_sync::Attention) -> Self {
        match attention {
            hosts_sync::Attention::NeedsPassword => Self::NeedsPassword,
            hosts_sync::Attention::VaultReplaced => Self::VaultReplaced,
            hosts_sync::Attention::Removed => Self::Removed,
            hosts_sync::Attention::MassDelete { count, of } => Self::MassDelete { count, of },
            hosts_sync::Attention::LoginRefused => Self::LoginRefused,
        }
    }
}

impl From<hosts_sync::PasswordError> for PasswordProblem {
    fn from(error: hosts_sync::PasswordError) -> Self {
        match error {
            hosts_sync::PasswordError::Missing => Self::Missing,
            hosts_sync::PasswordError::TooShort => Self::TooShort,
            hosts_sync::PasswordError::Mismatch => Self::Mismatch,
        }
    }
}

impl From<hosts_sync::LoginError> for LoginProblem {
    fn from(error: hosts_sync::LoginError) -> Self {
        match error {
            hosts_sync::LoginError::UrlInvalid => Self::UrlInvalid,
            hosts_sync::LoginError::UsernameMissing => Self::UsernameMissing,
            hosts_sync::LoginError::PasswordMissing => Self::PasswordMissing,
            hosts_sync::LoginError::EndpointInvalid => Self::EndpointInvalid,
            hosts_sync::LoginError::BucketInvalid => Self::BucketInvalid,
            hosts_sync::LoginError::AccessKeyMissing => Self::AccessKeyMissing,
            hosts_sync::LoginError::SecretKeyMissing => Self::SecretKeyMissing,
        }
    }
}

impl From<crate::store::StoreError> for SyncFailure {
    /// What reaching the data fails with: the core is gone.
    fn from(_: crate::store::StoreError) -> Self {
        Self::Closed
    }
}

impl From<JobError> for SyncFailure {
    fn from(error: JobError) -> Self {
        match error {
            JobError::Sync(error) => error.into(),
            JobError::Unavailable => Self::Unavailable,
            JobError::NoKeyring => Self::NoKeyring,
            JobError::Keychain(error) => Self::Keychain {
                problem: (&error).into(),
            },
            JobError::NotSaved(detail) => Self::NotSaved { detail },
            JobError::StateNotSaved(detail) => Self::StateNotSaved { detail },
            JobError::NoLogin => Self::NoLogin,
            JobError::InProgress => Self::InProgress,
            JobError::Busy => Self::Busy,
        }
    }
}

impl From<SyncError> for SyncFailure {
    fn from(error: SyncError) -> Self {
        match error {
            SyncError::Store(error) => Self::Storage {
                problem: error.into(),
            },
            SyncError::VaultMissing => Self::VaultMissing,
            SyncError::VaultExists => Self::VaultExists,
            SyncError::VaultTooNew(version) => Self::VaultTooNew { version },
            SyncError::BadVault(detail) => Self::BadVault { detail },
            SyncError::WrongPassword => Self::WrongPassword,
            SyncError::NeedsPassword => Self::NeedsPassword,
            SyncError::VaultReplaced => Self::VaultReplaced,
            SyncError::StateUnreadable(detail) => Self::StateUnreadable { detail },
            SyncError::MassDelete { count, of } => Self::MassDelete { count, of },
            SyncError::ReadOnly { min_reader } => Self::ReadOnly { min_reader },
            SyncError::Random => Self::Random,
        }
    }
}

impl From<StoreError> for StorageProblem {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound,
            StoreError::PreconditionFailed => Self::PreconditionFailed,
            StoreError::TooLarge => Self::TooLarge,
            StoreError::Timeout => Self::Timeout,
            StoreError::Unauthorized => Self::Unauthorized,
            StoreError::Unavailable(detail) => Self::Unavailable { detail },
            StoreError::Io(detail) => Self::Io { detail },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use futures::executor::block_on;
    use shellrs_hosts::sync::SyncServices;
    use shellrs_sync::{DeviceInfo, KdfParams};

    use super::{PasswordProblem, SyncFailure, SyncLocation, SyncOutcome};
    use crate::{
        Core, CoreConfig, HostAuth, HostDraft, HostProtocol, HostRoute, Keychain, KeychainError,
        SecretChange,
    };

    /// Deriving a key from the sync password, quickly.
    const FAST: KdfParams = KdfParams {
        memory_kib: 8,
        passes: 1,
        parallelism: 1,
    };

    #[derive(Default)]
    struct Memory(Mutex<HashMap<String, String>>);

    impl Memory {
        fn read(&self, account: &str) -> Option<String> {
            self.0.lock().unwrap().get(account).cloned()
        }
    }

    impl Keychain for Memory {
        fn available(&self) -> bool {
            true
        }

        fn get(&self, account: String) -> Result<Option<String>, KeychainError> {
            Ok(self.read(&account))
        }

        fn set(&self, account: String, value: String) -> Result<(), KeychainError> {
            self.0.lock().unwrap().insert(account, value);
            Ok(())
        }

        fn delete(&self, account: String) -> Result<(), KeychainError> {
            self.0.lock().unwrap().remove(&account);
            Ok(())
        }
    }

    /// A phone called `name`, keeping its data in `data`.
    fn phone(data: &Path, name: &str, keychain: &Arc<Memory>) -> Arc<Core> {
        let device = DeviceInfo {
            name: name.into(),
            platform: "ios".into(),
            app_version: "0.0.0".into(),
        };
        let mut services = SyncServices::system("ShellRS/0.0.0", device.clone());
        services.kdf = FAST;
        let config = CoreConfig {
            data_dir: data.display().to_string(),
            device_name: device.name,
            platform: device.platform,
            app_version: device.app_version,
        };
        Core::open(config, keychain.clone(), services).unwrap()
    }

    fn names(outcome: SyncOutcome) -> Vec<String> {
        match outcome {
            SyncOutcome::Synced { devices } => devices.into_iter().map(|d| d.name).collect(),
            outcome => panic!("{outcome:?}"),
        }
    }

    const PASSWORD: &str = "correct horse battery";

    #[test]
    fn two_phones_sync_hosts_and_passwords_through_a_folder() {
        let (one_dir, two_dir, folder) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let location = SyncLocation::Folder {
            path: folder.path().display().to_string(),
        };
        let (one_keys, two_keys) = (Arc::new(Memory::default()), Arc::new(Memory::default()));
        let one = phone(one_dir.path(), "one", &one_keys);
        let two = phone(two_dir.path(), "two", &two_keys);
        block_on(async {
            let draft = HostDraft {
                name: "web".into(),
                protocol: HostProtocol::Ssh,
                address: "10.0.0.1".into(),
                port: 22,
                user: "root".into(),
                auth: HostAuth::Password,
                route: HostRoute::Direct,
                group: None,
                notes: String::new(),
            };
            let password = SecretChange::Set {
                value: "hunter2".into(),
            };
            let id = one
                .store()
                .add_host(draft, password, SecretChange::Keep)
                .await
                .unwrap();
            let sync = one.sync();
            assert!(matches!(
                sync.sync_now(false, false).await,
                Err(SyncFailure::NotSetUp)
            ));
            assert!(matches!(
                sync.create(location.clone(), None, "short".into()).await,
                Err(SyncFailure::Password {
                    problem: PasswordProblem::TooShort
                })
            ));
            sync.create(location.clone(), None, PASSWORD.into())
                .await
                .unwrap();
            assert!(one_keys.read("sync:keyring").is_some());
            let status = sync.status();
            assert_eq!(status.location, Some(location.clone()));
            assert!(status.has_changes);
            assert_eq!(names(sync.sync_now(false, false).await.unwrap()), [""; 0]);
            let status = sync.status();
            assert!(!status.has_changes);
            assert!(status.device_id.is_some());

            let sync = two.sync();
            assert!(sync.inspect(location.clone(), None).await.unwrap());
            assert!(matches!(
                sync.join(location.clone(), None, "wrong horse battery".into(), true)
                    .await,
                Err(SyncFailure::WrongPassword)
            ));
            sync.join(location.clone(), None, PASSWORD.into(), true)
                .await
                .unwrap();
            assert_eq!(names(sync.sync_now(false, false).await.unwrap()), ["one"]);
            let hosts = two.store().hosts();
            assert_eq!(
                hosts
                    .iter()
                    .map(|h| h.public_id.clone())
                    .collect::<Vec<_>>(),
                [one.store().host(id).unwrap().public_id]
            );
            assert_eq!(
                two_keys.read("password:root@10.0.0.1:22").as_deref(),
                Some("hunter2")
            );
        });

        // Where sync goes is kept beside the database.
        drop(two);
        let two = phone(two_dir.path(), "two", &two_keys);
        let sync = two.sync();
        assert_eq!(sync.status().location, Some(location.clone()));
        block_on(async {
            let others = one.sync().sync_now(false, false).await.unwrap();
            assert_eq!(names(others), ["two"]);
            let two_id = sync.status().device_id.unwrap();
            assert!(matches!(
                one.sync().remove_device("not an id".into()).await,
                Err(SyncFailure::NoSuchDevice)
            ));

            // Turned off, it forgets the place, the keys and its file
            // there; the data stays.
            sync.turn_off(true).await.unwrap();
            assert_eq!(sync.status().location, None);
            assert!(two_keys.read("sync:keyring").is_none());
            assert!(!two_dir.path().join("sync.json").exists());
            assert_eq!(two.store().hosts().len(), 1);
            assert_eq!(
                names(one.sync().sync_now(false, false).await.unwrap()),
                [""; 0]
            );
            assert!(!one.sync().status().devices.iter().any(|d| d.id == two_id));
        });
    }
}
