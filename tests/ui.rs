//! UI integration tests: the production `Workspace` rendered in a headless
//! window, driven through real pointer and keyboard events.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use gpui_kit::component::{ActiveTheme as _, Root, WindowExt as _};
use gpui_kit::test::{TestAppContextExt, TestWindowExt};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, ElementId, Entity, InputEvent as _, MouseButton,
    MouseMoveEvent, TestAppContext, WindowHandle, point, px, size,
};

use shellrs::app::{
    CenterTab, ClearTerminal, CloseScope, CloseTabs, CollapseAllGroups, ConnectGroup,
    ConnectSession, CopySessionHost, CopySessionId, DeleteGroup, DeleteSession, DisconnectTerminal,
    EditSession, ExpandAllGroups, FindInTerminal, FindNextInTerminal, FindPreviousInTerminal,
    InstallCliCommand, NewLocalTerminal, NewSessionInGroup, OpenExplorer, ReconnectTerminal,
    RemoveAgentSkill, RenameGroup, RenameTerminal,
};
use shellrs::cli::{AgentKind, IntegrationPaths};
use shellrs::connection::{ConnectionPromptKind, ConnectionTester, LoginTest, TrustCallback};
use shellrs::explorer::ExplorerId;
use shellrs::secrets::{InMemorySecretStore, SecretRef, SecretStore as _};
use shellrs::session::{
    AuthKind, ConnectionState, GroupDraft, GroupId, HostOs, SessionDatabase, SessionDraft,
    SessionId, SessionStore,
};
use shellrs::settings::{Appearance, InterfaceLanguage, SettingsStore};
use shellrs::sftp::{
    DirectoryEntry, DirectoryListing, EntryKind, FileMetadata, LocalDirectoryProvider, RemotePath,
    SftpCommand, SftpEvent, SftpTransport, SftpTransportProvider, UploadRequest,
};
use shellrs::terminal::{
    FixedRemoteTerminalTransportProvider, Latency, LocalTerminalId, RemoteTerminalId, TerminalFont,
    TerminalLifecycle, TerminalPrompt, TerminalPromptField, TerminalPromptKind,
    TerminalPromptReply, TerminalSize, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory,
};
use shellrs::workspace::Workspace;

/// Seeded session ids, in insertion order (see `SessionStore::seed`).
const WEB_01: u64 = 1;
const WEB_02: u64 = 2;
const DB_01: u64 = 3;
/// The first SFTP tab a test opens; tab ids count up from 1.
const SFTP_TAB: u64 = 1;
const STAGING_API: u64 = 4;
const DEV_BOX: u64 = 6;
const INITIAL_WEB_TERMINAL: u64 = 1;
const INITIAL_STAGING_TERMINAL: u64 = 2;
const FIRST_NEW_TERMINAL: u64 = 3;
/// Seeded group ids, in insertion order: 生产, 测试, 开发.
const PRODUCTION: u64 = 1;
const DEVELOPMENT: u64 = 3;

fn open_workspace(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_store(cx, SessionStore::seed())
}

/// Production loads the store from the database; the tests hand one in
/// directly so they get the fixed shape `SessionStore::seed` describes.
fn open_workspace_with_store(
    cx: &mut TestAppContext,
    store: SessionStore,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_tester(cx, store, Arc::new(FakeConnectionTester::default()))
}

/// Same, with the session dialog's connection test answered by `tester`.
fn open_workspace_with_tester(
    cx: &mut TestAppContext,
    store: SessionStore,
    tester: Arc<FakeConnectionTester>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellrs::init);
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| store);
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| SettingsStore::in_memory()),
                remote,
                Arc::new(FakeTerminalFactory::default()),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                tester.clone(),
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
type TestedLogin = (String, u16, String, Option<String>);

/// Stands in for the SSH login behind 「测试连接」: records what the form sent,
/// optionally asks to trust a made-up host key, and answers with `result`.
struct FakeConnectionTester {
    asks_trust: bool,
    result: Result<(), String>,
    requests: Mutex<Vec<TestedLogin>>,
    trust_answers: Mutex<Vec<bool>>,
}

impl Default for FakeConnectionTester {
    fn default() -> Self {
        Self {
            asks_trust: false,
            result: Ok(()),
            requests: Mutex::default(),
            trust_answers: Mutex::default(),
        }
    }
}

impl FakeConnectionTester {
    fn failing(reason: &str) -> Self {
        Self {
            result: Err(reason.to_string()),
            ..Self::default()
        }
    }

    fn asking_trust() -> Self {
        Self {
            asks_trust: true,
            ..Self::default()
        }
    }

    fn requests(&self) -> Vec<TestedLogin> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn trust_answers(&self) -> Vec<bool> {
        self.trust_answers
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
}

#[derive(Clone, Copy, Default)]
enum FakeBehavior {
    #[default]
    Running,
    ExitFirst,
    FailFirst,
    ReportsOs(HostOs),
    ReportsLatency(Latency),
}

#[derive(Default)]
struct FakeTerminalFactory {
    starts: AtomicUsize,
    behavior: FakeBehavior,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    resizes: Arc<Mutex<Vec<TerminalSize>>>,
}

impl FakeTerminalFactory {
    fn exit_first() -> Self {
        Self {
            behavior: FakeBehavior::ExitFirst,
            ..Self::default()
        }
    }

    fn fail_first() -> Self {
        Self {
            behavior: FakeBehavior::FailFirst,
            ..Self::default()
        }
    }

    fn reports_os(os: HostOs) -> Self {
        Self {
            behavior: FakeBehavior::ReportsOs(os),
            ..Self::default()
        }
    }

    fn reports_latency(latency: Latency) -> Self {
        Self {
            behavior: FakeBehavior::ReportsLatency(latency),
            ..Self::default()
        }
    }

    fn written_text(&self) -> String {
        let bytes: Vec<_> = self
            .writes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .flatten()
            .copied()
            .collect();
        String::from_utf8(bytes).expect("fake transport receives UTF-8 test input")
    }

    fn starts(&self) -> usize {
        self.starts.load(Ordering::SeqCst)
    }
}

impl TerminalTransportFactory for FakeTerminalFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        let run = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        Box::new(FakeTerminalTransport {
            run,
            behavior: self.behavior,
            writes: self.writes.clone(),
            resizes: self.resizes.clone(),
        })
    }
}

struct FakeTerminalTransport {
    run: usize,
    behavior: FakeBehavior,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    resizes: Arc<Mutex<Vec<TerminalSize>>>,
}

impl TerminalTransport for FakeTerminalTransport {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: async_channel::Sender<TerminalTransportEvent>,
    ) -> anyhow::Result<()> {
        if matches!(self.behavior, FakeBehavior::FailFirst) && self.run == 1 {
            anyhow::bail!("测试启动失败");
        }
        events.send_blocking(TerminalTransportEvent::Started)?;
        if let FakeBehavior::ReportsOs(os) = self.behavior {
            events.send_blocking(TerminalTransportEvent::HostOsDetected(os))?;
        }
        if let FakeBehavior::ReportsLatency(latency) = self.behavior {
            events.send_blocking(TerminalTransportEvent::Latency(latency))?;
        }
        events.send_blocking(TerminalTransportEvent::Output(
            format!(
                "\x1b]0;测试终端 {}\x07run:{}$ alpha.txt 会议纪要.md\r\n",
                self.run, self.run
            )
            .into_bytes(),
        ))?;
        if matches!(self.behavior, FakeBehavior::ExitFirst) && self.run == 1 {
            events.send_blocking(TerminalTransportEvent::Exited {
                code: 9,
                signal: None,
            })?;
            return Ok(());
        }
        while let Ok(command) = commands.recv() {
            match command {
                TerminalTransportCommand::Write(bytes) => {
                    self.writes
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(bytes.clone());
                    events.send_blocking(TerminalTransportEvent::Output(bytes))?;
                }
                TerminalTransportCommand::Resize(size) => {
                    self.resizes
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .push(size);
                }
                TerminalTransportCommand::PromptReply { .. } => {}
                TerminalTransportCommand::Shutdown => break,
            }
        }
        Ok(())
    }
}

fn open_workspace_with_factory(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellrs::init);
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| SessionStore::seed());
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| SettingsStore::in_memory()),
                remote,
                factory.clone(),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                Arc::new(FakeConnectionTester::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.expect("workspace created"))
}

fn open_workspace_with_remote_factory(
    cx: &mut TestAppContext,
    store: SessionStore,
    factory: Arc<dyn TerminalTransportFactory>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellrs::init);
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| store);
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(factory.clone()));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| SettingsStore::in_memory()),
                remote,
                Arc::new(FakeTerminalFactory::default()),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                Arc::new(FakeConnectionTester::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.expect("workspace created"))
}

#[derive(Clone, Copy)]
enum PromptBehavior {
    Authentication,
    UnknownHost,
}

struct PromptTerminalFactory {
    behavior: PromptBehavior,
    accepted: Arc<AtomicUsize>,
    canceled: Arc<AtomicUsize>,
}

impl PromptTerminalFactory {
    fn new(behavior: PromptBehavior) -> Self {
        Self {
            behavior,
            accepted: Arc::new(AtomicUsize::new(0)),
            canceled: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl TerminalTransportFactory for PromptTerminalFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(PromptTerminalTransport {
            behavior: self.behavior,
            accepted: self.accepted.clone(),
            canceled: self.canceled.clone(),
        })
    }
}

struct PromptTerminalTransport {
    behavior: PromptBehavior,
    accepted: Arc<AtomicUsize>,
    canceled: Arc<AtomicUsize>,
}

impl TerminalTransport for PromptTerminalTransport {
    fn run(
        self: Box<Self>,
        _: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: async_channel::Sender<TerminalTransportEvent>,
    ) -> anyhow::Result<()> {
        let request_id = 9001;
        let kind = match self.behavior {
            PromptBehavior::Authentication => TerminalPromptKind::authentication(
                "SSH 登录",
                "请输入密码",
                vec![TerminalPromptField::new("密码", false)],
            ),
            PromptBehavior::UnknownHost => TerminalPromptKind::unknown_host(
                "example.test",
                22,
                "ssh-ed25519",
                "SHA256:test-fingerprint",
            ),
        };
        events.send_blocking(TerminalTransportEvent::Prompt(TerminalPrompt::new(
            request_id, kind,
        )))?;
        loop {
            match commands.recv()? {
                TerminalTransportCommand::PromptReply {
                    request_id: reply_id,
                    reply,
                } if reply_id == request_id => match reply {
                    TerminalPromptReply::TrustAndSave
                        if matches!(self.behavior, PromptBehavior::UnknownHost) =>
                    {
                        self.accepted.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    TerminalPromptReply::Answers(answers)
                        if matches!(self.behavior, PromptBehavior::Authentication)
                            && answers.len() == 1 =>
                    {
                        self.accepted.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    TerminalPromptReply::Cancel => {
                        self.canceled.fetch_add(1, Ordering::SeqCst);
                        events
                            .send_blocking(TerminalTransportEvent::Failed("认证已取消".into()))?;
                        return Ok(());
                    }
                    _ => {}
                },
                TerminalTransportCommand::Shutdown => return Ok(()),
                _ => {}
            }
        }
        events.send_blocking(TerminalTransportEvent::Started)?;
        while let Ok(command) = commands.recv() {
            if matches!(command, TerminalTransportCommand::Shutdown) {
                break;
            }
        }
        Ok(())
    }
}

fn one_session_store(auth: AuthKind) -> (SessionStore, SessionId) {
    let mut store = SessionStore::empty();
    let id = store.insert_unnotified(SessionDraft::new(
        "prompt-host",
        "example.test",
        22,
        "tester",
        auth,
        None,
    ));
    (store, id)
}

#[gpui_kit::test]
async fn session_form_switches_to_key_and_uses_native_path_picker(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_store(cx, SessionStore::empty());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-session", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("session-key-path").is_none());
        window.within("session-auth").click("input", cx);
        window.within("session-auth").press("down", cx);
        window.within("session-auth").press("down", cx);
        window.within("session-auth").press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("session-key-path").visible());
        window.click("choose-key", cx);
    })
    .unwrap();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files);
        assert!(!options.directories);
        assert!(!options.multiple);
        Some(vec!["/tmp/id_ed25519".into()])
    });
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("session-key-path").value(),
            Some("/tmp/id_ed25519")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn authentication_prompt_is_masked_and_drives_connected_state(cx: &mut TestAppContext) {
    let (store, id) = one_session_store(AuthKind::Password);
    let factory = Arc::new(PromptTerminalFactory::new(PromptBehavior::Authentication));
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find("ssh-auth-submit").is_some()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("status-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("ssh-auth-submit").is_some(),
            "点击对话框外部不应取消认证"
        );
    })
    .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let password = window.find(("ssh-auth-answer", 0usize));
        assert_eq!(
            password.value(),
            None,
            "masked input must not expose its value"
        );
        window.click(("ssh-auth-answer", 0usize), cx);
        window.input("test-password", cx);
        window.click("ssh-auth-submit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 prompt-host")
    })
    .await;

    assert_eq!(factory.accepted.load(Ordering::SeqCst), 1);
    assert!(workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .session(id)
            .unwrap()
            .state
            .is_connected()
    }));
}

#[gpui_kit::test]
async fn canceling_unknown_host_prompt_keeps_session_disconnected(cx: &mut TestAppContext) {
    let (store, id) = one_session_store(AuthKind::Auto);
    let factory = Arc::new(PromptTerminalFactory::new(PromptBehavior::UnknownHost));
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find("cancel").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("status-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("cancel").is_some(),
            "点击对话框外部不应取消主机指纹确认"
        );
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("cancel", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("未连接 prompt-host")
    })
    .await;

    assert_eq!(factory.accepted.load(Ordering::SeqCst), 0);
    assert_eq!(factory.canceled.load(Ordering::SeqCst), 1);
    assert!(!workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .session(id)
            .unwrap()
            .state
            .is_connected()
    }));

    // The failed terminal tab remains visible, but connecting the session
    // again must start a fresh transport instead of only activating that tab.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find("cancel").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("cancel", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.canceled.load(Ordering::SeqCst) == 2
    })
    .await;

    // The context-menu item dispatches this same command, so it must also
    // restart the exited transport instead of merely activating the tab.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectSession(id)), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find("cancel").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("cancel", cx);
    })
    .unwrap();
}

#[gpui_kit::test]
async fn connection_edits_reconnect_once_but_display_edits_do_not(cx: &mut TestAppContext) {
    let (store, id) = one_session_store(AuthKind::Auto);
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 1
            && window.find("status-connection").label() == Some("已连接 prompt-host")
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        store.update(cx, |store, cx| {
            let mut draft = store.session(id).unwrap().draft();
            draft.host = "new.example.test".into();
            assert!(store.update(id, draft, cx));
        });
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 2
            && window.find("status-connection").label() == Some("已连接 prompt-host")
    })
    .await;
    assert_eq!(factory.starts(), 2);

    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        store.update(cx, |store, cx| {
            let mut draft = store.session(id).unwrap().draft();
            draft.name = "renamed".into();
            assert!(store.update(id, draft, cx));
        });
    });
    cx.run_until_parked();
    assert_eq!(factory.starts(), 2);
}

