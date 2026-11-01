//! Compose routing, fixed retry targets and key synchronization, using injected transports.

use std::collections::HashMap;

use crate::support::*;
use shellrs::app::{
    ComposeScope, PasteTerminal, RetryCompose, SendCompose, SendComposeSnippet, SetComposeScope,
    SetComposeTarget,
};
use shellrs::host::{SnippetDraft, SnippetId};

struct PerHostTerminals(HashMap<String, Arc<FakeTerminalFactory>>);

impl RemoteTerminalTransportProvider for PerHostTerminals {
    fn factory_for(&self, login: &HostLogin) -> SharedTerminalTransportFactory {
        self.0[&login.host].clone()
    }
}

struct Fixture {
    handle: WindowHandle<Root>,
    workspace: Entity<Workspace>,
    web: Arc<FakeTerminalFactory>,
    staging: Arc<FakeTerminalFactory>,
    local: Arc<FakeTerminalFactory>,
}

async fn open(cx: &mut TestAppContext) -> Fixture {
    init_app(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    // Only web uses application cursor keys and bracketed paste.
    let web = Arc::new(FakeTerminalFactory::printing(
        "\x1b[?1h\x1b[?2004hready-web",
    ));
    let staging = Arc::new(FakeTerminalFactory::printing("ready-staging"));
    // The local shell runs a full-screen program, which takes no command.
    let local = Arc::new(FakeTerminalFactory::printing("\x1b[?1049hready-local"));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1440.), px(900.)), |window, cx| {
        let mut seed = HostStore::seed();
        // SnippetId(1), run on a click.
        seed.insert_snippet_unnotified(
            SnippetDraft::new("磁盘", "df -h", None).with_run_on_click(true),
        );
        let remote = Arc::new(PerHostTerminals(HashMap::from([
            (
                seed.host(HostId(WEB_01)).unwrap().address.to_string(),
                web.clone(),
            ),
            (
                seed.host(HostId(STAGING_API)).unwrap().address.to_string(),
                staging.clone(),
            ),
        ])));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                cx.new(|_| seed),
                cx.new(|_| settings_store()),
                remote,
                fake_telnet_terminals(),
                fake_serial_terminals(),
                Arc::new(FixedLocalTerminalTransportProvider::new(local.clone())),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                Arc::new(FakeConnectionTester::default()),
                Arc::new(FakeForwardProvider::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    let workspace = workspace.unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL]
            .into_iter()
            .all(|id| {
                let workspace = workspace.read(cx);
                let terminal = workspace
                    .remote_terminal(RemoteTerminalId(id))
                    .unwrap()
                    .read(cx);
                let view = terminal.terminal().read(cx);
                view.lifecycle(cx).accepts_input() && view.screen_text(cx).contains("ready-")
            })
    })
    .await;
    Fixture {
        handle,
        workspace,
        web,
        staging,
        local,
    }
}

fn show(cx: &mut TestAppContext, handle: WindowHandle<Root>, scope: ComposeScope) {
    in_frame(cx, handle, |window, cx| {
        window.click("toggle-compose-pane", cx)
    });
    set_scope(cx, handle, scope);
}
fn set_scope(cx: &mut TestAppContext, handle: WindowHandle<Root>, scope: ComposeScope) {
    in_frame(cx, handle, |window, cx| {
        window.click("compose-pane", cx);
        window.dispatch_action(Box::new(SetComposeScope::new(scope)), cx);
    });
}
fn target(cx: &mut TestAppContext, handle: WindowHandle<Root>, tab: CenterTab, selected: bool) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SetComposeTarget::new(tab, selected)), cx);
    });
}
fn draft(cx: &mut TestAppContext, handle: WindowHandle<Root>, text: &str) {
    in_frame(cx, handle, |window, cx| {
        window.click("compose-pane", cx);
        window.input(text, cx);
    });
}
fn submit(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SendCompose), cx)
    });
}
async fn open_local(cx: &mut TestAppContext, fixture: &Fixture) {
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    cx.wait_for(fixture.handle.into(), Duration::from_secs(2), |_, cx| {
        fixture
            .workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .is_some_and(|panel| {
                panel
                    .read(cx)
                    .terminal()
                    .read(cx)
                    .lifecycle(cx)
                    .accepts_input()
            })
    })
    .await;
}
fn layout(fixture: &Fixture, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        format!(
            "{:?}",
            fixture
                .workspace
                .read(cx)
                .dock_area()
                .read(cx)
                .layout(gpui_kit::component::dock::DockPlacement::Center)
        )
    })
}

