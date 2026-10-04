//! 凭据: the sidebar list, the dialog with its kinds and key sources, and
//! the hosts that log in with a credential.

use crate::support::*;

/// A store with one password credential, 「运维」 as `deploy`, and the host
/// db-01 logging in with it; the keychain holds the credential's password.
fn store_with_credential(secrets: Arc<InMemorySecretStore>) -> (HostStore, CredentialId, HostId) {
    let mut store = HostStore::empty();
    let credential = store.insert_credential_unnotified(CredentialDraft::new(
        "运维",
        CredentialKind::Password,
        "deploy",
    ));
    secrets
        .set(
            &store.credential(credential).unwrap().password_secret(),
            "hunter2",
        )
        .unwrap();
    let host = store.insert_unnotified(
        HostDraft::new("db-01", "10.0.2.5", 22, "root", AuthKind::Password, None)
            .with_credential(credential),
    );
    (store.with_secrets(secrets), credential, host)
}

#[gpui_kit::test]
async fn the_title_bar_switches_the_sidebar_to_credentials(cx: &mut TestAppContext) {
    let (handle, _) = open_workspace_with_credentials(
        cx,
        HostStore::seed(),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("show-credentials").checked(), Some(true));
        assert_eq!(window.find("show-hosts").checked(), Some(false));
        assert_eq!(window.find("show-forwards").checked(), Some(false));
        assert_eq!(window.find("credential-empty").label(), Some("还没有凭据"));
        // The dock's toolbar follows the list; the settings footer stays.
        assert!(window.find("new-credential").visible());
        assert!(window.try_find("new-group").is_none());
        assert!(window.find("open-settings").visible());
        assert!(window.try_find("host-search").is_none());
    });

    // The search shortcut goes to the list that is up.
    in_frame(cx, handle, |window, cx| {
        window.activate_window();
        window.dispatch_action(Box::new(FocusSearch), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("credential-search").focused(), Some(true));
    });

    in_frame(cx, handle, |window, cx| window.click("show-hosts", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.find("host-search").visible());
        assert!(window.try_find("credential-search").is_none());
        assert_eq!(window.find("show-credentials").checked(), Some(false));
    });
}

#[gpui_kit::test]
async fn a_new_password_credential_keeps_its_password_in_the_keychain(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        HostStore::empty().with_secrets(secrets.clone()),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("credential-empty-new", cx)
    });
    in_frame(cx, handle, |window, cx| {
        // A new credential starts at its name, as a password credential.
        assert_eq!(window.find("credential-name").focused(), Some(true));
        assert!(window.find("credential-password").visible());
        window.input("运维", cx);
        window.click("credential-user", cx);
        window.press("cmd-a", cx);
        window.input("deploy", cx);
        window.click("credential-password", cx);
        window.input("hunter2", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;

    let credential = cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.credentials().len(), 1);
        let credential = store.credentials()[0].clone();
        assert_eq!(credential.name.as_ref(), "运维");
        assert_eq!(credential.user.as_ref(), "deploy");
        assert_eq!(credential.kind, CredentialKind::Password);
        assert!(
            !format!("{credential:?}").contains("hunter2"),
            "凭据本身不该带着密码"
        );
        credential
    });
    assert_eq!(
        secrets
            .get(&credential.password_secret())
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("hunter2")
    );
    // The new credential is in the list, selected.
    in_frame(cx, handle, |window, _| {
        let row = window.find(("credential-row", credential.id.0));
        assert!(row.visible());
        assert_eq!(row.selected(), Some(true));
    });
}

