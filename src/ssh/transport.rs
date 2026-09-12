use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use async_channel::Sender;
use russh::ChannelMsg;
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::known_hosts::{known_host_keys_path, learn_known_hosts_path};
use russh::keys::{
    HashAlg, PrivateKeyWithHashAlg, PublicKey, load_secret_key, parse_public_key_base64,
};
use russh::{MethodKind, MethodSet};
use tokio::sync::{mpsc as tokio_mpsc, oneshot, watch};
use zeroize::Zeroizing;

use crate::secrets::{SecretRef, SharedSecretStore};
use crate::session::{AuthKind, Session};
use crate::terminal::{
    RemoteTerminalTransportProvider, SharedTerminalTransportFactory, TerminalPrompt,
    TerminalPromptField, TerminalPromptKind, TerminalPromptReply, TerminalSize, TerminalTransport,
    TerminalTransportCommand, TerminalTransportEvent, TerminalTransportFactory,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const AUTH_RETRIES: usize = 3;
static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

/// Production remote provider. Its write lock is shared by every connection,
/// making host-key checks and appends to shellr's trust file serial.
pub struct SshTerminalTransportProvider {
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    secrets: SharedSecretStore,
}

impl SshTerminalTransportProvider {
    pub fn new(path: impl Into<PathBuf>, secrets: SharedSecretStore) -> Self {
        Self {
            known_hosts_path: path.into(),
            known_hosts_lock: Arc::new(Mutex::new(())),
            secrets,
        }
    }
}

impl RemoteTerminalTransportProvider for SshTerminalTransportProvider {
    fn factory_for(&self, session: &Session) -> SharedTerminalTransportFactory {
        Arc::new(SshTerminalTransportFactory {
            config: SshConnectionConfig::from(session),
            known_hosts_path: self.known_hosts_path.clone(),
            known_hosts_lock: self.known_hosts_lock.clone(),
            secrets: self.secrets.clone(),
        })
    }
}

#[derive(Clone)]
struct SshConnectionConfig {
    host: String,
    port: u16,
    user: String,
    auth: AuthKind,
    key_path: Option<PathBuf>,
}

impl From<&Session> for SshConnectionConfig {
    fn from(session: &Session) -> Self {
        Self {
            host: session.host.to_string(),
            port: session.port,
            user: session.user.to_string(),
            auth: session.auth,
            key_path: session
                .key_path
                .as_ref()
                .map(|path| PathBuf::from(path.as_ref())),
        }
    }
}

struct SshTerminalTransportFactory {
    config: SshConnectionConfig,
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    secrets: SharedSecretStore,
}

impl TerminalTransportFactory for SshTerminalTransportFactory {
    fn create(&self) -> Box<dyn TerminalTransport> {
        Box::new(SshTerminalTransport {
            config: self.config.clone(),
            known_hosts_path: self.known_hosts_path.clone(),
            known_hosts_lock: self.known_hosts_lock.clone(),
            secrets: self.secrets.clone(),
        })
    }
}

struct SshTerminalTransport {
    config: SshConnectionConfig,
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    secrets: SharedSecretStore,
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
        let broker = Arc::new(PromptBroker::new(events.clone(), shutdown_rx));
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

        let config = client::Config {
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            keepalive_max: 3,
            nodelay: true,
            ..Default::default()
        };
        let handler = SshClientHandler {
            host: self.config.host.clone(),
            port: self.config.port,
            known_hosts_path: self.known_hosts_path.clone(),
            known_hosts_lock: self.known_hosts_lock.clone(),
            broker: broker.clone(),
        };

        let connect = client::connect(
            Arc::new(config),
            (self.config.host.as_str(), self.config.port),
            handler,
        );
        let mut shutdown = broker.shutdown_receiver();
        let mut handle = tokio::select! {
            result = timeout_excluding_prompts(
                connect,
                broker.prompt_activity_receiver(),
                CONNECT_TIMEOUT,
            ) => {
                result?.map_err(|error| anyhow!(safe_connect_error(&error)))?
            }
            _ = shutdown.changed() => bail!("连接已取消"),
        };

        tokio::select! {
            result = authenticate(&mut handle, &self.config, &self.secrets, &broker) => result?,
            _ = shutdown.changed() => bail!("连接已取消"),
        }
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

struct PromptBroker {
    pending: Mutex<HashMap<u64, oneshot::Sender<TerminalPromptReply>>>,
    events: Sender<TerminalTransportEvent>,
    shutdown: watch::Receiver<bool>,
    prompt_activity: watch::Sender<bool>,
}

impl PromptBroker {
    fn new(events: Sender<TerminalTransportEvent>, shutdown: watch::Receiver<bool>) -> Self {
        let (prompt_activity, _) = watch::channel(false);
        Self {
            pending: Mutex::new(HashMap::new()),
            events,
            shutdown,
            prompt_activity,
        }
    }

    fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown.clone()
    }

    fn prompt_activity_receiver(&self) -> watch::Receiver<bool> {
        self.prompt_activity.subscribe()
    }

    async fn ask(&self, kind: TerminalPromptKind) -> Result<TerminalPromptReply> {
        let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(request_id, sender);
        let _ = self.prompt_activity.send(true);
        let result = async {
            self.events
                .send(TerminalTransportEvent::Prompt(TerminalPrompt::new(
                    request_id, kind,
                )))
                .await
                .map_err(|_| anyhow!("终端标签页已关闭"))?;
            let mut shutdown = self.shutdown.clone();
            tokio::select! {
                reply = receiver => reply.map_err(|_| anyhow!("认证请求已取消")),
                _ = shutdown.changed() => bail!("连接已取消"),
            }
        }
        .await;
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&request_id);
        let _ = self.prompt_activity.send(false);
        result
    }

    async fn emit(&self, kind: TerminalPromptKind) {
        let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let _ = self
            .events
            .send(TerminalTransportEvent::Prompt(TerminalPrompt::new(
                request_id, kind,
            )))
            .await;
    }

    fn respond(&self, request_id: u64, reply: TerminalPromptReply) {
        if let Some(sender) = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&request_id)
        {
            let _ = sender.send(reply);
        }
    }

    fn cancel_all(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

/// Apply the network handshake timeout without counting time spent waiting
/// for an explicit answer from the user to a host-trust prompt.
async fn timeout_excluding_prompts<F>(
    future: F,
    mut prompt_activity: watch::Receiver<bool>,
    timeout: Duration,
) -> Result<F::Output>
where
    F: Future,
{
    tokio::pin!(future);
    let mut remaining = timeout;
    loop {
        if *prompt_activity.borrow() {
            tokio::select! {
                result = &mut future => return Ok(result),
                changed = prompt_activity.changed() => {
                    if changed.is_err() {
                        bail!("连接已取消");
                    }
                }
            }
            continue;
        }

        let started = tokio::time::Instant::now();
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = tokio::time::sleep(remaining) => bail!("连接超时（15 秒）"),
            changed = prompt_activity.changed() => {
                if changed.is_err() {
                    bail!("连接已取消");
                }
                remaining = remaining.saturating_sub(started.elapsed());
                if remaining.is_zero() && !*prompt_activity.borrow() {
                    bail!("连接超时（15 秒）");
                }
            }
        }
    }
}