#[gpui_kit::test]
async fn selection_preserves_layout_focus_and_accepts_zero_or_one(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let h = fixture.handle;
    in_frame(cx, h, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    show(cx, h, ComposeScope::Current);
    let before = layout(&fixture, cx);
    set_scope(cx, h, ComposeScope::Selected);
    in_frame(cx, h, |window, _| {
        assert_eq!(
            window
                .find(("compose-remote", INITIAL_WEB_TERMINAL))
                .checked(),
            Some(false)
        );
        assert_eq!(
            window
                .find(("compose-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(false)
        );
    });
    // Only the non-current, background tab receives this command.
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)),
        false,
    );
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        true,
    );
    set_scope(cx, h, ComposeScope::Window);
    set_scope(cx, h, ComposeScope::Selected);
    assert_eq!(before, layout(&fixture, cx));
    in_frame(cx, h, |window, _| {
        assert_eq!(
            window
                .find(("compose-remote", INITIAL_WEB_TERMINAL))
                .checked(),
            Some(false)
        );
        assert_eq!(
            window
                .find(("compose-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(true)
        );
        assert_eq!(
            window.find("compose-pane-target").label(),
            Some("staging-api")
        )
    });
    draft(cx, h, "uptime");
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text() == "uptime\r"
    })
    .await;
    assert_eq!(fixture.web.written_text(), "");
    let active = cx.update(|cx| fixture.workspace.read(cx).active_tab());
    in_frame(cx, h, |window, cx| {
        window.click(("compose-remote", INITIAL_STAGING_TERMINAL), cx)
    });
    assert_eq!(
        active,
        cx.update(|cx| fixture.workspace.read(cx).active_tab())
    );
    assert_eq!(before, layout(&fixture, cx));
    in_frame(cx, h, |window, _| {
        assert_eq!(
            window.find("compose-pane-target").label(),
            Some("请选择终端")
        );
        assert!(!enabled(&window.find("compose-send")));
        assert!(!enabled(&window.find("compose-interrupt")));
    });
    draft(cx, h, "retained");
    submit(cx, h);
    in_frame(cx, h, |window, cx| window.click("compose-close", cx));
    in_frame(cx, h, |window, _| {
        assert!(
            window
                .try_find(("compose-remote", INITIAL_WEB_TERMINAL))
                .is_none()
        )
    });
    in_frame(cx, h, |window, cx| window.click("toggle-compose-pane", cx));
    in_frame(cx, h, |window, _| {
        assert_eq!(
            window.find("compose-pane-target").label(),
            Some("请选择终端")
        )
    });
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        true,
    );
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text() == "uptime\rretained\r"
    })
    .await;
}

#[gpui_kit::test]
async fn window_scope_includes_background_tabs_and_snippets_without_splitting(
    cx: &mut TestAppContext,
) {
    let f = open(cx).await;
    let h = f.handle;
    show(cx, h, ComposeScope::Window);
    let before = layout(&f, cx);
    in_frame(cx, h, |window, _| {
        assert_eq!(window.find("compose-pane-target").label(), Some("2 个终端"));
        assert!(
            window
                .try_find(("compose-remote", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert_eq!(window.find("compose-keys").checked(), Some(false));
    });
    draft(cx, h, "uptime");
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.staging.written_text().ends_with('\r') && f.web.written_text().ends_with('\r')
    })
    .await;
    assert_eq!(f.web.written_text(), "\x1b[200~uptime\x1b[201~\r");
    assert_eq!(f.staging.written_text(), "uptime\r");
    assert_eq!(before, layout(&f, cx));
    submit(cx, h); // Empty window-scope drafts never press Enter.
    in_frame(cx, h, |window, cx| {
        window.dispatch_action(Box::new(SendComposeSnippet(SnippetId(1))), cx)
    });
    in_frame(cx, h, |window, cx| window.click("compose-interrupt", cx));
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.staging.written_text().ends_with('\x03')
    })
    .await;
    assert_eq!(f.staging.written_text(), "uptime\rdf -h\r\x03");
    set_scope(cx, h, ComposeScope::Selected);
    in_frame(cx, h, |window, _| {
        for id in [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL] {
            assert_eq!(window.find(("compose-remote", id)).checked(), Some(false));
        }
    });
}

