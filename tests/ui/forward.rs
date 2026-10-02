//! 端口转发: the sidebar list, the dialog and its diagram, and starting,
//! stopping and reconnecting a forward.

use crate::support::*;

/// A local forward through a seeded host: `port` on this machine to the
/// database behind the server.
fn local_forward(host: u64, port: u16) -> ForwardDraft {
    ForwardDraft::new(
        ForwardKind::Local,
        HostId(host),
        ForwardEndpoint::new("127.0.0.1", port),
        Some(ForwardEndpoint::new("db.internal", 3306)),
    )
}

/// The seeded store with one rule, 「数据库」, through db-01.
fn store_with_forward() -> (HostStore, ForwardId) {
    let mut store = HostStore::seed();
    let id = store
        .insert_forward_unnotified(local_forward(DB_01, 8080).with_name("数据库"))
        .expect("db-01 is seeded");
    (store, id)
}

/// Wait until the row of `id` says `label`.
async fn wait_for_forward_status(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    id: ForwardId,
    label: &str,
) {
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find(("forward-status", id.0))
            .is_some_and(|status| status.label() == Some(label))
    })
    .await;
}

#[gpui_kit::test]
async fn the_title_bar_switches_the_sidebar_between_hosts_and_forwards(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_forwards(
        cx,
        HostStore::seed(),
        Arc::new(FakeForwardProvider::default()),
    );
    cx.run_until_parked();
    let showing = |cx: &mut TestAppContext, id: &'static str| {
        in_frame(cx, handle, |window, _| {
            window.try_find(id).is_some_and(|element| element.visible())
        })
    };

    // Hosts come first, and the switch says so.
    assert!(showing(cx, "host-search"));
    assert!(!showing(cx, "forward-search"));
    assert!(showing(cx, "new-group"));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("show-hosts").checked(), Some(true));
        assert_eq!(window.find("show-forwards").checked(), Some(false));
        // A new host, the usual addition, comes before a new group.
        let host = window.find("new-host-panel").bounds();
        let group = window.find("new-group").bounds();
        assert!(host.right() <= group.left(), "{host:?} {group:?}");
    });

    show_forwards(cx, handle).await;
    assert!(!showing(cx, "host-search"));
    // The dock's title bar and toolbar follow the list.
    assert!(showing(cx, "new-forward"));
    assert!(!showing(cx, "new-group"));
    // What both lists share stays.
    assert!(showing(cx, "open-settings"));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("show-hosts").checked(), Some(false));
        assert_eq!(window.find("show-forwards").checked(), Some(true));
        assert_eq!(window.find("forward-empty").label(), Some("还没有端口转发"));
    });

    // The search shortcut goes to the list that is up.
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.dispatch_action(Box::new(FocusSearch), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("forward-search").focused(), Some(true));
    });

    // Hidden, the sidebar marks neither; picking a list brings it back.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ToggleHostPanel), cx);
    });
    assert!(!showing(cx, "forward-search"));
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("show-forwards").checked(), Some(false));
    });
    in_frame(cx, handle, |window, cx| window.click("show-hosts", cx));
    assert!(showing(cx, "host-search"));
    assert!(!showing(cx, "forward-search"));
    // And the host list still takes its own commands.
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.dispatch_action(Box::new(FocusSearch), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("host-search").focused(), Some(true));
    });
}

