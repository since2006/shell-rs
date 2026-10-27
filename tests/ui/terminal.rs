//! Terminals: local terminals, keys and input, selection, find, clearing,
//! the font and the size, links, the mouse for programs and their copies.

use crate::support::*;
use shellrs::app::{CopyTerminal, PasteTerminal, SelectAllTerminal, Shortcut};
use shellrs::terminal::{RightClick, SendDelay};

fn open_workspace_with_factory(
    cx: &mut TestAppContext,
    factory: Arc<FakeTerminalFactory>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    open_workspace_with_local_terminals(
        cx,
        Arc::new(FixedLocalTerminalTransportProvider::new(factory)),
        size(px(1280.), px(800.)),
    )
}

/// With `local` asked for each local terminal's start.
fn open_workspace_with_local_terminals(
    cx: &mut TestAppContext,
    local: Arc<FixedLocalTerminalTransportProvider>,
    window_size: gpui_kit::Size<gpui_kit::Pixels>,
) -> (WindowHandle<Root>, Entity<Workspace>) {
    init_app(cx);
    // Dialogs still, as in every fixture: see `open_workspace_with_tester`.
    cx.update(|cx| cx.set_reduce_motion(true));
    let mut workspace = None;
    let handle = cx.open_window(window_size, |window, cx| {
        let store = cx.new(|_| HostStore::seed());
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| settings_store()),
                remote,
                fake_telnet_terminals(),
                fake_serial_terminals(),
                local.clone(),
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
        // Elsewhere Ctrl+T is the shell's in a terminal.
        #[cfg(target_os = "macos")]
        window.press("cmd-t", cx);
        #[cfg(not(target_os = "macos"))]
        window.click("new-local-terminal", cx);
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
async fn ctrl_with_a_letter_is_the_shells_in_a_terminal(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_factory(cx, factory.clone());
    // 新建本地终端 as on Windows and Linux.
    cx.update(|cx| {
        let settings = workspace.read(cx).settings().clone();
        settings.update(cx, |store, cx| {
            store.update(
                |settings| {
                    settings.shortcuts.set(
                        Shortcut::find("new-local-terminal").unwrap(),
                        "ctrl-t".into(),
                    )
                },
                cx,
            )
        });
    });
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

    // In the terminal it goes to the shell, as tmux's Ctrl+B would.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("local-terminal", 1_u64)).focused(), Some(true));
        window.press("ctrl-t", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text() == "\x14"
    })
    .await;
    cx.run_until_parked();
    assert!(cx.update(|cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(2))
            .is_none()
    }));

    // Outside a terminal the shortcut works.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("open-settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("ctrl-t", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(cx.update(|cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(2))
            .is_some()
    }));
}

