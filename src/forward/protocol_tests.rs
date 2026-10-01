//! Loopback-only tests of the real forward worker against an in-process SSH
//! server. No user hosts, no keychain.
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use russh::{
    ChannelOpenFailure,
    server::{self, Server as _},
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::watch,
};

use super::*;
use crate::{
    connection::{ConnectionPromptKind, ConnectionPromptReply, ConnectionSecret},
    host::{
        AuthKind, ForwardDraft, ForwardEndpoint, ForwardId, ForwardKind, ForwardRule, Host,
        HostDraft, HostId, HostLogin,
    },
    secrets::{InMemorySecretStore, SecretRef, SecretStore as _},
    ssh::{SshConnectionConfig, SshConnector, SshPrompts},
};

const USER: &str = "tester";
const PASSWORD: &str = "fixture-password";

/// What the fixture server allows, and what it has seen.
#[derive(Default)]
struct Policy {
    /// Refuse every direct-tcpip channel, as `AllowTcpForwarding no` does.
    forbid_direct: AtomicBool,
    /// Refuse to listen for a remote forward.
    forbid_remote: AtomicBool,
    connections: AtomicUsize,
    ended: AtomicUsize,
    /// The logged-in connections, so a test can hang up on them.
    logged_in: Mutex<Vec<server::Handle>>,
}

struct TestServer {
    policy: Arc<Policy>,
}

struct Handler {
    policy: Arc<Policy>,
    /// The ports this connection made the server listen on.
    listeners: Vec<tokio::task::JoinHandle<()>>,
}

impl server::Server for TestServer {
    type Handler = Handler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Handler {
        self.policy.connections.fetch_add(1, Ordering::SeqCst);
        Handler {
            policy: self.policy.clone(),
            listeners: Vec::new(),
        }
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        for listener in &self.listeners {
            listener.abort();
        }
        self.policy.ended.fetch_add(1, Ordering::SeqCst);
    }
}

impl server::Handler for Handler {
    type Error = anyhow::Error;

    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == USER && password == PASSWORD {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }

    async fn auth_succeeded(&mut self, session: &mut server::Session) -> Result<(), Self::Error> {
        self.policy.logged_in.lock().unwrap().push(session.handle());
        Ok(())
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
    ) -> Result<(), Self::Error> {
        if self.policy.forbid_direct.load(Ordering::SeqCst) {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
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

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        session: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        if self.policy.forbid_remote.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let Ok(listener) = TcpListener::bind((address, *port as u16)).await else {
            return Ok(false);
        };
        let bound = u32::from(listener.local_addr()?.port());
        *port = bound;
        let handle = session.handle();
        let address = address.to_string();
        self.listeners.push(tokio::spawn(async move {
            while let Ok((mut socket, peer)) = listener.accept().await {
                let (handle, address) = (handle.clone(), address.clone());
                tokio::spawn(async move {
                    // A client that refuses the channel closes the socket.
                    if let Ok(channel) = handle
                        .channel_open_forwarded_tcpip(
                            address,
                            bound,
                            peer.ip().to_string(),
                            peer.port().into(),
                        )
                        .await
                    {
                        let mut stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                    }
                });
            }
        }));
        Ok(true)
    }

    async fn cancel_tcpip_forward(
        &mut self,
        _: &str,
        _: u32,
        _: &mut server::Session,
    ) -> Result<bool, Self::Error> {
        for listener in self.listeners.drain(..) {
            listener.abort();
        }
        Ok(true)
    }
}

struct Running {
    port: u16,
    policy: Arc<Policy>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Running {
    /// End every logged-in connection from the server's side.
    async fn hang_up(&self) {
        let logged_in: Vec<_> = self.policy.logged_in.lock().unwrap().drain(..).collect();
        for handle in logged_in {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "bye".into(), "en".into())
                .await;
        }
    }

    /// Stop accepting connections, as a host that went away would.
    async fn stop_listening(&mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

async fn server() -> Option<Running> {
    let listener = match TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("loopback unavailable; run this test with local socket permission");
            return None;
        }
        Err(error) => panic!("{error}"),
    };
    let port = listener.local_addr().unwrap().port();
    let policy = Arc::new(Policy::default());
    let mut server = TestServer {
        policy: policy.clone(),
    };
    let key = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .unwrap();
    let config = Arc::new(server::Config {
        methods: russh::MethodSet::from(&[russh::MethodKind::Password][..]),
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        server.run_on_socket(config, &listener).await.unwrap();
    });
    Some(Running { port, policy, task })
}

/// A TCP service that sends back what it receives, and its port.
async fn echo_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut reader, mut writer) = socket.split();
                let _ = tokio::io::copy(&mut reader, &mut writer).await;
            });
        }
    });
    (port, task)
}