#[gpui_kit::test]
async fn the_credential_kind_decides_which_fields_show(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        HostStore::empty(),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| window.click("new-credential", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("credential-key-path").is_none());
        assert!(window.try_find("credential-agent-note").is_none());
        // Nothing but a name is missing, and that is said first.
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("form-error").label(), Some("请输入名称"));
        window.click("credential-name", cx);
        window.input("部署", cx);
        window.within("credential-kind").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("credential-password").is_none());
        assert!(window.find("credential-passphrase").visible());
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("form-error").label(),
            Some("密钥凭据需要选择私钥文件")
        );
        window.click("choose-credential-key", cx);
    });
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|_| Some(vec!["/tmp/id_deploy".into()]));
    cx.run_until_parked();
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("credential-key-path").value(),
            Some("/tmp/id_deploy")
        );
        window.within("credential-kind").click(2usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("credential-agent-note").visible());
        assert!(window.try_find("credential-key-path").is_none());
        assert!(window.try_find("credential-password").is_none());
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let credential = &store.credentials()[0];
        assert_eq!(credential.kind, CredentialKind::Agent);
        // The key file picked on the way is not kept by an agent credential.
        assert_eq!(credential.key_path, None);
        assert_eq!(credential.user.as_ref(), "root");
    });
}

#[gpui_kit::test]
async fn the_credential_list_moves_with_the_arrow_keys_and_edits_on_enter(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let first = store.insert_credential_unnotified(CredentialDraft::new(
        "运维",
        CredentialKind::Password,
        "root",
    ));
    let second = store.insert_credential_unnotified(CredentialDraft::new(
        "个人",
        CredentialKind::Agent,
        "me",
    ));
    let (handle, _) = open_workspace_with_credentials(
        cx,
        store,
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("credential-row", first.0), cx)
    });
    in_frame(cx, handle, |window, cx| window.press("down", cx));
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find(("credential-row", second.0)).selected(),
            Some(true)
        );
        assert_eq!(
            window.find(("credential-row", first.0)).selected(),
            Some(false)
        );
        window.press("enter", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(window.find("credential-name").value(), Some("个人"));
        // An agent credential opens with its own kind picked.
        assert!(window.find("credential-agent-note").visible());
    });
}

#[gpui_kit::test]
async fn a_host_can_use_a_credential_instead_of_typing_a_login(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, credential, _) = store_with_credential(secrets.clone());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store,
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    in_frame(cx, handle, |window, cx| window.click("new-host-panel", cx));
    in_frame(cx, handle, |window, cx| {
        window.click("host-name", cx);
        window.input("web-01", cx);
        window.click("host-address", cx);
        window.input("10.0.1.12", cx);
        window.within("host-auth-source").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        // The credential brings the user and the secret: neither is asked.
        assert!(window.try_find("host-user").is_none());
        assert!(window.try_find("host-password").is_none());
        assert_eq!(window.find("host-credential").value(), Some("请选择凭据"));
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("form-error").label(), Some("请选择凭据"));
        window.within("host-credential").click("input", cx);
    });
    for key in ["down", "enter"] {
        in_frame(cx, handle, |window, cx| window.press(key, cx));
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("host-credential").value(),
            Some("运维（deploy · 密码）")
        );
        assert_eq!(
            window.find("host-credential-summary").label(),
            Some("以 deploy 登录，使用凭据保存的密码")
        );
        // Off the select first: a focused select opens on the commit.
        window.click("host-name", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        let created = store
            .hosts()
            .iter()
            .find(|host| host.name == "web-01")
            .expect("web-01 inserted");
        assert_eq!(created.credential, Some(credential));
        assert_eq!(created.user.as_ref(), "deploy");
        assert_eq!(created.auth, AuthKind::Password);
    });
    // Nothing of the host's own went to the keychain.
    assert_eq!(secrets.len(), 1);
}