#[gpui_kit::test]
async fn opening_sftp_updates_connection_state_without_terminal(cx: &mut TestAppContext) {
    let (store, id) = one_session_store(AuthKind::Auto);
    let (handle, workspace) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(OpenExplorer(id)), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .session(id)
            .unwrap()
            .state
            == ConnectionState::Connected
    })
    .await;

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.explorers_of(id, cx).len(), 1);
        assert_eq!(
            workspace.store().read(cx).session(id).unwrap().state,
            shellrs::session::ConnectionState::Connected
        );
    });
}

#[gpui_kit::test]
async fn double_click_on_session_opens_terminal_and_updates_status(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Sessions seeded as connected already have terminal tabs.
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());

        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 db-01")
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        assert_eq!(
            window.find("status-connection").label(),
            Some("已连接 db-01")
        );
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(DB_01), cx).is_some());
        let store = workspace.store().read(cx);
        assert!(
            store
                .session(SessionId(DB_01))
                .unwrap()
                .state
                .is_connected()
        );
    });
}

#[gpui_kit::test]
async fn connected_session_opens_an_independent_terminal_each_time(cx: &mut TestAppContext) {
    let (store, id) = one_session_store(AuthKind::Auto);
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 1
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 2
    })
    .await;

    // The right-click menu's “连接” item dispatches this same action.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectSession(id)), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 3
            && workspace
                .read(cx)
                .store()
                .read(cx)
                .session(id)
                .unwrap()
                .state
                .is_connected()
    })
    .await;

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.terminal_count(id, cx), 3);
        assert!(workspace.remote_terminal(RemoteTerminalId(1)).is_some());
        assert!(workspace.remote_terminal(RemoteTerminalId(2)).is_some());
        assert!(workspace.remote_terminal(RemoteTerminalId(3)).is_some());
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", 3_u64), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        let workspace = workspace.read(cx);
        workspace.terminal_count(id, cx) == 2
            && workspace
                .store()
                .read(cx)
                .session(id)
                .unwrap()
                .state
                .is_connected()
    })
    .await;
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.terminal_count(id, cx), 2);
        assert!(
            workspace
                .store()
                .read(cx)
                .session(id)
                .unwrap()
                .state
                .is_connected()
        );
    });
}

#[gpui_kit::test]
async fn new_session_dialog_validates_then_inserts(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-session", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        assert!(window.try_find("form-error").is_none());

        // An empty form is rejected and the dialog stays open. The commit
        // action is dispatched deferred, so the error shows after effects run.
        // The dialog's focus trap owns focus until a field is clicked.
        window.click("session-name", cx);
        assert_eq!(window.find("session-name").focused(), Some(true));
        window.click("commit", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("form-error").visible());
        assert!(window.find("commit").visible());

        window.click("session-name", cx);
        window.input("db-02", cx);
        window.click("session-host", cx);
        window.input("10.0.3.7", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .sessions()
            .iter()
            .find(|session| session.name == "db-02")
            .expect("db-02 inserted");
        assert_eq!(created.host.as_ref(), "10.0.3.7");
        assert_eq!(created.port, 22);
    });
}

/// What the user reported: the keychain has the working password, the edit
/// dialog's field has been changed to a wrong one, and 「测试连接」 must try the
/// field — without saving anything.
#[gpui_kit::test]
async fn testing_a_connection_logs_in_with_what_the_form_shows(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let tester = Arc::new(FakeConnectionTester::failing("用户名或密码错误"));
    let (handle, workspace) = open_workspace_with_tester(cx, store, tester.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditSession(id)), cx);
    })
    .unwrap();
    // The saved password is read on a background thread, then fills the field.
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("session-password", cx);
        window.press("cmd-a", cx);
        window.input("wrong-password", cx);
        window.click("session-port", cx);
        window.press("cmd-a", cx);
        window.input("2222", cx);
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;

    assert_eq!(
        tester.requests(),
        [(
            "10.0.2.5".to_string(),
            2222,
            "postgres".to_string(),
            Some("wrong-password".to_string())
        )]
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Nothing was saved, and the dialog stays open to fix the field.
        assert!(window.find("commit").visible());
    })
    .unwrap();
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.session(id).unwrap().port, 22);
    });
    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2"),
        "测试连接不写钥匙串"
    );
}

#[gpui_kit::test]
async fn a_connection_test_needs_a_host_and_a_user_first(cx: &mut TestAppContext) {
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, _) = open_workspace_with_tester(cx, SessionStore::empty(), tester.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-session", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let test = window.find("test-connection").bounds();
        let cancel = window.find("cancel").bounds();
        let commit = window.find("commit").bounds();
        assert!(test.right() < cancel.left());
        assert!(cancel.right() < commit.left());
        assert!(window.notifications(cx).is_empty());
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Reported at once as a failed test; the form shows no result of its own.
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(window.try_find("form-error").is_none());
        window.click("session-host", cx);
        window.input("10.0.3.7", cx);
        window.click("session-user", cx);
        #[cfg(target_os = "macos")]
        window.press("cmd-a", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-a", cx);
        window.press("backspace", cx);
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.notifications(cx).len(), 2, "没有用户名也不该去连");
    })
    .unwrap();
    assert!(tester.requests().is_empty());
}

#[gpui_kit::test]
async fn a_first_seen_host_key_is_put_to_the_user_above_the_session_dialog(
    cx: &mut TestAppContext,
) {
    let tester = Arc::new(FakeConnectionTester::asking_trust());
    let (store, id) = one_session_store(AuthKind::Password);
    let (handle, _) = open_workspace_with_tester(cx, store, tester.clone());

    // Trust: the question reaches the tester as a yes.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditSession(id)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.try_find("ok").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;
    assert_eq!(tester.trust_answers(), [true]);

    // Escape dismisses the question, which declines it; the session dialog
    // underneath stays open.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("ok").is_none());
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.try_find("ok").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 2
    })
    .await;
    assert_eq!(tester.trust_answers(), [true, false]);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn search_filters_the_tree(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .within("session-tree")
                .try_find(("session-row", WEB_01))
                .is_some()
        );
        window.click("session-search", cx);
        window.input("staging", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tree = window.within("session-tree");
        assert!(tree.try_find(("session-row", STAGING_API)).is_some());
        assert!(tree.try_find(("session-row", WEB_01)).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn group_expansion_survives_reopening_the_database(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = SessionDatabase::open(&path).unwrap();
    let seed = SessionStore::seed();
    for group in seed.groups() {
        database.insert_group(group).unwrap();
    }
    for session in seed.sessions() {
        database.insert_session(session).unwrap();
    }
    let (handle, _) = open_workspace_with_store(cx, SessionStore::load(database).unwrap());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("session-row", WEB_01)).visible());
        window.click(("group-row", PRODUCTION), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", PRODUCTION)).visible());
        assert!(window.try_find(("session-row", WEB_01)).is_none());
    })
    .unwrap();
    let database = SessionDatabase::open(&path).unwrap();
    assert!(
        !database
            .load()
            .unwrap()
            .groups
            .iter()
            .find(|group| group.id == GroupId(PRODUCTION))
            .unwrap()
            .expanded
    );

    let (reopened, _) = open_workspace_with_store(cx, SessionStore::load(database).unwrap());
    cx.update_window(reopened.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", PRODUCTION)).visible());
        assert!(window.try_find(("session-row", WEB_01)).is_none());
        assert!(window.find(("session-row", STAGING_API)).visible());
        window.click(("group-row", PRODUCTION), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(reopened.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("session-row", WEB_01)).visible());
    })
    .unwrap();
    let saved = SessionDatabase::open(&path).unwrap().load().unwrap();
    assert!(
        saved
            .groups
            .iter()
            .find(|group| group.id == GroupId(PRODUCTION))
            .unwrap()
            .expanded
    );
}

#[gpui_kit::test]
fn expand_and_collapse_all_groups_include_nested_groups_and_persist(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = SessionDatabase::open(&path).unwrap();
    let mut seed = SessionStore::seed();
    let nested =
        seed.insert_group_unnotified(GroupDraft::new("内部服务", Some(GroupId(PRODUCTION))));
    for group in seed.groups() {
        database.insert_group(group).unwrap();
    }
    for session in seed.sessions() {
        database.insert_session(session).unwrap();
    }
    let (handle, _) = open_workspace_with_store(cx, SessionStore::load(database).unwrap());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", nested.0)).visible());
        assert!(window.find(("session-row", WEB_01)).visible());
        window.dispatch_action(Box::new(CollapseAllGroups), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", PRODUCTION)).visible());
        assert!(window.try_find(("group-row", nested.0)).is_none());
        assert!(window.try_find(("session-row", WEB_01)).is_none());
    })
    .unwrap();
    assert!(
        SessionDatabase::open(&path)
            .unwrap()
            .load()
            .unwrap()
            .groups
            .iter()
            .all(|group| !group.expanded)
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(ExpandAllGroups), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", nested.0)).visible());
        assert!(window.find(("session-row", WEB_01)).visible());
    })
    .unwrap();
    let database = SessionDatabase::open(&path).unwrap();
    assert!(
        database
            .load()
            .unwrap()
            .groups
            .iter()
            .all(|group| group.expanded)
    );
    let (reopened, _) = open_workspace_with_store(cx, SessionStore::load(database).unwrap());
    cx.update_window(reopened.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", nested.0)).visible());
        assert!(window.find(("session-row", WEB_01)).visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn connect_group_opens_each_host_in_its_subtree(cx: &mut TestAppContext) {
    let mut store = SessionStore::seed();
    let child =
        store.insert_group_unnotified(GroupDraft::new("内部服务", Some(GroupId(PRODUCTION))));
    let grandchild = store.insert_group_unnotified(GroupDraft::new("后端", Some(child)));
    let nested_host = store.insert_unnotified(SessionDraft::new(
        "backend-01",
        "10.0.3.8",
        22,
        "deploy",
        AuthKind::Auto,
        Some(grandchild),
    ));
    let (handle, workspace) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectGroup(child)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.terminal_count(nested_host, cx), 1);
        assert_eq!(workspace.terminal_count(SessionId(WEB_01), cx), 1);
        assert_eq!(workspace.terminal_count(SessionId(DB_01), cx), 0);
        assert_eq!(workspace.terminal_count(SessionId(STAGING_API), cx), 1);
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.terminal_count(nested_host, cx), 2);
        assert_eq!(workspace.terminal_count(SessionId(WEB_01), cx), 2);
        assert_eq!(workspace.terminal_count(SessionId(WEB_02), cx), 1);
        assert_eq!(workspace.terminal_count(SessionId(DB_01), cx), 1);
        assert_eq!(workspace.terminal_count(SessionId(STAGING_API), cx), 1);
        assert_eq!(workspace.terminal_count(SessionId(DEV_BOX), cx), 0);
    });
}

#[gpui_kit::test]
fn dragging_a_host_into_a_group_updates_the_session_tree(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("group-count", PRODUCTION)).label(),
            Some("3 台主机")
        );
        assert_eq!(
            window.find(("group-count", DEVELOPMENT)).label(),
            Some("1 台主机")
        );
        window.within("session-tree").drag_to(
            ("session-row", DB_01),
            ("group-row", DEVELOPMENT),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(
            store.session(SessionId(DB_01)).unwrap().group,
            Some(GroupId(DEVELOPMENT))
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("group-count", PRODUCTION)).label(),
            Some("2 台主机")
        );
        assert_eq!(
            window.find(("group-count", DEVELOPMENT)).label(),
            Some("2 台主机")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn dragging_peers_changes_their_order_and_groups_can_nest(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .drag_to(("session-row", DB_01), ("session-row", WEB_01), cx);
        window.within("session-tree").drag_to(
            ("group-row", DEVELOPMENT),
            ("group-row", PRODUCTION),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(
            store.group(GroupId(DEVELOPMENT)).unwrap().parent,
            Some(GroupId(PRODUCTION))
        );
        let db = store.session(SessionId(DB_01)).unwrap();
        let web = store.session(SessionId(WEB_01)).unwrap();
        let web02 = store.session(SessionId(2)).unwrap();
        assert!(web.sort_order < db.sort_order && db.sort_order < web02.sort_order);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("group-count", PRODUCTION)).label(),
            Some("4 台主机")
        );
        assert_eq!(
            window.find(("group-count", DEVELOPMENT)).label(),
            Some("1 台主机")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn dragging_a_host_to_blank_tree_space_moves_it_to_the_root(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let from = window.find(("session-row", DB_01)).bounds().center();
        let tree = window.find("session-tree").bounds();
        window.drag(from, point(tree.center().x, tree.bottom() - px(12.)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.session(SessionId(DB_01)).unwrap().group, None);
    });
}

#[gpui_kit::test]
fn dragged_order_survives_reopening_the_database(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = SessionDatabase::open(&path).unwrap();
    let seed = SessionStore::seed();
    for group in seed.groups() {
        database.insert_group(group).unwrap();
    }
    for session in seed.sessions() {
        database.insert_session(session).unwrap();
    }
    let (handle, _) = open_workspace_with_store(cx, SessionStore::load(database).unwrap());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .drag_to(("session-row", DB_01), ("session-row", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    let reopened = SessionDatabase::open(&path).unwrap().load().unwrap();
    let order = |id| {
        reopened
            .sessions
            .iter()
            .find(|session| session.id == SessionId(id))
            .unwrap()
            .sort_order
    };
    assert!(order(WEB_01) < order(DB_01));
    assert!(order(DB_01) < order(2));
}

#[gpui_kit::test]
async fn sftp_button_opens_explorer_and_navigates(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // The active terminal tab's toolbar shows the SFTP button.
        window.click(("sftp", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("remote-path")
            .is_some_and(|p| p.value() == Some("/home/tester"))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("explorer", SFTP_TAB)).visible());
        assert_eq!(window.find("remote-path").value(), Some("/home/tester"));

        window
            .within(("remote-pane", SFTP_TAB))
            .double_click(ElementId::Name("name:..".into()), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/home")
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("remote-path").value(), Some("/home"));
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        let explorer = workspace
            .explorer(ExplorerId(SFTP_TAB))
            .expect("explorer open");
        assert_eq!(explorer.read(cx).remote().read(cx).path(), "/home");
    });
}

#[gpui_kit::test]
fn theme_toggle_flips_mode(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    let before = cx.update(|cx| {
        let theme = cx.theme();
        assert_eq!(theme.list_hover, theme.tokens.list_hover.color);
        assert!(theme.list_hover.a > 0.9);
        theme.is_dark()
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("theme-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let after = cx.update(|cx| {
        let theme = cx.theme();
        assert_eq!(theme.list_hover, theme.tokens.list_hover.color);
        assert!(theme.list_hover.a > 0.9);
        theme.is_dark()
    });
    assert_ne!(before, after);
}

#[gpui_kit::test]
fn tab_close_button_closes_the_terminal_and_disconnects(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        window.click(("close-terminal", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());
        assert!(
            window
                .try_find(("close-terminal", FIRST_NEW_TERMINAL))
                .is_none()
        );
        // The other seeded tabs are untouched.
        assert!(
            window
                .find(("close-terminal", INITIAL_WEB_TERMINAL))
                .visible()
        );
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(DB_01), cx).is_none());
        let store = workspace.store().read(cx);
        assert!(
            !store
                .session(SessionId(DB_01))
                .unwrap()
                .state
                .is_connected()
        );
    });
}

