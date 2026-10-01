//! SSH connection and authentication shared by terminals and SFTP.
use crate::{
    connection::{
        ConnectionPrompt, ConnectionPromptField, ConnectionPromptKind, ConnectionPromptReply,
    },
    host::{HostLogin, JumpLogin, LoginMethod, LoginRoute, ProxyLogin},
    secrets::{SecretRef, SharedSecretStore},
};
use anyhow::{Context as _, Result, anyhow, bail};
use russh::keys::{
    HashAlg, PrivateKeyWithHashAlg, PublicKey,
    known_hosts::{known_host_keys_path, learn_known_hosts_path},
    load_secret_key, parse_public_key_base64,
};
use russh::{
    MethodKind, MethodSet,
    client::{self, AuthResult, KeyboardInteractiveAuthResponse},
};
use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::Zeroizing;

use super::proxy::{ProxyAuth, handshake};
use super::tester::describe_login_error;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const AUTH_RETRIES: usize = 3;
/// How long to wait for the agent to answer the door. A named pipe that
/// stays busy would otherwise be retried forever.
const AGENT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_FAILED: &str = "无法建立 SSH 连接，请检查地址、端口和主机密钥";
static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

/// What one connection logs in with: a host's login as the store resolved
/// it on the UI thread, credential and all.
#[derive(Clone)]
pub struct SshConnectionConfig {
    login: HostLogin,
}

impl From<&HostLogin> for SshConnectionConfig {
    fn from(login: &HostLogin) -> Self {
        Self {
            login: login.clone(),
        }
    }
}

/// Where the SSH agent listens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AgentLocation {
    /// The user's own agent: the socket `SSH_AUTH_SOCK` names on macOS and
    /// Linux, the OpenSSH agent service's pipe on Windows.
    #[default]
    System,
    /// An agent at this socket or pipe. Tests use it rather than change the
    /// process's environment under other tests.
    At(PathBuf),
}

/// Cloneable trust and credential service. Every clone shares the trust-file lock.
#[derive(Clone)]
pub struct SshConnector {
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    secrets: SharedSecretStore,
    agent: AgentLocation,
}
impl SshConnector {
    pub fn new(path: impl Into<PathBuf>, secrets: SharedSecretStore) -> Self {
        Self {
            known_hosts_path: path.into(),
            known_hosts_lock: Arc::new(Mutex::new(())),
            secrets,
            agent: AgentLocation::System,
        }
    }

    /// Use the agent at `agent` instead of the user's own.
    pub fn with_agent(mut self, agent: AgentLocation) -> Self {
        self.agent = agent;
        self
    }
    /// Must run on a worker: keychain and private-key reads are blocking.
    pub async fn connect(
        &self,
        config: &SshConnectionConfig,
        broker: Arc<SshPrompts>,
    ) -> Result<(client::Handle<SshClientHandler>, String)> {
        self.connect_with(config, broker, &self.secrets).await
    }

