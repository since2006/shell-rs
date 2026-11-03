//! The runtime everything runs on, the saved hosts, sync, and what all
//! connections share.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shellrs_hosts::secrets::KeychainSource;
use shellrs_hosts::sync::SyncServices;
use shellrs_hosts::{HostData, HostDatabase, HostId, HostOs, OpenError};
use shellrs_ssh::{self as ssh, AgentLocation, HostOsProbe, ProbeOutcome, SshConnector, SshHandle};
use shellrs_sync::DeviceInfo;
use tokio::runtime::{Handle, Runtime};

use crate::connection::{Connection, State};
use crate::login::Login;
use crate::secrets::{AppKeychain, Keychain};
use crate::store::{HostStore, Shared, StoreError, StoreFailure};
use crate::sync::{self, SyncService};

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreConfig {
    /// The app's own data folder, kept as the desktop keeps its own: the
    /// database (`shellrs.db`), the private keys ShellRS keeps (`keys/`),
    /// the trusted host keys (`known_hosts`), where sync goes through and
    /// the resume records of transfers.
    pub data_dir: String,
    /// This device as the others list it in sync, such as "Ann's iPhone".
    pub device_name: String,
    /// `ios` or `android`.
    pub platform: String,
    /// The app's version, which sync shows beside the device and the
    /// storage is asked with.
    pub app_version: String,
}

/// Why there is no core.
#[derive(Debug, uniffi::Error)]
pub enum CoreError {
    /// Another core runs on this data folder.
    DataFolderInUse,
    /// The data folder could not be made, in the system's words.
    DataFolder { detail: String },
    /// A newer ShellRS wrote the database, at schema `version`; this one
    /// knows up to `supported`. Update the app.
    DatabaseNewer { version: i64, supported: i64 },
    /// An early development build wrote the database.
    DatabaseTooOld { version: i64 },
    /// The database could not be opened or read, in SQLite's words.
    Database { detail: String },
    /// What should never happen: a bug in ShellRS.
    Internal { detail: String },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for CoreError {}

impl From<OpenError> for CoreError {
    fn from(error: OpenError) -> Self {
        match error {
            OpenError::Newer { version, supported } => Self::DatabaseNewer { version, supported },
            OpenError::TooOld { version } => Self::DatabaseTooOld { version },
            OpenError::Sqlite(error) => Self::Database {
                detail: error.to_string(),
            },
        }
    }
}

/// The core: make one when the app starts and keep it for as long as it
/// runs. Its connections, terminals and sessions stop when it goes, and
/// its store and sync answer no more.
#[derive(uniffi::Object)]
pub struct Core {
    runtime: Option<Runtime>,
    handle: Handle,
    connector: SshConnector,
    data_dir: PathBuf,
    store: Arc<Shared>,
    sync: Arc<sync::Inner>,
}

/// The data folders a core runs on now: two would both write the database,
/// the `known_hosts` and the resume records.
static DATA_DIRS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

#[uniffi::export]
impl Core {
    /// Opens the database, upgrading one an older version wrote: a moment
    /// on the thread it is called on. Fails when another core runs on
    /// `config.data_dir`, or the folder or the database will not open.
    #[uniffi::constructor]
    pub fn new(config: CoreConfig, keychain: Arc<dyn Keychain>) -> Result<Arc<Self>, CoreError> {
        let user_agent = format!("ShellRS/{}", config.app_version);
        let device = DeviceInfo {
            name: config.device_name.clone(),
            platform: config.platform.clone(),
            app_version: config.app_version.clone(),
        };
        Self::open(config, keychain, SyncServices::system(&user_agent, device))
    }

    /// The saved hosts, groups and credentials.
    pub fn store(&self) -> Arc<HostStore> {
        Arc::new(HostStore {
            shared: self.store.clone(),
        })
    }

    /// Sync with the user's other devices.
    pub fn sync(&self) -> Arc<SyncService> {
        Arc::new(SyncService {
            inner: self.sync.clone(),
        })
    }

    /// Start connecting to a saved SSH host, and return at once: what
    /// happens comes as the connection's events. Once it is in, the host
    /// goes to the front of the recent ones, and what system it runs on is
    /// asked again for its mark.
    pub fn connect_host(&self, id: u64) -> Result<Arc<Connection>, StoreError> {
        let id = HostId(id);
        let login = {
            let data = self.store.lock();
            let host = data
                .saved_host(id)
                .filter(|host| !host.is_serial())
                .ok_or(StoreError::NotFound)?;
            if !host.is_ssh() {
                return Err(StoreError::NotSsh);
            }
            data.login_of(host)
        };
        // Not through a bastion host that opened a link: another channel
        // ends its session.
        let shell_only = login.shell_only;
        let connection = Connection::start(
            self.handle.clone(),
            self.connector.clone(),
            login,
            self.data_dir.clone(),
        );
        let (store, mut state) = (self.store.clone(), connection.inner.state.clone());
        self.handle.spawn(async move {
            let handle = loop {
                let handle = match &*state.borrow_and_update() {
                    State::Up { handle, .. } => Some(handle.clone()),
                    State::Closed => return,
                    State::Connecting => None,
                };
                if let Some(handle) = handle {
                    break handle;
                }
                if state.changed().await.is_err() {
                    return;
                }
            };
            let _ = store.with_data(|data| data.record_connection(id));
            if !shell_only {
                probe(&handle, &store, id).await;
            }
        });
        Ok(connection)
    }