#[gpui_kit::test]
async fn closing_every_tab_shows_the_recent_sessions(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // While tabs are open the start page stays out of the way.
        assert!(window.try_find("recent-sessions").is_none());
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("terminal", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(window.try_find("recent-sessions").is_none());
        // The last tab closes too (the tab group alone would refuse).
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        assert!(window.find("recent-sessions").visible());
        // The page takes the focus the closed tab held.
        assert_eq!(window.find("recent-sessions").focused(), Some(true));
        // Both sessions had been connected, so both are listed.
        assert!(window.find(("recent-session", WEB_01)).visible());
        assert!(window.find(("recent-session", STAGING_API)).visible());
        assert_eq!(window.find("status-connection").label(), Some("未连接"));

        window.click(("recent-session", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());
        window.double_click(("recent-session", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 web-01")
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        assert!(window.try_find("recent-sessions").is_none());
        assert_eq!(
            window.find("status-connection").label(),
            Some("已连接 web-01")
        );
    })
    .unwrap();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let recent: Vec<_> = store
            .recent_sessions()
            .map(|session| session.name.to_string())
            .collect();
        // Reconnecting moved web-01 to the front.
        assert_eq!(recent, ["web-01", "staging-api"]);
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
        // Reopening the page must not reconnect the old selection on Enter.
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
        assert!(
            window
                .try_find(("terminal", FIRST_NEW_TERMINAL + 1))
                .is_none()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn enter_connects_the_selected_recent_session(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("recent-session", STAGING_API), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 staging-api")
    })
    .await;
}

/// The start page's row menu is the session tree's menu. Menus are not
/// driven here, so this dispatches what its items dispatch, from the page:
/// the page is drawn deferred over the dock, and its actions must still
/// reach the workspace.
#[gpui_kit::test]
async fn recent_session_menu_commands_work_from_the_start_page(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    for terminal in [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("close-terminal", terminal), cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("recent-sessions").focused(), Some(true));
        window.dispatch_action(Box::new(EditSession(SessionId(STAGING_API))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("session-name").value(), Some("staging-api"));
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();

    // A closed dialog leaves nothing focused; clicking the row focuses the
    // page again, as a right click would.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("recent-session", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DeleteSession(SessionId(WEB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("recent-session", WEB_01)).is_none());
        assert!(window.find(("recent-session", STAGING_API)).visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn settings_open_from_the_session_list_as_one_tab(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings").visible());
        assert_eq!(window.find("settings").focused(), Some(true));
        assert_eq!(window.find("settings-tab").label(), Some("设置"));
        // Two columns: the first category, 外观, is shown until another is
        // picked. `Settings` names its category rows by position.
        assert_eq!(appearance_dropdown(window, 0).as_deref(), Some("简体中文"));
        assert!(window.try_find("terminal-font-preview").is_none());
        window.within("settings").click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // 终端 is the second.
        assert!(window.find("terminal-font-preview").visible());
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        assert!(window.try_find("settings").is_none());
        // Opening settings again brings the same tab forward.
        #[cfg(target_os = "macos")]
        window.press("cmd-,", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-,", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings").visible());
        assert_eq!(window.find("settings").focused(), Some(true));
    })
    .unwrap();
    cx.update(|cx| {
        let settings = workspace.read(cx).settings_tab().expect("settings open");
        let group = settings.read(cx).tab_group().unwrap().upgrade().unwrap();
        // The two session terminals and a single settings tab.
        assert_eq!(group.read(cx).panels().len(), 3);
    });

    // Opening it while it is displayed changes nothing.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings").visible());
        assert_eq!(window.find("settings").focused(), Some(true));
        // ⌘W closes it like any other tab.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-tab").is_none());
        assert!(window.try_find("settings").is_none());
    })
    .unwrap();
    cx.update(|cx| assert!(workspace.read(cx).settings_tab().is_none()));
}

/// The label of one dropdown in the 外观 page's 常规 group. `Settings` names
/// its groups, items and dropdown buttons by position.
/// Open the settings tab on 外部 CLI, the third category.
fn open_external_cli_settings(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("settings").click("0-2", cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_external_cli_switch_is_off_until_turned_on(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let enabled = |cx: &mut TestAppContext| {
        workspace.read_with(cx, |workspace, cx| {
            workspace
                .settings()
                .read(cx)
                .settings()
                .external_cli
                .enabled
        })
    };
    open_external_cli_settings(cx, handle);
    assert!(!enabled(cx));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // 访问控制 › 启用外部 CLI.
        let switch = window
            .within("settings")
            .within("group-0")
            .within("item-0")
            .find("check");
        assert_eq!(switch.checked(), Some(false));
        window
            .within("settings")
            .within("group-0")
            .within("item-0")
            .click("check", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(enabled(cx));
}

#[gpui_kit::test]
fn without_a_home_to_install_into_the_external_cli_offers_nothing(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    open_external_cli_settings(cx, handle);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("skill-status-codex").label(),
            Some("此系统暂不支持安装")
        );
        assert!(window.try_find("skill-codex").is_none());
        assert!(window.try_find("cli-binary").is_none());
        // Copying the skill needs no home directory.
        window.click("copy-agent-skill", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(shellrs::cli::SKILL.to_string())
    );
}

#[gpui_kit::test]
fn agent_skills_install_where_each_agent_looks_and_come_off_again(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let root = tempfile::tempdir().unwrap();
    let exe = root.path().join("app").join("shellrs");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "binary").unwrap();
    let paths = IntegrationPaths {
        home: root.path().join("home"),
        bin_link: root.path().join("bin").join("shellrs"),
        exe,
        user_path: None,
    };
    let codex = paths.skill_file(AgentKind::Codex);
    workspace.update(cx, |workspace, cx| {
        workspace.cli_integration().update(cx, |integration, cx| {
            integration.set_paths(Some(paths.clone()), cx)
        })
    });
    open_external_cli_settings(cx, handle);

    let status = |cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.find(id).label().map(str::to_string)
        })
        .unwrap()
    };
    assert_eq!(
        status(cx, "skill-status-codex").as_deref(),
        Some(format!("未安装（{}）", codex.display()).as_str())
    );

    // The row's button dispatches the install.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("skill-codex", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        std::fs::read_to_string(&codex).unwrap(),
        shellrs::cli::SKILL
    );
    assert_eq!(
        status(cx, "skill-status-codex").as_deref(),
        Some(format!("已安装于 {}", codex.display()).as_str())
    );
    // Only Codex's.
    assert!(!paths.skill_file(AgentKind::ClaudeCode).exists());

    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(RemoveAgentSkill(AgentKind::Codex)), cx);
        window.dispatch_action(Box::new(InstallCliCommand), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!codex.exists());
    assert_eq!(
        status(cx, "cli-binary-status").as_deref(),
        Some(format!("已安装于 {}", paths.bin_link.display()).as_str())
    );
}

fn appearance_dropdown(window: &mut gpui_kit::Window, item: usize) -> Option<String> {
    window
        .within("settings")
        .within("group-0")
        .within(format!("item-{item}"))
        .find("btn")
        .label()
        .map(str::to_string)
}

/// Menus are not driven here: a dropdown's item writes the settings store,
/// so the test writes it the same way and checks what follows.
#[gpui_kit::test]
fn the_appearance_setting_drives_the_theme_and_the_title_bar_switch(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // 外观 is the first category.
        window.within("settings").click("0-0", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(appearance_dropdown(window, 0).as_deref(), Some("简体中文"));
        assert_eq!(appearance_dropdown(window, 1).as_deref(), Some("跟随系统"));
    })
    .unwrap();

    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.appearance = Appearance::Dark, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let theme = cx.theme();
        assert!(theme.is_dark());
        // Applied like the title bar switch always did it.
        assert_eq!(theme.list_hover, theme.tokens.list_hover.color);
        assert!(theme.list_hover.a > 0.9);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(appearance_dropdown(window, 1).as_deref(), Some("深色"));
        // The title bar's switch picks the other appearance outright.
        window.click("theme-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        assert!(!cx.theme().is_dark());
        let chosen = settings.read(cx).settings();
        assert_eq!(chosen.appearance, Appearance::Light);
        assert_eq!(chosen.language, InterfaceLanguage::SimplifiedChinese);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(appearance_dropdown(window, 1).as_deref(), Some("浅色"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn close_shortcut_closes_the_displayed_tab_down_to_none(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // The new tab is displayed, with its input focused.
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());
        assert!(window.try_find("recent-sessions").is_none());
        // Keep closing whichever tab the dock displays next.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("terminal", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(
            window
                .try_find(("terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        assert!(window.find("recent-sessions").visible());
        // With nothing open the shortcut does nothing.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn creates_independent_local_terminals_from_button_and_shortcut(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .map(|element| element.label() == Some("运行中 本地终端"))
            .unwrap_or(false)
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("local-terminal", 1_u64)).visible());
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
        #[cfg(target_os = "macos")]
        window.press("cmd-t", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-t", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .map(|element| element.label() == Some("运行中 本地终端"))
            .unwrap_or(false)
            && window.try_find(("local-terminal", 2_u64)).is_some()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("local-terminal", 2_u64)).visible());
        assert_eq!(
            window.find("status-connection").label(),
            Some("运行中 本地终端")
        );
        window.click(("close-local-terminal", 2_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("local-terminal", 2_u64)).is_none());
        assert!(window.find(("local-terminal", 1_u64)).visible());
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.local_terminal(LocalTerminalId(1)).is_some());
        assert!(workspace.local_terminal(LocalTerminalId(2)).is_none());
        assert!(workspace.store().read(cx).active().is_none());
        assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    });
}

#[gpui_kit::test]
async fn tab_keys_are_sent_to_the_focused_terminal(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .is_some_and(|element| element.label() == Some("运行中 本地终端"))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
        window.press("tab", cx);
        window.press("shift-tab", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "\t\x1b[Z"
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn exited_local_terminal_keeps_its_tab_and_restarts_fresh(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::exit_first());
    let (handle, workspace) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("local-terminal-exit", 1_u64)).is_some()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("local-terminal", 1_u64)).visible());
        assert!(window.find(("local-terminal-exit", 1_u64)).visible());
        cx.write_to_clipboard(ClipboardItem::new_string("退出后不可粘贴".into()));
        #[cfg(target_os = "macos")]
        window.press("cmd-v", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-v", cx);
        assert!(factory.written_text().is_empty());

        let bounds = window.find(("local-terminal", 1_u64)).bounds();
        window.drag(
            point(bounds.left() + px(1.), bounds.top() + px(8.)),
            point(bounds.right() - px(1.), bounds.top() + px(8.)),
            cx,
        );
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("run:1$ alpha.txt 会议纪要.md".into())
        );
        window.click(("restart-local-terminal", 1_u64), cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("local-terminal-exit", 1_u64)).is_none()
            && window
                .try_find("status-connection")
                .map(|element| element.label() == Some("运行中 本地终端"))
                .unwrap_or(false)
    })
    .await;

    cx.update(|cx| {
        let panel = workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .clone();
        assert_eq!(
            panel.read(cx).status(cx).lifecycle(),
            &TerminalLifecycle::Running
        );
        assert_eq!(factory.starts.load(Ordering::SeqCst), 2);
    });
}

#[gpui_kit::test]
async fn local_terminal_start_failure_is_kept_with_a_chinese_error(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::fail_first());
    let (handle, workspace) = open_workspace_with_factory(cx, factory);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("local-terminal-exit", 1_u64)).is_some()
    })
    .await;

    cx.update(|cx| {
        let panel = workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .clone();
        assert!(matches!(
            panel.read(cx).status(cx).lifecycle(),
            TerminalLifecycle::Failed(error) if error.contains("测试启动失败")
        ));
        assert!(
            panel
                .read(cx)
                .terminal()
                .read(cx)
                .screen_text(cx)
                .contains("终端错误：测试启动失败")
        );
    });
}

#[gpui_kit::test]
async fn terminal_selection_copies_only_on_command_and_finishes_outside_view(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, _) = open_workspace_with_factory(cx, factory);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .is_some_and(|element| element.label() == Some("运行中 本地终端"))
    })
    .await;

    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("原剪贴板".into())));
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.right_click(("local-terminal", 1_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let mut popup = window.within("popup-menu");
        assert_eq!(popup.find(0_usize).label(), Some("复制"));
        popup.click(0_usize, cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("原剪贴板".into())
        );
        window.click("session-search", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let bounds = window.find(("local-terminal", 1_u64)).bounds();
        let from = point(bounds.left() + px(1.), bounds.top() + px(8.));
        let to = point(bounds.right() - px(1.), bounds.top() + px(8.));
        window.drag(from, to, cx);

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("原剪贴板".into())
        );
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("run:1$ alpha.txt 会议纪要.md".into())
        );

        window.right_click(("local-terminal", 1_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let mut popup = window.within("popup-menu");
        assert_eq!(popup.find(0_usize).label(), Some("复制"));
        assert_eq!(popup.find(1_usize).label(), Some("粘贴"));
        popup.click(0_usize, cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let bounds = window.find(("local-terminal", 1_u64)).bounds();
        let from = point(bounds.left() + px(100.), bounds.top() + px(8.));
        let outside = point(px(20.), bounds.top() + px(8.));
        let to = point(bounds.right() - px(1.), bounds.top() + px(8.));
        window.drag(from, outside, cx);
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        let selection_after_release = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("outside release leaves a valid selection");

        cx.write_to_clipboard(ClipboardItem::new_string("哨兵".into()));
        window.dispatch_event(
            MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        #[cfg(target_os = "macos")]
        window.press("cmd-c", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-shift-c", cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(selection_after_release)
        );
        window.click(("close-local-terminal", 1_u64), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn cjk_input_is_sent_once_and_resize_commands_are_deduplicated(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_factory(cx, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .is_some_and(|element| element.label() == Some("运行中 本地终端"))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("输入一次", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "输入一次"
    })
    .await;

    cx.update(|cx| {
        let terminal = workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .expect("local terminal exists")
            .read(cx)
            .terminal()
            .clone();
        let screen = terminal.read(cx).screen_text(cx);
        assert_eq!(screen.matches("输入一次").count(), 1);
    });

    let resizes = factory
        .resizes
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(!resizes.is_empty());
    assert!(resizes.windows(2).all(|sizes| sizes[0] != sizes[1]));
}

/// Open a local terminal on `factory` and wait for the fake shell's banner.
async fn open_running_local_terminal(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    let (handle, workspace) = open_workspace_with_factory(cx, factory);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("status-connection")
            .is_some_and(|element| element.label() == Some("运行中 本地终端"))
    })
    .await;
    (handle, workspace)
}

/// The row height the fake shell was last told about.
fn last_cell_height(factory: &FakeTerminalFactory) -> Option<u16> {
    factory
        .resizes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .last()
        .map(TerminalSize::cell_height)
}

/// Menus are not driven here: the settings page's fields write the settings
/// store, so the test writes it the same way.
#[gpui_kit::test]
async fn the_terminal_font_setting_reaches_open_terminals_and_the_preview(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory.clone()).await;
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());

    // The defaults keep the 20 px rows terminals had before the setting.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        last_cell_height(&factory) == Some(20)
    })
    .await;

    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| {
                    settings.terminal_font.size = 16.;
                    settings.terminal_font.line_height = 1.5;
                },
                cx,
            )
        })
    });
    // The open terminal takes the new rows at once.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        last_cell_height(&factory) == Some(24)
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // 终端 is the second category.
        window.within("settings").click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let default_family = cx.update(|cx| cx.theme().mono_font_family.to_string());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let field = |window: &mut gpui_kit::Window, item: usize| {
            window
                .within("settings")
                .within("group-0")
                .within(format!("item-{item}"))
                .find("btn")
                .label()
                .map(str::to_string)
        };
        assert_eq!(field(window, 0), Some(format!("{default_family}（默认）")));
        assert_eq!(field(window, 1).as_deref(), Some("16"));
        assert_eq!(
            window.find("terminal-font-preview").label(),
            Some(format!("{default_family} 16 px，行高 1.5").as_str())
        );
    })
    .unwrap();

    // A family that is not installed, say from a file copied from another
    // machine, gives way to the default instead of a wrong font.
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| settings.terminal_font.family = Some("没有这个字体".into()),
                cx,
            )
        })
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(cx.global::<TerminalFont>().family, None);
        assert_eq!(
            settings.read(cx).settings().terminal_font.family.as_deref(),
            Some("没有这个字体")
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("terminal-font-preview").label(),
            Some(format!("{default_family} 16 px，行高 1.5").as_str())
        );
    })
    .unwrap();
}

