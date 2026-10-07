//! What is counted, what is sent and when. Everything here takes the time
//! as an argument, so the tests can move it on.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::path::Path;

use chrono::{DateTime, NaiveDate, SecondsFormat, TimeDelta, Utc};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// How long uses are summed before they go out as one `usage` event,
/// counted from the first.
const SUMMARY_SPAN: TimeDelta = TimeDelta::hours(4);
/// Aptabase drops an event older than a day: nothing is sent stamped older
/// than this, and a queued event that gets older is dropped here.
const MAX_AGE: TimeDelta = TimeDelta::hours(23);
/// A session ends after this long without an event, as in Aptabase's SDKs.
const SESSION_TIMEOUT: TimeDelta = TimeDelta::hours(1);
/// The most events Aptabase takes in one request.
const BATCH_LIMIT: usize = 25;
/// The most lifecycle events kept while they cannot be sent.
const QUEUE_LIMIT: usize = 50;
/// The longest wait before trying again after failures in a row.
const LONGEST_WAIT: TimeDelta = TimeDelta::hours(6);
/// A development build's `SUMMARY_SPAN`, and its wait after any failure.
const DEBUG_SUMMARY_SPAN: TimeDelta = TimeDelta::minutes(1);
const DEBUG_WAIT: TimeDelta = TimeDelta::seconds(10);
/// Which SDK sent the events, as Aptabase's own SDKs name themselves.
const SDK_VERSION: &str = concat!("shellrs@", env!("CARGO_PKG_VERSION"));

/// A use of a feature, summed into the next `usage` event. The keys are
/// what the dashboard shows and the manual lists: never rename one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Counter {
    /// An SSH terminal connected, a reconnection too. The `Ssh*` below
    /// count the same connections again, by how they were made.
    Ssh,
    SshJump,
    SshProxy,
    SshCredential,
    SshNoPassword,
    /// 临时连接.
    SshTemporary,
    /// 外部连接: opened by a link from a bastion host.
    SshExternal,
    /// An SFTP tab connected, a reconnection too.
    Sftp,
    SftpTemporary,
    SftpExternal,
    LocalTerminal,
    /// A port forward started, by the user or with ShellRS.
    ForwardLocal,
    ForwardRemote,
    ForwardDynamic,
    /// A batch of transfers queued in an SFTP tab.
    Upload,
    Download,
    /// A file opened in the editor.
    EditorLocal,
    EditorRemote,
    PreviewImage,
    PreviewMarkdown,
    /// A tool of the right sidebar opened.
    ToolSnippets,
    ToolHistory,
    ToolDocker,
    ToolServices,
    ToolProcesses,
    ToolNetstat,
    ToolMonitor,
    /// A snippet, or a command from the history, put into a terminal.
    CommandSnippet,
    CommandHistory,
    /// The terminal's find bar opened.
    Find,
    /// A notification shown: a program's, the bell, a highlight rule's.
    NoticeProgram,
    NoticeBell,
    NoticeKeyword,
    /// A `shellrs` command served.
    CliExec,
    CliUpload,
    CliDownload,
    CliSync,
    CliHosts,
    CliCredentials,
    /// 关键字高亮 and 启用外部 CLI switched.
    HighlightOn,
    HighlightOff,
    ExternalCliOn,
    ExternalCliOff,
}

