//! One round of sync, from reading the records to writing this device's
//! file, and what it works with.
//!
//! The round runs anywhere: what it needs of the app, the data and a
//! thread to block on, comes through [`RoundHost`]. Its two halves block
//! (the storage, the keychain, the sync core) and its middle, applying the
//! changes, holds the data briefly.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use shellrs_sync::{
    DeviceInfo, KdfParams, LocalFolderStore, Pulled, Pushed, RemoteDevice, RoundOptions,
    S3Location, S3Store, StoreError, SyncError, SyncStore, WebDavStore,
};

use super::{JobError, SyncLocation, worker};
use crate::{ApplyError, HostData};

/// Tries a round makes when its changes meet newer ones.
const ATTEMPTS: usize = 3;

/// Opens the storage at a location, a WebDAV server's with its password.
pub type OpenStore = Arc<
    dyn Fn(&SyncLocation, Option<&str>) -> Result<Arc<dyn SyncStore>, StoreError> + Send + Sync,
>;

/// What sync works with. Production has the real ones
/// ([`SyncServices::system`]); tests share one `MemoryStore` between
/// devices.
#[derive(Clone)]
pub struct SyncServices {
    pub open: OpenStore,
    /// What deriving the key of a new sync password costs.
    pub kdf: KdfParams,
    /// This device, as the others list it.
    pub device: DeviceInfo,
}

impl SyncServices {
    /// The real storage, asked for with `user_agent`, for `device`.
    pub fn system(user_agent: &str, device: DeviceInfo) -> Self {
        let user_agent = user_agent.to_owned();
        Self {
            open: Arc::new(move |location, password| match location {
                SyncLocation::Folder { path } => Ok(Arc::new(LocalFolderStore::new(path))),
                SyncLocation::WebDav { url, username } => Ok(Arc::new(WebDavStore::new(
                    url,
                    username,
                    password.unwrap_or_default(),
                    &user_agent,
                )?)),
                SyncLocation::S3 {
                    endpoint,
                    region,
                    bucket,
                    prefix,
                    path_style,
                    access_key,
                } => {
                    let bucket = S3Location {
                        endpoint: endpoint.clone(),
                        region: region.clone(),
                        bucket: bucket.clone(),
                        prefix: prefix.clone(),
                        path_style: *path_style,
                    };
                    Ok(Arc::new(S3Store::new(
                        &bucket,
                        access_key,
                        password.unwrap_or_default(),
                        &user_agent,
                    )?))
                }
            }),
            kdf: KdfParams::DEFAULT,
            device,
        }
    }
}

/// What only the user can settle. Automatic rounds wait for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attention {
    /// The sync password was changed on another device, or the keychain
    /// lost the keys: the password is needed.
    NeedsPassword,
    /// Sync was reset on another device: join it anew, or turn it off.
    VaultReplaced,
    /// This device was removed from sync on another one: join again, or
    /// turn it off.
    Removed,
    /// A round would delete `count` of the `of` records synced: delete, or
    /// leave sync waiting.
    MassDelete { count: u32, of: u32 },
    /// The WebDAV server or the bucket refused the login, or the keychain
    /// lost it: a new login is needed.
    LoginRefused,
}

/// How a round ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoundEnd {
    /// It went through, having read the data at `revision`.
    Synced {
        revision: u64,
        devices: Vec<RemoteDevice>,
    },
    Attention(Attention),
    Failed(JobError),
}

/// Work that blocks, for [`RoundHost::background`].
pub type Blocking<R> = Box<dyn FnOnce() -> R + Send>;

/// What a round needs of the app.
pub trait RoundHost {
    /// Run `f` with the app's data, briefly; `None` once the data is gone.
    fn with_data<R>(&self, f: impl FnOnce(&mut HostData) -> R) -> Option<R>;

    /// Run `work`, which blocks, off the thread the round runs on.
    ///
    /// `work` comes boxed: with a closure's type in the future's, rustc
    /// cannot tell the round's future `Send`, which a multi-threaded
    /// runtime needs.
    fn background<R: Send + 'static>(&self, work: Blocking<R>) -> impl Future<Output = R> + Send;
}