    /// Same as [`Self::connect`], for a connection that carries port
    /// forwards. The receiver yields every channel the server opens for a
    /// remote forward, and ends when the connection does: the handler that
    /// feeds it lives exactly as long as the connection, so this is how an
    /// idle forward learns that its connection is gone.
    pub async fn connect_forwarding(
        &self,
        config: &SshConnectionConfig,
        broker: Arc<SshPrompts>,
    ) -> Result<(SshHandle, String, mpsc::UnboundedReceiver<ForwardedTcpip>)> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let (handle, fingerprint) = self
            .connect_inner(config, broker, &self.secrets, Some(sender))
            .await?;
        Ok((handle, fingerprint, receiver))
    }

    /// The keychain this connector reads saved secrets from.
    pub(super) fn secrets(&self) -> &SharedSecretStore {
        &self.secrets
    }

    /// Same as [`Self::connect`], with saved secrets read from `secrets`.
    pub(super) async fn connect_with(
        &self,
        config: &SshConnectionConfig,
        broker: Arc<SshPrompts>,
        secrets: &SharedSecretStore,
    ) -> Result<(client::Handle<SshClientHandler>, String)> {
        self.connect_inner(config, broker, secrets, None).await
    }

    async fn connect_inner(
        &self,
        config: &SshConnectionConfig,
        broker: Arc<SshPrompts>,
        secrets: &SharedSecretStore,
        forwarded: Option<mpsc::UnboundedSender<ForwardedTcpip>>,
    ) -> Result<(client::Handle<SshClientHandler>, String)> {
        let login = &config.login;
        let tunnel = match &login.route {
            LoginRoute::Direct => {
                Box::new(step(&broker, tcp(&login.host, login.port)).await?) as Tunnel
            }
            LoginRoute::Proxy(proxy) => {
                Box::new(step(&broker, through_proxy(proxy, login, secrets)).await?)
            }
            LoginRoute::Jump(hops) => self.through_jumps(hops, login, &broker, secrets).await?,
        };
        self.log_in(tunnel, login, None, &broker, secrets, forwarded)
            .await
    }

    /// Open the SSH connection to `login`'s host over `tunnel` and log in.
    /// `jump_host` names a jump host, for its questions.
    async fn log_in(
        &self,
        tunnel: Tunnel,
        login: &HostLogin,
        jump_host: Option<&str>,
        broker: &Arc<SshPrompts>,
        secrets: &SharedSecretStore,
        forwarded: Option<mpsc::UnboundedSender<ForwardedTcpip>>,
    ) -> Result<(SshHandle, String)> {
        let fingerprint = Arc::new(Mutex::new(String::new()));
        let handler = SshClientHandler {
            host: login.host.clone(),
            port: login.port,
            jump_host: jump_host.map(str::to_string),
            known_hosts_path: self.known_hosts_path.clone(),
            known_hosts_lock: self.known_hosts_lock.clone(),
            broker: broker.clone(),
            fingerprint: fingerprint.clone(),
            forwarded,
        };
        let connect = async {
            client::connect_stream(ssh_config(), tunnel, handler)
                .await
                .map_err(|error| error.context(CONNECT_FAILED))
        };
        let mut handle = step(broker, connect).await?;
        let prompts = Asker {
            prompts: broker,
            jump_host,
        };
        let mut shutdown = broker.shutdown_receiver();
        tokio::select! {
            result = authenticate(&mut handle, login, secrets, &self.agent, prompts) => result?,
            _ = shutdown.changed() => bail!("连接已取消"),
        }
        let fingerprint = lock(&fingerprint).clone();
        Ok((handle, fingerprint))
    }

    /// A stream to `login`'s host through `hops`, each logged in to over the
    /// one before. A failure says which jump host it was at.
    async fn through_jumps(
        &self,
        hops: &[JumpLogin],
        login: &HostLogin,
        broker: &Arc<SshPrompts>,
        secrets: &SharedSecretStore,
    ) -> Result<Tunnel> {
        // Nothing is connected while the way is known to be broken.
        let hosts = hops
            .iter()
            .enumerate()
            .map(|(ix, hop)| match hop {
                JumpLogin::Host { name, login } => Ok((name.as_str(), &**login)),
                JumpLogin::Deleted => Err(anyhow!(RouteFailure(format!(
                    "第 {} 台跳板主机已被删除，请编辑这台主机的连接方式",
                    ix + 1
                )))),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut tunnel = None;
        for (ix, (name, hop)) in hosts.iter().enumerate() {
            let (next_host, next_port) = hosts
                .get(ix + 1)
                .map_or((login.host.as_str(), login.port), |(_, next)| {
                    (next.host.as_str(), next.port)
                });
            let reached = self
                .jump(
                    tunnel.take(),
                    name,
                    hop,
                    (next_host, next_port),
                    broker,
                    secrets,
                )
                .await
                .map_err(|error| {
                    if error.downcast_ref::<RouteFailure>().is_some() {
                        return error;
                    }
                    let reason = describe_login_error(&error);
                    error.context(RouteFailure(format!("跳板主机「{name}」：{reason}")))
                })?;
            tunnel = Some(reached);
        }
        tunnel.ok_or_else(|| anyhow!("没有可用的跳板主机"))
    }

    /// Log in to the jump host `hop`, over `tunnel` or else directly, and
    /// open a channel through it to `next`.
    ///
    /// The jump host's connection lives as long as that channel does: its
    /// handle goes here, and the channel's stream keeps the connection's
    /// task running until the connection over it ends.
    async fn jump(
        &self,
        tunnel: Option<Tunnel>,
        name: &str,
        hop: &HostLogin,
        next: (&str, u16),
        broker: &Arc<SshPrompts>,
        secrets: &SharedSecretStore,
    ) -> Result<Tunnel> {
        let tunnel = match tunnel {
            Some(tunnel) => tunnel,
            None => Box::new(step(broker, tcp(&hop.host, hop.port)).await?),
        };
        let (handle, _) = self
            .log_in(tunnel, hop, Some(name), broker, secrets, None)
            .await?;
        let (next_host, next_port) = next;
        let open = async {
            handle
                .channel_open_direct_tcpip(next_host, u32::from(next_port), "127.0.0.1", 0)
                .await
                .map_err(|error| {
                    let reason = match &error {
                        russh::Error::ChannelOpenFailure(
                            russh::ChannelOpenFailure::ConnectFailed,
                        ) => "连接失败",
                        russh::Error::ChannelOpenFailure(
                            russh::ChannelOpenFailure::AdministrativelyProhibited,
                        ) => "服务器不允许端口转发",
                        _ => "无法打开转发通道",
                    };
                    let text =
                        format!("跳板主机「{name}」无法连接到 {next_host}:{next_port}：{reason}");
                    // Refused like a direct connection would be, so it is
                    // retried like one.
                    let refused = matches!(
                        error,
                        russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ConnectFailed)
                    );
                    let error = if refused {
                        anyhow::Error::from(std::io::Error::new(
                            std::io::ErrorKind::ConnectionRefused,
                            text.clone(),
                        ))
                    } else {
                        anyhow::Error::from(error)
                    };
                    error.context(RouteFailure(text))
                })
        };
        let channel = step(broker, open).await?;
        Ok(Box::new(channel.into_stream()))
    }
}

