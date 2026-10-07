//! Fake port forwards, and the workspace that runs them.

use super::*;

/// What a fake forward does once it is started.
#[derive(Clone, Default)]
pub enum ForwardScript {
    /// Logs in and listens.
    #[default]
    Listens,
    /// Asks whether to trust the host before it listens.
    AsksTrust,
    /// Cannot be brought up.
    Fails(&'static str),
}

/// One run of a fake forward, as the tests see it.
#[derive(Clone)]
pub struct FakeForwardRun {
    pub rule: ForwardRule,
    pub host: HostId,
    /// The worker's end of the event channel: a test sends what a real
    /// forward would report later (a dropped connection, a failure).
    pub events: async_channel::Sender<ForwardEvent>,
    pub stopped: Arc<AtomicUsize>,
}

/// Stands in for the SSH forward worker: opens no socket, records each run
/// and answers `Stop` with `Stopped` like the real one.
#[derive(Default)]
pub struct FakeForwardProvider {
    pub script: ForwardScript,
    pub runs: Arc<Mutex<Vec<FakeForwardRun>>>,
}

impl FakeForwardProvider {
    pub fn with_script(script: ForwardScript) -> Self {
        Self {
            script,
            ..Self::default()
        }
    }

    pub fn runs(&self) -> Vec<FakeForwardRun> {
        self.runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// How many of the runs have been told to stop and did.
    pub fn stopped(&self) -> usize {
        self.runs()
            .iter()
            .map(|run| run.stopped.load(Ordering::SeqCst))
            .sum()
    }
}

impl ForwardTransportProvider for FakeForwardProvider {
    fn create(&self, rule: &ForwardRule, _: &HostLogin) -> Box<dyn ForwardTransport> {
        Box::new(FakeForwardTransport {
            script: self.script.clone(),
            rule: rule.clone(),
            host: rule.host,
            runs: self.runs.clone(),
        })
    }
}

pub struct FakeForwardTransport {
    pub script: ForwardScript,
    pub rule: ForwardRule,
    pub host: HostId,
    pub runs: Arc<Mutex<Vec<FakeForwardRun>>>,
}

impl ForwardTransport for FakeForwardTransport {
    fn run(
        self: Box<Self>,
        commands: async_channel::Receiver<ForwardCommand>,
        events: async_channel::Sender<ForwardEvent>,
    ) {
        let stopped = Arc::new(AtomicUsize::new(0));
        self.runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(FakeForwardRun {
                rule: self.rule.clone(),
                host: self.host,
                events: events.clone(),
                stopped: stopped.clone(),
            });
        let _ = events.send_blocking(ForwardEvent::Connecting);
        match self.script {
            ForwardScript::Listens => {
                let _ = events.send_blocking(ForwardEvent::Listening);
            }
            ForwardScript::AsksTrust => {
                let _ = events.send_blocking(ForwardEvent::Prompt(ConnectionPrompt::new(
                    7,
                    ConnectionPromptKind::unknown_host(
                        "10.0.2.5",
                        22,
                        "ssh-ed25519",
                        "SHA256:forward-fingerprint",
                    ),
                )));
            }
            ForwardScript::Fails(reason) => {
                let _ = events.send_blocking(ForwardEvent::Failed(reason.to_string()));
                return;
            }
        }
        while let Ok(command) = commands.recv_blocking() {
            match command {
                ForwardCommand::PromptReply { reply, .. } => {
                    if matches!(reply, ConnectionPromptReply::TrustAndSave) {
                        let _ = events.send_blocking(ForwardEvent::Listening);
                    }
                }
                ForwardCommand::Stop => break,
            }
        }
        stopped.fetch_add(1, Ordering::SeqCst);
        let _ = events.send_blocking(ForwardEvent::Stopped);
    }
}

/// The workspace with a fake forward provider the test keeps hold of.
pub fn open_workspace_with_forwards(
    cx: &mut TestAppContext,
    store: HostStore,
    provider: Arc<FakeForwardProvider>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_sized_workspace_with_forwards(cx, store, provider, size(px(1280.), px(800.)))
}

pub fn open_sized_workspace_with_forwards(
    cx: &mut TestAppContext,
    store: HostStore,
    provider: Arc<FakeForwardProvider>,
    window_size: gpui_kit::Size<gpui_kit::Pixels>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    init_app(cx);
    // The forward dialog's diagram plays a flow over real time, and dialogs
    // slide in: both would move targets between locating and clicking them.
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(window_size, |window, cx| {
        let store = cx.new(|_| store);
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| settings_store()),
                remote,
                Arc::new(FakeTerminalFactory::default()),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                Arc::new(FakeConnectionTester::default()),
                provider,
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.expect("workspace created"))
}

/// Show the forward list and wait for it to be up.
pub async fn show_forwards(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| window.click("show-forwards", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("forward-search").is_some()
    })
    .await;
}