fn local_screen(workspace: &Entity<Workspace>, cx: &App) -> String {
    workspace
        .read(cx)
        .local_terminal(LocalTerminalId(1))
        .expect("local terminal exists")
        .read(cx)
        .terminal()
        .read(cx)
        .screen_text(cx)
}

fn remote_lifecycle(
    workspace: &Entity<Workspace>,
    id: RemoteTerminalId,
    cx: &App,
) -> TerminalLifecycle {
    workspace
        .read(cx)
        .remote_terminal(id)
        .expect("terminal exists")
        .read(cx)
        .lifecycle(cx)
}

async fn wait_for_find_count(cx: &mut TestAppContext, handle: WindowHandle<Root>, label: &str) {
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("terminal-find-count")
            .is_some_and(|element| element.label() == Some(label))
    })
    .await;
}

#[gpui_kit::test]
async fn find_highlights_matches_and_steps_between_them(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory.clone()).await;
    // The banner holds one 「alpha」; the fake shell echoes two more.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("alpha alpha", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        local_screen(&workspace, cx).matches("alpha").count() == 3
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(FindInTerminal), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("terminal-find").focused(), Some(true));
        // Nothing is counted before there is a query.
        assert!(window.try_find("terminal-find-count").is_none());
        // The bar floats over the terminal; clicking it must not hand the
        // keyboard back to the terminal underneath.
        window.click("terminal-find", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("terminal-find").focused(), Some(true));
        window.input("alpha", cx);
    })
    .unwrap();
    // The first match on screen is focused.
    wait_for_find_count(cx, handle, "1/3").await;
    // What is typed into the find bar never reaches the shell.
    assert_eq!(factory.written_text(), "alpha alpha");

    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(FindNextInTerminal), cx);
    })
    .unwrap();
    wait_for_find_count(cx, handle, "2/3").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(FindPreviousInTerminal), cx);
        window.dispatch_action(Box::new(FindPreviousInTerminal), cx);
    })
    .unwrap();
    // Stepping up from the first match wraps to the last.
    wait_for_find_count(cx, handle, "3/3").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("enter", cx);
    })
    .unwrap();
    wait_for_find_count(cx, handle, "1/3").await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("backspace", cx);
        window.input("z", cx);
    })
    .unwrap();
    wait_for_find_count(cx, handle, "无结果").await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("terminal-find").is_none());
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
        window.input("!", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "alpha alpha!"
    })
    .await;
}

#[gpui_kit::test]
async fn clearing_a_terminal_keeps_only_the_prompt_line(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory.clone()).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.input("root@localhost:~# ", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        local_screen(&workspace, cx).contains("root@localhost:~#")
    })
    .await;
    assert!(
        cx.update(|cx| local_screen(&workspace, cx))
            .contains("alpha.txt")
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ClearTerminal), cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(
        cx.update(|cx| local_screen(&workspace, cx)).trim_end(),
        "root@localhost:~#"
    );
    // Clearing happens on this side; the shell is not sent anything.
    assert_eq!(factory.written_text(), "root@localhost:~# ");
}

#[gpui_kit::test]
async fn disconnecting_one_tab_leaves_the_other_tabs_of_its_session(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let second = RemoteTerminalId(FIRST_NEW_TERMINAL);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectSession(SessionId(WEB_01))), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        remote_lifecycle(&workspace, second, cx) == TerminalLifecycle::Running
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DisconnectTerminal(second)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        assert!(!remote_lifecycle(&workspace, second, cx).accepts_input());
        assert_eq!(
            remote_lifecycle(&workspace, RemoteTerminalId(INITIAL_WEB_TERMINAL), cx),
            TerminalLifecycle::Running
        );
        let workspace = workspace.read(cx);
        let store = workspace.store().read(cx);
        assert_eq!(
            store.session(SessionId(WEB_01)).unwrap().state,
            ConnectionState::Connected
        );
        let screen = workspace
            .remote_terminal(second)
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .screen_text(cx);
        assert!(screen.contains("已断开连接"));
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ReconnectTerminal(second)), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        remote_lifecycle(&workspace, second, cx) == TerminalLifecycle::Running
    })
    .await;
}

#[gpui_kit::test]
async fn new_group_from_the_toolbar_appears_in_the_tree(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-group", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        // An unnamed group is rejected and the dialog stays open.
        window.click("group-name", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("form-error").visible());
        window.click("group-name", cx);
        window.input("预发", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let created = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store.groups().last().expect("group inserted");
        assert_eq!(created.name.as_ref(), "预发");
        assert_eq!(created.parent, None);
        created.id
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tree = window.within("session-tree");
        assert!(tree.find(("group-row", created.0)).visible());
        assert_eq!(
            tree.find(("group-count", created.0)).label(),
            Some("0 台主机")
        );
    })
    .unwrap();
}

/// The action a group row's 新建会话… menu entry dispatches.
#[gpui_kit::test]
async fn a_new_session_in_a_group_starts_out_in_that_group(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(NewSessionInGroup(GroupId(DEVELOPMENT))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("session-name", cx);
        window.input("dev-02", cx);
        window.click("session-host", cx);
        window.input("192.168.1.21", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .sessions()
            .iter()
            .find(|session| session.name == "dev-02")
            .expect("dev-02 inserted");
        // The form opened with 开发 pre-selected and nothing changed it.
        assert_eq!(created.group, Some(GroupId(DEVELOPMENT)));
    });
}

#[gpui_kit::test]
async fn renaming_a_group_keeps_the_sessions_under_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(RenameGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        window.click("group-name", cx);
        window.press("cmd-a", cx);
        window.input("生产环境", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let group = store.group(GroupId(PRODUCTION)).expect("group kept");
        assert_eq!(group.name.as_ref(), "生产环境");
        assert_eq!(store.group_path(GroupId(PRODUCTION)), "生产环境");
        // The three sessions still belong to it.
        assert_eq!(
            store
                .sessions()
                .iter()
                .filter(|s| s.group == Some(GroupId(PRODUCTION)))
                .count(),
            3
        );
    });
}

#[gpui_kit::test]
async fn deleting_a_group_removes_its_sessions_and_closes_their_tabs(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    // db-01 joins web-01, which starts connected, in having an open tab.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(WEB_01), cx).is_some());
        assert!(workspace.terminal(SessionId(DB_01), cx).is_some());
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DeleteGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(SessionId(WEB_01), cx).is_none());
        assert!(workspace.terminal(SessionId(DB_01), cx).is_none());
        // staging-api is in another group and keeps its tab.
        assert!(workspace.terminal(SessionId(STAGING_API), cx).is_some());

        let store = workspace.store().read(cx);
        assert!(store.group(GroupId(PRODUCTION)).is_none());
        assert_eq!(store.groups().len(), 2);
        let names: Vec<_> = store
            .sessions()
            .iter()
            .map(|session| session.name.as_ref())
            .collect();
        assert_eq!(names, ["staging-api", "qa-runner", "dev-box"]);
    });
}

#[gpui_kit::test]
async fn groups_and_sessions_are_read_back_from_the_database(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("shellrs.db");
    let store = SessionStore::load(SessionDatabase::open(&path).expect("database opened"))
        .expect("store loaded");
    // A first launch starts with nothing at all.
    assert_eq!(store.groups().len(), 0);
    assert_eq!(store.sessions().len(), 0);
    let (handle, workspace) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-group", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("group-name", cx);
        window.input("生产", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let group = cx.update(|cx| workspace.read(cx).store().read(cx).groups()[0].id);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // A closed dialog leaves nothing focused, and an action only reaches
        // handlers on the focused element's path.
        window.click("session-search", cx);
        window.dispatch_action(Box::new(NewSessionInGroup(group)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("session-name", cx);
        window.input("web-01", cx);
        window.click("session-host", cx);
        window.input("10.0.1.12", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let session = cx.update(|cx| workspace.read(cx).store().read(cx).sessions()[0].id);

    // Connecting is what puts a session on the start page's recent list.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", session.0), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 web-01")
    })
    .await;

    // Everything above went through the real write path; read it back with a
    // second connection to the same file.
    let reloaded = SessionStore::load(SessionDatabase::open(&path).expect("database reopened"))
        .expect("store reloaded");
    assert_eq!(reloaded.groups().len(), 1);
    assert_eq!(reloaded.groups()[0].name.as_ref(), "生产");
    assert_eq!(reloaded.sessions().len(), 1);
    let saved = &reloaded.sessions()[0];
    assert_eq!(saved.name.as_ref(), "web-01");
    assert_eq!(saved.host.as_ref(), "10.0.1.12");
    assert_eq!(saved.port, 22);
    assert_eq!(saved.group, Some(group));
    // Runtime state is not persisted, but the last connection time is.
    assert!(!saved.state.is_connected());
    assert_eq!(
        reloaded.recent_sessions().map(|s| s.id).collect::<Vec<_>>(),
        [session]
    );
}

/// A session's terminal and SFTP tabs sit side by side in the center, and
/// only the active one renders. The tab going inactive used to keep the
/// window focus, which took its focus handle out of the dispatch tree and
/// left every 「×」 dead.
#[gpui_kit::test]
fn both_tabs_of_one_session_stay_closable(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    // The terminal tab is active and focused; opening SFTP puts a second tab
    // for the same session beside it and activates that one.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("sftp", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("explorer", SFTP_TAB)).visible());
        window.click(("close-explorer", SFTP_TAB), cx);
    })
    .unwrap();
    cx.run_until_parked();

    // The SFTP tab is gone and the terminal tab, now active again, still closes.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("close-explorer", SFTP_TAB)).is_none());
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        window.click(("close-terminal", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("close-terminal", FIRST_NEW_TERMINAL))
                .is_none()
        );
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.explorer(ExplorerId(SFTP_TAB)).is_none());
        assert!(workspace.terminal(SessionId(DB_01), cx).is_none());
    });
}

#[gpui_kit::test]
async fn a_terminal_tab_can_be_renamed_and_follow_the_session_again(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let tab = ("terminal-tab", INITIAL_WEB_TERMINAL);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some("web-01"));
        // Undetected hosts show the fallback mark in the tab too.
        assert_eq!(
            window
                .find(("terminal-tab-os", INITIAL_WEB_TERMINAL))
                .label(),
            Some("未探测到系统")
        );
        window.dispatch_action(
            Box::new(RenameTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("tab-name").value(), Some("web-01"));
        window.click("tab-name", cx);
        window.press("cmd-a", cx);
        window.input("日志排查", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some("日志排查"));
        // The other tab of the bar keeps its session name.
        assert_eq!(
            window
                .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
                .label(),
            Some("staging-api")
        );
    })
    .unwrap();
    cx.update(|cx| {
        // A tab title is not a session setting.
        let store = workspace.read(cx).store().read(cx);
        let session = store.session(SessionId(WEB_01)).expect("session kept");
        assert_eq!(session.name.as_ref(), "web-01");
    });

    // Clearing the field returns the tab to the session name.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // A closed dialog leaves nothing focused, and an action only reaches
        // handlers on the focused element's path.
        window.click("session-search", cx);
        window.dispatch_action(
            Box::new(RenameTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("tab-name").value(), Some("日志排查"));
        window.click("tab-name", cx);
        window.press("cmd-a", cx);
        window.press("backspace", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some("web-01"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn copy_session_host_puts_the_host_on_the_clipboard(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(CopySessionHost(SessionId(STAGING_API))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    // Only the host field: no user, no port.
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("10.0.9.20".to_string())
    );
}

#[gpui_kit::test]
fn hovering_a_session_row_shows_its_address_beside_the_row(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    let row = ("session-row", WEB_01);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.hover(row, cx);
    })
    .unwrap();
    // Tooltips wait half a second before they open.
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tooltip = window.find("session-tooltip");
        assert_eq!(tooltip.label(), Some("root@10.0.1.12:22"));
        // Beside the row, so it never covers the rows below.
        let row_bounds = window.find(row).bounds();
        assert!(tooltip.bounds().left() >= row_bounds.right());
        assert!((tooltip.bounds().center().y - row_bounds.center().y).abs() < px(2.));
        // A click puts it away.
        window.click(row, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("session-tooltip").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_row_tooltip_follows_the_pointer_down_the_list(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    let move_to = |cx: &mut TestAppContext, id: u64| {
        cx.update_window(handle.into(), |_, window, cx| {
            let row = window.find(("session-row", id)).bounds();
            window.dispatch_event(
                gpui_kit::PlatformInput::MouseMove(MouseMoveEvent {
                    position: point(row.left() + px(40.), row.center().y),
                    pressed_button: None,
                    modifiers: Default::default(),
                }),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_millis(1000));
        cx.run_until_parked();
    };
    let tooltip = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("session-tooltip")
                .and_then(|tooltip| tooltip.label().map(str::to_string))
        })
        .unwrap()
    };

    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    move_to(cx, WEB_01);
    assert_eq!(tooltip(cx).as_deref(), Some("root@10.0.1.12:22"));
    // Down the list the next row hears of the pointer before this one lets
    // go of it; the tooltip must survive that and move with the pointer.
    move_to(cx, WEB_02);
    assert_eq!(tooltip(cx).as_deref(), Some("root@10.0.1.13:22"));
    move_to(cx, WEB_01);
    assert_eq!(tooltip(cx).as_deref(), Some("root@10.0.1.12:22"));
}

#[gpui_kit::test]
fn copy_session_id_puts_the_public_id_on_the_clipboard(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let public_id = workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .session(SessionId(STAGING_API))
            .unwrap()
            .public_id
            .to_string()
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(CopySessionId(SessionId(STAGING_API))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(public_id.len(), 16);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(public_id)
    );
}

#[gpui_kit::test]
fn batch_close_commands_take_the_tabs_around_the_clicked_one(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let dispatch = |cx: &mut TestAppContext, action: Box<dyn gpui_kit::Action>| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.dispatch_action(action, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    let close = |tab, scope| Box::new(CloseTabs { tab, scope });
    let terminal = |id| CenterTab::Terminal(RemoteTerminalId(id));
    let open_terminals = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let workspace = workspace.read(cx);
            (1..=FIRST_NEW_TERMINAL + 1)
                .filter(|id| workspace.remote_terminal(RemoteTerminalId(*id)).is_some())
                .collect::<Vec<_>>()
        })
    };

    // [web-01, staging-api, web-01 · SFTP, local]
    dispatch(cx, Box::new(OpenExplorer(SessionId(WEB_01))));
    dispatch(cx, Box::new(NewLocalTerminal));
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.explorer(ExplorerId(SFTP_TAB)).is_some());
        assert!(workspace.local_terminal(LocalTerminalId(1)).is_some());
    });

    // Right of staging-api: the SFTP and local tabs, whatever their kind.
    dispatch(
        cx,
        close(terminal(INITIAL_STAGING_TERMINAL), CloseScope::Right),
    );
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.explorer(ExplorerId(SFTP_TAB)).is_none());
        assert!(workspace.local_terminal(LocalTerminalId(1)).is_none());
    });
    assert_eq!(
        open_terminals(cx),
        [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL]
    );

    // [web-01, staging-api, web-01 #2] → left of staging-api.
    dispatch(cx, Box::new(ConnectSession(SessionId(WEB_01))));
    dispatch(
        cx,
        close(terminal(INITIAL_STAGING_TERMINAL), CloseScope::Left),
    );
    assert_eq!(
        open_terminals(cx),
        [INITIAL_STAGING_TERMINAL, FIRST_NEW_TERMINAL]
    );

    // [staging-api, web-01 #2, staging-api #2] → others than web-01 #2.
    dispatch(cx, Box::new(ConnectSession(SessionId(STAGING_API))));
    assert_eq!(open_terminals(cx).len(), 3);
    dispatch(cx, close(terminal(FIRST_NEW_TERMINAL), CloseScope::Others));
    assert_eq!(open_terminals(cx), [FIRST_NEW_TERMINAL]);

    // All of them, down to the start page.
    dispatch(cx, close(terminal(FIRST_NEW_TERMINAL), CloseScope::All));
    assert!(open_terminals(cx).is_empty());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
    })
    .unwrap();
}

