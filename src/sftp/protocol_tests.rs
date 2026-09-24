//! Loopback-only SSH/SFTP worker tests. No user hosts, credentials or shell.
use super::*;
use crate::{
    connection::{ConnectionPromptKind, ConnectionPromptReply, ConnectionSecret},
    secrets::InMemorySecretStore,
    session::{AuthKind, Session, SessionDraft, SessionId},
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
    shells: Arc<AtomicUsize>,
    interrupts: Arc<AtomicUsize>,
    unsupported: bool,
}
struct Handler {
    directory: PathBuf,
    shells: Arc<AtomicUsize>,
    interrupts: Arc<AtomicUsize>,
    unsupported: bool,
    channels: HashMap<ChannelId, russh::Channel<server::Msg>>,
}
impl server::Server for TestServer {
    type Handler = Handler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Handler {
        self.connections.fetch_add(1, Ordering::SeqCst);
        Handler {
            directory: self.directory.clone(),
            shells: self.shells.clone(),
            interrupts: self.interrupts.clone(),
            unsupported: self.unsupported,
            channels: HashMap::new(),
        }
    }
}
impl server::Handler for Handler {
    type Error = anyhow::Error;
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
        tokio::spawn(async move {
            let executable = if cfg!(target_os = "macos") {
                "/usr/libexec/sftp-server"
            } else {
                "/usr/lib/openssh/sftp-server"
            };
            let mut child = tokio::process::Command::new(executable)
                .current_dir(directory)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
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
            tokio::select! { _ = forward=>{}, _=tokio::io::copy(&mut output,&mut writer)=>{} }
            child.kill().await.ok();
            child.wait().await.ok();
        });
        Ok(())
    }
}
struct Running {
    port: u16,
    connections: Arc<AtomicUsize>,
    shells: Arc<AtomicUsize>,
    interrupts: Arc<AtomicUsize>,
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
    let shells = Arc::new(AtomicUsize::new(0));
    let interrupts = Arc::new(AtomicUsize::new(interrupts));
    let mut server = TestServer {
        directory,
        connections: connections.clone(),
        shells: shells.clone(),
        interrupts: interrupts.clone(),
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
        shells,
        interrupts,
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
    let provider = SshSftpTransportProvider::new(
        SshConnector::new(
            data.join("known_hosts"),
            Arc::new(InMemorySecretStore::default()),
        ),
        data.join("upload-resume"),
        data.join("download-resume"),
    );
    let session = Session::new(
        SessionId(1),
        SessionDraft::new(
            "fixture",
            "127.0.0.1",
            port,
            "tester",
            AuthKind::Password,
            None,
        ),
    );
    let transport = provider.create(&session);
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
