//! 文件: the right sidebar's single remote pane beside an SSH terminal, on an
//! SFTP channel of the terminal's own connection. It logs in nowhere, comes
//! and goes with its terminal, and leaves SFTP tabs as they are; it follows
//! the folder the terminal's shell reports (目录跟随).

use crate::support::*;
use shellrs::analytics::{Analytics, Counter, Signal};
use shellrs::app::{
    CloseTerminal, ConnectHost, DisconnectTerminal, ExplorerAction, ExplorerCommand,
    ReconnectTerminal,
};
use shellrs::explorer::ExplorerPanel;

/// The first terminal's 文件, the first explorer the workspace makes.
const FILES: u64 = 1;
const REMOTE_FILE: &str = "/home/tester/文件 甲.txt";

struct Fixture {
    handle: WindowHandle<Root>,
    workspace: Entity<Workspace>,
    terminals: Arc<FakeTerminalFactory>,
    sftp: Arc<FakeSftpProvider>,
    local: FakeLocalDirectory,
}

/// The seeded hosts, web-01 and staging-api with a terminal each, on fakes
/// the test can look into.
fn open(cx: &mut TestAppContext, sftp: FakeSftpProvider) -> Fixture {
    open_with(cx, sftp, FakeTerminalFactory::default())
}

fn open_with(
    cx: &mut TestAppContext,
    sftp: FakeSftpProvider,
    terminals: FakeTerminalFactory,
) -> Fixture {
    init_app(cx);
    cx.update(|cx| cx.set_reduce_motion(true));
    let terminals = Arc::new(terminals);
    let sftp = Arc::new(sftp);
    let local = FakeLocalDirectory::default();
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                cx.new(|_| HostStore::seed()),
                cx.new(|_| settings_store()),
                Arc::new(FixedRemoteTerminalTransportProvider::new(terminals.clone())),
                fake_telnet_terminals(),
                fake_serial_terminals(),
                fake_local_terminals(),
                sftp.clone(),
                Arc::new(local.clone()),
                Arc::new(FakeConnectionTester::default()),
                Arc::new(FakeForwardProvider::default()),
                window,
                cx,
            )
        });
        workspace = Some(view.clone());
        Root::new(view, window, cx)
    });
    Fixture {
        handle,
        workspace: workspace.unwrap(),
        terminals,
        sftp,
        local,
    }
}

/// Bring a terminal forward and open 文件 beside it.
async fn show_files(cx: &mut TestAppContext, fixture: &Fixture, terminal: u64, explorer: u64) {
    in_frame(cx, fixture.handle, |window, cx| {
        window.activate_window();
        window.click(("terminal-tab", terminal), cx);
    });
    in_frame(cx, fixture.handle, |window, cx| {
        if window
            .try_find(("remote-pane", explorer))
            .is_none_or(|pane| !pane.visible())
        {
            window.click("tool-files", cx);
        }
    });
    wait_for_path(cx, fixture.handle, explorer, "/home/tester").await;
}

async fn wait_for_path(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    explorer: u64,
    path: &str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", explorer))
            .try_find("remote-path")
            .is_some_and(|label| label.value() == Some(path))
    })
    .await;
}

fn sidebar(fixture: &Fixture, terminal: u64, cx: &App) -> Option<Entity<ExplorerPanel>> {
    fixture
        .workspace
        .read(cx)
        .file_sidebar(RemoteTerminalId(terminal))
        .cloned()
}

fn state(fixture: &Fixture, cx: &mut TestAppContext) -> Option<ConnectionState> {
    cx.update(|cx| {
        sidebar(fixture, INITIAL_WEB_TERMINAL, cx).map(|panel| panel.read(cx).connection_state())
    })
}

async fn wait_for_state(cx: &mut TestAppContext, fixture: &Fixture, wanted: ConnectionState) {
    for _ in 0..100 {
        if state(fixture, cx) == Some(wanted) {
            return;
        }
        cx.executor().advance_clock(Duration::from_millis(20));
        cx.run_until_parked();
    }
    panic!("文件 never got {wanted:?}: {:?}", state(fixture, cx));
}