#[gpui_kit::test]
async fn testing_a_connection_with_a_credential_uses_the_saved_login(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, _, host) = store_with_credential(secrets);
    let tester = Arc::new(FakeConnectionTester::default());
    let (handle, _) = open_workspace_with_credentials(
        cx,
        store,
        Arc::new(RecordingRemoteProvider::default()),
        tester.clone(),
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(EditHost(host)), cx)
    });
    in_frame(cx, handle, |window, cx| {
        // A host using a credential opens that way.
        assert_eq!(
            window.find("host-credential").value(),
            Some("运维（deploy · 密码）")
        );
        assert!(window.try_find("host-user").is_none());
        window.click("test-connection", cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(3), |window, cx| {
        window.render_frame(cx);
        window.notifications(cx).len() == 1
    })
    .await;
    // The credential's user, and no password from the form: the saved one
    // is read by the login itself.
    assert_eq!(
        tester.requests(),
        [("10.0.2.5".to_string(), 22, "deploy".to_string(), None)]
    );
}

#[gpui_kit::test]
async fn a_terminal_logs_in_with_its_credentials_login(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, credential, host) = store_with_credential(secrets);
    let secret = store.credential(credential).unwrap().password_secret();
    let remote = Arc::new(RecordingRemoteProvider::default());
    let (handle, _) = open_workspace_with_credentials(
        cx,
        store,
        remote.clone(),
        Arc::new(FakeConnectionTester::default()),
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    let logins = remote.logins();
    assert_eq!(logins.len(), 1);
    assert_eq!(logins[0].user, "deploy");
    assert_eq!(logins[0].method, LoginMethod::Password);
    assert_eq!(logins[0].password, secret);
}

#[gpui_kit::test]
async fn editing_a_credentials_user_reconnects_the_hosts_using_it(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (mut store, credential, host) = store_with_credential(secrets);
    let other = store.insert_unnotified(HostDraft::new(
        "web-01",
        "10.0.1.12",
        22,
        "root",
        AuthKind::Password,
        None,
    ));
    let remote = Arc::new(RecordingRemoteProvider::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store,
        remote.clone(),
        Arc::new(FakeConnectionTester::default()),
    );
    for id in [host, other] {
        in_frame(cx, handle, |window, cx| {
            window.dispatch_action(Box::new(ConnectHost(id)), cx)
        });
    }
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .host(host)
            .is_some_and(|host| host.state.is_connected())
    })
    .await;
    assert_eq!(remote.logins().len(), 2);
    show_credentials(cx, handle).await;

    // A new name changes no login: nothing reconnects.
    in_frame(cx, handle, |window, cx| {
        window.click(("credential-row", credential.0), cx);
        window.dispatch_action(Box::new(EditCredential(credential)), cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("credential-usage").label(),
            Some(
                "有 1 台主机使用此凭据。修改用户名、类型或私钥文件后，其中已连接的主机会重新连接。"
            )
        );
        window.click("credential-name", cx);
        window.press("cmd-a", cx);
        window.input("生产运维", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    assert_eq!(remote.logins().len(), 2);

    // A new user is a new login for the host using it, and only for it.
    in_frame(cx, handle, |window, cx| {
        window.click(("credential-row", credential.0), cx);
        window.dispatch_action(Box::new(EditCredential(credential)), cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.click("credential-user", cx);
        window.press("cmd-a", cx);
        window.input("admin", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    let logins = remote.logins();
    assert_eq!(logins.len(), 3);
    assert_eq!(logins[2].user, "admin");
    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert_eq!(store.host(host).unwrap().endpoint(), "admin@10.0.2.5:22");
        assert_eq!(store.host(other).unwrap().user.as_ref(), "root");
    });
}

#[gpui_kit::test]
async fn deleting_a_used_credential_leaves_its_hosts_connected_and_logging_in_on_their_own(
    cx: &mut TestAppContext,
) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (store, credential, host) = store_with_credential(secrets.clone());
    let secret = store.credential(credential).unwrap().password_secret();
    let remote = Arc::new(RecordingRemoteProvider::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store,
        remote.clone(),
        Arc::new(FakeConnectionTester::default()),
    );
    in_frame(cx, handle, |window, cx| {
        window.dispatch_action(Box::new(ConnectHost(host)), cx)
    });
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("credential-row", credential.0), cx);
        window.dispatch_action(Box::new(DeleteCredential(credential)), cx);
    });
    in_frame(cx, handle, |window, cx| window.click("ok", cx));
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
        window.try_find(("credential-row", credential.0)).is_none()
    })
    .await;
    cx.run_until_parked();

    cx.update(|cx| {
        let store = workspace.read(cx).store().read(cx);
        assert!(store.credentials().is_empty());
        let host = store.host(host).unwrap();
        assert_eq!(host.credential, None);
        assert_eq!(host.auth, AuthKind::Password);
        assert_eq!(host.user.as_ref(), "deploy");
    });
    // The working connection was left alone, and the password went with the
    // credential.
    assert_eq!(remote.logins().len(), 1);
    assert!(secrets.get(&secret).unwrap().is_none());
}

