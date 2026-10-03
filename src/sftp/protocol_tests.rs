//! Loopback-only SSH/SFTP worker tests. No user hosts, credentials or shell.
use super::*;
use crate::{
    connection::{ConnectionPromptKind, ConnectionPromptReply, ConnectionSecret, Latency},
    host::{AuthKind, Host, HostDraft, HostId, HostLogin, HostStore, SshLink},
    secrets::{InMemorySecretStore, SharedSecretStore},
    ssh::SshConnector,
};
use russh::{
    ChannelId,
    server::{self, Server as _},
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct TestServer {
    directory: PathBuf,
    connections: Arc<AtomicUsize>,
    ended: Arc<AtomicUsize>,
    shells: Arc<AtomicUsize>,
    interrupts: Arc<AtomicUsize>,
    hangup: Arc<tokio::sync::Notify>,
    unsupported: bool,
}
struct Handler {
    directory: PathBuf,
    /// Counts connections that have ended, as this drops with its own.
    ended: Arc<AtomicUsize>,
    shells: Arc<AtomicUsize>,
    interrupts: Arc<AtomicUsize>,
    /// Ends an SFTP session while nothing is asked of it.
    hangup: Arc<tokio::sync::Notify>,
    unsupported: bool,
    channels: HashMap<ChannelId, russh::Channel<server::Msg>>,
}
impl server::Server for TestServer {
    type Handler = Handler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Handler {
        self.connections.fetch_add(1, Ordering::SeqCst);
        Handler {
            directory: self.directory.clone(),
            ended: self.ended.clone(),
            shells: self.shells.clone(),
            interrupts: self.interrupts.clone(),
            hangup: self.hangup.clone(),
            unsupported: self.unsupported,
            channels: HashMap::new(),
        }
    }
}
impl Drop for Handler {
    fn drop(&mut self) {
        self.ended.fetch_add(1, Ordering::SeqCst);
    }
}
/// Who the server lets in without asking anything: a bastion host's token,
/// which is the user name alone.
const TOKEN_USER: &str = "b478e26f-811b-4a90-81c3-74929127898a";

impl server::Handler for Handler {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, user: &str) -> Result<server::Auth, Self::Error> {
        Ok(if user == TOKEN_USER {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == "tester" && password == "fixture-password" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.shells.fetch_add(1, Ordering::SeqCst);
        session.channel_failure(channel)?;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if self.unsupported || name != "sftp" {
            session.channel_failure(channel)?;
            session.close(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        let stream = self.channels.remove(&channel).unwrap().into_stream();
        let directory = self.directory.clone();
        let interrupts = self.interrupts.clone();
        let hangup = self.hangup.clone();
        tokio::spawn(async move {
            let executable = if cfg!(target_os = "macos") {
                "/usr/libexec/sftp-server"
            } else {
                "/usr/lib/openssh/sftp-server"
            };
            let mut child = {
                let _forks = crate::testing::no_forks();
                tokio::process::Command::new(executable)
                    .current_dir(directory)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap()
            };
            let mut input = child.stdin.take().unwrap();
            let mut output = child.stdout.take().unwrap();
            let (mut reader, mut writer) = tokio::io::split(stream);
            let forward = async {
                let mut writes = 0;
                loop {
                    let size = reader.read_u32().await?;
                    if size > 256 * 1024 {
                        return Err::<(), _>(std::io::Error::other("invalid frame"));
                    }
                    let mut bytes = vec![0; size as usize];
                    reader.read_exact(&mut bytes).await?;
                    // SSH_FXP_WRITE (6) for uploads, SSH_FXP_READ (5) for downloads.
                    if matches!(bytes.first(), Some(&5) | Some(&6)) {
                        writes += 1;
                        if writes == 2
                            && interrupts
                                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                                    n.checked_sub(1)
                                })
                                .is_ok()
                        {
                            return Err::<(), _>(std::io::Error::other("injected disconnect"));
                        }
                    }
                    input.write_u32(size).await?;
                    input.write_all(&bytes).await?;
                    input.flush().await?;
                }
            };
            tokio::select! {
                _ = forward => {}
                _ = tokio::io::copy(&mut output, &mut writer) => {}
                _ = hangup.notified() => {}
            }
            child.kill().await.ok();
            child.wait().await.ok();
        });
        Ok(())
    }
}
struct Running {
    port: u16,
    connections: Arc<AtomicUsize>,
    ended: Arc<AtomicUsize>,
    shells: Arc<AtomicUsize>,
    interrupts: Arc<AtomicUsize>,
    hangup: Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(directory: PathBuf, interrupts: usize, unsupported: bool) -> Option<Running> {
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("loopback unavailable; run this test with local socket permission");
            return None;
        }
        Err(e) => panic!("{e}"),
    };
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    let ended = Arc::new(AtomicUsize::new(0));
    let shells = Arc::new(AtomicUsize::new(0));
    let interrupts = Arc::new(AtomicUsize::new(interrupts));
    let hangup = Arc::new(tokio::sync::Notify::new());
    let mut server = TestServer {
        directory,
        connections: connections.clone(),
        ended: ended.clone(),
        shells: shells.clone(),
        interrupts: interrupts.clone(),
        hangup: hangup.clone(),
        unsupported,
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
    Some(Running {
        port,
        connections,
        ended,
        shells,
        interrupts,
        hangup,
        task,
    })
}
struct Worker {
    commands: async_channel::Sender<SftpCommand>,
    events: async_channel::Receiver<SftpEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.commands.try_send(SftpCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn worker(port: u16, data: &std::path::Path) -> Worker {
    let host = Host::new(
        HostId(1),
        HostDraft::new(
            "fixture",
            "127.0.0.1",
            port,
            "tester",
            AuthKind::Password,
            None,
        ),
    );
    worker_for(
        &HostLogin::of(&host, None),
        Arc::new(InMemorySecretStore::default()),
        data,
    )
}
fn worker_for(login: &HostLogin, secrets: SharedSecretStore, data: &std::path::Path) -> Worker {
    let provider = SshSftpTransportProvider::new(
        SshConnector::new(data.join("known_hosts"), secrets),
        data.join("upload-resume"),
        data.join("download-resume"),
    );
    let transport = provider.create(login);
    let (commands, rx) = async_channel::unbounded();
    let (tx, events) = async_channel::unbounded();
    let thread = std::thread::spawn(move || transport.run(rx, tx).unwrap());
    Worker {
        commands,
        events,
        thread: Some(thread),
    }
}
async fn next(worker: &Worker) -> SftpEvent {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(25), worker.events.recv())
            .await
            .expect("worker timed out")
            .unwrap();
        if let SftpEvent::Prompt(p) = event {
            let reply = match p.kind() {
                ConnectionPromptKind::UnknownHost(_) => ConnectionPromptReply::TrustAndSave,
                ConnectionPromptKind::Authentication(_) => {
                    ConnectionPromptReply::Answers(vec![ConnectionSecret::new("fixture-password")])
                }
                _ => panic!("unexpected trust change"),
            };
            worker
                .commands
                .send(SftpCommand::PromptReply {
                    request_id: p.request_id(),
                    reply,
                })
                .await
                .unwrap();
        } else {
            return event;
        }
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn worker_uses_sftp_without_shell_and_reconnects_after_interrupted_write() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        let Some(server) = server(remote.clone(), 1, false).await else {
            return;
        };
        let worker = worker(server.port, temp.path());
        let home = loop {
            match next(&worker).await {
                SftpEvent::Connected { home } => break home,
                SftpEvent::Disconnected(e) => panic!("connect failed: {e}"),
                _ => {}
            }
        };
        let source = temp.path().join("文件.bin");
        let data = vec![42; 300_007];
        std::fs::write(&source, &data).unwrap();
        worker
            .commands
            .send(SftpCommand::Upload(
                UploadRequest::new(vec![source], home).unwrap(),
            ))
            .await
            .unwrap();
        let mut reconnecting = false;
        loop {
            match next(&worker).await {
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Completed => {
                    assert_eq!(p.succeeded(), 1);
                    break;
                }
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Stopped => {
                    panic!("stopped unexpectedly: {p:?}")
                }
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Reconnecting => {
                    reconnecting = true
                }
                SftpEvent::Question(q) => panic!("unexpected question: {q:?}"),
                _ => {}
            }
        }
        assert!(reconnecting);
        assert_eq!(server.connections.load(Ordering::SeqCst), 2);
        assert_eq!(server.shells.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(remote.join("文件.bin")).unwrap(), data);
    });
}

/// A connection that drops while nothing is asked of it is reported at
/// once, as a terminal's is, not at the next request; 继续 connects again.
#[test]
fn an_idle_connection_that_drops_is_reported_without_a_request() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let Some(server) = server(temp.path().into(), 0, false).await else {
            return;
        };
        let worker = worker(server.port, temp.path());
        loop {
            match next(&worker).await {
                SftpEvent::Connected { .. } => break,
                SftpEvent::Disconnected(e) => panic!("connect failed: {e}"),
                _ => {}
            }
        }
        server.hangup.notify_one();
        loop {
            match next(&worker).await {
                SftpEvent::Disconnected(reason) => {
                    assert_eq!(reason, "SFTP 连接中断，请重新连接");
                    break;
                }
                SftpEvent::Connected { .. } => panic!("still connected"),
                _ => {}
            }
        }
        worker.commands.send(SftpCommand::Resume).await.unwrap();
        loop {
            match next(&worker).await {
                SftpEvent::Connected { .. } => break,
                SftpEvent::Disconnected(e) => panic!("reconnect failed: {e}"),
                _ => {}
            }
        }
        assert_eq!(server.connections.load(Ordering::SeqCst), 2);

        // Watching an idle connection must not keep one open after 断开.
        worker.commands.send(SftpCommand::Disconnect).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while server.ended.load(Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the connection stayed open after 断开");
    });
}