/// What carries a connection to the host: a socket, or a channel through a
/// jump host.
type Tunnel = Box<dyn TunnelStream>;

trait TunnelStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> TunnelStream for T {}

fn ssh_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    })
}

/// One step on the way to a host: given up after `CONNECT_TIMEOUT`, not
/// counting time a person spends answering, or when the connection is
/// cancelled.
async fn step<T>(broker: &SshPrompts, future: impl Future<Output = Result<T>>) -> Result<T> {
    let mut shutdown = broker.shutdown_receiver();
    tokio::select! {
        result = timeout_excluding_prompts(future, broker.prompt_activity_receiver(), CONNECT_TIMEOUT) => result?,
        _ = shutdown.changed() => bail!("连接已取消"),
    }
}

/// A socket to `host:port`, failing the way russh's own connect does: a
/// name that does not resolve then counts as the network failing.
async fn tcp(host: &str, port: u16) -> Result<TcpStream> {
    let socket = TcpStream::connect((host, port))
        .await
        .map_err(russh::Error::IO)
        .context(CONNECT_FAILED)?;
    // As russh's own connect does: the terminal's keystrokes go out at once.
    let _ = socket.set_nodelay(true);
    Ok(socket)
}

/// A stream to `login`'s host through `proxy`.
async fn through_proxy(
    proxy: &ProxyLogin,
    login: &HostLogin,
    secrets: &SharedSecretStore,
) -> Result<TcpStream> {
    let mut socket = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .map_err(|error| {
            let error = anyhow::Error::from(russh::Error::IO(error));
            let reason = describe_login_error(&error);
            error.context(RouteFailure(format!(
                "无法连接代理服务器 {}:{}：{reason}",
                proxy.host, proxy.port
            )))
        })?;
    let _ = socket.set_nodelay(true);
    let password = proxy
        .password()
        .and_then(|secret| saved_secret(secrets, &secret));
    let auth = proxy.user.as_deref().map(|user| ProxyAuth {
        user,
        password: password.as_deref().map_or("", String::as_str),
    });
    handshake(&mut socket, proxy.kind, &login.host, login.port, auth)
        .await
        .map_err(|error| {
            let text = error.to_string();
            error.context(RouteFailure(text))
        })?;
    Ok(socket)
}

