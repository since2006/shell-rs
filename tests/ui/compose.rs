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
async fn the_pane_stays_visible_without_targets(cx: &mut TestAppContext) {
    let fixture = open(cx, true).await;
    let handle = fixture.handle;
    for id in [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL] {
        in_frame(cx, handle, |window, cx| {
            assert!(window.find("compose-pane").visible());
            window.click(("close-terminal", id), cx)
        });
    }
    // A requested pane stays visible with a useful disabled state.
    in_frame(cx, handle, |window, _| {
        assert!(window.find("compose-pane").visible());
        assert_eq!(
            window.find("compose-pane-target").label(),
            Some("没有可发送的终端")
        );
    });
    assert!(pane_setting(&fixture, cx));
    in_frame(cx, handle, |window, cx| window.click("compose-close", cx));
    assert!(!pane_setting(&fixture, cx));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleComposePane), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.find("compose-pane").visible())
    });
}

#[gpui_kit::test]
async fn compose_works_with_only_local_terminals_and_resets_scope_on_restart(
    cx: &mut TestAppContext,
) {
    use shellrs::app::{ComposeScope, SetComposeScope, SetComposeTarget};
    init_app(cx);
    let factory = Arc::new(FakeTerminalFactory::default());
    let mut workspace = None;
    let handle = cx.open_window(size(px(1100.), px(760.)), |window, cx| {
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                cx.new(|_| HostStore::empty()),
                cx.new(|_| settings_store()),
                Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
                    FakeTerminalFactory::default(),
                ))),
                fake_telnet_terminals(),
                fake_serial_terminals(),
                Arc::new(FixedLocalTerminalTransportProvider::new(factory.clone())),
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
    for id in 1..=2 {
        in_frame(cx, handle, |w, cx| w.click("new-local-terminal", cx));
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            workspace
                .read(cx)
                .local_terminal(LocalTerminalId(id))
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
    in_frame(cx, handle, |w, cx| w.click("toggle-compose-pane", cx));
    in_frame(cx, handle, |w, cx| {
        w.dispatch_action(Box::new(SetComposeScope::new(ComposeScope::Window)), cx)
    });
    in_frame(cx, handle, |w, _| {
        assert_eq!(
            w.find("compose-pane-target").label(),
            Some("2 个终端 · 本地 2")
        )
    });
    in_frame(cx, handle, |w, cx| {
        w.click("compose-pane", cx);
        w.input("both", cx);
        w.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "both\rboth\r"
    })
    .await;
    in_frame(cx, handle, |w, cx| w.click("compose-keys", cx));
    in_frame(cx, handle, |w, cx| {
        w.click(("local-terminal", 2_u64), cx);
        w.input("x", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "both\rboth\rxx"
    })
    .await;
    in_frame(cx, handle, |w, cx| {
        w.dispatch_action(Box::new(SetComposeScope::new(ComposeScope::Selected)), cx)
    });
    in_frame(cx, handle, |w, cx| {
        w.dispatch_action(
            Box::new(SetComposeTarget::new(
                CenterTab::LocalTerminal(LocalTerminalId(1)),
                true,
            )),
            cx,
        )
    });
    in_frame(cx, handle, |w, cx| {
        w.click("compose-pane", cx);
        w.input("one", cx);
        w.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text().ends_with("xxone\r")
    })
    .await;
    // Renaming a local target updates the summary without changing membership.
    cx.update(|cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(1))
            .unwrap()
            .clone()
            .update(cx, |panel, cx| {
                panel.set_name(
                    shellrs::terminal::SavedName {
                        name: "renamed-local".into(),
                        fixed: true,
                        title: None,
                    },
                    cx,
                );
            });
    });
    in_frame(cx, handle, |w, _| {
        assert!(
            w.find("compose-pane-target")
                .label()
                .unwrap()
                .contains("renamed-local")
        );
    });
    // A fresh Workspace never restores in-memory selection or key sync.
    let next = open(cx, true).await;
    in_frame(cx, next.handle, |w, _| {
        assert_eq!(w.find("compose-scope").label(), Some("发送范围：当前终端"));
        assert!(w.try_find("compose-keys").is_none());
    });
}

