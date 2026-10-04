//! Connecting a host: login questions, its connection state, the tabs it
//! opens, and the system and latency found on the way.

use crate::support::*;

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
            PromptBehavior::Authentication => ConnectionPromptKind::authentication(
                "SSH 登录",
                "请输入密码",
                vec![ConnectionPromptField::new("密码", false)],
            ),
            PromptBehavior::UnknownHost => ConnectionPromptKind::unknown_host(
                "example.test",
                22,
                "ssh-ed25519",
                "SHA256:test-fingerprint",
            ),
        };
        events.send_blocking(TerminalTransportEvent::Prompt(ConnectionPrompt::new(
            request_id, kind,
        )))?;
        loop {
            match commands.recv()? {
                TerminalTransportCommand::PromptReply {
                    request_id: reply_id,
                    reply,
                } if reply_id == request_id => match reply {
                    ConnectionPromptReply::TrustAndSave
                        if matches!(self.behavior, PromptBehavior::UnknownHost) =>
                    {
                        self.accepted.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    ConnectionPromptReply::Answers(answers)
                        if matches!(self.behavior, PromptBehavior::Authentication)
                            && answers.len() == 1 =>
                    {
                        self.accepted.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    ConnectionPromptReply::Cancel => {
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

#[gpui_kit::test]
async fn authentication_prompt_is_masked_and_drives_connected_state(cx: &mut TestAppContext) {
    let (store, id) = one_host_store(AuthKind::Password);
    let factory = Arc::new(PromptTerminalFactory::new(PromptBehavior::Authentication));
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
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
            .host(id)
            .unwrap()
            .state
            .is_connected()
    }));
}

#[gpui_kit::test]
async fn canceling_unknown_host_prompt_keeps_host_disconnected(cx: &mut TestAppContext) {
    let (store, id) = one_host_store(AuthKind::Password);
    let factory = Arc::new(PromptTerminalFactory::new(PromptBehavior::UnknownHost));
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
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
    // The window says so at once; the transport hears of the cancel on its
    // own thread, a moment later.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("未连接 prompt-host")
            && factory.canceled.load(Ordering::SeqCst) == 1
    })
    .await;

    assert_eq!(factory.accepted.load(Ordering::SeqCst), 0);
    assert_eq!(factory.canceled.load(Ordering::SeqCst), 1);
    assert!(!workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .host(id)
            .unwrap()
            .state
            .is_connected()
    }));

    // The failed terminal tab remains visible, but connecting the host
    // again must start a fresh transport instead of only activating that tab.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
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
        window.dispatch_action(Box::new(ConnectHost(id)), cx);
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
async fn opening_sftp_updates_connection_state_without_terminal(cx: &mut TestAppContext) {
    let (store, id) = one_host_store(AuthKind::Password);
    let (handle, workspace) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(OpenExplorer(id)), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace.read(cx).store().read(cx).host(id).unwrap().state == ConnectionState::Connected
    })
    .await;

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.explorers_of(id, cx).len(), 1);
        assert_eq!(
            workspace.store().read(cx).host(id).unwrap().state,
            shellrs::host::ConnectionState::Connected
        );
    });
}

#[gpui_kit::test]
async fn double_click_on_host_opens_terminal_and_updates_status(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Hosts seeded as connected already have terminal tabs.
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());

        window
            .within("host-tree")
            .double_click(("host-row", DB_01), cx);
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
        assert!(workspace.terminal(HostId(DB_01), cx).is_some());
        let store = workspace.store().read(cx);
        assert!(store.host(HostId(DB_01)).unwrap().state.is_connected());
    });
}

#[gpui_kit::test]
async fn connected_host_opens_an_independent_terminal_each_time(cx: &mut TestAppContext) {
    let (store, id) = one_host_store(AuthKind::Password);
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
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
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
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
        window.dispatch_action(Box::new(ConnectHost(id)), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 3
            && workspace
                .read(cx)
                .store()
                .read(cx)
                .host(id)
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
                .host(id)
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
                .host(id)
                .unwrap()
                .state
                .is_connected()
        );
    });
}

#[gpui_kit::test]
async fn disconnecting_one_tab_leaves_the_other_tabs_of_its_host(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let second = RemoteTerminalId(FIRST_NEW_TERMINAL);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx);
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
            store.host(HostId(WEB_01)).unwrap().state,
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
async fn connecting_marks_the_host_with_the_host_operating_system(cx: &mut TestAppContext) {
    let (store, id) = one_host_store(AuthKind::Password);
    let factory = Arc::new(FakeTerminalFactory::reports_os(HostOs::Fedora));
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("host-os", id.0)).label(), Some("未探测到系统"));
        window
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
    })
    .unwrap();
    // The engine batches transport events on a 16ms timer, so the mark
    // changes a frame or two after the tab opens.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find(("host-os", id.0)).label() == Some("Fedora")
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.host(id).unwrap().os, Some(HostOs::Fedora));
    });
}

