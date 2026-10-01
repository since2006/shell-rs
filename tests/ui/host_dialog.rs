//! The host dialog: authentication, 连接方式 (jump hosts and proxies),
//! 备注, passwords in the keychain and 测试连接.

use crate::support::*;

#[gpui_kit::test]
async fn a_host_without_a_password_says_what_it_tries_and_is_saved_as_such(
    cx: &mut TestAppContext,
) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    in_frame(cx, handle, |window, cx| window.click("new-host", cx));
    in_frame(cx, handle, |window, cx| {
        // A new host logs in with a password.
        let sources = window.within("host-auth-source");
        assert_eq!(sources.find(0usize).selected(), Some(true));
        assert!(window.find("host-password").visible());
        assert!(window.try_find("host-no-password-note").is_none());
        window.click("host-name", cx);
        window.input("box", cx);
        window.click("host-address", cx);
        window.input("10.0.0.9", cx);
        window.within("host-auth-source").click(2usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("host-user").visible());
        assert!(window.try_find("host-password").is_none());
        assert!(window.try_find("host-credential").is_none());
        assert_eq!(
            window.find("host-no-password-note").label(),
            Some("依次尝试服务器免认证、SSH Agent 和 ~/.ssh 中的默认私钥。")
        );
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let host = &store.hosts()[0];
        assert_eq!(host.auth, AuthKind::NoPassword);
        assert_eq!(host.credential, None);
        assert_eq!(
            store.login(host.id).unwrap().method,
            LoginMethod::NoPassword
        );
    });
}

/// A store with three hosts to jump through or to, in this order:
/// 阿里云99, 禅道 and 内网库.
fn store_with_jump_hosts() -> (HostStore, [HostId; 3]) {
    let mut store = HostStore::empty();
    let ids = [
        ("阿里云99", "120.25.220.186"),
        ("禅道", "8.138.95.125"),
        ("内网库", "10.0.0.5"),
    ]
    .map(|(name, host)| {
        store.insert_unnotified(HostDraft::new(
            name,
            host,
            22,
            "root",
            AuthKind::Password,
            None,
        ))
    });
    (store, ids)
}

/// Pick the host called `name` with the jump-host picker, the way a person
/// with many hosts would: by searching for it.
fn add_jump_host(cx: &mut TestAppContext, handle: WindowHandle<Root>, name: &str) {
    in_frame(cx, handle, |window, cx| {
        window.click("jump-add-trigger", cx)
    });
    in_frame(cx, handle, |window, cx| window.input(name, cx));
    in_frame(cx, handle, |window, cx| window.press("enter", cx));
}

fn chain(window: &mut gpui_kit::Window) -> Option<String> {
    window.find("host-route-chain").label().map(str::to_string)
}

#[gpui_kit::test]
async fn a_host_goes_through_the_jump_hosts_it_lists_in_order(cx: &mut TestAppContext) {
    let (store, [aliyun, zentao, _]) = store_with_jump_hosts();
    let (handle, workspace) = open_workspace_with_store(cx, store);
    in_frame(cx, handle, |window, cx| window.click("new-host", cx));
    in_frame(cx, handle, |window, cx| {
        // A new host connects directly, with nothing more to fill in.
        let routes = window.within("host-route");
        assert_eq!(routes.find(0usize).selected(), Some(true));
        assert!(window.try_find("host-route-chain").is_none());
        window.click("host-name", cx);
        window.input("db", cx);
        window.click("host-address", cx);
        window.input("10.0.9.9", cx);
        window.within("host-route").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(chain(window).as_deref(), Some("本机 → 当前主机"));
        assert_eq!(
            window.find("host-route-note").label(),
            Some("依次经过跳板主机连接到当前主机，可添加多台。")
        );
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("form-error").label(), Some("请添加跳板主机"));
    });

    add_jump_host(cx, handle, "阿里云");
    add_jump_host(cx, handle, "禅道");
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            chain(window).as_deref(),
            Some("本机 → 阿里云99 → 禅道 → 当前主机")
        );
        assert_eq!(
            window.find(("jump-hop", aliyun.0)).label(),
            Some("阿里云99")
        );
        assert!(window.find(("jump-hop", zentao.0)).visible());
        // Taking the first one off and adding it back puts it last.
        window.click(("remove-jump-hop", 0usize), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(chain(window).as_deref(), Some("本机 → 禅道 → 当前主机"));
        assert!(window.try_find(("jump-hop", aliyun.0)).is_none());
    });
    // Searching by address finds it too.
    add_jump_host(cx, handle, "120.25");
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            chain(window).as_deref(),
            Some("本机 → 禅道 → 阿里云99 → 当前主机")
        );
        // Off the picker first: a focused picker opens on the commit.
        window.click("host-name", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store.hosts().last().unwrap();
        assert_eq!(created.name.as_ref(), "db");
        assert_eq!(created.route, Route::Jump(vec![Some(zentao), Some(aliyun)]));
    });
}

