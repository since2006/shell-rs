//! The window around the features: tabs and closing them, the start page,
//! the title bar and dialogs in general.

use crate::support::*;

#[gpui_kit::test]
fn tab_close_button_closes_the_terminal_and_disconnects(cx: &mut TestAppContext) {
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
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        window.click(("close-terminal", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());
        assert!(
            window
                .try_find(("close-terminal", FIRST_NEW_TERMINAL))
                .is_none()
        );
        // The other seeded tabs are untouched.
        assert!(
            window
                .find(("close-terminal", INITIAL_WEB_TERMINAL))
                .visible()
        );
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(HostId(DB_01), cx).is_none());
        let store = workspace.store().read(cx);
        assert!(!store.host(HostId(DB_01)).unwrap().state.is_connected());
    });
}

#[gpui_kit::test]
async fn closing_every_tab_shows_the_recent_hosts(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // While tabs are open the start page stays out of the way.
        assert!(window.try_find("recent-hosts").is_none());
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("terminal", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(window.try_find("recent-hosts").is_none());
        // The last tab closes too (the tab group alone would refuse).
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        assert!(window.find("recent-hosts").visible());
        // The page takes the focus the closed tab held.
        assert_eq!(window.find("recent-hosts").focused(), Some(true));
        // Both hosts had been connected, so both are listed.
        assert!(window.find(("recent-host", WEB_01)).visible());
        assert!(window.find(("recent-host", STAGING_API)).visible());
        assert_eq!(window.find("status-connection").label(), Some("未连接"));

        window.click(("recent-host", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-hosts").visible());
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());
        window.double_click(("recent-host", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 web-01")
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        assert!(window.try_find("recent-hosts").is_none());
        assert_eq!(
            window.find("status-connection").label(),
            Some("已连接 web-01")
        );
    })
    .unwrap();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let recent: Vec<_> = store
            .recent_hosts()
            .map(|host| host.name.to_string())
            .collect();
        // Reconnecting moved web-01 to the front.
        assert_eq!(recent, ["web-01", "staging-api"]);
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-hosts").visible());
        // Reopening the page must not reconnect the old selection on Enter.
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-hosts").visible());
        assert!(
            window
                .try_find(("terminal", FIRST_NEW_TERMINAL + 1))
                .is_none()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
async fn enter_connects_the_selected_recent_host(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", INITIAL_WEB_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("close-terminal", INITIAL_STAGING_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("recent-host", STAGING_API), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-hosts").visible());
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 staging-api")
    })
    .await;
}

/// 快速连接 on the start page: search the saved hosts and connect to one
/// with Enter. (Choosing several is switched off for now.)
#[gpui_kit::test]
async fn quick_connect_finds_saved_hosts_and_connects_one(cx: &mut TestAppContext) {
    use shellrs::host::GroupDraft;

    let mut store = HostStore::empty();
    let group = store.insert_group_unnotified(GroupDraft::new("数据库", None));
    let [web, db, cache] = [
        ("web-01", "10.0.0.1", None),
        ("db-01", "10.0.0.2", Some(group)),
        ("cache", "10.0.0.3", None),
    ]
    .map(|(name, address, group)| {
        store.insert_unnotified(HostDraft::new(
            name,
            address,
            22,
            "root",
            AuthKind::Password,
            group,
        ))
    });
    let (handle, _) = open_workspace_with_store(cx, store);
    let row = |id: HostId| ("quick-connect-host", id.0);

    in_frame(cx, handle, |window, cx| {
        window.click("recent-quick-connect", cx)
    });
    in_frame(cx, handle, |window, cx| {
        // Every saved host, by name while none has been connected to.
        let tops: Vec<f32> = [cache, db, web]
            .map(|id| window.find(row(id)).bounds().origin.y.into())
            .to_vec();
        assert!(tops.is_sorted(), "{tops:?}");
        assert_eq!(
            window.find(row(db)).label(),
            Some("db-01 · 数据库 · root@10.0.0.2:22")
        );
        // The search box has the keys: a group's name finds its hosts.
        window.input("数据", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find(row(web)).is_none()
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find(row(db)).is_some());
        window.press("enter", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window
            .try_find(("terminal-tab", 1u64))
            .is_some_and(|tab| tab.label() == Some("db-01"))
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("quick-connect").is_none());
    });
}

