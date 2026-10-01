//! Loopback-only tests of reaching a host through jump hosts and proxies,
//! against in-process SSH servers and proxies. No user hosts, no keychain.
use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::Duration,
};

use anyhow::Result;
use russh::{
    ChannelOpenFailure, MethodKind, MethodSet,
    server::{self, Server as _},
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::watch,
};

use super::{SshConnectionConfig, SshConnectionTester, SshConnector, SshPrompts};
use crate::{
    connection::{
        ConnectionPrompt, ConnectionPromptKind, ConnectionPromptReply, ConnectionSecret,
        ConnectionTester as _, LoginTest,
    },
    secrets::{InMemorySecretStore, SecretRef, SecretStore as _},
    session::{
        AuthKind, JumpLogin, LoginRoute, ProxyKind, ProxyLogin, ProxySettings, SessionLogin,
    },
};

/// An SSH server that lets one user in with one password and forwards
/// direct-tcpip channels: a jump host, and with nothing asked of it beyond
/// the login, the host behind one too.
struct RouteServer {
    port: u16,
    state: Arc<Mutex<RouteState>>,
    _runtime: tokio::runtime::Runtime,
}

#[derive(Default)]
struct RouteState {
    /// Where each direct-tcpip channel asked to go.
    targets: Vec<(String, u32)>,
    /// Connections that have ended.
    ended: usize,
}

#[derive(Clone)]
struct Accepting {
    user: &'static str,
    password: &'static str,
    state: Arc<Mutex<RouteState>>,
}

struct RouteHandler {
    accepting: Accepting,
}

impl Drop for RouteHandler {
    fn drop(&mut self) {
        self.accepting.state.lock().unwrap().ended += 1;
    }
}

impl server::Server for Accepting {
    type Handler = RouteHandler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> RouteHandler {
        RouteHandler {
            accepting: self.clone(),
        }
    }
}

impl server::Handler for RouteHandler {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<server::Auth> {
        Ok(
            if user == self.accepting.user && password == self.accepting.password {
                server::Auth::Accept
            } else {
                server::Auth::Reject {
                    proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])),
                    partial_success: false,
                }
            },
        )
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: russh::Channel<server::Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        self.accepting
            .state
            .lock()
            .unwrap()
            .targets
            .push((host_to_connect.to_string(), port_to_connect));
        match TcpStream::connect((host_to_connect, port_to_connect as u16)).await {
            Ok(mut socket) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                });
            }
            Err(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap()
}

/// A loopback listener, or `None` in a sandbox without loopback sockets.
fn listen(runtime: &tokio::runtime::Runtime) -> Option<TcpListener> {
    match runtime.block_on(TcpListener::bind(("127.0.0.1", 0))) {
        Ok(listener) => Some(listener),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => None,
        Err(error) => panic!("failed to bind: {error}"),
    }
}

fn start_server(user: &'static str, password: &'static str) -> Option<RouteServer> {
    let runtime = runtime();
    let listener = listen(&runtime)?;
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(RouteState::default()));
    let mut server = Accepting {
        user,
        password,
        state: state.clone(),
    };
    let mut rng = russh::keys::key::safe_rng();
    let key = russh::keys::PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519).unwrap();
    let config = Arc::new(server::Config {
        methods: MethodSet::from(&[MethodKind::Password][..]),
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key],
        ..server::Config::default()
    });
    runtime.spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    Some(RouteServer {
        port,
        state,
        _runtime: runtime,
    })
}

impl RouteServer {
    fn ended(&self) -> usize {
        self.state.lock().unwrap().ended
    }

    fn targets(&self) -> Vec<(String, u32)> {
        self.state.lock().unwrap().targets.clone()
    }
}

/// A proxy that tunnels to whatever it is asked for, and remembers what.
struct Relay {
    port: u16,
    targets: Arc<Mutex<Vec<String>>>,
    _runtime: tokio::runtime::Runtime,
}

fn start_relay(kind: ProxyKind, auth: Option<(&'static str, &'static str)>) -> Option<Relay> {
    let runtime = runtime();
    let listener = listen(&runtime)?;
    let port = listener.local_addr().unwrap().port();
    let targets = Arc::new(Mutex::new(Vec::new()));
    let seen = targets.clone();
    runtime.spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let _ = match kind {
                    ProxyKind::Http => relay_http(socket, seen).await,
                    ProxyKind::Socks5 => relay_socks5(socket, auth, seen).await,
                };
            });
        }
    });
    Some(Relay {
        port,
        targets,
        _runtime: runtime,
    })
}