impl Counter {
    pub const ALL: [Counter; 43] = [
        Self::Ssh,
        Self::SshJump,
        Self::SshProxy,
        Self::SshCredential,
        Self::SshNoPassword,
        Self::SshTemporary,
        Self::SshExternal,
        Self::Sftp,
        Self::SftpTemporary,
        Self::SftpExternal,
        Self::LocalTerminal,
        Self::ForwardLocal,
        Self::ForwardRemote,
        Self::ForwardDynamic,
        Self::Upload,
        Self::Download,
        Self::EditorLocal,
        Self::EditorRemote,
        Self::PreviewImage,
        Self::PreviewMarkdown,
        Self::ToolSnippets,
        Self::ToolHistory,
        Self::ToolDocker,
        Self::ToolServices,
        Self::ToolProcesses,
        Self::ToolNetstat,
        Self::ToolMonitor,
        Self::CommandSnippet,
        Self::CommandHistory,
        Self::Find,
        Self::NoticeProgram,
        Self::NoticeBell,
        Self::NoticeKeyword,
        Self::CliExec,
        Self::CliUpload,
        Self::CliDownload,
        Self::CliSync,
        Self::CliHosts,
        Self::CliCredentials,
        Self::HighlightOn,
        Self::HighlightOff,
        Self::ExternalCliOn,
        Self::ExternalCliOff,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Ssh => "ssh",
            Self::SshJump => "ssh_jump",
            Self::SshProxy => "ssh_proxy",
            Self::SshCredential => "ssh_credential",
            Self::SshNoPassword => "ssh_no_password",
            Self::SshTemporary => "ssh_temporary",
            Self::SshExternal => "ssh_external",
            Self::Sftp => "sftp",
            Self::SftpTemporary => "sftp_temporary",
            Self::SftpExternal => "sftp_external",
            Self::LocalTerminal => "local_terminal",
            Self::ForwardLocal => "forward_local",
            Self::ForwardRemote => "forward_remote",
            Self::ForwardDynamic => "forward_dynamic",
            Self::Upload => "upload",
            Self::Download => "download",
            Self::EditorLocal => "editor_local",
            Self::EditorRemote => "editor_remote",
            Self::PreviewImage => "preview_image",
            Self::PreviewMarkdown => "preview_markdown",
            Self::ToolSnippets => "tool_snippets",
            Self::ToolHistory => "tool_history",
            Self::ToolDocker => "tool_docker",
            Self::ToolServices => "tool_services",
            Self::ToolProcesses => "tool_processes",
            Self::ToolNetstat => "tool_netstat",
            Self::ToolMonitor => "tool_monitor",
            Self::CommandSnippet => "command_snippet",
            Self::CommandHistory => "command_history",
            Self::Find => "find",
            Self::NoticeProgram => "notice_program",
            Self::NoticeBell => "notice_bell",
            Self::NoticeKeyword => "notice_keyword",
            Self::CliExec => "cli_exec",
            Self::CliUpload => "cli_upload",
            Self::CliDownload => "cli_download",
            Self::CliSync => "cli_sync",
            Self::CliHosts => "cli_hosts",
            Self::CliCredentials => "cli_credentials",
            Self::HighlightOn => "highlight_on",
            Self::HighlightOff => "highlight_off",
            Self::ExternalCliOn => "external_cli_on",
            Self::ExternalCliOff => "external_cli_off",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|counter| counter.key() == key)
    }
}

/// How ShellRS is set up, sent with the lifecycle events. Every value is
/// from a fixed set, which the types keep to: nothing the user typed fits.
/// The workspace fills it in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// 应用外观: `dark`, `light` or `system`.
    pub appearance: &'static str,
    /// 界面语言, as the settings file spells it.
    pub language: &'static str,
    /// The built-in themes chosen, by key.
    pub light_theme: &'static str,
    pub dark_theme: &'static str,
    pub ui_follows_theme: bool,
    pub highlight: bool,
    pub external_cli: bool,
    pub auto_update: bool,
    /// `stable` or `beta`.
    pub update_channel: &'static str,
    /// Whether any keyboard shortcut was changed.
    pub custom_shortcuts: bool,
    /// How many are saved; only their band goes out.
    pub hosts: usize,
    pub credentials: usize,
    pub forwards: usize,
    pub snippets: usize,
}

impl Snapshot {
    fn props(&self) -> Map<String, Value> {
        let switch = |on: bool| if on { "on" } else { "off" };
        [
            ("arch", std::env::consts::ARCH),
            ("appearance", self.appearance),
            ("language", self.language),
            ("light_theme", self.light_theme),
            ("dark_theme", self.dark_theme),
            ("ui_follows_theme", switch(self.ui_follows_theme)),
            ("highlight", switch(self.highlight)),
            ("external_cli", switch(self.external_cli)),
            ("auto_update", switch(self.auto_update)),
            ("update_channel", self.update_channel),
            (
                "custom_shortcuts",
                if self.custom_shortcuts { "yes" } else { "no" },
            ),
            ("hosts", band(self.hosts)),
            ("credentials", band(self.credentials)),
            ("forwards", band(self.forwards)),
            ("snippets", band(self.snippets)),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), Value::from(value)))
        .collect()
    }
}

/// Roughly how many: enough to tell a few from hundreds.
fn band(count: usize) -> &'static str {
    match count {
        0 => "0",
        1..=5 => "1-5",
        6..=20 => "6-20",
        21..=100 => "21-100",
        _ => "100+",
    }
}

/// What the workspace tells the analytics thread.
#[derive(Clone, Debug, PartialEq)]
pub enum Signal {
    /// ShellRS started.
    Started(Snapshot),
    /// The window came forward.
    Active(Snapshot),
    /// A feature was used.
    Count(Counter),
    /// 发送匿名使用统计 was switched.
    Enabled(bool),
}

/// One Aptabase event, before the system's part is added.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub name: &'static str,
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub props: Map<String, Value>,
}