async fn wait_for(cx: &mut TestAppContext, handle: WindowHandle<Root>, id: &'static str) {
    cx.wait_for(handle.into(), Duration::from_secs(2), move |window, cx| {
        window.render_frame(cx);
        window.try_find(id).is_some()
    })
    .await;
}

#[gpui_kit::test]
async fn files_open_on_the_terminals_own_connection(cx: &mut TestAppContext) {
    let fixture = open(cx, FakeSftpProvider::default());
    // Nothing until it is first shown.
    assert_eq!(*fixture.sftp.on_terminal.lock().unwrap(), 0);
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;

    // A channel of the terminal's connection, no login of its own.
    assert_eq!(*fixture.sftp.on_terminal.lock().unwrap(), 1);
    assert_eq!(fixture.terminals.sftp_opens(), [1]);
    in_frame(cx, fixture.handle, |window, _| {
        assert!(window.try_find("prompt-dialog").is_none());
        // The remote pane alone, in the sidebar, not a tab.
        let sidebar = window.within(("tool-sidebar", INITIAL_WEB_TERMINAL));
        assert!(sidebar.try_find(("remote-pane", FILES)).is_some());
        assert!(window.try_find(("local-pane", FILES)).is_none());
        assert!(window.try_find(("explorer-tab", FILES)).is_none());
        let pane = window.within(("remote-pane", FILES));
        // Narrow: no 目录列表 or 查找, and an upload button of its own.
        assert!(pane.try_find("path-select").is_none());
        assert!(pane.try_find("find-files").is_none());
        assert!(pane.try_find("choose-files").is_some());
    });
    cx.update(|cx| {
        let panel = sidebar(&fixture, INITIAL_WEB_TERMINAL, cx).unwrap();
        let panel = panel.read(cx);
        assert!(panel.is_sidebar());
        assert_eq!(
            panel.remote().read(cx).column_names(cx),
            ["名称", "大小", "修改时间"]
        );
        // Downloads go to Downloads, and the folder is never listed.
        assert_eq!(panel.local().read(cx).path(), "/local/tester/Downloads");
    });
    assert!(fixture.local.listed.lock().unwrap().is_empty());

    // Hidden and shown again, it is the same one.
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("tool-files", cx)
    });
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("tool-files", cx)
    });
    wait_for_path(cx, fixture.handle, FILES, "/home/tester").await;
    assert_eq!(*fixture.sftp.on_terminal.lock().unwrap(), 1);
    assert_eq!(fixture.terminals.sftp_opens(), [1]);
}

/// 修改时间 is past the sidebar's right edge as it opens: the list scrolls
/// sideways to it, and a click on its title sorts by it as in an SFTP tab,
/// newest first, then oldest first, then back to by name.
#[gpui_kit::test]
async fn files_scroll_sideways_to_the_times_and_sort_by_them(cx: &mut TestAppContext) {
    use gpui_kit::ScrollDelta;
    let fixture = open(cx, FakeSftpProvider::default());
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    let names = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        cx.update(|cx| {
            let panel = sidebar(&fixture, INITIAL_WEB_TERMINAL, cx).unwrap();
            let entries = panel.read(cx).remote().read(cx).entries(cx);
            entries
                .into_iter()
                .map(|entry| entry.name.to_string())
                .collect::<Vec<_>>()
        })
    };
    let by_name = ["..", "目录", "链接目录", "文件 乙.txt", "文件 甲.txt"];
    assert_eq!(names(cx), by_name);

    // The title is there, cut off by the sidebar's edge.
    let (before, edge) = in_frame(cx, fixture.handle, |window, _| {
        let edge = window.find(("remote-pane", FILES)).bounds().right();
        let title = window
            .within(("remote-pane", FILES))
            .find("column:modified");
        (title.bounds(), edge)
    });
    assert!(before.right() > edge, "{before:?} ends within {edge:?}");

    in_frame(cx, fixture.handle, |window, cx| {
        window.within(("remote-pane", FILES)).scroll(
            "column:size",
            ScrollDelta::Pixels(point(px(-400.), px(0.))),
            cx,
        );
    });
    let after = in_frame(cx, fixture.handle, |window, _| {
        window
            .within(("remote-pane", FILES))
            .find("column:modified")
            .bounds()
    });
    assert!(after.left() < before.left(), "{after:?} did not move");
    assert!(after.right() <= edge, "{after:?} is still past {edge:?}");

    let click_modified = |cx: &mut TestAppContext| {
        in_frame(cx, fixture.handle, |window, cx| {
            window
                .within(("remote-pane", FILES))
                .click("column:modified", cx);
        });
    };
    // Folders stay ahead of files whichever way.
    click_modified(cx);
    assert_eq!(
        names(cx),
        ["..", "链接目录", "目录", "文件 乙.txt", "文件 甲.txt"]
    );
    click_modified(cx);
    assert_eq!(
        names(cx),
        ["..", "目录", "链接目录", "文件 甲.txt", "文件 乙.txt"]
    );
    click_modified(cx);
    assert_eq!(names(cx), by_name);
}