/// A port nothing listens on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The client side of one test: where trust and passwords are kept.
struct Fixture {
    _data: tempfile::TempDir,
    connector: SshConnector,
    secrets: Arc<InMemorySecretStore>,
    ssh_port: u16,
}

impl Fixture {
    fn new(ssh_port: u16) -> Self {
        let data = tempfile::tempdir().unwrap();
        let secrets = Arc::new(InMemorySecretStore::default());
        Self {
            connector: SshConnector::new(data.path().join("known_hosts"), secrets.clone()),
            _data: data,
            secrets,
            ssh_port,
        }
    }

    /// Save the login password, so logging in again needs nobody.
    fn save_password(&self) {
        self.secrets
            .set(
                &SecretRef::password(USER, "127.0.0.1", self.ssh_port),
                PASSWORD,
            )
            .unwrap();
    }

    fn host(&self) -> Host {
        Host::new(
            HostId(1),
            HostDraft::new(
                "fixture",
                "127.0.0.1",
                self.ssh_port,
                USER,
                AuthKind::Password,
                None,
            ),
        )
    }

    fn start(&self, kind: ForwardKind, bind: u16, target: Option<u16>) -> Run {
        self.start_with(kind, bind, target, &[])
    }

    fn start_with(
        &self,
        kind: ForwardKind,
        bind: u16,
        target: Option<u16>,
        reconnect_delays: &[Duration],
    ) -> Run {
        let provider = SshForwardTransportProvider::new(self.connector.clone())
            .with_reconnect_delays(reconnect_delays);
        let rule = ForwardRule::new(
            ForwardId(1),
            ForwardDraft::new(
                kind,
                HostId(1),
                ForwardEndpoint::new("127.0.0.1", bind),
                target.map(|port| ForwardEndpoint::new("127.0.0.1", port)),
            ),
        );
        let transport = provider.create(&rule, &HostLogin::of(&self.host(), None));
        let (commands, command_receiver) = async_channel::unbounded();
        let (event_sender, events) = async_channel::unbounded();
        let thread = std::thread::spawn(move || transport.run(command_receiver, event_sender));
        Run {
            commands,
            events,
            thread: Some(thread),
        }
    }
}

