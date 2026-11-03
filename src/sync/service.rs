//! The sync service: when rounds run ([`run_round`] says how one goes),
//! and the jobs that set sync up, change its password and turn it off.
//!
//! The blocking calls run on the background executor; a round's two halves
//! there and its middle, applying the changes, on the UI thread with the
//! store. One round runs at a time, and none beside a job: asked for
//! meanwhile, it runs once that ends. A round that would need the user
//! (a new password, a reset, many deletions, a WebDAV login refused) stops
//! and says so; automatic rounds then wait until the user has dealt with it.

use std::future::Future;
use std::mem;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::*;
use shellrs_sync::{
    DeviceId, DeviceInfo, JoinMode, RemoteDevice, RoundOptions, SyncError, SyncState, SyncStore,
};
use zeroize::Zeroizing;

use super::{
    Attention, Blocking, JobError, RoundEnd, RoundHost, SyncLocation, SyncServices, run_round,
    worker,
};
use crate::host::{HostData, HostStore};
use crate::secrets::{SecretStore, SharedSecretStore};

/// After an edit, how long the user may go on editing before a round runs.
pub const EDIT_PAUSE: Duration = Duration::from_secs(5);
/// A window coming forward runs a round no more often than this.
const ACTIVATION_GAP: Duration = Duration::from_secs(60);
/// How long quitting waits for the last round, and how often it looks.
const QUIT_WAIT: Duration = Duration::from_secs(3);
const QUIT_POLL: Duration = Duration::from_millis(50);

/// The real services: the storage asked for as this version of ShellRS,
/// and this computer by its name.
pub fn system_services() -> SyncServices {
    let version = crate::update::build_info::VERSION;
    SyncServices::system(
        &format!("ShellRS/{version}"),
        DeviceInfo {
            name: crate::app::computer_name().unwrap_or_else(|| "ShellRS".into()),
            platform: std::env::consts::OS.into(),
            app_version: version.into(),
        },
    )
}

/// Where sync stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncPhase {
    Off,
    /// On, and no round has gone through yet this run.
    Waiting,
    Syncing,
    /// The last round went through, at this time (milliseconds since the
    /// epoch).
    Synced {
        at_ms: u64,
    },
    Failed(JobError),
}

/// Everything the settings page and the dialogs show, as a plain value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncSnapshot {
    /// Whether this copy can sync: it has the services and a keychain.
    pub available: bool,
    pub location: Option<SyncLocation>,
    pub phase: SyncPhase,
    pub attention: Option<Attention>,
    /// This computer, as the others list it.
    pub device: Option<DeviceInfo>,
    /// The other devices, as the last round found them.
    pub devices: Vec<RemoteDevice>,
    /// A round or a job runs.
    pub busy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// The user has something to settle, newly.
    NeedsAttention(Attention),
    /// Sync goes through this location now (set up, joined, or signed in
    /// to anew): keep it in the settings.
    LocationSet(SyncLocation),
    /// Sync was turned off: forget the location.
    TurnedOff,
}

pub struct SyncService {
    store: Entity<HostStore>,
    services: Option<SyncServices>,
    location: Option<SyncLocation>,
    phase: SyncPhase,
    attention: Option<Attention>,
    devices: Vec<RemoteDevice>,
    round: Option<Task<()>>,
    /// A job other than a round runs.
    job_running: bool,
    /// A round was asked for while one ran: run it once that ends.
    again: bool,
    /// The next round may delete however many records, as the user agreed.
    allow_mass_delete: bool,
    /// The next round may write this computer's file again, removed
    /// elsewhere as it was.
    accept_removed: bool,
    /// The store's revision the last round that went through had read.
    synced_revision: Option<u64>,
    /// The revision last seen, to tell edits from other notifications.
    seen_revision: u64,
    last_round_at: Option<Instant>,
    pause: Option<Task<()>>,
    _store: Subscription,
}

impl EventEmitter<SyncEvent> for SyncService {}

