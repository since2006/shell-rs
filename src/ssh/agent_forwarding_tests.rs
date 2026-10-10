//! Real loopback SSH connections and isolated Unix agents; no user services.
use super::*;
use crate::host::{AuthKind, JumpLogin, LoginRoute};
use crate::secrets::NoSecretStore;
use crate::ssh::AgentLocation;
use russh::{Channel, ChannelId, server};
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::time::timeout;

#[derive(Clone, Copy)]
enum Answer {
    Accept,
    Refuse,
    Ignore,
}

struct Server {
    answer: Answer,
    requests: Arc<Mutex<Vec<&'static str>>>,
}

impl server::Handler for Server {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, _: &str) -> Result<server::Auth> {
        Ok(server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        reply.accept().await;
        Ok(())
    }
    async fn agent_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<bool> {
        self.requests.lock().unwrap().push("agent");
        match self.answer {
            Answer::Accept => session.channel_success(channel)?,
            Answer::Refuse => session.channel_failure(channel)?,
            Answer::Ignore => {}
        }
        Ok(matches!(self.answer, Answer::Accept))
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        session: &mut server::Session,
    ) -> Result<()> {
        self.requests.lock().unwrap().push("pty");
        session.channel_success(channel)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<()> {
        self.requests.lock().unwrap().push("shell");
        session.channel_success(channel)?;
        session.data(channel, b"ready".to_vec())?;
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: server::ChannelOpenHandle,
        session: &mut server::Session,
    ) -> Result<()> {
        // Even a jump host with the saved setting enabled must be refused.
        let handle = session.handle();
        let requests = self.requests.clone();
        tokio::spawn(async move {
            assert!(handle.channel_open_agent().await.is_err());
            requests.lock().unwrap().push("agent-rejected");
        });
        let mut target = TcpStream::connect((host, port as u16)).await?;
        reply.accept().await;
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut target).await;
        });
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    connector: SshConnector,
    login: HostLogin,
    server: oneshot::Receiver<server::Handle>,
    requests: Arc<Mutex<Vec<&'static str>>>,
    agent: UnixListener,
}

impl Fixture {
    async fn new(enabled: bool, answer: Answer) -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("agent");
        let agent = UnixListener::bind(&socket).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let key = russh::keys::PrivateKey::random(
            &mut russh::keys::key::safe_rng(),
            russh::keys::Algorithm::Ed25519,
        )
        .unwrap();
        let known = dir.path().join("known_hosts");
        russh::keys::known_hosts::learn_known_hosts_path(
            "127.0.0.1",
            port,
            key.public_key(),
            &known,
        )
        .unwrap();
        let config = Arc::new(server::Config {
            keys: vec![key],
            ..Default::default()
        });
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler = Server {
            answer,
            requests: requests.clone(),
        };
        let (send, server) = oneshot::channel();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let running = server::run_stream(config, stream, handler).await.unwrap();
            let _ = send.send(running.handle());
            let _ = running.await;
        });
        let connector =
            SshConnector::new(known, Arc::new(NoSecretStore)).with_agent(AgentLocation::At(socket));
        let mut login = HostLogin::manual("127.0.0.1", port, "test", AuthKind::NoPassword);
        login.agent_forwarding = enabled;
        login.shell_only = true;
        Self {
            _dir: dir,
            connector,
            login,
            server,
            requests,
            agent,
        }
    }

    fn start(
        &self,
    ) -> (
        tokio_mpsc::UnboundedSender<TerminalTransportCommand>,
        async_channel::Receiver<TerminalTransportEvent>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let transport = SshTerminalTransport {
            config: (&self.login).into(),
            connector: self.connector.clone(),
        };
        let (send, receive) = tokio_mpsc::unbounded_channel();
        let (events, output) = async_channel::unbounded();
        let task = tokio::spawn(async move {
            transport
                .run_async(TerminalSize::DEFAULT, receive, events)
                .await
        });
        (send, output, task)
    }
}