#[gpui_kit::test]
async fn a_deleted_jump_host_keeps_its_place_until_it_is_removed(cx: &mut TestAppContext) {
    let (mut store, [aliyun, zentao, inner]) = store_with_jump_hosts();
    store.update_unnotified(
        inner,
        HostDraft::new("内网库", "10.0.0.5", 22, "root", AuthKind::Password, None)
            .with_route(Route::Jump(vec![Some(aliyun)])),
    );
    store.remove_unnotified(aliyun);
    let (handle, workspace) = open_workspace_with_store(cx, store);
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(inner)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.within("host-route").find(1usize).selected(),
            Some(true)
        );
        assert_eq!(
            chain(window).as_deref(),
            Some("本机 → 已删除的主机 → 当前主机")
        );
        assert_eq!(
            window.find(("jump-hop-deleted", 0usize)).label(),
            Some("已删除的主机")
        );
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("form-error").label(),
            Some("请移除已删除的跳板主机")
        );
        window.click(("remove-jump-hop", 0usize), cx);
    });
    add_jump_host(cx, handle, "禅道");
    in_frame(cx, handle, |window, cx| {
        assert_eq!(chain(window).as_deref(), Some("本机 → 禅道 → 当前主机"));
        window.click("host-name", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(
            store.host(inner).unwrap().route,
            Route::Jump(vec![Some(zentao)])
        );
    });
}

#[gpui_kit::test]
async fn testing_a_connection_through_a_jump_host_sends_its_login(cx: &mut TestAppContext) {
    let (store, _) = store_with_jump_hosts();
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, _) = open_workspace_with_tester(cx, store, tester.clone());
    in_frame(cx, handle, |window, cx| window.click("new-host", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("host-name", cx);
        window.input("db", cx);
        window.click("host-address", cx);
        window.input("10.0.9.9", cx);
        window.within("host-route").click(1usize, cx);
    });
    add_jump_host(cx, handle, "阿里云");
    in_frame(cx, handle, |window, cx| {
        window.click("host-name", cx);
        window.click("test-connection", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;
    let routes = tester.routes();
    let [(LoginRoute::Jump(hops), None)] = routes.as_slice() else {
        panic!("not one test through a jump host: {routes:?}");
    };
    let [JumpLogin::Host { name, login }] = hops.as_slice() else {
        panic!("not one jump host: {hops:?}");
    };
    assert_eq!(name, "阿里云99");
    assert_eq!(
        **login,
        HostLogin::manual("120.25.220.186", 22, "root", AuthKind::Password)
    );
}

#[gpui_kit::test]
async fn a_hosts_notes_take_several_lines_and_come_back_when_edited(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_store(cx, HostStore::empty());
    in_frame(cx, handle, |window, cx| window.click("new-host", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("host-name", cx);
        window.input("db", cx);
        window.click("host-address", cx);
        window.input("10.0.9.9", cx);
        window.click("host-notes", cx);
        window.input("机房 A", cx);
        // Enter starts a new line; it does not submit the dialog.
        window.press("enter", cx);
        window.input("负责人：张三", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("commit").visible());
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    let id = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let host = &store.hosts()[0];
        assert_eq!(host.notes.as_ref(), "机房 A\n负责人：张三");
        host.id
    });

    // Edited, the notes are there to change. The closed dialog took the
    // focus with it, so the list takes it back first.
    in_frame(cx, handle, |window, cx| {
        window.click(("host-row", id.0), cx);
        window.dispatch_action(Box::new(EditHost(id)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        window.click("host-notes", cx);
        window.press("cmd-a", cx);
        window.input("已下线", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.host(id).unwrap().notes.as_ref(), "已下线");
    });
}

#[gpui_kit::test]
async fn deleting_a_jump_host_leaves_the_connection_behind_it_alone(cx: &mut TestAppContext) {
    let (mut store, [aliyun, _, inner]) = store_with_jump_hosts();
    store.update_unnotified(
        inner,
        HostDraft::new("内网库", "10.0.0.5", 22, "root", AuthKind::Password, None)
            .with_route(Route::Jump(vec![Some(aliyun)])),
    );
    let remote = Arc::new(RecordingRemoteProvider::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store,
        remote.clone(),
        Arc::new(FakeConnectionTester::default()),
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(inner)), cx)
    });
    // The terminal is given the way there, jump host and all.
    let logins = remote.logins();
    let [login] = logins.as_slice() else {
        panic!("not one connection: {logins:?}");
    };
    assert!(
        matches!(&login.route, LoginRoute::Jump(hops)
            if matches!(hops.as_slice(), [JumpLogin::Host { name, .. }] if name == "阿里云99")),
        "{:?}",
        login.route
    );

    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(DeleteHost(aliyun)), cx)
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.run_until_parked();
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert!(store.host(aliyun).is_none());
        assert_eq!(store.host(inner).unwrap().route, Route::Jump(vec![None]));
    });
    // Not reconnected: that would only fail now.
    assert_eq!(remote.logins().len(), 1);
}