#[gpui_kit::test]
async fn compose_controls_fit_narrow_windows_long_names_and_larger_fonts(cx: &mut TestAppContext) {
    use shellrs::shared::RenamableTab;
    let f = open(cx, true).await;
    let long = "a-very-long-terminal-name-that-must-not-push-controls-out-of-the-pane";
    cx.update(|cx| {
        f.workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
            .unwrap()
            .clone()
            .update(cx, |panel, cx| {
                panel.set_custom_title(Some(long.into()), cx)
            });
    });
    in_frame(cx, f.handle, |w, _| {
        let pane = w.find("compose-pane").bounds();
        assert!(
            pane.size.height <= px(112.),
            "compose should start compact: {pane:?}"
        );
        let toggle = w.find("toggle-compose-pane").bounds();
        let sidebar = w.find("toggle-hosts").bounds();
        assert_eq!(toggle.top(), sidebar.top());
        assert!(toggle.left() >= sidebar.right());
        assert!(
            toggle.bottom()
                < w.find(("terminal-tab", INITIAL_WEB_TERMINAL))
                    .bounds()
                    .bottom()
        );
        assert!(w.try_find("tab-bar-controls").is_none());
    });
    cx.simulate_window_resize(f.handle.into(), size(px(900.), px(700.)));
    for language in [
        InterfaceLanguage::English,
        InterfaceLanguage::SimplifiedChinese,
    ] {
        for appearance in [Appearance::Light, Appearance::Dark] {
            in_frame(cx, f.handle, |window, cx| {
                f.workspace
                    .read(cx)
                    .settings()
                    .clone()
                    .update(cx, |store, cx| {
                        store.update(
                            |s| {
                                s.language = language;
                                s.appearance = appearance;
                                s.interface.font_size = 20.;
                            },
                            cx,
                        )
                    });
                window.refresh();
            });
            in_frame(cx, f.handle, |w, _| {
                let pane = w.find("compose-pane").bounds();
                for id in [
                    "compose-close",
                    "compose-send",
                    "compose-interrupt",
                    "compose-scope",
                ] {
                    let control = w.find(id).bounds();
                    assert!(
                        control.left() >= pane.left() && control.right() <= pane.right(),
                        "{id} outside pane horizontally: {control:?} {pane:?}"
                    );
                    assert!(
                        control.top() >= pane.top() && control.bottom() <= pane.bottom(),
                        "{id} outside pane vertically: {control:?} {pane:?}"
                    );
                }
                assert_eq!(w.find("compose-pane-target").label(), Some(long));
            });
            for scope in [
                shellrs::app::ComposeScope::Window,
                shellrs::app::ComposeScope::Selected,
            ] {
                in_frame(cx, f.handle, |w, cx| {
                    w.click("compose-body", cx);
                    w.dispatch_action(Box::new(shellrs::app::SetComposeScope::new(scope)), cx);
                });
                in_frame(cx, f.handle, |w, _| {
                    let pane = w.find("compose-pane").bounds();
                    for id in [
                        "compose-scope",
                        "compose-snippets",
                        "compose-interrupt",
                        "compose-send",
                        "compose-close",
                    ] {
                        let bounds = w.find(id).bounds();
                        assert!(
                            bounds.left() >= pane.left() && bounds.right() <= pane.right(),
                            "{id}: {bounds:?} {pane:?}"
                        );
                        assert!(bounds.top() >= pane.top() && bounds.bottom() <= pane.bottom());
                    }
                    assert!(w.find("compose-body").bounds().size.height >= px(32.));
                });
            }
            in_frame(cx, f.handle, |w, cx| {
                w.click("compose-body", cx);
                w.dispatch_action(
                    Box::new(shellrs::app::SetComposeScope::new(
                        shellrs::app::ComposeScope::Current,
                    )),
                    cx,
                );
            });
        }
    }
}