/// A store holding one session, wired to a keychain the test can inspect.
fn store_with_secrets(secrets: Arc<InMemorySecretStore>) -> (SessionStore, SessionId, SecretRef) {
    let mut store = SessionStore::empty();
    let id = store.insert_unnotified(SessionDraft::new(
        "db-01",
        "10.0.2.5",
        22,
        "postgres",
        AuthKind::Password,
        None,
    ));
    let endpoint = store.session(id).unwrap().password_secret();
    (store.with_secrets(secrets), id, endpoint)
}

#[gpui_kit::test]
async fn a_new_session_saves_its_password_to_the_keychain(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) =
        open_workspace_with_store(cx, SessionStore::empty().with_secrets(secrets.clone()));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-session", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("session-name", cx);
        window.input("db-02", cx);
        window.click("session-host", cx);
        window.input("10.0.3.7", cx);
        window.click("session-password", cx);
        window.input("hunter2", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&SecretRef::password("root", "10.0.3.7", 22))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2")
    );

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .sessions()
            .iter()
            .find(|session| session.name == "db-02")
            .expect("db-02 inserted");
        assert!(
            !format!("{created:?}").contains("hunter2"),
            "会话本身不该带着密码"
        );
    });
}

#[gpui_kit::test]
async fn editing_the_host_moves_the_saved_password_with_it(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditSession(id)), cx);
    })
    .unwrap();
    // The saved password is read on a background thread, then fills the field.
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("session-password").visible());
        window.click("session-host", cx);
        window.press("cmd-a", cx);
        window.input("10.9.9.9", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&SecretRef::password("postgres", "10.9.9.9", 22))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2"),
        "预填的密码应当跟着会话搬到新端点"
    );
    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        None,
        "没有会话再用旧端点了，旧条目应当被清掉"
    );
}

#[gpui_kit::test]
async fn clearing_the_password_field_forgets_the_saved_password(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditSession(id)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("session-password", cx);
        window.press("cmd-a", cx);
        window.press("backspace", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        None,
        "清空字段就是要删掉已保存的密码"
    );
}

#[gpui_kit::test]
async fn deleting_the_last_session_on_an_endpoint_forgets_its_password(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DeleteSession(id)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        None,
        "最后一个用这个端点的会话没了，密码也该没了"
    );
}

#[gpui_kit::test]
async fn the_tree_falls_back_to_the_first_letter_until_a_host_is_probed(cx: &mut TestAppContext) {
    let mut store = SessionStore::empty();
    let probed = store.insert_unnotified(SessionDraft::new(
        "web-01",
        "10.0.1.12",
        22,
        "root",
        AuthKind::Auto,
        None,
    ));
    let fresh = store.insert_unnotified(SessionDraft::new(
        "数据库",
        "10.0.2.5",
        22,
        "root",
        AuthKind::Auto,
        None,
    ));
    store.set_host_os_unnotified(probed, Some(HostOs::Debian));
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("session-os", probed.0)).label(),
            Some("Debian")
        );
        assert_eq!(
            window.find(("session-os", fresh.0)).label(),
            Some("未探测到系统"),
            "没探测过就退回名称首字的中性徽章"
        );
        // The badges carry brand colours of their own, so they have to hold up
        // in both themes.
        window.click("theme-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("session-os", probed.0)).visible());
        assert!(window.find(("session-os", fresh.0)).visible());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn connecting_marks_the_session_with_the_host_operating_system(cx: &mut TestAppContext) {
    let (store, id) = one_session_store(AuthKind::Auto);
    let factory = Arc::new(FakeTerminalFactory::reports_os(HostOs::Fedora));
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("session-os", id.0)).label(),
            Some("未探测到系统")
        );
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    // The engine batches transport events on a 16ms timer, so the mark
    // changes a frame or two after the tab opens.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find(("session-os", id.0)).label() == Some("Fedora")
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.session(id).unwrap().os, Some(HostOs::Fedora));
    });
}

#[gpui_kit::test]
async fn the_tab_bar_shows_the_connection_latency_while_it_runs(cx: &mut TestAppContext) {
    let (store, id) = one_session_store(AuthKind::Auto);
    let factory = Arc::new(FakeTerminalFactory::reports_latency(Latency::Measured(
        Duration::from_millis(32),
    )));
    let (handle, _) = open_workspace_with_remote_factory(cx, store, factory);
    // The first terminal of a store with nothing connected.
    let terminal = 1_u64;
    let latency = ("terminal-latency", terminal);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("session-tree")
            .double_click(("session-row", id.0), cx);
    })
    .unwrap();
    // Transport events reach the engine on its 16ms batching timer.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(latency)
            .is_some_and(|element| element.label() == Some("32 ms"))
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", terminal), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(latency).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn the_start_page_marks_recent_hosts_with_their_operating_system(cx: &mut TestAppContext) {
    let mut store = SessionStore::empty();
    let id = store.insert_unnotified(SessionDraft::new(
        "web-01",
        "10.0.1.12",
        22,
        "root",
        AuthKind::Auto,
        None,
    ));
    // Connecting once puts it on the start page; disconnecting keeps it there
    // and leaves the centre empty, which is when that page shows.
    store.set_state_unnotified(id, ConnectionState::Connected);
    store.set_state_unnotified(id, ConnectionState::Disconnected);
    store.set_host_os_unnotified(id, Some(HostOs::Ubuntu));
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-sessions").visible());
        assert_eq!(
            window.find(("recent-session-os", id.0)).label(),
            Some("Ubuntu"),
            "开始页和会话树用同一个标记"
        );
    })
    .unwrap();
}

#[derive(Default)]
struct FakeSftpProvider {
    requests: Arc<Mutex<Vec<UploadRequest>>>,
    downloads: Arc<Mutex<Vec<shellrs::sftp::DownloadRequest>>>,
    operations: Arc<Mutex<Vec<shellrs::sftp::RemoteOperation>>>,
    events: Arc<Mutex<Vec<async_channel::Sender<SftpEvent>>>>,
    /// When set, the next connection waits for a message before it is up.
    hold_connection: Arc<Mutex<Option<mpsc::Receiver<()>>>>,
    /// Connections made again on 重新连接 (a bare 继续).
    reconnects: Arc<Mutex<usize>>,
}
impl SftpTransportProvider for FakeSftpProvider {
    fn create(&self, _: &shellrs::session::Session) -> Box<dyn SftpTransport> {
        Box::new(FakeSftpTransport {
            requests: self.requests.clone(),
            downloads: self.downloads.clone(),
            operations: self.operations.clone(),
            events: self.events.clone(),
            hold: self.hold_connection.lock().unwrap().take(),
            reconnects: self.reconnects.clone(),
        })
    }
}
struct FakeSftpTransport {
    requests: Arc<Mutex<Vec<UploadRequest>>>,
    downloads: Arc<Mutex<Vec<shellrs::sftp::DownloadRequest>>>,
    operations: Arc<Mutex<Vec<shellrs::sftp::RemoteOperation>>>,
    events: Arc<Mutex<Vec<async_channel::Sender<SftpEvent>>>>,
    hold: Option<mpsc::Receiver<()>>,
    reconnects: Arc<Mutex<usize>>,
}
impl SftpTransport for FakeSftpTransport {
    fn run(
        self: Box<Self>,
        commands: async_channel::Receiver<SftpCommand>,
        events: async_channel::Sender<SftpEvent>,
    ) -> anyhow::Result<()> {
        use shellrs::sftp::{TransferChoice, TransferPhase, TransferProgress};
        self.events.lock().unwrap().push(events.clone());
        if let Some(hold) = &self.hold {
            let _ = hold.recv();
        }
        events.send_blocking(SftpEvent::Connected {
            home: RemotePath::new("/home/tester")?,
        })?;
        // As in the real engine, 继续 goes on with a stopped batch, and with
        // none it only reconnects.
        let mut stopped = false;
        while let Ok(command) = commands.recv_blocking() {
            match command {
                SftpCommand::List { request_id, path } => {
                    if path.as_str() == "/slow" {
                        continue;
                    }
                    let result = match path.as_str() {
                        "/denied" => Err("权限不足".into()),
                        "/empty" => Ok(DirectoryListing::new("/empty", vec![])),
                        _ => Ok(fake_listing(path.as_str())),
                    };
                    events.send_blocking(SftpEvent::Listed { request_id, result })?;
                }
                SftpCommand::Operate {
                    request_id,
                    operation,
                } => {
                    let refused = format!("{operation:?}").contains("denied");
                    self.operations.lock().unwrap().push(operation);
                    events.send_blocking(SftpEvent::Operated {
                        request_id,
                        result: if refused {
                            Err("权限不足".into())
                        } else {
                            Ok(())
                        },
                    })?;
                }
                SftpCommand::Download(request) => {
                    self.downloads.lock().unwrap().push(request);
                    events.send_blocking(SftpEvent::Progress(
                        TransferProgress::new(TransferPhase::Transferring)
                            .with_direction(shellrs::sftp::TransferDirection::Download),
                    ))?;
                }
                SftpCommand::Upload(request) => {
                    self.requests.lock().unwrap().push(request);
                    events.send_blocking(SftpEvent::Progress(Default::default()))?;
                }
                // The real engine says Idle once a batch stops or ends,
                // which is when the queue may move on.
                SftpCommand::Cancel => {
                    stopped = true;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        TransferPhase::Stopped,
                    )))?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Discard => {
                    stopped = false;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        TransferPhase::Completed,
                    )))?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Resume if stopped => {
                    stopped = false;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        TransferPhase::Transferring,
                    )))?;
                }
                SftpCommand::Resume => {
                    *self.reconnects.lock().unwrap() += 1;
                    events.send_blocking(SftpEvent::Connected {
                        home: RemotePath::new("/home/tester")?,
                    })?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Answer { answer, .. } => {
                    stopped = answer.choice() == TransferChoice::Cancel;
                    events.send_blocking(SftpEvent::Progress(TransferProgress::new(
                        if answer.choice() == TransferChoice::Cancel {
                            TransferPhase::Stopped
                        } else {
                            TransferPhase::Completed
                        },
                    )))?;
                    events.send_blocking(SftpEvent::Idle)?;
                }
                SftpCommand::Shutdown => break,
                _ => {}
            }
        }
        Ok(())
    }
}
/// The local file system of the tests: every directory lists
/// `fake_listing`, and changes are recorded, never made.
#[derive(Clone, Default)]
struct FakeLocalDirectory {
    calls: Arc<Mutex<Vec<String>>>,
}
impl FakeLocalDirectory {
    fn record(&self, call: String) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}
impl LocalDirectoryProvider for FakeLocalDirectory {
    fn home(&self) -> std::path::PathBuf {
        "/local/tester".into()
    }
    fn list(&self, path: &std::path::Path) -> anyhow::Result<DirectoryListing> {
        Ok(fake_listing(path.to_str().unwrap()))
    }
    fn trash(&self, paths: &[std::path::PathBuf]) -> anyhow::Result<()> {
        self.record(format!("trash {paths:?}"))
    }
    fn rename(&self, from: &std::path::Path, to: &std::path::Path) -> anyhow::Result<()> {
        self.record(format!("rename {} -> {}", from.display(), to.display()))
    }
    fn create_dir(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.record(format!("mkdir {}", path.display()))
    }
    fn create_file(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.record(format!("touch {}", path.display()))
    }
    fn set_permissions(
        &self,
        paths: &[std::path::PathBuf],
        edit: shellrs::sftp::PermissionEdit,
        recursive: bool,
        add_x_to_dirs: bool,
    ) -> anyhow::Result<()> {
        self.record(format!(
            "chmod {paths:?} +{:o} -{:o} recursive={recursive} x={add_x_to_dirs}",
            edit.set(),
            edit.clear()
        ))
    }
}
fn fake_listing(path: &str) -> DirectoryListing {
    DirectoryListing::new(
        path,
        vec![
            DirectoryEntry::new(
                "文件 甲.txt",
                FileMetadata::new(EntryKind::File, 12, Some(100), Some(0o644)),
            ),
            DirectoryEntry::new(
                "文件 乙.txt",
                FileMetadata::new(EntryKind::File, 34, Some(200), Some(0o644)),
            ),
            DirectoryEntry::new(
                "目录",
                FileMetadata::new(EntryKind::Directory, 0, None, Some(0o755)),
            ),
            DirectoryEntry::new(
                "链接目录",
                FileMetadata::new(EntryKind::Symlink, 7, Some(300), Some(0o120_777)),
            )
            .with_target_kind(Some(EntryKind::Directory)),
        ]
        .into_iter()
        .map(|entry| entry.with_owner(Some("root".into()), Some("wheel".into())))
        .collect(),
    )
}

fn open_workspace_with_sftp(
    cx: &mut TestAppContext,
    provider: Arc<FakeSftpProvider>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_services(cx, provider, FakeLocalDirectory::default())
}
fn open_workspace_with_services(
    cx: &mut TestAppContext,
    provider: Arc<FakeSftpProvider>,
    local: FakeLocalDirectory,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellrs::init);
    // Dialogs slide in over real time; small targets such as checkboxes
    // would move between the frame that locates them and the click.
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| SessionStore::seed());
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| SettingsStore::in_memory()),
                remote,
                Arc::new(FakeTerminalFactory::default()),
                provider,
                Arc::new(local),
                Arc::new(FakeConnectionTester::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    (handle, workspace.unwrap())
}
async fn open_test_explorer(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(OpenExplorer(SessionId(DB_01))), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find("remote-path")
            .is_some_and(|p| p.value() == Some("/home/tester"))
            && window
                .within(("local-pane", SFTP_TAB))
                .try_find("file:文件 甲.txt")
                .is_some()
    })
    .await;
}