async fn started(events: &async_channel::Receiver<TerminalTransportEvent>) {
    timeout(Duration::from_secs(3), async {
        let mut started = false;
        loop {
            match events.recv().await.unwrap() {
                TerminalTransportEvent::Started => started = true,
                TerminalTransportEvent::Output(bytes) if bytes == b"ready" => {
                    assert!(started);
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

async fn agent_exchange(
    server: &server::Handle,
    listener: &UnixListener,
    first: Option<UnixStream>,
) -> (russh::ChannelStream<server::Msg>, UnixStream) {
    let channel = server.channel_open_agent().await.unwrap();
    let mut local = match first {
        Some(stream) => stream,
        None => listener.accept().await.unwrap().0,
    };
    let mut remote = channel.into_stream();
    // A real SSH_AGENTC_REQUEST_IDENTITIES frame, fragmented on purpose.
    remote.write_all(&[0, 0]).await.unwrap();
    remote.write_all(&[0, 1, 11]).await.unwrap();
    let mut request = [0; 5];
    local.read_exact(&mut request).await.unwrap();
    assert_eq!(request, [0, 0, 0, 1, 11]);
    let response = [0, 0, 0, 5, 12, 0, 0, 0, 0];
    local.write_all(&response).await.unwrap();
    let mut received = [0; 9];
    remote.read_exact(&mut received).await.unwrap();
    assert_eq!(received, response);
    (remote, local)
}

#[tokio::test]
async fn forwarding_bridges_independent_channels_and_closes_them_with_terminal() {
    timeout(Duration::from_secs(10), async {
        let fixture = Fixture::new(true, Answer::Accept).await;
        let (send, events, task) = fixture.start();
        started(&events).await;
        assert_eq!(*fixture.requests.lock().unwrap(), ["agent", "pty", "shell"]);
        let server = fixture.server.await.unwrap();
        let first = fixture.agent.accept().await.unwrap().0;
        let (mut remote1, mut local1) = agent_exchange(&server, &fixture.agent, Some(first)).await;
        let (mut remote2, mut local2) = agent_exchange(&server, &fixture.agent, None).await;
        remote1.write_all(b"one").await.unwrap();
        remote2.write_all(b"two").await.unwrap();
        let mut data = [0; 3];
        local2.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"two");
        local1.read_exact(&mut data).await.unwrap();
        assert_eq!(&data, b"one");
        send.send(TerminalTransportCommand::Shutdown).unwrap();
        task.await.unwrap().unwrap();
        assert_eq!(local1.read(&mut data).await.unwrap(), 0);
        assert_eq!(local2.read(&mut data).await.unwrap(), 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn forwarding_disabled_rejects_unsolicited_channels_without_touching_agent() {
    let fixture = Fixture::new(false, Answer::Accept).await;
    let (send, events, task) = fixture.start();
    started(&events).await;
    assert_eq!(*fixture.requests.lock().unwrap(), ["pty", "shell"]);
    let server = fixture.server.await.unwrap();
    assert!(
        timeout(Duration::from_secs(2), server.channel_open_agent())
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(30), fixture.agent.accept())
            .await
            .is_err()
    );
    send.send(TerminalTransportCommand::Shutdown).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn forwarding_refusal_and_timeout_do_not_start_shell() {
    for answer in [Answer::Refuse, Answer::Ignore] {
        let fixture = Fixture::new(true, answer).await;
        let (_send, _events, task) = fixture.start();
        let mut local = fixture.agent.accept().await.unwrap().0;
        let result = timeout(Duration::from_secs(8), task)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(*fixture.requests.lock().unwrap(), ["agent"]);
        let mut byte = [0];
        assert_eq!(
            timeout(Duration::from_secs(1), local.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn shared_connector_never_grants_terminal_forwarding_permission() {
    let fixture = Fixture::new(true, Answer::Accept).await;
    let (_send, shutdown) = watch::channel(false);
    let prompts = Arc::new(SshPrompts::new(Arc::new(|_| false), shutdown));
    // SFTP, exec and connection testing all use this shared path.
    let (_handle, _) = fixture
        .connector
        .connect(&(&fixture.login).into(), prompts)
        .await
        .unwrap();
    let server = fixture.server.await.unwrap();
    assert!(
        timeout(Duration::from_secs(2), server.channel_open_agent())
            .await
            .unwrap()
            .is_err()
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
    assert!(
        timeout(Duration::from_millis(30), fixture.agent.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn forwarding_is_only_granted_to_the_final_host_through_a_jump() {
    let jump = Fixture::new(true, Answer::Accept).await;
    let mut target = Fixture::new(true, Answer::Accept).await;
    let jump_keys = std::fs::read(jump._dir.path().join("known_hosts")).unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(target._dir.path().join("known_hosts"))
        .unwrap()
        .write_all(&jump_keys)
        .unwrap();
    target.login.route = LoginRoute::Jump(vec![JumpLogin::Host {
        name: "jump".into(),
        login: Box::new(jump.login.clone()),
    }]);
    let (send, events, task) = target.start();
    started(&events).await;
    let server = target.server.await.unwrap();
    let first = target.agent.accept().await.unwrap().0;
    timeout(
        Duration::from_secs(2),
        agent_exchange(&server, &target.agent, Some(first)),
    )
    .await
    .unwrap();
    assert_eq!(*jump.requests.lock().unwrap(), ["agent-rejected"]);
    assert!(
        timeout(Duration::from_millis(30), jump.agent.accept())
            .await
            .is_err()
    );
    send.send(TerminalTransportCommand::Shutdown).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn forwarding_cancels_while_waiting_for_server_and_closes_local_socket() {
    let fixture = Fixture::new(true, Answer::Ignore).await;
    let (send, _events, task) = fixture.start();
    let mut local = fixture.agent.accept().await.unwrap().0;
    timeout(Duration::from_secs(2), async {
        while fixture.requests.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    send.send(TerminalTransportCommand::Shutdown).unwrap();
    assert!(
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(1), local.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(*fixture.requests.lock().unwrap(), ["agent"]);
}

#[tokio::test]
async fn forwarding_limits_channels_and_releases_capacity_after_eof() {
    timeout(Duration::from_secs(10), async {
        let fixture = Fixture::new(true, Answer::Accept).await;
        let (send, events, task) = fixture.start();
        started(&events).await;
        let server = fixture.server.await.unwrap();
        let mut remotes = Vec::new();
        let mut locals = Vec::new();
        for _ in 0..16 {
            remotes.push(server.channel_open_agent().await.unwrap().into_stream());
            locals.push(fixture.agent.accept().await.unwrap().0);
        }
        assert!(server.channel_open_agent().await.is_err());
        let mut remote = remotes.pop().unwrap();
        let mut local = locals.pop().unwrap();
        remote.shutdown().await.unwrap();
        local.shutdown().await.unwrap();
        let mut byte = [0];
        assert_eq!(remote.read(&mut byte).await.unwrap(), 0);
        assert_eq!(local.read(&mut byte).await.unwrap(), 0);
        // Completion and permit release happen on the worker's next poll.
        loop {
            match server.channel_open_agent().await {
                Ok(channel) => {
                    remotes.push(channel.into_stream());
                    break;
                }
                Err(_) => tokio::task::yield_now().await,
            }
        }
        locals.push(fixture.agent.accept().await.unwrap().0);
        send.send(TerminalTransportCommand::Shutdown).unwrap();
        task.await.unwrap().unwrap();
        for mut local in locals {
            assert_eq!(local.read(&mut byte).await.unwrap(), 0);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn forwarding_missing_agent_fails_before_starting_shell() {
    let mut fixture = Fixture::new(true, Answer::Accept).await;
    fixture.connector = fixture
        .connector
        .clone()
        .with_agent(AgentLocation::At(fixture._dir.path().join("missing")));
    let (_send, _events, task) = fixture.start();
    assert!(
        timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn forwarding_reconnect_failure_rejects_only_the_new_channel() {
    timeout(Duration::from_secs(5), async {
        let fixture = Fixture::new(true, Answer::Accept).await;
        let (send, events, task) = fixture.start();
        started(&events).await;
        let server = fixture.server.await.unwrap();
        let first = fixture.agent.accept().await.unwrap().0;
        let (mut remote, mut local) = agent_exchange(&server, &fixture.agent, Some(first)).await;
        drop(fixture.agent);
        assert!(server.channel_open_agent().await.is_err());
        remote.write_all(b"still alive").await.unwrap();
        let mut bytes = [0; 11];
        local.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"still alive");
        send.send(TerminalTransportCommand::Shutdown).unwrap();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn forwarding_permission_is_not_active_until_terminal_requests_it() {
    let fixture = Fixture::new(true, Answer::Accept).await;
    let (_send, shutdown) = watch::channel(false);
    let prompts = Arc::new(SshPrompts::new(Arc::new(|_| false), shutdown));
    let (_handle, forwarding) = fixture
        .connector
        .connect_terminal(&(&fixture.login).into(), prompts)
        .await
        .unwrap();
    let server = fixture.server.await.unwrap();
    assert!(
        timeout(Duration::from_secs(2), server.channel_open_agent())
            .await
            .unwrap()
            .is_err()
    );
    let mut local = fixture.agent.accept().await.unwrap().0;
    drop(forwarding);
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(1), local.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn forwarding_closes_local_stream_when_remote_connection_ends() {
    timeout(Duration::from_secs(5), async {
        let fixture = Fixture::new(true, Answer::Accept).await;
        let (_send, events, task) = fixture.start();
        started(&events).await;
        let server = fixture.server.await.unwrap();
        let first = fixture.agent.accept().await.unwrap().0;
        let (_remote, mut local) = agent_exchange(&server, &fixture.agent, Some(first)).await;
        server
            .disconnect(russh::Disconnect::ByApplication, "done".into(), "en".into())
            .await
            .unwrap();
        task.await.unwrap().unwrap();
        let mut byte = [0];
        assert_eq!(local.read(&mut byte).await.unwrap(), 0);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn forwarding_uses_the_host_agent_selection_without_connector_override() {
    timeout(Duration::from_secs(10), async {
        let mut fixture = Fixture::new(true, Answer::Accept).await;
        fixture.connector = SshConnector::new(
            fixture._dir.path().join("known_hosts"),
            Arc::new(NoSecretStore),
        );
        fixture.login.ssh_agent =
            crate::ssh_agent::AgentSelection::Path(fixture._dir.path().join("agent"));
        let (send, events, task) = fixture.start();
        started(&events).await;
        let server = fixture.server.await.unwrap();
        let first = fixture.agent.accept().await.unwrap().0;
        let _ = agent_exchange(&server, &fixture.agent, Some(first)).await;
        let _ = agent_exchange(&server, &fixture.agent, None).await;
        send.send(TerminalTransportCommand::Shutdown).unwrap();
        task.await.unwrap().unwrap();
    })
    .await
    .unwrap();
}
