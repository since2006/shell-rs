//! 命令广播: the checkboxes on terminal tabs, the split layout they bring
//! with 撰写窗格 sending to them all, and keys shared on request, with an
//! independent transport log for every participant. No server, PTY or
//! keychain is touched.

use std::collections::HashMap;

use crate::support::*;
use gpui_kit::component::dock::ToggleZoom;
use shellrs::app::{CloseSettings, PasteTerminal, SendComposeSnippet};
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

/// Turn 命令广播 on and check web-01's and staging-api's terminals.
fn select_pair(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.click("toggle-terminal-broadcast", cx)
    });
    for id in [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL] {
        in_frame(cx, handle, |window, cx| {
            assert_eq!(window.find(("broadcast-remote", id)).checked(), Some(false));
            window.click(("broadcast-remote", id), cx);
        });
    }
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("toggle-terminal-broadcast").label(),
            Some("广播中 · 2 个终端")
        );
    });
}

/// Open a local terminal, LocalTerminalId(1), and wait for its shell.
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

#[gpui_kit::test]
async fn the_toggle_puts_checkboxes_ahead_of_the_tab_marks(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        assert!(window.try_find("toggle-dock:Left").is_none());
        let broadcast = window.find("toggle-terminal-broadcast").bounds();
        let tab = window.find(("terminal-tab", INITIAL_WEB_TERMINAL)).bounds();
        assert!(broadcast.right() <= tab.left());
        // The controls run the bar's full height, so their bottom border
        // lines up with the one under the tabs.
        let controls = window.find("tab-bar-controls").bounds();
        let bar = window.find("center-tab-bar").bounds();
        assert!(bar.size.height > px(0.));
        assert_eq!(controls.top(), bar.top());
        assert_eq!(controls.bottom(), bar.bottom());
        assert!(controls.right() <= bar.left());
        assert!(
            window
                .find(("terminal-tab-os", INITIAL_WEB_TERMINAL))
                .visible()
        );
        window.click("toggle-terminal-broadcast", cx);
    });
    let active = cx.update(|cx| fixture.workspace.read(cx).active_tab());
    in_frame(cx, handle, |window, cx| {
        // The checkbox comes ahead of the mark, which still tells whether
        // the terminal is connected.
        let checkbox = window.find(("broadcast-remote", INITIAL_WEB_TERMINAL));
        let mark = window.find(("terminal-tab-os", INITIAL_WEB_TERMINAL));
        assert!(mark.visible());
        assert!(checkbox.bounds().right() <= mark.bounds().left());
        assert_eq!(checkbox.checked(), Some(false));
        window.click(("broadcast-remote", INITIAL_WEB_TERMINAL), cx);
    });
    // One checked terminal leaves the layout alone and brings no pane.
    assert_eq!(
        cx.update(|cx| fixture.workspace.read(cx).active_tab()),
        active
    );
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        assert_eq!(
            window.find("toggle-terminal-broadcast").label(),
            Some("命令广播 · 勾选至少两个终端")
        );
        window.click("toggle-terminal-broadcast", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .find(("terminal-tab-os", INITIAL_WEB_TERMINAL))
                .visible()
        );
        assert!(
            window
                .try_find(("broadcast-remote", INITIAL_WEB_TERMINAL))
                .is_none()
        );
    });
    // With a tab left, the toggle stays; the start page has no tab bar.
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("toggle-terminal-broadcast").visible());
        // A lone tab is still a tab, selected as it is among others,
        // rather than a plain title.
        let tab = window.within("center-tab-bar").find(0usize);
        assert_eq!(tab.selected(), Some(true));
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("toggle-terminal-broadcast").is_none());
    });
}