async fn relay_http(mut socket: TcpStream, seen: Arc<Mutex<Vec<String>>>) -> Result<()> {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(socket.read_u8().await?);
    }
    let head = String::from_utf8(head)?;
    let target = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    seen.lock().unwrap().push(target.clone());
    let mut upstream = TcpStream::connect(target.as_str()).await?;
    socket
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await?;
    tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
    Ok(())
}

async fn relay_socks5(
    mut socket: TcpStream,
    auth: Option<(&'static str, &'static str)>,
    seen: Arc<Mutex<Vec<String>>>,
) -> Result<()> {
    let mut greeting = [0u8; 2];
    socket.read_exact(&mut greeting).await?;
    let mut methods = vec![0u8; usize::from(greeting[1])];
    socket.read_exact(&mut methods).await?;
    match auth {
        None => socket.write_all(&[5, 0]).await?,
        Some((user, password)) => {
            socket.write_all(&[5, 2]).await?;
            let mut header = [0u8; 2];
            socket.read_exact(&mut header).await?;
            let mut given_user = vec![0u8; usize::from(header[1])];
            socket.read_exact(&mut given_user).await?;
            let mut given_password = vec![0u8; usize::from(socket.read_u8().await?)];
            socket.read_exact(&mut given_password).await?;
            if given_user != user.as_bytes() || given_password != password.as_bytes() {
                socket.write_all(&[1, 1]).await?;
                return Ok(());
            }
            socket.write_all(&[1, 0]).await?;
        }
    }
    let mut request = [0u8; 4];
    socket.read_exact(&mut request).await?;
    let host = match request[3] {
        1 => {
            let mut address = [0u8; 4];
            socket.read_exact(&mut address).await?;
            std::net::Ipv4Addr::from(address).to_string()
        }
        3 => {
            let mut name = vec![0u8; usize::from(socket.read_u8().await?)];
            socket.read_exact(&mut name).await?;
            String::from_utf8(name)?
        }
        other => anyhow::bail!("address type {other}"),
    };
    let port = socket.read_u16().await?;
    seen.lock().unwrap().push(format!("{host}:{port}"));
    let Ok(mut upstream) = TcpStream::connect((host.as_str(), port)).await else {
        socket.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        return Ok(());
    };
    socket.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    tokio::io::copy_bidirectional(&mut socket, &mut upstream).await?;
    Ok(())
}

const TARGET_USER: &str = "tester";
const TARGET_PASSWORD: &str = "target-password";
const JUMP_USER: &str = "jumper";
const JUMP_PASSWORD: &str = "jump-password";

fn manual(host: &str, port: u16, user: &str) -> SessionLogin {
    SessionLogin::manual(host, port, user, AuthKind::Password)
}

fn hop(name: &str, port: u16) -> JumpLogin {
    JumpLogin::Host {
        name: name.into(),
        login: Box::new(manual("127.0.0.1", port, JUMP_USER)),
    }
}

/// Prompts that answer every question as a person would: the hosts' keys
/// trusted, each password typed, the jump hosts' told apart by their names.
/// What was asked is recorded.
fn answering_broker() -> (
    Arc<SshPrompts>,
    Arc<Mutex<Vec<ConnectionPromptKind>>>,
    watch::Sender<bool>,
) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let slot: Arc<OnceLock<Weak<SshPrompts>>> = Arc::new(OnceLock::new());
    let events = {
        let (asked, slot) = (asked.clone(), slot.clone());
        Arc::new(move |prompt: ConnectionPrompt| {
            let reply = match prompt.kind() {
                ConnectionPromptKind::UnknownHost(_) => ConnectionPromptReply::TrustAndSave,
                ConnectionPromptKind::Authentication(question) => {
                    let password = if question.instructions().starts_with("跳板主机") {
                        JUMP_PASSWORD
                    } else {
                        TARGET_PASSWORD
                    };
                    ConnectionPromptReply::Answers(vec![ConnectionSecret::new(password)])
                }
                ConnectionPromptKind::HostKeyChanged(_) => ConnectionPromptReply::Cancel,
            };
            asked.lock().unwrap().push(prompt.kind().clone());
            if let Some(broker) = slot.get().and_then(Weak::upgrade) {
                broker.respond(prompt.request_id(), reply);
            }
            true
        })
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let broker = Arc::new(SshPrompts::new(events, shutdown_rx));
    let _ = slot.set(Arc::downgrade(&broker));
    (broker, asked, shutdown_tx)
}

/// A connection test the way the host form runs one, trusting every host.
fn test_login(
    known_hosts: &Path,
    keychain: Arc<InMemorySecretStore>,
    request: LoginTest,
) -> Result<(), String> {
    SshConnectionTester::new(SshConnector::new(known_hosts, keychain))
        .test(request, Box::new(|_| true))
}