/// What Aptabase hears about the build and the machine with each event.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemProps {
    /// A development build's events go to the dashboard's debug view.
    pub is_debug: bool,
    pub os_name: String,
    pub os_version: String,
    pub app_version: String,
}

/// The request body: `events` as a JSON array. Each field is cut to what
/// Aptabase takes, since one field too long fails the whole batch.
pub fn body(events: &[Event], system: &SystemProps) -> Vec<u8> {
    let system = json!({
        "isDebug": system.is_debug,
        "osName": clip(&system.os_name, 30),
        "osVersion": clip(&system.os_version, 100),
        "appVersion": clip(&system.app_version, 50),
        "sdkVersion": SDK_VERSION,
    });
    let events: Vec<Value> = events
        .iter()
        .map(|event| {
            json!({
                "timestamp": event.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
                "sessionId": event.session_id,
                "eventName": event.name,
                "systemProps": system,
                "props": event.props,
            })
        })
        .collect();
    serde_json::to_vec(&events).unwrap_or_default()
}

fn clip(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// Where an app key's events go: the key names Aptabase's region
/// (`A-US-…`). `None` for a key ShellRS cannot send with, a self-hosted or
/// malformed one.
pub fn endpoint(app_key: &str) -> Option<&'static str> {
    let parts: Vec<&str> = app_key.split('-').collect();
    let [first, region, number] = parts.as_slice() else {
        return None;
    };
    if *first != "A" || number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match *region {
        "US" => Some("https://us.aptabase.com/api/v0/events"),
        "EU" => Some("https://eu.aptabase.com/api/v0/events"),
        _ => None,
    }
}

/// How a request went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Aptabase has the events.
    Sent,
    /// The network or Aptabase failed for now: try again later.
    Retry,
    /// Aptabase refused them (a wrong key, an account over its quota):
    /// sending them again would not help.
    Rejected,
}

/// Uses summed since a moment. Times are Unix seconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tally {
    /// The first use counted.
    pub since: i64,
    /// The latest.
    pub last: i64,
    pub counts: BTreeMap<String, u64>,
}

impl Tally {
    fn new(now: DateTime<Utc>) -> Self {
        Self {
            since: now.timestamp(),
            last: now.timestamp(),
            counts: BTreeMap::new(),
        }
    }

    fn add(&mut self, counter: Counter, now: DateTime<Utc>) {
        let count = self.counts.entry(counter.key().to_string()).or_default();
        *count = count.saturating_add(1);
        self.last = self.last.max(now.timestamp());
    }

    fn merge(&mut self, other: Tally) {
        self.since = self.since.min(other.since);
        self.last = self.last.max(other.last);
        for (key, count) in other.counts {
            let sum = self.counts.entry(key).or_default();
            *sum = sum.saturating_add(count);
        }
    }

    fn props(&self) -> Map<String, Value> {
        self.counts
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(key, count)| (key.clone(), Value::from(*count)))
            .collect()
    }

    /// Only the counters this version knows, so whatever went out is one
    /// of them; `None` when nothing is left.
    fn known(mut self) -> Option<Self> {
        self.counts
            .retain(|key, count| *count > 0 && Counter::from_key(key).is_some());
        (!self.counts.is_empty()).then_some(self)
    }
}

/// What outlives the process, in `analytics.json`: whether this data
/// directory's installation was reported, and the uses not sent yet.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// `app_installed` reached Aptabase.
    pub installed: bool,
    /// Uses since the last summary.
    pub counting: Option<Tally>,
    /// Summaries that could not be sent yet, merged into one.
    pub unsent: Option<Tally>,
}

impl State {
    /// The state kept in `path`. None there, or one that cannot be read,
    /// starts afresh: the worst that costs is an installation reported
    /// twice.
    pub fn load(path: &Path) -> Self {
        let state: Self = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            installed: state.installed,
            counting: state.counting.and_then(Tally::known),
            unsent: state.unsent.and_then(Tally::known),
        }
    }

    /// Write it whole, through a temporary file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec(self)?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, json)?;
        fs::rename(&temporary, path)
    }
}

/// Aptabase's session: a new one after an hour without events. The id is
/// the start in seconds followed by eight random digits, as its SDKs make
/// them; the server reads the start back out of it.
#[derive(Default)]
struct Session {
    id: String,
    last: Option<DateTime<Utc>>,
}

impl Session {
    fn at(&mut self, now: DateTime<Utc>) -> String {
        if self.last.is_none_or(|last| now - last > SESSION_TIMEOUT) {
            let random: u64 = rand::rng().random_range(0..=99_999_999);
            let start = u64::try_from(now.timestamp()).unwrap_or(0);
            self.id = (start * 100_000_000 + random).to_string();
        }
        self.last = Some(now);
        self.id.clone()
    }
}