/// What went wrong on the way to a host, at a jump host or at the proxy, in
/// words that say where. The error underneath stays in the chain, so a
/// network failure on the way is retried like any other and a missing
/// password is still recognised.
#[derive(Debug)]
pub(super) struct RouteFailure(pub(super) String);

impl std::fmt::Display for RouteFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RouteFailure {}

/// The questions of one host on the way. A jump host's say which one: the
/// host the user opened is a different one.
#[derive(Clone, Copy)]
struct Asker<'a> {
    prompts: &'a SshPrompts,
    jump_host: Option<&'a str>,
}

impl Asker<'_> {
    async fn ask_credential(
        &self,
        need: MissingCredential,
        kind: ConnectionPromptKind,
    ) -> Result<ConnectionPromptReply> {
        self.prompts
            .ask_credential(need, kind.at_jump_host(self.jump_host))
            .await
    }
}
impl SshConnectionConfig {
    pub fn endpoint(&self) -> String {
        self.login.endpoint()
    }
}

pub struct SshPrompts {
    pending: Mutex<HashMap<u64, oneshot::Sender<ConnectionPromptReply>>>,
    events: Arc<dyn Fn(ConnectionPrompt) -> bool + Send + Sync>,
    shutdown: watch::Receiver<bool>,
    prompt_activity: watch::Sender<bool>,
    /// Whether a password, passphrase or keyboard-interactive answer may be
    /// asked of the user. When not, such a need fails the connection with a
    /// [`MissingCredential`] instead.
    interactive: bool,
}

/// A credential authentication needed and could not ask for, because the
/// connection runs without anyone to answer (a connection test, or a command
/// from the external CLI).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingCredential {
    /// `rejected` when a password was tried and the server refused it.
    Password {
        rejected: bool,
    },
    /// `rejected` when a passphrase was tried and did not unlock the key.
    Passphrase {
        rejected: bool,
    },
    KeyboardInteractive,
}

impl std::fmt::Display for MissingCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MissingCredential::Password { rejected: true } => "用户名或密码错误",
            MissingCredential::Password { rejected: false } => "未填写密码",
            MissingCredential::Passphrase { rejected: true } => "私钥口令错误",
            MissingCredential::Passphrase { rejected: false } => "私钥已加密，未填写口令",
            MissingCredential::KeyboardInteractive => "服务器要求键盘交互式认证，无法在测试中完成",
        })
    }
}

impl std::error::Error for MissingCredential {}

impl SshPrompts {
    pub fn new(
        events: Arc<dyn Fn(ConnectionPrompt) -> bool + Send + Sync>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        let (prompt_activity, _) = watch::channel(false);
        Self {
            pending: Mutex::new(HashMap::new()),
            events,
            shutdown,
            prompt_activity,
            interactive: true,
        }
    }

    /// Fail on a credential need instead of asking; see [`MissingCredential`].
    pub fn non_interactive(mut self) -> Self {
        self.interactive = false;
        self
    }

    pub fn shutdown_receiver(&self) -> watch::Receiver<bool> {
        self.shutdown.clone()
    }

    pub(super) fn prompt_activity_receiver(&self) -> watch::Receiver<bool> {
        self.prompt_activity.subscribe()
    }

    async fn ask(&self, kind: ConnectionPromptKind) -> Result<ConnectionPromptReply> {
        let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(request_id, sender);
        let _ = self.prompt_activity.send(true);
        let result = async {
            if !(self.events)(ConnectionPrompt::new(request_id, kind)) {
                bail!("连接视图已关闭");
            }
            let mut shutdown = self.shutdown.clone();
            tokio::select! {
                reply = receiver => reply.map_err(|_| anyhow!("认证请求已取消")),
                _ = shutdown.changed() => bail!("连接已取消"),
            }
        }
        .await;
        lock(&self.pending).remove(&request_id);
        let _ = self.prompt_activity.send(false);
        result
    }