/// The start page's row menu is the host tree's menu. Menus are not
/// driven here, so this dispatches what its items dispatch, from the page:
/// the page is drawn deferred over the dock, and its actions must still
/// reach the workspace.
#[gpui_kit::test]
async fn recent_host_menu_commands_work_from_the_start_page(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    for terminal in [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("close-terminal", terminal), cx);
        })
        .unwrap();
        cx.run_until_parked();
    }

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("recent-hosts").focused(), Some(true));
        window.dispatch_action(Box::new(EditHost(HostId(STAGING_API))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("host-name").value(), Some("staging-api"));
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();

    // A closed dialog leaves nothing focused; clicking the row focuses the
    // page again, as a right click would.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("recent-host", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DeleteHost(HostId(WEB_01))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("recent-host", WEB_01)).is_none());
        assert!(window.find(("recent-host", STAGING_API)).visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn close_shortcut_closes_the_displayed_tab_down_to_none(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

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
        // The new tab is displayed, with its input focused.
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("terminal", FIRST_NEW_TERMINAL)).is_none());
        assert!(window.try_find("recent-hosts").is_none());
        // Keep closing whichever tab the dock displays next.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("terminal", INITIAL_WEB_TERMINAL))
                .is_none()
        );
        assert!(
            window
                .try_find(("terminal", INITIAL_STAGING_TERMINAL))
                .is_none()
        );
        assert!(window.find("recent-hosts").visible());
        // With nothing open the shortcut does nothing.
        window.press("cmd-w", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-hosts").visible());
    })
    .unwrap();
}

/// A host's terminal and SFTP tabs sit side by side in the center, and
/// only the active one renders. The tab going inactive used to keep the
/// window focus, which took its focus handle out of the dispatch tree and
/// left every 「×」 dead.
#[gpui_kit::test]
fn both_tabs_of_one_host_stay_closable(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    // The terminal tab is active and focused; opening SFTP puts a second tab
    // for the same host beside it and activates that one.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("sftp", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("explorer", SFTP_TAB)).visible());
        window.click(("close-explorer", SFTP_TAB), cx);
    })
    .unwrap();
    cx.run_until_parked();

    // The SFTP tab is gone and the terminal tab, now active again, still closes.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find(("close-explorer", SFTP_TAB)).is_none());
        assert!(window.find(("terminal", FIRST_NEW_TERMINAL)).visible());
        window.click(("close-terminal", FIRST_NEW_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .try_find(("close-terminal", FIRST_NEW_TERMINAL))
                .is_none()
        );
    })
    .unwrap();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.explorer(ExplorerId(SFTP_TAB)).is_none());
        assert!(workspace.terminal(HostId(DB_01), cx).is_none());
    });
}

/// Double-clicking a tab's title shows or hides the host sidebar; a
/// single click only selects the tab.
#[gpui_kit::test]
async fn double_clicking_a_tab_toggles_the_host_sidebar(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    cx.run_until_parked();
    let sidebar_shown = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("host-search")
                .is_some_and(|search| search.visible())
        })
        .unwrap()
    };
    assert!(sidebar_shown(cx));
    for shown in [false, true] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.double_click(("terminal-tab", INITIAL_WEB_TERMINAL), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(sidebar_shown(cx), shown);
    }
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(sidebar_shown(cx));
}