#[gpui_kit::test]
async fn a_host_behind_a_proxy_keeps_the_proxys_password_in_the_keychain(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, workspace) = open_workspace_with_tester(
        cx,
        HostStore::empty().with_secrets(secrets.clone()),
        tester.clone(),
    );
    in_frame(cx, handle, |window, cx| window.click("new-host", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("host-name", cx);
        window.input("abroad", cx);
        window.click("host-address", cx);
        window.input("203.0.113.7", cx);
        window.within("host-route").click(2usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-proxy-kind").value(), Some("HTTP 代理"));
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("form-error").label(), Some("请输入代理地址"));
        window.within("host-proxy-kind").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("host-proxy-kind").value(), Some("SOCKS5 代理"));
        window.click("host-proxy-host", cx);
        window.input("127.0.0.1", cx);
        window.click("host-proxy-port", cx);
        window.input("7890", cx);
        window.click("host-proxy-password", cx);
        window.input("hunter2", cx);
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("form-error").label(),
            Some("填写代理密码时请同时填写用户名")
        );
        window.click("host-proxy-user", cx);
        window.input("me", cx);
        window.click("test-connection", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;
    let proxy = ProxySettings::new(ProxyKind::Socks5, "127.0.0.1", 7890).with_user("me");
    // The test takes the password from the form, saved or not.
    assert_eq!(
        tester.routes(),
        [(
            LoginRoute::Proxy((&proxy).into()),
            Some("hunter2".to_string())
        )]
    );
    in_frame(cx, handle, |window, cx| window.click("commit", cx));
    wait_for_dialog_to_close(cx, handle).await;

    assert_eq!(
        secrets
            .get(&SecretRef::proxy("me", "127.0.0.1", 7890))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2")
    );
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.hosts()[0].route, Route::Proxy(proxy));
    });
}

#[gpui_kit::test]
async fn connection_edits_reconnect_once_but_display_edits_do_not(cx: &mut TestAppContext) {
    let (store, id) = one_host_store(AuthKind::Password);
    let factory = Arc::new(FakeTerminalFactory::default());
    let (handle, workspace) = open_workspace_with_remote_factory(cx, store, factory.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window
            .within("host-tree")
            .double_click(("host-row", id.0), cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 1
            && window.find("status-connection").label() == Some("已连接 prompt-host")
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        store.update(cx, |store, cx| {
            let mut draft = store.host(id).unwrap().draft();
            draft.address = "new.example.test".into();
            assert!(store.update(id, draft, cx));
        });
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        factory.starts() == 2
            && window.find("status-connection").label() == Some("已连接 prompt-host")
    })
    .await;
    assert_eq!(factory.starts(), 2);

    cx.update(|cx| {
        let store = workspace.read(cx).store().clone();
        store.update(cx, |store, cx| {
            let mut draft = store.host(id).unwrap().draft();
            draft.name = "renamed".into();
            assert!(store.update(id, draft, cx));
        });
    });
    cx.run_until_parked();
    assert_eq!(factory.starts(), 2);
}

/// The error is painted above the dialog and its backdrop, not under them:
/// a click on it reaches it, and puts it away, instead of landing on the
/// backdrop.
#[gpui_kit::test]
async fn a_form_error_sits_above_the_dialog_and_a_click_puts_it_away(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| window.click("new-host", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("host-name", cx);
        window.click("commit", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.try_find("form-error").is_some()
    })
    .await;

    in_frame(cx, handle, |window, cx| window.click("form-error", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).is_empty()
    })
    .await;
    in_frame(cx, handle, |window, _| {
        assert!(window.find("commit").visible(), "the dialog stays open");
    });
}