    /// Start connecting to `login`'s host, saved or not, and return at
    /// once: what happens comes as the connection's events.
    pub fn connect(&self, login: Login) -> Arc<Connection> {
        Connection::start(
            self.handle.clone(),
            self.connector.clone(),
            login.into(),
            self.data_dir.clone(),
        )
    }

    /// The password of a connection that is not saved
    /// ([`crate::SecretRef::Temporary`] of `host`), kept in memory only
    /// until the core goes; `None` forgets it.
    pub fn set_temporary_password(&self, host: u64, password: Option<String>) {
        let secrets = self.store.lock().secrets();
        let secret = ssh::SecretRef::temporary(host);
        // Memory only, never the keychain: neither blocks nor fails.
        let _ = match password {
            Some(password) => secrets.set(&secret, &password),
            None => secrets.delete(&secret),
        };
    }

    /// How many files of a transfer move at once, 1 to 10, in every SFTP
    /// session; 3 until told.
    pub fn set_parallel_transfers(&self, count: u32) {
        ssh::sftp::set_parallel_transfers(count as usize);
    }
}

impl Core {
    /// A core whose sync works with `services`.
    pub(crate) fn open(
        config: CoreConfig,
        keychain: Arc<dyn Keychain>,
        services: SyncServices,
    ) -> Result<Arc<Self>, CoreError> {
        let data_dir = PathBuf::from(&config.data_dir);
        std::fs::create_dir_all(&data_dir).map_err(|error| CoreError::DataFolder {
            detail: error.to_string(),
        })?;
        if !data_dirs()
            .get_or_insert_with(HashSet::new)
            .insert(data_dir.clone())
        {
            return Err(CoreError::DataFolderInUse);
        }
        let opened = Self::open_data(&data_dir, keychain).and_then(|(data, moved)| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("shellrs-core")
                .enable_all()
                .build()
                .map_err(|error| CoreError::Internal {
                    detail: error.to_string(),
                })?;
            Ok((runtime, data, moved))
        });
        let (runtime, data, moved) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                release(&data_dir);
                return Err(error);
            }
        };
        let handle = runtime.handle().clone();
        let secrets = data.secrets();
        let store = Shared::new(data, handle.clone());
        if let Some(failure) = moved {
            store.report(StoreFailure::from(&failure));
        }
        // A phone has no agent and no `~/.ssh`; iOS asks for NAT64.
        let connector = SshConnector::new(
            data_dir.join("known_hosts"),
            KeychainSource::shared(secrets),
        )
        .with_agent(AgentLocation::None)
        .with_default_key_dir(None)
        .with_nat64(true);
        let sync = Arc::new(sync::Inner::new(
            store.clone(),
            services,
            data_dir.join("sync.json"),
        ));
        Ok(Arc::new(Self {
            handle,
            runtime: Some(runtime),
            connector,
            data_dir,
            store,
            sync,
        }))
    }

    /// The saved data in `data_dir`, with the app's keychain. The private
    /// keys ShellRS keeps are found again where the folder is now: an iOS
    /// app's moves with each update.
    fn open_data(
        data_dir: &std::path::Path,
        keychain: Arc<dyn Keychain>,
    ) -> Result<(HostData, Option<shellrs_hosts::Failure>), CoreError> {
        let database = HostDatabase::open(&data_dir.join("shellrs.db"))?;
        let data = HostData::load(database).map_err(|error| CoreError::Database {
            detail: error.to_string(),
        })?;
        let mut data = data
            .with_secrets(Arc::new(AppKeychain(keychain)))
            .with_key_dir(data_dir.join("keys"));
        let moved = data.adopt_moved_keys();
        Ok((data, moved))
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.store.close();
        // Never waits: the app may drop it on its main thread.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
        release(&self.data_dir);
    }
}

/// How long each step of [`probe`] may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most a step of [`probe`] reads; the probe itself keeps less.
const PROBE_OUTPUT_LIMIT: usize = 8 * 1024;

/// Ask what system the host runs on, on channels of their own, as the
/// desktop does each time it connects, and keep it for the host's mark. A
/// host that will not say keeps the mark it had.
async fn probe(handle: &SshHandle, store: &Arc<Shared>, id: HostId) {
    let mut probe = HostOsProbe::new();
    while let Some(command) = probe.command() {
        let Ok(output) = ssh::run_exec(handle, command, PROBE_OUTPUT_LIMIT, PROBE_TIMEOUT).await
        else {
            return;
        };
        probe.push(&output);
        if let ProbeOutcome::Detected(key) = probe.finish()
            && let Some(os) = HostOs::from_stored(key)
        {
            let _ = store.with_data(|data| data.set_host_os(id, Some(os)));
        }
    }
}

fn data_dirs() -> std::sync::MutexGuard<'static, Option<HashSet<PathBuf>>> {
    DATA_DIRS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn release(data_dir: &PathBuf) {
    if let Some(dirs) = data_dirs().as_mut() {
        dirs.remove(data_dir);
    }
}
