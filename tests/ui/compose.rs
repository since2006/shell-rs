//! 撰写窗格: shown and hidden by its shortcut, below the terminals, sending
//! what it holds to the terminal in front, local or remote, and remembered
//! in the settings. No server, PTY or keychain is touched.

use std::collections::HashMap;

use crate::support::*;
use shellrs::app::{SendCompose, ToggleComposePane};

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

/// web-01's and staging-api's terminals, connected, web-01's in front;
/// `shown` is whether the pane was left showing last time.
async fn open(cx: &mut TestAppContext, shown: bool) -> Fixture {
    init_app(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let web = Arc::new(FakeTerminalFactory::printing("ready-web"));
    let staging = Arc::new(FakeTerminalFactory::printing("ready-staging"));
    let local = Arc::new(FakeTerminalFactory::printing("ready-local"));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let seed = HostStore::seed();
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
        let mut settings = settings_store();
        settings.update_unnotified(|settings| settings.compose_pane = shown);
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                cx.new(|_| seed),
                cx.new(|_| settings),
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
            .all(|id| remote_lifecycle(&workspace, RemoteTerminalId(id), cx).accepts_input())
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx);
    });
    Fixture {
        handle,
        workspace,
        web,
        staging,
        local,
    }
}

fn pane_setting(fixture: &Fixture, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| {
        fixture
            .workspace
            .read(cx)
            .settings()
            .read(cx)
            .settings()
            .compose_pane
    })
}

#[gpui_kit::test]
async fn the_shortcut_shows_the_pane_and_enter_runs_what_it_holds(cx: &mut TestAppContext) {
    let fixture = open(cx, false).await;
    let handle = fixture.handle;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        window.dispatch_action(Box::new(ToggleComposePane), cx);
    });
    in_frame(cx, handle, |window, _| {
        let pane = window.find("compose-pane").bounds();
        let terminal = window.find(("terminal", INITIAL_WEB_TERMINAL)).bounds();
        assert!(terminal.bottom() <= pane.top());
        assert_eq!(window.find("compose-pane-target").label(), Some("web-01"));
    });
    assert!(pane_setting(&fixture, cx));
    // The pane has the keyboard. Shift+Enter starts a line; nothing goes
    // out until Enter.
    in_frame(cx, handle, |window, cx| {
        window.input("echo one", cx);
        window.press("shift-enter", cx);
        window.input("echo two", cx);
    });
    assert_eq!(fixture.web.written_text(), "");
    in_frame(cx, handle, |window, cx| window.press("enter", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text() == "echo one\necho two\r"
    })
    .await;
    // Sent, so the pane is empty again: Enter now presses Enter there.
    in_frame(cx, handle, |window, cx| window.press("enter", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text() == "echo one\necho two\r\r"
    })
    .await;
    assert_eq!(fixture.staging.written_text(), "");
    // Hidden again, the keyboard goes back to the terminal.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleComposePane), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        window.input("ls", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text().ends_with("\r\rls")
    })
    .await;
    assert!(!pane_setting(&fixture, cx));
}

#[gpui_kit::test]
async fn the_pane_sends_to_the_terminal_in_front(cx: &mut TestAppContext) {
    let fixture = open(cx, true).await;
    let handle = fixture.handle;
    // Left showing last time: there again with the terminals.
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("compose-pane-target").label(), Some("web-01"));
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("compose-pane-target").label(),
            Some("staging-api")
        );
        window.click("compose-pane", cx);
        window.input("uptime", cx);
    });
    in_frame(cx, handle, |window, cx| window.click("compose-send", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text() == "uptime\r"
    })
    .await;
    assert_eq!(fixture.web.written_text(), "");
    // A local terminal in front takes it too.
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
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
    in_frame(cx, handle, |window, cx| {
        assert_ne!(
            window.find("compose-pane-target").label(),
            Some("staging-api")
        );
        window.click("compose-pane", cx);
        window.input("pwd", cx);
        window.dispatch_action(Box::new(SendCompose), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.local.written_text() == "pwd\r"
    })
    .await;
    assert_eq!(fixture.staging.written_text(), "uptime\r");
}

#[gpui_kit::test]
async fn the_pane_goes_with_the_last_terminal_and_comes_back_with_the_next(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx, true).await;
    let handle = fixture.handle;
    for id in [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL] {
        in_frame(cx, handle, |window, cx| {
            assert!(window.find("compose-pane").visible());
            window.click(("close-terminal", id), cx)
        });
    }
    // The start page: nothing to write to, and the shortcut does nothing.
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        window.dispatch_action(Box::new(ToggleComposePane), cx);
    });
    assert!(pane_setting(&fixture, cx));
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("compose-pane").visible());
    });
}

#[gpui_kit::test]
async fn up_and_down_go_through_what_was_sent(cx: &mut TestAppContext) {
    let fixture = open(cx, true).await;
    let handle = fixture.handle;
    // One key a frame, as typed. Sent by the action Enter dispatches: the
    // test's Enter also types a newline where the caret is, which no
    // system's does.
    let press = |cx: &mut TestAppContext, key: &str| {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    };
    let send = |cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(SendCompose), cx)
        });
    };
    let sent = async |cx: &mut TestAppContext, text: &str| {
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
            fixture.web.written_text().ends_with(text)
        })
        .await;
    };
    in_frame(cx, handle, |window, cx| {
        window.click("compose-pane", cx);
        window.input("echo one", cx);
        window.press("shift-enter", cx);
        window.input("echo two", cx);
    });
    send(cx);
    in_frame(cx, handle, |window, cx| window.input("uptime", cx));
    send(cx);
    sent(cx, "uptime\r").await;
    assert_eq!(fixture.web.written_text(), "echo one\necho two\ruptime\r");
    // Back from what is being written: the newest, then the two lines,
    // whose caret is on the first, so ↑ goes on at once. Nothing is older.
    in_frame(cx, handle, |window, cx| window.input("df", cx));
    for key in ["up", "up", "up"] {
        press(cx, key);
    }
    send(cx);
    sent(cx, "uptime\recho one\necho two\r").await;
    // Forward again: ↓ goes down the two lines first, then to the newest,
    // then to what was being written, kept.
    in_frame(cx, handle, |window, cx| window.input("df", cx));
    for key in ["up", "up", "down", "down", "down"] {
        press(cx, key);
    }
    send(cx);
    sent(cx, "echo two\rdf\r").await;
    // What was sent last is where ↑ starts again.
    press(cx, "up");
    send(cx);
    sent(cx, "df\rdf\r").await;
    // ↑ below the first line moves up the text, as in any.
    in_frame(cx, handle, |window, cx| {
        window.input("a", cx);
        window.press("shift-enter", cx);
        window.input("b", cx);
    });
    press(cx, "up");
    in_frame(cx, handle, |window, cx| window.input("x", cx));
    send(cx);
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        let text = fixture.web.written_text();
        text.ends_with("df\rxa\nb\r") || text.ends_with("df\rax\nb\r")
    })
    .await;
}
