//! The host tree: search, groups, dragging, the row tooltip and the
//! commands on a row.

use crate::support::*;

#[gpui_kit::test]
fn search_filters_the_tree(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window
                .within("host-tree")
                .try_find(("host-row", WEB_01))
                .is_some()
        );
        window.click("host-search", cx);
        window.input("staging", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tree = window.within("host-tree");
        assert!(tree.try_find(("host-row", STAGING_API)).is_some());
        assert!(tree.try_find(("host-row", WEB_01)).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn search_finds_a_group_by_name_with_all_of_it(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.click("host-search", cx);
        window.input("生产", cx);
    });
    in_frame(cx, handle, |window, _| {
        let tree = window.within("host-tree");
        assert!(tree.try_find(("group-row", PRODUCTION)).is_some());
        for host in [WEB_01, WEB_02, DB_01] {
            assert!(tree.try_find(("host-row", host)).is_some(), "{host}");
        }
        assert!(tree.try_find(("host-row", STAGING_API)).is_none());
        assert!(tree.try_find(("group-row", DEVELOPMENT)).is_none());
    });
}

#[gpui_kit::test]
fn group_expansion_survives_reopening_the_database(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = HostDatabase::open(&path).unwrap();
    let seed = HostStore::seed();
    for group in seed.groups() {
        database.insert_group(group).unwrap();
    }
    for host in seed.hosts() {
        database.insert_host(host).unwrap();
    }
    let (handle, _) = open_workspace_with_store(cx, HostStore::load(database).unwrap());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("host-row", WEB_01)).visible());
        window.click(("group-row", PRODUCTION), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", PRODUCTION)).visible());
        assert!(window.try_find(("host-row", WEB_01)).is_none());
    })
    .unwrap();
    let database = HostDatabase::open(&path).unwrap();
    assert!(
        !database
            .load()
            .unwrap()
            .groups
            .iter()
            .find(|group| group.id == GroupId(PRODUCTION))
            .unwrap()
            .expanded
    );

    let (reopened, _) = open_workspace_with_store(cx, HostStore::load(database).unwrap());
    cx.update_window(reopened.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", PRODUCTION)).visible());
        assert!(window.try_find(("host-row", WEB_01)).is_none());
        assert!(window.find(("host-row", STAGING_API)).visible());
        window.click(("group-row", PRODUCTION), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(reopened.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("host-row", WEB_01)).visible());
    })
    .unwrap();
    let saved = HostDatabase::open(&path).unwrap().load().unwrap();
    assert!(
        saved
            .groups
            .iter()
            .find(|group| group.id == GroupId(PRODUCTION))
            .unwrap()
            .expanded
    );
}

#[gpui_kit::test]
fn expand_and_collapse_all_groups_include_nested_groups_and_persist(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = HostDatabase::open(&path).unwrap();
    let mut seed = HostStore::seed();
    let nested =
        seed.insert_group_unnotified(GroupDraft::new("内部服务", Some(GroupId(PRODUCTION))));
    for group in seed.groups() {
        database.insert_group(group).unwrap();
    }
    for host in seed.hosts() {
        database.insert_host(host).unwrap();
    }
    let (handle, _) = open_workspace_with_store(cx, HostStore::load(database).unwrap());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", nested.0)).visible());
        assert!(window.find(("host-row", WEB_01)).visible());
        window.dispatch_action(Box::new(CollapseAllGroups), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", PRODUCTION)).visible());
        assert!(window.try_find(("group-row", nested.0)).is_none());
        assert!(window.try_find(("host-row", WEB_01)).is_none());
    })
    .unwrap();
    assert!(
        HostDatabase::open(&path)
            .unwrap()
            .load()
            .unwrap()
            .groups
            .iter()
            .all(|group| !group.expanded)
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.dispatch_action(Box::new(ExpandAllGroups), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", nested.0)).visible());
        assert!(window.find(("host-row", WEB_01)).visible());
    })
    .unwrap();
    let database = HostDatabase::open(&path).unwrap();
    assert!(
        database
            .load()
            .unwrap()
            .groups
            .iter()
            .all(|group| group.expanded)
    );
    let (reopened, _) = open_workspace_with_store(cx, HostStore::load(database).unwrap());
    cx.update_window(reopened.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("group-row", nested.0)).visible());
        assert!(window.find(("host-row", WEB_01)).visible());
    })
    .unwrap();
}