#[gpui_kit::test]
async fn local_terminals_run_the_shell_the_settings_name(cx: &mut TestAppContext) {
    let local = fake_local_terminals();
    // Tall enough for the whole of 终端, whose groups the page's list
    // draws only near the view.
    let (handle, workspace) =
        open_workspace_with_local_terminals(cx, local.clone(), size(px(1280.), px(2400.)));
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());

    // Until one is named, the system's default shell.
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    assert_eq!(local.shells(), [LocalShell::default()]);

    // 终端 › 本地终端, the page's last group: the field is empty, and what
    // is typed in it is the setting at once.
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-1", cx)
    });
    in_frame(cx, handle, |window, cx| {
        // The field runs across the row under its title, 选择文件… at its
        // end, inside the page.
        let field = window.find("local-shell").bounds();
        let choose = window.find("browse-local-shell").bounds();
        let page = window.find("settings").bounds();
        assert!(
            field.size.width > page.size.width / 2.,
            "{field:?} in {page:?}"
        );
        assert!(field.right() <= choose.left() && choose.right() <= page.right());
        assert_eq!(window.find("local-shell").value(), Some(""));
        window.click("local-shell", cx);
        window.input("/bin/bash -l ", cx);
    });
    let shell =
        |cx: &mut TestAppContext| cx.update(|cx| settings.read(cx).settings().local_terminal.shell);
    assert_eq!(shell(cx).as_deref(), Some("/bin/bash -l"));
    in_frame(cx, handle, |window, _| {
        // The blank typed last stays in the field, ready for what follows.
        assert_eq!(window.find("local-shell").value(), Some("/bin/bash -l "));
    });

    // The open terminal takes it when it restarts, a new one at once.
    in_frame(cx, handle, |window, cx| {
        window.click(("local-terminal-tab", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("restart-local-terminal-toolbar", 1_u64), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    let bash = LocalShell::new("/bin/bash -l");
    assert_eq!(
        local.shells(),
        [LocalShell::default(), bash.clone(), bash.clone()]
    );

    // Set back elsewhere (重置, the file read anew), the field follows.
    settings.update(cx, |store, cx| {
        store.update(|settings| settings.local_terminal.shell = None, cx)
    });
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("local-shell").value(), Some(""));
    });
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    assert_eq!(local.shells().last(), Some(&LocalShell::default()));
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
        window.click(("local-terminal", 1_u64), cx);
        window.press("enter", cx);
        assert_eq!(
            factory.starts(),
            1,
            "local shells do not opt into SSH reconnect"
        );
        assert!(!local_screen(&workspace, cx).contains("按 Enter 重新连接"));
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
async fn copy_on_select_puts_what_the_mouse_selects_on_the_clipboard(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory).await;
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        local_screen(&workspace, cx).contains("会议纪要")
    })
    .await;

    // 终端交互 is the second group of 终端: 选中复制, off, then 右键行为.
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-1", cx)
    });
    in_frame(cx, handle, |window, cx| {
        let mut settings = window.within("settings");
        let mut group = settings.within("group-1");
        assert_eq!(group.within("item-0").find("check").checked(), Some(false));
        assert_eq!(group.within("item-1").find("btn").label(), Some("显示菜单"));
        group.within("item-0").click("check", cx);
    });
    cx.update(|cx| {
        assert!(
            settings
                .read(cx)
                .settings()
                .terminal_interaction
                .copy_on_select
        )
    });
    in_frame(cx, handle, |window, cx| {
        window.click(("local-terminal-tab", 1_u64), cx)
    });

    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("原剪贴板".into())));
    in_frame(cx, handle, |window, cx| {
        let bounds = window.find(("local-terminal", 1_u64)).bounds();
        let from = point(bounds.left() + px(1.), bounds.top() + px(8.));
        let to = point(bounds.right() - px(1.), bounds.top() + px(8.));
        // A click selects nothing, and the clipboard keeps what it had.
        click_at(window, from, gpui_kit::Modifiers::none(), cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("原剪贴板".into())
        );
        window.drag(from, to, cx);
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("run:1$ alpha.txt 会议纪要.md".into())
        );
    });
}

/// 全选 takes the scrollback too, without the blank lines below the text.
/// It only selects; with 选中复制 on, selecting is copying.
#[gpui_kit::test]
async fn select_all_takes_the_scrollback_too(cx: &mut TestAppContext) {
    let lines: Vec<String> = (1..=80).map(|line| format!("第 {line} 行")).collect();
    let printed: &'static str = Box::leak(format!("{}\r\n", lines.join("\r\n")).into_boxed_str());
    let (handle, workspace, _) = terminal_printing(cx, printed, "第 80 行").await;
    assert!(
        !cx.update(|cx| local_screen(&workspace, cx))
            .contains("第 1 行")
    );
    let everything = format!("run:1$ alpha.txt 会议纪要.md\n{}", lines.join("\n"));
    let clipboard = |cx: &mut TestAppContext| {
        cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
    };
    let dispatch = |cx: &mut TestAppContext, action: Box<dyn gpui_kit::Action>| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.dispatch_action(action, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };

    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("原剪贴板".into())));
    dispatch(cx, Box::new(SelectAllTerminal));
    assert_eq!(clipboard(cx).as_deref(), Some("原剪贴板"));
    dispatch(cx, Box::new(CopyTerminal));
    assert_eq!(clipboard(cx), Some(everything.clone()));

    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| settings.terminal_interaction.copy_on_select = true,
                cx,
            )
        });
        cx.write_to_clipboard(ClipboardItem::new_string("原剪贴板".into()));
    });
    dispatch(cx, Box::new(SelectAllTerminal));
    assert_eq!(clipboard(cx), Some(everything));
}