    /// Ask for a credential, or fail with `need` when nobody may be asked.
    async fn ask_credential(
        &self,
        need: MissingCredential,
        kind: ConnectionPromptKind,
    ) -> Result<ConnectionPromptReply> {
        if !self.interactive {
            return Err(need.into());
        }
        self.ask(kind).await
    }

    fn emit(&self, kind: ConnectionPromptKind) {
        let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        (self.events)(ConnectionPrompt::new(request_id, kind));
    }

    pub fn respond(&self, request_id: u64, reply: ConnectionPromptReply) {
        if let Some(sender) = lock(&self.pending).remove(&request_id) {
            let _ = sender.send(reply);
        }
    }

    pub fn cancel_all(&self) {
        lock(&self.pending).clear();
    }
}

/// Apply the network handshake timeout without counting time spent waiting
/// for an explicit answer from the user to a host-trust prompt.
pub(super) async fn timeout_excluding_prompts<F>(
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
            _ = tokio::time::sleep(remaining) => return Err(russh::Error::ConnectionTimeout.into()),
            changed = prompt_activity.changed() => {
                if changed.is_err() {
                    bail!("连接已取消");
                }
                remaining = remaining.saturating_sub(started.elapsed());
                if remaining.is_zero() && !*prompt_activity.borrow() {
                    return Err(russh::Error::ConnectionTimeout.into());
                }
            }
        }
    }
}

pub type SshHandle = client::Handle<SshClientHandler>;

/// Lock a mutex that a panicking thread may have poisoned; the data it guards
/// here stays usable either way.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// A connection the server opened back to this client for a remote forward
/// (`ssh -R`): someone connected to the port the server listens on for us.
#[non_exhaustive]
pub struct ForwardedTcpip {
    pub channel: russh::Channel<client::Msg>,
    /// Accepts or refuses the channel. Dropping it refuses.
    pub reply: client::ChannelOpenHandle,
    /// The address and port on the server that was connected to.
    pub connected_address: String,
    pub connected_port: u32,
    pub originator_address: String,
    pub originator_port: u32,
}

pub struct SshClientHandler {
    host: String,
    port: u16,
    /// The jump host's name, when this connection is to one.
    jump_host: Option<String>,
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    broker: Arc<SshPrompts>,
    fingerprint: Arc<Mutex<String>>,
    /// Where channels for a remote forward go. Only a forwarding connection
    /// has one; every other connection refuses such channels.
    forwarded: Option<mpsc::UnboundedSender<ForwardedTcpip>>,
}

impl client::Handler for SshClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        *lock(&self.fingerprint) = fingerprint.clone();
        let algorithm = key.algorithm().to_string();
        let known = {
            let _guard = lock(&self.known_hosts_lock);
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
            self.broker.emit(
                ConnectionPromptKind::host_key_changed(
                    &self.host,
                    self.port,
                    algorithm,
                    old,
                    fingerprint,
                    &self.known_hosts_path,
                )
                .at_jump_host(self.jump_host.as_deref()),
            );
            return Ok(false);
        }

        let reply = self
            .broker
            .ask(
                ConnectionPromptKind::unknown_host(&self.host, self.port, algorithm, fingerprint)
                    .at_jump_host(self.jump_host.as_deref()),
            )
            .await?;
        if !matches!(reply, ConnectionPromptReply::TrustAndSave) {
            return Ok(false);
        }
        let _guard = lock(&self.known_hosts_lock);
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

    /// The server only opens these after a `tcpip-forward` request, which
    /// nothing but a forwarding connection sends. One that arrives anyway is
    /// refused rather than accepted and left dangling.
    #[allow(clippy::too_many_arguments)]
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: russh::Channel<client::Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<(), Self::Error> {
        match &self.forwarded {
            // The forward decides: it accepts once it has reached its target.
            // Should it be gone already, the undelivered `reply` is dropped
            // with the message, which refuses the channel.
            Some(forwarded) => {
                let _ = forwarded.send(ForwardedTcpip {
                    channel,
                    reply,
                    connected_address: connected_address.to_string(),
                    connected_port,
                    originator_address: originator_address.to_string(),
                    originator_port,
                });
            }
            None => {
                reply
                    .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                    .await
            }
        }
        Ok(())
    }
}