#[gpui_kit::test]
async fn a_forward_is_created_edited_and_deleted_through_its_dialog(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_forwards(
        cx,
        HostStore::seed(),
        Arc::new(FakeForwardProvider::default()),
    );
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    // The second host of the list, which the test picks below. It is not
    // the one whose tab is in front: a new rule does not take that one.
    let host = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let second = store.hosts()[1].clone();
        assert_ne!(store.active().map(|active| active.id), Some(second.id));
        second
    });

    in_frame(cx, handle, |window, cx| window.click("new-forward", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("commit").visible());
        // The sentence is there before anything is typed, with the gaps shown.
        assert_eq!(
            window.find("forward-explanation").label(),
            Some("在本机连接 127.0.0.1:…，就等于从 SSH 服务器连接 …:…。")
        );
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        // No host is picked for a new rule: that is asked for first.
        assert_eq!(
            window.find("form-error").label(),
            Some("请选择端口转发经由的主机")
        );
        // The field shows its prompt, not a host.
        assert_eq!(window.find("forward-host").value(), Some("请选择主机"));
        window.within("forward-host").click("input", cx);
    });
    // The second entry of the list that opened.
    for key in ["down", "down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("forward-host").value(),
            Some(format!("{}（{}）", host.name, host.endpoint()).as_str())
        );
        window.click("forward-bind-port", cx);
        window.input("8080", cx);
        window.click("forward-target-host", cx);
        window.input("db.internal", cx);
        window.click("forward-target-port", cx);
        window.input("3306", cx);
    });
    in_frame(cx, handle, |window, cx| {
        // The picture's sentence follows the fields. Both name the server
        // by the part it plays, not by the host picked for it.
        assert_eq!(
            window.find("forward-explanation").label(),
            Some("在本机连接 127.0.0.1:8080，就等于从 SSH 服务器连接 db.internal:3306。")
        );
        assert_eq!(
            window.find("forward-diagram").label(),
            Some("本机 → SSH 服务器 → 目标服务")
        );
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let id = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.forwards().len(), 1);
        let rule = &store.forwards()[0];
        assert_eq!(rule.kind, ForwardKind::Local);
        assert_eq!(rule.host, host.id);
        assert_eq!(rule.bind, ForwardEndpoint::new("127.0.0.1", 8080));
        assert_eq!(rule.target, Some(ForwardEndpoint::new("db.internal", 3306)));
        assert!(!rule.auto_start);
        rule.id
    });
    // Created, not started: the row is there with its switch.
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("forward-row", id.0)).visible());
        assert_eq!(
            window.find(("forward-status", id.0)).label(),
            Some("已停止")
        );
    });

    // Editing: a dynamic forward has no target to fill in.
    in_frame(cx, handle, |window, cx| {
        // A closed dialog leaves nothing focused.
        window.click(("forward-row", id.0), cx);
        window.dispatch_action(Box::new(EditForward(id)), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("forward-target-host").is_some());
        window.within("forward-kind").click(2usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("forward-target-host").is_none());
        assert_eq!(
            window.find("forward-explanation").label(),
            Some("把应用的 SOCKS5 代理设为 127.0.0.1:8080，它的连接都从 SSH 服务器发出。")
        );
        assert_eq!(
            window.find("forward-diagram").label(),
            Some("本机 → SSH 服务器 → 任意地址")
        );
        window.click("forward-name", cx);
        window.input("代理", cx);
        window.click("forward-auto-start", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let rule = store.forward(id).expect("the rule is still there");
        assert_eq!(rule.kind, ForwardKind::Dynamic);
        assert_eq!(rule.target, None);
        assert_eq!(rule.name.as_ref(), "代理");
        assert!(rule.auto_start);
    });

    // Deleting asks first, by name.
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-row", id.0), cx);
        window.dispatch_action(Box::new(DeleteForward(id)), cx);
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("forward-row", id.0)).is_none()
    })
    .await;
    cx.update(|cx| {
        assert!(workspace.read(cx).store().read(cx).forwards().is_empty());
    });
}