/// Below 复制, a remote terminal's menu copies its host's address and ID.
#[gpui_kit::test]
async fn a_remote_terminals_menu_copies_the_host_address_and_id(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let public_id = workspace.read_with(cx, |workspace, cx| {
        let store = workspace.store().read(cx);
        store.host(HostId(WEB_01)).unwrap().public_id.to_string()
    });
    let pick = |cx: &mut TestAppContext, item: usize| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.right_click(("terminal", INITIAL_WEB_TERMINAL), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let mut popup = window.within("popup-menu");
            let labels: Vec<_> = (0..5_usize)
                .map(|item| popup.find(item).label().map(str::to_owned))
                .collect();
            assert_eq!(
                labels,
                ["复制", "复制 IP", "复制 ID", "粘贴", "全选"].map(|label| Some(label.to_owned()))
            );
            popup.click(item, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
    };

    assert_eq!(pick(cx, 1).as_deref(), Some("10.0.1.12"));
    assert_eq!(pick(cx, 2), Some(public_id));
}

#[gpui_kit::test]
async fn right_click_pastes_or_does_nothing_when_set_to(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory.clone()).await;
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    let right_click = |cx: &mut TestAppContext, action: RightClick, clipboard: &str| {
        cx.update(|cx| {
            settings.update(cx, |settings, cx| {
                settings.update(
                    |settings| settings.terminal_interaction.right_click = action,
                    cx,
                )
            });
            cx.write_to_clipboard(ClipboardItem::new_string(clipboard.into()));
        });
        in_frame(cx, handle, |window, cx| {
            window.right_click(("local-terminal", 1_u64), cx)
        });
        in_frame(cx, handle, |window, _| {
            assert!(window.try_find("popup-menu").is_none())
        });
    };

    right_click(cx, RightClick::Paste, "粘贴一次");
    take_written(cx, handle, &factory, "粘贴一次").await;

    // Nothing is pasted: had it been, the next paste would follow it.
    right_click(cx, RightClick::Nothing, "不该粘贴");
    right_click(cx, RightClick::Paste, "再粘贴");
    take_written(cx, handle, &factory, "再粘贴").await;
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

/// The interface language changes under an open find bar: the placeholder
/// gpui-kit keeps is given again, and what is drawn is drawn in English.
#[gpui_kit::test]
async fn an_open_find_bar_follows_the_interface_language(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory.clone()).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(FindInTerminal), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("terminal-find").label(), Some("查找"));
        window.input("zzz", cx);
    })
    .unwrap();
    wait_for_find_count(cx, handle, "无结果").await;

    workspace.update(cx, |workspace, cx| {
        workspace.settings().update(cx, |settings, cx| {
            settings.update(
                |settings| settings.language = InterfaceLanguage::English,
                cx,
            )
        });
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("terminal-find").label(), Some("Find"));
        assert_eq!(
            window.find(("local-terminal", 1_u64)).label(),
            Some("Local terminal 1")
        );
    })
    .unwrap();
    wait_for_find_count(cx, handle, "No results").await;
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