struct SshClientHandler {
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    broker: Arc<PromptBroker>,
}

impl client::Handler for SshClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let algorithm = key.algorithm().to_string();
        let known = {
            let _guard = self
                .known_hosts_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            read_known_keys(&self.host, self.port, &self.known_hosts_path)?
        };
        if known.iter().any(|(_, saved)| saved == &key) {
            return Ok(true);
        }
        if !known.is_empty() {
            let old = known
                .iter()
                .map(|(_, saved)| saved.fingerprint(HashAlg::Sha256).to_string())
                .collect();
            self.broker
                .emit(TerminalPromptKind::host_key_changed(
                    &self.host,
                    self.port,
                    algorithm,
                    old,
                    fingerprint,
                    &self.known_hosts_path,
                ))
                .await;
            return Ok(false);
        }

        let reply = self
            .broker
            .ask(TerminalPromptKind::unknown_host(
                &self.host,
                self.port,
                algorithm,
                fingerprint,
            ))
            .await?;
        if !matches!(reply, TerminalPromptReply::TrustAndSave) {
            return Ok(false);
        }
        let _guard = self
            .known_hosts_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let known = read_known_keys(&self.host, self.port, &self.known_hosts_path)?;
        if known.iter().any(|(_, saved)| saved == &key) {
            return Ok(true);
        }
        if !known.is_empty() {
            bail!("保存主机密钥时发现信任文件已发生变化");
        }
        learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts_path)
            .map_err(|_| anyhow!("无法写入主机信任文件：{}", self.known_hosts_path.display()))?;
        Ok(true)
    }
}

