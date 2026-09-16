use super::{
    connection::{SshConnectionConfig, SshConnector, SshPrompts},
    probe::{HostOsProbe, ProbeOutcome},
};
use crate::{
    secrets::SharedSecretStore,
    session::Session,
    terminal::{
        RemoteTerminalTransportProvider, SharedTerminalTransportFactory, TerminalSize,
        TerminalTransport, TerminalTransportCommand, TerminalTransportEvent,
        TerminalTransportFactory,
    },
};
use anyhow::{Context as _, Result, anyhow, bail};
use async_channel::Sender;
use russh::{ChannelMsg, client};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use tokio::sync::{mpsc as tokio_mpsc, watch};
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Production remote-terminal adapter for the shared SSH connector.
pub struct SshTerminalTransportProvider {
    connector: SshConnector,
}
impl SshTerminalTransportProvider {
    pub fn new(path: impl Into<PathBuf>, secrets: SharedSecretStore) -> Self {
        Self::with_connector(SshConnector::new(path, secrets))
    }
    pub fn with_connector(connector: SshConnector) -> Self {
        Self { connector }
    }
}
impl RemoteTerminalTransportProvider for SshTerminalTransportProvider {
    fn factory_for(&self, session: &Session) -> SharedTerminalTransportFactory {
        Arc::new(SshTerminalTransportFactory {
            config: SshConnectionConfig::from(session),
            connector: self.connector.clone(),
        })
    }
}
struct SshTerminalTransportFactory {
    config: SshConnectionConfig,
    connector: SshConnector,
}
impl TerminalTransportFactory for SshTerminalTransportFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(SshTerminalTransport {
            config: self.config.clone(),
            connector: self.connector.clone(),
        })
    }
}
struct SshTerminalTransport {
    config: SshConnectionConfig,
    connector: SshConnector,
}

