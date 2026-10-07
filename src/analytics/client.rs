//! The analytics thread, the handle the workspace signals it through, and
//! the request to Aptabase.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use chrono::Utc;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, USER_AGENT};

use super::APP_KEY;
use super::model::{
    Counter, Delivery, Pace, Reporter, Signal, Snapshot, State, SystemProps, body, endpoint,
};
use super::system::SystemInfo;
use crate::update::{Channel, build_info};

/// Lets a development build report, to its debug view, to try it out.
const DEBUG_ENV: &str = "SHELLRS_ANALYTICS_DEBUG";

/// Where the events go.
pub trait Transport: Send + Sync {
    /// Post `body`, a JSON array of events, and say how it went. Blocks: it
    /// runs on the analytics thread.
    fn send(&self, body: &[u8], user_agent: &str) -> Delivery;
}

/// Aptabase, over HTTPS.
pub struct HttpTransport {
    endpoint: String,
    app_key: String,
    /// How long connecting may take, and the whole request.
    connect_timeout: Duration,
    timeout: Duration,
    /// Whether to go through the system's proxy, as the update check does.
    /// Only the loopback tests turn it off: a developer's proxy would
    /// capture 127.0.0.1.
    system_proxy: bool,
}

impl HttpTransport {
    pub fn new(endpoint: impl Into<String>, app_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            app_key: app_key.into(),
            connect_timeout: Duration::from_secs(5),
            timeout: Duration::from_secs(15),
            system_proxy: true,
        }
    }
}

impl Transport for HttpTransport {
    fn send(&self, body: &[u8], user_agent: &str) -> Delivery {
        // Built for each request, on the analytics thread: the blocking
        // client runs a runtime of its own, and requests are a few a day.
        let builder = Client::builder()
            .connect_timeout(self.connect_timeout)
            .timeout(self.timeout);
        let builder = if self.system_proxy {
            builder
        } else {
            builder.no_proxy()
        };
        let Ok(client) = builder.build() else {
            return Delivery::Retry;
        };
        let response = client
            .post(&self.endpoint)
            .header("App-Key", &self.app_key)
            .header(CONTENT_TYPE, "application/json")
            .header(USER_AGENT, user_agent)
            .body(body.to_vec())
            .send();
        match response {
            Ok(response) => delivery(response.status().as_u16()),
            Err(_) => Delivery::Retry,
        }
    }
}

/// What an answer means for the events: kept for a later try only when
/// trying again can help.
fn delivery(status: u16) -> Delivery {
    match status {
        200..=299 => Delivery::Sent,
        408 | 429 | 500..=599 => Delivery::Retry,
        _ => Delivery::Rejected,
    }
}

/// What the analytics thread works with: production's from `system`, the
/// tests' with a fake transport.
pub struct AnalyticsServices {
    pub transport: Arc<dyn Transport>,
    /// `analytics.json`.
    pub state_path: PathBuf,
    /// A development build's events go to the dashboard's debug view, and
    /// at its quicker pace.
    pub is_debug: bool,
    /// Reads the system's name and version, on the thread.
    pub detect: fn() -> SystemInfo,
}

impl AnalyticsServices {
    /// Production's, or `None` for a build that does not report: only
    /// release builds do, and a development build with
    /// `SHELLRS_ANALYTICS_DEBUG` set.
    pub fn system() -> Option<Self> {
        let is_debug = Channel::of_this_build().is_none();
        if is_debug && std::env::var_os(DEBUG_ENV).is_none() {
            return None;
        }
        let endpoint = endpoint(APP_KEY)?;
        Some(Self {
            transport: Arc::new(HttpTransport::new(endpoint, APP_KEY)),
            state_path: crate::app::analytics_path(),
            is_debug,
            detect: SystemInfo::detect,
        })
    }
}

/// The workspace's end. Signals go to the analytics thread and nothing
/// comes back, so the window never waits on the network; with 发送匿名使用统计
/// off, none are sent.
pub struct Analytics {
    sender: Sender<Signal>,
    enabled: bool,
}

impl Analytics {
    /// Signals go to `sender`. The tests read them off its receiver.
    pub fn new(sender: Sender<Signal>, enabled: bool) -> Self {
        Self { sender, enabled }
    }