/// One run of the worker on its own thread.
struct Run {
    commands: async_channel::Sender<ForwardCommand>,
    events: async_channel::Receiver<ForwardEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.commands.try_send(ForwardCommand::Stop);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl Run {
    /// The next event exactly as the worker sent it.
    async fn raw(&self) -> ForwardEvent {
        tokio::time::timeout(Duration::from_secs(20), self.events.recv())
            .await
            .expect("worker timed out")
            .expect("worker ended without a last event")
    }

    /// The next event that is not a question; questions are answered the
    /// way a user who knows the fixture would.
    async fn next(&self) -> ForwardEvent {
        loop {
            let event = self.raw().await;
            let ForwardEvent::Prompt(prompt) = event else {
                return event;
            };
            let reply = match prompt.kind() {
                ConnectionPromptKind::UnknownHost(_) => ConnectionPromptReply::TrustAndSave,
                ConnectionPromptKind::Authentication(_) => {
                    ConnectionPromptReply::Answers(vec![ConnectionSecret::new(PASSWORD)])
                }
                ConnectionPromptKind::HostKeyChanged(_) => panic!("unexpected trust change"),
            };
            self.commands
                .send(ForwardCommand::PromptReply {
                    request_id: prompt.request_id(),
                    reply,
                })
                .await
                .unwrap();
        }
    }

    /// The next event other than a connection count.
    async fn next_state(&self) -> ForwardEvent {
        loop {
            match self.next().await {
                ForwardEvent::Connections(_) => {}
                event => return event,
            }
        }
    }

    async fn listening(&self) {
        assert_eq!(self.next_state().await, ForwardEvent::Connecting);
        assert_eq!(self.next_state().await, ForwardEvent::Listening);
    }

    async fn stop(&self) {
        self.commands.send(ForwardCommand::Stop).await.unwrap();
        loop {
            match self.next().await {
                ForwardEvent::Stopped => return,
                ForwardEvent::Connections(_) => {}
                event => panic!("expected the forward to stop, got {event:?}"),
            }
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

async fn connect(port: u16) -> TcpStream {
    TcpStream::connect(("127.0.0.1", port)).await.unwrap()
}

/// Send `message` and read it back.
async fn echoes(socket: &mut TcpStream, message: &[u8]) {
    socket.write_all(message).await.unwrap();
    let mut answer = vec![0; message.len()];
    tokio::time::timeout(Duration::from_secs(10), socket.read_exact(&mut answer))
        .await
        .expect("no answer through the forward")
        .unwrap();
    assert_eq!(answer, message);
}

/// Whether the other end closed the connection without sending anything.
async fn closed(socket: &mut TcpStream) -> bool {
    let mut byte = [0_u8; 1];
    matches!(
        tokio::time::timeout(Duration::from_secs(10), socket.read(&mut byte)).await,
        Ok(Ok(0)) | Ok(Err(_))
    )
}

async fn wait_for(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_local_forward_carries_bytes_both_ways_until_it_is_stopped() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let (echo, _echo_task) = echo_server().await;
        let fixture = Fixture::new(server.port);
        let local = free_port();
        let run = fixture.start(ForwardKind::Local, local, Some(echo));
        run.listening().await;

        let mut socket = connect(local).await;
        echoes(&mut socket, b"ping").await;
        assert_eq!(run.next().await, ForwardEvent::Connections(1));
        // Closing our half reaches the target, whose own close comes back.
        socket.shutdown().await.unwrap();
        assert!(closed(&mut socket).await);
        assert_eq!(run.next().await, ForwardEvent::Connections(0));

        // Two at once are two connections over the one SSH connection.
        let mut first = connect(local).await;
        let mut second = connect(local).await;
        echoes(&mut first, b"one").await;
        echoes(&mut second, b"two").await;
        assert_eq!(server.policy.connections.load(Ordering::SeqCst), 1);

        run.stop().await;
        // Stopping logs out, closes what was open and frees the port.
        wait_for("the SSH connection to end", || {
            server.policy.ended.load(Ordering::SeqCst) == 1
        })
        .await;
        assert!(closed(&mut first).await);
        assert!(TcpStream::connect(("127.0.0.1", local)).await.is_err());
    });
}

#[test]
fn a_target_the_server_cannot_reach_fails_that_connection_only() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        let local = free_port();
        let nothing = free_port();
        let run = fixture.start(ForwardKind::Local, local, Some(nothing));
        run.listening().await;

        let mut socket = connect(local).await;
        assert!(closed(&mut socket).await);
        assert_eq!(
            run.next_state().await,
            ForwardEvent::ConnectionFailed(format!("服务器无法连接 127.0.0.1:{nothing}"))
        );

        // The forward itself is still up; so is the server's rule book.
        server.policy.forbid_direct.store(true, Ordering::SeqCst);
        let mut socket = connect(local).await;
        assert!(closed(&mut socket).await);
        let ForwardEvent::ConnectionFailed(reason) = run.next_state().await else {
            panic!("expected the refusal to be reported");
        };
        assert!(reason.contains("AllowTcpForwarding"), "{reason}");
    });
}

#[test]
fn a_taken_local_port_fails_before_anyone_is_asked_anything() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        let taken = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let run = fixture.start(ForwardKind::Local, port, Some(1));
        // Not `next`: a question here would mean it logged in first.
        assert_eq!(
            run.raw().await,
            ForwardEvent::Failed(format!("本机端口 {port} 已被占用"))
        );
        assert_eq!(server.policy.connections.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn a_remote_forward_brings_the_servers_port_to_a_local_target() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let (echo, _echo_task) = echo_server().await;
        let fixture = Fixture::new(server.port);
        let remote = free_port();
        let run = fixture.start(ForwardKind::Remote, remote, Some(echo));
        run.listening().await;

        // "On the server": the fixture listens on this machine's loopback.
        let mut socket = connect(remote).await;
        echoes(&mut socket, b"from the far side").await;
        assert_eq!(run.next().await, ForwardEvent::Connections(1));

        run.stop().await;
        wait_for("the server to stop listening", || {
            std::net::TcpStream::connect(("127.0.0.1", remote)).is_err()
        })
        .await;
    });
}

#[test]
fn a_remote_forward_whose_target_is_down_refuses_the_far_side() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        let remote = free_port();
        let nothing = free_port();
        let run = fixture.start(ForwardKind::Remote, remote, Some(nothing));
        run.listening().await;

        let mut socket = connect(remote).await;
        assert!(closed(&mut socket).await);
        let ForwardEvent::ConnectionFailed(reason) = run.next_state().await else {
            panic!("expected the failed dial to be reported");
        };
        assert!(
            reason.starts_with(&format!("无法连接本机目标 127.0.0.1:{nothing}：")),
            "{reason}"
        );
    });
}