#[gpui_kit::test]
async fn a_long_credential_row_fits_the_smallest_window(cx: &mut TestAppContext) {
    let mut store = HostStore::seed();
    let id = store.insert_credential_unnotified(
        CredentialDraft::new(
            "生产环境所有数据库服务器共用的只读巡检账号（不要用于写操作）",
            CredentialKind::Key,
            "readonly-inspector",
        )
        .with_key_path("/Users/someone/.ssh/a_rather_long_private_key_file_name_ed25519"),
    );
    let (handle, _) = open_sized_workspace_with_forwards(
        cx,
        store,
        Arc::new(FakeForwardProvider::default()),
        size(px(960.), px(600.)),
    );
    cx.run_until_parked();
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, _| {
        let list = window.find("credential-list").bounds();
        let row = window.find(("credential-row", id.0)).bounds();
        assert!(row.right() <= list.right(), "{row:?} in {list:?}");
    });
}

/// A store that keeps pasted and generated keys under `data`, as the app
/// keeps them beside its database.
fn store_keeping_keys(data: &std::path::Path, secrets: Arc<InMemorySecretStore>) -> HostStore {
    HostStore::empty()
        .with_secrets(secrets)
        .with_key_dir(data.join("keys"))
}

/// Open 「生成密钥…」 from the empty credential list and wait for the key.
async fn open_generate_key(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click("credential-empty-generate", cx)
    });
    cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
        window.try_find("credential-public-key").is_some()
    })
    .await;
}

#[gpui_kit::test]
async fn a_generated_key_shows_its_public_half_and_is_kept_by_shellrs(cx: &mut TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store_keeping_keys(data.path(), secrets.clone()),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    open_generate_key(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        // A key credential with its key made already, waiting for a name.
        assert_eq!(window.find("credential-name").focused(), Some(true));
        assert!(window.try_find("credential-key-path").is_none());
        assert!(window.find("credential-passphrase").visible());
        window.input("部署", cx);
    });
    let line = in_frame(cx, handle, |window, cx| {
        let line = window
            .find("credential-public-key")
            .label()
            .unwrap()
            .to_string();
        assert!(line.starts_with("ssh-ed25519 "), "{line}");
        // The key's comment is the credential's name.
        assert!(line.ends_with(" 部署"), "{line}");
        window.click("commit", cx);
        line
    });
    wait_for_dialog_to_close(cx, handle).await;

    let credential = cx.update(|cx| workspace.read(cx).store().read(cx).credentials()[0].clone());
    assert_eq!(credential.kind, CredentialKind::Key);
    let path = credential.key_path.clone().unwrap();
    let path = std::path::Path::new(path.as_ref());
    assert!(path.starts_with(data.path().join("keys")), "{path:?}");
    // The file holds the key whose public half the dialog showed.
    assert_eq!(read_public_key(path), Some(line));
    let text = std::fs::read_to_string(path).unwrap();
    assert!(!PastedKey::parse(&text).unwrap().is_encrypted());
    assert!(secrets.get(&SecretRef::passphrase(path)).unwrap().is_none());
    // The list names it by kind: its file name means nothing to anyone.
    in_frame(cx, handle, |window, _| {
        assert!(window.find(("credential-row", credential.id.0)).visible());
    });
}

