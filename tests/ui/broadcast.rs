//! 命令广播: 撰写窗格 sending to the current terminal, to the terminals of
//! the tab in front, or to terminals chosen by hand in any tabs, and keys
//! shared on request, with an independent transport log for every
//! participant. No server, PTY or keychain is touched.

use std::collections::HashMap;

use crate::support::*;
use gpui_kit::component::{Placement, dock::ToggleZoom};
use shellrs::app::{
    ChooseComposeTerminals, PasteTerminal, SendComposeSnippet, SendTarget, SetSendTarget,
    ToggleComposePane,
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

const WEB: PaneContent = PaneContent::Terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL));
const STAGING: PaneContent = PaneContent::Terminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL));

fn dispatch(cx: &mut TestAppContext, handle: WindowHandle<Root>, action: impl gpui_kit::Action) {
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(action), cx)
    });
}

/// Where 撰写窗格 says it sends.
fn target(cx: &mut TestAppContext, handle: WindowHandle<Root>) -> Option<String> {
    in_frame(cx, handle, |window, _| {
        window
            .try_find("compose-pane-target")
            .and_then(|target| target.label().map(str::to_string))
    })
}

/// staging-api's terminal on the right of web-01's, in one tab.
fn side_by_side(cx: &mut TestAppContext, fixture: &Fixture) {
    split_beside(
        cx,
        fixture.handle,
        &fixture.workspace,
        STAGING,
        WEB,
        Placement::Right,
    );
}

/// The two terminals side by side, 撰写窗格 sending to both.
fn send_to_the_tab(cx: &mut TestAppContext, fixture: &Fixture) {
    side_by_side(cx, fixture);
    dispatch(cx, fixture.handle, SetSendTarget(SendTarget::Tab));
    assert_eq!(
        target(cx, fixture.handle).as_deref(),
        Some("当前标签的 2 个终端")
    );
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
async fn the_tab_bar_starts_with_the_sidebar_switch(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("compose-pane").is_none());
        assert!(window.try_find("toggle-terminal-broadcast").is_none());
        let hide = window.find("toggle-dock:Left").bounds();
        let tab = window.find(("terminal-tab", INITIAL_WEB_TERMINAL)).bounds();
        assert!(hide.right() <= tab.left());
        // The controls run the bar's full height, so their bottom border
        // lines up with the one under the tabs.
        let controls = window.find("tab-bar-controls").bounds();
        let bar = window.find("center-tab-bar").bounds();
        assert!(bar.size.height > px(0.));
        assert_eq!(controls.top(), bar.top());
        assert_eq!(controls.bottom(), bar.bottom());
        assert_eq!(controls.left(), bar.left());
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        // A lone tab is still a tab, selected as it is among others,
        // rather than a plain title.
        let tab = window.within("center-tab-bar").find(0usize);
        assert_eq!(tab.selected(), Some(true));
    });
}

#[gpui_kit::test]
async fn the_tabs_terminals_take_what_the_pane_sends(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    send_to_the_tab(cx, &fixture);
    in_frame(cx, handle, |window, _| {
        let pane = window.find("compose-pane").bounds();
        let web = window.find(("terminal", INITIAL_WEB_TERMINAL)).bounds();
        let staging = window.find(("terminal", INITIAL_STAGING_TERMINAL)).bounds();
        // Laid out as they were: sending to both moves nothing.
        assert!(web.right() <= staging.left());
        assert!(web.bottom() <= pane.top());
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
    dispatch(cx, handle, SendComposeSnippet(SnippetId(1)));
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
    // × hides the pane, and the split stays; shown again, it sends to the
    // current terminal, the broadcast not left chosen.
    in_frame(cx, handle, |window, cx| window.click("compose-close", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("compose-pane").is_none());
        assert!(window.find(("terminal", INITIAL_WEB_TERMINAL)).visible());
        assert!(
            window
                .find(("terminal", INITIAL_STAGING_TERMINAL))
                .visible()
        );
    });
    dispatch(cx, handle, ToggleComposePane);
    let target = target(cx, handle).expect("the pane shows again");
    assert!(!target.contains("个终端"), "still broadcasting: {target}");
}

/// Hidden, the pane shares no keys: what is typed in one terminal, a
/// password perhaps, does not go to others unseen.
#[gpui_kit::test]
async fn hiding_the_pane_stops_sharing_keys(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    send_to_the_tab(cx, &fixture);
    in_frame(cx, handle, |window, cx| window.click("broadcast-keys", cx));
    in_frame(cx, handle, |window, cx| window.click("compose-close", cx));
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.input("secret", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text() == "secret"
    })
    .await;
    assert_eq!(fixture.staging.written_text(), "");
}