#[gpui_kit::test]
async fn a_remote_forward_swaps_the_machines_in_the_picture_and_the_labels(
    cx: &mut TestAppContext,
) {
    let (store, id) = store_with_forward();
    let (handle, workspace) =
        open_workspace_with_forwards(cx, store, Arc::new(FakeForwardProvider::default()));
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-row", id.0), cx);
        window.dispatch_action(Box::new(EditForward(id)), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("forward-diagram").label(),
            Some("本机 → SSH 服务器 → 目标服务")
        );
        window.within("forward-kind").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        // The server is where the tunnel is entered now.
        assert_eq!(
            window.find("forward-diagram").label(),
            Some("SSH 服务器 → 本机 → 目标服务")
        );
        assert_eq!(
            window.find("forward-explanation").label(),
            Some("在 SSH 服务器上连接 127.0.0.1:8080，就等于从本机连接 db.internal:3306。")
        );
        assert!(window.try_find("forward-notes").is_none());
        // Listening beyond the server's loopback needs the server's consent.
        window.click("forward-bind-host", cx);
        window.press("cmd-a", cx);
        window.input("0.0.0.0", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(
            window
                .find("forward-notes")
                .label()
                .is_some_and(|note| note.contains("GatewayPorts"))
        );
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let rule = store.forward(id).unwrap();
        assert_eq!(rule.kind, ForwardKind::Remote);
        assert_eq!(rule.bind, ForwardEndpoint::new("0.0.0.0", 8080));
        assert_eq!(rule.summary(), "0.0.0.0:8080 ← db.internal:3306");
    });
}

#[gpui_kit::test]
async fn a_forward_starts_and_stops_from_its_row_and_from_the_keyboard(cx: &mut TestAppContext) {
    let (mut store, first) = store_with_forward();
    let second = store
        .insert_forward_unnotified(local_forward(WEB_02, 8081))
        .unwrap();
    let provider = Arc::new(FakeForwardProvider::default());
    let (handle, workspace) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    wait_for_forward_status(cx, handle, first, "已停止").await;
    assert!(provider.runs().is_empty());

    // The row's own switch.
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", first.0), cx)
    });
    wait_for_forward_status(cx, handle, first, "运行中").await;
    let runs = provider.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].rule.id, first);
    assert_eq!(runs[0].host, HostId(DB_01));
    in_frame(cx, handle, |window, _| {
        // The title bar says a forward is running, whatever list is up.
        assert_eq!(
            window.find("show-forwards").label(),
            Some("端口转发，1 条运行中")
        );
    });

    // What it carries shows in the row.
    runs[0]
        .events
        .send_blocking(ForwardEvent::Connections(3))
        .unwrap();
    wait_for_forward_status(cx, handle, first, "运行中，3 个连接").await;

    // A forward is on its own: 断开连接 on its host leaves it running,
    // and the host does not read as connected because of it.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(DisconnectHost(HostId(DB_01))), cx);
    });
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.forwards().read(cx).is_active(first));
        let store = workspace.store().read(cx);
        assert!(!store.host(HostId(DB_01)).unwrap().state.is_connected());
    });
    assert_eq!(provider.stopped(), 0);

    // A double click on the row stops it.
    in_frame(cx, handle, |window, cx| {
        window.double_click(("forward-row", first.0), cx)
    });
    wait_for_forward_status(cx, handle, first, "已停止").await;
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        provider.stopped() == 1
    })
    .await;

    // The keyboard: down to the next row, Enter to start it.
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-row", first.0), cx);
        window.press("down", cx);
    });
    in_frame(cx, handle, |window, cx| window.press("enter", cx));
    wait_for_forward_status(cx, handle, second, "运行中").await;
    let runs = provider.runs();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].rule.id, second);
    cx.update(|cx| {
        let forwards = workspace.read(cx).forwards().read(cx);
        assert_eq!(forwards.status(first), ForwardStatus::Stopped);
        assert_eq!(
            forwards.status(second),
            ForwardStatus::Running { connections: 0 }
        );
    });
}