/// A program that asks for the background and then for the cursor, as
/// termenv does (ollama), hears the answers in that order: it takes a
/// cursor report coming first to mean no color is coming, and the color
/// that comes after would reach the shell as typed text.
#[gpui_kit::test]
async fn a_program_hears_its_queries_answered_in_the_order_asked(cx: &mut TestAppContext) {
    let (handle, _, factory) = terminal_printing(cx, "\x1b]11;?\x1b\\\x1b[6nasked", "asked").await;
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.written_text().contains('R')
    })
    .await;
    let written = factory.written_text();
    assert!(written.starts_with("\x1b]11;rgb:"), "{written:?}");
    assert!(written.ends_with("\x1b\\\x1b[2;1R"), "{written:?}");
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
async fn the_scrollbar_scrolls_back_through_the_history(cx: &mut TestAppContext) {
    let text: String = (1..=200).map(|line| format!("line {line}\r\n")).collect();
    let (handle, workspace, _) = terminal_printing(cx, String::leak(text + "end"), "end").await;
    let screen = |cx: &mut TestAppContext| cx.read(|cx| local_screen(&workspace, cx));
    assert!(!screen(cx).lines().any(|line| line == "line 1"));

    // A click at the top of the scrollbar's track, in the margin beside the
    // text, goes to the oldest line.
    in_frame(cx, handle, |window, cx| {
        let terminal = window.find(("local-terminal", 1_u64)).bounds();
        let top = point(terminal.right() + px(2.), terminal.top() + px(2.));
        click_at(window, top, gpui_kit::Modifiers::none(), cx);
    });
    let lines = screen(cx);
    assert!(lines.lines().any(|line| line == "line 1"), "{lines}");
    assert!(!lines.contains("end"), "{lines}");
}

/// A selection dragged past the bottom takes the view down with it, step
/// by step while the pointer stays out there, as in every editor; the wheel
/// turned while dragging does the same. It stops once let go.
#[gpui_kit::test]
async fn a_selection_dragged_past_the_edge_scrolls_on(cx: &mut TestAppContext) {
    use gpui_kit::{MouseDownEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent, TouchPhase};

    let text: String = (1..=200).map(|line| format!("line {line}\r\n")).collect();
    let (handle, workspace, _) = terminal_printing(cx, String::leak(text + "end"), "end").await;
    let screen = |cx: &mut TestAppContext| cx.read(|cx| local_screen(&workspace, cx));
    // The topmost numbered line on screen; the oldest line is the prompt.
    let first_line = |cx: &mut TestAppContext| {
        screen(cx).lines().find_map(|line| {
            line.strip_prefix("line ")
                .and_then(|number| number.trim().parse::<usize>().ok())
        })
    };

    // To the oldest line, then press at the start of it.
    let terminal = cx
        .update_window(handle.into(), |_, window, _| {
            window.find(("local-terminal", 1_u64)).bounds()
        })
        .unwrap();
    in_frame(cx, handle, |window, cx| {
        let top = point(terminal.right() + px(2.), terminal.top() + px(2.));
        click_at(window, top, gpui_kit::Modifiers::none(), cx);
    });
    assert_eq!(first_line(cx), Some(1));
    let start = point(terminal.left() + px(1.), terminal.top() + px(4.));
    let below = point(terminal.left() + px(40.), terminal.bottom() + px(30.));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position: start,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseMoveEvent {
                position: below,
                pressed_button: Some(MouseButton::Left),
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
    });

    // Held out there, it goes on scrolling down.
    for _ in 0..5 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    let scrolled = first_line(cx).expect("still in the history");
    assert!(scrolled > 5, "scrolled to line {scrolled}");

    // The wheel, still dragging, takes it further.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_event(
            ScrollWheelEvent {
                position: point(terminal.left() + px(40.), terminal.bottom() - px(4.)),
                delta: ScrollDelta::Lines(point(0., -5.)),
                modifiers: Default::default(),
                touch_phase: TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
    let wheeled = first_line(cx).expect("still in the history");
    assert!(wheeled > scrolled, "{wheeled} after {scrolled}");

    // Let go: what was selected runs from line 1 to the bottom of the view,
    // and nothing scrolls any more.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position: below,
                modifiers: Default::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_action(Box::new(CopyTerminal), cx);
    });
    let copied = cx
        .update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("a selection");
    assert!(copied.starts_with("run:1$"), "from the first row: {copied}");
    assert!(copied.contains("\nline 1\nline 2\n"), "{copied}");
    // Down to the bottom row, as far across as the pointer.
    let last = screen(cx).lines().last().unwrap_or_default().to_string();
    let end = copied.lines().last().unwrap_or_default();
    assert!(
        !end.is_empty() && last.starts_with(end),
        "{end:?} of {last:?}"
    );
    let settled = first_line(cx);
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    assert_eq!(first_line(cx), settled);
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