fn read_known_keys(host: &str, port: u16, path: &Path) -> Result<Vec<(usize, PublicKey)>> {
    if path.exists() {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取主机信任文件：{}", path.display()))?;
        for line in contents.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let valid = fields.next().is_some()
                && fields.next().is_some()
                && fields
                    .next()
                    .is_some_and(|encoded| parse_public_key_base64(encoded).is_ok());
            if !valid {
                bail!("主机信任文件已损坏：{}", path.display());
            }
        }
    }
    known_host_keys_path(host, port, path)
        .map_err(|_| anyhow!("主机信任文件已损坏或无法读取：{}", path.display()))
}

async fn authenticate<H: client::Handler>(
    handle: &mut client::Handle<H>,
    config: &SshConnectionConfig,
    secrets: &SharedSecretStore,
    broker: &PromptBroker,
) -> Result<()>
where
    H::Error: From<russh::Error>,
{
    let first = handle
        .authenticate_none(&config.user)
        .await
        .map_err(|_| anyhow!("无法查询服务器支持的认证方式"))?;
    if first.success() {
        return Ok(());
    }
    let mut methods = remaining_methods(first);
    let mut partial = false;

    if config.auth == AuthKind::Auto
        && methods.contains(&MethodKind::PublicKey)
        && let Some(result) = try_agent(handle, &config.user).await?
    {
        if result.success() {
            return Ok(());
        }
        partial = is_partial(&result);
        methods = remaining_methods(result);
    }

    if matches!(config.auth, AuthKind::Auto | AuthKind::Key)
        && methods.contains(&MethodKind::PublicKey)
    {
        let paths = key_paths(config)?;
        for path in paths {
            let Some(key) = load_private_key(&path, secrets, broker).await? else {
                continue;
            };
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|_| anyhow!("无法协商 RSA 签名算法"))?
                .flatten();
            let result = handle
                .authenticate_publickey(
                    &config.user,
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|_| anyhow!("私钥认证失败"))?;
            if result.success() {
                return Ok(());
            }
            partial = is_partial(&result);
            methods = remaining_methods(result);
            if partial {
                break;
            }
        }
        if config.auth == AuthKind::Key && !partial {
            bail!("服务器未接受指定的私钥");
        }
    }

    if matches!(config.auth, AuthKind::Auto | AuthKind::Password) || partial {
        if methods.contains(&MethodKind::Password) {
            // Try what the session has saved before bothering anyone.
            let mut saved_rejected = false;
            if let Some(saved) = saved_secret(secrets, &password_secret(config)) {
                let result = handle
                    .authenticate_password(&config.user, saved.to_string())
                    .await
                    .map_err(|_| anyhow!("密码认证失败"))?;
                if result.success() {
                    return Ok(());
                }
                // The entry stays: the user typed it into the session dialog,
                // and deleting it behind their back would be baffling. This
                // connection just falls back to asking, and says why.
                saved_rejected = true;
                partial = is_partial(&result);
                methods = remaining_methods(result);
            }
            let instructions = if saved_rejected {
                "已保存的密码被服务器拒绝，请重新输入"
            } else {
                "请输入登录密码"
            };
            if !partial && methods.contains(&MethodKind::Password) {
                for _ in 0..AUTH_RETRIES {
                    let answer = ask_one_secret(broker, "SSH 登录", instructions, "密码").await?;
                    let result = handle
                        .authenticate_password(&config.user, answer.into_inner())
                        .await
                        .map_err(|_| anyhow!("密码认证失败"))?;
                    if result.success() {
                        return Ok(());
                    }
                    partial = is_partial(&result);
                    methods = remaining_methods(result);
                    if partial || !methods.contains(&MethodKind::Password) {
                        break;
                    }
                }
            }
        }
        if methods.contains(&MethodKind::KeyboardInteractive) {
            for _ in 0..AUTH_RETRIES {
                if keyboard_interactive(handle, &config.user, broker).await? {
                    return Ok(());
                }
            }
        }
    }
    bail!("认证失败：服务器未接受可用的认证方式")
}

