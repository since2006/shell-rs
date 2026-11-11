//! A fake 测试连接 that records each login it is given.

use super::*;

/// Same, with the host dialog's connection test answered by `tester`.
///
/// Motion is reduced, as in the other fixtures: dialogs would otherwise
/// slide in over real time, and under a loaded test run a field or button
/// can move between being found and being clicked.
pub fn open_workspace_with_tester(
    cx: &mut TestAppContext,
    store: HostStore,
    tester: Arc<FakeConnectionTester>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_settings_and_tester(cx, store, settings_store(), tester)
}

pub fn open_workspace_with_settings_and_tester(
    cx: &mut TestAppContext,
    store: HostStore,
    settings: SettingsStore,
    tester: Arc<FakeConnectionTester>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    init_app(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| store);
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| settings),
                remote,
                fake_telnet_terminals(),
                fake_serial_terminals(),
                fake_local_terminals(),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                tester.clone(),
                Arc::new(FakeForwardProvider::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.expect("workspace created"))
}

/// `(host, port, user, password)` of one connection test.
pub type TestedLogin = (String, u16, String, Option<String>);

/// A Telnet connection test: address, port, route, and the proxy password
/// typed.
pub type TestedTelnet = (String, u16, LoginRoute, Option<String>);

/// Stands in for the SSH login behind 「测试连接」: records what the form sent,
/// optionally asks to trust a made-up host key, and answers with `result`.
pub struct FakeConnectionTester {
    pub asks_trust: bool,
    pub result: Result<(), String>,
    pub requests: Mutex<Vec<TestedLogin>>,
    /// How each test was to reach the host, with the proxy password the
    /// form gave.
    pub routes: Mutex<Vec<(LoginRoute, Option<String>)>>,
    pub trust_answers: Mutex<Vec<bool>>,
    /// The serial lines opened, answered with `result` too.
    pub serial_lines: Mutex<Vec<SerialLine>>,
    /// Each Telnet test: address, port, route, and the proxy password typed.
    pub telnet_tests: Mutex<Vec<TestedTelnet>>,
    /// The one-time codes each test was to fill in, with the secret typed.
    pub totps: Mutex<Vec<(Option<TotpLogin>, Option<String>)>>,
}

impl Default for FakeConnectionTester {
    fn default() -> Self {
        Self {
            asks_trust: false,
            result: Ok(()),
            requests: Mutex::default(),
            routes: Mutex::default(),
            trust_answers: Mutex::default(),
            serial_lines: Mutex::default(),
            telnet_tests: Mutex::default(),
            totps: Mutex::default(),
        }
    }
}

impl FakeConnectionTester {
    pub fn failing(reason: &str) -> Self {
        Self {
            result: Err(reason.to_string()),
            ..Self::default()
        }
    }

    pub fn asking_trust() -> Self {
        Self {
            asks_trust: true,
            ..Self::default()
        }
    }

    pub fn requests(&self) -> Vec<TestedLogin> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn routes(&self) -> Vec<(LoginRoute, Option<String>)> {
        self.routes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn trust_answers(&self) -> Vec<bool> {
        self.trust_answers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn serial_lines(&self) -> Vec<SerialLine> {
        self.serial_lines
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn totps(&self) -> Vec<(Option<TotpLogin>, Option<String>)> {
        self.totps
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn telnet_tests(&self) -> Vec<TestedTelnet> {
        self.telnet_tests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl ConnectionTester for FakeConnectionTester {
    fn test(&self, request: LoginTest, trust: TrustCallback) -> Result<(), String> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((
                request.host().to_string(),
                request.port(),
                request.user().to_string(),
                request.password().map(str::to_string),
            ));
        self.routes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((
                request.login().route.clone(),
                request.proxy_password().map(str::to_string),
            ));
        self.totps
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((
                request.login().totp.as_deref().cloned(),
                request.totp_secret().map(str::to_string),
            ));
        if self.asks_trust {
            let ConnectionPromptKind::UnknownHost(prompt) = ConnectionPromptKind::unknown_host(
                request.host(),
                request.port(),
                "ssh-ed25519",
                "SHA256:test-fingerprint",
            ) else {
                unreachable!("unknown_host builds an UnknownHost prompt");
            };
            let trusted = trust(prompt);
            self.trust_answers
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(trusted);
            if !trusted {
                return Err("未信任该主机的密钥".to_string());
            }
        }
        self.result.clone()
    }

    fn test_serial(&self, line: &SerialLine) -> Result<(), String> {
        self.serial_lines
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(line.clone());
        self.result.clone()
    }

    fn test_telnet(&self, request: TelnetTest, _: TrustCallback) -> Result<(), String> {
        self.telnet_tests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((
                request.host().to_string(),
                request.port(),
                request.route().clone(),
                request.proxy_password().map(str::to_string),
            ));
        self.result.clone()
    }
}