#[gpui_kit::test]
async fn checking_two_terminals_splits_them_above_the_compose_pane(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    select_pair(cx, handle);
    in_frame(cx, handle, |window, _| {
        let pane = window.find("compose-pane").bounds();
        let web = window.find(("terminal", INITIAL_WEB_TERMINAL));
        let staging = window.find(("terminal", INITIAL_STAGING_TERMINAL));
        assert!(web.visible() && staging.visible());
        // Side by side, in the order checked, above the pane.
        assert!(web.bounds().right() <= staging.bounds().left());
        assert!(web.bounds().bottom() <= pane.top());
        assert!(staging.bounds().bottom() <= pane.top());
        assert_eq!(
            window.find("compose-pane-target").label(),
            Some("勾选的 2 个终端")
        );
    });
    // The pane has the keyboard: Enter runs it everywhere, each terminal
    // pasting the way its program asked.
    in_frame(cx, handle, |window, cx| {
        window.input("uptime", cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text().ends_with('\r') && fixture.staging.written_text().ends_with('\r')
    })
    .await;
    // Typed after what is there, as other tools send to all: nothing
    // cleared first.
    assert_eq!(fixture.web.written_text(), "\x1b[200~uptime\x1b[201~\r");
    assert_eq!(fixture.staging.written_text(), "uptime\r");
    // Sent, so the line is empty again; the snippets and Ctrl+C go to all.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(SendComposeSnippet(SnippetId(1))), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("compose-interrupt", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text().ends_with('\x03')
            && fixture.staging.written_text().ends_with('\x03')
    })
    .await;
    assert_eq!(fixture.staging.written_text(), "uptime\rdf -h\r\x03");
    // Typing in a terminal stays there until keys are shared.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.input("solo", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text().ends_with("solo")
    })
    .await;
    assert!(fixture.staging.written_text().ends_with('\x03'));
    // Stopping takes the pane and the checkboxes away; the split stays.
    in_frame(cx, handle, |window, cx| window.click("compose-close", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("compose-pane").is_none());
        assert!(
            window
                .try_find(("broadcast-remote", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        assert!(
            window
                .find(("terminal", INITIAL_STAGING_TERMINAL))
                .visible()
        );
    });
}

#[gpui_kit::test]
async fn a_terminal_that_cannot_take_the_command_says_so(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    open_local(cx, &fixture).await;
    in_frame(cx, handle, |window, cx| {
        window.click("toggle-terminal-broadcast", cx)
    });
    for target in [
        ElementId::from(("broadcast-remote", INITIAL_WEB_TERMINAL)),
        ElementId::from(("broadcast-local", 1_u64)),
    ] {
        in_frame(cx, handle, |window, cx| window.click(target.clone(), cx));
    }
    in_frame(cx, handle, |window, cx| {
        window.input("ls", cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        fixture.web.written_text().ends_with('\r') && window.notifications(cx).len() == 1
    })
    .await;
    // The full-screen program got nothing.
    assert_eq!(fixture.local.written_text(), "");
}

#[gpui_kit::test]
async fn shared_keys_reach_every_checked_terminal_in_its_own_mode(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    select_pair(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("broadcast-keys", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("broadcast-keys").checked(), Some(true));
        window.dispatch_action(Box::new(FindInTerminal), cx)
    });
    // Nothing typed in the find bar goes anywhere.
    in_frame(cx, handle, |window, cx| {
        window.input("search-only", cx);
        window.press("escape", cx);
    });
    assert_eq!(fixture.web.written_text(), "");
    assert_eq!(fixture.staging.written_text(), "");
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.input("中文", cx);
        window.press("up", cx);
        window.press("enter", cx);
        window.press("ctrl-c", cx);
    });
    // Each key a platform event of its own, as typed: the keys reach the
    // other terminal before the paste does.
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text().ends_with('\x03')
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("one\ntwo".into()));
        window.dispatch_action(Box::new(PasteTerminal), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text().ends_with("one\ntwo")
            && fixture.web.written_text().ends_with("\x1b[201~")
    })
    .await;
    assert_eq!(
        fixture.web.written_text(),
        "中文\x1bOA\r\x03\x1b[200~one\ntwo\x1b[201~"
    );
    assert_eq!(fixture.staging.written_text(), "中文\x1b[A\r\x03one\ntwo");
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_STAGING_TERMINAL), cx);
        window.input("from-b", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text().ends_with("from-b")
    })
    .await;
    // Stopping ends the sharing too.
    in_frame(cx, handle, |window, cx| {
        window.click("toggle-terminal-broadcast", cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_STAGING_TERMINAL), cx);
        window.input("alone", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text().ends_with("alone")
    })
    .await;
    assert!(fixture.web.written_text().ends_with("from-b"));
}

#[gpui_kit::test]
async fn unchecking_leaves_the_broadcast_and_new_tabs_are_not_checked(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    select_pair(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("broadcast-keys", cx));
    in_frame(cx, handle, |window, cx| {
        window.click(("broadcast-remote", INITIAL_STAGING_TERMINAL), cx)
    });
    // One left: no pane, nothing shared.
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.input("unshared", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text() == "unshared"
    })
    .await;
    assert_eq!(fixture.staging.written_text(), "");
    // A terminal opened meanwhile is not checked by itself.
    open_local(cx, &fixture).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("broadcast-local", 1_u64)).checked(),
            Some(false)
        )
    });
}

#[gpui_kit::test]
async fn hidden_tabs_stay_selected_but_reconnected_tabs_leave_broadcast(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    select_pair(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("toggle-terminal-broadcast").label(),
            Some("广播中 · 2 个终端")
        )
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(CloseSettings), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(
            Box::new(ReconnectTerminal(RemoteTerminalId(
                INITIAL_STAGING_TERMINAL,
            ))),
            cx,
        );
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        fixture.staging.starts() == 2
            && fixture
                .workspace
                .read(cx)
                .remote_terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL))
                .unwrap()
                .read(cx)
                .terminal()
                .read(cx)
                .lifecycle(cx)
                .accepts_input()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window
                .find(("broadcast-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(false)
        );
        assert!(window.try_find("compose-pane").is_none());
    });
}

#[gpui_kit::test]
async fn dragging_and_zooming_preserve_broadcast_membership_and_connections(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    select_pair(cx, handle);
    in_frame(cx, handle, |window, cx| window.click("broadcast-keys", cx));
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.dispatch_action(Box::new(ToggleZoom), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("toggle-terminal-broadcast").label(),
            Some("广播中 · 2 个终端")
        );
        window.input("zoomed", cx);
        window.dispatch_action(Box::new(ToggleZoom), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text() == "zoomed"
    })
    .await;
    // Merged back into one group by hand: still checked, still connected.
    in_frame(cx, handle, |window, cx| {
        let from = window
            .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
            .bounds()
            .center();
        let to = window
            .find(("terminal", INITIAL_WEB_TERMINAL))
            .bounds()
            .center();
        window.drag(from, to, cx);
    });
    in_frame(cx, handle, |window, _| {
        let shown = [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL]
            .into_iter()
            .filter(|id| {
                window
                    .try_find(("terminal", *id))
                    .is_some_and(|element| element.visible())
            })
            .count();
        assert_eq!(shown, 1);
        assert_eq!(
            window.find("toggle-terminal-broadcast").label(),
            Some("广播中 · 2 个终端")
        );
        assert_eq!(
            window
                .find(("broadcast-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(true)
        );
    });
    assert_eq!(fixture.web.starts(), 1);
    assert_eq!(fixture.staging.starts(), 1);
}