/// Whether an error means the network or the connection failed, as opposed
/// to the server refusing a login or the user declining a host key. Only the
/// first kind is worth retrying without anyone looking.
pub fn is_network_error(error: &anyhow::Error) -> bool {
    use std::io::ErrorKind;
    error.chain().any(|cause| {
        // A bare I/O error may also be a file that could not be read (the
        // trust file, a private key), so only the socket's kinds count.
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                ErrorKind::NotConnected
                    | ErrorKind::ConnectionRefused
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::TimedOut
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::BrokenPipe
                    | ErrorKind::HostUnreachable
                    | ErrorKind::NetworkUnreachable
                    | ErrorKind::NetworkDown
            )
        }) || matches!(
            cause.downcast_ref::<russh::Error>(),
            Some(
                // russh wraps the socket's error instead of chaining it, and
                // a name that no longer resolves arrives the same way.
                russh::Error::IO(_)
                    | russh::Error::Disconnect
                    | russh::Error::HUP
                    | russh::Error::ConnectionTimeout
                    | russh::Error::KeepaliveTimeout
                    | russh::Error::InactivityTimeout
                    | russh::Error::SendError
            )
        )
    })
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

async fn authenticate(
    handle: &mut SshHandle,
    login: &HostLogin,
    secrets: &SharedSecretStore,
    agent: &AgentLocation,
    broker: Asker<'_>,
) -> Result<()> {
    let user = login.user.as_str();
    let method = login.method;
    let first = handle
        .authenticate_none(user)
        .await
        .map_err(|_| anyhow!("无法查询服务器支持的认证方式"))?;
    if first.success() {
        return Ok(());
    }
    let mut methods = remaining_methods(first);
    let mut partial = false;

    // A login without a password tries the agent quietly and moves on; a
    // login that is only the agent says what went wrong with it.
    if matches!(method, LoginMethod::NoPassword | LoginMethod::Agent) {
        if methods.contains(&MethodKind::PublicKey) {
            match try_agent(handle, user, agent).await? {
                Ok(result) => {
                    if result.success() {
                        return Ok(());
                    }
                    partial = is_partial(&result);
                    methods = remaining_methods(result);
                }
                Err(problem) if method == LoginMethod::Agent => bail!(problem.to_string()),
                Err(_) => {}
            }
            if method == LoginMethod::Agent && !partial {
                bail!("服务器未接受 SSH Agent 中的密钥");
            }
        } else if method == LoginMethod::Agent {
            bail!("服务器不接受公钥登录，无法使用 SSH Agent");
        }
    }

    if matches!(method, LoginMethod::NoPassword | LoginMethod::Key)
        && methods.contains(&MethodKind::PublicKey)
    {
        let paths = key_paths(login)?;
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
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
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
        if method == LoginMethod::Key && !partial {
            bail!("服务器未接受指定的私钥");
        }
    }

    // Nothing typed is all a login without a password has. A server that
    // wants more is told no here, not answered with a question; a code it
    // asks for after a key is still asked, below.
    if method == LoginMethod::NoPassword && !partial {
        if methods.contains(&MethodKind::Password)
            || methods.contains(&MethodKind::KeyboardInteractive)
        {
            bail!("服务器要求密码，「无密码」不会询问；请改用「密码」或「使用凭据」");
        }
        bail!("服务器未接受 SSH Agent 和 ~/.ssh 中的任何私钥");
    }

    if method == LoginMethod::Password || partial {
        if methods.contains(&MethodKind::Password) {
            // Try what the host has saved before bothering anyone.
            let mut saved_rejected = false;
            if let Some(saved) = saved_secret(secrets, &login.password) {
                let result = handle
                    .authenticate_password(user, saved.to_string())
                    .await
                    .map_err(|_| anyhow!("密码认证失败"))?;
                if result.success() {
                    return Ok(());
                }
                // The entry stays: the user typed it into the host dialog,
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
                    let answer = ask_one_secret(
                        broker,
                        MissingCredential::Password {
                            rejected: saved_rejected,
                        },
                        "SSH 登录",
                        instructions,
                        "密码",
                    )
                    .await?;
                    let result = handle
                        .authenticate_password(user, answer.into_inner())
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
                if keyboard_interactive(handle, user, broker).await? {
                    return Ok(());
                }
            }
        }
    }
    bail!("认证失败：服务器未接受可用的认证方式")
}