/// Wait until the batch at the head of the transfer queue reads `status`.
async fn wait_for_head_status(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    status: &'static str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(2), move |window, cx| {
        window.render_frame(cx);
        window
            .try_find("transfer-status")
            .is_some_and(|head| head.label() == Some(status))
    })
    .await;
}

/// `TestWindowExt::click` sends no modifiers, and file lists need ⌘ and
/// Shift clicks.
fn modified_click(
    window: &mut gpui_kit::Window,
    pane: (&'static str, u64),
    row: &str,
    modifiers: gpui_kit::Modifiers,
    cx: &mut App,
) {
    let row = ElementId::Name(row.to_string().into());
    let position = window.within(pane).find(row).bounds().center();
    click_at(window, position, modifiers, cx);
}

/// A left click at a point, for places without an element of their own.
fn click_at(
    window: &mut gpui_kit::Window,
    position: gpui_kit::Point<gpui_kit::Pixels>,
    modifiers: gpui_kit::Modifiers,
    cx: &mut App,
) {
    use gpui_kit::{MouseDownEvent, MouseUpEvent};
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers,
        }
        .to_platform_input(),
        cx,
    );
    window.render_frame(cx);
    window.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    window.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
}

/// The selected names of one explorer pane, in display order.
fn pane_selection(workspace: &Entity<Workspace>, remote: bool, cx: &App) -> Vec<String> {
    workspace
        .read(cx)
        .explorer(ExplorerId(SFTP_TAB))
        .unwrap()
        .read(cx)
        .pane(remote)
        .read(cx)
        .selected_names(cx)
}

#[gpui_kit::test]
async fn sftp_multi_selection_keyboard_upload_freezes_paths_and_cancel_resumes(
    cx: &mut TestAppContext,
) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    // Names sort by code point: 文件 乙 comes before 文件 甲.
    cx.update_window(handle.into(), |_, window, cx| {
        window
            .within(("local-pane", SFTP_TAB))
            .click("name:文件 乙.txt", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(pane_selection(&workspace, false, cx), ["文件 乙.txt"]));
    // Shift+↓ extends to the next row; Space takes the cursor row back out.
    for (key, expected) in [
        ("shift-down", &["文件 乙.txt", "文件 甲.txt"][..]),
        ("space", &["文件 乙.txt"][..]),
    ] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press(key, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| assert_eq!(pane_selection(&workspace, false, cx), expected));
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-a", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .explorer(ExplorerId(SFTP_TAB))
                .unwrap()
                .read(cx)
                .local()
                .read(cx)
                .upload_sources(cx)
                .len(),
            4
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("f5", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
        // Counted by kind, and each item on a line of its own by name, so a
        // long path cannot pass for two items.
        assert!(
            window
                .find("transfer-summary")
                .label()
                .unwrap()
                .starts_with("上传 4 个项目（2 个文件夹、2 个文件）到 ")
        );
        let source = |path: &str| {
            window
                .find(ElementId::Name(format!("source:{path}").into()))
                .label()
                .map(str::to_string)
        };
        assert_eq!(
            source("/local/tester/文件 甲.txt").as_deref(),
            Some("文件 甲.txt")
        );
        assert_eq!(
            source("/local/tester/链接目录").as_deref(),
            Some("链接目录")
        );
        window.click("upload-target", cx);
        window.press("cmd-a", cx);
        window.input("/固定目标", cx);
        window.click("upload-confirm", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "正在扫描").await;
    assert_eq!(
        provider.requests.lock().unwrap()[0].destination().as_str(),
        "/固定目标"
    );
    assert_eq!(provider.requests.lock().unwrap()[0].sources().len(), 4);
    // A batch unfolds to its item results. None has ended yet, and the list
    // says so rather than opening empty; a second click folds it again.
    for open in [true, false] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("queue-expand", 1u64), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.try_find("transfer-detail-empty").is_some(), open);
        })
        .unwrap();
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/other".into(),
                },
            )),
            cx,
        );
        window.click("cancel-transfer", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "已停止").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("resume-transfer", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "0%").await;
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(
        provider.requests.lock().unwrap()[0].destination().as_str(),
        "/固定目标"
    );
}

/// A transfer confirmed while another runs waits its turn in the queue,
/// WinSCP-style, and starts when the engine is free again.
#[gpui_kit::test]
async fn sftp_transfers_wait_their_turn_in_the_queue(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    use shellrs::sftp::{TransferPhase, TransferProgress};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    let upload = |cx: &mut TestAppContext, name: &str| {
        let paths = vec![std::path::PathBuf::from(format!("/local/tester/{name}"))];
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.dispatch_action(
                Box::new(ExplorerAction::new(
                    ExplorerId(SFTP_TAB),
                    ExplorerCommand::UploadPaths {
                        paths,
                        target: "/home/tester".into(),
                    },
                )),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
    };
    let confirm = |cx: &mut TestAppContext, queued: bool| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.try_find("transfer-queued-note").is_some(), queued);
            window.click("upload-confirm", cx);
        })
        .unwrap();
    };
    let label = |cx: &mut TestAppContext, id: ElementId| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find(id)
                .and_then(|row| row.label().map(str::to_string))
        })
        .unwrap()
    };
    let send = |event: SftpEvent| {
        provider.events.lock().unwrap()[0]
            .send_blocking(event)
            .unwrap()
    };

    upload(cx, "文件 甲.txt");
    confirm(cx, false);
    wait_for_head_status(cx, handle, "正在扫描").await;
    upload(cx, "文件 乙.txt");
    confirm(cx, true);
    cx.run_until_parked();
    assert_eq!(
        label(cx, ("queue-entry", 2u64).into()).as_deref(),
        Some("等待中")
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 1);

    // The first one ends; the engine goes idle and takes the second.
    send(SftpEvent::Progress(TransferProgress::new(
        TransferPhase::Completed,
    )));
    send(SftpEvent::Idle);
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("queue-entry", 1u64))
            .is_some_and(|row| row.label() == Some("已完成"))
    })
    .await;
    assert_eq!(provider.requests.lock().unwrap().len(), 2);
    assert_eq!(
        provider.requests.lock().unwrap()[1].sources(),
        [std::path::PathBuf::from("/local/tester/文件 乙.txt")]
    );

    // Its file in flight gets a row and a bar of its own.
    send(SftpEvent::Progress(
        TransferProgress::new(TransferPhase::Transferring)
            .with_bytes(512, 1024)
            .with_current(
                "/local/tester/文件 乙.txt",
                "/home/tester/文件 乙.txt",
                256,
                1024,
            ),
    ));
    wait_for_head_status(cx, handle, "50%").await;
    assert_eq!(
        label(cx, ("queue-file", 2u64).into()).as_deref(),
        Some("/local/tester/文件 乙.txt")
    );
    assert_eq!(
        label(cx, ("queue-file-status", 2u64).into()).as_deref(),
        Some("25%")
    );

    // Cleared once done, and the queue goes away with its last row.
    let clear = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("clear-finished-transfers", cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    clear(cx);
    assert_eq!(label(cx, ("queue-entry", 1u64).into()), None);
    send(SftpEvent::Progress(TransferProgress::new(
        TransferPhase::Completed,
    )));
    send(SftpEvent::Idle);
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("queue-entry", 2u64))
            .is_some_and(|row| row.label() == Some("已完成"))
    })
    .await;
    clear(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("transfer-queue", SFTP_TAB))
                .is_none_or(|queue| !queue.visible())
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_native_picker_and_external_drop_share_confirmation(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    // 「选择文件上传…」 sits in the upload button's menu, which the tests do
    // not open; dispatch what the item dispatches.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(
            Box::new(shellrs::app::ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                shellrs::app::ExplorerCommand::ChooseFiles,
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && options.directories && options.multiple);
        Some(vec!["/tmp/甲".into(), "/tmp/目录".into()])
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
        assert_eq!(window.find("upload-target").value(), Some("/home/tester"));
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .hover("file:目录", cx);
        window.render_frame(cx);
        let position = window
            .within(("remote-pane", SFTP_TAB))
            .find("file:目录")
            .bounds()
            .center();
        let paths =
            gpui_kit::ExternalPaths(vec![std::path::PathBuf::from("/tmp/Finder 文件")].into());
        window.dispatch_event(
            gpui_kit::FileDropEvent::Entered { position, paths }.to_platform_input(),
            cx,
        );
        window.dispatch_event(
            gpui_kit::FileDropEvent::Submit { position }.to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
        assert_eq!(
            window.find("upload-target").value(),
            Some("/home/tester/目录")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_internal_drag_conflict_and_close_confirmation(cx: &mut TestAppContext) {
    use shellrs::sftp::{TransferQuestion, TransferQuestionKind};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let from = window
            .within(("local-pane", SFTP_TAB))
            .find("name:文件 甲.txt")
            .bounds()
            .center();
        let to = window
            .within(("remote-pane", SFTP_TAB))
            .find("file:目录")
            .bounds()
            .center();
        window.drag(from, to, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("upload-target").value(),
            Some("/home/tester/目录")
        );
        window.click("upload-confirm", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "正在扫描").await;
    provider.events.lock().unwrap()[0]
        .send_blocking(SftpEvent::Question(TransferQuestion::new(
            900,
            TransferQuestionKind::Conflict,
            "/home/tester/目录/文件 甲.txt",
            "目标已存在",
        )))
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find("transfer-question-confirm").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("transfer-apply-all", cx);
        window.click("transfer-question-cancel", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "已停止").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("resume-transfer", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "0%").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-explorer", SFTP_TAB), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("ok").visible());
        assert!(workspace.read(cx).explorer(ExplorerId(SFTP_TAB)).is_some());
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert!(workspace.read(cx).explorer(ExplorerId(SFTP_TAB)).is_none()));
}

#[gpui_kit::test]
async fn sftp_sort_range_selection_and_dialog_focus_preserve_path_identity(
    cx: &mut TestAppContext,
) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("local-pane", SFTP_TAB))
            .click("name:文件 乙.txt", cx);
    })
    .unwrap();
    cx.run_until_parked();
    // Sorting twice (descending, then ascending by name) keeps the selection
    // by name, not by row position.
    for _ in 0..2 {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .within(("local-pane", SFTP_TAB))
                .click(("col-header", 0usize), cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .explorer(ExplorerId(SFTP_TAB))
                .unwrap()
                .read(cx)
                .local()
                .read(cx)
                .upload_sources(cx),
            vec![std::path::PathBuf::from("/local/tester/文件 乙.txt")]
        )
    });
    // ⌘-click adds a row; Shift-click selects the range from the anchor.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        modified_click(
            window,
            ("local-pane", SFTP_TAB),
            "name:目录",
            gpui_kit::Modifiers::secondary_key(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let mut selected = pane_selection(&workspace, false, cx);
        selected.sort();
        assert_eq!(selected, ["文件 乙.txt", "目录"]);
    });
    // 目录 is the anchor and sorts first; Shift-click on the last row takes
    // every row whatever the name order is.
    let last = cx.update(|cx| {
        workspace
            .read(cx)
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .read(cx)
            .local()
            .read(cx)
            .entries(cx)
            .last()
            .unwrap()
            .name
            .to_string()
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        modified_click(
            window,
            ("local-pane", SFTP_TAB),
            &format!("name:{last}"),
            gpui_kit::Modifiers::shift(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(pane_selection(&workspace, false, cx).len(), 4));
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window
                .within(("local-pane", SFTP_TAB))
                .find("name:文件 甲.txt")
                .selected(),
            Some(true)
        );
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("local-pane", SFTP_TAB))
            .click("name:文件 甲.txt", cx);
        window.press("f5", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    // F5 works again without clicking a pane after dismissing the dialog.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("f5", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("upload-confirm").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_panes_list_winscp_columns_and_open_links_to_directories(cx: &mut TestAppContext) {
    use shellrs::explorer::FileKind;
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    cx.update(|cx| {
        let explorer = workspace
            .read(cx)
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .read(cx);
        assert_eq!(
            explorer.local().read(cx).column_names(cx),
            ["名称", "大小", "类型", "修改时间"]
        );
        assert_eq!(
            explorer.remote().read(cx).column_names(cx),
            ["名称", "大小", "修改时间", "权限", "所有者"]
        );
        let rows = explorer.remote().read(cx).entries(cx);
        let names: Vec<_> = rows.iter().map(|row| row.name.to_string()).collect();
        assert_eq!(
            names,
            ["..", "目录", "链接目录", "文件 乙.txt", "文件 甲.txt"]
        );
        assert_eq!(rows[2].target, Some(FileKind::Dir));
        assert_eq!(rows[3].owner.as_deref(), Some("root"));
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .double_click("name:链接目录", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/home/tester/链接目录")
    })
    .await;
}

/// The 大小 column shows whole kilobytes, as WinSCP does, until its title's
/// menu picks another format, which then holds for both panes and is saved.
/// The menu itself is not driven; the test dispatches what its items do.
#[gpui_kit::test]
async fn sftp_size_column_shows_kilobytes_until_another_format_is_chosen(cx: &mut TestAppContext) {
    use shellrs::app::SetFileSizeFormat;
    use shellrs::explorer::FileSizeFormat;
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let size = |cx: &mut TestAppContext, name: &str| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .within(("remote-pane", SFTP_TAB))
                .find(ElementId::Name(format!("size:{name}").into()))
                .label()
                .map(str::to_string)
        })
        .unwrap()
    };
    let saved = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            workspace
                .read(cx)
                .settings()
                .read(cx)
                .settings()
                .file_size_format
        })
    };
    // 12 bytes, rounded up.
    assert_eq!(size(cx, "文件 甲.txt").as_deref(), Some("1 KB"));
    assert_eq!(saved(cx), FileSizeFormat::Kilobytes);

    for (format, shown) in [
        (FileSizeFormat::Bytes, "12 B"),
        (FileSizeFormat::Short, "12 B"),
        (FileSizeFormat::Kilobytes, "1 KB"),
    ] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.dispatch_action(Box::new(SetFileSizeFormat(format)), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(size(cx, "文件 甲.txt").as_deref(), Some(shown));
        assert_eq!(saved(cx), format);
    }
}

/// Whether a button accepts input. gpui-base does not report `disabled` for
/// buttons, but a disabled one also leaves the focus order.
fn enabled(button: &gpui_kit::test::ElementSnapshot) -> bool {
    button.focused().is_some()
}

/// Click a toolbar button of the remote pane and wait for the path it lands on.
async fn click_remote_tool(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    button: &'static str,
    expected: &str,
) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within(("remote-pane", SFTP_TAB)).click(button, cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some(expected)
    })
    .await;
}