#[gpui_kit::test]
async fn keys_encode_once_per_terminal_and_are_isolated_from_other_inputs(cx: &mut TestAppContext) {
    let f = open(cx).await;
    let h = f.handle;
    in_frame(cx, h, |w, cx| {
        w.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    show(cx, h, ComposeScope::Window);
    in_frame(cx, h, |w, cx| w.click("compose-keys", cx));
    in_frame(cx, h, |w, cx| {
        assert_eq!(w.find("compose-keys").checked(), Some(true));
        assert!(
            w.try_find(("compose-sync-remote", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(w.try_find("compose-sync-status").is_none());
        w.dispatch_action(Box::new(FindInTerminal), cx);
    });
    in_frame(cx, h, |w, cx| {
        w.input("search-only", cx);
        w.press("escape", cx);
    });
    assert_eq!(f.web.written_text(), "");
    assert_eq!(f.staging.written_text(), "");
    draft(cx, h, "draft-only");
    assert_eq!(f.web.written_text(), "");
    assert_eq!(f.staging.written_text(), "");
    in_frame(cx, h, |w, cx| {
        w.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        w.input("中文", cx);
        w.press("up", cx);
        w.press("enter", cx);
        w.press("ctrl-c", cx);
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.staging.written_text().ends_with('\x03')
    })
    .await;
    in_frame(cx, h, |w, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("one\ntwo".into()));
        w.dispatch_action(Box::new(PasteTerminal), cx);
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.staging.written_text().ends_with("one\ntwo")
            && f.web.written_text().ends_with("\x1b[201~")
    })
    .await;
    assert_eq!(
        f.web.written_text(),
        "中文\x1bOA\r\x03\x1b[200~one\ntwo\x1b[201~"
    );
    assert_eq!(f.staging.written_text(), "中文\x1b[A\r\x03one\ntwo");
    in_frame(cx, h, |w, cx| w.click("compose-close", cx));
    in_frame(cx, h, |w, cx| w.input("solo", cx));
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.web.written_text().ends_with("solo")
    })
    .await;
    assert!(f.staging.written_text().ends_with("one\ntwo"));
    in_frame(cx, h, |w, cx| w.click("toggle-compose-pane", cx));
    in_frame(cx, h, |w, _| {
        assert_eq!(w.find("compose-keys").checked(), Some(false))
    });
}

#[gpui_kit::test]
async fn new_and_reconnected_targets_stop_keys_and_manual_targets_do_not_rejoin(
    cx: &mut TestAppContext,
) {
    let f = open(cx).await;
    let h = f.handle;
    show(cx, h, ComposeScope::Window);
    in_frame(cx, h, |w, cx| w.click("compose-keys", cx));
    open_local(cx, &f).await;
    in_frame(cx, h, |w, _| {
        assert_eq!(
            w.find("compose-pane-target").label(),
            Some("3 个终端 · 本地 1")
        );
        assert_eq!(w.find("compose-keys").checked(), Some(false));
        assert!(w.try_find("compose-sync-status").is_none());
    });
    set_scope(cx, h, ComposeScope::Selected);
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        true,
    );
    in_frame(cx, h, |w, cx| {
        w.dispatch_action(
            Box::new(ReconnectTerminal(RemoteTerminalId(
                INITIAL_STAGING_TERMINAL,
            ))),
            cx,
        );
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, cx| {
        f.staging.starts() == 2
            && remote_lifecycle(&f.workspace, RemoteTerminalId(INITIAL_STAGING_TERMINAL), cx)
                .accepts_input()
    })
    .await;
    in_frame(cx, h, |w, _| {
        assert_eq!(
            w.find(("compose-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(false)
        )
    });
    set_scope(cx, h, ComposeScope::Window);
    in_frame(cx, h, |w, _| {
        assert_eq!(
            w.find("compose-pane-target").label(),
            Some("3 个终端 · 本地 1")
        )
    });
}

#[gpui_kit::test]
async fn partial_submission_blocks_enter_and_retries_only_original_unsent_targets(
    cx: &mut TestAppContext,
) {
    let f = open(cx).await;
    let h = f.handle;
    open_local(cx, &f).await;
    show(cx, h, ComposeScope::Window);
    draft(cx, h, "once");
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.web.written_text().ends_with('\r') && f.staging.written_text().ends_with('\r')
    })
    .await;
    assert_eq!(f.local.written_text(), "");
    cx.simulate_window_resize(h.into(), size(px(900.), px(700.)));
    in_frame(cx, h, |w, cx| {
        let pane = w.find("compose-pane").bounds();
        for id in ["compose-close", "compose-send", "compose-interrupt"] {
            let bounds = w.find(id).bounds();
            assert!(
                bounds.left() >= pane.left() && bounds.right() <= pane.right(),
                "{id}: {bounds:?} {pane:?}"
            );
            assert!(
                bounds.top() >= pane.top() && bounds.bottom() <= pane.bottom(),
                "{id}: {bounds:?} {pane:?}"
            );
        }
        assert!(!enabled(&w.find("compose-send")));
        assert_eq!(w.find("compose-result").label(), Some("已提交 2，未发送 1"));
        w.press("enter", cx);
    });
    in_frame(cx, h, |w, _| {
        assert!(w.try_find("compose-result-details").is_none())
    });
    in_frame(cx, h, |w, cx| w.click("compose-result-expand", cx));
    in_frame(cx, h, |w, cx| {
        assert!(w.find("compose-result-details").visible());
        w.press("escape", cx);
    });
    in_frame(cx, h, |w, cx| {
        assert!(w.try_find("compose-result-details").is_none());
        w.click("compose-body", cx);
    });
    for language in [
        InterfaceLanguage::English,
        InterfaceLanguage::SimplifiedChinese,
    ] {
        in_frame(cx, h, |_, cx| {
            f.workspace
                .read(cx)
                .settings()
                .clone()
                .update(cx, |store, cx| {
                    store.update(
                        |s| {
                            s.language = language;
                            s.interface.font_size = 20.;
                        },
                        cx,
                    );
                });
        });
        in_frame(cx, h, |w, _| {
            let pane = w.find("compose-pane").bounds();
            for id in [
                "compose-result",
                "compose-result-expand",
                "compose-retry",
                "compose-edit-new",
            ] {
                let bounds = w.find(id).bounds();
                assert!(
                    bounds.left() >= pane.left() && bounds.right() <= pane.right(),
                    "{id}: {bounds:?} {pane:?}"
                );
                assert!(bounds.top() >= pane.top() && bounds.bottom() <= pane.bottom());
            }
        });
    }
    // Another terminal appears after the original snapshot.
    in_frame(cx, h, |w, cx| {
        w.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| f.web.starts() == 2)
        .await;
    // Exit the local full-screen program without reconnecting.
    f.local.print("\x1b[?1049lready-for-retry");
    cx.wait_for(h.into(), Duration::from_secs(2), |_, cx| {
        f.workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .screen_text(cx)
            .contains("ready-for-retry")
    })
    .await;
    in_frame(cx, h, |w, cx| w.dispatch_action(Box::new(RetryCompose), cx));
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.local.written_text() == "once\r"
    })
    .await;
    assert_eq!(f.web.written_text(), "\x1b[200~once\x1b[201~\r");
    assert_eq!(f.staging.written_text(), "once\r");
    in_frame(cx, h, |w, _| {
        assert_eq!(w.find("compose-result").label(), Some("已提交 3，未发送 0"))
    });
}