/// Have local terminal `id` print `text`: the fake shell prints back what it
/// is sent, and a paste outside bracketed paste mode sends it as it is.
fn print_into(cx: &mut TestAppContext, workspace: &Entity<Workspace>, id: u64, text: &str) {
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        let terminal = workspace
            .read(cx)
            .local_terminal(LocalTerminalId(id))
            .expect("the terminal is open")
            .read(cx)
            .terminal()
            .clone();
        terminal.update(cx, |terminal, cx| terminal.paste_clipboard(cx));
    });
}

/// The system notifications shown once there are `count` of them.
async fn system_notifications(
    cx: &mut TestAppContext,
    count: usize,
) -> Vec<gpui_kit::SystemNotification> {
    for _ in 0..200 {
        let shown = cx.shown_system_notifications();
        if shown.len() >= count {
            return shown;
        }
        cx.executor().timer(Duration::from_millis(10)).await;
    }
    panic!(
        "expected {count} system notifications, got {:?}",
        cx.shown_system_notifications()
    );
}

fn in_window(cx: &mut TestAppContext, handle: WindowHandle<Root>) -> usize {
    cx.update_window(handle.into(), |_, window, cx| {
        window.notifications(cx).len()
    })
    .unwrap()
}

#[gpui_kit::test]
async fn terminals_notify_where_the_user_will_see_it(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory).await;
    // Test windows start out not active.
    in_frame(cx, handle, |window, _| window.activate_window());
    assert!(cx.shown_system_notifications().is_empty());

    // In sight, a program's notice shows in the window; the bell says nothing.
    print_into(cx, &workspace, 1, "\x1b]777;notify;构建;完成\x07\x07");
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;
    cx.background_executor
        .advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(in_window(cx, handle), 1);
    assert!(cx.shown_system_notifications().is_empty());

    // Its notice gone, the first terminal rings behind a second one.
    cx.background_executor
        .advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(in_window(cx, handle), 0);
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(2))
            .is_some_and(|terminal| {
                terminal.read(cx).status(cx).lifecycle() == &TerminalLifecycle::Running
            })
    })
    .await;
    print_into(cx, &workspace, 1, "y/N\x07");
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;

    // With the window away, a notice goes to the system, under the tab's
    // name, and clicking it shows that terminal.
    cx.update(|cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(shellrs::app::CenterTab::LocalTerminal(LocalTerminalId(2)))
        );
    });
    gpui_kit::VisualTestContext::from_window(handle.into(), cx).deactivate_window();
    print_into(cx, &workspace, 1, "\x1b]9;部署完成\x07");
    let shown = system_notifications(cx, 1).await;
    assert_eq!(shown[0].tag.as_ref(), "terminal:local:1");
    assert_eq!(shown[0].title.as_ref(), "本地终端 1");
    assert_eq!(shown[0].body.as_ref(), "部署完成");
    cx.simulate_system_notification_response(gpui_kit::SystemNotificationResponse {
        tag: shown[0].tag.clone(),
        action_id: None,
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(shellrs::app::CenterTab::LocalTerminal(LocalTerminalId(1)))
        );
    });
}