#[gpui_kit::test]
async fn the_tab_bar_shows_the_connection_latency_while_it_runs(cx: &mut TestAppContext) {
    let (store, id) = one_host_store(AuthKind::Password);
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
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
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

/// The first terminal and the first host of an empty store, which a link
/// opens.
const LINK_TERMINAL: u64 = 1;
const LINK_HOST: HostId = HostId(1);

fn open_link(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    url: &str,
    tab: Option<&str>,
) {
    let link = shellrs::cli::OpenLink {
        url: url.into(),
        tab: tab.map(str::to_string),
    };
    cx.update_window(handle.into(), |_, window, cx| {
        workspace.update(cx, |workspace, cx| workspace.open_link(link, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn a_link_opens_a_terminal_to_a_host_that_goes_with_its_last_tab(cx: &mut TestAppContext) {
    use shellrs::app::{CloseExplorer, CloseTerminal};
    use shellrs::host::BookmarkSide;

    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("shellrs.db");
    let keychain = Arc::new(InMemorySecretStore::default());
    let store = HostStore::load(HostDatabase::open(&path).unwrap())
        .unwrap()
        .with_secrets(keychain.clone());
    let remote = Arc::new(RecordingRemoteProvider::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store,
        remote.clone(),
        Arc::new(FakeConnectionTester::default()),
    );

    open_link(
        cx,
        handle,
        &workspace,
        "ssh://token:p%40ss@10.0.0.9:2222",
        Some("db-prod"),
    );
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 db-prod")
    })
    .await;

    let login = remote.logins().pop().expect("connected");
    assert_eq!(
        (login.host.as_str(), login.port, login.user.as_str()),
        ("10.0.0.9", 2222, "token")
    );
    assert_eq!(login.method, LoginMethod::Password);
    assert_eq!(login.route, LoginRoute::Direct);
    assert_eq!(login.password, SecretRef::temporary(LINK_HOST.0));
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("terminal-tab", LINK_TERMINAL)).label(),
            Some("db-prod")
        );
        // Not saved, and across the window: the hosts made way.
        assert!(window.try_find(("host-row", LINK_HOST.0)).is_none());
        assert_eq!(window.find("show-hosts").checked(), Some(false));
        assert!(window.try_find("host-search").is_none());
        // The bastion host gets the terminal alone: of the tools, only the
        // snippets, which type into it.
        assert!(window.try_find("tool-snippets").is_some());
        for tool in ["tool-history", "tool-docker", "tool-monitor"] {
            assert!(window.try_find(tool).is_none(), "{tool}");
        }
    })
    .unwrap();
    assert!(login.shell_only);
    let store = workspace.read_with(cx, |workspace, _| workspace.store().clone());
    store.read_with(cx, |store, _| {
        assert!(store.is_temporary(LINK_HOST));
        assert!(store.hosts().is_empty());
        assert!(store.recent_hosts().next().is_none());
        // The link's password is in memory, not in the keychain.
        assert_eq!(
            store
                .secrets()
                .get(&SecretRef::temporary(LINK_HOST.0))
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("p@ss")
        );
    });
    assert!(keychain.is_empty());

    // There is nothing to edit.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(EditHost(LINK_HOST)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("host-name").is_none());
    })
    .unwrap();

    // Its SFTP tab works, bookmarks and all, without writing to disk.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(LINK_HOST)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    store.update(cx, |store, cx| {
        assert!(store.add_bookmark(LINK_HOST, BookmarkSide::Remote, "/var/log", cx));
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.notifications(cx).is_empty());
    })
    .unwrap();

    // The host stays while a tab of it does.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(LINK_TERMINAL))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(store.read_with(cx, |store, _| store.host(LINK_HOST).is_some()));
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(CloseExplorer(ExplorerId(SFTP_TAB))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        assert!(store.host(LINK_HOST).is_none());
        assert!(
            store
                .secrets()
                .get(&SecretRef::temporary(LINK_HOST.0))
                .unwrap()
                .is_none()
        );
    });

    let saved = HostStore::load(HostDatabase::open(&path).unwrap()).unwrap();
    assert!(saved.hosts().is_empty());
    assert!(saved.recent_hosts().next().is_none());
    assert!(saved.bookmarks(LINK_HOST, BookmarkSide::Remote).is_empty());
}

#[gpui_kit::test]
async fn an_sftp_link_opens_an_sftp_tab_across_the_window(cx: &mut TestAppContext) {
    use shellrs::app::CloseExplorer;

    let remote = Arc::new(RecordingRemoteProvider::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        HostStore::empty(),
        remote.clone(),
        Arc::new(FakeConnectionTester::default()),
    );
    // How JumpServer opens WinSCP: the link alone.
    open_link(
        cx,
        handle,
        &workspace,
        "sftp://token:p%40ss@10.0.0.9:2222",
        None,
    );
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 10.0.0.9")
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("explorer-tab", SFTP_TAB)).label(),
            Some("10.0.0.9 · SFTP")
        );
        // An SFTP tab alone, across the window.
        assert!(window.try_find(("terminal", LINK_TERMINAL)).is_none());
        assert_eq!(window.find("show-hosts").checked(), Some(false));
        assert!(window.try_find(("host-row", LINK_HOST.0)).is_none());
    })
    .unwrap();
    assert!(remote.logins().is_empty());
    let store = workspace.read_with(cx, |workspace, _| workspace.store().clone());
    store.read_with(cx, |store, _| {
        assert!(store.is_external(LINK_HOST));
        let login = store.login(LINK_HOST).unwrap();
        assert_eq!(
            (login.host.as_str(), login.port, login.user.as_str()),
            ("10.0.0.9", 2222, "token")
        );
        assert_eq!(login.password, SecretRef::temporary(LINK_HOST.0));
    });

    // It goes with its tab.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(CloseExplorer(ExplorerId(SFTP_TAB))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| assert!(store.host(LINK_HOST).is_none()));
}