#[gpui_kit::test]
async fn sftp_toolbar_goes_up_root_home_back_and_forward(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    use shellrs::session::BookmarkSide;
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let remote = window.within(("remote-pane", SFTP_TAB));
        assert!(!enabled(&remote.find("back")));
        assert!(!enabled(&remote.find("home")), "already home");
        assert!(enabled(&remote.find("up")));
    })
    .unwrap();
    click_remote_tool(cx, handle, "up", "/home").await;
    click_remote_tool(cx, handle, "root", "/").await;
    click_remote_tool(cx, handle, "back", "/home").await;
    click_remote_tool(cx, handle, "back", "/home/tester").await;
    click_remote_tool(cx, handle, "forward", "/home").await;
    click_remote_tool(cx, handle, "home", "/home/tester").await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let remote = window.within(("remote-pane", SFTP_TAB));
        assert!(!enabled(&remote.find("forward")), "a visit drops forward");
        assert!(enabled(&remote.find("back")));
    })
    .unwrap();
    // A failed load leaves both the path and the history alone.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/denied".into(),
                },
            )),
            cx,
        );
    })
    .unwrap();
    // In red at the window's bottom left, where the connection shows.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("权限不足")
    })
    .await;
    cx.update(|cx| {
        let explorer = workspace
            .read(cx)
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .read(cx);
        let remote = explorer.remote().read(cx);
        assert_eq!(remote.path(), "/home/tester");
        assert_eq!(remote.back_target().as_deref(), Some("/home"));
    });

    // Bookmarks belong to the session and the pane.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::AddBookmark {
                    remote: true,
                    path: None,
                },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(
            store.bookmarks(SessionId(DB_01), BookmarkSide::Remote),
            ["/home/tester"]
        );
        assert!(
            store
                .bookmarks(SessionId(DB_01), BookmarkSide::Local)
                .is_empty()
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::RemoveBookmark {
                    remote: true,
                    path: "/home/tester".into(),
                },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(
            workspace
                .read(cx)
                .store()
                .read(cx)
                .bookmarks(SessionId(DB_01), BookmarkSide::Remote)
                .is_empty()
        )
    });
}

fn remote_pane_state<T>(
    workspace: &Entity<Workspace>,
    cx: &mut TestAppContext,
    read: impl FnOnce(&shellrs::explorer::FilePane) -> T,
) -> T {
    cx.update(|cx| {
        read(
            workspace
                .read(cx)
                .explorer(ExplorerId(SFTP_TAB))
                .unwrap()
                .read(cx)
                .remote()
                .read(cx),
        )
    })
}

#[gpui_kit::test]
async fn sftp_path_label_opens_ancestors_and_the_open_directory_dialog(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    // Focus events, which track the current pane, only reach an active window.
    cx.update_window(handle.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();

    // Every directory on the way is a part of the label and opens itself.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let mut remote = window.within(("remote-pane", SFTP_TAB));
        for part in ["path:/", "path:/home", "path:/home/tester"] {
            assert!(remote.find(part).visible(), "{part}");
        }
        // The parts read as one path: `/home/tester/`, no gaps.
        assert_eq!(
            remote.find("path:/").bounds().right(),
            remote.find("path:/home").bounds().left()
        );
        remote.click("path:/home", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/home")
    })
    .await;
    assert_eq!(
        remote_pane_state(&workspace, cx, |pane| pane.back_target()),
        Some("/home/tester".into())
    );

    // Clicking the current directory opens 打开目录 on it, selected, so
    // typing replaces it; Enter opens.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .click("path:/home", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("open-directory-path").value(), Some("/home"));
        assert!(
            window.try_find("open-directory-browse").is_none(),
            "no browsing the server with a local picker"
        );
        window.input("/etc", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/etc")
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("open-directory-path").is_none());
    })
    .unwrap();

    // Double-clicking beside the path opens it too; Escape changes nothing.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .double_click("path-parts", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("open-directory-path").value(), Some("/etc"));
        window.input("/var", cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("open-directory-path").is_none());
        assert_eq!(window.find("remote-path").value(), Some("/etc"));
    })
    .unwrap();

    // WinSCP's 打开目录 key works from the list.
    #[cfg(target_os = "macos")]
    let open_directory = "cmd-o";
    #[cfg(not(target_os = "macos"))]
    let open_directory = "ctrl-o";
    press_on_row(cx, handle, "remote-pane", "目录", open_directory);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("open-directory-path").visible());
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();

    // 复制路径 from the label's menu; the pane used last is the current one.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::CopyPath { remote: true },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("/etc".into())
    );
    assert!(remote_pane_state(&workspace, cx, |pane| pane.is_current()));
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::FocusPane { remote: false },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!remote_pane_state(&workspace, cx, |pane| pane.is_current()));
    cx.update(|cx| {
        let explorer = workspace.read(cx).explorer(ExplorerId(SFTP_TAB)).unwrap();
        assert!(explorer.read(cx).local().read(cx).is_current());
    });
}

#[gpui_kit::test]
async fn sftp_bookmark_dialog_adds_orders_removes_and_opens(cx: &mut TestAppContext) {
    use shellrs::session::BookmarkSide;
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let bookmarks = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            workspace
                .read(cx)
                .store()
                .read(cx)
                .bookmarks(SessionId(DB_01), BookmarkSide::Remote)
                .to_vec()
        })
    };

    // The toolbar's bookmark button opens the dialog on the pane's directory.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .click("bookmarks", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("open-directory-path").value(),
            Some("/home/tester")
        );
        assert!(!enabled(&window.find("bookmark-remove")));
        window.click("bookmark-add", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(bookmarks(cx), ["/home/tester"]);

    // The bookmark naming the directory is the selected one; a typed
    // directory is bookmarked as the pane would open it.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("bookmark:/home/tester").selected(), Some(true));
        assert!(!enabled(&window.find("bookmark-add")), "already bookmarked");
        window.click("open-directory-path", cx);
        window.press("cmd-a", cx);
        window.input("/etc/", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("bookmark-add", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(bookmarks(cx), ["/home/tester", "/etc"]);

    // 上移 moves the selected one; clicking another picks it.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!enabled(&window.find("bookmark-down")));
        window.click("bookmark-up", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(bookmarks(cx), ["/etc", "/home/tester"]);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("bookmark:/home/tester", cx);
    })
    .unwrap();
    cx.run_until_parked();

    // Delete in the list removes it and selects the neighbour.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("open-directory-path").value(),
            Some("/home/tester")
        );
        window.press("delete", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(bookmarks(cx), ["/etc"]);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("open-directory-path").value(), Some("/etc"));
        assert_eq!(window.find("bookmark:/etc").selected(), Some(true));
        // Double-clicking a bookmark opens it.
        window.double_click("bookmark:/etc", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/etc")
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("open-directory-path").is_none());
    })
    .unwrap();

    // Local directories can be picked with the system dialog.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("local-pane", SFTP_TAB))
            .click("bookmarks", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-directory-browse", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec!["/picked".into()])
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("open-directory-path").value(), Some("/picked"));
        window.click("open-directory-confirm", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("local-path").value() == Some("/picked")
    })
    .await;
}

#[gpui_kit::test]
async fn sftp_path_label_folds_the_middle_of_a_long_path(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let deep = "/home/tester/customer-projects/desktop-client/source-tree/user-interface/\
                file-browser/path-label/implementation";
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: deep.into(),
                },
            )),
            cx,
        );
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some(deep)
    })
    .await;
    // The first frame measures the label; the next one folds to fit it.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.simulate_next_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let label = window.find("remote-path").bounds();
        let remote = window.within(("remote-pane", SFTP_TAB));
        assert!(remote.find("path:/").visible(), "the root stays");
        assert!(
            remote
                .find(ElementId::Name(format!("path:{deep}").into()))
                .visible()
        );
        assert!(
            remote.try_find("path:/home").is_none(),
            "the middle folds into …"
        );
        let parts = remote.find("path-parts").bounds();
        assert!(parts.right() <= label.right());
        let last = remote
            .find(ElementId::Name(format!("path:{deep}").into()))
            .bounds();
        assert!(last.right() <= parts.right() + px(1.), "{last:?} {parts:?}");
    })
    .unwrap();
}

/// An SFTP tab has a terminal tab's buttons: 打开 SFTP opens another tab of
/// the session, and 重新连接 connects again, from a dropped connection or a
/// live one. Its connection, and why it dropped, show in red at the window's
/// bottom left, so the list is never pushed around.
#[gpui_kit::test]
async fn sftp_tab_reconnects_from_its_tab_bar_and_reports_at_the_bottom_left(
    cx: &mut TestAppContext,
) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    let reconnect = ("reconnect-sftp", SFTP_TAB);
    let status_is = |expected: &'static str| {
        move |window: &mut gpui_kit::Window, cx: &mut App| {
            window.render_frame(cx);
            window.find("status-connection").label() == Some(expected)
        }
    };
    let table = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("status-connection").label(),
                Some("已连接 db-01")
            );
            window
                .within(("remote-pane", SFTP_TAB))
                .find("table")
                .bounds()
        })
        .unwrap();

    // Dropped: the reason goes to the window's status line.
    let events = provider.events.lock().unwrap()[0].clone();
    events
        .send_blocking(SftpEvent::Disconnected("连接已断开".into()))
        .unwrap();
    cx.wait_for(
        handle.into(),
        Duration::from_secs(2),
        status_is("未连接 db-01：连接已断开"),
    )
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let remote = window.within(("remote-pane", SFTP_TAB));
        assert_eq!(remote.find("table").bounds(), table);
        assert!(!enabled(&remote.find("refresh")));
        assert_ne!(remote.find("pane-status").label(), Some("连接已断开"));
        window.click(reconnect, cx);
    })
    .unwrap();
    cx.wait_for(
        handle.into(),
        Duration::from_secs(2),
        status_is("已连接 db-01"),
    )
    .await;
    assert_eq!(*provider.reconnects.lock().unwrap(), 1);

    // Connected, it drops the connection and makes a new one.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(reconnect, cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        *provider.reconnects.lock().unwrap() == 2
    })
    .await;
    cx.wait_for(
        handle.into(),
        Duration::from_secs(2),
        status_is("已连接 db-01"),
    )
    .await;

    // 打开 SFTP opens another tab of the same session.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("open-sftp", SFTP_TAB), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            workspace.read(cx).explorers_of(SessionId(DB_01), cx).len(),
            2
        )
    });
}

/// Click a row in one pane, then press a key with the list focused.
fn press_on_row(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    pane: &'static str,
    row: &str,
    key: &str,
) {
    let row = ElementId::Name(format!("name:{row}").into());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within((pane, SFTP_TAB)).click(row, cx);
        window.press(key, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn sftp_file_commands_confirm_validate_and_send_one_operation(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    use shellrs::explorer::NewEntryKind;
    use shellrs::sftp::{PermissionEdit, RemoteOperation};
    let provider = Arc::new(FakeSftpProvider::default());
    let local = FakeLocalDirectory::default();
    let (handle, workspace) = open_workspace_with_services(cx, provider.clone(), local.clone());
    open_test_explorer(cx, handle).await;
    let path = |name: &str| RemotePath::new(format!("/home/tester/{name}")).unwrap();
    let last_operation = || provider.operations.lock().unwrap().last().cloned();
    // A pane takes no new file command until the last one answers.
    let idle = |cx: &App| {
        let explorer = workspace
            .read(cx)
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .read(cx);
        !explorer.remote().read(cx).is_busy() && !explorer.local().read(cx).is_busy()
    };

    // 删除 asks first, naming the item; remote deletes are permanent.
    press_on_row(cx, handle, "remote-pane", "文件 甲.txt", "f8");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        last_operation().is_some() && idle(cx)
    })
    .await;
    assert_eq!(
        last_operation(),
        Some(RemoteOperation::Delete {
            paths: vec![path("文件 甲.txt")]
        })
    );

    // Local deletes go to the Trash through the injected provider.
    press_on_row(cx, handle, "local-pane", "文件 乙.txt", "delete");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        !local.calls.lock().unwrap().is_empty() && idle(cx)
    })
    .await;
    assert_eq!(
        local.calls.lock().unwrap()[0],
        r#"trash ["/local/tester/文件 乙.txt"]"#
    );

    // 重命名 checks the name against the listing before sending anything.
    press_on_row(cx, handle, "remote-pane", "文件 乙.txt", "f2");
    for (typed, error) in [
        (None, "名称未改变"),
        (Some("目录"), "已有名为「目录」的项目"),
        (Some(""), "名称不能为空"),
    ] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            if let Some(typed) = typed {
                window.click("entry-name", cx);
                window.press("cmd-a", cx);
                window.press("backspace", cx);
                window.input(typed, cx);
            }
            window.click("commit", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("form-error").label(), None);
            assert!(window.find("form-error").visible(), "{error}");
        })
        .unwrap();
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("entry-name", cx);
        window.press("cmd-a", cx);
        window.input("新名字.txt", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        matches!(last_operation(), Some(RemoteOperation::Rename { .. })) && idle(cx)
    })
    .await;
    assert_eq!(
        last_operation(),
        Some(RemoteOperation::Rename {
            from: path("文件 乙.txt"),
            to: path("新名字.txt")
        })
    );

    // 新建 › 文件夹… offers a default name. A closed dialog leaves no focus
    // for `dispatch_action`, so click into the list first.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .click("name:目录", cx);
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::New {
                    remote: true,
                    kind: NewEntryKind::Folder,
                },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("entry-name").value(), Some("新建文件夹"));
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        matches!(
            last_operation(),
            Some(RemoteOperation::CreateDirectory { .. })
        ) && idle(cx)
    })
    .await;
    assert_eq!(
        last_operation(),
        Some(RemoteOperation::CreateDirectory {
            path: path("新建文件夹")
        })
    );

    // 属性: the grid and the octal field agree, and only the changed bit goes out.
    press_on_row(cx, handle, "remote-pane", "文件 甲.txt", "f9");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("perm-octal").value(), Some("644"));
        assert_eq!(window.find(("perm", 2usize)).checked(), Some(false));
        window.click(("perm", 2usize), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("perm-octal").value(), Some("744"));
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        matches!(
            last_operation(),
            Some(RemoteOperation::SetPermissions { .. })
        ) && idle(cx)
    })
    .await;
    assert_eq!(
        last_operation(),
        Some(RemoteOperation::SetPermissions {
            paths: vec![path("文件 甲.txt")],
            edit: PermissionEdit::new(0o100, 0),
            recursive: false,
            add_x_to_dirs: false,
        })
    );
}

#[gpui_kit::test]
async fn sftp_downloads_the_selection_with_f5_into_a_chosen_folder(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    // F5 in the remote list downloads the selection into the local directory.
    press_on_row(cx, handle, "remote-pane", "文件 甲.txt", "f5");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("download-confirm").visible());
        assert_eq!(
            window.find("download-target").value(),
            Some("/local/tester")
        );
        window.click("download-browse", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec!["/picked".into()])
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("download-target").value(), Some("/picked"));
        window.click("download-confirm", cx);
    })
    .unwrap();
    wait_for_head_status(cx, handle, "0%").await;
    {
        let downloads = provider.downloads.lock().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(
            downloads[0].sources(),
            [RemotePath::new("/home/tester/文件 甲.txt").unwrap()]
        );
        assert_eq!(downloads[0].destination(), std::path::Path::new("/picked"));
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("cancel-transfer").visible());
        // The running download's row, with the arrow pointing down.
        assert_eq!(window.find(("queue-entry", 1u64)).label(), Some("0%"));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_dragging_remote_rows_to_a_local_directory_asks_to_download(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    // By its name: the rest of the row draws a selection rectangle.
    let drag_name = |cx: &mut TestAppContext, name: &str| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let from = window
                .within(("remote-pane", SFTP_TAB))
                .find(ElementId::Name(format!("name:{name}").into()))
                .bounds()
                .center();
            let to = window
                .within(("local-pane", SFTP_TAB))
                .find("file:目录")
                .bounds()
                .center();
            window.drag(from, to, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    let summary = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("download-target").value(),
                Some("/local/tester/目录")
            );
            window.find("transfer-summary").label().unwrap().to_string()
        })
        .unwrap()
    };

    // A name that is not selected is selected alone and goes alone.
    cx.update_window(handle.into(), |_, window, cx| {
        window
            .within(("remote-pane", SFTP_TAB))
            .click("name:文件 甲.txt", cx)
    })
    .unwrap();
    cx.run_until_parked();
    drag_name(cx, "文件 乙.txt");
    cx.update(|cx| assert_eq!(pane_selection(&workspace, true, cx), ["文件 乙.txt"]));
    assert!(summary(cx).ends_with(" 下载 1 个文件"));
    cx.update_window(handle.into(), |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();

    // A selected name takes the whole selection with it.
    cx.update_window(handle.into(), |_, window, cx| {
        modified_click(
            window,
            ("remote-pane", SFTP_TAB),
            "name:文件 甲.txt",
            gpui_kit::Modifiers::secondary_key(),
            cx,
        )
    })
    .unwrap();
    cx.run_until_parked();
    drag_name(cx, "文件 乙.txt");
    cx.update(|cx| {
        assert_eq!(
            pane_selection(&workspace, true, cx),
            ["文件 乙.txt", "文件 甲.txt"]
        )
    });
    assert!(summary(cx).ends_with(" 下载 2 个文件"));
}

