//! The SFTP tab: the panes, navigating, the path label, bookmarks, file
//! commands, selection and the connection.

use crate::support::*;

#[gpui_kit::test]
async fn sftp_button_opens_explorer_and_navigates(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", DB_01), cx);
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
    use shellrs::host::BookmarkSide;
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

    // Bookmarks belong to the host and the pane.
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
            store.bookmarks(HostId(DB_01), BookmarkSide::Remote),
            ["/home/tester"]
        );
        assert!(
            store
                .bookmarks(HostId(DB_01), BookmarkSide::Local)
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
                .bookmarks(HostId(DB_01), BookmarkSide::Remote)
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
    use shellrs::host::BookmarkSide;
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let bookmarks = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            workspace
                .read(cx)
                .store()
                .read(cx)
                .bookmarks(HostId(DB_01), BookmarkSide::Remote)
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
/// the host, and 重新连接 connects again, from a dropped connection or a
/// live one. Its connection, and why it dropped, show in red at the window's
/// bottom left, so the list is never pushed around.
/// A new SFTP tab splits its width half and half between the panes, with
/// every toolbar button of each showing and the two lists level.
#[gpui_kit::test]
async fn sftp_panes_open_half_and_half(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let local = window.find(("local-pane", SFTP_TAB)).bounds();
        let remote = window.find(("remote-pane", SFTP_TAB)).bounds();
        assert!(
            (local.size.width - remote.size.width).abs() <= px(1.),
            "{local:?} {remote:?}"
        );
        for (pane, transfer) in [("local-pane", "upload"), ("remote-pane", "download")] {
            let scope = window.within((pane, SFTP_TAB));
            for id in ["path-select", "forward", transfer, "new"] {
                assert!(scope.find(id).visible(), "{pane} {id}");
            }
        }
        let top = |pane: &'static str, window: &mut gpui_kit::Window| {
            window.within((pane, SFTP_TAB)).find("table").bounds().top()
        };
        assert_eq!(top("local-pane", window), top("remote-pane", window));
    })
    .unwrap();
}

/// Where the user drags the divider between the panes stays while another
/// tab is shown and this one comes back.
#[gpui_kit::test]
async fn sftp_panes_keep_their_split_across_tab_switches(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let local_width = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.find(("local-pane", SFTP_TAB)).bounds().size.width
        })
        .unwrap()
    };
    let opened = local_width(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let local = window.find(("local-pane", SFTP_TAB)).bounds();
        let divider = gpui_kit::point(local.right(), local.center().y);
        window.drag(divider, divider - gpui_kit::point(px(120.), px(0.)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let dragged = local_width(cx);
    assert!(dragged < opened - px(100.), "{opened:?} → {dragged:?}");

    // Another SFTP tab of the host comes to the front, then this one.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(HostId(DB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("local-pane", SFTP_TAB)).is_none());
        window.click(("explorer-tab", SFTP_TAB), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(local_width(cx), dragged);
}

/// Disconnected, anything asked of the remote side says so in a dialog with
/// 重新连接, instead of doing nothing. 取消 leaves it as it is; local work
/// goes on without asking.
#[gpui_kit::test]
async fn sftp_remote_commands_while_disconnected_offer_to_reconnect(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    let events = provider.events.lock().unwrap()[0].clone();
    events
        .send_blocking(SftpEvent::Disconnected("SFTP 连接中断，请重新连接".into()))
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("未连接 db-01：SFTP 连接中断，请重新连接")
    })
    .await;

    // A toolbar button still answers, with the dialog.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within(("remote-pane", SFTP_TAB))
            .click("refresh", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("ok").visible());
        window.click("cancel", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("ok").is_none());
        assert!(
            window
                .find("status-connection")
                .label()
                .is_some_and(|status| status.starts_with("未连接"))
        );
    })
    .unwrap();
    assert_eq!(*provider.reconnects.lock().unwrap(), 0);

    // Local work does not ask.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Refresh { remote: false },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("ok").is_none());
    })
    .unwrap();

    // A key or menu command asks too, and 重新连接 there reconnects.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Up { remote: true },
            )),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 db-01")
    })
    .await;
    assert_eq!(*provider.reconnects.lock().unwrap(), 1);
}