#[gpui_kit::test]
async fn a_terminal_tab_can_be_renamed_and_follow_the_host_again(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let tab = ("terminal-tab", INITIAL_WEB_TERMINAL);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some("web-01"));
        // Undetected hosts show the fallback mark in the tab too.
        assert_eq!(
            window
                .find(("terminal-tab-os", INITIAL_WEB_TERMINAL))
                .label(),
            Some("未探测到系统")
        );
        window.dispatch_action(
            Box::new(RenameTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("tab-name").value(), Some("web-01"));
        window.click("tab-name", cx);
        window.press("cmd-a", cx);
        window.input("日志排查", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(tab).label(), Some("日志排查"));
        // The other tab of the bar keeps its host name.
        assert_eq!(
            window
                .find(("terminal-tab", INITIAL_STAGING_TERMINAL))
                .label(),
            Some("staging-api")
        );
    })
    .unwrap();
    cx.update(|cx| {
        // A tab title is not a host setting.
        let store = workspace.read(cx).store().read(cx);
        let host = store.host(HostId(WEB_01)).expect("host kept");
        assert_eq!(host.name.as_ref(), "web-01");
    });

    // Clearing the field returns the tab to the host name.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // A closed dialog leaves nothing focused, and an action only reaches
        // handlers on the focused element's path.
        window.click("host-search", cx);
        window.dispatch_action(
            Box::new(RenameTerminal(RemoteTerminalId(INITIAL_WEB_TERMINAL))),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("tab-name").value(), Some("日志排查"));
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
        assert_eq!(window.find(tab).label(), Some("web-01"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn batch_close_commands_take_the_tabs_around_the_clicked_one(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let dispatch = |cx: &mut TestAppContext, action: Box<dyn gpui_kit::Action>| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.dispatch_action(action, cx);
        })
        .unwrap();
        cx.run_until_parked();
    };
    let close = |tab, scope| Box::new(CloseTabs { tab, scope });
    let terminal = |id| CenterTab::Terminal(RemoteTerminalId(id));
    let open_terminals = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let workspace = workspace.read(cx);
            (1..=FIRST_NEW_TERMINAL + 1)
                .filter(|id| workspace.remote_terminal(RemoteTerminalId(*id)).is_some())
                .collect::<Vec<_>>()
        })
    };

    // [web-01, staging-api, web-01 · SFTP, local]
    dispatch(cx, Box::new(OpenExplorer(HostId(WEB_01))));
    dispatch(cx, Box::new(NewLocalTerminal));
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.explorer(ExplorerId(SFTP_TAB)).is_some());
        assert!(workspace.local_terminal(LocalTerminalId(1)).is_some());
    });

    // Right of staging-api: the SFTP and local tabs, whatever their kind.
    dispatch(
        cx,
        close(terminal(INITIAL_STAGING_TERMINAL), CloseScope::Right),
    );
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.explorer(ExplorerId(SFTP_TAB)).is_none());
        assert!(workspace.local_terminal(LocalTerminalId(1)).is_none());
    });
    assert_eq!(
        open_terminals(cx),
        [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL]
    );

    // [web-01, staging-api, web-01 #2] → left of staging-api.
    dispatch(cx, Box::new(ConnectHost(HostId(WEB_01))));
    dispatch(
        cx,
        close(terminal(INITIAL_STAGING_TERMINAL), CloseScope::Left),
    );
    assert_eq!(
        open_terminals(cx),
        [INITIAL_STAGING_TERMINAL, FIRST_NEW_TERMINAL]
    );

    // [staging-api, web-01 #2, staging-api #2] → others than web-01 #2.
    dispatch(cx, Box::new(ConnectHost(HostId(STAGING_API))));
    assert_eq!(open_terminals(cx).len(), 3);
    dispatch(cx, close(terminal(FIRST_NEW_TERMINAL), CloseScope::Others));
    assert_eq!(open_terminals(cx), [FIRST_NEW_TERMINAL]);

    // All of them, down to the start page.
    dispatch(cx, close(terminal(FIRST_NEW_TERMINAL), CloseScope::All));
    assert!(open_terminals(cx).is_empty());
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-hosts").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
async fn the_start_page_marks_recent_hosts_with_their_operating_system(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let id = store.insert_unnotified(HostDraft::new(
        "web-01",
        "10.0.1.12",
        22,
        "root",
        AuthKind::Password,
        None,
    ));
    // Connecting once puts it on the start page; disconnecting keeps it there
    // and leaves the centre empty, which is when that page shows.
    store.set_state_unnotified(id, ConnectionState::Connected);
    store.set_state_unnotified(id, ConnectionState::Disconnected);
    store.set_host_os_unnotified(id, Some(HostOs::Ubuntu));
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("recent-hosts").visible());
        assert_eq!(
            window.find(("recent-host-os", id.0)).label(),
            Some("Ubuntu"),
            "开始页和主机树用同一个标记"
        );
    })
    .unwrap();
}