/// The title bar's 临时连接: a host's login without its group, route or
/// notes, connected to with every feature and saved nowhere.
#[gpui_kit::test]
async fn a_temporary_connection_has_every_feature_and_saves_nothing(cx: &mut TestAppContext) {
    use shellrs::app::CloseTerminal;

    let data = tempfile::tempdir().unwrap();
    let path = data.path().join("shellrs.db");
    let keychain = Arc::new(InMemorySecretStore::default());
    let store = HostStore::load(HostDatabase::open(&path).unwrap())
        .unwrap()
        .with_secrets(keychain.clone());
    let remote = Arc::new(RecordingRemoteProvider::default());
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, workspace) =
        open_workspace_with_credentials(cx, store, remote.clone(), tester.clone());

    in_frame(cx, handle, |window, cx| {
        window.click("temporary-connection", cx)
    });
    in_frame(cx, handle, |window, cx| {
        // Only what a login needs.
        assert!(window.try_find("host-route").is_none());
        assert!(window.try_find("host-notes").is_none());
        assert!(window.try_find("test-connection").is_some());
        assert_eq!(
            window.find("temporary-note").label(),
            Some("仅当前使用的临时会话，不会保存到主机列表。")
        );
        // The name may be left out.
        window.click("host-address", cx);
        window.input("10.0.0.9", cx);
        window.click("host-port", cx);
        window.press("cmd-a", cx);
        window.input("2222", cx);
        window.click("host-user", cx);
        window.press("cmd-a", cx);
        window.input("deploy", cx);
        window.click("host-password", cx);
        window.input("p@ss", cx);
        window.click("test-connection", cx);
    });
    // 测试连接 tries it as it would connect: directly.
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;
    assert_eq!(tester.routes(), [(LoginRoute::Direct, None)]);
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 10.0.0.9")
    })
    .await;

    let login = remote.logins().pop().expect("connected");
    assert_eq!(
        (login.host.as_str(), login.port, login.user.as_str()),
        ("10.0.0.9", 2222, "deploy")
    );
    assert_eq!(login.method, LoginMethod::Password);
    assert_eq!(login.route, LoginRoute::Direct);
    assert_eq!(login.password, SecretRef::temporary(LINK_HOST.0));
    // Every channel it likes, unlike a bastion host's link.
    assert!(!login.shell_only);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("terminal-tab", LINK_TERMINAL)).label(),
            Some("10.0.0.9")
        );
        assert!(window.try_find(("host-row", LINK_HOST.0)).is_none());
        // The hosts stay where they were.
        assert_eq!(window.find("show-hosts").checked(), Some(true));
        for tool in [
            "tool-snippets",
            "tool-history",
            "tool-docker",
            "tool-monitor",
        ] {
            assert!(window.try_find(tool).is_some(), "{tool}");
        }
    })
    .unwrap();
    let store = workspace.read_with(cx, |workspace, _| workspace.store().clone());
    store.read_with(cx, |store, _| {
        assert!(store.is_temporary(LINK_HOST));
        assert!(!store.is_external(LINK_HOST));
        assert!(store.hosts().is_empty());
        assert!(store.recent_hosts().next().is_none());
        assert_eq!(
            store
                .secrets()
                .get(&SecretRef::temporary(LINK_HOST.0))
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("p@ss")
        );
    });
    assert!(keychain.is_empty());

    // It goes with its tab, password and all.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(CloseTerminal(RemoteTerminalId(LINK_TERMINAL))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        assert!(store.host(LINK_HOST).is_none());
        assert!(
            store
                .secrets()
                .get(&SecretRef::temporary(LINK_HOST.0))
                .unwrap()
                .is_none()
        );
    });
    let saved = HostStore::load(HostDatabase::open(&path).unwrap()).unwrap();
    assert!(saved.hosts().is_empty());
}

#[gpui_kit::test]
async fn a_link_that_cannot_be_read_says_why_and_opens_nothing(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        HostStore::empty(),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    open_link(cx, handle, &workspace, "ssh://10.0.0.9:2222", None);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(window.try_find(("terminal", LINK_TERMINAL)).is_none());
        // Nothing opened, so the hosts stay.
        assert_eq!(window.find("show-hosts").checked(), Some(true));
    })
    .unwrap();
    workspace.read_with(cx, |workspace, cx| {
        assert!(workspace.store().read(cx).host(LINK_HOST).is_none());
    });
}
