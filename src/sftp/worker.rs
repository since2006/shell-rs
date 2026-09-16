use super::{
    RemotePath, SftpCommand, SftpEvent, SftpTransport, SftpTransportProvider, UploadPhase,
    client::{SftpClient, is_network_error},
    control::{Cancelled, UploadControl},
    journal::Journal,
    upload::UploadBatch,
};
use crate::{
    session::Session,
    ssh::{SshConnectionConfig, SshConnector, SshPrompts},
};
use anyhow::Result;
use async_channel::{Receiver, Sender};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{RwLock, watch};

pub struct SshSftpTransportProvider {
    connector: SshConnector,
    resume_dir: PathBuf,
}
impl SshSftpTransportProvider {
    pub fn new(connector: SshConnector, resume_dir: PathBuf) -> Self {
        Self {
            connector,
            resume_dir,
        }
    }
}
impl SftpTransportProvider for SshSftpTransportProvider {
    fn create(&self, session: &Session) -> Box<dyn SftpTransport> {
        Box::new(SshSftpTransport {
            connector: self.connector.clone(),
            config: SshConnectionConfig::from(session),
            journal: Journal::new(self.resume_dir.clone()),
        })
    }
}
struct SshSftpTransport {
    connector: SshConnector,
    config: SshConnectionConfig,
    journal: Journal,
}
impl SftpTransport for SshSftpTransport {
    fn run(
        self: Box<Self>,
        commands: Receiver<SftpCommand>,
        events: Sender<SftpEvent>,
    ) -> Result<()> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(self.run_async(commands, events))
    }
}
impl SshSftpTransport {
    async fn connect(
        &self,
        prompts: Arc<SshPrompts>,
        events: &Sender<SftpEvent>,
        shutdown: &watch::Receiver<bool>,
        shared: &RwLock<Option<Arc<SftpClient>>>,
    ) -> Result<Arc<SftpClient>> {
        events.send(SftpEvent::Connecting).await?;
        let mut stop = shutdown.clone();
        let client = tokio::select! {
            result = SftpClient::connect(&self.connector, &self.config, prompts) => Arc::new(result?),
            _ = stop.changed() => return Err(Cancelled.into()),
        };
        let home = client.canonicalize(&RemotePath::new(".")?).await?;
        *shared.write().await = Some(client.clone());
        events.send(SftpEvent::Connected { home }).await?;
        Ok(client)
    }
    async fn run_async(
        &self,
        commands: Receiver<SftpCommand>,
        events: Sender<SftpEvent>,
    ) -> Result<()> {
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (cancel_tx, cancel) = watch::channel(false);
        let (answers_tx, answers) = async_channel::unbounded();
        let (operations_tx, operations) = async_channel::bounded(1);
        let prompt_events = events.clone();
        let prompts = Arc::new(SshPrompts::new(
            Arc::new(move |p| prompt_events.try_send(SftpEvent::Prompt(p)).is_ok()),
            shutdown.clone(),
        ));
        let client: Arc<RwLock<Option<Arc<SftpClient>>>> = Arc::new(RwLock::new(None));
        let busy = Arc::new(AtomicBool::new(false));
        let router = {
            let prompts = prompts.clone();
            let events = events.clone();
            let client = client.clone();
            let busy = busy.clone();
            let cancel_tx = cancel_tx.clone();
            tokio::spawn(async move {
                while let Ok(command) = commands.recv().await {
                    match command {
                        SftpCommand::PromptReply { request_id, reply } => {
                            prompts.respond(request_id, reply)
                        }
                        SftpCommand::Answer { request_id, answer } => {
                            let _ = answers_tx.send((request_id, answer)).await;
                        }
                        SftpCommand::Cancel => {
                            cancel_tx.send_replace(true);
                            prompts.cancel_all();
                        }
                        SftpCommand::Disconnect => {
                            cancel_tx.send_replace(true);
                            prompts.cancel_all();
                            *client.write().await = None;
                            let _ = events
                                .send(SftpEvent::Disconnected("SFTP 已断开".into()))
                                .await;
                        }
                        SftpCommand::Shutdown => break,
                        SftpCommand::List { request_id, path } => {
                            let shared = client.clone();
                            let connected = client.read().await.clone();
                            let events = events.clone();
                            tokio::spawn(async move {
                                let result = match connected {
                                    Some(connected) => match connected.list(&path).await {
                                        Ok(listing) => Ok(listing),
                                        Err(error) => {
                                            if is_network_error(&error) {
                                                let mut current = shared.write().await;
                                                if current
                                                    .as_ref()
                                                    .is_some_and(|c| Arc::ptr_eq(c, &connected))
                                                {
                                                    *current = None;
                                                    let _ = events
                                                        .send(SftpEvent::Disconnected(
                                                            "SFTP 连接中断，请重新连接".into(),
                                                        ))
                                                        .await;
                                                }
                                            }
                                            Err(format!("无法读取目录：{error:#}"))
                                        }
                                    },
                                    None => Err("SFTP 未连接，请重新连接".into()),
                                };
                                let _ = events.send(SftpEvent::Listed { request_id, result }).await;
                            });
                        }
                        command => {
                            if busy.swap(true, Ordering::AcqRel) {
                                let _ = events
                                    .send(SftpEvent::Notice("已有上传批次正在处理".into()))
                                    .await;
                            } else {
                                cancel_tx.send_replace(false);
                                if operations_tx.send(command).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
                shutdown_tx.send_replace(true);
                cancel_tx.send_replace(true);
                prompts.cancel_all();
            })
        };
        let control = UploadControl {
            events: events.clone(),
            cancel,
            answers,
        };
        let mut batch: Option<UploadBatch> = None;
        match control
            .run(self.connect(prompts.clone(), &events, &shutdown, &client))
            .await
        {
            Ok(connected) => *client.write().await = Some(connected),
            Err(error) => {
                let _ = events
                    .send(SftpEvent::Disconnected(error.to_string()))
                    .await;
            }
        }
        let mut stop = shutdown.clone();
        while !*stop.borrow() {
            let operation = tokio::select! { operation = operations.recv() => match operation { Ok(operation) => operation, Err(_) => break }, _ = stop.changed() => break };
            if client.read().await.is_none() {
                match control
                    .run(self.connect(prompts.clone(), &events, &shutdown, &client))
                    .await
                {
                    Ok(connected) => *client.write().await = Some(connected),
                    Err(error) => {
                        events
                            .send(SftpEvent::Disconnected(error.to_string()))
                            .await?;
                        events.send(SftpEvent::Idle).await?;
                        busy.store(false, Ordering::Release);
                        continue;
                    }
                }
            }
            let connected = client.read().await.clone().expect("connected above");
            let prepared: Result<()> = async {
                if let SftpCommand::Upload(request) = &operation {
                    batch = None;
                    events
                        .send(SftpEvent::Progress(super::UploadProgress::default()))
                        .await?;
                    let target = control
                        .run(connected.canonicalize(request.destination()))
                        .await?;
                    let request = super::UploadRequest::new(request.sources().to_vec(), target)?;
                    batch = Some(
                        control
                            .run(UploadBatch::scan(
                                &request,
                                &self.config.endpoint(),
                                connected.fingerprint(),
                                self.journal.clone(),
                                &control,
                            ))
                            .await?,
                    );
                }
                if let Some(batch) = &batch {
                    batch.verify_host(connected.fingerprint())?;
                }
                if matches!(operation, SftpCommand::Discard) {
                    if let Some(current) = &batch {
                        current.discard(connected.as_ref()).await?;
                    }
                    batch = None;
                    events
                        .send(SftpEvent::Progress(super::UploadProgress {
                            phase: UploadPhase::Completed,
                            ..Default::default()
                        }))
                        .await?;
                }
                Ok(())
            }
            .await;
            if let Err(error) = prepared {
                events.send(SftpEvent::Notice(error.to_string())).await?;
                if let Some(batch) = &mut batch {
                    batch.phase(UploadPhase::Stopped, &control);
                } else {
                    events
                        .send(SftpEvent::Progress(super::UploadProgress {
                            phase: UploadPhase::Stopped,
                            details: vec![error.to_string()],
                            ..Default::default()
                        }))
                        .await?;
                }
                busy.store(false, Ordering::Release);
                continue;
            }
            if !matches!(operation, SftpCommand::Discard)
                && let Some(batch) = &mut batch
            {
                let mut reconnects = 0;
                loop {
                    let connected = client.read().await.clone();
                    let result = match connected {
                        Some(connected) => batch.run(connected.as_ref(), &control).await,
                        None => {
                            if control.check().is_err() {
                                Err(Cancelled.into())
                            } else {
                                Err(std::io::Error::new(
                                    std::io::ErrorKind::NotConnected,
                                    "SFTP 未连接",
                                )
                                .into())
                            }
                        }
                    };
                    match result {
                        Ok(()) => break,
                        Err(error) if is_network_error(&error) => {
                            *client.write().await = None;
                            events
                                .send(SftpEvent::Disconnected("连接中断，上传进度已保留".into()))
                                .await?;
                            let mut recovered = false;
                            while reconnects < 3 {
                                batch.progress.current = format!(
                                    "将在 {} 秒后重连（{}/3）",
                                    [1, 3, 10][reconnects],
                                    reconnects + 1
                                );
                                batch.phase(UploadPhase::Reconnecting, &control);
                                let delay = Duration::from_secs([1, 3, 10][reconnects]);
                                reconnects += 1;
                                if control
                                    .run(async {
                                        tokio::time::sleep(delay).await;
                                        Ok(())
                                    })
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                                match control
                                    .run(self.connect(prompts.clone(), &events, &shutdown, &client))
                                    .await
                                {
                                    Ok(next) => match batch.verify_host(next.fingerprint()) {
                                        Ok(()) => {
                                            *client.write().await = Some(next);
                                            recovered = true;
                                            break;
                                        }
                                        Err(error) => {
                                            events
                                                .send(SftpEvent::Notice(error.to_string()))
                                                .await?;
                                            break;
                                        }
                                    },
                                    Err(error) => {
                                        events
                                            .send(SftpEvent::Disconnected(error.to_string()))
                                            .await?;
                                        // Authentication/trust failures must require user action.
                                        if !is_network_error(&error) {
                                            break;
                                        }
                                    }
                                }
                            }
                            if recovered {
                                continue;
                            }
                            batch.phase(UploadPhase::Stopped, &control);
                            break;
                        }
                        Err(error) => {
                            if !error.is::<Cancelled>() {
                                events.send(SftpEvent::Notice(error.to_string())).await?;
                            }
                            batch.phase(UploadPhase::Stopped, &control);
                            break;
                        }
                    }
                }
                if batch.is_complete() {
                    batch.phase(UploadPhase::Completed, &control);
                }
            }
            busy.store(false, Ordering::Release);
            events.send(SftpEvent::Idle).await?;
        }
        router.abort();
        *client.write().await = None;
        Ok(())
    }
}