#[gpui_kit::test]
async fn a_forward_question_is_answered_in_a_dialog_and_declining_stops_the_forward(
    cx: &mut TestAppContext,
) {
    let (store, id) = store_with_forward();
    let provider = Arc::new(FakeForwardProvider::with_script(ForwardScript::AsksTrust));
    let (handle, _) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    show_forwards(cx, handle).await;

    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", id.0), cx)
    });
    // The host key is put to the user, as for a terminal.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("cancel").is_some()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("forward-status", id.0)).label(),
            Some("正在连接")
        );
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    wait_for_forward_status(cx, handle, id, "运行中").await;

    // Stop, start again, and decline this time.
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", id.0), cx)
    });
    wait_for_forward_status(cx, handle, id, "已停止").await;
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        provider.stopped() == 1
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", id.0), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("cancel").is_some()
    })
    .await;
    in_frame(cx, handle, |window, cx| window.click("cancel", cx));
    // Declined is stopped, not failed: nothing to report.
    wait_for_forward_status(cx, handle, id, "已停止").await;
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        provider.stopped() == 2
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.notifications(cx).is_empty());
    });
}

#[gpui_kit::test]
async fn stopping_a_forward_takes_its_open_question_with_it(cx: &mut TestAppContext) {
    let (store, id) = store_with_forward();
    let provider = Arc::new(FakeForwardProvider::with_script(ForwardScript::AsksTrust));
    let (handle, _) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StartForward(id)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("cancel").is_some()
    })
    .await;
    // Stopped from elsewhere while the question is up.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StopForward(id)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("cancel").is_none()
    })
    .await;
    wait_for_forward_status(cx, handle, id, "已停止").await;
}

#[gpui_kit::test]
async fn a_forward_that_fails_says_so_in_its_row_and_in_a_notification(cx: &mut TestAppContext) {
    let (store, id) = store_with_forward();
    let provider = Arc::new(FakeForwardProvider::with_script(ForwardScript::Fails(
        "本机端口 8080 已被占用",
    )));
    let (handle, workspace) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    // From the host list: the forward list is not even showing.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StartForward(id)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;
    cx.update(|cx| {
        assert_eq!(
            workspace.read(cx).forwards().read(cx).status(id),
            ForwardStatus::Failed("本机端口 8080 已被占用".into())
        );
    });
    show_forwards(cx, handle).await;
    wait_for_forward_status(cx, handle, id, "已停止：本机端口 8080 已被占用").await;
    in_frame(cx, handle, |window, _| {
        // Nothing is running, so the title bar counts nothing.
        assert_ne!(
            window.find("show-forwards").label(),
            Some("端口转发，1 条运行中")
        );
    });

    // Starting it again clears the old failure while it tries.
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", id.0), cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 2
    })
    .await;
    assert_eq!(provider.runs().len(), 2);
}

#[gpui_kit::test]
async fn a_running_forward_reports_reconnecting_and_giving_up(cx: &mut TestAppContext) {
    let (store, id) = store_with_forward();
    let provider = Arc::new(FakeForwardProvider::default());
    let (handle, _) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", id.0), cx)
    });
    wait_for_forward_status(cx, handle, id, "运行中").await;
    let run = provider.runs().remove(0);

    // One connection that could not be carried is not the forward failing.
    run.events
        .send_blocking(ForwardEvent::ConnectionFailed(
            "服务器无法连接 db.internal:3306".into(),
        ))
        .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find(("forward-status", id.0)).label() == Some("运行中")
    })
    .await;
    in_frame(cx, handle, |window, cx| {
        assert!(window.notifications(cx).is_empty());
    });

    run.events
        .send_blocking(ForwardEvent::Reconnecting {
            attempt: 1,
            of: 3,
            delay: Duration::from_secs(1),
        })
        .unwrap();
    wait_for_forward_status(cx, handle, id, "连接中断，正在重连（1/3）").await;
    in_frame(cx, handle, |window, _| {
        // Still trying counts as running: the switch offers to stop it.
        assert_eq!(
            window.find("show-forwards").label(),
            Some("端口转发，1 条运行中")
        );
    });

    run.events
        .send_blocking(ForwardEvent::Failed("连接中断，重连 3 次均未成功".into()))
        .unwrap();
    wait_for_forward_status(cx, handle, id, "已停止：连接中断，重连 3 次均未成功").await;
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.notifications(cx).len(), 1);
    });
}