#[gpui_kit::test]
fn connect_group_opens_each_host_in_its_subtree(cx: &mut TestAppContext) {
    let mut store = HostStore::seed();
    let child =
        store.insert_group_unnotified(GroupDraft::new("内部服务", Some(GroupId(PRODUCTION))));
    let grandchild = store.insert_group_unnotified(GroupDraft::new("后端", Some(child)));
    let nested_host = store.insert_unnotified(HostDraft::new(
        "backend-01",
        "10.0.3.8",
        22,
        "deploy",
        AuthKind::Password,
        Some(grandchild),
    ));
    let (handle, workspace) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectGroup(child)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.terminal_count(nested_host, cx), 1);
        assert_eq!(workspace.terminal_count(HostId(WEB_01), cx), 1);
        assert_eq!(workspace.terminal_count(HostId(DB_01), cx), 0);
        assert_eq!(workspace.terminal_count(HostId(STAGING_API), cx), 1);
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(ConnectGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert_eq!(workspace.terminal_count(nested_host, cx), 2);
        assert_eq!(workspace.terminal_count(HostId(WEB_01), cx), 2);
        assert_eq!(workspace.terminal_count(HostId(WEB_02), cx), 1);
        assert_eq!(workspace.terminal_count(HostId(DB_01), cx), 1);
        assert_eq!(workspace.terminal_count(HostId(STAGING_API), cx), 1);
        assert_eq!(workspace.terminal_count(HostId(DEV_BOX), cx), 0);
    });
}

#[gpui_kit::test]
fn dragging_a_host_into_a_group_updates_the_host_tree(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("group-count", PRODUCTION)).label(),
            Some("3 台主机")
        );
        assert_eq!(
            window.find(("group-count", DEVELOPMENT)).label(),
            Some("1 台主机")
        );
        window
            .within("host-tree")
            .drag_to(("host-row", DB_01), ("group-row", DEVELOPMENT), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(
            store.host(HostId(DB_01)).unwrap().group,
            Some(GroupId(DEVELOPMENT))
        );
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("group-count", PRODUCTION)).label(),
            Some("2 台主机")
        );
        assert_eq!(
            window.find(("group-count", DEVELOPMENT)).label(),
            Some("2 台主机")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn dragging_peers_changes_their_order_and_groups_can_nest(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .drag_to(("host-row", DB_01), ("host-row", WEB_01), cx);
        window.within("host-tree").drag_to(
            ("group-row", DEVELOPMENT),
            ("group-row", PRODUCTION),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(
            store.group(GroupId(DEVELOPMENT)).unwrap().parent,
            Some(GroupId(PRODUCTION))
        );
        let db = store.host(HostId(DB_01)).unwrap();
        let web = store.host(HostId(WEB_01)).unwrap();
        let web02 = store.host(HostId(2)).unwrap();
        assert!(web.sort_order < db.sort_order && db.sort_order < web02.sort_order);
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find(("group-count", PRODUCTION)).label(),
            Some("4 台主机")
        );
        assert_eq!(
            window.find(("group-count", DEVELOPMENT)).label(),
            Some("1 台主机")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn dragging_a_host_to_blank_tree_space_moves_it_to_the_root(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let from = window.find(("host-row", DB_01)).bounds().center();
        let tree = window.find("host-tree").bounds();
        window.drag(from, point(tree.center().x, tree.bottom() - px(12.)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.host(HostId(DB_01)).unwrap().group, None);
    });
}

#[gpui_kit::test]
fn dragged_order_survives_reopening_the_database(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shellrs.db");
    let database = HostDatabase::open(&path).unwrap();
    let seed = HostStore::seed();
    for group in seed.groups() {
        database.insert_group(group).unwrap();
    }
    for host in seed.hosts() {
        database.insert_host(host).unwrap();
    }
    let (handle, _) = open_workspace_with_store(cx, HostStore::load(database).unwrap());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .drag_to(("host-row", DB_01), ("host-row", WEB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    let reopened = HostDatabase::open(&path).unwrap().load().unwrap();
    let order = |id| {
        reopened
            .hosts
            .iter()
            .find(|host| host.id == HostId(id))
            .unwrap()
            .sort_order
    };
    assert!(order(WEB_01) < order(DB_01));
    assert!(order(DB_01) < order(2));
}

#[gpui_kit::test]
async fn new_group_from_the_toolbar_appears_in_the_tree(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-group", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        // An unnamed group is rejected and the dialog stays open.
        window.click("group-name", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("form-error").visible());
        window.click("group-name", cx);
        window.input("预发", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let created = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store.groups().last().expect("group inserted");
        assert_eq!(created.name.as_ref(), "预发");
        assert_eq!(created.parent, None);
        created.id
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tree = window.within("host-tree");
        assert!(tree.find(("group-row", created.0)).visible());
        assert_eq!(
            tree.find(("group-count", created.0)).label(),
            Some("0 台主机")
        );
    })
    .unwrap();
}

/// The action a group row's 新建主机… menu entry dispatches.
#[gpui_kit::test]
async fn a_new_host_in_a_group_starts_out_in_that_group(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(NewHostInGroup(GroupId(DEVELOPMENT))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("host-name", cx);
        window.input("dev-02", cx);
        window.click("host-address", cx);
        window.input("192.168.1.21", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .hosts()
            .iter()
            .find(|host| host.name == "dev-02")
            .expect("dev-02 inserted");
        // The form opened with 开发 pre-selected and nothing changed it.
        assert_eq!(created.group, Some(GroupId(DEVELOPMENT)));
    });
}

#[gpui_kit::test]
async fn renaming_a_group_keeps_the_hosts_under_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(RenameGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        window.click("group-name", cx);
        window.press("cmd-a", cx);
        window.input("生产环境", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let group = store.group(GroupId(PRODUCTION)).expect("group kept");
        assert_eq!(group.name.as_ref(), "生产环境");
        assert_eq!(store.group_path(GroupId(PRODUCTION)), "生产环境");
        // The three hosts still belong to it.
        assert_eq!(
            store
                .hosts()
                .iter()
                .filter(|s| s.group == Some(GroupId(PRODUCTION)))
                .count(),
            3
        );
    });
}

#[gpui_kit::test]
async fn deleting_a_group_removes_its_hosts_and_closes_their_tabs(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    // db-01 joins web-01, which starts connected, in having an open tab.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", DB_01), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(HostId(WEB_01), cx).is_some());
        assert!(workspace.terminal(HostId(DB_01), cx).is_some());
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DeleteGroup(GroupId(PRODUCTION))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let workspace = workspace.read(cx);
        assert!(workspace.terminal(HostId(WEB_01), cx).is_none());
        assert!(workspace.terminal(HostId(DB_01), cx).is_none());
        // staging-api is in another group and keeps its tab.
        assert!(workspace.terminal(HostId(STAGING_API), cx).is_some());

        let store = workspace.store().read(cx);
        assert!(store.group(GroupId(PRODUCTION)).is_none());
        assert_eq!(store.groups().len(), 2);
        let names: Vec<_> = store
            .hosts()
            .iter()
            .map(|host| host.name.as_ref())
            .collect();
        assert_eq!(names, ["staging-api", "qa-runner", "dev-box"]);
    });
}