/// 命令片段 beside web-01's terminal, where its dialogs open from.
fn show_snippets(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
}

/// A dialog holds what was typed into it: a click beside it does not close
/// it, Escape (like its buttons) does.
#[gpui_kit::test]
async fn a_click_beside_a_dialog_does_not_close_it(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_forwards(
        cx,
        HostStore::seed(),
        Arc::new(FakeForwardProvider::default()),
    );
    cx.run_until_parked();

    for (open, field) in [
        ("new-host-panel", "host-name"),
        ("temporary-connection", "host-address"),
        ("new-group", "group-name"),
        ("new-forward", "forward-name"),
        ("new-credential", "credential-name"),
        ("snippets-new", "snippet-name"),
        ("snippets-new-category", "snippet-category-name"),
    ] {
        if open == "new-forward" {
            show_forwards(cx, handle).await;
        }
        if open == "new-credential" {
            show_credentials(cx, handle).await;
        }
        if open == "snippets-new" {
            show_snippets(cx, handle);
        }
        in_frame(cx, handle, |window, cx| window.click(open, cx));
        in_frame(cx, handle, |window, cx| {
            window.click(field, cx);
            window.input("typed", cx);
        });
        // The status bar is outside the dialog, under its backdrop.
        in_frame(cx, handle, |window, cx| {
            window.click("status-connection", cx)
        });
        in_frame(cx, handle, |window, cx| {
            assert!(
                window.try_find("commit").is_some(),
                "{open}: a click beside the dialog closed it"
            );
            assert_eq!(window.find(field).value(), Some("typed"));
            window.press("escape", cx);
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
            window.try_find("commit").is_none()
        })
        .await;
    }
}

/// Every add and edit dialog says why it refused in an error notification,
/// not in a line under its form, and takes the notification with it when it
/// closes.
#[gpui_kit::test]
async fn a_dialog_says_what_is_wrong_in_a_notification_that_goes_with_it(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_forwards(
        cx,
        HostStore::seed(),
        Arc::new(FakeForwardProvider::default()),
    );
    cx.run_until_parked();

    for (open, field, error) in [
        ("new-host-panel", "host-name", "请输入名称"),
        ("temporary-connection", "host-address", "请输入地址"),
        ("new-group", "group-name", "请输入分组名称"),
        ("new-forward", "forward-name", "请选择端口转发经由的主机"),
        ("new-credential", "credential-name", "请输入名称"),
        ("snippets-new", "snippet-name", "请输入名称"),
        (
            "snippets-new-category",
            "snippet-category-name",
            "请输入分类名称",
        ),
    ] {
        if open == "new-forward" {
            show_forwards(cx, handle).await;
        }
        if open == "new-credential" {
            show_credentials(cx, handle).await;
        }
        if open == "snippets-new" {
            show_snippets(cx, handle);
        }
        in_frame(cx, handle, |window, cx| window.click(open, cx));
        in_frame(cx, handle, |window, cx| {
            window.click(field, cx);
            window.click("commit", cx);
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
            window.render_frame(cx);
            window.try_find("form-error").is_some()
        })
        .await;
        in_frame(cx, handle, |window, cx| {
            assert_eq!(window.find("form-error").label(), Some(error), "{open}");
            assert_eq!(window.notifications(cx).len(), 1, "{open}");
            assert!(window.try_find("commit").is_some(), "{open}: still open");
            window.click(field, cx);
            window.press("escape", cx);
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
            window.render_frame(cx);
            window.try_find("commit").is_none() && window.notifications(cx).is_empty()
        })
        .await;
    }
}

