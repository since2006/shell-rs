//! The interface as the apps use it, against an SSH server in this process.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shellrs_hosts as hosts;
use shellrs_mobile::{
    ConnectError, Connection, ConnectionEvent, Core, CoreConfig, CoreError, CredentialDraft,
    CredentialKind, Dependents, EntryKind, FailureLink, GroupDraft, HostAuth, HostDraft,
    HostProblem, HostProtocol, HostRoute, HostStore, IoKind, KeyAlgorithm, KeyChange, KeyProblem,
    Keychain, KeychainError, Login, LoginMethod, PromptKind, PromptReply, Route, SavedSecret,
    SecretChange, SecretProblem, SecretRef, SftpEvent, SshError, StoreError, StoreEvent,
    StoreFailure, TerminalEvent, TerminalOptions, TransferPhase, TreeDrop, TreeNode, account_for,
    check_private_key,
};
use shellrs_ssh::testing::{TEST_PASSWORD, TEST_USER, TestServer, closed_port, has_sftp_server};
use shellrs_ssh::{PROBE_COMMAND, WINDOWS_PROBE_COMMAND};

/// What the app's keychain holds, by account.
#[derive(Default)]
struct TestKeychain(Mutex<HashMap<String, String>>);

impl TestKeychain {
    fn get(&self, secret: SecretRef) -> Option<String> {
        self.0.lock().unwrap().get(&account_for(secret)).cloned()
    }

    fn set(&self, secret: SecretRef, value: &str) {
        self.0
            .lock()
            .unwrap()
            .insert(account_for(secret), value.into());
    }
}

impl Keychain for TestKeychain {
    fn available(&self) -> bool {
        true
    }

    fn get(&self, account: String) -> Result<Option<String>, KeychainError> {
        Ok(self.0.lock().unwrap().get(&account).cloned())
    }

    fn set(&self, account: String, value: String) -> Result<(), KeychainError> {
        self.0.lock().unwrap().insert(account, value);
        Ok(())
    }

    fn delete(&self, account: String) -> Result<(), KeychainError> {
        self.0.lock().unwrap().remove(&account);
        Ok(())
    }
}

fn config(data: &Path) -> CoreConfig {
    CoreConfig {
        data_dir: data.display().to_string(),
        device_name: "Test phone".into(),
        platform: "ios".into(),
        app_version: "0.0.0".into(),
    }
}

fn password_ref(port: u16) -> SecretRef {
    SecretRef::Password {
        user: TEST_USER.into(),
        host: "127.0.0.1".into(),
        port,
    }
}

fn login(port: u16) -> Login {
    Login {
        host: "127.0.0.1".into(),
        port,
        user: TEST_USER.into(),
        method: LoginMethod::Password,
        key_path: None,
        password: password_ref(port),
        route: Route::Direct,
    }
}

/// A core on a data folder of its own, with `password` saved for the
/// server at `port`.
fn core(data: &Path, port: u16, password: Option<&str>) -> Arc<Core> {
    let keychain = TestKeychain::default();
    if let Some(password) = password {
        keychain.set(password_ref(port), password);
    }
    Core::new(config(data), Arc::new(keychain)).unwrap()
}

/// `future`, or a failed test rather than a hung one.
async fn soon<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), future)
        .await
        .expect("in time")
}

/// The connection's next event other than a latency reading.
async fn next(connection: &Connection) -> ConnectionEvent {
    loop {
        match soon(connection.next_event()).await.expect("an event") {
            ConnectionEvent::Latency { .. } => {}
            event => return event,
        }
    }
}