#[gpui_kit::test]
async fn deleting_a_host_or_its_group_takes_the_forwards_through_it(cx: &mut TestAppContext) {
    let (mut store, database) = store_with_forward();
    let replica = store
        .insert_forward_unnotified(local_forward(DB_01, 8081))
        .unwrap();
    let web = store
        .insert_forward_unnotified(local_forward(WEB_02, 8082))
        .unwrap();
    let dev = store
        .insert_forward_unnotified(local_forward(DEV_BOX, 8083))
        .unwrap();
    let provider = Arc::new(FakeForwardProvider::default());
    let (handle, workspace) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", database.0), cx)
    });
    wait_for_forward_status(cx, handle, database, "运行中").await;
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", web.0), cx)
    });
    wait_for_forward_status(cx, handle, web, "运行中").await;

    // The host goes, and both of its rules with it; the running one stops.
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(DeleteHost(HostId(DB_01))), cx);
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("forward-row", database.0)).is_none() && provider.stopped() == 1
    })
    .await;
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        let ids: Vec<_> = workspace
            .store()
            .read(cx)
            .forwards()
            .iter()
            .map(|rule| rule.id)
            .collect();
        assert_eq!(ids, [web, dev]);
        assert!(!workspace.forwards().read(cx).is_active(database));
        assert!(!workspace.forwards().read(cx).is_active(replica));
        assert!(workspace.forwards().read(cx).is_active(web));
    });

    // A group takes the rules of every host inside it.
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-row", web.0), cx);
        window.dispatch_action(Box::new(DeleteGroup(GroupId(PRODUCTION))), cx);
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("forward-row", web.0)).is_none() && provider.stopped() == 2
    })
    .await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.forwards().len(), 1);
        assert_eq!(store.forwards()[0].id, dev);
    });
}

#[gpui_kit::test]
async fn a_running_forward_restarts_when_what_it_does_changes(cx: &mut TestAppContext) {
    let (store, id) = store_with_forward();
    let provider = Arc::new(FakeForwardProvider::default());
    let (handle, workspace) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", id.0), cx)
    });
    wait_for_forward_status(cx, handle, id, "运行中").await;
    let store = cx.update(|cx| workspace.read(cx).store().clone());

    // A new name changes nothing the forward does.
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            store.update_forward(id, local_forward(DB_01, 8080).with_name("主库"), cx)
        })
    });
    cx.run_until_parked();
    wait_for_forward_status(cx, handle, id, "运行中").await;
    assert_eq!(provider.runs().len(), 1);
    assert_eq!(provider.stopped(), 0);

    // Another port does: the old run ends before the new one begins.
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            store.update_forward(id, local_forward(DB_01, 9090).with_name("主库"), cx)
        })
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        provider.runs().len() == 2
    })
    .await;
    assert_eq!(provider.stopped(), 1);
    assert_eq!(provider.runs()[1].rule.bind.port, 9090);
    wait_for_forward_status(cx, handle, id, "运行中").await;

    // So does the server moving.
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            let mut draft = store.host(HostId(DB_01)).unwrap().draft();
            draft.address = "10.0.2.99".into();
            store.update(HostId(DB_01), draft, cx)
        })
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        provider.runs().len() == 3
    })
    .await;
    assert_eq!(provider.stopped(), 2);
    wait_for_forward_status(cx, handle, id, "运行中").await;

    // A stopped forward stays stopped through an edit.
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-toggle", id.0), cx)
    });
    wait_for_forward_status(cx, handle, id, "已停止").await;
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            store.update_forward(id, local_forward(DB_01, 9091), cx)
        })
    });
    cx.run_until_parked();
    wait_for_forward_status(cx, handle, id, "已停止").await;
    assert_eq!(provider.runs().len(), 3);
}