    /// Start the analytics thread. Off, it only forgets what an earlier
    /// run left counted.
    pub fn start(services: AnalyticsServices, enabled: bool) -> Self {
        let (sender, receiver) = mpsc::channel();
        // A thread that cannot start leaves the signals unheard, which is
        // all it costs.
        let _ = thread::Builder::new()
            .name("shellrs-analytics".into())
            .spawn(move || run(services, receiver));
        let analytics = Self::new(sender, enabled);
        if !enabled {
            let _ = analytics.sender.send(Signal::Enabled(false));
        }
        analytics
    }

    pub fn started(&self, snapshot: Snapshot) {
        self.send(Signal::Started(snapshot));
    }

    pub fn window_active(&self, snapshot: Snapshot) {
        self.send(Signal::Active(snapshot));
    }

    pub fn count(&self, counter: Counter) {
        self.send(Signal::Count(counter));
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled != self.enabled {
            self.enabled = enabled;
            let _ = self.sender.send(Signal::Enabled(enabled));
        }
    }

    /// A thread that is gone is not the window's problem.
    fn send(&self, signal: Signal) {
        if self.enabled {
            let _ = self.sender.send(signal);
        }
    }
}

/// The analytics thread: wait for a signal, take it in with whatever else
/// arrived meanwhile, send what is due, keep the state. Nothing happens
/// between signals; the thread ends with the workspace's handle.
fn run(services: AnalyticsServices, signals: Receiver<Signal>) {
    let info = (services.detect)();
    let user_agent = info.user_agent(build_info::VERSION);
    let system = SystemProps {
        is_debug: services.is_debug,
        os_name: info.os_name,
        os_version: info.os_version,
        app_version: build_info::VERSION.to_string(),
    };
    let pace = if services.is_debug {
        Pace::Debug
    } else {
        Pace::Release
    };
    let mut reporter = Reporter::new(State::load(&services.state_path), pace);
    while let Ok(signal) = signals.recv() {
        let now = Utc::now();
        reporter.signal(signal, now);
        for signal in signals.try_iter() {
            reporter.signal(signal, now);
        }
        if let Some(batch) = reporter.take_batch(now) {
            let delivery = services
                .transport
                .send(&body(&batch.events, &system), &user_agent);
            reporter.settle(batch, delivery, Utc::now());
        }
        if reporter.take_changed() {
            // Unwritable, the state lives on in memory until the next try.
            let _ = reporter.state().save(&services.state_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::time::Instant;

    use serde_json::Value;

    use super::*;

    /// A request as the loopback server saw it.
    #[derive(Clone, Debug, Default)]
    struct Seen {
        request_line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Seen {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    /// Answers every request with `status`, or with nothing at all for
    /// `None`, and remembers what it was sent.
    struct Server {
        port: u16,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Server {
        fn start(status: Option<u16>) -> Self {
            let listener = {
                let _forks = crate::testing::no_forks();
                TcpListener::bind("127.0.0.1:0").unwrap()
            };
            let port = listener.local_addr().unwrap().port();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let shared = seen.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let seen = shared.clone();
                    thread::spawn(move || serve(stream, status, &seen));
                }
            });
            Self { port, seen }
        }

        fn transport(&self) -> HttpTransport {
            HttpTransport {
                endpoint: format!("http://127.0.0.1:{}/api/v0/events", self.port),
                app_key: "A-US-1".into(),
                connect_timeout: Duration::from_millis(500),
                timeout: Duration::from_millis(500),
                system_proxy: false,
            }
        }

        fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    fn serve(stream: TcpStream, status: Option<u16>, seen: &Mutex<Vec<Seen>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request = Seen::default();
        if reader.read_line(&mut request.request_line).is_err() {
            return;
        }
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                request
                    .headers
                    .push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        let length = request
            .header("content-length")
            .and_then(|length| length.parse().ok())
            .unwrap_or(0);
        request.body = vec![0; length];
        let _ = reader.read_exact(&mut request.body);
        seen.lock().unwrap().push(request);
        let Some(status) = status else {
            // Keep the connection open and say nothing.
            thread::sleep(Duration::from_secs(2));
            return;
        };
        let mut stream = stream;
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 {status} Whatever\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            )
            .as_bytes(),
        );
    }

    #[test]
    fn the_request_is_what_aptabase_reads() {
        let server = Server::start(Some(200));
        let delivery = server
            .transport()
            .send(b"[]", "ShellRS/1 (macOS 15; aarch64)");
        assert_eq!(delivery, Delivery::Sent);
        let seen = server.seen();
        let request = &seen[0];
        assert!(request.request_line.starts_with("POST /api/v0/events "));
        assert_eq!(request.header("app-key"), Some("A-US-1"));
        assert_eq!(request.header("content-type"), Some("application/json"));
        assert_eq!(
            request.header("user-agent"),
            Some("ShellRS/1 (macOS 15; aarch64)")
        );
        assert_eq!(request.body, b"[]");
    }

    #[test]
    fn only_what_may_pass_is_tried_again() {
        for (status, expected) in [
            (202, Delivery::Sent),
            (500, Delivery::Retry),
            (503, Delivery::Retry),
            (429, Delivery::Retry),
            (400, Delivery::Rejected),
            (404, Delivery::Rejected),
        ] {
            let server = Server::start(Some(status));
            assert_eq!(server.transport().send(b"[]", "ua"), expected, "{status}");
        }
    }

    #[test]
    fn no_network_is_tried_again_later() {
        // A port nothing listens on.
        let port = {
            let _forks = crate::testing::no_forks();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let mut transport = Server::start(Some(200)).transport();
        transport.endpoint = format!("http://127.0.0.1:{port}/api/v0/events");
        assert_eq!(transport.send(b"[]", "ua"), Delivery::Retry);
    }

    #[test]
    fn a_server_that_never_answers_is_given_up_on() {
        let server = Server::start(None);
        let started = Instant::now();
        assert_eq!(server.transport().send(b"[]", "ua"), Delivery::Retry);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Hands each body to the test.
    struct FakeTransport(Mutex<Sender<Vec<u8>>>);

    impl Transport for FakeTransport {
        fn send(&self, body: &[u8], _: &str) -> Delivery {
            let _ = self.0.lock().unwrap().send(body.to_vec());
            Delivery::Sent
        }
    }

    fn a_mac() -> SystemInfo {
        SystemInfo {
            os_name: "macOS".into(),
            os_version: "15.1.0".into(),
        }
    }

    #[test]
    fn the_thread_reports_the_start_and_keeps_the_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("analytics.json");
        let (bodies, received) = mpsc::channel();
        let services = AnalyticsServices {
            transport: Arc::new(FakeTransport(Mutex::new(bodies))),
            state_path: path.clone(),
            is_debug: true,
            detect: a_mac,
        };
        let analytics = Analytics::start(services, true);
        analytics.started(Snapshot::default());
        let body = received.recv_timeout(Duration::from_secs(5)).unwrap();
        let events: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(events[0]["eventName"], "app_installed");
        assert_eq!(events[1]["eventName"], "app_started");
        assert_eq!(events[0]["systemProps"]["osName"], "macOS");
        assert_eq!(events[0]["systemProps"]["isDebug"], true);
        analytics.count(Counter::Ssh);
        // The count is on disk once the thread has taken it in.
        let deadline = Instant::now() + Duration::from_secs(5);
        let state = loop {
            let state = State::load(&path);
            if state.counting.is_some() || Instant::now() > deadline {
                break state;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert!(state.installed);
        assert_eq!(state.counting.unwrap().counts["ssh"], 1);
    }

    #[test]
    fn switched_off_nothing_is_signalled() {
        let (sender, receiver) = mpsc::channel();
        let mut analytics = Analytics::new(sender, true);
        analytics.count(Counter::Find);
        analytics.set_enabled(false);
        analytics.count(Counter::Find);
        analytics.window_active(Snapshot::default());
        analytics.set_enabled(true);
        let signals: Vec<Signal> = receiver.try_iter().collect();
        assert_eq!(
            signals,
            [
                Signal::Count(Counter::Find),
                Signal::Enabled(false),
                Signal::Enabled(true)
            ]
        );
    }
}
