//! The updater: when to look, what was found, the download, and the switch
//! to the new version.
//!
//! Network and disk work runs on a `shellrs-update` thread per job; the
//! entity collects its events on a timer while a job runs, like the port
//! forwards do, so no worker ever wakes the window. Results carry the job's
//! generation and are dropped once a newer job has replaced it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_channel::{Receiver, Sender, TryRecvError};
use gpui_kit::*;
use rand::Rng as _;
use semver::Version;
use serde::{Deserialize, Serialize};

use super::build_info::{self, Channel};
use super::error::UpdateError;
use super::feed::{HttpFeed, UpdateFeed};
use super::install::{Installer, Launch, Relaunch, Staged, Unsupported, system_installer};
use super::manifest::{Offer, Release};
use super::verify::{Opened, TrustedKeys, open_envelope, verify_file};

/// The first look, a while after start so it does not compete with the
/// window coming up.
const FIRST_CHECK: Duration = Duration::from_secs(30);
/// Between looks, plus up to [`JITTER`].
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const JITTER: Duration = Duration::from_secs(30 * 60);
/// How often a running job's events are collected.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// A restart that has not ended ShellRS after this long has failed.
const RESTART_GRACE: Duration = Duration::from_secs(2);
/// Failed automatic downloads before the title bar offers to retry.
pub(super) const FAILURES_BEFORE_BADGE: u32 = 2;

/// What the updater works with. Production has the real ones
/// ([`UpdateServices::system`]); the UI tests inject fakes.
#[derive(Clone)]
pub struct UpdateServices {
    pub feed: Arc<dyn UpdateFeed>,
    pub installer: Arc<dyn Installer>,
    pub keys: TrustedKeys,
    /// `false` for a development build, which never looks.
    pub release_build: bool,
    pub current: Version,
    /// Downloads, and the note the next start reads.
    pub folder: PathBuf,
    /// This machine's own number for staged rollouts, in `0..1`.
    pub draw: f64,
}

impl UpdateServices {
    /// `bundle` is `cx.app_path()`, which on macOS is the app bundle.
    pub fn system(bundle: Option<PathBuf>) -> Self {
        let folder = crate::app::updates_dir();
        Self {
            feed: Arc::new(HttpFeed::system()),
            installer: system_installer(bundle),
            keys: TrustedKeys::builtin(),
            release_build: Channel::of_this_build().is_some(),
            current: build_info::version(),
            draw: rollout_draw(&folder),
            folder,
        }
    }
}

/// The number this machine drew for staged rollouts, drawn once and kept.
/// It never leaves the machine.
fn rollout_draw(folder: &Path) -> f64 {
    let file = folder.join("rollout");
    if let Some(draw) = fs::read_to_string(&file)
        .ok()
        .and_then(|text| text.trim().parse::<f64>().ok())
        .filter(|draw| (0.0..1.0).contains(draw))
    {
        return draw;
    }
    let draw = rand::rng().random_range(0.0..1.0);
    let _ = fs::create_dir_all(folder).and_then(|()| fs::write(&file, draw.to_string()));
    draw
}

/// Where the updater stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Never looks: a development build, or no services at all.
    Off,
    /// Has not looked yet.
    Idle,
    Checking,
    UpToDate,
    /// A newer version, not downloaded: automatic updates are off, or this
    /// copy cannot install it, or it needs the download page.
    Available,
    Downloading {
        done: u64,
        total: u64,
    },
    Verifying,
    /// Downloaded, verified and staged: installs on restart or quit.
    Ready,
    /// ShellRS is restarting or quitting into the new version.
    Installing,
    Failed {
        stage: Stage,
        error: UpdateError,
    },
}

/// What a failure interrupted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Check,
    Download,
    Install,
}

/// A newer version this copy cannot install itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ManualUpdate {
    pub version: Version,
    pub installer: Option<String>,
}

/// Everything the settings page, the title bar and the dialog show, as a
/// plain value: render callbacks never read the entity.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateSnapshot {
    pub phase: Phase,
    pub current: Version,
    pub release: Option<Release>,
    pub manual: Option<ManualUpdate>,
    /// Why this copy cannot install updates itself, when it cannot.
    pub unsupported: Option<Unsupported>,
    pub automatic: bool,
    /// Automatic downloads that failed in a row.
    pub download_failures: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdaterEvent {
    /// The phase or the version on offer changed. Download progress alone
    /// does not count: the window does not redraw for every chunk.
    Changed,
    /// This start follows an update. `completed` is false when ShellRS is
    /// still the version it was before.
    Updated {
        from: Version,
        to: Version,
        completed: bool,
    },
}