#[gpui_kit::test]
async fn each_terminal_has_files_of_its_own(cx: &mut TestAppContext) {
    let fixture = open(cx, FakeSftpProvider::default());
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(FILES),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/srv".into(),
                },
            )),
            cx,
        )
    });
    wait_for_path(cx, fixture.handle, FILES, "/srv").await;

    // The other terminal's, made once 文件 is opened there too.
    show_files(cx, &fixture, INITIAL_STAGING_TERMINAL, FILES + 1).await;
    in_frame(cx, fixture.handle, |window, _| {
        assert!(window.try_find(("remote-pane", FILES)).is_none());
    });
    assert_eq!(*fixture.sftp.on_terminal.lock().unwrap(), 2);

    // Back where it was.
    in_frame(cx, fixture.handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    wait_for_path(cx, fixture.handle, FILES, "/srv").await;
    assert_eq!(*fixture.sftp.on_terminal.lock().unwrap(), 2);
}

#[gpui_kit::test]
async fn files_go_down_and_come_back_with_their_terminal(cx: &mut TestAppContext) {
    let fixture = open(cx, FakeSftpProvider::default());
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;

    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(
            Box::new(DisconnectTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        )
    });
    wait_for_state(cx, &fixture, ConnectionState::Disconnected).await;
    // The host goes by its tabs, not by 文件.
    cx.update(|cx| {
        assert!(
            !fixture
                .workspace
                .read(cx)
                .has_connection(HostId(WEB_01), cx)
        );
    });

    // On the terminal's next connection.
    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(
            Box::new(ReconnectTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        )
    });
    wait_for_state(cx, &fixture, ConnectionState::Connected).await;
    // Runs count across terminals: staging-api's was the second.
    assert_eq!(fixture.terminals.sftp_opens(), [1, 3]);
    assert_eq!(*fixture.sftp.on_terminal.lock().unwrap(), 1);
}