#[gpui_kit::test]
async fn forwards_marked_to_start_with_the_application_do(cx: &mut TestAppContext) {
    let (mut store, manual) = store_with_forward();
    let automatic = store
        .insert_forward_unnotified(local_forward(WEB_02, 8081).with_auto_start(true))
        .unwrap();
    let provider = Arc::new(FakeForwardProvider::default());
    let (handle, workspace) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
        provider.runs().len() == 1
    })
    .await;
    assert_eq!(provider.runs()[0].rule.id, automatic);
    show_forwards(cx, handle).await;
    wait_for_forward_status(cx, handle, automatic, "运行中").await;
    cx.update(|cx| {
        let forwards = workspace.read(cx).forwards().read(cx);
        assert_eq!(forwards.status(manual), ForwardStatus::Stopped);
        assert_eq!(forwards.active_count(), 1);
    });
}

#[gpui_kit::test]
async fn forwards_are_read_back_from_the_database(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("shellrs.db");
    let store =
        HostStore::load(HostDatabase::open(&path).expect("database opened")).expect("store loaded");
    let host = HostDraft::new(
        "db-01",
        "10.0.2.5",
        22,
        "postgres",
        AuthKind::Password,
        None,
    );
    let (handle, workspace) =
        open_workspace_with_forwards(cx, store, Arc::new(FakeForwardProvider::default()));
    cx.run_until_parked();
    let store = cx.update(|cx| workspace.read(cx).store().clone());
    let (host, rule) = cx.update(|cx| {
        store.update(cx, |store, cx| {
            let host = store.insert(host, cx);
            let rule = store
                .insert_forward(
                    ForwardDraft::new(
                        ForwardKind::Remote,
                        host,
                        ForwardEndpoint::new("0.0.0.0", 9000),
                        Some(ForwardEndpoint::new("localhost", 3000)),
                    )
                    .with_name("演示站")
                    .with_auto_start(true),
                    cx,
                )
                .expect("the host exists");
            (host, rule)
        })
    });
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        assert!(
            window.notifications(cx).is_empty(),
            "nothing failed to save"
        );
    });

    let reloaded = HostStore::load(HostDatabase::open(&path).expect("database reopened"))
        .expect("store reloaded");
    assert_eq!(reloaded.forwards().len(), 1);
    let saved = &reloaded.forwards()[0];
    assert_eq!(saved.id, rule);
    assert_eq!(saved.host, host);
    assert_eq!(saved.kind, ForwardKind::Remote);
    assert_eq!(saved.name.as_ref(), "演示站");
    assert_eq!(saved.bind, ForwardEndpoint::new("0.0.0.0", 9000));
    assert_eq!(saved.target, Some(ForwardEndpoint::new("localhost", 3000)));
    assert!(saved.auto_start);

    // Deleting the host on disk takes the rule with it.
    cx.update(|cx| store.update(cx, |store, cx| store.remove(host, cx)));
    let reloaded = HostStore::load(HostDatabase::open(&path).expect("database reopened"))
        .expect("store reloaded");
    assert!(reloaded.forwards().is_empty());
}

#[gpui_kit::test]
async fn the_forward_dialog_and_a_long_row_fit_the_smallest_window(cx: &mut TestAppContext) {
    let mut store = HostStore::seed();
    let id = store
        .insert_forward_unnotified(
            local_forward(DB_01, 8080)
                .with_name("生产环境只读副本数据库的本地调试入口（不要对外开放这个端口）"),
        )
        .unwrap();
    let window_size = size(px(960.), px(600.));
    let (handle, _) = open_sized_workspace_with_forwards(
        cx,
        store,
        Arc::new(FakeForwardProvider::default()),
        window_size,
    );
    cx.run_until_parked();
    show_forwards(cx, handle).await;

    in_frame(cx, handle, |window, _| {
        // A long name is cut short; the switch at the row's end stays.
        let list = window.find("forward-list").bounds();
        let toggle = window.find(("forward-toggle", id.0));
        assert!(toggle.visible());
        assert!(
            toggle.bounds().right() <= list.right(),
            "the row's switch must stay inside the list: {:?} in {:?}",
            toggle.bounds(),
            list
        );
        let row = window.find(("forward-row", id.0)).bounds();
        assert!(row.right() <= list.right(), "{row:?} in {list:?}");
    });

    in_frame(cx, handle, |window, cx| {
        window.click(("forward-row", id.0), cx);
        window.dispatch_action(Box::new(EditForward(id)), cx);
    });
    in_frame(cx, handle, |window, _| {
        // Whatever of the body does not fit scrolls; the buttons do not.
        let commit = window.find("commit");
        assert!(commit.visible());
        assert!(
            commit.bounds().bottom() <= window_size.height,
            "the commit button must stay inside the window: {:?}",
            commit.bounds()
        );
        let dialog = window.find("forward-diagram").bounds();
        assert!(dialog.right() <= window_size.width);
    });
}