/// One round at `location`, tried again while its changes meet newer
/// ones. `accept_removed` writes this device's file again though another
/// device removed it.
pub async fn run_round(
    host: &impl RoundHost,
    services: &SyncServices,
    location: &SyncLocation,
    opts: RoundOptions,
    accept_removed: bool,
) -> RoundEnd {
    let gone = || RoundEnd::Failed(JobError::Unavailable);
    let signs_in = location.signs_in();
    let Some(secrets) = host.with_data(|data| data.secrets()) else {
        return gone();
    };
    let remote = host
        .background({
            let (services, location, secrets) =
                (services.clone(), location.clone(), secrets.clone());
            Box::new(move || worker::open(&services, &location, None, &*secrets).map(Remote))
        })
        .await;
    let Remote(remote) = match remote {
        Ok(remote) => remote,
        Err(error) => return stopped(error, signs_in),
    };
    for _ in 0..ATTEMPTS {
        let Some((export, state, key_dir)) = host.with_data(|data| {
            (
                data.sync_export(),
                data.sync_state(),
                data.key_dir().map(Path::to_path_buf),
            )
        }) else {
            return gone();
        };
        let pulled = host
            .background({
                let (remote, secrets, export) = (remote.clone(), secrets.clone(), export.clone());
                Box::new(move || {
                    worker::pull_round(&*remote, &*secrets, &export, state.as_deref(), opts)
                })
            })
            .await;
        let (pulled, keyring) = match pulled {
            Ok(pulled) => pulled,
            Err(error) => return stopped(error, signs_in),
        };
        if pulled.report.removed && !accept_removed {
            return RoundEnd::Attention(Attention::Removed);
        }
        let state_bytes = pulled.state.to_bytes();
        let Some(applied) =
            host.with_data(|data| data.apply_sync(&pulled.changes, &state_bytes, &export))
        else {
            return gone();
        };
        let work = match applied {
            Ok(work) => work,
            Err(ApplyError::Stale) => continue,
            Err(ApplyError::NotSaved(error)) => return RoundEnd::Failed(JobError::NotSaved(error)),
        };
        let Pulled { state, report, .. } = pulled;
        let (written, pushed) = host
            .background({
                let (remote, secrets, device) =
                    (remote.clone(), secrets.clone(), services.device.clone());
                Box::new(move || {
                    worker::push_round(
                        &*remote,
                        &*secrets,
                        key_dir.as_deref(),
                        work,
                        &keyring,
                        &device,
                        &state,
                    )
                })
            })
            .await;
        if host
            .with_data(|data| data.adopt_synced_secrets(&written))
            .is_none()
        {
            return gone();
        }
        match pushed {
            Ok(Pushed::Uploaded { state }) => {
                match host.with_data(|data| data.save_sync_state(&state.to_bytes())) {
                    Some(Ok(())) => {}
                    Some(Err(error)) => return RoundEnd::Failed(JobError::StateNotSaved(error)),
                    None => return gone(),
                }
            }
            Ok(Pushed::Unchanged) => {}
            Ok(Pushed::Conflict) => continue,
            Err(error) => return stopped(error, signs_in),
        }
        return RoundEnd::Synced {
            revision: export.revision,
            devices: report.devices,
        };
    }
    RoundEnd::Failed(JobError::Busy)
}

/// The storage a round opened, as [`RoundHost::background`] gives it
/// back: a type of its own, without the lifetime a `dyn` carries, which
/// rustc loses on the way to telling the round's future `Send`.
struct Remote(Arc<dyn SyncStore>);

/// A round's error, or what the user is to settle when only they can. A
/// refusal is a login's to settle on a server or a bucket, a folder's
/// permissions otherwise.
fn stopped(error: JobError, signs_in: bool) -> RoundEnd {
    match error {
        JobError::NoKeyring | JobError::Sync(SyncError::NeedsPassword) => {
            RoundEnd::Attention(Attention::NeedsPassword)
        }
        JobError::Sync(SyncError::VaultReplaced) => RoundEnd::Attention(Attention::VaultReplaced),
        JobError::Sync(SyncError::MassDelete { count, of }) => {
            RoundEnd::Attention(Attention::MassDelete { count, of })
        }
        JobError::NoLogin => RoundEnd::Attention(Attention::LoginRefused),
        JobError::Sync(SyncError::Store(StoreError::Unauthorized)) if signs_in => {
            RoundEnd::Attention(Attention::LoginRefused)
        }
        error => RoundEnd::Failed(error),
    }
}