/// A file drag shows what it will do only over the pane that takes it; over
/// its own list the pointer says no instead, as in WinSCP.
#[gpui_kit::test]
async fn sftp_a_file_drag_shows_its_preview_only_over_the_other_pane(cx: &mut TestAppContext) {
    use gpui_kit::{MouseDownEvent, MouseUpEvent};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let move_to = |window: &mut gpui_kit::Window, position, cx: &mut App| {
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button: Some(MouseButton::Left),
                modifiers: gpui_kit::Modifiers::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        window
            .try_find("file-drag-preview")
            .and_then(|preview| preview.label().map(str::to_string))
    };
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let local = window.within(("local-pane", SFTP_TAB));
        let from = local.find("name:文件 甲.txt").bounds().center();
        let still_local = local.find("size:目录").bounds().center();
        let remote = window
            .within(("remote-pane", SFTP_TAB))
            .find("size:文件 乙.txt")
            .bounds()
            .center();
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                modifiers: gpui_kit::Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        // Dragging, but still over its own list: no preview.
        assert_eq!(move_to(window, still_local, cx), None);
        // Over the other pane it says what a drop does.
        assert_eq!(
            move_to(window, remote, cx).as_deref(),
            Some("上传 1 个项目")
        );
        // And back.
        assert_eq!(move_to(window, still_local, cx), None);
        // Let go where nothing takes it.
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position: still_local,
                modifiers: gpui_kit::Modifiers::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("upload-confirm").is_none());
    })
    .unwrap();
}

/// Dragging from anywhere but a name draws a selection rectangle, as in
/// WinSCP without full row select: the rows it crosses are selected, and
/// nothing is dragged to the other pane.
#[gpui_kit::test]
async fn sftp_dragging_outside_the_names_selects_the_rows_crossed(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    // Remote rows: .., 目录, 链接目录, 文件 乙.txt, 文件 甲.txt.
    let sweep = |cx: &mut TestAppContext, from: &str, to: &str| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let pane = window.within(("remote-pane", SFTP_TAB));
            let from = pane
                .find(ElementId::Name(format!("size:{from}").into()))
                .bounds()
                .center();
            let to = pane
                .find(ElementId::Name(format!("size:{to}").into()))
                .bounds()
                .center();
            window.drag(from, to, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    sweep(cx, "文件 甲.txt", "链接目录");
    cx.update(|cx| {
        assert_eq!(
            pane_selection(&workspace, true, cx),
            ["链接目录", "文件 乙.txt", "文件 甲.txt"]
        );
        // GPUI tells every pane about the drag; the other one ignores it.
        assert!(pane_selection(&workspace, false, cx).is_empty());
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // No transfer was started, and the rectangle is gone with the button.
        assert!(window.try_find("download-confirm").is_none());
        assert!(
            window
                .within(("remote-pane", SFTP_TAB))
                .try_find("selection-rectangle")
                .is_none()
        );
    })
    .unwrap();

    // A new rectangle replaces the selection. The name cell right of the
    // name is empty space too.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let pane = window.within(("remote-pane", SFTP_TAB));
        let blank = |name: &str| {
            let cell = pane
                .find(ElementId::Name(format!("name-cell:{name}").into()))
                .bounds();
            gpui_kit::point(cell.right() - gpui_kit::px(8.), cell.center().y)
        };
        let (from, to) = (blank("目录"), blank("链接目录"));
        window.drag(from, to, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(pane_selection(&workspace, true, cx), ["目录", "链接目录"]));
}

/// As in WinSCP without full row select, only the name cell is the item: a
/// click on the rest of a row, or below the rows, is a click on empty space
/// and clears the selection. A ⌘ click there, or a click on a column title,
/// leaves it alone.
#[gpui_kit::test]
async fn sftp_clicking_outside_the_names_clears_the_selection(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let pane = ("remote-pane", SFTP_TAB);
    let select_two = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within(pane).click("name:文件 甲.txt", cx);
            modified_click(
                window,
                pane,
                "name:文件 乙.txt",
                gpui_kit::Modifiers::secondary_key(),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(
                pane_selection(&workspace, true, cx),
                ["文件 乙.txt", "文件 甲.txt"]
            )
        });
    };
    let selection = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        cx.update(|cx| pane_selection(&workspace, true, cx))
    };

    // The size of a selected row.
    select_two(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // The selection shows across the whole name cell: the column's
        // 240 px, and the row's height but for the row's 1 px bottom border.
        let name = window.within(pane).find("name-cell:文件 甲.txt").bounds();
        let row = window.within(pane).find("file:文件 甲.txt").bounds();
        assert_eq!(name.top(), row.top());
        assert_eq!(name.size.height, row.size.height - gpui_kit::px(1.));
        assert_eq!(name.size.width, gpui_kit::px(240.));
        window.within(pane).click("size:文件 甲.txt", cx);
    })
    .unwrap();
    assert!(selection(cx).is_empty());

    // The name cell is the item, right of the name too: a plain click
    // selects that row alone.
    select_two(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let cell = window.within(pane).find("name-cell:文件 甲.txt").bounds();
        let blank = gpui_kit::point(cell.right() - gpui_kit::px(8.), cell.center().y);
        click_at(window, blank, gpui_kit::Modifiers::default(), cx);
    })
    .unwrap();
    assert_eq!(selection(cx), ["文件 甲.txt"]);

    // A file drag let go over the name it began on is not a click: the
    // selection it carried stays.
    select_two(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let name = window.within(pane).find("name:文件 乙.txt").bounds();
        let to = gpui_kit::point(name.right() + gpui_kit::px(20.), name.center().y);
        window.drag(name.center(), to, cx);
    })
    .unwrap();
    assert_eq!(selection(cx), ["文件 乙.txt", "文件 甲.txt"]);

    // Below the last row.
    select_two(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Halfway between the last row and the status bar under the list,
        // clear of the table's scrollbars.
        let last = window.within(pane).find("file:文件 甲.txt").bounds();
        let status = window.within(pane).find("pane-status").bounds();
        let below = gpui_kit::point(
            last.center().x,
            last.bottom() + (status.top() - last.bottom()) / 2.,
        );
        click_at(window, below, gpui_kit::Modifiers::default(), cx);
    })
    .unwrap();
    assert!(selection(cx).is_empty());

    // ⌘ held, or a column title: the selection stays.
    select_two(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        modified_click(
            window,
            pane,
            "size:链接目录",
            gpui_kit::Modifiers::secondary_key(),
            cx,
        );
        window.render_frame(cx);
        window.within(pane).click("column:size", cx);
    })
    .unwrap();
    assert_eq!(selection(cx), ["文件 乙.txt", "文件 甲.txt"]);
}

#[gpui_kit::test]
async fn the_sftp_tab_shows_the_host_mark_like_its_terminal_tabs(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let mark = ("explorer-tab-os", SFTP_TAB);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(mark).label(), Some("未探测到系统"));
    })
    .unwrap();
    // A terminal of the same session finds the system; the SFTP tab follows.
    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        store.update(cx, |store, cx| {
            store.set_host_os(SessionId(DB_01), Some(HostOs::Ubuntu), cx)
        });
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(mark).label(), Some("Ubuntu"));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn the_sftp_tab_can_be_renamed_and_follow_the_session_again(cx: &mut TestAppContext) {
    use shellrs::app::RenameExplorer;
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let tab = ("explorer-tab", SFTP_TAB);
    let default = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        format!("{} · SFTP", store.session(SessionId(DB_01)).unwrap().name)
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some(default.as_str()));
        window.dispatch_action(Box::new(RenameExplorer(ExplorerId(SFTP_TAB))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("tab-name").value(), Some(default.as_str()));
        window.click("tab-name", cx);
        window.press("cmd-a", cx);
        window.input("生产库文件", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some("生产库文件"));
    })
    .unwrap();

    // Clearing the field returns the tab to its default title.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // A closed dialog leaves nothing focused.
        window.click("session-search", cx);
        window.dispatch_action(Box::new(RenameExplorer(ExplorerId(SFTP_TAB))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("tab-name").value(), Some("生产库文件"));
        window.click("tab-name", cx);
        window.press("cmd-a", cx);
        window.press("backspace", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some(default.as_str()));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_clicking_empty_list_space_makes_that_pane_current_at_once(cx: &mut TestAppContext) {
    use gpui_kit::{MouseDownEvent, MouseUpEvent};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    // Focus events only reach an active window.
    cx.update_window(handle.into(), |_, window, _| window.activate_window())
        .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("remote-path").selected(), Some(true));
        assert_eq!(window.find("local-path").selected(), Some(false));
        // Below the last row of the local list: only the table takes focus.
        let table = window
            .within(("local-pane", SFTP_TAB))
            .find("table")
            .bounds();
        let position = point(table.center().x, table.bottom() - px(8.));
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    // No render of our own: only the frames the click asked for.
    cx.update_window(handle.into(), |_, window, _| {
        assert_eq!(window.find("local-path").selected(), Some(true));
        assert_eq!(window.find("remote-path").selected(), Some(false));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn opening_sftp_again_opens_another_tab_of_its_own(cx: &mut TestAppContext) {
    use shellrs::app::{DisconnectSession, ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let second = ExplorerId(SFTP_TAB + 1);
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(SessionId(DB_01))), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("remote-pane", second.0))
            .is_some_and(|pane| pane.visible())
            && window.find("remote-path").value() == Some("/home/tester")
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("explorer-tab", SFTP_TAB)).visible());
        assert!(window.find(("explorer-tab", second.0)).visible());
        // Each tab browses on its own.
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                second,
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/etc".into(),
                },
            )),
            cx,
        );
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/etc")
    })
    .await;
    let paths = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            workspace
                .read(cx)
                .explorers_of(SessionId(DB_01), cx)
                .iter()
                .map(|panel| panel.read(cx).remote().read(cx).path())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(paths(cx), ["/home/tester", "/etc"]);

    // Closing one keeps the other, and the session stays connected.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-explorer", second.0), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(paths(cx), ["/home/tester"]);
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(
            store.session(SessionId(DB_01)).unwrap().state,
            ConnectionState::Connected
        );
    });

    // Disconnecting the session disconnects every SFTP tab of it.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(SessionId(DB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(DisconnectSession(SessionId(DB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let explorers = workspace.read(cx).explorers_of(SessionId(DB_01), cx);
        assert_eq!(explorers.len(), 2);
        assert!(
            explorers
                .iter()
                .all(|panel| panel.read(cx).connection_state() == ConnectionState::Disconnected)
        );
    });
}

#[gpui_kit::test]
async fn sftp_connecting_shows_under_the_list_without_moving_it(cx: &mut TestAppContext) {
    let (release, hold) = mpsc::channel();
    let provider = Arc::new(FakeSftpProvider {
        hold_connection: Arc::new(Mutex::new(Some(hold))),
        ..Default::default()
    });
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(OpenExplorer(SessionId(DB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let table = cx
        .update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let remote = window.within(("remote-pane", SFTP_TAB));
            assert_eq!(remote.find("pane-status").label(), Some("正在连接 SFTP…"));
            remote.find("table").bounds()
        })
        .unwrap();
    release.send(()).unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/home/tester")
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let remote = window.within(("remote-pane", SFTP_TAB));
        assert_eq!(remote.find("table").bounds(), table);
        assert_ne!(remote.find("pane-status").label(), Some("正在连接 SFTP…"));
    })
    .unwrap();
}

#[gpui_kit::test]
async fn sftp_reading_a_directory_never_moves_the_list(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let list = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .within(("remote-pane", SFTP_TAB))
                .find("file:目录")
                .bounds()
        })
        .unwrap()
    };
    let before = list(cx);
    // `/slow` never answers, so the load stays in flight.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/slow".into(),
                },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(list(cx), before);
    assert!(!remote_pane_state(&workspace, cx, |pane| pane.is_loading_slowly()));
    // A slow load says so in the status line under the list.
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert!(remote_pane_state(&workspace, cx, |pane| pane.is_loading_slowly()));
    assert_eq!(list(cx), before);
}

#[gpui_kit::test]
async fn sftp_discards_stale_directory_replies(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    for path in ["/slow", "/fresh"] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.dispatch_action(
                Box::new(ExplorerAction::new(
                    ExplorerId(SFTP_TAB),
                    ExplorerCommand::Navigate {
                        remote: true,
                        path: path.into(),
                    },
                )),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
    }
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .explorer(ExplorerId(SFTP_TAB))
            .unwrap()
            .read(cx)
            .remote()
            .read(cx)
            .path()
            == "/fresh"
    })
    .await;
    provider.events.lock().unwrap()[0]
        .send_blocking(SftpEvent::Listed {
            request_id: 2,
            result: Ok(fake_listing("/slow")),
        })
        .unwrap();
    // Follow with an ordinary command and wait for its reply to ensure both events were consumed.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Refresh { remote: true },
            )),
            cx,
        );
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("remote-path").value() == Some("/fresh")
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            workspace
                .read(cx)
                .explorer(ExplorerId(SFTP_TAB))
                .unwrap()
                .read(cx)
                .remote()
                .read(cx)
                .path(),
            "/fresh"
        )
    });
}

#[gpui_kit::test]
async fn sftp_controls_fit_small_window_in_light_dark_and_zoom(cx: &mut TestAppContext) {
    use gpui_kit::component::{Theme, ThemeMode};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        for zoom in [12., 20.] {
            cx.update_window(handle.into(), |_, window, cx| {
                Theme::change(mode, Some(window), cx);
                Theme::global_mut(cx).font_size = px(zoom);
                window.resize(size(px(960.), px(600.)));
                window.render_frame(cx);
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                for (pane, transfer) in [("local-pane", "upload"), ("remote-pane", "download")] {
                    let bounds = window.find((pane, SFTP_TAB)).bounds();
                    let scope = window.within((pane, SFTP_TAB));
                    for id in [
                        "path-select",
                        "bookmarks",
                        "up",
                        "root",
                        "home",
                        "refresh",
                        "back",
                        "forward",
                        transfer,
                        "delete",
                        "rename",
                        "properties",
                        "new",
                    ] {
                        let button = scope.find(id).bounds();
                        assert!(
                            button.left() >= bounds.left()
                                && button.right() <= bounds.right()
                                && button.top() >= bounds.top(),
                            "{pane} {id} at zoom {zoom}"
                        );
                    }
                }
                assert!(window.find("remote-path").visible());
            })
            .unwrap();
        }
    }
}