/// The three kinds are one row of equal cards, each holding its own line.
///
/// The headless text system measures every character at 0.6 em, so Chinese
/// text is narrower here than in the app and this cannot show the cards
/// wrapping on the width of their text, which is what they once did. It
/// holds the structure that prevents it.
#[gpui_kit::test]
async fn the_three_kinds_of_forward_share_one_row(cx: &mut TestAppContext) {
    let (store, id) = store_with_forward();
    let (handle, _) =
        open_workspace_with_forwards(cx, store, Arc::new(FakeForwardProvider::default()));
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("forward-row", id.0), cx);
        window.dispatch_action(Box::new(EditForward(id)), cx);
    });
    in_frame(cx, handle, |window, _| {
        let kinds = window.within("forward-kind");
        let cards = [0usize, 1, 2].map(|ix| kinds.find(ix).bounds());
        for (ix, card) in cards.iter().enumerate() {
            assert_eq!(card.top(), cards[0].top(), "card {ix} is on another row");
            // A third of the row each, to the pixel it rounds to.
            assert_eq!(card.size.height, cards[0].size.height);
            assert!(
                (card.size.width - cards[0].size.width).abs() <= px(1.),
                "card {ix} is another width: {card:?}"
            );
            // The purpose runs the card's whole width inside its padding,
            // under the dot as well as under the title, after its icon.
            let purpose = window.find(("forward-purpose", ix)).bounds();
            let (before, after) = (purpose.left() - card.left(), card.right() - purpose.right());
            assert!(
                before > px(16.) && before < px(32.) && after > px(0.) && after < px(16.),
                "the line of card {ix} does not span it: {purpose:?} in {card:?}"
            );
        }
        assert!(cards[0].right() <= cards[1].left() && cards[1].right() <= cards[2].left());
        // They take the width the picture under them takes, no more.
        let picture = window.find("forward-diagram").bounds();
        assert!(cards[0].left() <= picture.left() && cards[2].right() >= picture.right());
        assert!(cards[2].right() <= picture.right() + (picture.left() - cards[0].left()));
        // Below the picture, the name and the host each take a row.
        let name = window.find("forward-name").bounds();
        let host = window.find("forward-host").bounds();
        assert!(host.top() > name.bottom(), "{host:?} beside {name:?}");
        assert_eq!(
            (host.left(), host.size.width),
            (name.left(), name.size.width)
        );
        // This fixture reduces motion: the picture is still, with no dot.
        assert!(window.try_find("forward-flow-dot").is_none());
    });
}