type Agent = russh::keys::agent::client::AgentClient<
    Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin>,
>;

/// Why the agent's keys could not be offered at all. `Display` is the text a
/// person reads. The cause is folded into the words rather than chained, so
/// a missing agent is never mistaken for a network failure worth retrying.
#[derive(Debug)]
enum AgentProblem {
    Unreachable(String),
    Empty,
    /// Every key failed to sign: a locked agent, or one that asked to
    /// confirm and was told no.
    Unsigned,
}

impl std::fmt::Display for AgentProblem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentProblem::Unreachable(reason) => write!(formatter, "无法连接 SSH Agent：{reason}"),
            AgentProblem::Empty => formatter.write_str("SSH Agent 中没有密钥，请先用 ssh-add 添加"),
            AgentProblem::Unsigned => formatter.write_str("SSH Agent 拒绝了签名请求"),
        }
    }
}

/// Offer the agent's keys one after another. The result is the server's
/// answer to the last key offered: a success, or the refusal that says what
/// the server will take next.
async fn try_agent(
    handle: &mut SshHandle,
    user: &str,
    location: &AgentLocation,
) -> Result<Result<AuthResult, AgentProblem>> {
    let mut agent = match connect_agent(location).await {
        Ok(agent) => agent,
        Err(problem) => return Ok(Err(problem)),
    };
    let identities = match agent.request_identities().await {
        Ok(identities) if identities.is_empty() => return Ok(Err(AgentProblem::Empty)),
        Ok(identities) => identities,
        Err(error) => return Ok(Err(AgentProblem::Unreachable(error.to_string()))),
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
            Ok(result) if result.success() => return Ok(Ok(result)),
            Ok(result) => last = Some(result),
            Err(_) => continue,
        }
    }
    Ok(last.ok_or(AgentProblem::Unsigned))
}

#[cfg(unix)]
async fn connect_agent(location: &AgentLocation) -> Result<Agent, AgentProblem> {
    use russh::keys::agent::client::AgentClient;

    let path = match location {
        AgentLocation::At(path) => path.clone(),
        AgentLocation::System => match std::env::var_os("SSH_AUTH_SOCK") {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => {
                return Err(AgentProblem::Unreachable(
                    "没有设置 SSH_AUTH_SOCK".to_string(),
                ));
            }
        },
    };
    match tokio::time::timeout(AGENT_TIMEOUT, AgentClient::connect_uds(&path)).await {
        Ok(Ok(agent)) => Ok(agent.dynamic()),
        Ok(Err(russh::keys::Error::IO(error))) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(AgentProblem::Unreachable(format!(
                "{} 不存在",
                path.display()
            )))
        }
        Ok(Err(error)) => Err(AgentProblem::Unreachable(error.to_string())),
        Err(_) => Err(AgentProblem::Unreachable("没有响应".to_string())),
    }
}

#[cfg(windows)]
async fn connect_agent(location: &AgentLocation) -> Result<Agent, AgentProblem> {
    use russh::keys::agent::client::AgentClient;

    /// Where the OpenSSH Authentication Agent service listens.
    const SYSTEM_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
    let pipe = match location {
        AgentLocation::At(path) => path.as_os_str().to_owned(),
        AgentLocation::System => SYSTEM_PIPE.into(),
    };
    match tokio::time::timeout(AGENT_TIMEOUT, AgentClient::connect_named_pipe(&pipe)).await {
        Ok(Ok(agent)) => Ok(agent.dynamic()),
        Ok(Err(russh::keys::Error::IO(error))) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(AgentProblem::Unreachable(
                "OpenSSH Authentication Agent 服务没有运行".to_string(),
            ))
        }
        Ok(Err(error)) => Err(AgentProblem::Unreachable(error.to_string())),
        Err(_) => Err(AgentProblem::Unreachable("没有响应".to_string())),
    }
}