/// What a worker reports.
enum WorkerEvent {
    Checked(Result<Opened, UpdateError>),
    Progress(u64),
    Verifying,
    Staged(Result<Staged, UpdateError>),
}

struct Job {
    events: Receiver<WorkerEvent>,
    cancel: Arc<AtomicBool>,
}

pub struct Updater {
    services: Option<UpdateServices>,
    phase: Phase,
    release: Option<Release>,
    manual: Option<ManualUpdate>,
    staged: Option<Staged>,
    automatic: bool,
    channel: Channel,
    download_failures: u32,
    job: Option<Job>,
    polling: bool,
    _schedule: Option<Task<()>>,
    _quit: Option<Subscription>,
}

impl EventEmitter<UpdaterEvent> for Updater {}

impl Default for Updater {
    fn default() -> Self {
        Self::new()
    }
}

impl Updater {
    /// An updater with nothing to work with: it never looks. The workspace
    /// gives production its services.
    pub fn new() -> Self {
        Self {
            services: None,
            phase: Phase::Off,
            release: None,
            manual: None,
            staged: None,
            automatic: true,
            channel: Channel::Stable,
            download_failures: 0,
            job: None,
            polling: false,
            _schedule: None,
            _quit: None,
        }
    }

    pub fn set_services(&mut self, services: UpdateServices, cx: &mut Context<Self>) {
        self.cancel_job();
        self.phase = if services.release_build {
            Phase::Idle
        } else {
            Phase::Off
        };
        self.services = Some(services);
        self.release = None;
        self.manual = None;
        self.staged = None;
        self.changed(cx);
    }