/// A request's worth of events, and what `Reporter::settle` needs to know
/// about them.
#[derive(Debug)]
pub struct Batch {
    pub events: Vec<Event>,
    /// How many came from the front of the queue.
    queued: usize,
    /// Whether the last is the summary of `State::unsent`.
    summary: bool,
}

/// How soon uses go out, and failures are tried again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pace {
    /// Uses summed for four hours; after a failure a minute's wait, twice
    /// as long after each next, at most six hours.
    #[default]
    Release,
    /// A development build being tried out, so what it does shows in the
    /// dashboard soon, as Aptabase's SDKs flush every two seconds in debug:
    /// uses summed for a minute, ten seconds' wait after any failure.
    Debug,
}

impl Pace {
    fn summary_span(self) -> TimeDelta {
        match self {
            Pace::Release => SUMMARY_SPAN,
            Pace::Debug => DEBUG_SUMMARY_SPAN,
        }
    }

    /// How long to wait after `failures` failures in a row.
    fn wait_after(self, failures: u32) -> TimeDelta {
        match self {
            Pace::Release => {
                let doublings = failures.saturating_sub(1).min(16);
                TimeDelta::minutes(1 << doublings).min(LONGEST_WAIT)
            }
            Pace::Debug => DEBUG_WAIT,
        }
    }
}

/// Works out what is sent and when, from the signals and the time. The
/// thread around it only moves bytes.
#[derive(Default)]
pub struct Reporter {
    state: State,
    pace: Pace,
    /// Lifecycle events not sent yet, oldest first.
    queue: VecDeque<Event>,
    session: Session,
    /// The UTC day of the last `app_started` or `app_active`: Aptabase
    /// counts devices by UTC day.
    reported_day: Option<NaiveDate>,
    /// After a failure, nothing is sent before this.
    retry_at: Option<DateTime<Utc>>,
    failures: u32,
    /// Whether `state` changed since `take_changed` last asked.
    changed: bool,
}

impl Reporter {
    pub fn new(state: State, pace: Pace) -> Self {
        Self {
            state,
            pace,
            ..Self::default()
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Whether the state changed since last asked, and so wants saving.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn signal(&mut self, signal: Signal, now: DateTime<Utc>) {
        match signal {
            Signal::Started(snapshot) => {
                if !self.state.installed {
                    self.push("app_installed", snapshot.props(), now);
                }
                self.push("app_started", snapshot.props(), now);
                self.reported_day = Some(now.date_naive());
            }
            Signal::Active(snapshot) => {
                if self.reported_day != Some(now.date_naive()) {
                    self.push("app_active", snapshot.props(), now);
                    self.reported_day = Some(now.date_naive());
                }
            }
            Signal::Count(counter) => {
                self.state
                    .counting
                    .get_or_insert_with(|| Tally::new(now))
                    .add(counter, now);
                self.changed = true;
            }
            Signal::Enabled(true) => {}
            // Whatever was waiting goes; only knowing the installation was
            // reported stays. Turned back on, the day is reported again.
            Signal::Enabled(false) => {
                self.queue.clear();
                let counted = self.state.counting.take().is_some();
                let unsent = self.state.unsent.take().is_some();
                self.changed |= counted || unsent;
                self.reported_day = None;
                self.retry_at = None;
                self.failures = 0;
            }
        }
    }

    fn push(&mut self, name: &'static str, props: Map<String, Value>, now: DateTime<Utc>) {
        let session_id = self.session.at(now);
        if self.queue.len() == QUEUE_LIMIT {
            self.queue.pop_front();
        }
        self.queue.push_back(Event {
            name,
            timestamp: now,
            session_id,
            props,
        });
    }

    /// The events to send now, or `None`: nothing is due, or a failure
    /// asked to wait. Uses go out once four hours (a minute in a
    /// development build) have passed since the first of them.
    pub fn take_batch(&mut self, now: DateTime<Utc>) -> Option<Batch> {
        let span = self.pace.summary_span().num_seconds();
        if let Some(counting) = self
            .state
            .counting
            .take_if(|counting| now.timestamp() - counting.since >= span)
        {
            match &mut self.state.unsent {
                Some(unsent) => unsent.merge(counting),
                None => self.state.unsent = Some(counting),
            }
            self.changed = true;
        }
        if self.retry_at.is_some_and(|at| now < at) {
            return None;
        }
        let oldest = now - MAX_AGE;
        self.queue.retain(|event| event.timestamp >= oldest);
        let summary = self.state.unsent.as_ref().map(|unsent| {
            // When the uses were, as near as Aptabase still takes.
            let last = DateTime::from_timestamp(unsent.last, 0).unwrap_or(now);
            (last.clamp(oldest, now), unsent.props())
        });
        let room = BATCH_LIMIT - usize::from(summary.is_some());
        let queued = self.queue.len().min(room);
        let mut events: Vec<Event> = self.queue.iter().take(queued).cloned().collect();
        let has_summary = summary.is_some();
        if let Some((timestamp, props)) = summary {
            events.push(Event {
                name: "usage",
                timestamp,
                session_id: self.session.at(now),
                props,
            });
        }
        (!events.is_empty()).then_some(Batch {
            events,
            queued,
            summary: has_summary,
        })
    }

    /// Take in how `batch` went: sent or refused, its events go; failed,
    /// they wait as long as the pace says.
    pub fn settle(&mut self, batch: Batch, delivery: Delivery, now: DateTime<Utc>) {
        if delivery == Delivery::Retry {
            self.failures = self.failures.saturating_add(1);
            self.retry_at = Some(now + self.pace.wait_after(self.failures));
            return;
        }
        let installed = delivery == Delivery::Sent
            && batch.events[..batch.queued]
                .iter()
                .any(|event| event.name == "app_installed");
        self.queue.drain(..batch.queued);
        if batch.summary {
            self.state.unsent = None;
            self.changed = true;
        }
        if installed {
            self.state.installed = true;
            self.changed = true;
        }
        self.failures = 0;
        self.retry_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-10-07T{hour:02}:{minute:02}:00Z"))
            .unwrap()
            .to_utc()
    }

    fn days_later(time: DateTime<Utc>, days: i64) -> DateTime<Utc> {
        time + TimeDelta::days(days)
    }

    fn names(batch: &Option<Batch>) -> Vec<&'static str> {
        batch
            .iter()
            .flat_map(|batch| batch.events.iter().map(|event| event.name))
            .collect()
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            appearance: "system",
            language: "zh-CN",
            light_theme: "shellrs-light",
            dark_theme: "shellrs-dark",
            update_channel: "stable",
            hosts: 7,
            ..Snapshot::default()
        }
    }