async fn try_agent<H: client::Handler>(
    handle: &mut client::Handle<H>,
    user: &str,
) -> Result<Option<AuthResult>>
where
    H::Error: From<russh::Error>,
{
    #[cfg(unix)]
    {
        let Ok(mut agent) = AgentClient::connect_env().await else {
            return Ok(None);
        };
        let Ok(identities) = agent.request_identities().await else {
            return Ok(None);
        };
        let hash = handle
            .best_supported_rsa_hash()
            .await
            .map_err(|_| anyhow!("无法协商 SSH Agent 签名算法"))?
            .flatten();
        let mut last = None;
        for identity in identities {
            let key = identity.public_key().into_owned();
            match handle
                .authenticate_publickey_with(user, key, hash, &mut agent)
                .await
            {
                Ok(result) if result.success() => return Ok(Some(result)),
                Ok(result) => last = Some(result),
                Err(_) => continue,
            }
        }
        Ok(last)
    }
    #[cfg(not(unix))]
    {
        let _ = (handle, user);
        Ok(None)
    }
}

fn key_paths(config: &SshConnectionConfig) -> Result<Vec<PathBuf>> {
    if config.auth == AuthKind::Key {
        return config
            .key_path
            .clone()
            .map(|path| vec![path])
            .ok_or_else(|| anyhow!("私钥认证需要选择私钥文件"));
    }
    let Some(home) = dirs::home_dir() else {
        return Ok(Vec::new());
    };
    Ok(["id_ed25519", "id_ecdsa", "id_rsa"]
        .into_iter()
        .map(|name| home.join(".ssh").join(name))
        .collect())
}

async fn load_private_key(
    path: &Path,
    secrets: &SharedSecretStore,
    broker: &PromptBroker,
) -> Result<Option<russh::keys::PrivateKey>> {
    if !path.exists() {
        return Ok(None);
    }
    match load_secret_key(path, None) {
        Ok(key) => return Ok(Some(key)),
        Err(russh::keys::Error::KeyIsEncrypted) => {}
        Err(_) => bail!("无法读取私钥文件：{}", path.display()),
    }
    // Passphrases are saved per key file, so one saved answer unlocks the same
    // key for every session that uses it.
    let mut saved_rejected = false;
    if let Some(saved) = saved_secret(secrets, &SecretRef::passphrase(path)) {
        if let Ok(key) = load_secret_key(path, Some(saved.as_str())) {
            return Ok(Some(key));
        }
        saved_rejected = true;
    }
    let instructions = if saved_rejected {
        format!("已保存的口令无法解开 {}，请重新输入", path.display())
    } else {
        format!("请输入 {} 的口令", path.display())
    };
    for _ in 0..AUTH_RETRIES {
        let answer = ask_one_secret(broker, "私钥口令", &instructions, "口令").await?;
        if let Ok(key) = load_secret_key(path, Some(answer.expose())) {
            return Ok(Some(key));
        }
    }
    bail!("私钥口令错误次数过多")
}

/// Where this connection's password lives in the system keychain.
fn password_secret(config: &SshConnectionConfig) -> SecretRef {
    SecretRef::password(&config.user, &config.host, config.port)
}

/// Read a saved secret. A keychain that errors, is locked, or holds an empty
/// value counts as nothing saved: the connection then falls back to asking,
/// which is always better than refusing to connect.
///
/// This blocks. It only ever runs on the SSH worker thread, whose runtime
/// serves this one connection, and only while authentication is already
/// waiting on a person.
fn saved_secret(secrets: &SharedSecretStore, secret: &SecretRef) -> Option<Zeroizing<String>> {
    secrets
        .get(secret)
        .ok()
        .flatten()
        .filter(|value| !value.is_empty())
}

async fn ask_one_secret(
    broker: &PromptBroker,
    title: &str,
    instructions: &str,
    label: &str,
) -> Result<crate::terminal::TerminalSecret> {
    let reply = broker
        .ask(TerminalPromptKind::authentication(
            title,
            instructions,
            vec![TerminalPromptField::new(label, false)],
        ))
        .await?;
    match reply {
        TerminalPromptReply::Answers(mut answers) if answers.len() == 1 => Ok(answers.remove(0)),
        TerminalPromptReply::Cancel => bail!("认证已取消"),
        _ => bail!("认证回复无效"),
    }
}