#[gpui_kit::test]
async fn closing_a_terminal_whose_files_download_asks_first(cx: &mut TestAppContext) {
    let fixture = open(cx, FakeSftpProvider::default());
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    in_frame(cx, fixture.handle, |window, cx| {
        window
            .within(("remote-pane", FILES))
            .click("name:文件 甲.txt", cx);
    });
    in_frame(cx, fixture.handle, |window, cx| window.press("f5", cx));
    in_frame(cx, fixture.handle, |window, cx| {
        assert_eq!(
            window.find("download-target").value(),
            Some("/local/tester/Downloads")
        );
        window.click("download-confirm", cx);
    });
    cx.wait_for(
        fixture.handle.into(),
        Duration::from_secs(2),
        |window, cx| {
            window.render_frame(cx);
            window.try_find("transfer-status").is_some()
        },
    )
    .await;
    // The narrow queue: a batch on lines of its own, no column titles.
    in_frame(cx, fixture.handle, |window, _| {
        let edge = window
            .find(("tool-sidebar", INITIAL_WEB_TERMINAL))
            .bounds()
            .right();
        assert!(window.find(("queue-entry", 1u64)).bounds().right() <= edge);
        assert!(window.find("choose-files").bounds().right() <= edge);
    });

    let close = |cx: &mut TestAppContext| {
        in_frame(cx, fixture.handle, |window, cx| {
            window.dispatch_action(
                Box::new(CloseTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
                cx,
            )
        })
    };
    close(cx);
    wait_for(cx, fixture.handle, "cancel").await;
    in_frame(cx, fixture.handle, |window, cx| window.click("cancel", cx));
    cx.update(|cx| {
        let workspace = fixture.workspace.read(cx);
        assert!(
            workspace
                .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
                .is_some()
        );
        assert!(sidebar(&fixture, INITIAL_WEB_TERMINAL, cx).is_some());
    });

    close(cx);
    wait_for(cx, fixture.handle, "ok").await;
    in_frame(cx, fixture.handle, |window, cx| window.click("ok", cx));
    cx.update(|cx| {
        let workspace = fixture.workspace.read(cx);
        assert!(
            workspace
                .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(sidebar(&fixture, INITIAL_WEB_TERMINAL, cx).is_none());
        assert!(workspace.explorer(ExplorerId(FILES)).is_none());
    });
}

#[gpui_kit::test]
async fn closing_a_terminal_asks_once_about_files_its_files_left_unsaved(cx: &mut TestAppContext) {
    let fixture = open(
        cx,
        FakeSftpProvider::with_files(&[(REMOTE_FILE, b"text\n")]),
    );
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    in_frame(cx, fixture.handle, |window, cx| {
        window
            .within(("remote-pane", FILES))
            .double_click("name:文件 甲.txt", cx)
    });
    let editor = ("editor", 1u64);
    cx.wait_for(
        fixture.handle.into(),
        Duration::from_secs(2),
        move |window, cx| {
            window.render_frame(cx);
            window.try_find(editor).is_some()
        },
    )
    .await;
    in_frame(cx, fixture.handle, |window, cx| window.input("1", cx));

    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(
            Box::new(CloseTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        )
    });
    wait_for(cx, fixture.handle, "ok").await;
    in_frame(cx, fixture.handle, |window, cx| window.click("ok", cx));
    cx.update(|cx| {
        let workspace = fixture.workspace.read(cx);
        assert!(
            workspace
                .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(workspace.editor(shellrs::editor::EditorId(1)).is_none());
    });
    assert!(fixture.sftp.writes.lock().unwrap().is_empty());
}

#[gpui_kit::test]
async fn files_dropped_on_files_are_offered_for_upload(cx: &mut TestAppContext) {
    let fixture = open(cx, FakeSftpProvider::default());
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    in_frame(cx, fixture.handle, |window, cx| {
        // Empty space under the rows, above the status line.
        let pane = window.find(("remote-pane", FILES)).bounds();
        let position = gpui_kit::point(pane.center().x, pane.bottom() - px(48.));
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
    });
    in_frame(cx, fixture.handle, |window, _| {
        assert!(window.find("upload-confirm").visible());
        assert_eq!(window.find("upload-target").value(), Some("/home/tester"));
    });
}

/// Its queue, as an SFTP tab's, scrolls down to a batch as it is added.
#[gpui_kit::test]
async fn files_queue_scrolls_to_a_batch_as_it_is_added(cx: &mut TestAppContext) {
    let fixture = open(cx, FakeSftpProvider::default());
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    const BATCHES: u64 = 12;
    for n in 1..=BATCHES {
        let paths = vec![std::path::PathBuf::from(format!("/local/tester/{n}.txt"))];
        in_frame(cx, fixture.handle, |window, cx| {
            window.dispatch_action(
                Box::new(ExplorerAction::new(
                    ExplorerId(FILES),
                    ExplorerCommand::UploadPaths {
                        paths,
                        target: "/home/tester".into(),
                    },
                )),
                cx,
            )
        });
        in_frame(cx, fixture.handle, |window, cx| {
            window.click("upload-confirm", cx)
        });
    }
    in_frame(cx, fixture.handle, |window, _| {
        let rows = window.find("transfer-queue-rows").bounds();
        let first = window.find(("queue-entry", 1u64)).bounds();
        let last = window.find(("queue-entry", BATCHES)).bounds();
        assert!(first.top() < rows.top(), "{first:?} in {rows:?}");
        assert!(
            last.top() >= rows.top() && last.bottom() <= rows.bottom() + px(0.5),
            "{last:?} in {rows:?}"
        );
    });
}

#[gpui_kit::test]
async fn files_count_as_a_tool_not_as_an_sftp_connection(cx: &mut TestAppContext) {
    let fixture = open(cx, FakeSftpProvider::default());
    let (sender, signals) = mpsc::channel();
    cx.update_window(fixture.handle.into(), |_, window, cx| {
        fixture.workspace.update(cx, |workspace, cx| {
            workspace.set_analytics(Analytics::new(sender, true), window, cx)
        });
    })
    .unwrap();
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    // ⌘F there finds nothing, so nothing is counted for it either.
    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(FILES),
                ExplorerCommand::FindFiles,
            )),
            cx,
        )
    });
    // The seeded terminals' own connections aside.
    let counted: Vec<Counter> = signals
        .try_iter()
        .filter_map(|signal| match signal {
            Signal::Count(counter) if counter != Counter::Ssh => Some(counter),
            _ => None,
        })
        .collect();
    assert_eq!(counted, [Counter::ToolFiles]);
}