/// What goes wrong while the window is being built (a database that will
/// not open, a service that will not start) is said in a notification, and
/// the window has no `Root` to show one yet. Pushing it there and then
/// crashed the app at launch.
#[gpui_kit::test]
fn a_problem_found_while_the_window_is_built_is_shown_once_it_is_open(cx: &mut TestAppContext) {
    init_app(cx);
    let handle = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let store = cx.new(|_| HostStore::empty());
        let remote = Arc::new(FixedRemoteTerminalTransportProvider::new(Arc::new(
            FakeTerminalFactory::default(),
        )));
        let view = cx.new(|cx| {
            Workspace::new_with_services(
                store,
                cx.new(|_| settings_store()),
                remote,
                Arc::new(FakeTerminalFactory::default()),
                Arc::new(FakeSftpProvider::default()),
                Arc::new(FakeLocalDirectory::default()),
                Arc::new(FakeConnectionTester::default()),
                Arc::new(FakeForwardProvider::default()),
                window,
                cx,
            )
        });
        // As `main` does for a database it could not open.
        shellrs::workspace::notify_once_open(
            Notification::error("无法打开本地数据库，本次运行的改动不会被保存"),
            window,
            cx,
        );
        Root::new(view, window, cx)
    });
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(window.find("host-search").visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_tool_switch_shows_a_tool_and_hides_it_again(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    let sidebar = ("tool-sidebar", INITIAL_WEB_TERMINAL);
    in_frame(cx, handle, |window, _| {
        // Hidden at first: the switch is there, none of it pressed.
        assert!(window.try_find(sidebar).is_none());
        assert_eq!(window.find("tool-snippets").checked(), Some(false));
    });

    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(sidebar).label(), Some("命令片段"));
        assert!(window.find("snippets").visible());
        assert_eq!(window.find("tool-snippets").checked(), Some(true));
        // Between the center and the switch.
        let bounds = window.find(sidebar).bounds();
        let switch = window.find("tool-switch").bounds();
        assert!(bounds.right() <= switch.left(), "{bounds:?} {switch:?}");
        // The switch is the sidebar's only control: no collapse button in
        // the center's tab bar.
        assert!(window.try_find("toggle-dock:Right").is_none());
    });

    // Another tool takes its place.
    in_frame(cx, handle, |window, cx| window.click("tool-docker", cx));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(sidebar).label(), Some("Docker"));
        assert_eq!(window.find("tool-snippets").checked(), Some(false));
        assert_eq!(window.find("tool-docker").checked(), Some(true));
    });

    // The tool showing hides the sidebar.
    in_frame(cx, handle, |window, cx| window.click("tool-docker", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(sidebar).is_none());
        assert_eq!(window.find("tool-docker").checked(), Some(false));
    });

    // The shortcut brings back the tool shown last.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleToolSidebar), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(sidebar).label(), Some("Docker"));
    });
}

#[gpui_kit::test]
fn a_tool_follows_the_terminal_in_front_and_leaves_it_the_keyboard(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx);
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find(("tool-sidebar", INITIAL_WEB_TERMINAL))
                .is_some()
        );
        // The switch does not take the keyboard from the terminal.
        assert_eq!(
            window.find(("terminal", INITIAL_WEB_TERMINAL)).focused(),
            Some(true)
        );
    });

    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    let sidebar = ("tool-sidebar", INITIAL_STAGING_TERMINAL);
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(sidebar).is_some());
        assert!(window.find("snippets").visible());
    });

    // A click in the sidebar takes the focus there; hiding the sidebar
    // hands it back to the terminal, so the shortcuts keep working.
    in_frame(cx, handle, |window, cx| {
        window.click("snippets-summary", cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find(sidebar).focused(), Some(true));
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find(sidebar).is_none());
        assert_eq!(
            window
                .find(("terminal", INITIAL_STAGING_TERMINAL))
                .focused(),
            Some(true)
        );
    });
}