impl SyncService {
    /// A service with nothing to work with: it never syncs. The workspace
    /// gives production its services.
    pub fn new(store: Entity<HostStore>, cx: &mut Context<Self>) -> Self {
        let seen_revision = store.read(cx).sync_revision();
        let subscription = cx.observe(&store, |this, store, cx| {
            let revision = store.read(cx).sync_revision();
            if revision != this.seen_revision {
                this.seen_revision = revision;
                this.sync_after_pause(cx);
            }
        });
        Self {
            store,
            services: None,
            location: None,
            phase: SyncPhase::Off,
            attention: None,
            devices: Vec::new(),
            round: None,
            job_running: false,
            again: false,
            allow_mass_delete: false,
            accept_removed: false,
            synced_revision: None,
            seen_revision,
            last_round_at: None,
            pause: None,
            _store: subscription,
        }
    }

    pub fn set_services(&mut self, services: SyncServices, cx: &mut Context<Self>) {
        self.services = Some(services);
        cx.notify();
        self.sync_automatically(cx);
    }

    pub fn snapshot(&self, cx: &App) -> SyncSnapshot {
        SyncSnapshot {
            available: self.available(cx),
            location: self.location.clone(),
            phase: self.phase.clone(),
            attention: self.attention,
            device: self
                .services
                .as_ref()
                .map(|services| services.device.clone()),
            devices: self.devices.clone(),
            busy: self.busy(),
        }
    }

    fn available(&self, cx: &App) -> bool {
        self.services.is_some() && self.secrets(cx).is_available()
    }

    fn secrets(&self, cx: &App) -> SharedSecretStore {
        self.store.read(cx).secrets()
    }

    fn busy(&self) -> bool {
        self.round.is_some() || self.job_running
    }

    /// The location from the settings: sync is on while there is one.
    pub fn set_location(&mut self, location: Option<SyncLocation>, cx: &mut Context<Self>) {
        if location == self.location {
            return;
        }
        self.location = location;
        self.devices.clear();
        self.attention = None;
        self.synced_revision = None;
        self.phase = if self.location.is_some() {
            SyncPhase::Waiting
        } else {
            SyncPhase::Off
        };
        cx.notify();
        self.sync_automatically(cx);
    }

    /// 立即同步: a round now, whatever waited for the user (it asks again
    /// if it still does).
    pub fn sync_now(&mut self, cx: &mut Context<Self>) {
        self.attention = None;
        self.start_round(cx);
    }

    /// The window came forward: a round, unless one ran just now.
    pub fn window_activated(&mut self, cx: &mut Context<Self>) {
        if self
            .last_round_at
            .is_none_or(|at| at.elapsed() >= ACTIVATION_GAP)
        {
            self.sync_automatically(cx);
        }
    }

    /// The user agreed to the deletions a round would make.
    pub fn allow_mass_delete(&mut self, cx: &mut Context<Self>) {
        self.allow_mass_delete = true;
        self.sync_now(cx);
    }

    /// The user wants this computer back in sync, removed elsewhere as it
    /// was.
    pub fn accept_removed(&mut self, cx: &mut Context<Self>) {
        self.accept_removed = true;
        self.sync_now(cx);
    }

    fn sync_automatically(&mut self, cx: &mut Context<Self>) {
        if self.attention.is_none() {
            self.start_round(cx);
        }
    }