/// The flow in the forward dialog's picture goes round for as long as the
/// dialog is up: across the tunnel, on to the target, a rest, and again. A
/// round is 3.6 s: 1.44 s a hop and 0.72 s of rest.
#[gpui_kit::test]
async fn the_forward_diagram_keeps_showing_its_flow(cx: &mut TestAppContext) {
    // Every fixture reduces motion, which stills the flow; this test is
    // about the motion.
    let (store, id) = store_with_forward();
    let (handle, _) = open_workspace_with_store(cx, store);
    cx.update(|cx| cx.set_reduce_motion(false));
    cx.run_until_parked();
    // Where the dot is, if it is on either hop.
    let dot = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("forward-flow-dot")
                .map(|dot| dot.bounds().left())
        })
        .unwrap()
    };
    let wait = |cx: &mut TestAppContext, millis: u64| {
        cx.executor().advance_clock(Duration::from_millis(millis));
    };

    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.dispatch_action(Box::new(EditForward(id)), cx);
    });
    let entrance = dot(cx).expect("the flow sets out when the dialog opens");
    wait(cx, 700);
    let on_tunnel = dot(cx).expect("under way on the tunnel");
    assert!(on_tunnel > entrance, "{on_tunnel:?} from {entrance:?}");
    wait(cx, 1400);
    let on_plain_hop = dot(cx).expect("on to the target");
    assert!(on_plain_hop > on_tunnel, "the second hop is further right");
    wait(cx, 900);
    assert_eq!(dot(cx), None, "a rest at the end of the round");
    wait(cx, 1300);
    assert_eq!(dot(cx), Some(on_tunnel), "the next round is the same");
    // Ten rounds on it has not come to rest.
    wait(cx, 36_000);
    assert_eq!(dot(cx), Some(on_tunnel));

    // Another kind is another direction: the flow starts over at its entrance.
    in_frame(cx, handle, |window, cx| {
        window.within("forward-kind").click(1usize, cx);
    });
    assert_eq!(dot(cx), Some(entrance));
    // The same kind again is nothing new.
    wait(cx, 700);
    in_frame(cx, handle, |window, cx| {
        window.within("forward-kind").click(1usize, cx);
    });
    assert_eq!(dot(cx), Some(on_tunnel));
}

#[gpui_kit::test]
async fn disconnecting_a_host_leaves_a_forward_question_open(cx: &mut TestAppContext) {
    let (store, id) = store_with_forward();
    let provider = Arc::new(FakeForwardProvider::with_script(ForwardScript::AsksTrust));
    let (handle, workspace) = open_workspace_with_forwards(cx, store, provider.clone());
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(StartForward(id)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("cancel").is_some()
    })
    .await;
    // 断开连接 is about the host's tabs; the forward goes on asking.
    in_frame(cx, handle, |window, cx| {
        // The dialog holds the focus, inside the workspace.
        window.dispatch_action(Box::new(DisconnectHost(HostId(DB_01))), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("cancel").is_some());
        assert!(window.notifications(cx).is_empty());
        window.click("ok", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace.read(cx).forwards().read(cx).status(id)
            == ForwardStatus::Running { connections: 0 }
    })
    .await;
    assert_eq!(provider.stopped(), 0);
}

#[gpui_kit::test]
async fn a_forward_rows_tooltip_opens_beside_it_and_makes_way_for_its_switch(
    cx: &mut TestAppContext,
) {
    let (store, id) = store_with_forward();
    let (handle, _) =
        open_workspace_with_forwards(cx, store, Arc::new(FakeForwardProvider::default()));
    cx.run_until_parked();
    show_forwards(cx, handle).await;
    let row = ("forward-row", id.0);
    let hover = |cx: &mut TestAppContext, target: ElementId| {
        in_frame(cx, handle, |window, cx| window.hover(target, cx));
        // Tooltips wait half a second before they open.
        cx.executor().advance_clock(Duration::from_millis(1000));
        cx.run_until_parked();
    };

    hover(cx, row.into());
    in_frame(cx, handle, |window, _| {
        let tooltip = window.find("forward-tooltip");
        assert_eq!(
            tooltip.label(),
            Some("本地转发 · 8080 → db.internal:3306 · db-01")
        );
        // Beside the row, as the host list's, so it never covers the rows
        // below.
        let row = window.find(row).bounds();
        assert!(tooltip.bounds().left() >= row.right());
        assert!((tooltip.bounds().center().y - row.center().y).abs() < px(2.));
    });

    // The switch has a tooltip of its own, and the row's makes way for it.
    hover(cx, ("forward-toggle", id.0).into());
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("forward-tooltip").is_none());
    });
    // Back on the rest of the row, it comes back.
    hover(cx, row.into());
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("forward-tooltip").is_some());
    });
}