#[cfg(not(any(unix, windows)))]
async fn connect_agent(location: &AgentLocation) -> Result<Agent, AgentProblem> {
    let _ = location;
    Err(AgentProblem::Unreachable("这个平台不支持".to_string()))
}

fn key_paths(login: &HostLogin) -> Result<Vec<PathBuf>> {
    if login.method == LoginMethod::Key {
        return login
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
    broker: Asker<'_>,
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
    // key for every host that uses it.
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
        let answer = ask_one_secret(
            broker,
            MissingCredential::Passphrase {
                rejected: saved_rejected,
            },
            "私钥口令",
            &instructions,
            "口令",
        )
        .await?;
        if let Ok(key) = load_secret_key(path, Some(answer.expose())) {
            return Ok(Some(key));
        }
    }
    bail!("私钥口令错误次数过多")
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
    broker: Asker<'_>,
    need: MissingCredential,
    title: &str,
    instructions: &str,
    label: &str,
) -> Result<crate::connection::ConnectionSecret> {
    let reply = broker
        .ask_credential(
            need,
            ConnectionPromptKind::authentication(
                title,
                instructions,
                vec![ConnectionPromptField::new(label, false)],
            ),
        )
        .await?;
    match reply {
        ConnectionPromptReply::Answers(mut answers) if answers.len() == 1 => Ok(answers.remove(0)),
        ConnectionPromptReply::Cancel => bail!("认证已取消"),
        _ => bail!("认证回复无效"),
    }
}

async fn keyboard_interactive(
    handle: &mut SshHandle,
    user: &str,
    broker: Asker<'_>,
) -> Result<bool> {
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
                    .map(|prompt| ConnectionPromptField::new(prompt.prompt, prompt.echo))
                    .collect();
                let reply = broker
                    .ask_credential(
                        MissingCredential::KeyboardInteractive,
                        ConnectionPromptKind::authentication(name, instructions, fields),
                    )
                    .await?;
                let ConnectionPromptReply::Answers(answers) = reply else {
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

#[cfg(test)]
mod tests {
    use super::read_known_keys;
    #[test]
    fn malformed_known_hosts_is_blocked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(&path, "this is not a key\n").unwrap();
        let error = read_known_keys("example.test", 22, &path).unwrap_err();
        assert!(error.to_string().contains("已损坏"));
    }

    #[test]
    fn only_a_failing_network_counts_as_a_network_error() {
        use super::{MissingCredential, is_network_error};
        use std::io::{Error, ErrorKind};

        // What a refused or unresolvable connect looks like coming out of
        // russh: the socket's error wrapped, under the connector's context.
        let refused =
            anyhow::Error::from(russh::Error::IO(Error::from(ErrorKind::ConnectionRefused)))
                .context("无法建立 SSH 连接");
        assert!(is_network_error(&refused));
        let unresolved = anyhow::Error::from(russh::Error::IO(Error::other("no such host")));
        assert!(is_network_error(&unresolved));
        assert!(is_network_error(&russh::Error::KeepaliveTimeout.into()));
        assert!(is_network_error(&russh::Error::Disconnect.into()));
        assert!(is_network_error(
            &Error::from(ErrorKind::ConnectionReset).into()
        ));

        // A login the server refuses, a key the user does not trust and a
        // file that cannot be read all need a person.
        assert!(!is_network_error(
            &MissingCredential::Password { rejected: true }.into()
        ));
        assert!(!is_network_error(&russh::Error::UnknownKey.into()));
        assert!(!is_network_error(
            &Error::from(ErrorKind::PermissionDenied).into()
        ));
        assert!(!is_network_error(&anyhow::anyhow!(
            "认证失败：服务器未接受可用的认证方式"
        )));
    }
}