    fn system() -> SystemProps {
        SystemProps {
            is_debug: false,
            os_name: "macOS".into(),
            os_version: "15.1.0".into(),
            app_version: "0.1.5".into(),
        }
    }

    /// A reporter that already sent this launch's events.
    fn started_at(now: DateTime<Utc>) -> Reporter {
        let mut reporter = Reporter::new(
            State {
                installed: true,
                ..State::default()
            },
            Pace::Release,
        );
        reporter.signal(Signal::Started(snapshot()), now);
        let batch = reporter.take_batch(now).unwrap();
        reporter.settle(batch, Delivery::Sent, now);
        reporter
    }

    #[test]
    fn the_region_comes_from_the_app_key() {
        assert_eq!(
            endpoint("A-US-1582708162"),
            Some("https://us.aptabase.com/api/v0/events")
        );
        assert_eq!(
            endpoint("A-EU-1234567890"),
            Some("https://eu.aptabase.com/api/v0/events")
        );
        for key in [
            "A-SH-1234567890",
            "A-DEV-1",
            "A-US-",
            "A-US-12x",
            "B-US-1",
            "A-US",
        ] {
            assert_eq!(endpoint(key), None, "{key}");
        }
        assert!(endpoint(super::super::APP_KEY).is_some());
    }

    #[test]
    fn the_body_is_what_aptabase_reads() {
        let mut props = Map::new();
        props.insert("ssh".into(), Value::from(3));
        let event = Event {
            name: "usage",
            timestamp: at(9, 30),
            session_id: "176000000012345678".into(),
            props,
        };
        let mut system = system();
        system.os_name = "x".repeat(40);
        let json: Value = serde_json::from_slice(&body(&[event], &system)).unwrap();
        let event = &json[0];
        assert_eq!(event["timestamp"], "2026-10-07T09:30:00.000Z");
        assert_eq!(event["sessionId"], "176000000012345678");
        assert_eq!(event["eventName"], "usage");
        assert_eq!(event["props"]["ssh"], 3);
        let props = &event["systemProps"];
        assert_eq!(props["isDebug"], false);
        assert_eq!(props["osName"].as_str().unwrap().len(), 30);
        assert_eq!(props["osVersion"], "15.1.0");
        assert_eq!(props["appVersion"], "0.1.5");
        assert!(
            props["sdkVersion"]
                .as_str()
                .unwrap()
                .starts_with("shellrs@")
        );
        // Nothing about where the user is or what language they read.
        assert!(props.get("locale").is_none());
    }