#[test]
fn a_server_that_will_not_listen_fails_the_remote_forward() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        server.policy.forbid_remote.store(true, Ordering::SeqCst);
        let fixture = Fixture::new(server.port);
        let remote = free_port();
        let run = fixture.start(ForwardKind::Remote, remote, Some(1));
        assert_eq!(run.next_state().await, ForwardEvent::Connecting);
        let ForwardEvent::Failed(reason) = run.next_state().await else {
            panic!("expected the forward to fail");
        };
        assert!(
            reason.starts_with(&format!("服务器拒绝在 127.0.0.1:{remote} 上监听")),
            "{reason}"
        );
        // It does not stay logged in after giving up.
        wait_for("the SSH connection to end", || {
            server.policy.ended.load(Ordering::SeqCst) == 1
        })
        .await;
    });
}

#[test]
fn a_dynamic_forward_is_a_socks_proxy_for_both_protocol_versions() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let (echo, _echo_task) = echo_server().await;
        let fixture = Fixture::new(server.port);
        let local = free_port();
        let run = fixture.start(ForwardKind::Dynamic, local, None);
        run.listening().await;

        // SOCKS5, naming the target so the server resolves it.
        let mut socket = connect(local).await;
        socket.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut method = [0_u8; 2];
        socket.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [0x05, 0x00]);
        let mut request = vec![0x05, 0x01, 0x00, 0x03, 9];
        request.extend_from_slice(b"localhost");
        request.extend_from_slice(&echo.to_be_bytes());
        socket.write_all(&request).await.unwrap();
        let mut answer = [0_u8; 10];
        socket.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer[..2], [0x05, 0x00]);
        echoes(&mut socket, b"over socks5").await;

        // SOCKS4a, with its first bytes sent along with the request.
        let mut socket = connect(local).await;
        let mut request = vec![0x04, 0x01];
        request.extend_from_slice(&echo.to_be_bytes());
        request.extend_from_slice(&[0, 0, 0, 1, 0]);
        request.extend_from_slice(b"localhost\0");
        request.extend_from_slice(b"early");
        socket.write_all(&request).await.unwrap();
        let mut answer = [0_u8; 8];
        socket.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer[..2], [0x00, 0x5A]);
        let mut early = [0_u8; 5];
        socket.read_exact(&mut early).await.unwrap();
        assert_eq!(&early, b"early");

        // A target nothing listens on is refused in the client's protocol,
        // and is not the forward's problem.
        let nothing = free_port();
        let mut socket = connect(local).await;
        socket.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        socket.read_exact(&mut method).await.unwrap();
        let mut request = vec![0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1];
        request.extend_from_slice(&nothing.to_be_bytes());
        socket.write_all(&request).await.unwrap();
        let mut answer = [0_u8; 10];
        socket.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer[..2], [0x05, 0x05]);

        // Something that is not SOCKS at all is worth a word.
        let mut socket = connect(local).await;
        socket.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        assert!(closed(&mut socket).await);
        let ForwardEvent::ConnectionFailed(reason) = run.next_state().await else {
            panic!("expected the stray request to be reported");
        };
        assert!(reason.contains("SOCKS"), "{reason}");
    });
}

#[test]
fn an_idle_forward_notices_the_connection_dropping_and_logs_in_again() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let (echo, _echo_task) = echo_server().await;
        let fixture = Fixture::new(server.port);
        fixture.save_password();
        let local = free_port();
        let delays = [Duration::from_millis(50), Duration::from_millis(50)];
        let run = fixture.start_with(ForwardKind::Local, local, Some(echo), &delays);
        run.listening().await;

        // Nothing is being carried when the server hangs up.
        server.hang_up().await;
        assert_eq!(
            run.raw().await,
            ForwardEvent::Reconnecting {
                attempt: 1,
                of: 2,
                delay: delays[0]
            }
        );
        // Unattended: the saved password and the trusted key are enough.
        assert_eq!(run.raw().await, ForwardEvent::Listening);
        assert_eq!(server.policy.connections.load(Ordering::SeqCst), 2);

        // The same local port carries again.
        let mut socket = connect(local).await;
        echoes(&mut socket, b"back").await;
    });
}