#[test]
fn a_host_behind_two_jump_hosts_is_reached_through_them_in_order() {
    let (Some(first), Some(second), Some(target)) = (
        start_server(JUMP_USER, JUMP_PASSWORD),
        start_server(JUMP_USER, JUMP_PASSWORD),
        start_server(TARGET_USER, TARGET_PASSWORD),
    ) else {
        eprintln!("loopback sockets are unavailable in this sandbox; skipping");
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let connector = SshConnector::new(
        directory.path().join("known_hosts"),
        Arc::new(InMemorySecretStore::default()),
    );
    let login = manual("127.0.0.1", target.port, TARGET_USER).with_route(LoginRoute::Jump(vec![
        hop("阿里云99", first.port),
        hop("禅道", second.port),
    ]));

    let (broker, asked, _shutdown) = answering_broker();
    let ended = runtime().block_on(async {
        let (handle, _) = connector
            .connect(&SshConnectionConfig::from(&login), broker)
            .await
            .expect("connect through the jump hosts");
        handle
            .disconnect(russh::Disconnect::ByApplication, "done", "zh-CN")
            .await
            .unwrap();
        drop(handle);
        // Closing the connection to the host closes the ones it went
        // through, while this runtime, which runs them, is still up.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while (first.ended() < 1 || second.ended() < 1) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        (first.ended(), second.ended())
    });
    assert_eq!(ended, (1, 1));

    // Each hop opened the way to the next.
    assert_eq!(
        first.targets(),
        [("127.0.0.1".to_string(), u32::from(second.port))]
    );
    assert_eq!(
        second.targets(),
        [("127.0.0.1".to_string(), u32::from(target.port))]
    );

    let asked: Vec<String> = asked
        .lock()
        .unwrap()
        .iter()
        .map(|kind| match kind {
            ConnectionPromptKind::UnknownHost(question) => {
                question.description().lines().next().unwrap().to_string()
            }
            ConnectionPromptKind::Authentication(question) => question.instructions().to_string(),
            other => panic!("unexpected question: {other:?}"),
        })
        .collect();
    assert_eq!(
        asked,
        [
            "跳板主机：阿里云99".to_string(),
            "跳板主机「阿里云99」：请输入登录密码".to_string(),
            "跳板主机：禅道".to_string(),
            "跳板主机「禅道」：请输入登录密码".to_string(),
            format!("主机：127.0.0.1:{}", target.port),
            "请输入登录密码".to_string(),
        ]
    );
}

#[test]
fn a_worker_that_stops_with_a_connection_through_a_jump_host_still_open_stops_cleanly() {
    let (Some(jump), Some(target)) = (
        start_server(JUMP_USER, JUMP_PASSWORD),
        start_server(TARGET_USER, TARGET_PASSWORD),
    ) else {
        eprintln!("loopback sockets are unavailable in this sandbox; skipping");
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let connector = SshConnector::new(
        directory.path().join("known_hosts"),
        Arc::new(InMemorySecretStore::default()),
    );
    let login = manual("127.0.0.1", target.port, TARGET_USER)
        .with_route(LoginRoute::Jump(vec![hop("阿里云99", jump.port)]));
    let (broker, _, _shutdown) = answering_broker();
    // A worker's own runtime, dropped with the connection still up: what
    // the jump host's channel does as it goes must not need the runtime.
    let worker = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (handle, _) = worker
        .block_on(connector.connect(&SshConnectionConfig::from(&login), broker))
        .expect("connect through the jump host");
    drop(worker);
    drop(handle);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while jump.ended() < 1 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(jump.ended(), 1);
}

#[test]
fn a_jump_host_that_cannot_log_in_is_named() {
    let (Some(jump), Some(target)) = (
        start_server(JUMP_USER, JUMP_PASSWORD),
        start_server(TARGET_USER, TARGET_PASSWORD),
    ) else {
        eprintln!("loopback sockets are unavailable in this sandbox; skipping");
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let known_hosts = directory.path().join("known_hosts");
    let keychain = Arc::new(InMemorySecretStore::default());
    let request = || {
        LoginTest::typed(
            manual("127.0.0.1", target.port, TARGET_USER)
                .with_route(LoginRoute::Jump(vec![hop("阿里云99", jump.port)])),
        )
        .with_password(TARGET_PASSWORD)
    };

    // The form's password is the host's; the jump host's is its own, saved.
    assert_eq!(
        test_login(&known_hosts, keychain.clone(), request()),
        Err("跳板主机「阿里云99」：未填写密码".into())
    );
    keychain
        .set(
            &SecretRef::password(JUMP_USER, "127.0.0.1", jump.port),
            JUMP_PASSWORD,
        )
        .unwrap();
    assert_eq!(test_login(&known_hosts, keychain, request()), Ok(()));
}

#[test]
fn a_jump_host_that_cannot_reach_the_next_says_so() {
    let Some(jump) = start_server(JUMP_USER, JUMP_PASSWORD) else {
        eprintln!("loopback sockets are unavailable in this sandbox; skipping");
        return;
    };
    let closed = closed_port();
    let directory = tempfile::tempdir().unwrap();
    let keychain = Arc::new(InMemorySecretStore::default());
    keychain
        .set(
            &SecretRef::password(JUMP_USER, "127.0.0.1", jump.port),
            JUMP_PASSWORD,
        )
        .unwrap();
    let request = LoginTest::typed(
        manual("127.0.0.1", closed, TARGET_USER)
            .with_route(LoginRoute::Jump(vec![hop("阿里云99", jump.port)])),
    );
    assert_eq!(
        test_login(&directory.path().join("known_hosts"), keychain, request),
        Err(format!(
            "跳板主机「阿里云99」无法连接到 127.0.0.1:{closed}：连接失败"
        ))
    );
}

#[test]
fn a_deleted_jump_host_fails_before_anything_is_connected() {
    let directory = tempfile::tempdir().unwrap();
    let request = LoginTest::typed(manual("10.0.0.5", 22, TARGET_USER).with_route(
        LoginRoute::Jump(vec![hop("阿里云99", 1), JumpLogin::Deleted]),
    ));
    assert_eq!(
        test_login(
            &directory.path().join("known_hosts"),
            Arc::new(InMemorySecretStore::default()),
            request
        ),
        Err("第 2 台跳板主机已被删除，请编辑这台主机的连接方式".into())
    );
}

fn through(kind: ProxyKind, port: u16, user: Option<&str>) -> LoginRoute {
    let proxy = ProxySettings::new(kind, "127.0.0.1", port).with_user(user.unwrap_or_default());
    LoginRoute::Proxy(ProxyLogin::from(&proxy))
}

#[test]
fn a_host_behind_a_socks5_proxy_is_reached_by_name_with_the_proxys_password() {
    let (Some(relay), Some(target)) = (
        start_relay(ProxyKind::Socks5, Some(("me", "proxy-password"))),
        start_server(TARGET_USER, TARGET_PASSWORD),
    ) else {
        eprintln!("loopback sockets are unavailable in this sandbox; skipping");
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let known_hosts = directory.path().join("known_hosts");
    let keychain = Arc::new(InMemorySecretStore::default());
    // The name goes to the proxy, which resolves it.
    let request = |proxy_password: &str| {
        LoginTest::typed(
            manual("localhost", target.port, TARGET_USER).with_route(through(
                ProxyKind::Socks5,
                relay.port,
                Some("me"),
            )),
        )
        .with_password(TARGET_PASSWORD)
        .with_proxy_password(proxy_password)
    };

    assert_eq!(
        test_login(&known_hosts, keychain.clone(), request("wrong")),
        Err("代理服务器拒绝了用户名或密码".into())
    );
    assert_eq!(
        test_login(&known_hosts, keychain, request("proxy-password")),
        Ok(())
    );
    assert_eq!(
        relay.targets.lock().unwrap().last().cloned(),
        Some(format!("localhost:{}", target.port))
    );
}

#[test]
fn a_host_behind_an_http_proxy_is_reached_through_it() {
    let (Some(relay), Some(target)) = (
        start_relay(ProxyKind::Http, None),
        start_server(TARGET_USER, TARGET_PASSWORD),
    ) else {
        eprintln!("loopback sockets are unavailable in this sandbox; skipping");
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let request = LoginTest::typed(
        manual("127.0.0.1", target.port, TARGET_USER).with_route(through(
            ProxyKind::Http,
            relay.port,
            None,
        )),
    )
    .with_password(TARGET_PASSWORD);
    assert_eq!(
        test_login(
            &directory.path().join("known_hosts"),
            Arc::new(InMemorySecretStore::default()),
            request
        ),
        Ok(())
    );
    assert_eq!(
        *relay.targets.lock().unwrap(),
        [format!("127.0.0.1:{}", target.port)]
    );
}

/// A loopback port nothing listens on.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn a_proxy_that_is_not_there_is_named() {
    let directory = tempfile::tempdir().unwrap();
    let port = closed_port();
    let request = LoginTest::typed(manual("10.0.0.5", 22, TARGET_USER).with_route(through(
        ProxyKind::Socks5,
        port,
        None,
    )));
    assert_eq!(
        test_login(
            &directory.path().join("known_hosts"),
            Arc::new(InMemorySecretStore::default()),
            request
        ),
        Err(format!(
            "无法连接代理服务器 127.0.0.1:{port}：连接被拒绝，该端口上没有服务在监听"
        ))
    );
}
