//! The changes the external CLI asks for, as the workspace makes them:
//! hosts and credentials created, changed and deleted from `shellrs`.
//! The socket is not in it; `apply_cli_change` is where the server hands
//! each change over.

use shellrs::cli::{CliChange, CliError, CredentialFields, ErrorCode, HostFields, Reply, Secret};

use crate::support::*;

/// Make `change` as the server's poller would, and wait for its outcome.
fn apply(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    workspace: &Entity<Workspace>,
    change: CliChange,
) -> Result<Reply, CliError> {
    let outcome = in_frame(cx, handle, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.apply_cli_change(change, window, cx)
        })
    });
    let outcome = cx.foreground_executor.block_test(outcome);
    cx.run_until_parked();
    outcome
}

fn host_id(workspace: &Entity<Workspace>, id: u64, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .host(HostId(id))
            .unwrap()
            .public_id
            .to_string()
    })
}

fn store(workspace: &Entity<Workspace>, cx: &mut TestAppContext) -> Entity<HostStore> {
    cx.update(|cx| workspace.read(cx).store().clone())
}

#[gpui_kit::test]
fn a_host_from_the_cli_lands_in_the_tree_with_its_group_and_password(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let (handle, workspace) =
        open_workspace_with_store(cx, HostStore::seed().with_secrets(secrets.clone()));
    let fields = HostFields {
        name: Some("api-01".into()),
        host: Some("10.0.9.1".into()),
        group: Some(Some("生产/接口".into())),
        password: Some(Some(Secret::new("s3cret"))),
        ..HostFields::default()
    };
    let Ok(Reply::Host(created)) = apply(cx, handle, &workspace, CliChange::CreateHost(fields))
    else {
        panic!("not created");
    };
    assert_eq!(created.group.as_deref(), Some("生产/接口"));
    assert_eq!(created.password_saved, Some(true));

    let store = store(&workspace, cx);
    let host = cx.update(|cx| {
        let store = store.read(cx);
        let host = store
            .hosts()
            .iter()
            .find(|host| host.public_id.as_str() == created.id)
            .unwrap();
        let group = store.group(host.group.unwrap()).unwrap();
        assert_eq!(group.name.as_ref(), "接口");
        assert_eq!(group.parent, Some(GroupId(PRODUCTION)));
        host.id
    });
    assert_eq!(
        secrets
            .get(&SecretRef::password("root", "10.0.9.1", 22))
            .unwrap()
            .as_deref()
            .map(String::as_str),
        Some("s3cret")
    );
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .within("host-tree")
                .try_find(("host-row", host.0))
                .is_some()
        );
    });

    // Checked as the host form checks it.
    let nameless = HostFields {
        host: Some("10.0.9.2".into()),
        ..HostFields::default()
    };
    let error = apply(cx, handle, &workspace, CliChange::CreateHost(nameless)).unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (ErrorCode::BadRequest, "请输入名称")
    );
}

#[gpui_kit::test]
fn a_host_moved_to_another_port_keeps_its_password_until_told_otherwise(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let before = SecretRef::password("postgres", "10.0.2.5", 22);
    secrets.set(&before, "pw").unwrap();
    let (handle, workspace) =
        open_workspace_with_store(cx, HostStore::seed().with_secrets(secrets.clone()));
    let db = host_id(&workspace, DB_01, cx);
    let moved = HostFields {
        port: Some(2222),
        ..HostFields::default()
    };
    let Ok(Reply::Host(changed)) = apply(
        cx,
        handle,
        &workspace,
        CliChange::UpdateHost {
            host: db.clone(),
            fields: moved,
        },
    ) else {
        panic!("not changed");
    };
    assert_eq!((changed.port, changed.password_saved), (2222, Some(true)));
    let after = SecretRef::password("postgres", "10.0.2.5", 2222);
    assert_eq!(
        secrets.get(&after).unwrap().as_deref().map(String::as_str),
        Some("pw")
    );
    // The entry it left goes, as no other host uses it.
    assert!(secrets.get(&before).unwrap().is_none());

    let forget = HostFields {
        password: Some(None),
        ..HostFields::default()
    };
    let Ok(Reply::Host(changed)) = apply(
        cx,
        handle,
        &workspace,
        CliChange::UpdateHost {
            host: db,
            fields: forget,
        },
    ) else {
        panic!("not changed");
    };
    assert_eq!(changed.password_saved, Some(false));
    assert!(secrets.get(&after).unwrap().is_none());
}