    /// Look on a schedule, tidy up after the previous update, report it,
    /// and install a ready update when ShellRS quits. Production only.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.report_previous_update(cx);
        self._quit = Some(cx.on_app_quit(|this, cx| {
            this.install_on_quit(cx);
            async {}
        }));
        self._schedule = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            if this.update(cx, |this, cx| this.clean_up(cx)).is_err() {
                return;
            }
            loop {
                let looked = this.update(cx, |this, cx| {
                    if this.automatic {
                        this.check(cx);
                    }
                });
                if looked.is_err() {
                    break;
                }
                let jitter = rand::rng().random_range(0..JITTER.as_secs());
                let wait = CHECK_INTERVAL + Duration::from_secs(jitter);
                cx.background_executor().timer(wait).await;
            }
        }));
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        UpdateSnapshot {
            phase: self.phase.clone(),
            current: self
                .services
                .as_ref()
                .map_or_else(build_info::version, |services| services.current.clone()),
            release: self.release.clone(),
            manual: self.manual.clone(),
            unsupported: self.unsupported(),
            automatic: self.automatic,
            download_failures: self.download_failures,
        }
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Why this copy cannot install updates itself, when it cannot.
    pub fn unsupported(&self) -> Option<Unsupported> {
        let Some(services) = &self.services else {
            return Some(Unsupported::DevelopmentBuild);
        };
        if !services.release_build {
            return Some(Unsupported::DevelopmentBuild);
        }
        match services.installer.kind() {
            super::install::InstallKind::Unsupported(reason) => Some(reason.clone()),
            _ => None,
        }
    }

    fn installable(&self) -> bool {
        self.unsupported().is_none()
    }

    /// 自动升级: look on a schedule and download what is found. Turning it
    /// off stops a download in progress; turning it on downloads a version
    /// already found.
    pub fn set_automatic(&mut self, automatic: bool, cx: &mut Context<Self>) {
        if self.automatic == automatic {
            return;
        }
        self.automatic = automatic;
        if !automatic && matches!(self.phase, Phase::Downloading { .. }) {
            self.cancel_job();
            self.phase = Phase::Available;
        } else if automatic && self.phase == Phase::Available {
            self.download(cx);
        }
        self.changed(cx);
    }

    /// 更新渠道. The other channel offers other versions, so what was found,
    /// downloaded or is downloading is dropped, and the new channel is looked
    /// at right away when looking is automatic.
    pub fn set_channel(&mut self, channel: Channel, cx: &mut Context<Self>) {
        if self.channel == channel {
            return;
        }
        self.channel = channel;
        if matches!(self.phase, Phase::Off | Phase::Installing) {
            return;
        }
        self.cancel_job();
        self.release = None;
        self.manual = None;
        self.staged = None;
        self.download_failures = 0;
        self.phase = Phase::Idle;
        if self.automatic {
            self.check(cx);
        }
        self.changed(cx);
    }

    /// Look for a newer version now.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.phase,
            Phase::Idle
                | Phase::UpToDate
                | Phase::Available
                | Phase::Failed { .. }
                | Phase::Checking
        ) {
            return;
        }
        let Some(services) = self.services.clone() else {
            return;
        };
        if !services.release_build {
            return;
        }
        let channel = self.channel;
        self.phase = Phase::Checking;
        self.run(cx, move |events, _| {
            let opened = services
                .feed
                .fetch(channel)
                .and_then(|bytes| open_envelope(&bytes, channel, &services.keys));
            let _ = events.send_blocking(WorkerEvent::Checked(opened));
        });
        self.changed(cx);
    }

    /// Download the version found, when this copy can install it.
    pub fn download(&mut self, cx: &mut Context<Self>) {
        let ready_to_download = matches!(
            self.phase,
            Phase::Available
                | Phase::Failed {
                    stage: Stage::Download | Stage::Install,
                    ..
                }
        );
        if !ready_to_download || !self.installable() {
            return;
        }
        let (Some(services), Some(release)) = (self.services.clone(), self.release.clone()) else {
            return;
        };
        self.phase = Phase::Downloading {
            done: 0,
            total: release.asset.size,
        };
        self.run(cx, move |events, cancel| {
            let staged = download_and_stage(&services, &release, events, cancel);
            let _ = events.send_blocking(WorkerEvent::Staged(staged));
        });
        self.changed(cx);
    }

    /// Put the ready version in place and restart into it.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        let Some(relaunch) = self.apply(true, cx) else {
            return;
        };
        match relaunch {
            Relaunch::Restart(path) => {
                cx.set_restart_path(path);
                cx.restart();
                // A restart that cannot start the helper script leaves
                // ShellRS running; say so instead of looking stuck.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(RESTART_GRACE).await;
                    let _ = this.update(cx, |this, cx| {
                        if this.phase == Phase::Installing {
                            this.phase = Phase::Failed {
                                stage: Stage::Install,
                                error: UpdateError::Install(
                                    "无法自动重新启动，请手动重新打开 ShellRS".into(),
                                ),
                            };
                            this.changed(cx);
                        }
                    });
                })
                .detach();
            }
            Relaunch::Hand(launch) => {
                hand_over(launch);
                cx.quit();
            }
            Relaunch::Nothing => {}
        }
    }

    /// ShellRS is quitting with a version ready: put it in place now, the
    /// way it would have been on restart, so the next start runs it.
    fn install_on_quit(&mut self, cx: &mut Context<Self>) {
        if let Some(Relaunch::Hand(launch)) = self.apply(false, cx) {
            hand_over(launch);
        }
    }

    fn apply(&mut self, relaunch: bool, cx: &mut Context<Self>) -> Option<Relaunch> {
        if self.phase != Phase::Ready {
            return None;
        }
        let (services, staged) = (self.services.clone()?, self.staged.clone()?);
        let note = Applied {
            from: services.current.clone(),
            to: staged.version.clone(),
        };
        let note_file = services.folder.join(APPLIED);
        let _ = serde_json::to_vec(&note)
            .ok()
            .map(|bytes| fs::write(&note_file, bytes));
        match services.installer.apply(&staged, relaunch) {
            Ok(relaunch) => {
                self.phase = Phase::Installing;
                self.changed(cx);
                Some(relaunch)
            }
            Err(error) => {
                let _ = fs::remove_file(&note_file);
                self.staged = None;
                self.phase = Phase::Failed {
                    stage: Stage::Install,
                    error,
                };
                self.changed(cx);
                None
            }
        }
    }

    /// The download page, or this platform's installer when known.
    pub fn download_page(&self) -> String {
        self.release
            .as_ref()
            .and_then(|release| release.installer.clone())
            .or_else(|| {
                self.manual
                    .as_ref()
                    .and_then(|manual| manual.installer.clone())
            })
            .unwrap_or_else(|| build_info::DOWNLOAD_PAGE.to_string())
    }

    /// Read and remove the note the previous version left before it
    /// installed this one.
    fn report_previous_update(&mut self, cx: &mut Context<Self>) {
        let Some(services) = &self.services else {
            return;
        };
        let file = services.folder.join(APPLIED);
        let Ok(bytes) = fs::read(&file) else {
            return;
        };
        let _ = fs::remove_file(&file);
        if let Ok(note) = serde_json::from_slice::<Applied>(&bytes) {
            let completed = services.current == note.to;
            cx.emit(UpdaterEvent::Updated {
                from: note.from,
                to: note.to,
                completed,
            });
        }
    }

    /// Remove what earlier updates left: the installed copy's backups, and
    /// downloads for versions this one is not older than.
    fn clean_up(&mut self, cx: &mut Context<Self>) {
        let Some(services) = self.services.clone() else {
            return;
        };
        cx.background_spawn(async move {
            services.installer.clean_up();
            clean_downloads(&services.folder, &services.current);
        })
        .detach();
    }

    /// Start a job on its own thread, replacing the one running.
    fn run(
        &mut self,
        cx: &mut Context<Self>,
        job: impl FnOnce(&Sender<WorkerEvent>, &AtomicBool) + Send + 'static,
    ) {
        self.cancel_job();
        let (sender, events) = async_channel::unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("shellrs-update".into())
            .spawn(move || job(&sender, &worker_cancel));
        if let Err(error) = spawned {
            self.phase = Phase::Failed {
                stage: Stage::Check,
                error: UpdateError::Disk(error.to_string()),
            };
            return;
        }
        self.job = Some(Job { events, cancel });
        self.poll_while_running(cx);
    }

    fn cancel_job(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn poll_while_running(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let running = this.update(cx, |this, cx| {
                    this.collect_events(cx);
                    this.polling = this.job.is_some();
                    this.polling
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn collect_events(&mut self, cx: &mut Context<Self>) {
        while let Some(job) = &self.job {
            match job.events.try_recv() {
                Ok(event) => self.on_event(event, cx),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => {
                    self.job = None;
                    if matches!(
                        self.phase,
                        Phase::Checking | Phase::Downloading { .. } | Phase::Verifying
                    ) {
                        self.phase = Phase::Failed {
                            stage: Stage::Check,
                            error: UpdateError::Network("检查意外中止".into()),
                        };
                        self.changed(cx);
                    }
                }
            }
        }
    }

    fn on_event(&mut self, event: WorkerEvent, cx: &mut Context<Self>) {
        match event {
            WorkerEvent::Checked(result) => {
                self.job = None;
                self.on_checked(result, cx);
            }
            WorkerEvent::Progress(done) => {
                if let Phase::Downloading { total, .. } = self.phase {
                    self.phase = Phase::Downloading { done, total };
                    // Progress only: observers redraw, the window does not.
                    cx.notify();
                }
            }
            WorkerEvent::Verifying => {
                self.phase = Phase::Verifying;
                self.changed(cx);
            }
            WorkerEvent::Staged(result) => {
                self.job = None;
                match result {
                    Ok(staged) => {
                        self.staged = Some(staged);
                        self.download_failures = 0;
                        self.phase = Phase::Ready;
                    }
                    Err(UpdateError::Cancelled) => self.phase = Phase::Available,
                    Err(error) => {
                        self.download_failures += 1;
                        self.phase = Phase::Failed {
                            stage: Stage::Download,
                            error,
                        };
                    }
                }
                self.changed(cx);
            }
        }
    }

    fn on_checked(&mut self, result: Result<Opened, UpdateError>, cx: &mut Context<Self>) {
        let Some(services) = &self.services else {
            return;
        };
        let offer = match result {
            Ok(Opened::Manifest(manifest)) => manifest.offer(
                &services.current,
                super::platform::platform_key(),
                services.draw,
            ),
            Ok(Opened::NewerFormat { version }) if version > services.current => Offer::Manual {
                version,
                installer: None,
            },
            Ok(Opened::NewerFormat { .. }) => Offer::UpToDate,
            Err(error) => {
                self.phase = Phase::Failed {
                    stage: Stage::Check,
                    error,
                };
                self.changed(cx);
                return;
            }
        };
        match offer {
            Offer::UpToDate => {
                self.release = None;
                self.manual = None;
                self.phase = Phase::UpToDate;
            }
            Offer::Update(release) => {
                if self.release.as_ref().map(|known| &known.version) != Some(&release.version) {
                    self.download_failures = 0;
                }
                self.release = Some(release);
                self.manual = None;
                self.phase = Phase::Available;
                if self.automatic && self.installable() {
                    self.download(cx);
                }
            }
            Offer::Manual { version, installer } => {
                self.release = None;
                self.manual = Some(ManualUpdate { version, installer });
                self.phase = Phase::Available;
            }
        }
        self.changed(cx);
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(UpdaterEvent::Changed);
        cx.notify();
    }
}

/// The note `apply` leaves for the next start, in the downloads folder.
const APPLIED: &str = "applied.json";

#[derive(Debug, Serialize, Deserialize)]
struct Applied {
    from: Version,
    to: Version,
}

/// Download the release's package (or reuse one already verified), check
/// it, and stage it. Runs on the job's thread.
fn download_and_stage(
    services: &UpdateServices,
    release: &Release,
    events: &Sender<WorkerEvent>,
    cancel: &AtomicBool,
) -> Result<Staged, UpdateError> {
    let folder = services.folder.join(release.version.to_string());
    fs::create_dir_all(&folder)?;
    let package = folder.join(package_name(release));
    let verified = package.exists() && verify_file(&package, &release.asset).is_ok();
    if !verified {
        let _ = fs::remove_file(&package);
        let mut last_sent = 0;
        services.feed.download(
            &release.asset.urls,
            release.asset.size,
            &package,
            &mut |done| {
                // About every percent: the window does not need every chunk.
                if done == release.asset.size || done >= last_sent + release.asset.size / 100 {
                    last_sent = done;
                    let _ = events.send_blocking(WorkerEvent::Progress(done));
                }
            },
            cancel,
        )?;
        let _ = events.send_blocking(WorkerEvent::Verifying);
        if let Err(error) = verify_file(&package, &release.asset) {
            let _ = fs::remove_file(&package);
            return Err(error);
        }
    } else {
        let _ = events.send_blocking(WorkerEvent::Verifying);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    services.installer.stage(&package, release)
}

/// The package's file name, from its address: the installers recognise it
/// by its extension. Anything unusual in it is replaced.
fn package_name(release: &Release) -> String {
    let name = release
        .asset
        .urls
        .first()
        .and_then(|url| url.rsplit('/').next())
        .unwrap_or_default();
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.trim_matches(['.', '_']).is_empty() {
        "package".into()
    } else {
        name
    }
}

/// Remove downloads of versions not newer than `current`, and stray
/// partial ones.
fn clean_downloads(folder: &Path, current: &Version) {
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = Version::parse(&name).map_or(true, |version| version <= *current);
        if stale {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// Windows hands the install to a process started once ShellRS's event
/// loop has ended: starting a process can pump the message loop, which must
/// not happen while GPUI is busy quitting.
static HANDED_OVER: Mutex<Option<Launch>> = Mutex::new(None);

fn hand_over(launch: Launch) {
    *HANDED_OVER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(launch);
}

/// Start what an update handed over, if anything. `main` calls this after
/// the application has stopped running.
pub fn start_handed_over() {
    let launch = HANDED_OVER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(launch) = launch
        && let Err(error) = launch.spawn()
    {
        eprintln!("shellrs: 无法启动更新安装程序：{error}");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use semver::Version;

    use super::{clean_downloads, package_name, rollout_draw};
    use crate::update::feed::part_path;
    use crate::update::manifest::{Asset, Release};

    fn release(urls: &[&str]) -> Release {
        Release {
            version: Version::new(0, 2, 0),
            published_at: None,
            notes: String::new(),
            notes_url: None,
            asset: Asset {
                urls: urls.iter().map(|url| url.to_string()).collect(),
                size: 1,
                sha256: String::new(),
            },
            installer: None,
        }
    }

    #[test]
    fn a_package_is_named_after_its_address() {
        assert_eq!(
            package_name(&release(&[
                "https://dl.shellrs.com/releases/0.2.0/ShellRS-0.2.0-macos-universal.app.zip"
            ])),
            "ShellRS-0.2.0-macos-universal.app.zip"
        );
        assert_eq!(package_name(&release(&["https://x/a b?.exe"])), "a_b_.exe");
        assert_eq!(package_name(&release(&["https://x/.."])), "package");
        assert_eq!(package_name(&release(&[])), "package");
    }

    #[test]
    fn downloads_of_older_versions_are_cleaned_up() {
        let folder = tempfile::tempdir().unwrap();
        for name in ["0.1.0", "0.2.0", "0.3.0", "junk"] {
            fs::create_dir_all(folder.path().join(name)).unwrap();
        }
        fs::write(folder.path().join("rollout"), "0.5").unwrap();
        clean_downloads(folder.path(), &Version::new(0, 2, 0));
        let mut left: Vec<String> = fs::read_dir(folder.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["0.3.0", "rollout"]);
    }

    #[test]
    fn the_rollout_draw_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let first = rollout_draw(folder.path());
        assert!((0.0..1.0).contains(&first));
        assert_eq!(rollout_draw(folder.path()), first);
        fs::write(folder.path().join("rollout"), "7").unwrap();
        assert!((0.0..1.0).contains(&rollout_draw(folder.path())));
    }

    #[test]
    fn a_partial_download_sits_next_to_its_package() {
        assert_eq!(
            part_path(Path::new("/u/0.2.0/setup.exe")),
            Path::new("/u/0.2.0/setup.exe.part")
        );
    }
}