/// Like a terminal tab, an SFTP tab shows its connection's round trip beside
/// its buttons, and only while it is connected.
#[gpui_kit::test]
async fn the_sftp_tab_bar_shows_the_connection_latency_while_connected(cx: &mut TestAppContext) {
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, _) = open_workspace_with_sftp(cx, provider.clone());
    open_test_explorer(cx, handle).await;
    let latency = ("sftp-latency", SFTP_TAB);
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(latency).is_none(), "nothing measured yet");
    });

    let events = provider.events.lock().unwrap()[0].clone();
    for millis in [32, 180] {
        events
            .send_blocking(SftpEvent::Latency(Latency::Measured(
                Duration::from_millis(millis),
            )))
            .unwrap();
        let shown = format!("{millis} ms");
        cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
            window.render_frame(cx);
            window
                .try_find(latency)
                .is_some_and(|element| element.label() == Some(shown.as_str()))
        })
        .await;
    }

    events
        .send_blocking(SftpEvent::Disconnected("SFTP 连接中断，请重新连接".into()))
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find(latency).is_none()
    })
    .await;
}

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
        // Still there to click: it answers with the offer to reconnect.
        assert!(enabled(&remote.find("refresh")));
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

    // 打开 SFTP opens another tab of the same host.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("open-sftp", SFTP_TAB), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(workspace.read(cx).explorers_of(HostId(DB_01), cx).len(), 2));
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
            // Said in a notification, one at a time.
            assert_eq!(window.find("form-error").label(), Some(error));
            assert_eq!(window.notifications(cx).len(), 1);
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
    // A terminal of the same host finds the system; the SFTP tab follows.
    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        store.update(cx, |store, cx| {
            store.set_host_os(HostId(DB_01), Some(HostOs::Ubuntu), cx)
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
async fn the_sftp_tab_can_be_renamed_and_follow_the_host_again(cx: &mut TestAppContext) {
    use shellrs::app::RenameExplorer;
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let tab = ("explorer-tab", SFTP_TAB);
    let default = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        format!("{} · SFTP", store.host(HostId(DB_01)).unwrap().name)
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
        window.click("host-search", cx);
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
    use shellrs::app::{DisconnectHost, ExplorerAction, ExplorerCommand};
    let provider = Arc::new(FakeSftpProvider::default());
    let (handle, workspace) = open_workspace_with_sftp(cx, provider);
    open_test_explorer(cx, handle).await;
    let second = ExplorerId(SFTP_TAB + 1);
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(HostId(DB_01))), cx);
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
                .explorers_of(HostId(DB_01), cx)
                .iter()
                .map(|panel| panel.read(cx).remote().read(cx).path())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(paths(cx), ["/home/tester", "/etc"]);

    // Closing one keeps the other, and the host stays connected.
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
            store.host(HostId(DB_01)).unwrap().state,
            ConnectionState::Connected
        );
    });

    // Disconnecting the host disconnects every SFTP tab of it.
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(OpenExplorer(HostId(DB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(DisconnectHost(HostId(DB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let explorers = workspace.read(cx).explorers_of(HostId(DB_01), cx);
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
        window.dispatch_action(Box::new(OpenExplorer(HostId(DB_01))), cx);
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

/// Until a directory has been read the list does not claim to be empty: it
/// says it is connecting, then reading, as a slow network makes it wait.
/// A directory read with nothing in it lists only `..`.
#[gpui_kit::test]
async fn sftp_list_says_what_it_waits_for_before_saying_empty(cx: &mut TestAppContext) {
    use shellrs::app::{ExplorerAction, ExplorerCommand};
    let (release, hold) = mpsc::channel();
    let provider = Arc::new(FakeSftpProvider {
        hold_connection: Arc::new(Mutex::new(Some(hold))),
        slow_home: true,
        ..Default::default()
    });
    let (handle, _) = open_workspace_with_sftp(cx, provider);
    let placeholder_is = |expected: &'static str| {
        move |window: &mut gpui_kit::Window, cx: &mut App| {
            window.render_frame(cx);
            window
                .within(("remote-pane", SFTP_TAB))
                .try_find("list-placeholder")
                .is_some_and(|placeholder| placeholder.label() == Some(expected))
        }
    };
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(OpenExplorer(HostId(DB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        assert!(placeholder_is("正在连接 SFTP…")(window, cx));
    })
    .unwrap();

    // Connected; the home directory takes its time.
    release.send(()).unwrap();
    cx.wait_for(
        handle.into(),
        Duration::from_secs(2),
        placeholder_is("正在读取目录…"),
    )
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(
            Box::new(ExplorerAction::new(
                ExplorerId(SFTP_TAB),
                ExplorerCommand::Navigate {
                    remote: true,
                    path: "/empty".into(),
                },
            )),
            cx,
        );
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        let remote = window.within(("remote-pane", SFTP_TAB));
        remote.try_find("file:..").is_some() && remote.try_find("list-placeholder").is_none()
    })
    .await;
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
                // Toolbars stay one line high: the lists start level.
                let mut top = |pane: &'static str| {
                    window.within((pane, SFTP_TAB)).find("table").bounds().top()
                };
                let (local, remote) = (top("local-pane"), top("remote-pane"));
                assert_eq!(local, remote, "zoom {zoom}");
                for (pane, transfer) in [("local-pane", "upload"), ("remote-pane", "download")] {
                    let bounds = window.find((pane, SFTP_TAB)).bounds();
                    let scope = window.within((pane, SFTP_TAB));
                    assert!(scope.find("path-select").visible(), "{pane} at zoom {zoom}");
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
                        // Shown whole, or not at all when the pane is too
                        // narrow for it.
                        let button = scope.find(id);
                        if !button.visible() {
                            continue;
                        }
                        let button = button.bounds();
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