/// Like a terminal's, the connection's round trip is reported right after
/// connecting, with nothing asked of it.
#[test]
fn a_connection_reports_its_round_trip_right_after_connecting() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let Some(server) = server(temp.path().into(), 0, false).await else {
            return;
        };
        let worker = worker(server.port, temp.path());
        loop {
            match next(&worker).await {
                SftpEvent::Connected { .. } => break,
                SftpEvent::Disconnected(e) => panic!("connect failed: {e}"),
                _ => {}
            }
        }
        let latency = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let SftpEvent::Latency(latency) = next(&worker).await {
                    break latency;
                }
            }
        })
        .await
        .expect("no round trip after connecting");
        assert!(matches!(latency, Latency::Measured(_)), "{latency:?}");
    });
}

/// A bastion host lets WinSCP, and so ShellRS, in on its token alone: an
/// `sftp://token@…` link without a password connects without asking for
/// one, as Xshell's `ssh://` does.
#[test]
fn a_link_without_a_password_connects_where_the_server_asks_for_none() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let Some(server) = server(temp.path().into(), 0, false).await else {
            return;
        };
        let mut store = HostStore::empty();
        let link = format!("sftp://{TOKEN_USER}@127.0.0.1:{}", server.port);
        let host = store.insert_external_unnotified(SshLink::parse(&link, None).unwrap());
        let worker = worker_for(&store.login(host).unwrap(), store.secrets(), temp.path());
        loop {
            let event = tokio::time::timeout(Duration::from_secs(25), worker.events.recv())
                .await
                .expect("worker timed out")
                .unwrap();
            match event {
                SftpEvent::Prompt(prompt) => {
                    assert!(
                        matches!(prompt.kind(), ConnectionPromptKind::UnknownHost(_)),
                        "asked for more than trusting the host key"
                    );
                    worker
                        .commands
                        .send(SftpCommand::PromptReply {
                            request_id: prompt.request_id(),
                            reply: ConnectionPromptReply::TrustAndSave,
                        })
                        .await
                        .unwrap();
                }
                SftpEvent::Connected { .. } => break,
                SftpEvent::Disconnected(error) => panic!("connect failed: {error}"),
                _ => {}
            }
        }
        assert_eq!(server.connections.load(Ordering::SeqCst), 1);
        assert_eq!(server.shells.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn unsupported_sftp_does_not_launch_shell_or_loop_authentication() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let Some(server) = server(temp.path().into(), 0, true).await else {
            return;
        };
        let worker = worker(server.port, temp.path());
        loop {
            match next(&worker).await {
                SftpEvent::Disconnected(_) => break,
                SftpEvent::Connected { .. } => panic!("unsupported subsystem connected"),
                _ => {}
            }
        }
        assert_eq!(server.connections.load(Ordering::SeqCst), 1);
        assert_eq!(server.shells.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn reconnect_exhaustion_waits_for_manual_resume_and_resets_budget() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        let Some(server) = server(remote.clone(), 4, false).await else {
            return;
        };
        let worker = worker(server.port, temp.path());
        let home = loop {
            if let SftpEvent::Connected { home } = next(&worker).await {
                break home;
            }
        };
        let source = temp.path().join("file");
        std::fs::write(&source, vec![1; 200_000]).unwrap();
        worker
            .commands
            .send(SftpCommand::Upload(
                UploadRequest::new(vec![source], home).unwrap(),
            ))
            .await
            .unwrap();
        loop {
            match next(&worker).await {
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Stopped => break,
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Completed => {
                    panic!("exhaustion should stop")
                }
                SftpEvent::Question(q) => panic!("unexpected question {q:?}"),
                _ => {}
            }
        }
        assert_eq!(server.connections.load(Ordering::SeqCst), 4);
        // The next manual run has its own three-retry budget; interrupt once again.
        server.interrupts.store(1, Ordering::SeqCst);
        worker.commands.send(SftpCommand::Resume).await.unwrap();
        loop {
            match next(&worker).await {
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Completed => break,
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Stopped => {
                    panic!("manual retry failed")
                }
                SftpEvent::Question(q) => panic!("unexpected question {q:?}"),
                _ => {}
            }
        }
        assert_eq!(server.connections.load(Ordering::SeqCst), 6);
        assert_eq!(
            std::fs::read(remote.join("file")).unwrap(),
            vec![1; 200_000]
        );
    });
}