/// Log in, trusting the server's key when asked.
async fn connected(core: &Core, port: u16) -> Arc<Connection> {
    let connection = core.connect(login(port));
    loop {
        match next(&connection).await {
            ConnectionEvent::Prompt { prompt } => {
                assert!(matches!(prompt.kind, PromptKind::UnknownHost { .. }));
                connection.answer(prompt.id, PromptReply::TrustAndSave);
            }
            ConnectionEvent::Connected { .. } => return connection,
            event => panic!("{event:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_login_asks_about_the_key_and_reads_the_saved_password() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    let core = core(data.path(), server.port, Some(TEST_PASSWORD));

    let connection = core.connect(login(server.port));
    let ConnectionEvent::Prompt { prompt } = next(&connection).await else {
        panic!("asked about the key first");
    };
    let PromptKind::UnknownHost {
        host,
        port,
        fingerprint,
        ..
    } = &prompt.kind
    else {
        panic!("{prompt:?}");
    };
    assert_eq!((host.as_str(), *port), ("127.0.0.1", server.port));
    assert_eq!(prompt.jump_host, None);
    let trusted = fingerprint.clone();
    connection.answer(prompt.id, PromptReply::TrustAndSave);
    assert_eq!(
        next(&connection).await,
        ConnectionEvent::Connected {
            fingerprint: trusted.clone()
        }
    );
    assert!(data.path().join("known_hosts").exists());

    // Trusted now: straight in.
    let again = core.connect(login(server.port));
    assert_eq!(
        next(&again).await,
        ConnectionEvent::Connected {
            fingerprint: trusted
        }
    );

    connection.disconnect();
    assert_eq!(
        next(&connection).await,
        ConnectionEvent::Closed { failure: None }
    );
    assert_eq!(soon(connection.next_event()).await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_password_is_asked_for() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    let core = core(data.path(), server.port, Some("wrong"));

    let connection = core.connect(login(server.port));
    let mut asked = false;
    loop {
        match next(&connection).await {
            ConnectionEvent::Prompt { prompt } => match prompt.kind {
                PromptKind::UnknownHost { .. } => {
                    connection.answer(prompt.id, PromptReply::TrustAndSave)
                }
                PromptKind::Password { saved_rejected } => {
                    assert!(saved_rejected);
                    asked = true;
                    connection.answer(
                        prompt.id,
                        PromptReply::Answers {
                            answers: vec![TEST_PASSWORD.into()],
                        },
                    );
                }
                kind => panic!("{kind:?}"),
            },
            ConnectionEvent::Connected { .. } => break,
            event => panic!("{event:?}"),
        }
    }
    assert!(asked);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_port_fails_typed_as_the_network() {
    let data = tempfile::tempdir().unwrap();
    let port = closed_port();
    let core = core(data.path(), port, None);

    let connection = core.connect(login(port));
    let ConnectionEvent::Closed {
        failure: Some(failure),
    } = next(&connection).await
    else {
        panic!("closed with a failure");
    };
    assert!(failure.network);
    assert_eq!(
        failure.links[0],
        FailureLink::Connect {
            error: ConnectError::Unreachable {
                host: "127.0.0.1".into(),
                port,
            }
        }
    );
    assert!(failure.links.iter().any(|link| matches!(
        link,
        FailureLink::Io {
            kind: IoKind::ConnectionRefused,
            ..
        }
    )));
    assert!(matches!(
        soon(connection.open_terminal(options())).await,
        Err(SshError::NotConnected)
    ));
}

fn options() -> TerminalOptions {
    TerminalOptions {
        term: "xterm-256color".into(),
        columns: 80,
        rows: 24,
        width: 0,
        height: 0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_echoes_resizes_and_ends_with_its_shell() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    let core = core(data.path(), server.port, Some(TEST_PASSWORD));
    let connection = connected(&core, server.port).await;

    let terminal = soon(connection.open_terminal(options())).await.unwrap();
    terminal.write(b"hello ".to_vec()).unwrap();
    terminal.write("世界".as_bytes().to_vec()).unwrap();
    let mut echoed = Vec::new();
    while !echoed.ends_with("世界".as_bytes()) {
        match soon(terminal.next_event()).await {
            Some(TerminalEvent::Output { bytes }) => echoed.extend(bytes),
            event => panic!("{event:?}"),
        }
    }
    assert_eq!(echoed, "hello 世界".as_bytes());
    // The shell started on a terminal of the size asked for.
    assert_eq!(server.seen().size, Some((80, 24)));

    terminal.resize(120, 40, 0, 0).unwrap();
    soon(async {
        while server.seen().size != Some((120, 40)) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    terminal.write(b"exit\n".to_vec()).unwrap();
    assert_eq!(
        soon(terminal.next_event()).await,
        Some(TerminalEvent::Exited {
            code: Some(3),
            signal: None
        })
    );
    assert_eq!(soon(terminal.next_event()).await, None);
    assert!(matches!(
        terminal.write(b"late".to_vec()),
        Err(SshError::Closed)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_connection_ends_its_terminals_quietly() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    let core = core(data.path(), server.port, Some(TEST_PASSWORD));
    let connection = connected(&core, server.port).await;
    let terminal = soon(connection.open_terminal(options())).await.unwrap();

    connection.disconnect();
    assert_eq!(soon(terminal.next_event()).await, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ending_a_terminal_leaves_its_connection() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    let core = core(data.path(), server.port, Some(TEST_PASSWORD));
    let connection = connected(&core, server.port).await;
    let terminal = soon(connection.open_terminal(options())).await.unwrap();

    // A call waiting on it holds it, as one from Swift or Kotlin does:
    // ending it gets through all the same.
    let waiting = tokio::spawn({
        let terminal = terminal.clone();
        async move { terminal.next_event().await }
    });
    terminal.end();
    assert_eq!(soon(waiting).await.unwrap(), None);

    let output = soon(connection.exec("uptime".into())).await.unwrap();
    assert_eq!(output.stdout, b"uptime\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_run_beside_and_the_connection_answers_a_check() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    let core = core(data.path(), server.port, Some(TEST_PASSWORD));
    let connection = connected(&core, server.port).await;

    let output = soon(connection.exec("uptime".into())).await.unwrap();
    assert_eq!(output.stdout, b"uptime\n");
    assert_eq!(server.seen().commands, ["uptime"]);
    soon(connection.check_alive()).await.unwrap();

    connection.disconnect();
    assert_eq!(
        next(&connection).await,
        ConnectionEvent::Closed { failure: None }
    );
    assert!(matches!(
        soon(connection.check_alive()).await,
        Err(SshError::NotConnected)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_data_folder_has_one_core() {
    let data = tempfile::tempdir().unwrap();
    let first = core(data.path(), 22, None);
    assert!(matches!(
        Core::new(config(data.path()), Arc::new(TestKeychain::default())),
        Err(CoreError::DataFolderInUse)
    ));
    drop(first);
    Core::new(config(data.path()), Arc::new(TestKeychain::default())).unwrap();
}

/// The session's events until the transfer is over.
async fn transferred(session: &shellrs_mobile::SftpSession) -> Vec<SftpEvent> {
    let mut events = Vec::new();
    loop {
        let event = soon(session.next_event()).await.expect("an event");
        let idle = event == SftpEvent::Idle;
        events.push(event);
        if idle {
            return events;
        }
    }
}

fn completed(events: &[SftpEvent]) -> bool {
    events.iter().any(|event| {
        matches!(event, SftpEvent::Progress { progress }
            if progress.phase == TransferPhase::Completed && progress.failed == 0)
    })
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sftp_lists_changes_and_transfers_on_the_same_connection() {
    if !has_sftp_server() {
        eprintln!("skipped: no OpenSSH sftp-server on this machine");
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let remote = data.path().join("remote");
    let local = data.path().join("local");
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    let server = TestServer::start(remote.clone()).await.unwrap();
    let core = core(data.path(), server.port, Some(TEST_PASSWORD));
    let connection = connected(&core, server.port).await;

    let session = soon(connection.open_sftp()).await.unwrap();
    let home = session.home();
    assert_eq!(
        Path::new(&home).canonicalize().unwrap(),
        remote.canonicalize().unwrap()
    );

    let source = local.join("文件.txt");
    std::fs::write(&source, b"contents").unwrap();
    session
        .upload(vec![source.display().to_string()], home.clone())
        .unwrap();
    assert!(completed(&transferred(&session).await));
    assert_eq!(std::fs::read(remote.join("文件.txt")).unwrap(), b"contents");

    soon(session.create_directory(format!("{home}/目录")))
        .await
        .unwrap();
    soon(session.rename(format!("{home}/文件.txt"), format!("{home}/目录/改名.txt")))
        .await
        .unwrap();
    let listing = soon(session.list(format!("{home}/目录"))).await.unwrap();
    let names: Vec<_> = listing
        .entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.kind, entry.size))
        .collect();
    assert_eq!(names, [("改名.txt", EntryKind::File, 8)]);

    let back = local.join("back");
    std::fs::create_dir(&back).unwrap();
    session
        .download(
            vec![format!("{home}/目录/改名.txt")],
            back.display().to_string(),
        )
        .unwrap();
    assert!(completed(&transferred(&session).await));
    assert_eq!(std::fs::read(back.join("改名.txt")).unwrap(), b"contents");

    soon(session.delete(vec![format!("{home}/目录")]))
        .await
        .unwrap();
    assert!(!remote.join("目录").exists());
    let Err(SshError::Failed { failure }) = soon(session.list(format!("{home}/目录"))).await
    else {
        panic!("listing a deleted folder fails");
    };
    assert!(!failure.network);

    // The terminal shares the connection the session is on.
    soon(connection.open_terminal(options())).await.unwrap();
    session.end();
    assert_eq!(soon(session.next_event()).await, None);
}

/// `check` holding, or a failed test rather than a hung one.
async fn until(mut check: impl FnMut() -> bool) {
    soon(async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
}

fn host(name: &str, address: &str) -> HostDraft {
    HostDraft {
        name: name.into(),
        protocol: HostProtocol::Ssh,
        address: address.into(),
        port: 22,
        user: "root".into(),
        auth: HostAuth::Password,
        route: HostRoute::Direct,
        group: None,
        notes: String::new(),
    }
}

fn password_of(address: &str) -> SecretRef {
    SecretRef::Password {
        user: "root".into(),
        host: address.into(),
        port: 22,
    }
}

fn set(value: &str) -> SecretChange {
    SecretChange::Set {
        value: value.into(),
    }
}

/// A core with `keychain`, on `data`.
fn core_with(data: &Path, keychain: &Arc<TestKeychain>) -> Arc<Core> {
    Core::new(config(data), keychain.clone()).unwrap()
}

/// The store's events until `wanted` comes.
async fn event(store: &HostStore, wanted: impl Fn(&StoreEvent) -> bool) -> StoreEvent {
    loop {
        let event = soon(store.next_event()).await.expect("an event");
        if wanted(&event) {
            return event;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hosts_and_their_passwords_are_kept_as_the_desktop_keeps_them() {
    let data = tempfile::tempdir().unwrap();
    let keychain = Arc::new(TestKeychain::default());
    let core = core_with(data.path(), &keychain);
    let store = core.store();

    let group = store
        .add_group(GroupDraft {
            name: " 生产 ".into(),
            parent: None,
        })
        .unwrap();
    let draft = HostDraft {
        group: Some(group),
        ..host("", "10.0.0.1")
    };
    let id = soon(store.add_host(draft, set("hunter2"), SecretChange::Keep))
        .await
        .unwrap();
    // In the keychain by the time it returns, under the desktop's account.
    assert_eq!(
        keychain.get(password_of("10.0.0.1")).as_deref(),
        Some("hunter2")
    );
    assert_eq!(soon(store.next_event()).await, Some(StoreEvent::Changed));
    let saved = store.host(id).unwrap();
    assert_eq!(
        (saved.name.as_str(), saved.group, saved.auth),
        ("10.0.0.1", Some(group), HostAuth::Password)
    );
    assert_eq!(store.groups()[0].name, "生产");
    assert_eq!(
        soon(store.saved_secret(SavedSecret::HostPassword { host: id }))
            .await
            .unwrap()
            .as_deref(),
        Some("hunter2")
    );

    // Moved to another address, its password left as it is goes along.
    let moved = HostDraft {
        group: Some(group),
        ..host("web", "10.0.0.2")
    };
    soon(store.update_host(id, moved, SecretChange::Keep, SecretChange::Keep))
        .await
        .unwrap();
    assert_eq!(
        keychain.get(password_of("10.0.0.2")).as_deref(),
        Some("hunter2")
    );
    assert_eq!(keychain.get(password_of("10.0.0.1")), None);

    let copy = store.duplicate_host(id, "web copy".into()).unwrap();
    assert!(store.move_node(TreeNode::Host { id: copy }, TreeDrop::Top));
    assert_eq!(store.host(copy).unwrap().group, None);

    // All of it there when the app starts again.
    drop(store);
    drop(core);
    let core = core_with(data.path(), &keychain);
    let store = core.store();
    let names: Vec<_> = store
        .hosts()
        .into_iter()
        .map(|host| (host.name, host.address, host.group, host.public_id))
        .collect();
    assert_eq!(
        names,
        [
            (
                "web".into(),
                "10.0.0.2".into(),
                Some(group),
                saved.public_id.clone()
            ),
            (
                "web copy".into(),
                "10.0.0.2".into(),
                None,
                store.host(copy).unwrap().public_id
            ),
        ]
    );
    assert_ne!(names[1].3, saved.public_id);

    // Deleting the group deletes the host in it, and its password with it
    // once no host uses it.
    store.delete_host(copy).unwrap();
    assert_eq!(store.delete_group(group).unwrap(), [id]);
    assert!(store.hosts().is_empty());
    until(|| keychain.get(password_of("10.0.0.2")).is_none()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_store_says_what_is_wrong_and_changes_nothing() {
    let data = tempfile::tempdir().unwrap();
    let keychain = Arc::new(TestKeychain::default());
    let core = core_with(data.path(), &keychain);
    let store = core.store();
    let keep = || SecretChange::Keep;
    let add = |draft: HostDraft| store.add_host(draft, keep(), keep());

    assert!(matches!(
        soon(add(host("web", " "))).await,
        Err(StoreError::Host {
            problem: HostProblem::Address
        })
    ));
    assert!(matches!(
        soon(add(HostDraft {
            group: Some(99),
            ..host("web", "10.0.0.1")
        }))
        .await,
        Err(StoreError::GroupNotFound)
    ));
    assert!(matches!(
        soon(add(HostDraft {
            auth: HostAuth::Credential { id: 99 },
            ..host("web", "10.0.0.1")
        }))
        .await,
        Err(StoreError::CredentialNotFound)
    ));
    assert!(matches!(
        soon(add(HostDraft {
            route: HostRoute::Jump {
                hosts: vec![Some(99)]
            },
            ..host("web", "10.0.0.1")
        }))
        .await,
        Err(StoreError::JumpHostNotFound)
    ));
    assert!(store.hosts().is_empty());

    let bastion = soon(add(host("bastion", "10.0.0.1"))).await.unwrap();
    let behind = HostDraft {
        route: HostRoute::Jump {
            hosts: vec![Some(bastion)],
        },
        ..host("db", "10.0.0.2")
    };
    let db = soon(add(behind.clone())).await.unwrap();
    assert!(matches!(
        soon(store.update_host(
            db,
            HostDraft {
                route: HostRoute::Jump {
                    hosts: vec![Some(db)]
                },
                ..behind
            },
            keep(),
            keep()
        ))
        .await,
        Err(StoreError::JumpThroughItself)
    ));
    // Another host goes through it: it stays an SSH host.
    let telnet = HostDraft {
        protocol: HostProtocol::Telnet {
            user_prompt: String::new(),
            password_prompt: String::new(),
        },
        port: 23,
        ..host("bastion", "10.0.0.1")
    };
    assert!(matches!(
        soon(store.update_host(bastion, telnet, keep(), keep())).await,
        Err(StoreError::HasDependents)
    ));
    assert_eq!(store.host(bastion).unwrap().protocol, HostProtocol::Ssh);

    let top = store
        .add_group(GroupDraft {
            name: "top".into(),
            parent: None,
        })
        .unwrap();
    let inner = store
        .add_group(GroupDraft {
            name: "inner".into(),
            parent: Some(top),
        })
        .unwrap();
    assert!(matches!(
        store.add_group(GroupDraft {
            name: " ".into(),
            parent: None
        }),
        Err(StoreError::GroupName)
    ));
    assert!(matches!(
        store.update_group(
            top,
            GroupDraft {
                name: "top".into(),
                parent: Some(inner)
            }
        ),
        Err(StoreError::GroupIntoItself)
    ));
    assert!(matches!(store.delete_host(99), Err(StoreError::NotFound)));
    assert!(matches!(core.connect_host(99), Err(StoreError::NotFound)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_credentials_keep_their_keys_in_the_data_folder() {
    let data = tempfile::tempdir().unwrap();
    let keychain = Arc::new(TestKeychain::default());
    let core = core_with(data.path(), &keychain);
    let store = core.store();

    let key = soon(store.generate_key(KeyAlgorithm::Ed25519))
        .await
        .unwrap();
    let public = key.public_key("phone".into());
    assert!(public.starts_with("ssh-ed25519 "), "{public}");
    let draft = CredentialDraft {
        name: "phone".into(),
        kind: CredentialKind::Key,
        user: "deploy".into(),
    };
    let id = soon(store.add_credential(
        draft.clone(),
        KeyChange::Generated {
            key,
            passphrase: Some("open sesame".into()),
        },
        SecretChange::Keep,
    ))
    .await
    .unwrap();

    let kept: Vec<_> = std::fs::read_dir(data.path().join("keys"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let [path] = kept.as_slice() else {
        panic!("one key file: {kept:?}");
    };
    let passphrase = SecretRef::Passphrase {
        key_path: path.display().to_string(),
    };
    assert_eq!(
        keychain.get(passphrase.clone()).as_deref(),
        Some("open sesame")
    );
    let text = std::fs::read_to_string(path).unwrap();
    assert!(check_private_key(text.clone()).unwrap().encrypted);
    let shown = soon(store.public_key(id)).await.unwrap();
    assert_eq!(
        shown.split(' ').take(2).collect::<Vec<_>>(),
        public.split(' ').take(2).collect::<Vec<_>>()
    );

    // A pasted key: a public one is no use.
    assert!(matches!(
        check_private_key(public.clone()),
        Err(KeyProblem::PublicKey)
    ));
    assert!(matches!(
        soon(store.update_credential(
            id,
            draft.clone(),
            KeyChange::Text {
                text: public,
                passphrase: None
            },
            SecretChange::Keep
        ))
        .await,
        Err(StoreError::Key {
            problem: KeyProblem::PublicKey
        })
    ));

    // A host using it logs in as its user.
    let using = HostDraft {
        auth: HostAuth::Credential { id },
        user: String::new(),
        ..host("app", "10.0.0.3")
    };
    let app = soon(store.add_host(using, SecretChange::Keep, SecretChange::Keep))
        .await
        .unwrap();
    assert_eq!(store.host(app).unwrap().user, "deploy");
    assert_eq!(store.hosts_using(id), [app]);

    // Deleted, its hosts log in on their own; its key file and the
    // passphrase go.
    assert_eq!(store.delete_credential(id).unwrap(), [app]);
    assert_eq!(store.host(app).unwrap().auth, HostAuth::NoPassword);
    assert!(!path.exists());
    until(|| keychain.get(passphrase.clone()).is_none()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_host_connects_and_goes_to_the_front_of_the_recent_ones() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    let keychain = Arc::new(TestKeychain::default());
    let core = core_with(data.path(), &keychain);
    let store = core.store();
    let draft = HostDraft {
        port: server.port,
        user: TEST_USER.into(),
        ..host("test", "127.0.0.1")
    };
    let id = soon(store.add_host(draft, set(TEST_PASSWORD), SecretChange::Keep))
        .await
        .unwrap();
    assert!(store.recent_hosts().is_empty());

    let connection = core.connect_host(id).unwrap();
    loop {
        match next(&connection).await {
            ConnectionEvent::Prompt { prompt } => {
                assert!(matches!(prompt.kind, PromptKind::UnknownHost { .. }));
                connection.answer(prompt.id, PromptReply::TrustAndSave);
            }
            ConnectionEvent::Connected { .. } => break,
            event => panic!("{event:?}"),
        }
    }
    until(|| store.recent_hosts().first().map(|host| host.id) == Some(id)).await;
    // The server answers neither step of the probe as a system would: the
    // host keeps the mark it had.
    until(|| server.seen().commands == [PROBE_COMMAND, WINDOWS_PROBE_COMMAND]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(store.host(id).unwrap().os, None);

    let telnet = HostDraft {
        protocol: HostProtocol::Telnet {
            user_prompt: String::new(),
            password_prompt: String::new(),
        },
        port: 23,
        ..host("switch", "10.0.0.9")
    };
    let telnet = soon(store.add_host(telnet, SecretChange::Keep, SecretChange::Keep))
        .await
        .unwrap();
    assert!(matches!(core.connect_host(telnet), Err(StoreError::NotSsh)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_saved_host_is_asked_what_system_it_runs_on() {
    let data = tempfile::tempdir().unwrap();
    let server = TestServer::start(data.path().into()).await.unwrap();
    server.reply(
        PROBE_COMMAND,
        "Linux\nNAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n",
    );
    let keychain = Arc::new(TestKeychain::default());
    let core = core_with(data.path(), &keychain);
    let store = core.store();
    let draft = HostDraft {
        port: server.port,
        user: TEST_USER.into(),
        ..host("test", "127.0.0.1")
    };
    let id = soon(store.add_host(draft, set(TEST_PASSWORD), SecretChange::Keep))
        .await
        .unwrap();
    assert_eq!(store.host(id).unwrap().os, None);

    let connection = core.connect_host(id).unwrap();
    loop {
        match next(&connection).await {
            ConnectionEvent::Prompt { prompt } => {
                connection.answer(prompt.id, PromptReply::TrustAndSave);
            }
            ConnectionEvent::Connected { .. } => break,
            event => panic!("{event:?}"),
        }
    }
    until(|| store.host(id).unwrap().os.is_some()).await;
    let os = store.host(id).unwrap().os.unwrap();
    assert_eq!((os.key.as_str(), os.name.as_str()), ("ubuntu", "Ubuntu"));
    assert_eq!(server.seen().commands, [PROBE_COMMAND]);
}

/// A keychain that is locked.
struct Locked;

impl Keychain for Locked {
    fn available(&self) -> bool {
        false
    }

    fn get(&self, _: String) -> Result<Option<String>, KeychainError> {
        Err(KeychainError::Unavailable)
    }

    fn set(&self, _: String, _: String) -> Result<(), KeychainError> {
        Err(KeychainError::Unavailable)
    }

    fn delete(&self, _: String) -> Result<(), KeychainError> {
        Err(KeychainError::Unavailable)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_password_the_keychain_did_not_take_is_said_and_the_host_kept() {
    let data = tempfile::tempdir().unwrap();
    let core = Core::new(config(data.path()), Arc::new(Locked)).unwrap();
    let store = core.store();
    let id = soon(store.add_host(host("web", "10.0.0.1"), set("hunter2"), SecretChange::Keep))
        .await
        .unwrap();
    assert!(store.host(id).is_some());
    let failed = event(&store, |event| matches!(event, StoreEvent::Failed { .. })).await;
    assert_eq!(
        failed,
        StoreEvent::Failed {
            failure: StoreFailure::Secret {
                secret: password_of("10.0.0.1"),
                writing: true,
                problem: SecretProblem::Unavailable,
            }
        }
    );
    assert!(matches!(
        soon(store.saved_secret(SavedSecret::HostPassword { host: id })).await,
        Err(StoreError::Secret {
            problem: SecretProblem::Unavailable
        })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_says_what_goes_with_a_host_or_a_group() {
    let data = tempfile::tempdir().unwrap();
    // What only the desktop makes: a port forward, and a serial host the
    // app never lists.
    {
        let database = hosts::HostDatabase::open(&data.path().join("shellrs.db")).unwrap();
        let mut saved = hosts::HostData::load(database).unwrap();
        let group = saved.insert_group(hosts::GroupDraft::new("生产", None));
        let inner = saved.insert_group(hosts::GroupDraft::new("数据库", Some(group)));
        let web = saved.insert(hosts::HostDraft::new(
            "web",
            "10.0.0.1",
            22,
            "root",
            hosts::AuthKind::Password,
            Some(group),
        ));
        saved.insert(hosts::HostDraft::serial(
            "console",
            "/dev/ttyUSB0",
            hosts::SerialSettings::default(),
            Some(inner),
        ));
        let endpoint = |port| hosts::ForwardEndpoint::new("127.0.0.1", port);
        saved
            .insert_forward(hosts::ForwardDraft::new(
                hosts::ForwardKind::Local,
                web,
                endpoint(8080),
                Some(endpoint(80)),
            ))
            .unwrap();
    }
    let keychain = Arc::new(TestKeychain::default());
    let core = core_with(data.path(), &keychain);
    let store = core.store();
    let web = store.hosts()[0].id;
    let group = store.groups()[0].id;
    // A host outside the group that jumps through the one in it.
    let jumping = HostDraft {
        route: HostRoute::Jump {
            hosts: vec![Some(web)],
        },
        ..host("app", "10.0.0.2")
    };
    soon(store.add_host(jumping, SecretChange::Keep, SecretChange::Keep))
        .await
        .unwrap();

    let both = Dependents {
        forwards: 1,
        jump_users: 1,
        ..Dependents::default()
    };
    assert_eq!(store.dependents(TreeNode::Host { id: web }), both);
    assert_eq!(
        store.dependents(TreeNode::Group { id: group }),
        Dependents {
            hosts: 2,
            subgroups: 1,
            ..both
        }
    );
    assert_eq!(
        store.dependents(TreeNode::Host { id: 999 }),
        Dependents::default()
    );
}