#[gpui_kit::test]
async fn the_tool_sidebar_goes_with_ssh_terminals_only(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_sftp(cx, Arc::new(FakeSftpProvider::default()));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    let shown = |cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, _| {
            let switch = window.try_find("tool-switch").is_some();
            let sidebar = [INITIAL_WEB_TERMINAL, INITIAL_STAGING_TERMINAL]
                .into_iter()
                .any(|id| window.try_find(("tool-sidebar", id)).is_some());
            (switch, sidebar)
        })
    };
    assert_eq!(shown(cx), (true, true));

    // An SFTP tab in front: neither the sidebar nor its switch.
    open_test_explorer(cx, handle).await;
    assert_eq!(shown(cx), (false, false));

    // Nor the settings, nor a local terminal, which the shortcut leaves
    // alone too.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(OpenSettings), cx)
    });
    assert_eq!(shown(cx), (false, false));
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(NewLocalTerminal), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleToolSidebar), cx)
    });
    assert_eq!(shown(cx), (false, false));

    // The next SSH terminal brings it back as it was, working on that one.
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_STAGING_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        let sidebar = window.find(("tool-sidebar", INITIAL_STAGING_TERMINAL));
        assert_eq!(sidebar.label(), Some("命令片段"));
        assert_eq!(window.find("tool-snippets").checked(), Some(true));
    });
}

#[gpui_kit::test]
fn the_start_page_has_no_tool_switch(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_store(cx, HostStore::empty());
    in_frame(cx, handle, |window, _| {
        assert!(window.find("recent-hosts").visible());
        assert!(window.try_find("tool-switch").is_none());
    });
}

#[gpui_kit::test]
fn the_tool_sidebar_drags_wider_but_never_narrower_than_it_opens(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("tool-snippets", cx));
    let sidebar = ("tool-sidebar", INITIAL_WEB_TERMINAL);
    let bounds =
        |cx: &mut TestAppContext| in_frame(cx, handle, |window, _| window.find(sidebar).bounds());
    // Drag the sidebar's edge, the dock's resize handle, from `from` to `to`.
    let drag = |cx: &mut TestAppContext, from: gpui_kit::Point<gpui_kit::Pixels>, by: f32| {
        let to = point(from.x + px(by), from.y);
        in_frame(cx, handle, |window, cx| {
            window.dispatch_event(
                gpui_kit::PlatformInput::MouseDown(gpui_kit::MouseDownEvent {
                    button: MouseButton::Left,
                    position: from,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            // Past the drag threshold, then where the drag goes.
            for position in [point(from.x + px(by.signum() * 6.), from.y), to] {
                window.dispatch_event(
                    gpui_kit::PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        modifiers: Default::default(),
                    }),
                    cx,
                );
            }
            window.dispatch_event(
                gpui_kit::PlatformInput::MouseUp(gpui_kit::MouseUpEvent {
                    button: MouseButton::Left,
                    position: to,
                    modifiers: Default::default(),
                    click_count: 1,
                }),
                cx,
            );
        });
    };

    let opened = bounds(cx);
    let edge = point(opened.left() + px(2.), opened.center().y);
    drag(cx, edge, -100.);
    let wider = bounds(cx);
    // The dock goes by where the pointer is, which was pressed a little
    // inside the edge.
    assert!(
        (wider.size.width - opened.size.width - px(100.)).abs() < px(3.),
        "{opened:?} → {wider:?}"
    );

    // Dragged back past where it opened: it stops there.
    drag(cx, point(wider.left() + px(2.), wider.center().y), 250.);
    let narrowed = bounds(cx);
    assert!(
        (narrowed.size.width - opened.size.width).abs() < px(1.),
        "{opened:?} → {narrowed:?}"
    );
}