    #[test]
    fn the_snapshot_holds_bands_and_switches() {
        let props = snapshot().props();
        assert_eq!(props["arch"], std::env::consts::ARCH);
        assert_eq!(props["hosts"], "6-20");
        assert_eq!(props["credentials"], "0");
        assert_eq!(props["highlight"], "off");
        assert_eq!(props["custom_shortcuts"], "no");
        assert!(props.values().all(Value::is_string));
        assert_eq!(band(5), "1-5");
        assert_eq!(band(100), "21-100");
        assert_eq!(band(101), "100+");
    }

    #[test]
    fn every_counter_has_a_key_of_its_own() {
        let mut keys: Vec<&str> = Counter::ALL.iter().map(|c| c.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Counter::ALL.len());
        assert!(keys.iter().all(|key| key.len() <= 40));
        for counter in Counter::ALL {
            assert_eq!(Counter::from_key(counter.key()), Some(counter));
        }
    }

    #[test]
    fn the_first_start_also_reports_the_installation() {
        let mut reporter = Reporter::new(State::default(), Pace::Release);
        reporter.signal(Signal::Started(snapshot()), at(9, 0));
        let batch = reporter.take_batch(at(9, 0));
        assert_eq!(names(&batch), ["app_installed", "app_started"]);
        let batch = batch.unwrap();
        assert_eq!(batch.events[0].session_id, batch.events[1].session_id);
        reporter.settle(batch, Delivery::Sent, at(9, 0));
        assert!(reporter.state().installed);
        assert!(reporter.take_changed());
        assert!(reporter.take_batch(at(9, 1)).is_none());
    }

    #[test]
    fn an_installation_counts_as_reported_only_once_it_arrived() {
        let mut reporter = Reporter::new(State::default(), Pace::Release);
        reporter.signal(Signal::Started(snapshot()), at(9, 0));
        let batch = reporter.take_batch(at(9, 0)).unwrap();
        reporter.settle(batch, Delivery::Rejected, at(9, 0));
        assert!(!reporter.state().installed);
    }

    #[test]
    fn a_later_start_reports_only_itself() {
        let mut reporter = Reporter::new(
            State {
                installed: true,
                ..State::default()
            },
            Pace::Release,
        );
        reporter.signal(Signal::Started(snapshot()), at(9, 0));
        assert_eq!(names(&reporter.take_batch(at(9, 0))), ["app_started"]);
    }

    #[test]
    fn the_window_coming_forward_reports_each_day_once() {
        let mut reporter = started_at(at(9, 0));
        reporter.signal(Signal::Active(snapshot()), at(15, 0));
        assert!(reporter.take_batch(at(15, 0)).is_none());
        let tomorrow = days_later(at(1, 0), 1);
        reporter.signal(Signal::Active(snapshot()), tomorrow);
        let batch = reporter.take_batch(tomorrow);
        assert_eq!(names(&batch), ["app_active"]);
        reporter.settle(batch.unwrap(), Delivery::Sent, tomorrow);
        reporter.signal(Signal::Active(snapshot()), tomorrow + TimeDelta::hours(2));
        assert!(
            reporter
                .take_batch(tomorrow + TimeDelta::hours(2))
                .is_none()
        );
    }

    #[test]
    fn uses_go_out_four_hours_after_the_first() {
        let mut reporter = started_at(at(9, 0));
        reporter.signal(Signal::Count(Counter::Ssh), at(9, 10));
        reporter.signal(Signal::Count(Counter::Ssh), at(10, 0));
        reporter.signal(Signal::Count(Counter::Upload), at(12, 0));
        assert!(reporter.take_changed());
        assert!(reporter.take_batch(at(13, 9)).is_none());
        let batch = reporter.take_batch(at(13, 10));
        assert_eq!(names(&batch), ["usage"]);
        let batch = batch.unwrap();
        let usage = &batch.events[0];
        assert_eq!(usage.props["ssh"], 2);
        assert_eq!(usage.props["upload"], 1);
        assert_eq!(usage.props.len(), 2);
        // Stamped when the uses were.
        assert_eq!(usage.timestamp, at(12, 0));
        reporter.settle(batch, Delivery::Sent, at(13, 10));
        assert_eq!(reporter.state().counting, None);
        assert_eq!(reporter.state().unsent, None);
        // The next uses start a summary of their own.
        reporter.signal(Signal::Count(Counter::Find), at(14, 0));
        assert!(reporter.take_batch(at(14, 0)).is_none());
    }