#[gpui_kit::test]
async fn the_tab_target_goes_with_the_tab_in_front(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    dispatch(cx, handle, ToggleComposePane);
    assert_eq!(target(cx, handle).as_deref(), Some("web-01"));
    dispatch(cx, handle, SetSendTarget(SendTarget::Tab));
    assert_eq!(target(cx, handle).as_deref(), Some("当前标签的 1 个终端"));
    // staging-api's tab in front: it is the one the pane sends to.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("compose-pane", cx));
    in_frame(cx, handle, |window, cx| {
        window.input("whoami", cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text() == "whoami\r"
    })
    .await;
    assert_eq!(fixture.web.written_text(), "");
    // Back to the current terminal, the pane stays as it was switched on.
    dispatch(cx, handle, SetSendTarget(SendTarget::Current));
    assert_eq!(target(cx, handle).as_deref(), Some("staging-api"));
}

#[gpui_kit::test]
async fn terminals_chosen_by_hand_take_it_from_any_tab(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    open_local(cx, &fixture).await;
    side_by_side(cx, &fixture);
    let split = cx.read(|cx| {
        fixture
            .workspace
            .read(cx)
            .center()
            .read(cx)
            .tab_of(WEB)
            .expect("web-01's tab")
    });
    // From web-01: the chooser lists the open terminals tab by tab, the
    // split tab by its name with its terminals under it, web-01 checked.
    // Test windows start out inactive, where focus changes are not told.
    in_frame(cx, handle, |window, _| window.activate_window());
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx)
    });
    // The click's focus is told on the next frame.
    in_frame(cx, handle, |_, _| {});
    dispatch(cx, handle, ChooseComposeTerminals);
    in_frame(cx, handle, |window, cx| {
        let tab = window.find(("compose-choose-tab", split.0)).bounds();
        let web = window
            .find(("compose-choose-remote", INITIAL_WEB_TERMINAL))
            .bounds();
        let local = window.find(("compose-choose-local", 1_u64)).bounds();
        assert!(tab.top() < web.top() && web.top() < local.top());
        // Under its tab, indented.
        assert!(web.left() > tab.left());
        assert_eq!(local.left(), tab.left());
        assert_eq!(
            window
                .find(("compose-choose-remote", INITIAL_WEB_TERMINAL))
                .checked(),
            Some(true)
        );
        assert_eq!(
            window
                .find(("compose-choose-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(false)
        );
        // The split tab's own box checks its terminals.
        window.click(("compose-choose-tab", split.0), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window
                .find(("compose-choose-remote", INITIAL_STAGING_TERMINAL))
                .checked(),
            Some(true)
        );
        // A search that matches nothing lists nothing.
        window.click("compose-chooser-search", cx);
        window.input("no-such-terminal", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("compose-choose-remote", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        window.press("secondary-a", cx);
        window.press("backspace", cx);
    });
    // 全选 takes them all; the local one unchecked again, just the two.
    in_frame(cx, handle, |window, cx| {
        window.click("compose-choose-all", cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("compose-choose-local", 1_u64)).checked(),
            Some(true)
        );
        window.click(("compose-choose-local", 1_u64), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("compose-choose-ok", cx)
    });
    assert_eq!(target(cx, handle).as_deref(), Some("选定的 2 个终端"));
    // Each takes it.
    in_frame(cx, handle, |window, cx| {
        window.input("date", cx);
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.web.written_text().ends_with('\r') && fixture.staging.written_text().ends_with('\r')
    })
    .await;
    assert_eq!(fixture.staging.written_text(), "date\r");
    assert_eq!(fixture.local.written_text(), "");
}

/// While the chooser is open, it lists the terminals as they are: one
/// disconnected is greyed, says so, and is no longer checked.
#[gpui_kit::test]
async fn the_chooser_follows_the_terminals_while_open(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    dispatch(
        cx,
        handle,
        shellrs::app::SetComposeTerminals(vec![WEB, STAGING]),
    );
    dispatch(cx, handle, ChooseComposeTerminals);
    in_frame(cx, handle, |window, _| {
        let staging = window.find(("compose-choose-remote", INITIAL_STAGING_TERMINAL));
        assert_eq!(staging.checked(), Some(true));
    });
    dispatch(
        cx,
        handle,
        DisconnectTerminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
    );
    in_frame(cx, handle, |window, cx| {
        let staging = window.find(("compose-choose-remote", INITIAL_STAGING_TERMINAL));
        assert_eq!(staging.checked(), Some(false));
        // It cannot be checked again while it is not connected.
        window.click(("compose-choose-remote", INITIAL_STAGING_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, _| {
        let staging = window.find(("compose-choose-remote", INITIAL_STAGING_TERMINAL));
        assert_eq!(staging.checked(), Some(false));
    });
}

#[gpui_kit::test]
async fn a_terminal_that_cannot_take_the_command_says_so(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    open_local(cx, &fixture).await;
    dispatch(
        cx,
        handle,
        shellrs::app::SetComposeTerminals(vec![
            WEB,
            PaneContent::LocalTerminal(LocalTerminalId(1)),
        ]),
    );
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
async fn shared_keys_reach_every_terminal_it_sends_to_in_its_own_mode(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    send_to_the_tab(cx, &fixture);
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
    // Back to the current terminal ends the sharing too.
    dispatch(cx, handle, SetSendTarget(SendTarget::Current));
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
async fn a_reconnected_terminal_is_no_longer_chosen(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    dispatch(
        cx,
        handle,
        shellrs::app::SetComposeTerminals(vec![WEB, STAGING]),
    );
    assert_eq!(target(cx, handle).as_deref(), Some("选定的 2 个终端"));
    dispatch(
        cx,
        handle,
        ReconnectTerminal(RemoteTerminalId(INITIAL_STAGING_TERMINAL)),
    );
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
    assert_eq!(target(cx, handle).as_deref(), Some("选定的 1 个终端"));
    // The last one closed: back to the current terminal, the pane still
    // there.
    in_frame(cx, handle, |window, cx| {
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx)
    });
    assert_eq!(target(cx, handle).as_deref(), Some("staging-api"));
}

#[gpui_kit::test]
async fn zooming_and_moving_panes_keep_the_connections(cx: &mut TestAppContext) {
    let fixture = open(cx).await;
    let handle = fixture.handle;
    send_to_the_tab(cx, &fixture);
    in_frame(cx, handle, |window, cx| window.click("broadcast-keys", cx));
    // web-01's pane zoomed: staging-api's is off screen, still in the tab,
    // and still gets the keys.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal", INITIAL_WEB_TERMINAL), cx);
        window.dispatch_action(Box::new(ToggleZoom), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .try_find(("terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        window.input("zoomed", cx);
        window.dispatch_action(Box::new(ToggleZoom), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        fixture.staging.written_text() == "zoomed"
    })
    .await;
    // staging-api's pane out to a tab of its own: no longer in the tab
    // the pane sends to, still connected.
    in_frame(cx, handle, |window, cx| {
        window.drag_to(
            ("terminal-tab", INITIAL_STAGING_TERMINAL),
            "tab-bar-empty-space",
            cx,
        )
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    assert_eq!(target(cx, handle).as_deref(), Some("当前标签的 1 个终端"));
    assert_eq!(fixture.web.starts(), 1);
    assert_eq!(fixture.staging.starts(), 1);
}