impl TerminalTransport for SshTerminalTransport {
    fn run(
        self: Box<Self>,
        initial_size: TerminalSize,
        commands: mpsc::Receiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("无法启动 SSH 运行时")?;
        let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let bridge_stop = stop.clone();
        let bridge = thread::Builder::new()
            .name("shellr-ssh-command-bridge".into())
            .spawn(move || {
                while !bridge_stop.load(Ordering::Acquire) {
                    match commands.recv_timeout(Duration::from_millis(20)) {
                        Ok(command) => {
                            if command_tx.send(command).is_err() {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .context("无法启动 SSH 命令桥接线程")?;

        let result = runtime.block_on(self.run_async(initial_size, command_rx, events));
        stop.store(true, Ordering::Release);
        let _ = bridge.join();
        result
    }
}

impl SshTerminalTransport {
    async fn run_async(
        &self,
        initial_size: TerminalSize,
        mut bridge_commands: tokio_mpsc::UnboundedReceiver<TerminalTransportCommand>,
        events: Sender<TerminalTransportEvent>,
    ) -> Result<()> {
        let (io_tx, mut io_rx) = tokio_mpsc::unbounded_channel();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let prompt_events = events.clone();
        let broker = Arc::new(SshPrompts::new(
            Arc::new(move |prompt| {
                prompt_events
                    .try_send(TerminalTransportEvent::Prompt(prompt))
                    .is_ok()
            }),
            shutdown_rx,
        ));
        let router_broker = broker.clone();
        let router = tokio::spawn(async move {
            while let Some(command) = bridge_commands.recv().await {
                match command {
                    TerminalTransportCommand::PromptReply { request_id, reply } => {
                        router_broker.respond(request_id, reply);
                    }
                    TerminalTransportCommand::Shutdown => {
                        let _ = shutdown_tx.send(true);
                        router_broker.cancel_all();
                        let _ = io_tx.send(TerminalTransportCommand::Shutdown);
                        break;
                    }
                    command => {
                        if io_tx.send(command).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let (handle, _) = self.connector.connect(&self.config, broker.clone()).await?;
        let mut shutdown = broker.shutdown_receiver();
        let mut channel = tokio::select! {
            result = handle.channel_open_session() => {
                result.map_err(|_| anyhow!("无法创建 SSH 会话通道"))?
            }
            _ = shutdown.changed() => bail!("连接已取消"),
        };
        tokio::select! {
            result = channel.request_pty(
                true,
                "xterm-256color",
                dimension(initial_size.columns()),
                dimension(initial_size.rows()),
                pixel_dimension(initial_size.columns(), initial_size.cell_width()),
                pixel_dimension(initial_size.rows(), initial_size.cell_height()),
                &[],
            ) => result.map_err(|_| anyhow!("服务器拒绝创建终端"))?,
            _ = shutdown.changed() => bail!("连接已取消"),
        }
        tokio::select! {
            result = channel.request_shell(true) => {
                result.map_err(|_| anyhow!("服务器拒绝启动 Shell"))?
            }
            _ = shutdown.changed() => bail!("连接已取消"),
        }
        events
            .send(TerminalTransportEvent::Started)
            .await
            .map_err(|_| anyhow!("终端标签页已关闭"))?;

        // Ask what the host is running on a channel of its own, then let the
        // loop below collect the answer alongside the shell's output. Opening
        // it after `Started` keeps the terminal from waiting on a round trip,
        // and a host that refuses or ignores the probe simply keeps whatever
        // mark the session already had.
        let mut probe = HostOsProbe::new();
        let mut probe_channel = open_probe(&handle, probe.command()).await;

        let mut exit_code = 0;
        let mut exit_signal = None;
        loop {
            tokio::select! {
                command = io_rx.recv() => match command {
                    Some(TerminalTransportCommand::Write(bytes)) => {
                        channel.data_bytes(bytes).await.map_err(|_| anyhow!("向远程终端写入失败"))?;
                    }
                    Some(TerminalTransportCommand::Resize(size)) => {
                        channel.window_change(
                            dimension(size.columns()), dimension(size.rows()),
                            pixel_dimension(size.columns(), size.cell_width()),
                            pixel_dimension(size.rows(), size.cell_height()),
                        ).await.map_err(|_| anyhow!("调整远程终端尺寸失败"))?;
                    }
                    Some(TerminalTransportCommand::Shutdown) | None => {
                        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
                            let _ = channel.eof().await;
                            let _ = channel.close().await;
                            let _ = handle.disconnect(
                                russh::Disconnect::ByApplication,
                                "shellr closed the terminal",
                                "zh-CN",
                            ).await;
                        }).await;
                        router.abort();
                        return Ok(());
                    }
                    Some(TerminalTransportCommand::PromptReply { .. }) => {}
                },
                // Only armed while a probe channel is open, so a host that
                // never answers costs one idle channel and nothing else.
                message = async { probe_channel.as_mut().expect("guarded").wait().await },
                    if probe_channel.is_some() =>
                {
                    match message {
                        // stderr is skipped: on Windows it only holds the
                        // shell complaining that `uname` does not exist.
                        Some(ChannelMsg::Data { data }) => probe.push(&data),
                        Some(ChannelMsg::Eof | ChannelMsg::Close) | None => {
                            probe_channel = None;
                            match probe.finish() {
                                ProbeOutcome::Detected(os) => {
                                    let _ = events
                                        .send(TerminalTransportEvent::HostOsDetected(os))
                                        .await;
                                }
                                ProbeOutcome::AskWindows => {
                                    probe_channel = open_probe(&handle, probe.command()).await;
                                }
                                ProbeOutcome::GaveUp => {}
                            }
                        }
                        _ => {}
                    }
                },
                message = channel.wait() => match message {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if events.send(TerminalTransportEvent::Output(data.to_vec())).await.is_err() {
                            break;
                        }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => exit_code = exit_status,
                    Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                        exit_signal = Some(format!("{signal_name:?}"));
                    }
                    Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
        }
        let _ = events
            .send(TerminalTransportEvent::Exited {
                code: exit_code,
                signal: exit_signal,
            })
            .await;
        router.abort();
        Ok(())
    }
}

fn dimension(value: usize) -> u32 {
    value.min(u32::MAX as usize) as u32
}

fn pixel_dimension(cells: usize, cell_size: u16) -> u32 {
    cells
        .saturating_mul(usize::from(cell_size))
        .min(u32::MAX as usize) as u32
}

/// Open a channel and run one probe command on it. Best effort throughout: a
/// server that refuses the channel or the command just leaves the session's
/// recorded operating system as it was.
async fn open_probe<H: client::Handler>(
    handle: &client::Handle<H>,
    command: Option<&'static str>,
) -> Option<russh::Channel<client::Msg>> {
    let command = command?;
    let channel = handle.channel_open_session().await.ok()?;
    // No reply wanted: the answer is the output, and waiting for the
    // acknowledgement would cost another round trip for nothing.
    channel.exec(false, command).await.ok()?;
    Some(channel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{InMemorySecretStore, NoSecretStore, SecretStore as _};
    use crate::session::HostOs;
    use crate::ssh::probe::{PROBE_COMMAND, WINDOWS_PROBE_COMMAND};
    use crate::terminal::TerminalSecret;
    use crate::{
        session::AuthKind,
        terminal::{TerminalPromptKind, TerminalPromptReply},
    };
    use russh::keys::{PublicKey, known_hosts::learn_known_hosts_path};
    use russh::server::{self, Server as _};
    use russh::{MethodKind, MethodSet};
    use std::borrow::Cow;
    use std::{path::Path, sync::Mutex};

    const TEST_PASSWORD: &str = "test-password";

    #[derive(Default)]
    struct ServerState {
        pty: Option<(String, u32, u32, u32, u32)>,
        resize: Option<(u32, u32, u32, u32)>,
        input: Vec<u8>,
        /// Every command run through an exec channel, in order.
        execs: Vec<String>,
    }

    #[derive(Clone)]
    struct TestServer {
        state: Arc<Mutex<ServerState>>,
        auth: TestAuth,
        /// What an exec channel writes back for a given command.
        probe_reply: fn(&str) -> Option<&'static str>,
    }

    #[derive(Clone)]
    enum TestAuth {
        Password,
        PublicKey(PublicKey),
        KeyboardInteractive,
    }

    impl TestAuth {
        fn methods(&self) -> MethodSet {
            let method = match self {
                Self::Password => MethodKind::Password,
                Self::PublicKey(_) => MethodKind::PublicKey,
                Self::KeyboardInteractive => MethodKind::KeyboardInteractive,
            };
            MethodSet::from(&[method][..])
        }
    }

    impl server::Server for TestServer {
        type Handler = Self;

        fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self::Handler {
            self.clone()
        }
    }

    impl server::Handler for TestServer {
        type Error = russh::Error;

        async fn auth_password(
            &mut self,
            _: &str,
            password: &str,
        ) -> Result<server::Auth, Self::Error> {
            if !matches!(self.auth, TestAuth::Password) {
                return Ok(server::Auth::UnsupportedMethod);
            }
            Ok(if password == TEST_PASSWORD {
                server::Auth::Accept
            } else {
                // Like OpenSSH: a wrong password is refused but password auth
                // stays on the table, so the client can ask again. russh's
                // default rejection would drop the method instead.
                server::Auth::Reject {
                    proceed_with_methods: Some(MethodSet::from(&[MethodKind::Password][..])),
                    partial_success: false,
                }
            })
        }

        async fn auth_publickey(
            &mut self,
            _: &str,
            public_key: &PublicKey,
        ) -> Result<server::Auth, Self::Error> {
            Ok(match &self.auth {
                TestAuth::PublicKey(expected) if expected == public_key => server::Auth::Accept,
                TestAuth::PublicKey(_) => server::Auth::reject(),
                _ => server::Auth::UnsupportedMethod,
            })
        }

        async fn auth_keyboard_interactive<'a>(
            &'a mut self,
            _: &str,
            _: &str,
            response: Option<server::Response<'a>>,
        ) -> Result<server::Auth, Self::Error> {
            if !matches!(self.auth, TestAuth::KeyboardInteractive) {
                return Ok(server::Auth::UnsupportedMethod);
            }
            let Some(mut response) = response else {
                return Ok(server::Auth::Partial {
                    name: Cow::Borrowed("双字段验证"),
                    instructions: Cow::Borrowed("输入别名和一次性验证码"),
                    prompts: Cow::Owned(vec![
                        (Cow::Borrowed("别名"), true),
                        (Cow::Borrowed("验证码"), false),
                    ]),
                });
            };
            let alias_ok = response
                .next()
                .is_some_and(|answer| answer.as_ref() == b"tester");
            let otp_ok = response
                .next()
                .is_some_and(|answer| answer.as_ref() == b"123456");
            Ok(if alias_ok && otp_ok {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            })
        }

        async fn channel_open_session(
            &mut self,
            _: russh::Channel<server::Msg>,
            reply: server::ChannelOpenHandle,
            _: &mut server::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn pty_request(
            &mut self,
            channel: russh::ChannelId,
            term: &str,
            columns: u32,
            rows: u32,
            pixel_width: u32,
            pixel_height: u32,
            _: &[(russh::Pty, u32)],
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pty = Some((term.to_string(), columns, rows, pixel_width, pixel_height));
            session.channel_success(channel)?;
            Ok(())
        }

        async fn shell_request(
            &mut self,
            channel: russh::ChannelId,
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            session.channel_success(channel)?;
            session.data(channel, &b"ready\r\n"[..])?;
            Ok(())
        }

        /// Answers the host-operating-system probe. `probe_reply` of `None`
        /// stands for a host where the command produces nothing, which is
        /// what a Windows shell does with `uname`.
        async fn exec_request(
            &mut self,
            channel: russh::ChannelId,
            command: &[u8],
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .execs
                .push(String::from_utf8_lossy(command).into_owned());
            session.channel_success(channel)?;
            if let Some(reply) = (self.probe_reply)(&String::from_utf8_lossy(command)) {
                session.data(channel, reply.as_bytes().to_vec())?;
            }
            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }

        async fn window_change_request(
            &mut self,
            _: russh::ChannelId,
            columns: u32,
            rows: u32,
            pixel_width: u32,
            pixel_height: u32,
            _: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .resize = Some((columns, rows, pixel_width, pixel_height));
            Ok(())
        }

        async fn data(
            &mut self,
            channel: russh::ChannelId,
            data: &[u8],
            session: &mut server::Session,
        ) -> Result<(), Self::Error> {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .input
                .extend_from_slice(data);
            if data == b"exit\n" {
                session.exit_status_request(channel, 7)?;
                session.eof(channel)?;
                session.close(channel)?;
            } else {
                session.data(channel, data.to_vec())?;
            }
            Ok(())
        }
    }

    struct RunningTestServer {
        port: u16,
        state: Arc<Mutex<ServerState>>,
        handle: server::RunningServerHandle,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl Drop for RunningTestServer {
        fn drop(&mut self) {
            self.handle.shutdown("test complete".into());
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    fn no_probe_reply(_: &str) -> Option<&'static str> {
        None
    }

    /// A host that answers the POSIX probe like an Alpine box.
    fn alpine_probe_reply(command: &str) -> Option<&'static str> {
        (command == PROBE_COMMAND).then_some("Linux\nID=alpine\nID_LIKE=\n")
    }

    /// A host whose shell knows nothing about `uname`, then answers `ver`.
    fn windows_probe_reply(command: &str) -> Option<&'static str> {
        (command == WINDOWS_PROBE_COMMAND)
            .then_some("\r\nMicrosoft Windows [Version 10.0.19045.4291]\r\n")
    }

    fn start_server(auth: TestAuth) -> Option<RunningTestServer> {
        start_server_replying(auth, no_probe_reply)
    }

    fn start_server_replying(
        auth: TestAuth,
        probe_reply: fn(&str) -> Option<&'static str>,
    ) -> Option<RunningTestServer> {
        let state = Arc::new(Mutex::new(ServerState::default()));
        let server_state = state.clone();
        let methods = auth.methods();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        ready_tx.send(Err(error)).unwrap();
                        return;
                    }
                };
                let port = listener.local_addr().unwrap().port();
                let mut rng = russh::keys::key::safe_rng();
                let key =
                    russh::keys::PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519)
                        .unwrap();
                let config = Arc::new(server::Config {
                    methods,
                    auth_rejection_time: Duration::ZERO,
                    auth_rejection_time_initial: Some(Duration::ZERO),
                    keys: vec![key],
                    ..server::Config::default()
                });
                let mut server = TestServer {
                    state: server_state,
                    auth,
                    probe_reply,
                };
                let running = server.run_on_socket(config, &listener);
                ready_tx.send(Ok((port, running.handle()))).unwrap();
                running.await.unwrap();
            });
        });
        let (port, handle) = match ready_rx.recv_timeout(Duration::from_secs(3)).unwrap() {
            Ok(server) => server,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                thread.join().unwrap();
                return None;
            }
            Err(error) => panic!("failed to bind test server: {error}"),
        };
        Some(RunningTestServer {
            port,
            state,
            handle,
            thread: Some(thread),
        })
    }

    fn connect_then_shutdown(
        session: Session,
        known_hosts: &Path,
        answer: impl FnMut(&TerminalPromptKind) -> TerminalPromptReply,
    ) {
        connect(session, known_hosts, Arc::new(NoSecretStore), false, answer);
    }

    fn connect_with_secrets(
        session: Session,
        known_hosts: &Path,
        secrets: SharedSecretStore,
        answer: impl FnMut(&TerminalPromptKind) -> TerminalPromptReply,
    ) -> ConnectionReport {
        connect(session, known_hosts, secrets, false, answer)
    }

    /// Same, but waits for the host-operating-system probe to report before
    /// shutting the connection down.
    fn connect_and_probe(
        session: Session,
        known_hosts: &Path,
        answer: impl FnMut(&TerminalPromptKind) -> TerminalPromptReply,
    ) -> ConnectionReport {
        connect(session, known_hosts, Arc::new(NoSecretStore), true, answer)
    }

    /// What one connection told the UI about itself.
    #[derive(Default)]
    struct ConnectionReport {
        prompts: Vec<TerminalPromptKind>,
        host_os: Option<HostOs>,
    }

    /// Connect, answer whatever is asked, then shut down. The report says what
    /// was raised along the way, so a test can assert that nothing was.
    fn connect(
        session: Session,
        known_hosts: &Path,
        secrets: SharedSecretStore,
        wait_for_host_os: bool,
        mut answer: impl FnMut(&TerminalPromptKind) -> TerminalPromptReply,
    ) -> ConnectionReport {
        let mut report = ConnectionReport::default();
        let provider = SshTerminalTransportProvider::new(known_hosts, secrets);
        let factory = provider.factory_for(&session);
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = async_channel::unbounded();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            done_tx
                .send(
                    factory
                        .create()
                        .run(TerminalSize::DEFAULT, command_rx, event_tx),
                )
                .unwrap();
        });
        loop {
            match event_rx.recv_blocking().unwrap() {
                TerminalTransportEvent::Prompt(prompt) => {
                    report.prompts.push(prompt.kind().clone());
                    command_tx
                        .send(TerminalTransportCommand::PromptReply {
                            request_id: prompt.request_id(),
                            reply: answer(prompt.kind()),
                        })
                        .unwrap()
                }
                TerminalTransportEvent::Started => break,
                TerminalTransportEvent::Failed(error) => panic!("SSH failed: {error}"),
                TerminalTransportEvent::Exited { .. } => panic!("unexpected early exit"),
                TerminalTransportEvent::HostOsDetected(_) | TerminalTransportEvent::Output(_) => {}
            }
        }
        // The probe opens its channel after the shell is up, so its answer
        // lands after `Started`. Only the tests that care pay the wait.
        if wait_for_host_os {
            // Loopback answers in single-digit milliseconds; this only runs
            // out for a host that never answers at all.
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while report.host_os.is_none() && std::time::Instant::now() < deadline {
                match event_rx.try_recv() {
                    Ok(TerminalTransportEvent::HostOsDetected(os)) => report.host_os = Some(os),
                    Ok(_) => {}
                    Err(async_channel::TryRecvError::Empty) => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(async_channel::TryRecvError::Closed) => break,
                }
            }
        }
        command_tx.send(TerminalTransportCommand::Shutdown).unwrap();
        done_rx
            .recv_timeout(SHUTDOWN_TIMEOUT + Duration::from_secs(1))
            .expect("SSH worker did not stop in time")
            .unwrap();
        worker.join().unwrap();
        report
    }

    #[test]
    fn real_russh_server_covers_trust_password_pty_io_resize_and_exit() {
        let Some(server) = start_server(TestAuth::Password) else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");
        let session = Session::new(
            crate::session::SessionId(1),
            crate::session::SessionDraft::new(
                "test",
                "127.0.0.1",
                server.port,
                "tester",
                AuthKind::Password,
                None,
            ),
        );
        let provider = SshTerminalTransportProvider::new(&known_hosts, Arc::new(NoSecretStore));
        let factory = provider.factory_for(&session);
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = async_channel::unbounded();
        let worker = thread::spawn(move || {
            factory
                .create()
                .run(TerminalSize::new(90, 30, 9, 18), command_rx, event_tx)
                .unwrap();
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut started = false;
        let mut ready = false;
        while std::time::Instant::now() < deadline && !(started && ready) {
            match event_rx.recv_blocking().unwrap() {
                TerminalTransportEvent::Prompt(prompt) => match prompt.kind() {
                    TerminalPromptKind::UnknownHost(_) => command_tx
                        .send(TerminalTransportCommand::PromptReply {
                            request_id: prompt.request_id(),
                            reply: TerminalPromptReply::TrustAndSave,
                        })
                        .unwrap(),
                    TerminalPromptKind::Authentication(_) => command_tx
                        .send(TerminalTransportCommand::PromptReply {
                            request_id: prompt.request_id(),
                            reply: TerminalPromptReply::Answers(vec![TerminalSecret::new(
                                TEST_PASSWORD,
                            )]),
                        })
                        .unwrap(),
                    TerminalPromptKind::HostKeyChanged(_) => panic!("unexpected changed key"),
                },
                TerminalTransportEvent::Started => started = true,
                TerminalTransportEvent::HostOsDetected(_) => {}
                TerminalTransportEvent::Output(bytes) => {
                    ready |= String::from_utf8_lossy(&bytes).contains("ready")
                }
                TerminalTransportEvent::Failed(error) => panic!("SSH failed: {error}"),
                TerminalTransportEvent::Exited { .. } => panic!("unexpected early exit"),
            }
        }
        assert!(started && ready);
        assert!(known_hosts.exists());

        command_tx
            .send(TerminalTransportCommand::Resize(TerminalSize::new(
                120, 40, 10, 20,
            )))
            .unwrap();
        command_tx
            .send(TerminalTransportCommand::Write(b"hello\n".to_vec()))
            .unwrap();
        let echoed = loop {
            if let TerminalTransportEvent::Output(bytes) = event_rx.recv_blocking().unwrap() {
                break bytes;
            }
        };
        assert_eq!(echoed, b"hello\n");
        command_tx
            .send(TerminalTransportCommand::Write(b"exit\n".to_vec()))
            .unwrap();
        let exit = loop {
            if let TerminalTransportEvent::Exited { code, signal } =
                event_rx.recv_blocking().unwrap()
            {
                break (code, signal);
            }
        };
        assert_eq!(exit, (7, None));
        worker.join().unwrap();

        // A second connection must trust the saved key without prompting and
        // an active close must return the worker within the shutdown bound.
        let factory = provider.factory_for(&session);
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = async_channel::unbounded();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result =
                factory
                    .create()
                    .run(TerminalSize::new(90, 30, 9, 18), command_rx, event_tx);
            done_tx.send(result).unwrap();
        });
        loop {
            match event_rx.recv_blocking().unwrap() {
                TerminalTransportEvent::Prompt(prompt) => match prompt.kind() {
                    TerminalPromptKind::UnknownHost(_) => {
                        panic!("saved host key prompted again")
                    }
                    TerminalPromptKind::Authentication(_) => command_tx
                        .send(TerminalTransportCommand::PromptReply {
                            request_id: prompt.request_id(),
                            reply: TerminalPromptReply::Answers(vec![TerminalSecret::new(
                                "test-password",
                            )]),
                        })
                        .unwrap(),
                    TerminalPromptKind::HostKeyChanged(_) => panic!("unexpected changed key"),
                },
                TerminalTransportEvent::Started => break,
                TerminalTransportEvent::Failed(error) => panic!("SSH failed: {error}"),
                _ => {}
            }
        }
        command_tx.send(TerminalTransportCommand::Shutdown).unwrap();
        done_rx
            .recv_timeout(SHUTDOWN_TIMEOUT + Duration::from_secs(1))
            .expect("SSH worker did not stop in time")
            .unwrap();
        worker.join().unwrap();

        let state = server
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(state.pty, Some(("xterm-256color".into(), 90, 30, 810, 540)));
        assert_eq!(state.resize, Some((120, 40, 1200, 800)));
        assert_eq!(state.input, b"hello\nexit\n");
    }

    fn password_session(port: u16) -> Session {
        Session::new(
            crate::session::SessionId(1),
            crate::session::SessionDraft::new(
                "test",
                "127.0.0.1",
                port,
                "tester",
                AuthKind::Password,
                None,
            ),
        )
    }

    #[test]
    fn a_saved_password_connects_without_asking() {
        let Some(server) = start_server(TestAuth::Password) else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");
        let session = password_session(server.port);
        let secrets = Arc::new(InMemorySecretStore::default());
        secrets
            .set(&session.password_secret(), TEST_PASSWORD)
            .unwrap();

        let report = connect_with_secrets(session, &known_hosts, secrets, |kind| match kind {
            TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
            other => panic!("unexpected prompt: {other:?}"),
        });

        assert!(
            !report
                .prompts
                .iter()
                .any(|kind| matches!(kind, TerminalPromptKind::Authentication(_))),
            "已保存的密码不该再弹认证框"
        );
    }

    #[test]
    fn a_rejected_saved_password_asks_again_and_is_kept() {
        let Some(server) = start_server(TestAuth::Password) else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");
        let session = password_session(server.port);
        let endpoint = session.password_secret();
        let secrets = Arc::new(InMemorySecretStore::default());
        secrets.set(&endpoint, "stale-password").unwrap();

        let report =
            connect_with_secrets(session, &known_hosts, secrets.clone(), |kind| match kind {
                TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
                TerminalPromptKind::Authentication(_) => {
                    TerminalPromptReply::Answers(vec![TerminalSecret::new(TEST_PASSWORD)])
                }
                other => panic!("unexpected prompt: {other:?}"),
            });

        let instructions = report
            .prompts
            .iter()
            .find_map(|kind| match kind {
                TerminalPromptKind::Authentication(prompt) => Some(prompt.instructions()),
                _ => None,
            })
            .expect("被拒绝的密码应当退回认证弹框");
        assert!(
            instructions.contains("已保存的密码被服务器拒绝"),
            "弹框要说明为什么又问了一次：{instructions}"
        );
        assert_eq!(
            secrets
                .get(&endpoint)
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("stale-password"),
            "会话对话框里填的条目不该被静默删除"
        );
    }

    #[test]
    fn the_host_operating_system_is_detected_after_connecting() {
        let Some(server) = start_server_replying(TestAuth::Password, alpine_probe_reply) else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");

        let report =
            connect_and_probe(
                password_session(server.port),
                &known_hosts,
                |kind| match kind {
                    TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
                    TerminalPromptKind::Authentication(_) => {
                        TerminalPromptReply::Answers(vec![TerminalSecret::new(TEST_PASSWORD)])
                    }
                    other => panic!("unexpected prompt: {other:?}"),
                },
            );

        assert_eq!(report.host_os, Some(HostOs::Alpine));
        let execs = server
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .execs
            .clone();
        assert_eq!(
            execs,
            vec![PROBE_COMMAND.to_string()],
            "认出来了就不该再问第二遍"
        );
    }

    #[test]
    fn a_host_that_has_no_uname_is_asked_again_the_windows_way() {
        let Some(server) = start_server_replying(TestAuth::Password, windows_probe_reply) else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");

        let report =
            connect_and_probe(
                password_session(server.port),
                &known_hosts,
                |kind| match kind {
                    TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
                    TerminalPromptKind::Authentication(_) => {
                        TerminalPromptReply::Answers(vec![TerminalSecret::new(TEST_PASSWORD)])
                    }
                    other => panic!("unexpected prompt: {other:?}"),
                },
            );

        assert_eq!(report.host_os, Some(HostOs::Windows));
        let execs = server
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .execs
            .clone();
        assert_eq!(
            execs,
            vec![PROBE_COMMAND.to_string(), WINDOWS_PROBE_COMMAND.to_string()]
        );
    }

    #[test]
    fn a_host_that_answers_nothing_leaves_the_session_unmarked() {
        let Some(server) = start_server(TestAuth::Password) else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");

        let report =
            connect_and_probe(
                password_session(server.port),
                &known_hosts,
                |kind| match kind {
                    TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
                    TerminalPromptKind::Authentication(_) => {
                        TerminalPromptReply::Answers(vec![TerminalSecret::new(TEST_PASSWORD)])
                    }
                    other => panic!("unexpected prompt: {other:?}"),
                },
            );

        assert_eq!(report.host_os, None);
    }

    #[test]
    fn secret_debug_never_contains_plaintext() {
        let reply = TerminalPromptReply::Answers(vec![TerminalSecret::new("super-secret")]);
        let debug = format!("{reply:?}");
        assert!(!debug.contains("super-secret"));
        assert!(debug.contains("已隐藏"));
    }

    #[test]
    fn explicit_private_key_and_keyboard_interactive_authenticate() {
        let directory = tempfile::tempdir().unwrap();
        let mut rng = russh::keys::key::safe_rng();
        let client_key =
            russh::keys::PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519).unwrap();
        let Some(key_server) = start_server(TestAuth::PublicKey(client_key.public_key().clone()))
        else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let key_path = directory.path().join("id_ed25519");
        client_key
            .write_openssh_file(&key_path, russh::keys::ssh_key::LineEnding::LF)
            .unwrap();
        let key_session = Session::new(
            crate::session::SessionId(1),
            crate::session::SessionDraft::new(
                "key-test",
                "127.0.0.1",
                key_server.port,
                "tester",
                AuthKind::Key,
                None,
            )
            .with_key_path(key_path.to_string_lossy().into_owned()),
        );
        connect_then_shutdown(
            key_session,
            &directory.path().join("key-known-hosts"),
            |prompt| match prompt {
                TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
                other => panic!("unexpected key-auth prompt: {other:?}"),
            },
        );
        drop(key_server);

        let Some(interactive_server) = start_server(TestAuth::KeyboardInteractive) else {
            panic!("loopback became unavailable during keyboard-interactive test")
        };
        let interactive_session = Session::new(
            crate::session::SessionId(2),
            crate::session::SessionDraft::new(
                "interactive-test",
                "127.0.0.1",
                interactive_server.port,
                "tester",
                AuthKind::Password,
                None,
            ),
        );
        let mut saw_challenge = false;
        connect_then_shutdown(
            interactive_session,
            &directory.path().join("interactive-known-hosts"),
            |prompt| match prompt {
                TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
                TerminalPromptKind::Authentication(authentication) => {
                    saw_challenge = true;
                    assert_eq!(authentication.title(), "双字段验证");
                    assert_eq!(authentication.fields().len(), 2);
                    assert!(authentication.fields()[0].echo());
                    assert!(!authentication.fields()[1].echo());
                    TerminalPromptReply::Answers(vec![
                        TerminalSecret::new("tester"),
                        TerminalSecret::new("123456"),
                    ])
                }
                other => panic!("unexpected interactive prompt: {other:?}"),
            },
        );
        assert!(saw_challenge);
    }