#[gpui_kit::test]
async fn retry_refuses_a_new_generation_and_edit_as_new_releases_the_draft(
    cx: &mut TestAppContext,
) {
    let f = open(cx).await;
    let h = f.handle;
    open_local(cx, &f).await;
    show(cx, h, ComposeScope::Window);
    draft(cx, h, "original");
    submit(cx, h);
    in_frame(cx, h, |w, cx| {
        w.dispatch_action(
            Box::new(shellrs::app::RestartLocalTerminal(LocalTerminalId(1))),
            cx,
        )
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.local.starts() == 2
    })
    .await;
    in_frame(cx, h, |w, cx| w.dispatch_action(Box::new(RetryCompose), cx));
    assert_eq!(f.local.written_text(), "");
    in_frame(cx, h, |w, _| assert!(!enabled(&w.find("compose-retry"))));
    in_frame(cx, h, |w, cx| w.click("compose-edit-new", cx));
    set_scope(cx, h, ComposeScope::Selected);
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)),
        true,
    );
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        true,
    );
    in_frame(cx, h, |w, _| {
        assert!(w.try_find("compose-result").is_none())
    });
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.staging.written_text() == "original\roriginal\r"
    })
    .await;
}

#[gpui_kit::test]
async fn all_refused_retains_the_draft_and_unselected_input_is_not_shared(cx: &mut TestAppContext) {
    let f = open(cx).await;
    let h = f.handle;
    open_local(cx, &f).await;
    show(cx, h, ComposeScope::Current);
    draft(cx, h, "retained");
    submit(cx, h);
    in_frame(cx, h, |w, _| {
        assert_eq!(w.find("compose-result").label(), Some("已提交 0，未发送 1"))
    });
    assert_eq!(f.local.written_text(), "");
    f.local.print("\x1b[?1049lready-for-command");
    cx.wait_for(h.into(), Duration::from_secs(2), |_, cx| {
        f.workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .read(cx)
            .terminal()
            .read(cx)
            .screen_text(cx)
            .contains("ready-for-command")
    })
    .await;
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.local.written_text() == "retained\r"
    })
    .await;
    set_scope(cx, h, ComposeScope::Window);
    set_scope(cx, h, ComposeScope::Selected);
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)),
        true,
    );
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        true,
    );
    in_frame(cx, h, |w, cx| w.click("compose-keys", cx));
    in_frame(cx, h, |w, cx| {
        w.click(("local-terminal", 1_u64), cx);
        w.input("only-local", cx);
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.local.written_text().ends_with("only-local")
    })
    .await;
    assert_eq!(f.web.written_text(), "");
    assert_eq!(f.staging.written_text(), "");
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        false,
    );
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        true,
    );
    in_frame(cx, h, |w, _| {
        assert_eq!(w.find("compose-keys").checked(), Some(false))
    });
}