#[gpui_kit::test]
fn a_host_with_tabs_open_goes_only_with_force_and_takes_them_along(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let web = host_id(&workspace, WEB_01, cx);
    let delete = |force| CliChange::DeleteHost {
        host: web.clone(),
        force,
    };
    let error = apply(cx, handle, &workspace, delete(false)).unwrap_err();
    assert_eq!(error.code, ErrorCode::HostInUse);
    let store = store(&workspace, cx);
    assert!(cx.update(|cx| store.read(cx).host(HostId(WEB_01)).is_some()));
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find(("terminal-tab", INITIAL_WEB_TERMINAL))
                .is_some()
        );
    });

    let Ok(Reply::HostDeleted(deleted)) = apply(cx, handle, &workspace, delete(true)) else {
        panic!("not deleted");
    };
    assert_eq!(deleted.host.name, "web-01");
    assert!(cx.update(|cx| store.read(cx).host(HostId(WEB_01)).is_none()));
    in_frame(cx, handle, |window, _| {
        assert!(
            window
                .try_find(("terminal-tab", INITIAL_WEB_TERMINAL))
                .is_none()
        );
    });

    // Nothing open, nothing to force.
    let db = host_id(&workspace, DB_01, cx);
    let deleted = apply(
        cx,
        handle,
        &workspace,
        CliChange::DeleteHost {
            host: db,
            force: false,
        },
    );
    assert!(matches!(deleted, Ok(Reply::HostDeleted(_))));
}

#[gpui_kit::test]
fn a_host_not_saved_is_left_to_its_tabs(cx: &mut TestAppContext) {
    let mut seeded = HostStore::seed();
    let temporary = seeded.insert_temporary_unnotified(
        HostDraft::new("临时", "10.0.0.3", 22, "root", AuthKind::Password, None),
        None,
    );
    let (handle, workspace) = open_workspace_with_store(cx, seeded);
    let id = cx.update(|cx| {
        workspace
            .read(cx)
            .store()
            .read(cx)
            .host(temporary)
            .unwrap()
            .public_id
            .to_string()
    });
    for change in [
        CliChange::UpdateHost {
            host: id.clone(),
            fields: HostFields::default(),
        },
        CliChange::DeleteHost {
            host: id.clone(),
            force: true,
        },
    ] {
        let error = apply(cx, handle, &workspace, change).unwrap_err();
        assert_eq!(error.code, ErrorCode::BadRequest);
        assert!(
            error.message.contains("没有保存的连接"),
            "{}",
            error.message
        );
    }
}

#[gpui_kit::test]
fn a_pasted_key_becomes_a_credential_shellrs_keeps(cx: &mut TestAppContext) {
    let secrets = Arc::new(InMemorySecretStore::default());
    let data = tempfile::tempdir().unwrap();
    let (handle, workspace) = open_workspace_with_store(
        cx,
        HostStore::empty()
            .with_secrets(secrets.clone())
            .with_key_dir(data.path().join("keys")),
    );
    let key = GeneratedKey::generate(KeyAlgorithm::Ed25519).unwrap();
    let text = key.encode("deploy", "correct horse").unwrap().to_string();
    let fields = CredentialFields {
        name: Some("部署".into()),
        user: Some("deploy".into()),
        private_key: Some(Secret::new(text.clone())),
        passphrase: Some(Some(Secret::new("correct horse"))),
        ..CredentialFields::default()
    };
    let Ok(Reply::Credential(created)) =
        apply(cx, handle, &workspace, CliChange::CreateCredential(fields))
    else {
        panic!("not created");
    };
    assert!(created.kept);
    // Where it is kept is not said: the file is as good as the key.
    assert_eq!(created.key_path, None);
    assert_eq!(created.passphrase_saved, Some(true));

    let store = store(&workspace, cx);
    let path = cx.update(|cx| {
        let store = store.read(cx);
        let credential = &store.credentials()[0];
        assert_eq!(credential.keychain_id.as_str(), created.id);
        assert_eq!(credential.kind, CredentialKind::Key);
        credential.key_path.clone().unwrap()
    });
    assert!(path.starts_with(data.path().join("keys").to_str().unwrap()));
    assert_eq!(std::fs::read_to_string(path.as_ref()).unwrap(), text);
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
fn deleting_a_credential_lets_its_hosts_log_in_on_their_own(cx: &mut TestAppContext) {
    let mut seeded = HostStore::empty();
    let credential = seeded.insert_credential_unnotified(CredentialDraft::new(
        "运维",
        CredentialKind::Password,
        "deploy",
    ));
    let host = seeded.insert_unnotified(
        HostDraft::new("db-01", "10.0.2.5", 22, "root", AuthKind::Password, None)
            .with_credential(credential),
    );
    let id = seeded
        .credential(credential)
        .unwrap()
        .keychain_id
        .to_string();
    let (handle, workspace) = open_workspace_with_store(cx, seeded);
    let Ok(Reply::CredentialDeleted(deleted)) = apply(
        cx,
        handle,
        &workspace,
        CliChange::DeleteCredential { credential: id },
    ) else {
        panic!("not deleted");
    };
    assert_eq!(deleted.credential.name, "运维");
    let released: Vec<&str> = deleted
        .released
        .iter()
        .map(|host| host.name.as_str())
        .collect();
    assert_eq!(released, ["db-01"]);
    let store = store(&workspace, cx);
    cx.update(|cx| {
        let store = store.read(cx);
        assert!(store.credentials().is_empty());
        let host = store.host(host).unwrap();
        assert_eq!((host.credential, host.auth), (None, AuthKind::Password));
        assert_eq!(host.user.as_ref(), "deploy");
    });
}