async fn keyboard_interactive<H: client::Handler>(
    handle: &mut client::Handle<H>,
    user: &str,
    broker: &PromptBroker,
) -> Result<bool>
where
    H::Error: From<russh::Error>,
{
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|_| anyhow!("无法开始交互式认证"))?;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let fields = prompts
                    .into_iter()
                    .map(|prompt| TerminalPromptField::new(prompt.prompt, prompt.echo))
                    .collect();
                let reply = broker
                    .ask(TerminalPromptKind::authentication(
                        name,
                        instructions,
                        fields,
                    ))
                    .await?;
                let TerminalPromptReply::Answers(answers) = reply else {
                    bail!("交互式认证已取消")
                };
                response = handle
                    .authenticate_keyboard_interactive_respond(
                        answers
                            .into_iter()
                            .map(|answer| answer.into_inner())
                            .collect(),
                    )
                    .await
                    .map_err(|_| anyhow!("交互式认证失败"))?;
            }
        }
    }
}

fn remaining_methods(result: AuthResult) -> MethodSet {
    match result {
        AuthResult::Success => MethodSet::empty(),
        AuthResult::Failure {
            remaining_methods, ..
        } => remaining_methods,
    }
}

fn is_partial(result: &AuthResult) -> bool {
    matches!(
        result,
        AuthResult::Failure {
            partial_success: true,
            ..
        }
    )
}

fn safe_connect_error(_: &anyhow::Error) -> &'static str {
    "无法建立 SSH 连接，请检查主机、端口和主机密钥"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::{InMemorySecretStore, NoSecretStore, SecretStore as _};
    use crate::terminal::TerminalSecret;
    use russh::server::{self, Server as _};
    use std::borrow::Cow;

    const TEST_PASSWORD: &str = "test-password";

    #[derive(Default)]
    struct ServerState {
        pty: Option<(String, u32, u32, u32, u32)>,
        resize: Option<(u32, u32, u32, u32)>,
        input: Vec<u8>,
    }

    #[derive(Clone)]
    struct TestServer {
        state: Arc<Mutex<ServerState>>,
        auth: TestAuth,
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

    fn start_server(auth: TestAuth) -> Option<RunningTestServer> {
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
        connect_with_secrets(session, known_hosts, Arc::new(NoSecretStore), answer);
    }

    /// Connect, answer whatever is asked, then shut down. Returns the prompts
    /// that were raised, so a test can assert that none of them appeared.
    fn connect_with_secrets(
        session: Session,
        known_hosts: &Path,
        secrets: SharedSecretStore,
        mut answer: impl FnMut(&TerminalPromptKind) -> TerminalPromptReply,
    ) -> Vec<TerminalPromptKind> {
        let mut asked = Vec::new();
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
                    asked.push(prompt.kind().clone());
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
                TerminalTransportEvent::Output(_) => {}
            }
        }
        command_tx.send(TerminalTransportCommand::Shutdown).unwrap();
        done_rx
            .recv_timeout(SHUTDOWN_TIMEOUT + Duration::from_secs(1))
            .expect("SSH worker did not stop in time")
            .unwrap();
        worker.join().unwrap();
        asked
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

        let asked = connect_with_secrets(session, &known_hosts, secrets, |kind| match kind {
            TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
            other => panic!("unexpected prompt: {other:?}"),
        });

        assert!(
            !asked
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

        let asked =
            connect_with_secrets(session, &known_hosts, secrets.clone(), |kind| match kind {
                TerminalPromptKind::UnknownHost(_) => TerminalPromptReply::TrustAndSave,
                TerminalPromptKind::Authentication(_) => {
                    TerminalPromptReply::Answers(vec![TerminalSecret::new(TEST_PASSWORD)])
                }
                other => panic!("unexpected prompt: {other:?}"),
            });

        let instructions = asked
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
    fn malformed_known_hosts_is_blocked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(&path, "this is not a key\n").unwrap();
        let error = read_known_keys("example.test", 22, &path).unwrap_err();
        assert!(error.to_string().contains("已损坏"));
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