    #[test]
    fn changed_host_key_is_reported_and_blocked() {
        let Some(server) = start_server(TestAuth::Password) else {
            eprintln!(
                "loopback sockets are unavailable in this sandbox; skipping integration body"
            );
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let known_hosts = directory.path().join("known_hosts");
        let mut rng = russh::keys::key::safe_rng();
        let stale_key =
            russh::keys::PrivateKey::random(&mut rng, russh::keys::Algorithm::Ed25519).unwrap();
        learn_known_hosts_path(
            "127.0.0.1",
            server.port,
            stale_key.public_key(),
            &known_hosts,
        )
        .unwrap();
        let session = Session::new(
            crate::session::SessionId(1),
            crate::session::SessionDraft::new(
                "test",
                "127.0.0.1",
                server.port,
                "tester",
                AuthKind::Password,
                None,
            ),
        );
        let provider = SshTerminalTransportProvider::new(&known_hosts, Arc::new(NoSecretStore));
        let factory = provider.factory_for(&session);
        let (_command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = async_channel::unbounded();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            done_tx
                .send(
                    factory
                        .create()
                        .run(TerminalSize::DEFAULT, command_rx, event_tx),
                )
                .unwrap();
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let changed = loop {
            match event_rx.try_recv() {
                Ok(TerminalTransportEvent::Prompt(prompt)) => match prompt.kind() {
                    TerminalPromptKind::HostKeyChanged(changed) => break changed.clone(),
                    other => panic!("unexpected prompt: {other:?}"),
                },
                Ok(event) => panic!("unexpected event: {event:?}"),
                Err(async_channel::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("host-key prompt was not emitted: {error}"),
            }
        };
        assert_eq!(changed.host(), "127.0.0.1");
        assert_eq!(changed.port(), server.port);
        assert_eq!(changed.known_hosts_path(), known_hosts);
        assert_eq!(changed.old_fingerprints().len(), 1);
        assert_ne!(changed.old_fingerprints()[0], changed.fingerprint());
        assert!(
            done_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("mismatched host connection did not stop")
                .is_err()
        );
        worker.join().unwrap();
    }
}