#[test]
fn worker_downloads_and_resumes_after_an_interrupted_read() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        std::fs::create_dir(&remote).unwrap();
        let data: Vec<u8> = (0..300_007).map(|i| (i % 241) as u8).collect();
        std::fs::write(remote.join("文件.bin"), &data).unwrap();
        let out = temp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let Some(server) = server(remote.clone(), 1, false).await else {
            return;
        };
        let worker = worker(server.port, temp.path());
        let home = loop {
            match next(&worker).await {
                SftpEvent::Connected { home } => break home,
                SftpEvent::Disconnected(e) => panic!("connect failed: {e}"),
                _ => {}
            }
        };
        worker
            .commands
            .send(SftpCommand::Download(
                DownloadRequest::new(vec![home.join("文件.bin").unwrap()], out.clone()).unwrap(),
            ))
            .await
            .unwrap();
        let mut reconnecting = false;
        loop {
            match next(&worker).await {
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Completed => {
                    assert_eq!(p.succeeded(), 1);
                    assert_eq!(p.direction(), TransferDirection::Download);
                    break;
                }
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Stopped => {
                    panic!("stopped unexpectedly: {p:?}")
                }
                SftpEvent::Progress(p) if p.phase() == TransferPhase::Reconnecting => {
                    reconnecting = true
                }
                SftpEvent::Question(q) => panic!("unexpected question: {q:?}"),
                _ => {}
            }
        }
        assert!(reconnecting);
        assert_eq!(server.connections.load(Ordering::SeqCst), 2);
        assert_eq!(std::fs::read(out.join("文件.bin")).unwrap(), data);
        assert!(!out.join("文件.bin.filepart").exists());
    });
}