#[gpui_kit::test]
async fn a_generated_key_with_a_passphrase_is_saved_encrypted(cx: &mut TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store_keeping_keys(data.path(), secrets.clone()),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    open_generate_key(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.input("部署", cx);
        window.click("credential-passphrase", cx);
        window.input("correct horse", cx);
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;

    let path = cx.update(|cx| {
        workspace.read(cx).store().read(cx).credentials()[0]
            .key_path
            .clone()
            .unwrap()
    });
    let text = std::fs::read_to_string(path.as_ref()).unwrap();
    assert!(PastedKey::parse(&text).unwrap().is_encrypted());
    assert!(!text.contains("correct horse"));
    assert_eq!(
        secrets
            .get(&SecretRef::passphrase(path.as_ref()))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("correct horse")
    );
}

#[gpui_kit::test]
async fn a_pasted_key_is_checked_then_kept_by_shellrs(cx: &mut TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    let key = GeneratedKey::generate(KeyAlgorithm::Ed25519).unwrap();
    let text = key.encode("me@laptop", "").unwrap().to_string();
    let (handle, workspace) = open_workspace_with_credentials(
        cx,
        store_keeping_keys(data.path(), Arc::new(InMemorySecretStore::default())),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| window.click("new-credential", cx));
    in_frame(cx, handle, |window, cx| {
        window.input("个人", cx);
        window.within("credential-kind").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.within("credential-key-source").click(1usize, cx);
    });

    // The public half, pasted by mistake, is named for what it is.
    let public = key.public_key_line("me@laptop");
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(public.clone())));
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("credential-key-path").is_none());
        window.click("credential-key-text", cx);
        window.press("cmd-v", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert!(window.try_find("credential-public-key").is_none());
        window.click("commit", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("form-error").label(),
            Some("这是公钥，请粘贴私钥（以 -----BEGIN 开头的那一段）")
        );
    });

    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())));
    in_frame(cx, handle, |window, cx| {
        window.click("credential-key-text", cx);
        window.press("cmd-a", cx);
        window.press("cmd-v", cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("credential-public-key").label(),
            Some(public.as_str())
        );
        // A key without a passphrase has none to ask for.
        assert!(window.try_find("credential-passphrase").is_none());
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;

    let credential = cx.update(|cx| workspace.read(cx).store().read(cx).credentials()[0].clone());
    let path = credential.key_path.clone().unwrap();
    assert!(std::path::Path::new(path.as_ref()).starts_with(data.path().join("keys")));
    assert_eq!(std::fs::read_to_string(path.as_ref()).unwrap(), text);
}

#[gpui_kit::test]
async fn a_kept_key_is_deleted_when_its_credential_turns_to_a_password(cx: &mut TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    let mut store = store_keeping_keys(data.path(), Arc::new(InMemorySecretStore::default()));
    let path = store.save_private_key(None, "kept").unwrap();
    let credential = store.insert_credential_unnotified(
        CredentialDraft::new("部署", CredentialKind::Key, "deploy").with_key_path(path.clone()),
    );
    let (handle, _) = open_workspace_with_credentials(
        cx,
        store,
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("credential-row", credential.0), cx);
        window.dispatch_action(Box::new(EditCredential(credential)), cx);
    });
    in_frame(cx, handle, |window, cx| {
        // The kept key shows as the file it is.
        assert_eq!(
            window.find("credential-key-path").value(),
            Some(path.as_ref())
        );
        assert!(window.try_find("credential-kept-key-note").is_none());
        window.within("credential-kind").click(0usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("credential-kept-key-note").label(),
            Some("保存后，ShellRS 保存的原私钥会被删除。")
        );
        window.click("commit", cx);
    });
    wait_for_dialog_to_close(cx, handle).await;
    assert!(!std::path::Path::new(path.as_ref()).exists());
}