#[gpui_kit::test]
async fn new_host_dialog_validates_then_inserts(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-host", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        assert!(window.try_find("form-error").is_none());

        // The address and its port share a row; the name above them and the
        // user name below each take the row's whole width.
        let [name, host, port, user] = ["host-name", "host-address", "host-port", "host-user"]
            .map(|id| window.find(id).bounds());
        assert_eq!(host.top(), port.top());
        assert!(host.right() < port.left() && host.size.width > port.size.width);
        assert!(name.bottom() < host.top() && user.top() > host.bottom());
        assert_eq!((name.left(), name.right()), (host.left(), port.right()));
        assert_eq!((user.left(), user.right()), (name.left(), name.right()));

        // An empty form is rejected and the dialog stays open. The commit
        // action is dispatched deferred, so the error shows after effects run.
        // The dialog's focus trap owns focus until a field is clicked.
        window.click("host-name", cx);
        assert_eq!(window.find("host-name").focused(), Some(true));
        window.click("commit", cx);
    })
    .unwrap();
    // The commit action is dispatched deferred; wait for its error line.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window
            .try_find("form-error")
            .is_some_and(|error| error.visible())
    })
    .await;

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
        // The error pops up as a notification over the dialog, not as a line
        // at the bottom of the form.
        assert_eq!(window.notifications(cx).len(), 1);
        assert_eq!(window.find("form-error").label(), Some("请输入名称"));
        window.click("commit", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Trying again replaces it instead of stacking another.
        assert_eq!(window.notifications(cx).len(), 1);

        window.click("host-name", cx);
        window.input("db-02", cx);
        window.click("host-address", cx);
        window.input("10.0.3.7", cx);
        window.click("commit", cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    // The error was about the dialog, which is gone: so is the error.
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).is_empty() && window.try_find("form-error").is_none()
    })
    .await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .hosts()
            .iter()
            .find(|host| host.name == "db-02")
            .expect("db-02 inserted");
        assert_eq!(created.address.as_ref(), "10.0.3.7");
        assert_eq!(created.port, 22);
    });
}

/// What the user reported: the keychain has the working password, the edit
/// dialog's field has been changed to a wrong one, and 「测试连接」 must try the
/// field — without saving anything.
#[gpui_kit::test]
async fn testing_a_connection_logs_in_with_what_the_form_shows(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let tester = Arc::new(FakeConnectionTester::failing("用户名或密码错误"));
    let (handle, workspace) = open_workspace_with_tester(cx, store, tester.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditHost(id)), cx);
    })
    .unwrap();
    // The saved password is read on a background thread, then fills the field.
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("host-password", cx);
        window.press("cmd-a", cx);
        window.input("wrong-password", cx);
        window.click("host-port", cx);
        window.press("cmd-a", cx);
        window.input("2222", cx);
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;

    assert_eq!(
        tester.requests(),
        [(
            "10.0.2.5".to_string(),
            2222,
            "postgres".to_string(),
            Some("wrong-password".to_string())
        )]
    );
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Nothing was saved, and the dialog stays open to fix the field.
        assert!(window.find("commit").visible());
    })
    .unwrap();
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.host(id).unwrap().port, 22);
    });
    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2"),
        "测试连接不写钥匙串"
    );
}

#[gpui_kit::test]
async fn a_connection_test_needs_a_host_and_a_user_first(cx: &mut TestAppContext) {
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, _) = open_workspace_with_tester(cx, HostStore::empty(), tester.clone());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-host", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let test = window.find("test-connection").bounds();
        let cancel = window.find("cancel").bounds();
        let commit = window.find("commit").bounds();
        assert!(test.right() < cancel.left());
        assert!(cancel.right() < commit.left());
        assert!(window.notifications(cx).is_empty());
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // Reported at once as a failed test; the form shows no result of its own.
        assert_eq!(window.notifications(cx).len(), 1);
        assert!(window.try_find("form-error").is_none());
        window.click("host-address", cx);
        window.input("10.0.3.7", cx);
        window.click("host-user", cx);
        #[cfg(target_os = "macos")]
        window.press("cmd-a", cx);
        #[cfg(not(target_os = "macos"))]
        window.press("ctrl-a", cx);
        window.press("backspace", cx);
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.notifications(cx).len(), 2, "没有用户名也不该去连");
    })
    .unwrap();
    assert!(tester.requests().is_empty());
}

#[gpui_kit::test]
async fn a_first_seen_host_key_is_put_to_the_user_above_the_host_dialog(cx: &mut TestAppContext) {
    let tester = Arc::new(FakeConnectionTester::asking_trust());
    let (store, id) = one_host_store(AuthKind::Password);
    let (handle, _) = open_workspace_with_tester(cx, store, tester.clone());

    // Trust: the question reaches the tester as a yes.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditHost(id)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.try_find("ok").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;
    assert_eq!(tester.trust_answers(), [true]);

    // Escape dismisses the question, which declines it; the host dialog
    // underneath stays open.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("ok").is_none());
        window.click("test-connection", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.try_find("ok").is_some()
    })
    .await;
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 2
    })
    .await;
    assert_eq!(tester.trust_answers(), [true, false]);
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("commit").visible());
    })
    .unwrap();
}