/// What 目录跟随's check finds on a bash host whose ~/.bashrc does not
/// report the folder yet, and on one whose shell does.
const NOT_SET_UP: &str = "@@shell bash\n@@host web-01\n@@file .bashrc\n@@configured no\n";
const SET_UP: &str = "@@shell bash\n@@host web-01\n@@file .bashrc\n@@configured yes\n";

/// The first terminal's shell says it is in `folder`, as a prompt with the
/// setup would: the fake shell prints back what it is sent, and a paste
/// outside bracketed paste mode sends it as it is.
fn report(cx: &mut TestAppContext, fixture: &Fixture, sequence: &str) {
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(sequence.to_string()));
        let terminal = fixture
            .workspace
            .read(cx)
            .remote_terminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))
            .expect("the terminal is open")
            .read(cx)
            .terminal()
            .clone();
        terminal.update(cx, |terminal, cx| terminal.paste_clipboard(cx));
    });
    cx.run_until_parked();
}

fn osc7(host: &str, path: &str) -> String {
    format!("\x1b]7;file://{host}{path}\x07")
}

fn path(fixture: &Fixture, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        sidebar(fixture, INITIAL_WEB_TERMINAL, cx)
            .unwrap()
            .read(cx)
            .remote()
            .read(cx)
            .path()
    })
}

async fn wait_for_ask(cx: &mut TestAppContext, fixture: &Fixture) {
    cx.wait_for(
        fixture.handle.into(),
        Duration::from_secs(2),
        |window, cx| {
            window.render_frame(cx);
            window
                .try_find("ok")
                .is_some_and(|button| button.label() == Some("自动配置"))
        },
    )
    .await;
}

/// Let the check's answer come back, and whatever it starts settle.
fn settle(cx: &mut TestAppContext) {
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_millis(60));
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
async fn files_asks_once_per_connection_to_set_up_folder_reporting(cx: &mut TestAppContext) {
    let fixture = open_with(
        cx,
        FakeSftpProvider::default(),
        FakeTerminalFactory::answering(&[NOT_SET_UP]),
    );
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    wait_for_ask(cx, &fixture).await;
    assert!(fixture.terminals.exec_commands()[0].contains("@@configured"));
    in_frame(cx, fixture.handle, |window, cx| window.click("cancel", cx));

    // Not again on this connection, shown again or not.
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("tool-files", cx)
    });
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("tool-files", cx)
    });
    settle(cx);
    in_frame(cx, fixture.handle, |window, _| {
        assert!(window.try_find("ok").is_none());
    });
    assert_eq!(fixture.terminals.exec_count(), 1);

    // The next connection is asked about again.
    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(
            Box::new(ReconnectTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        )
    });
    wait_for_ask(cx, &fixture).await;
    assert_eq!(fixture.terminals.exec_count(), 2);
}