#[gpui_kit::test]
async fn a_key_credentials_public_key_is_copied_from_its_file(cx: &mut TestAppContext) {
    let data = tempfile::tempdir().unwrap();
    let key = GeneratedKey::generate(KeyAlgorithm::Ed25519).unwrap();
    let file = data.path().join("id_deploy");
    std::fs::write(&file, key.encode("deploy@laptop", "").unwrap().as_str()).unwrap();
    let mut store = HostStore::empty();
    let credential = store.insert_credential_unnotified(
        CredentialDraft::new("部署", CredentialKind::Key, "deploy")
            .with_key_path(file.display().to_string()),
    );
    let (handle, _) = open_workspace_with_credentials(
        cx,
        store,
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| {
        window.click(("credential-row", credential.0), cx);
        window.dispatch_action(Box::new(CopyCredentialPublicKey(credential)), cx);
    });
    cx.wait_for(handle.into(), Duration::from_secs(2), |window, cx| {
        window.notifications(cx).len() == 1
    })
    .await;
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(key.public_key_line("deploy@laptop"))
    );
}

#[gpui_kit::test]
async fn a_segment_that_cannot_be_chosen_stays_unchosen(cx: &mut TestAppContext) {
    // No key directory: pasted and generated keys have nowhere to go.
    let (handle, _) = open_workspace_with_credentials(
        cx,
        HostStore::empty(),
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    in_frame(cx, handle, |window, cx| window.click("new-credential", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("credential-kind").click(1usize, cx);
    });
    in_frame(cx, handle, |window, cx| {
        window.within("credential-key-source").click(1usize, cx);
    });
    in_frame(cx, handle, |window, _| {
        let sources = window.within("credential-key-source");
        assert_eq!(sources.find(0usize).selected(), Some(true));
        assert_eq!(sources.find(1usize).selected(), Some(false));
        assert!(window.find("credential-key-path").visible());
        assert!(window.try_find("credential-key-text").is_none());
    });
}

#[gpui_kit::test]
async fn a_credential_rows_tooltip_opens_beside_it_with_a_keys_whole_path(cx: &mut TestAppContext) {
    let mut store = HostStore::empty();
    let key = store.insert_credential_unnotified(
        CredentialDraft::new("部署", CredentialKind::Key, "deploy")
            .with_key_path("/Users/someone/.ssh/id_ed25519"),
    );
    let password = store.insert_credential_unnotified(CredentialDraft::new(
        "运维",
        CredentialKind::Password,
        "root",
    ));
    let (handle, _) = open_workspace_with_credentials(
        cx,
        store,
        Arc::new(RecordingRemoteProvider::default()),
        Arc::new(FakeConnectionTester::default()),
    );
    show_credentials(cx, handle).await;
    let hover = |cx: &mut TestAppContext, id: CredentialId| {
        in_frame(cx, handle, |window, cx| {
            window.hover(("credential-row", id.0), cx)
        });
        // Tooltips wait half a second before they open.
        cx.executor().advance_clock(Duration::from_millis(1000));
        cx.run_until_parked();
    };

    // The row shows the key's file name; the tooltip, under the detail, the
    // whole path.
    hover(cx, key);
    in_frame(cx, handle, |window, _| {
        let line = window.find("credential-tooltip");
        let note = window.find("credential-tooltip-note");
        assert_eq!(line.label(), Some("deploy · id_ed25519"));
        assert_eq!(note.label(), Some("/Users/someone/.ssh/id_ed25519"));
        assert!(note.bounds().top() >= line.bounds().bottom());
        let row = window.find(("credential-row", key.0)).bounds();
        assert!(line.bounds().left() >= row.right());
    });

    // A password has no file: the detail stands alone.
    hover(cx, password);
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find("credential-tooltip").label(),
            Some("root · 密码")
        );
        assert!(window.try_find("credential-tooltip-note").is_none());
    });
}