/// Run one batch to its end, answering nothing.
async fn transfer(worker: &Worker, command: SftpCommand) -> TransferProgress {
    worker.commands.send(command).await.unwrap();
    let mut last = None;
    loop {
        match next(worker).await {
            SftpEvent::Progress(progress) => last = Some(progress),
            SftpEvent::Question(question) => panic!("unexpected question: {question:?}"),
            SftpEvent::Idle => return last.expect("no progress before idle"),
            _ => {}
        }
    }
}

#[test]
fn scp_style_transfers_land_where_scp_would_put_them() {
    runtime().block_on(async {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote");
        std::fs::create_dir_all(remote.join("sub")).unwrap();
        let Some(server) = server(remote.clone(), 0, false).await else {
            return;
        };
        let worker = worker(server.port, temp.path());
        let home = loop {
            match next(&worker).await {
                SftpEvent::Connected { home } => break home,
                SftpEvent::Disconnected(e) => panic!("connect failed: {e}"),
                _ => {}
            }
        };
        let source = temp.path().join("local.txt");
        std::fs::write(&source, b"payload").unwrap();

        // Not there yet: the destination is the copy's own name.
        let request = UploadRequest::scp(source.clone(), home.join("renamed.txt").unwrap());
        let done = transfer(&worker, SftpCommand::Upload(request)).await;
        assert_eq!(done.phase(), TransferPhase::Completed);
        assert_eq!(
            std::fs::read(remote.join("renamed.txt")).unwrap(),
            b"payload"
        );

        // An existing directory: the copy goes inside, under its own name.
        let request = UploadRequest::scp(source, home.join("sub").unwrap());
        let done = transfer(&worker, SftpCommand::Upload(request)).await;
        assert_eq!(done.phase(), TransferPhase::Completed);
        assert_eq!(
            std::fs::read(remote.join("sub/local.txt")).unwrap(),
            b"payload"
        );

        // And back, under a new local name.
        let back = temp.path().join("back.txt");
        let request =
            DownloadRequest::scp(home.join("renamed.txt").unwrap(), back.clone()).unwrap();
        let done = transfer(&worker, SftpCommand::Download(request)).await;
        assert_eq!(done.phase(), TransferPhase::Completed);
        assert_eq!(std::fs::read(&back).unwrap(), b"payload");

        // A parent that is not there is an error, not a guess.
        let request = UploadRequest::scp(
            temp.path().join("back.txt"),
            home.join("missing").unwrap().join("x.txt").unwrap(),
        );
        let done = transfer(&worker, SftpCommand::Upload(request)).await;
        assert_ne!(done.phase(), TransferPhase::Completed);
        assert!(!remote.join("missing").exists());
    });
}