#[gpui_kit::test]
async fn setting_up_appends_to_the_startup_file_and_sources_it(cx: &mut TestAppContext) {
    let fixture = open_with(
        cx,
        FakeSftpProvider::default(),
        FakeTerminalFactory::answering(&[NOT_SET_UP, "@@installed\n"]),
    );
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    wait_for_ask(cx, &fixture).await;
    in_frame(cx, fixture.handle, |window, cx| window.click("ok", cx));
    settle(cx);
    let commands = fixture.terminals.exec_commands();
    assert_eq!(commands.len(), 2);
    assert!(commands[1].contains(" sh .bashrc '"), "{}", commands[1]);
    // Read again by the shell there, which reports from then on.
    assert!(
        fixture
            .terminals
            .written_text()
            .contains(" source ~/.bashrc\r")
    );
    report(cx, &fixture, &osc7("web-01", "/srv/app"));
    wait_for_path(cx, fixture.handle, FILES, "/srv/app").await;
}

#[gpui_kit::test]
async fn a_terminal_in_a_full_screen_program_is_not_typed_into(cx: &mut TestAppContext) {
    let fixture = open_with(
        cx,
        FakeSftpProvider::default(),
        FakeTerminalFactory::answering(&[NOT_SET_UP, "@@installed\n"]),
    );
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    wait_for_ask(cx, &fixture).await;
    report(cx, &fixture, "\x1b[?1049h");
    in_frame(cx, fixture.handle, |window, cx| window.click("ok", cx));
    settle(cx);
    assert_eq!(fixture.terminals.exec_count(), 2);
    assert!(!fixture.terminals.written_text().contains("source"));
}

#[gpui_kit::test]
async fn files_follows_its_hosts_folder_only_when_it_changes(cx: &mut TestAppContext) {
    let fixture = open_with(
        cx,
        FakeSftpProvider::default(),
        FakeTerminalFactory::answering(&[SET_UP]),
    );
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    settle(cx);
    // Its shell reports already: nothing to ask.
    in_frame(cx, fixture.handle, |window, _| {
        assert!(window.try_find("ok").is_none());
        assert_eq!(window.find("follow-terminal").checked(), Some(true));
    });
    report(cx, &fixture, &osc7("web-01", "/srv"));
    wait_for_path(cx, fixture.handle, FILES, "/srv").await;

    // Browsing elsewhere stays put through prompts in the same folder.
    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(FILES),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/tmp".into(),
                },
            )),
            cx,
        )
    });
    wait_for_path(cx, fixture.handle, FILES, "/tmp").await;
    report(cx, &fixture, &osc7("web-01", "/srv"));
    settle(cx);
    assert_eq!(path(&fixture, cx), "/tmp");
    // The host's name in another case is still this host.
    report(cx, &fixture, &osc7("WEB-01", "/var/log"));
    wait_for_path(cx, fixture.handle, FILES, "/var/log").await;

    // A host ssh'd to inside the terminal is not this one.
    report(cx, &fixture, &osc7("db-01", "/etc"));
    settle(cx);
    assert_eq!(path(&fixture, cx), "/var/log");
    // iTerm2's report names no host.
    report(cx, &fixture, "\x1b]1337;CurrentDir=/opt\x07");
    wait_for_path(cx, fixture.handle, FILES, "/opt").await;

    // Switched off, it stays where it is.
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("follow-terminal", cx)
    });
    report(cx, &fixture, &osc7("web-01", "/home"));
    settle(cx);
    assert_eq!(path(&fixture, cx), "/opt");
    in_frame(cx, fixture.handle, |window, _| {
        assert_eq!(window.find("follow-terminal").checked(), Some(false));
    });
}