#[test]
fn a_forward_gives_up_once_its_reconnect_attempts_run_out() {
    runtime().block_on(async {
        let Some(mut server) = server().await else {
            return;
        };
        let fixture = Fixture::new(server.port);
        fixture.save_password();
        let local = free_port();
        let delays = [Duration::from_millis(20), Duration::from_millis(20)];
        let run = fixture.start_with(ForwardKind::Local, local, Some(1), &delays);
        run.listening().await;

        server.stop_listening().await;
        server.hang_up().await;
        for attempt in 1..=2 {
            assert_eq!(
                run.raw().await,
                ForwardEvent::Reconnecting {
                    attempt,
                    of: 2,
                    delay: delays[attempt - 1]
                }
            );
        }
        assert_eq!(
            run.raw().await,
            ForwardEvent::Failed("连接中断，重连 2 次均未成功".into())
        );
        // A forward that gave up no longer holds its port.
        wait_for("the local port to be released", || {
            std::net::TcpListener::bind(("127.0.0.1", local)).is_ok()
        })
        .await;
    });
}

#[test]
fn logging_in_again_never_asks_for_a_password() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        let local = free_port();
        let delays = [Duration::from_millis(20), Duration::from_millis(20)];
        let run = fixture.start_with(ForwardKind::Local, local, Some(1), &delays);
        // The password is typed for the first login and saved nowhere.
        run.listening().await;

        server.hang_up().await;
        assert!(matches!(
            run.raw().await,
            ForwardEvent::Reconnecting { attempt: 1, .. }
        ));
        // Not a question, and not another attempt: it needs a person.
        assert_eq!(
            run.raw().await,
            ForwardEvent::Failed("连接中断，自动重连失败：需要输入密码，请手动启动".into())
        );
    });
}

#[test]
fn a_forward_without_reconnecting_fails_as_soon_as_the_connection_drops() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        let local = free_port();
        let run = fixture.start(ForwardKind::Local, local, Some(1));
        run.listening().await;
        server.hang_up().await;
        assert_eq!(
            run.raw().await,
            ForwardEvent::Failed("与服务器的连接已中断".into())
        );
    });
}

#[test]
fn stopping_while_a_question_is_open_ends_as_stopped() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        let local = free_port();
        let run = fixture.start(ForwardKind::Local, local, Some(1));
        assert_eq!(run.raw().await, ForwardEvent::Connecting);
        assert!(matches!(run.raw().await, ForwardEvent::Prompt(_)));
        run.commands.send(ForwardCommand::Stop).await.unwrap();
        assert_eq!(run.raw().await, ForwardEvent::Stopped);
    });
}

#[test]
fn a_wrong_password_fails_the_forward_with_the_login_error() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        let local = free_port();
        let run = fixture.start(ForwardKind::Local, local, Some(1));
        assert_eq!(run.raw().await, ForwardEvent::Connecting);
        loop {
            match run.raw().await {
                ForwardEvent::Prompt(prompt) => {
                    let reply = match prompt.kind() {
                        ConnectionPromptKind::UnknownHost(_) => ConnectionPromptReply::TrustAndSave,
                        _ => ConnectionPromptReply::Answers(vec![ConnectionSecret::new("wrong")]),
                    };
                    run.commands
                        .send(ForwardCommand::PromptReply {
                            request_id: prompt.request_id(),
                            reply,
                        })
                        .await
                        .unwrap();
                }
                ForwardEvent::Failed(reason) => {
                    assert!(!reason.is_empty());
                    break;
                }
                event => panic!("expected the login to fail, got {event:?}"),
            }
        }
        // The port it bound before logging in is free again.
        wait_for("the local port to be released", || {
            std::net::TcpListener::bind(("127.0.0.1", local)).is_ok()
        })
        .await;
    });
}

#[test]
fn a_connection_that_forwards_nothing_refuses_channels_from_the_server() {
    runtime().block_on(async {
        let Some(server) = server().await else { return };
        let fixture = Fixture::new(server.port);
        fixture.save_password();
        // Trust the host first, through a forward that asks.
        let run = fixture.start(ForwardKind::Local, free_port(), Some(1));
        run.listening().await;
        run.stop().await;
        drop(run);

        // A terminal's or an SFTP tab's kind of connection.
        let (_shutdown, shutdown) = watch::channel(false);
        let prompts = Arc::new(SshPrompts::new(Arc::new(|_| false), shutdown).non_interactive());
        let config = SshConnectionConfig::from(&HostLogin::of(&fixture.host(), None));
        let (handle, _) = fixture.connector.connect(&config, prompts).await.unwrap();
        let remote = free_port();
        handle
            .tcpip_forward("127.0.0.1", remote.into())
            .await
            .unwrap();

        // The server opens a channel for whoever connects; nobody here
        // asked for one, so it is refused rather than left dangling.
        let mut socket = connect(remote).await;
        assert!(closed(&mut socket).await);
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
    });
}