#[gpui_kit::test]
async fn compose_covers_telnet_serial_and_duplicate_host_terminals(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let telnet_id = store.insert_unnotified(HostDraft::telnet("switch", "10.0.0.5", 23, None));
    let serial_id = store.insert_unnotified(HostDraft::serial(
        "console",
        "/dev/ttyUSB0",
        shellrs::host::SerialSettings::default(),
        None,
    ));
    let telnet = Arc::new(FakeTerminalFactory::default());
    let serial = Arc::new(FakeTerminalFactory::default());
    let (h, workspace) = open_workspace_with_protocols(
        cx,
        store,
        Arc::new(FakeTerminalFactory::default()),
        Arc::new(FixedTelnetTerminalTransportProvider::new(telnet.clone())),
        Arc::new(FixedSerialTerminalTransportProvider::new(serial.clone())),
    );
    for (ix, host) in [telnet_id, serial_id, telnet_id].into_iter().enumerate() {
        in_frame(cx, h, |w, cx| {
            w.dispatch_action(Box::new(ConnectHost(host)), cx)
        });
        cx.wait_for(h.into(), Duration::from_secs(2), |_, cx| {
            workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(ix as u64 + 1))
                .is_some_and(|panel| {
                    panel
                        .read(cx)
                        .terminal()
                        .read(cx)
                        .lifecycle(cx)
                        .accepts_input()
                })
        })
        .await;
    }
    show(cx, h, ComposeScope::Window);
    draft(cx, h, "status");
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        telnet.written_text() == "status\rstatus\r" && serial.written_text() == "status\r"
    })
    .await;
    set_scope(cx, h, ComposeScope::Selected);
    target(cx, h, CenterTab::Terminal(RemoteTerminalId(3)), true);
    draft(cx, h, "only-second-tab");
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        telnet.written_text().ends_with("only-second-tab\r")
    })
    .await;
    assert_eq!(serial.written_text(), "status\r");
    let writes = telnet.writes_by_run.lock().unwrap();
    assert!(
        writes
            .iter()
            .any(|(run, bytes)| *run == 2 && bytes == b"only-second-tab\r")
    );
    assert!(
        !writes
            .iter()
            .any(|(run, bytes)| *run == 1 && bytes == b"only-second-tab\r")
    );
}