    #[test]
    fn uses_from_an_earlier_run_go_out_with_the_start() {
        let first = started_at(at(9, 0));
        let mut first = first;
        first.signal(Signal::Count(Counter::Sftp), at(10, 0));
        let saved = first.state().clone();
        // Quit before the four hours; started again the next day.
        let mut reporter = Reporter::new(saved, Pace::Release);
        let tomorrow = days_later(at(8, 0), 1);
        reporter.signal(Signal::Started(snapshot()), tomorrow);
        let batch = reporter.take_batch(tomorrow);
        assert_eq!(names(&batch), ["app_started", "usage"]);
        assert_eq!(batch.unwrap().events[1].timestamp, at(10, 0));
    }

    #[test]
    fn uses_kept_for_days_are_stamped_as_late_as_aptabase_takes() {
        let mut reporter = started_at(at(9, 0));
        reporter.signal(Signal::Count(Counter::Find), at(10, 0));
        let later = days_later(at(12, 0), 3);
        reporter.signal(Signal::Active(snapshot()), later);
        let batch = reporter.take_batch(later).unwrap();
        let usage = batch.events.iter().find(|e| e.name == "usage").unwrap();
        assert_eq!(usage.timestamp, later - MAX_AGE);
    }

    #[test]
    fn a_summary_that_failed_is_sent_with_the_next() {
        let mut reporter = started_at(at(0, 0));
        reporter.signal(Signal::Count(Counter::Ssh), at(0, 10));
        let batch = reporter.take_batch(at(4, 10)).unwrap();
        reporter.settle(batch, Delivery::Retry, at(4, 10));
        reporter.signal(Signal::Count(Counter::Ssh), at(5, 0));
        reporter.signal(Signal::Count(Counter::Download), at(6, 0));
        // Sent again once the wait is over, on the next signal; the newer
        // uses are still summing.
        let batch = reporter.take_batch(at(6, 0)).unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].props["ssh"], 1);
        reporter.settle(batch, Delivery::Retry, at(6, 0));
        let batch = reporter.take_batch(at(9, 30)).unwrap();
        assert_eq!(batch.events[0].props["ssh"], 2);
        assert_eq!(batch.events[0].props["download"], 1);
    }

    #[test]
    fn failures_wait_longer_each_time_up_to_six_hours() {
        assert_eq!(Pace::Release.wait_after(1), TimeDelta::minutes(1));
        assert_eq!(Pace::Release.wait_after(2), TimeDelta::minutes(2));
        assert_eq!(Pace::Release.wait_after(5), TimeDelta::minutes(16));
        assert_eq!(
            Pace::Release.wait_after(9),
            TimeDelta::hours(4) + TimeDelta::minutes(16)
        );
        assert_eq!(Pace::Release.wait_after(10), TimeDelta::hours(6));
        assert_eq!(Pace::Release.wait_after(u32::MAX), TimeDelta::hours(6));

        let mut reporter = Reporter::new(State::default(), Pace::Release);
        reporter.signal(Signal::Started(snapshot()), at(9, 0));
        let batch = reporter.take_batch(at(9, 0)).unwrap();
        reporter.settle(batch, Delivery::Retry, at(9, 0));
        // Nothing goes out while waiting, whatever happens meanwhile.
        reporter.signal(Signal::Count(Counter::Ssh), at(9, 0));
        assert!(reporter.take_batch(at(9, 0)).is_none());
        let batch = reporter.take_batch(at(9, 1)).unwrap();
        assert_eq!(names(&Some(batch)), ["app_installed", "app_started"]);
    }

    #[test]
    fn a_development_build_sums_up_by_the_minute_and_retries_soon() {
        let start = at(9, 0);
        let mut reporter = Reporter::new(State::default(), Pace::Debug);
        reporter.signal(Signal::Started(snapshot()), start);
        reporter.signal(Signal::Count(Counter::Ssh), start);
        let batch = reporter.take_batch(start);
        assert_eq!(names(&batch), ["app_installed", "app_started"]);
        reporter.settle(batch.unwrap(), Delivery::Retry, start);
        assert!(reporter.take_batch(start + TimeDelta::seconds(9)).is_none());
        let soon = start + TimeDelta::seconds(10);
        let batch = reporter.take_batch(soon).unwrap();
        reporter.settle(batch, Delivery::Sent, soon);
        assert!(reporter.take_batch(soon).is_none());
        let batch = reporter.take_batch(at(9, 1));
        assert_eq!(names(&batch), ["usage"]);
        assert_eq!(batch.unwrap().events[0].props["ssh"], 1);
        assert_eq!(Pace::Debug.wait_after(8), DEBUG_WAIT);
    }

    #[test]
    fn a_refused_batch_is_dropped() {
        let mut reporter = started_at(at(0, 0));
        reporter.signal(Signal::Count(Counter::Ssh), at(0, 0));
        let batch = reporter.take_batch(at(5, 0)).unwrap();
        reporter.settle(batch, Delivery::Rejected, at(5, 0));
        assert!(reporter.take_batch(at(5, 0)).is_none());
        assert_eq!(reporter.state().unsent, None);
    }

    #[test]
    fn a_batch_holds_at_most_twenty_five_events() {
        let mut reporter = Reporter::new(
            State {
                installed: true,
                ..State::default()
            },
            Pace::Release,
        );
        for minute in 0..40 {
            reporter.signal(Signal::Started(snapshot()), at(9, minute));
        }
        reporter.signal(Signal::Count(Counter::Ssh), at(5, 0));
        let batch = reporter.take_batch(at(9, 40)).unwrap();
        assert_eq!(batch.events.len(), 25);
        assert_eq!(batch.events.last().unwrap().name, "usage");
        reporter.settle(batch, Delivery::Sent, at(9, 40));
        assert_eq!(reporter.take_batch(at(9, 40)).unwrap().events.len(), 16);
    }

    #[test]
    fn events_waiting_too_long_are_dropped() {
        let mut reporter = Reporter::new(
            State {
                installed: true,
                ..State::default()
            },
            Pace::Release,
        );
        for _ in 0..60 {
            reporter.signal(Signal::Started(snapshot()), at(9, 0));
        }
        assert_eq!(reporter.queue.len(), QUEUE_LIMIT);
        let batch = reporter.take_batch(at(9, 0)).unwrap();
        reporter.settle(batch, Delivery::Retry, at(9, 0));
        assert!(reporter.take_batch(days_later(at(9, 0), 1)).is_none());
        assert!(reporter.queue.is_empty());
    }

    #[test]
    fn turning_it_off_forgets_all_but_the_installation() {
        let mut reporter = started_at(at(9, 0));
        reporter.take_changed();
        reporter.signal(Signal::Count(Counter::Ssh), at(9, 5));
        reporter.signal(Signal::Active(snapshot()), days_later(at(9, 0), 1));
        reporter.signal(Signal::Enabled(false), days_later(at(9, 1), 1));
        assert!(reporter.take_changed());
        assert_eq!(
            reporter.state(),
            &State {
                installed: true,
                ..State::default()
            }
        );
        assert!(reporter.take_batch(days_later(at(15, 0), 1)).is_none());
        // Back on, the day is reported again.
        reporter.signal(Signal::Enabled(true), days_later(at(16, 0), 1));
        reporter.signal(Signal::Active(snapshot()), days_later(at(16, 0), 1));
        assert_eq!(
            names(&reporter.take_batch(days_later(at(16, 0), 1))),
            ["app_active"]
        );
    }

    #[test]
    fn a_session_lasts_until_an_hour_without_events() {
        let mut session = Session::default();
        let first = session.at(at(9, 0));
        assert!(first.starts_with(&at(9, 0).timestamp().to_string()));
        assert_eq!(first.len(), 18);
        assert_eq!(session.at(at(9, 50)), first);
        assert_eq!(session.at(at(10, 49)), first);
        let next = session.at(at(11, 50));
        assert_ne!(next, first);
        assert!(next.starts_with(&at(11, 50).timestamp().to_string()));
    }

    #[test]
    fn the_state_is_kept_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("analytics.json");
        assert_eq!(State::load(&path), State::default());
        let mut tally = Tally::new(at(9, 0));
        tally.add(Counter::Upload, at(9, 30));
        let state = State {
            installed: true,
            counting: Some(tally),
            unsent: None,
        };
        state.save(&path).unwrap();
        assert_eq!(State::load(&path), state);
    }

    #[test]
    fn a_state_file_that_cannot_be_read_starts_afresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("analytics.json");
        fs::write(&path, "{not json").unwrap();
        assert_eq!(State::load(&path), State::default());
        // Counters this version does not know are left out.
        fs::write(
            &path,
            r#"{"installed":true,"counting":{"since":1,"last":2,"counts":{"ssh":2,"later":5}},
               "unsent":{"since":1,"last":2,"counts":{"later":1}}}"#,
        )
        .unwrap();
        let state = State::load(&path);
        assert!(state.installed);
        assert_eq!(
            state.counting.unwrap().counts,
            BTreeMap::from([("ssh".to_string(), 2)])
        );
        assert_eq!(state.unsent, None);
    }

    #[test]
    fn a_state_that_cannot_be_written_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("file");
        fs::write(&blocked, "").unwrap();
        assert!(
            State::default()
                .save(&blocked.join("analytics.json"))
                .is_err()
        );
    }
}