    /// A round once the user has stopped editing for a while.
    fn sync_after_pause(&mut self, cx: &mut Context<Self>) {
        if self.location.is_none() {
            return;
        }
        self.pause = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(EDIT_PAUSE).await;
            this.update(cx, |this, cx| this.sync_automatically(cx)).ok();
        }));
    }

    fn start_round(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            self.again = true;
            return;
        }
        let (Some(services), Some(location)) = (self.services.clone(), self.location.clone())
        else {
            return;
        };
        if !self.available(cx) {
            self.phase = SyncPhase::Failed(JobError::Unavailable);
            cx.notify();
            return;
        }
        self.pause = None;
        let opts = RoundOptions {
            allow_mass_delete: mem::take(&mut self.allow_mass_delete),
            join: JoinMode::Merge,
        };
        let accept_removed = mem::take(&mut self.accept_removed);
        let store = self.store.downgrade();
        self.phase = SyncPhase::Syncing;
        cx.notify();
        self.round = Some(cx.spawn(async move |this, cx| {
            let host = DesktopRound {
                store,
                cx: cx.clone(),
            };
            let ended = run_round(&host, &services, &location, opts, accept_removed).await;
            this.update(cx, |this, cx| this.round_ended(ended, cx)).ok();
        }));
    }

    fn round_ended(&mut self, ended: RoundEnd, cx: &mut Context<Self>) {
        self.round = None;
        self.last_round_at = Some(Instant::now());
        match ended {
            RoundEnd::Synced { revision, devices } => {
                self.phase = SyncPhase::Synced { at_ms: now_ms() };
                self.devices = devices;
                self.synced_revision = Some(revision);
            }
            RoundEnd::Attention(attention) => {
                // What the last round that went through found still stands.
                self.phase = match &self.phase {
                    SyncPhase::Syncing => SyncPhase::Waiting,
                    phase => phase.clone(),
                };
                if self.attention != Some(attention) {
                    self.attention = Some(attention);
                    cx.emit(SyncEvent::NeedsAttention(attention));
                }
            }
            RoundEnd::Failed(error) => self.phase = SyncPhase::Failed(error),
        }
        cx.notify();
        if mem::take(&mut self.again) {
            self.sync_automatically(cx);
        }
    }

    /// Before quitting: a round if anything is not synced yet, and a task
    /// that ends with it, or after [`QUIT_WAIT`]. `None` when there is
    /// nothing to wait for.
    pub fn sync_before_quit(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        if self.location.is_none() || self.attention.is_some() || !self.available(cx) {
            return None;
        }
        let revision = self.store.read(cx).sync_revision();
        if self.round.is_none() {
            if self.job_running || self.synced_revision == Some(revision) {
                return None;
            }
            self.start_round(cx);
        }
        Some(cx.spawn(async move |this, cx| {
            let mut waited = Duration::ZERO;
            while waited < QUIT_WAIT {
                cx.background_executor().timer(QUIT_POLL).await;
                waited += QUIT_POLL;
                let running = this.read_with(cx, |this, _| this.round.is_some());
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        }))
    }

    /// Whether sync is set up at `location` already: to join it, or to
    /// make it. A WebDAV server's opens with `login`, its password.
    pub fn inspect(
        &mut self,
        location: SyncLocation,
        login: Option<Zeroizing<String>>,
        cx: &mut Context<Self>,
    ) -> Task<Result<bool, JobError>> {
        self.job(location, login, cx, |remote, _, _| worker::inspect(remote))
    }

    /// 测试连接: write, read and delete a file at `location`, without
    /// keeping anything. Beside a round as well.
    pub fn test_connection(
        &mut self,
        location: SyncLocation,
        login: Option<Zeroizing<String>>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), JobError>> {
        let Some(services) = self.services.clone().filter(|_| self.available(cx)) else {
            return Task::ready(Err(JobError::Unavailable));
        };
        let secrets = self.secrets(cx);
        let probe = format!("probe-{:016x}.tmp", rand::random::<u64>());
        cx.background_executor().spawn(async move {
            let remote = worker::open(&services, &location, text(&login), &*secrets)?;
            worker::probe(&*remote, &probe)
        })
    }

    /// Set sync up at `location` with a new sync password. A WebDAV
    /// server's `login` is kept in the keychain once it is.
    pub fn create(
        &mut self,
        location: SyncLocation,
        login: Option<Zeroizing<String>>,
        password: Zeroizing<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), JobError>> {
        let kept = login.clone();
        let job = self.job(
            location.clone(),
            login,
            cx,
            move |remote, secrets, services| {
                worker::create(remote, secrets, &password, services.kdf)?;
                worker::keep_login(secrets, text(&kept))
            },
        );
        cx.spawn(async move |this, cx| {
            job.await?;
            this.update(cx, |this, cx| this.joined(location, true, cx))
                .ok();
            Ok(())
        })
    }

    /// Join the sync at `location` with its password. `fresh` forgets
    /// what this computer synced before: sync was reset, or this is its
    /// first time. The password changed elsewhere keeps it. A WebDAV
    /// server's `login`, when given, is kept in the keychain once joined.
    pub fn join(
        &mut self,
        location: SyncLocation,
        login: Option<Zeroizing<String>>,
        password: Zeroizing<String>,
        fresh: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), JobError>> {
        let kept = login.clone();
        let job = self.job(location.clone(), login, cx, move |remote, secrets, _| {
            worker::join(remote, secrets, &password)?;
            worker::keep_login(secrets, text(&kept))
        });
        cx.spawn(async move |this, cx| {
            job.await?;
            this.update(cx, |this, cx| this.joined(location, fresh, cx))
                .ok();
            Ok(())
        })
    }

    fn joined(&mut self, location: SyncLocation, fresh: bool, cx: &mut Context<Self>) {
        if fresh {
            self.store.update(cx, |store, _| store.clear_sync_state());
            self.synced_revision = None;
            self.devices.clear();
        }
        self.attention = None;
        self.location = Some(location.clone());
        if self.phase == SyncPhase::Off {
            self.phase = SyncPhase::Waiting;
        }
        cx.emit(SyncEvent::LocationSet(location));
        self.start_round(cx);
    }

    /// Sign in to the server or bucket sync goes through anew: at
    /// `location`, the same place as a new user or key, with `login`, which
    /// it must take before they are kept.
    pub fn change_login(
        &mut self,
        location: SyncLocation,
        login: Zeroizing<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), JobError>> {
        let same = self
            .location
            .as_ref()
            .is_some_and(|now| now.signs_in() && now.same_place(&location));
        if !same {
            return Task::ready(Err(JobError::Unavailable));
        }
        let kept = login.clone();
        let job = self.job(
            location.clone(),
            Some(login),
            cx,
            move |remote, secrets, _| {
                // The sync there is the one this computer joined: the round
                // tells, as for any other.
                if !worker::inspect(remote)? {
                    return Err(SyncError::VaultMissing.into());
                }
                worker::keep_login(secrets, Some(kept.as_str()))
            },
        );
        cx.spawn(async move |this, cx| {
            job.await?;
            this.update(cx, |this, cx| {
                this.attention = None;
                this.location = Some(location.clone());
                cx.emit(SyncEvent::LocationSet(location));
                this.start_round(cx);
            })
            .ok();
            Ok(())
        })
    }

    /// 更换同步密码, and the data key after it: what the other devices hold
    /// no longer opens anything new.
    pub fn change_password(
        &mut self,
        password: Zeroizing<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), JobError>> {
        let Some(location) = self.location.clone() else {
            return Task::ready(Err(JobError::Unavailable));
        };
        let job = self.job(location, None, cx, move |remote, secrets, services| {
            worker::new_password(remote, secrets, &password, services.kdf)
        });
        cx.spawn(async move |this, cx| {
            job.await?;
            // Written again with the new key.
            this.update(cx, |this, cx| this.start_round(cx)).ok();
            Ok(())
        })
    }

    /// 移除 a device: its file goes, and it leaves the list.
    pub fn remove_device(
        &mut self,
        device: DeviceId,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), JobError>> {
        let Some(location) = self.location.clone() else {
            return Task::ready(Err(JobError::Unavailable));
        };
        let gone = device.clone();
        let job = self.job(location, None, cx, move |remote, _, _| {
            worker::remove_device(remote, &device)
        });
        cx.spawn(async move |this, cx| {
            job.await?;
            this.update(cx, |this, cx| {
                this.devices.retain(|device| device.id != gone);
                cx.notify();
            })
            .ok();
            Ok(())
        })
    }

    /// 关闭同步: the data here stays as it is; the keys, a WebDAV password
    /// and the state go, and with `delete_file` this computer's file at the
    /// location too.
    pub fn turn_off(
        &mut self,
        delete_file: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), JobError>> {
        let Some(location) = self.location.clone() else {
            return Task::ready(Ok(()));
        };
        let state = self.store.read(cx).sync_state();
        let job = self.job_with(
            location,
            None,
            delete_file,
            cx,
            move |remote, secrets, _| {
                if let Some(remote) = remote
                    && let Some(state) = state.and_then(|bytes| SyncState::from_bytes(&bytes).ok())
                {
                    worker::remove_device(remote, state.device())?;
                }
                // Left in the keychain, they open nothing new once the next
                // set-up writes over them.
                let _ = worker::forget_keyring(secrets);
                let _ = worker::keep_login(secrets, None);
                Ok(())
            },
        );
        cx.spawn(async move |this, cx| {
            job.await?;
            this.update(cx, |this, cx| this.turned_off(cx)).ok();
            Ok(())
        })
    }

    fn turned_off(&mut self, cx: &mut Context<Self>) {
        self.store.update(cx, |store, _| store.clear_sync_state());
        self.location = None;
        self.phase = SyncPhase::Off;
        self.attention = None;
        self.devices.clear();
        self.synced_revision = None;
        self.again = false;
        cx.emit(SyncEvent::TurnedOff);
        cx.notify();
    }

    /// Run `work` on the background executor with the storage at
    /// `location` (a WebDAV server's opened with `login`, or the password
    /// the keychain keeps), alone: no round beside it.
    fn job<R: Send + 'static>(
        &mut self,
        location: SyncLocation,
        login: Option<Zeroizing<String>>,
        cx: &mut Context<Self>,
        work: impl FnOnce(&dyn SyncStore, &dyn SecretStore, &SyncServices) -> Result<R, JobError>
        + Send
        + 'static,
    ) -> Task<Result<R, JobError>> {
        self.job_with(
            location,
            login,
            true,
            cx,
            move |remote, secrets, services| work(remote.expect("opened"), secrets, services),
        )
    }

    /// As [`Self::job`], the storage opened only when `open`: turning sync
    /// off needs none unless it deletes a file.
    fn job_with<R: Send + 'static>(
        &mut self,
        location: SyncLocation,
        login: Option<Zeroizing<String>>,
        open: bool,
        cx: &mut Context<Self>,
        work: impl FnOnce(
            Option<&dyn SyncStore>,
            &dyn SecretStore,
            &SyncServices,
        ) -> Result<R, JobError>
        + Send
        + 'static,
    ) -> Task<Result<R, JobError>> {
        let Some(services) = self.services.clone().filter(|_| self.available(cx)) else {
            return Task::ready(Err(JobError::Unavailable));
        };
        if self.busy() {
            return Task::ready(Err(JobError::InProgress));
        }
        self.job_running = true;
        cx.notify();
        let secrets = self.secrets(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let remote = match open {
                        true => Some(worker::open(&services, &location, text(&login), &*secrets)?),
                        false => None,
                    };
                    work(remote.as_deref(), &*secrets, &services)
                })
                .await;
            this.update(cx, |this, cx| {
                this.job_running = false;
                cx.notify();
                if mem::take(&mut this.again) {
                    this.sync_automatically(cx);
                }
            })
            .ok();
            result
        })
    }
}

/// A round's way to the store: on the UI thread, settled after each use,
/// so a failure to save is reported as any edit's is.
struct DesktopRound {
    store: WeakEntity<HostStore>,
    cx: AsyncApp,
}

impl RoundHost for DesktopRound {
    fn with_data<R>(&self, f: impl FnOnce(&mut HostData) -> R) -> Option<R> {
        let mut cx = self.cx.clone();
        self.store
            .update(&mut cx, |store, cx| {
                let result = f(store);
                store.settle(cx);
                result
            })
            .ok()
    }

    fn background<R: Send + 'static>(&self, work: Blocking<R>) -> impl Future<Output = R> + Send {
        self.cx.background_executor().spawn(async move { work() })
    }
}

fn text(login: &Option<Zeroizing<String>>) -> Option<&str> {
    login.as_ref().map(|login| login.as_str())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}