/// A store holding one host, wired to a keychain the test can inspect.
fn store_with_secrets(secrets: Arc<InMemorySecretStore>) -> (HostStore, HostId, SecretRef) {
    let mut store = HostStore::empty();
    let id = store.insert_unnotified(HostDraft::new(
        "db-01",
        "10.0.2.5",
        22,
        "postgres",
        AuthKind::Password,
        None,
    ));
    let endpoint = store.host(id).unwrap().password_secret();
    (store.with_secrets(secrets), id, endpoint)
}

#[gpui_kit::test]
async fn a_new_host_saves_its_password_to_the_keychain(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) =
        open_workspace_with_store(cx, HostStore::empty().with_secrets(secrets.clone()));

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("new-host", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("host-name", cx);
        window.input("db-02", cx);
        window.click("host-address", cx);
        window.input("10.0.3.7", cx);
        window.click("host-password", cx);
        window.input("hunter2", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&SecretRef::password("root", "10.0.3.7", 22))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2")
    );

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .hosts()
            .iter()
            .find(|host| host.name == "db-02")
            .expect("db-02 inserted");
        assert!(
            !format!("{created:?}").contains("hunter2"),
            "主机本身不该带着密码"
        );
    });
}

#[gpui_kit::test]
async fn editing_the_host_moves_the_saved_password_with_it(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditHost(id)), cx);
    })
    .unwrap();
    // The saved password is read on a background thread, then fills the field.
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("host-password").visible());
        window.click("host-address", cx);
        window.press("cmd-a", cx);
        window.input("10.9.9.9", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&SecretRef::password("postgres", "10.9.9.9", 22))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2"),
        "预填的密码应当跟着主机搬到新端点"
    );
    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        None,
        "没有主机再用旧端点了，旧条目应当被清掉"
    );
}

#[gpui_kit::test]
async fn clearing_the_password_field_forgets_the_saved_password(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(EditHost(id)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("host-password", cx);
        window.press("cmd-a", cx);
        window.press("backspace", cx);
        window.click("commit", cx);
    })
    .unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find("commit").is_none()
    })
    .await;
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        None,
        "清空字段就是要删掉已保存的密码"
    );
}

#[gpui_kit::test]
async fn deleting_the_last_host_on_an_endpoint_forgets_its_password(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, id, endpoint) = store_with_secrets(secrets.clone());
    secrets.set(&endpoint, "hunter2").unwrap();
    let (handle, _) = open_workspace_with_store(cx, store);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.dispatch_action(Box::new(DeleteHost(id)), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("ok", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(
        secrets
            .get(&endpoint)
            .unwrap()
            .as_deref()
            .map(String::as_str),
        None,
        "最后一个用这个端点的主机没了，密码也该没了"
    );
}

#[gpui_kit::test]
async fn a_dialogs_choices_are_equal_segments_of_one_track(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_credentials(
        cx,
        HostStore::seed(),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    in_frame(cx, handle, |window, cx| window.click("new-host", cx));
    in_frame(cx, handle, |window, cx| {
        let name = window.find("host-name").bounds();
        let mut group = window.within("host-auth-source");
        let segments = [0usize, 1, 2].map(|ix| group.find(ix));
        let selected: Vec<_> = segments.iter().map(|segment| segment.selected()).collect();
        assert_eq!(selected, [Some(true), Some(false), Some(false)]);
        let [password, saved, keyless] = segments.map(|segment| segment.bounds());
        // Side by side in one row, sharing the track equally (to the
        // layout's rounding). The track spans the form like the name field
        // above it, and the segments fill it but for its inset.
        for segment in [saved, keyless] {
            assert_eq!(segment.top(), password.top());
            assert_eq!(segment.size.height, password.size.height);
            assert!((segment.size.width - password.size.width).abs() <= px(1.));
        }
        assert!(password.right() <= saved.left() && saved.right() <= keyless.left());
        assert!(name.left() < password.left() && keyless.right() < name.right());
        assert!(password.size.width * 3. > name.size.width * 0.95);
        group.click(1usize, cx);
    });
    in_frame(cx, handle, |window, _| {
        let group = window.within("host-auth-source");
        assert_eq!(group.find(1usize).selected(), Some(true));
        assert_eq!(group.find(0usize).selected(), Some(false));
        assert!(window.find("host-credential").visible());
    });
}
