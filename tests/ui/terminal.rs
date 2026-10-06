//! Terminals: local terminals, keys and input, selection, find, clearing,
//! the font and the size, links, the mouse for programs and their copies.

use crate::support::*;

fn open_workspace_with_factory(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    cx.update(shellrs::init);
    // Dialogs still, as in every fixture: see `open_workspace_with_tester`.
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| HostStore::seed());
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
        window.click("host-search", cx);
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
async fn the_status_bar_shows_the_size_of_the_terminal_in_front(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let terminal = RemoteTerminalId(FIRST_NEW_TERMINAL);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectHost(HostId(WEB_01))), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        remote_lifecycle(&workspace, terminal, cx) == TerminalLifecycle::Running
    })
    .await;
    let columns = |cx: &App| {
        workspace
            .read(cx)
            .remote_terminal(terminal)
            .expect("terminal exists")
            .read(cx)
            .status(cx)
            .columns()
    };
    // Whether the status bar shows the terminal's screen as it is now.
    let shows_its_size = |window: &mut gpui_kit::Window, cx: &mut App| {
        let status = workspace
            .read(cx)
            .remote_terminal(terminal)
            .expect("terminal exists")
            .read(cx)
            .status(cx);
        let size = format!("{}×{}", status.columns(), status.rows());
        window
            .try_find("status-terminal-size")
            .is_some_and(|element| element.label() == Some(size.as_str()))
    };
    // Laid out in the window, not the 80 columns it starts with.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        columns(cx) > 80 && shows_its_size(window, cx)
    })
    .await;
    let wide = cx.update(|cx| columns(cx));

    // A smaller window, a smaller terminal, and the status bar follows.
    cx.simulate_window_resize(handle.into(), size(px(900.), px(600.)));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        columns(cx) < wide && shows_its_size(window, cx)
    })
    .await;

    // An SFTP tab has no terminal, so no size either.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(HostId(WEB_01))), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find(("explorer", SFTP_TAB))
            .is_some_and(|explorer| explorer.visible())
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("status-connection").visible());
        assert!(window.try_find("status-terminal-size").is_none());
        window.click("new-local-terminal", cx);
    });

    // A local terminal shows its own.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        let Some(local) = workspace.read(cx).local_terminal(LocalTerminalId(1)) else {
            return false;
        };
        let status = local.read(cx).status(cx);
        let size = format!("{}×{}", status.columns(), status.rows());
        // Laid out, so no longer the 80×24 it starts with.
        size != "80×24"
            && window
                .try_find("status-terminal-size")
                .is_some_and(|element| element.label() == Some(size.as_str()))
    })
    .await;
}

/// Where the cell `(column, row)` of the first local terminal is, by the cell
/// size the fake shell was told of.
fn cell_center(
    window: &mut gpui_kit::Window,
    factory: &FakeTerminalFactory,
    column: usize,
    row: usize,
) -> gpui_kit::Point<gpui_kit::Pixels> {
    let size = *factory
        .resizes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .last()
        .expect("the terminal has been laid out");
    let bounds = window.find(("local-terminal", 1_u64)).bounds();
    point(
        bounds.left() + px(size.cell_width() as f32 * (column as f32 + 0.5)),
        bounds.top() + px(size.cell_height() as f32 * (row as f32 + 0.5)),
    )
}

/// A local terminal whose shell has printed `text`, which ends in `marker`,
/// laid out.
async fn terminal_printing(
    cx: &mut TestAppContext,
    text: &'static str,
    marker: &'static str,
) -> (
    WindowHandle<Root>,
    Entity<Workspace>,
    Arc<FakeTerminalFactory>,
) {
    let factory = Arc::new(FakeTerminalFactory::printing(text));
    let (handle, workspace) = open_running_local_terminal(cx, factory.clone()).await;
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        local_screen(&workspace, cx).contains(marker) && last_cell_height(&factory).is_some()
    })
    .await;
    (handle, workspace, factory)
}

#[gpui_kit::test]
async fn links_open_with_the_command_key_and_show_where_they_lead(cx: &mut TestAppContext) {
    // The prompt line is row 0; the link starts row 1.
    let (handle, _, factory) =
        terminal_printing(cx, "https://example.com/docs. 文档\r\n", "文档").await;

    // A plain click selects, as it always has, and opens nothing.
    in_frame(cx, handle, |window, cx| {
        let link = cell_center(window, &factory, 2, 1);
        click_at(window, link, gpui_kit::Modifiers::none(), cx);
    });
    assert_eq!(cx.opened_url(), None);

    // Resting on it shows where it leads, without the full stop.
    cx.background_executor
        .advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("terminal-link-tooltip").label(),
            Some("https://example.com/docs")
        );
        let link = cell_center(window, &factory, 2, 1);
        click_at(window, link, gpui_kit::Modifiers::secondary_key(), cx);
    });
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/docs"));
}

/// Wait until the fake shell has been sent `expected`, which it records on
/// its own thread, and forget it.
async fn take_written(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    factory: &FakeTerminalFactory,
    expected: &str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == expected
    })
    .await;
    factory
        .writes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clear();
}

#[gpui_kit::test]
async fn programs_that_ask_for_the_mouse_hear_clicks_and_the_wheel(cx: &mut TestAppContext) {
    // 1000: clicks, 1006: SGR encoding.
    let (handle, _, factory) =
        terminal_printing(cx, "\x1b[?1000h\x1b[?1006hmouse on", "mouse on").await;
    in_frame(cx, handle, |window, cx| {
        let cell = cell_center(window, &factory, 2, 0);
        click_at(window, cell, gpui_kit::Modifiers::none(), cx);
    });
    take_written(cx, handle, &factory, "\x1b[<0;3;1M\x1b[<0;3;1m").await;

    // Shift keeps the mouse for selecting text: the program hears only the
    // wheel after it.
    in_frame(cx, handle, |window, cx| {
        let cell = cell_center(window, &factory, 2, 0);
        click_at(window, cell, gpui_kit::Modifiers::shift(), cx);
        let position = cell_center(window, &factory, 4, 1);
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Lines(point(0., 1.)),
                modifiers: gpui_kit::Modifiers::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    take_written(cx, handle, &factory, "\x1b[<64;5;2M").await;
}

#[gpui_kit::test]
async fn a_program_copies_to_the_local_clipboard(cx: &mut TestAppContext) {
    // 「你好」 in base64.
    let (handle, _, _) = terminal_printing(cx, "\x1b]52;c;5L2g5aW9\x07copied", "copied").await;
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        cx.read_from_clipboard().and_then(|item| item.text()) == Some("你好".into())
    })
    .await;
}