#[gpui_kit::test]
async fn a_terminal_in_a_split_beside_rings_without_a_reminder(cx: &mut TestAppContext) {
    use gpui_kit::component::Placement;
    use gpui_kit::component::dock::{DockPlacement, InsertTarget, PanelId};

    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory).await;
    in_frame(cx, handle, |window, _| window.activate_window());
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(2))
            .is_some_and(|terminal| {
                terminal.read(cx).status(cx).lifecycle() == &TerminalLifecycle::Running
            })
    })
    .await;
    // The second beside the first.
    cx.update_window(handle.into(), |_, window, cx| {
        let panel = PanelId::from(
            workspace
                .read(cx)
                .local_terminal(LocalTerminalId(2))
                .expect("the second terminal")
                .entity_id(),
        );
        let area = workspace.read(cx).dock_area().clone();
        area.update(cx, |area, cx| {
            let node = area
                .layout(DockPlacement::Center)
                .and_then(|tree| tree.find_panel_node(panel))
                .expect("in the center");
            area.move_panel(
                panel,
                InsertTarget::Split {
                    node,
                    placement: Placement::Right,
                    size: None,
                },
                window,
                cx,
            );
        });
    })
    .unwrap();
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("local-terminal", 1_u64)).visible());
        assert!(window.find(("local-terminal", 2_u64)).visible());
    });
    cx.update(|cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(shellrs::app::CenterTab::LocalTerminal(LocalTerminalId(1)))
        );
    });
    // In sight, though not the tab in front: the bell says nothing.
    print_into(cx, &workspace, 2, "y/N\x07");
    cx.background_executor
        .advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(in_window(cx, handle), 0);
}

/// Have the settings hold `rules`, as the 关键字高亮 page writes them, with
/// highlighting on.
fn set_highlight_rules(
    cx: &mut TestAppContext,
    workspace: &Entity<Workspace>,
    rules: Vec<shellrs::terminal::HighlightRule>,
) {
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    settings.update(cx, |settings, cx| {
        settings.update(
            |settings| {
                settings.terminal_highlight.enabled = true;
                settings.terminal_highlight.rules = rules;
            },
            cx,
        )
    });
    cx.run_until_parked();
}

#[gpui_kit::test]
async fn a_matching_line_notifies_from_a_terminal_out_of_sight(cx: &mut TestAppContext) {
    use shellrs::terminal::HighlightRule;

    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory).await;
    // WARN only colors; ERROR tells.
    set_highlight_rules(
        cx,
        &workspace,
        vec![
            HighlightRule {
                pattern: "WARN".into(),
                ..HighlightRule::default()
            },
            HighlightRule {
                pattern: "ERROR".into(),
                notify: true,
                ..HighlightRule::default()
            },
        ],
    );
    in_frame(cx, handle, |window, _| window.activate_window());

    // Behind a second terminal, a finished line that matches shows in the
    // window; neither the WARN line nor the line still being written does.
    in_frame(cx, handle, |window, cx| {
        window.click("new-local-terminal", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .local_terminal(LocalTerminalId(2))
            .is_some_and(|terminal| {
                terminal.read(cx).status(cx).lifecycle() == &TerminalLifecycle::Running
            })
    })
    .await;
    print_into(cx, &workspace, 1, "12:01 WARN low disk\n12:02 ERR");
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        local_screen(&workspace, cx).contains("12:02 ERR")
    })
    .await;
    print_into(cx, &workspace, 1, "OR boom\n");
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;

    // With the window away, the system's notification names the tab and
    // the rule, and has the line.
    cx.background_executor
        .advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    gpui_kit::VisualTestContext::from_window(handle.into(), cx).deactivate_window();
    print_into(cx, &workspace, 1, "12:03 ERROR again\n");
    let shown = system_notifications(cx, 1).await;
    assert_eq!(shown[0].tag.as_ref(), "terminal:local:1");
    assert_eq!(shown[0].title.as_ref(), "本地终端 1：ERROR");
    assert_eq!(shown[0].body.as_ref(), "12:03 ERROR again");

    // In sight, a matching line says nothing: had it, the next one would
    // be held back as too soon after it.
    cx.background_executor
        .advance_clock(Duration::from_secs(10));
    cx.simulate_system_notification_response(gpui_kit::SystemNotificationResponse {
        tag: shown[0].tag.clone(),
        action_id: None,
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, _| window.activate_window());
    cx.update(|cx| {
        assert_eq!(
            workspace.read(cx).active_tab(),
            Some(shellrs::app::CenterTab::LocalTerminal(LocalTerminalId(1)))
        );
    });
    // The program's notice after it comes once the line has been weighed.
    print_into(cx, &workspace, 1, "12:04 ERROR seen\n\x1b]9;done\x07");
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;
    gpui_kit::VisualTestContext::from_window(handle.into(), cx).deactivate_window();
    print_into(cx, &workspace, 1, "12:05 ERROR later\n");
    let shown = system_notifications(cx, 2).await;
    assert_eq!(shown[1].body.as_ref(), "12:05 ERROR later");
}