#[gpui_kit::test]
async fn groups_and_hosts_are_read_back_from_the_database(cx: &mut TestAppContext) {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("shellrs.db");
    let store =
        HostStore::load(HostDatabase::open(&path).expect("database opened")).expect("store loaded");
    // A first launch starts with nothing at all.
    assert_eq!(store.groups().len(), 0);
    assert_eq!(store.hosts().len(), 0);
    let (handle, workspace) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-group", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("group-name", cx);
        window.input("生产", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let group = cx.update(|cx| workspace.read(cx).store().read(cx).groups()[0].id);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // A closed dialog leaves nothing focused, and an action only reaches
        // handlers on the focused element's path.
        window.click("host-search", cx);
        window.dispatch_action(Box::new(NewHostInGroup(group)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("host-name", cx);
        window.input("web-01", cx);
        window.click("host-address", cx);
        window.input("10.0.1.12", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;

    let host = cx.update(|cx| workspace.read(cx).store().read(cx).hosts()[0].id);

    // Connecting is what puts a host on the start page's recent list.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", host.0), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.find("status-connection").label() == Some("已连接 web-01")
    })
    .await;

    // Everything above went through the real write path; read it back with a
    // second connection to the same file.
    let reloaded = HostStore::load(HostDatabase::open(&path).expect("database reopened"))
        .expect("store reloaded");
    assert_eq!(reloaded.groups().len(), 1);
    assert_eq!(reloaded.groups()[0].name.as_ref(), "生产");
    assert_eq!(reloaded.hosts().len(), 1);
    let saved = &reloaded.hosts()[0];
    assert_eq!(saved.name.as_ref(), "web-01");
    assert_eq!(saved.address.as_ref(), "10.0.1.12");
    assert_eq!(saved.port, 22);
    assert_eq!(saved.group, Some(group));
    // Runtime state is not persisted, but the last connection time is.
    assert!(!saved.state.is_connected());
    assert_eq!(
        reloaded.recent_hosts().map(|s| s.id).collect::<Vec<_>>(),
        [host]
    );
}

#[gpui_kit::test]
fn copy_host_address_puts_the_host_on_the_clipboard(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(CopyHostAddress(HostId(STAGING_API))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    // Only the host field: no user, no port.
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("10.0.9.20".to_string())
    );
}

#[gpui_kit::test]
fn hovering_a_host_row_shows_its_address_beside_the_row(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    let row = ("host-row", WEB_01);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.hover(row, cx);
    })
    .unwrap();
    // Tooltips wait half a second before they open.
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let tooltip = window.find("host-tooltip");
        assert_eq!(tooltip.label(), Some("root@10.0.1.12:22"));
        // Beside the row, so it never covers the rows below.
        let row_bounds = window.find(row).bounds();
        assert!(tooltip.bounds().left() >= row_bounds.right());
        assert!((tooltip.bounds().center().y - row_bounds.center().y).abs() < px(2.));
        // A click puts it away.
        window.click(row, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("host-tooltip").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_host_rows_tooltip_shows_its_notes_under_the_address(cx: &mut TestAppContext) {
    let mut store = HostStore::seed();
    let web = HostId(WEB_01);
    let draft = store
        .host(web)
        .unwrap()
        .draft()
        .with_notes("机房 A\n负责人：张三");
    store.update_unnotified(web, draft);
    let (handle, _) = open_workspace_with_store(cx, store);
    let hover = |cx: &mut TestAppContext, id: u64| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.hover(("host-row", id), cx);
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_millis(1000));
        cx.run_until_parked();
    };

    hover(cx, WEB_01);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let address = window.find("host-tooltip");
        let notes = window.find("host-tooltip-note");
        assert_eq!(address.label(), Some("root@10.0.1.12:22"));
        assert_eq!(notes.label(), Some("机房 A\n负责人：张三"));
        assert!(notes.bounds().top() >= address.bounds().bottom());
    })
    .unwrap();

    // A host without notes shows its address alone.
    hover(cx, WEB_02);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("host-tooltip").label(),
            Some("root@10.0.1.13:22")
        );
        assert!(window.try_find("host-tooltip-note").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_row_tooltip_follows_the_pointer_down_the_list(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    let move_to = |cx: &mut TestAppContext, id: u64| {
        cx.update_window(handle.into(), |_, window, cx| {
            let row = window.find(("host-row", id)).bounds();
            window.dispatch_event(
                gpui_kit::PlatformInput::MouseMove(MouseMoveEvent {
                    position: point(row.left() + px(40.), row.center().y),
                    pressed_button: None,
                    modifiers: Default::default(),
                }),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        cx.executor().advance_clock(Duration::from_millis(1000));
        cx.run_until_parked();
    };
    let tooltip = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("host-tooltip")
                .and_then(|tooltip| tooltip.label().map(str::to_string))
        })
        .unwrap()
    };

    cx.update_window(handle.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    move_to(cx, WEB_01);
    assert_eq!(tooltip(cx).as_deref(), Some("root@10.0.1.12:22"));
    // Down the list the next row hears of the pointer before this one lets
    // go of it; the tooltip must survive that and move with the pointer.
    move_to(cx, WEB_02);
    assert_eq!(tooltip(cx).as_deref(), Some("root@10.0.1.13:22"));
    move_to(cx, WEB_01);
    assert_eq!(tooltip(cx).as_deref(), Some("root@10.0.1.12:22"));
}

#[gpui_kit::test]
fn copy_host_id_puts_the_public_id_on_the_clipboard(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let public_id = workspace.read_with(cx, |workspace, cx| {
        workspace
            .store()
            .read(cx)
            .host(HostId(STAGING_API))
            .unwrap()
            .public_id
            .to_string()
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(CopyHostId(HostId(STAGING_API))), cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(public_id.len(), 16);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(public_id)
    );
}

#[gpui_kit::test]
async fn the_tree_falls_back_to_the_first_letter_until_a_host_is_probed(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let probed = store.insert_unnotified(HostDraft::new(
        "web-01",
        "10.0.1.12",
        22,
        "root",
        AuthKind::Password,
        None,
    ));
    let fresh = store.insert_unnotified(HostDraft::new(
        "数据库",
        "10.0.2.5",
        22,
        "root",
        AuthKind::Password,
        None,
    ));
    store.set_host_os_unnotified(probed, Some(HostOs::Debian));
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("host-os", probed.0)).label(), Some("Debian"));
        assert_eq!(
            window.find(("host-os", fresh.0)).label(),
            Some("未探测到系统"),
            "没探测过就退回名称首字的中性徽章"
        );
        // The badges carry brand colours of their own, so they have to hold up
        // in both themes.
        window.click("theme-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find(("host-os", probed.0)).visible());
        assert!(window.find(("host-os", fresh.0)).visible());
    })
    .unwrap();
}