/// 目录跟随 as switched in one terminal's 文件 is where the host's next 文件
/// starts, kept on the host for the next launch; a host never switched
/// starts as the setting says.
#[gpui_kit::test]
async fn the_follow_switch_is_kept_for_the_host(cx: &mut TestAppContext) {
    let fixture = open_with(
        cx,
        FakeSftpProvider::default(),
        FakeTerminalFactory::answering(&[SET_UP, SET_UP, SET_UP]),
    );
    let store = cx.update(|cx| fixture.workspace.read(cx).store().clone());
    let remembered = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            store
                .read(cx)
                .host(HostId(WEB_01))
                .unwrap()
                .directory_follow
        })
    };
    let follows = |cx: &mut TestAppContext, terminal| {
        cx.update(|cx| sidebar(&fixture, terminal, cx).map(|panel| panel.read(cx).follows()))
    };
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    settle(cx);
    assert_eq!(remembered(cx), None);
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("follow-terminal", cx)
    });
    assert_eq!(remembered(cx), Some(false));

    // Another terminal on web-01 has 文件 beside it at once, switched off.
    in_frame(cx, fixture.handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx)
    });
    settle(cx);
    assert_eq!(follows(cx, FIRST_NEW_TERMINAL), Some(false));
    in_frame(cx, fixture.handle, |window, _| {
        assert_eq!(window.find("follow-terminal").checked(), Some(false));
    });

    // staging-api was never switched: on, as the setting says.
    in_frame(cx, fixture.handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx);
    });
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("tool-files", cx)
    });
    settle(cx);
    assert_eq!(follows(cx, INITIAL_STAGING_TERMINAL), Some(true));

    // Switched on again in the newer one: the first keeps its own.
    in_frame(cx, fixture.handle, |window, cx| {
        window.click(("terminal-tab", FIRST_NEW_TERMINAL), cx);
    });
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("follow-terminal", cx)
    });
    assert_eq!(remembered(cx), Some(true));
    assert_eq!(follows(cx, INITIAL_WEB_TERMINAL), Some(false));
}

#[gpui_kit::test]
async fn files_opens_where_the_terminal_is(cx: &mut TestAppContext) {
    let terminals = FakeTerminalFactory {
        behavior: FakeBehavior::Prints("\x1b]7;file://web-01/srv/app\x07"),
        ..FakeTerminalFactory::answering(&[SET_UP])
    };
    let fixture = open_with(cx, FakeSftpProvider::default(), terminals);
    in_frame(cx, fixture.handle, |window, cx| {
        window.activate_window();
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx);
    });
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("tool-files", cx)
    });
    wait_for_path(cx, fixture.handle, FILES, "/srv/app").await;
}

#[gpui_kit::test]
async fn with_the_setting_off_files_neither_asks_nor_follows_until_switched_on(
    cx: &mut TestAppContext,
) {
    let fixture = open_with(
        cx,
        FakeSftpProvider::default(),
        FakeTerminalFactory::answering(&[NOT_SET_UP]),
    );
    let settings = cx.update(|cx| fixture.workspace.read(cx).settings().clone());
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| settings.file_sidebar.detect_directory_follow = false,
                cx,
            )
        })
    });
    show_files(cx, &fixture, INITIAL_WEB_TERMINAL, FILES).await;
    report(cx, &fixture, &osc7("web-01", "/srv"));
    settle(cx);
    assert_eq!(fixture.terminals.exec_count(), 0);
    assert_eq!(path(&fixture, cx), "/home/tester");
    in_frame(cx, fixture.handle, |window, _| {
        assert_eq!(window.find("follow-terminal").checked(), Some(false));
    });

    // Switched on: the host is looked at, without asking, and followed.
    in_frame(cx, fixture.handle, |window, cx| {
        window.click("follow-terminal", cx)
    });
    wait_for_path(cx, fixture.handle, FILES, "/srv").await;
    assert_eq!(fixture.terminals.exec_count(), 1);
    in_frame(cx, fixture.handle, |window, _| {
        assert!(window.try_find("ok").is_none());
    });
}