#[gpui_kit::test]
async fn manual_selection_survives_user_splits_zoom_and_renaming(cx: &mut TestAppContext) {
    use gpui_kit::component::{
        Placement,
        dock::{DockPlacement, InsertTarget, PanelId, ToggleZoom},
    };
    use shellrs::shared::RenamableTab;
    let f = open(cx).await;
    let h = f.handle;
    show(cx, h, ComposeScope::Window);
    set_scope(cx, h, ComposeScope::Selected);
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL)),
        true,
    );
    target(
        cx,
        h,
        CenterTab::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
        true,
    );
    cx.update_window(h.into(), |_, w, cx| {
        let workspace = f.workspace.read(cx);
        let panel = PanelId::from(
            workspace
                .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
                .unwrap()
                .entity_id(),
        );
        let area = workspace.dock_area().clone();
        area.update(cx, |area, cx| {
            let node = area
                .layout(DockPlacement::Center)
                .unwrap()
                .find_panel_node(panel)
                .unwrap();
            area.move_panel(
                panel,
                InsertTarget::Split {
                    node,
                    placement: Placement::Right,
                    size: None,
                },
                w,
                cx,
            );
        });
    })
    .unwrap();
    cx.run_until_parked();
    let split = layout(&f, cx);
    in_frame(cx, h, |w, cx| w.click("compose-keys", cx));
    cx.update(|cx| {
        f.workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
            .unwrap()
            .clone()
            .update(cx, |panel, cx| {
                panel.set_custom_title(Some("renamed".into()), cx)
            })
    });
    in_frame(cx, h, |w, cx| {
        assert!(w.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        assert!(w.find(("terminal", INITIAL_STAGING_TERMINAL)).visible());
        assert_eq!(w.find("compose-keys").checked(), Some(true));
        w.click(("compose-remote", INITIAL_STAGING_TERMINAL), cx);
    });
    assert_eq!(split, layout(&f, cx));
    in_frame(cx, h, |w, cx| {
        w.click(("compose-remote", INITIAL_STAGING_TERMINAL), cx);
    });
    in_frame(cx, h, |w, _| {
        assert_eq!(
            w.find(("compose-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(true)
        );
    });
    in_frame(cx, h, |w, cx| w.click("compose-keys", cx));
    draft(cx, h, "zoomed");
    in_frame(cx, h, |w, cx| {
        w.click(("terminal", INITIAL_STAGING_TERMINAL), cx);
        w.dispatch_action(Box::new(ToggleZoom), cx);
    });
    // Dock zoom temporarily shows only the active group and stops key sync.
    // Routing still includes the hidden split; restoring keeps the selection.
    in_frame(cx, h, |w, _| {
        assert!(
            w.try_find(("compose-remote", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        assert!(
            w.try_find(("compose-sync-remote", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
    });
    submit(cx, h);
    in_frame(cx, h, |w, cx| {
        w.click(("terminal", INITIAL_STAGING_TERMINAL), cx);
        w.dispatch_action(Box::new(ToggleZoom), cx);
    });
    in_frame(cx, h, |w, _| {
        assert_eq!(w.find("compose-keys").checked(), Some(false));
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.web.written_text().ends_with('\r') && f.staging.written_text() == "zoomed\r"
    })
    .await;
    assert_eq!(f.web.starts(), 1);
    assert_eq!(f.staging.starts(), 1);
}

#[gpui_kit::test]
async fn snippets_preserve_an_existing_draft_and_respect_run_on_click(cx: &mut TestAppContext) {
    let f = open(cx).await;
    let h = f.handle;
    show(cx, h, ComposeScope::Window);
    draft(cx, h, "draft-kept");
    let snippet = cx.update(|cx| {
        f.workspace
            .read(cx)
            .store()
            .clone()
            .update(cx, |store, cx| {
                store.insert_snippet(SnippetDraft::new("insert", "echo prepare", None), cx)
            })
    });
    in_frame(cx, h, |w, cx| {
        w.dispatch_action(Box::new(SendComposeSnippet(snippet)), cx)
    });
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.staging.written_text() == "echo prepare"
    })
    .await;
    submit(cx, h);
    cx.wait_for(h.into(), Duration::from_secs(2), |_, _| {
        f.staging.written_text() == "echo preparedraft-kept\r"
    })
    .await;
}