/// A local terminal where one ssh'd on to a machine that runs `rz`: the
/// picker is asked for there too, and choosing nothing cancels it.
#[gpui_kit::test]
async fn rz_in_a_local_terminal_asks_for_files_too(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::printing(
        "rz waiting to receive.**\x18B0100000023be50\r\n\x11",
    ));
    let (handle, _) = open_workspace_with_factory(cx, factory.clone());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.run_until_parked();
    for _ in 0..200 {
        if cx.did_prompt_for_paths() {
            break;
        }
        cx.executor().timer(Duration::from_millis(10)).await;
        cx.run_until_parked();
    }
    cx.simulate_path_prompt_response(|options| {
        assert!(options.files && options.multiple);
        None
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        factory.data().starts_with(&[0x18; 10])
    })
    .await;
}

#[gpui_kit::test]
async fn a_paste_waits_between_its_lines_when_the_settings_say_so(cx: &mut TestAppContext) {
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_running_local_terminal(cx, factory.clone()).await;
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| {
                    settings.send_delay = SendDelay {
                        line_ms: 400,
                        character_ms: 0,
                    }
                },
                cx,
            )
        });
    });
    cx.run_until_parked();
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("one\ntwo".into())));
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(PasteTerminal), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(factory.written_text(), "one");

    // The line's Enter is the next write, due when the delay is up; the
    // line after it goes at once.
    cx.background_executor
        .advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(factory.written_text(), "one\rtwo");
}

#[gpui_kit::test]
async fn a_session_log_is_written_without_colors_and_with_a_time_on_each_line(
    cx: &mut TestAppContext,
) {
    let factory = Arc::new(FakeTerminalFactory::printing("\x1b[31mhello\x1b[0m\r\n"));
    let (handle, workspace) = open_workspace_with_factory(cx, factory);
    let folder = tempfile::tempdir().expect("a temporary folder");
    let path = folder.path().join("test.log");
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(
                |settings| {
                    settings.session_log.enabled = true;
                    settings.session_log.directory =
                        Some(folder.path().to_string_lossy().into_owned());
                    settings.session_log.template = "test.log".into();
                    settings.session_log.timestamp = true;
                    settings.session_log.timestamp_format = "[T] ".into();
                },
                cx,
            )
        });
    });
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-local-terminal", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        std::fs::read_to_string(&path).is_ok_and(|text| text.contains("hello"))
    })
    .await;

    let text = std::fs::read_to_string(&path).expect("the log was written");
    assert!(!text.contains('\x1b'), "{text:?}");
    assert!(text.contains("hello"), "{text:?}");
    assert!(
        text.lines()
            .filter(|line| !line.is_empty())
            .all(|line| line.starts_with("[T] ")),
        "{text:?}"
    );
}
