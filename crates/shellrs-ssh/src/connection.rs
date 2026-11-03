//! SSH connection and authentication shared by terminals and SFTP.
use super::error::{
    AgentProblem, AgentUnreachable, AuthError, ConnectError, ForwardProblem, KnownHostsError,
    MissingCredential, PasswordWanted, RouteFailure, WrapErr, find_cause, wrap,
};
use super::login::{DEFAULT_KEY_FILES, HostLogin, JumpLogin, LoginMethod, LoginRoute, ProxyLogin};
use super::prompt::{
    AuthenticationRequest, ConnectionPrompt, ConnectionPromptField, ConnectionPromptKind,
    ConnectionPromptReply, ConnectionSecret,
};
use super::session::ChannelStream;
use anyhow::{Result, anyhow, bail};
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
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch};
use zeroize::Zeroizing;

use super::proxy::{ProxyAuth, handshake};
use super::secret::{SecretRef, SharedSecretSource, read_secret};
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const AUTH_RETRIES: usize = 3;
/// How long to wait for the agent to answer the door. A named pipe that
/// stays busy would otherwise be retried forever.
const AGENT_TIMEOUT: Duration = Duration::from_secs(5);
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

impl SshConnectionConfig {
    /// See [`HostLogin::shell_only`].
    pub fn shell_only(&self) -> bool {
        self.login.shell_only
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
    /// No agent at all, as on a phone: a login that is only the agent says
    /// it is not supported here.
    None,
}

/// Cloneable trust and credential service. Every connector of one trust
/// file shares its lock, whoever made it.
#[derive(Clone)]
pub struct SshConnector {
    known_hosts_path: PathBuf,
    known_hosts_lock: Arc<Mutex<()>>,
    secrets: SharedSecretSource,
    agent: AgentLocation,
    /// Where a login without a password looks for [`DEFAULT_KEY_FILES`].
    key_dir: Option<PathBuf>,
    /// See [`Self::with_nat64`].
    nat64: bool,
}
impl SshConnector {
    /// Trusts the host keys in the file at `path`, reads saved secrets from
    /// `secrets`, and uses the user's own agent and `~/.ssh`.
    pub fn new(path: impl Into<PathBuf>, secrets: SharedSecretSource) -> Self {
        let known_hosts_path = path.into();
        Self {
            known_hosts_lock: known_hosts_lock(&known_hosts_path),
            known_hosts_path,
            secrets,
            agent: AgentLocation::System,
            key_dir: dirs::home_dir().map(|home| home.join(".ssh")),
            nat64: false,
        }
    }

    /// Use the agent at `agent` instead of the user's own.
    pub fn with_agent(mut self, agent: AgentLocation) -> Self {
        self.agent = agent;
        self
    }

    /// Look for the default keys in `dir` rather than `~/.ssh`; `None`
    /// tries none.
    pub fn with_default_key_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.key_dir = dir;
        self
    }

    /// Connect to IPv4 addresses as the system's resolver gives them, as
    /// iOS asks of apps: on an IPv6-only network with NAT64, that is the
    /// IPv6 address that reaches them, where the address as written does
    /// not. Only Apple's systems do this; elsewhere it changes nothing.
    pub fn with_nat64(mut self, nat64: bool) -> Self {
        self.nat64 = nat64;
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
    pub fn secrets(&self) -> &SharedSecretSource {
        &self.secrets
    }

    /// Same as [`Self::connect`], with saved secrets read from `secrets`.
    pub async fn connect_with(
        &self,
        config: &SshConnectionConfig,
        broker: Arc<SshPrompts>,
        secrets: &SharedSecretSource,
    ) -> Result<(client::Handle<SshClientHandler>, String)> {
        self.connect_inner(config, broker, secrets, None).await
    }

    async fn connect_inner(
        &self,
        config: &SshConnectionConfig,
        broker: Arc<SshPrompts>,
        secrets: &SharedSecretSource,
        forwarded: Option<mpsc::UnboundedSender<ForwardedTcpip>>,
    ) -> Result<(client::Handle<SshClientHandler>, String)> {
        let login = &config.login;
        let tunnel = self
            .open_route(&login.route, &login.host, login.port, &broker, secrets)
            .await?;
        self.log_in(tunnel, login, None, &broker, secrets, forwarded)
            .await
    }

    /// A stream to `host:port` by `route`: a socket, through the proxy, or a
    /// channel through the last jump host. Each leg is given up after
    /// `CONNECT_TIMEOUT`, not counting time spent answering, or when
    /// `broker` is shut down; the jump hosts log in with their own logins and
    /// ask through `broker`, naming the jump host. What an SSH login starts
    /// from, and all a Telnet connection is.
    pub async fn open_route(
        &self,
        route: &LoginRoute,
        host: &str,
        port: u16,
        broker: &Arc<SshPrompts>,
        secrets: &SharedSecretSource,
    ) -> Result<Tunnel> {
        Ok(match route {
            LoginRoute::Direct => Box::new(step(broker, tcp(host, port, self.nat64)).await?),
            LoginRoute::Proxy(proxy) => {
                let proxied = through_proxy(proxy, (host, port), secrets, self.nat64);
                Box::new(step(broker, proxied).await?)
            }
            LoginRoute::Jump(hops) => {
                self.through_jumps(hops, (host, port), broker, secrets)
                    .await?
            }
        })
    }

    /// Open the SSH connection to `login`'s host over `tunnel` and log in.
    /// `jump_host` names a jump host, for its questions.
    async fn log_in(
        &self,
        tunnel: Tunnel,
        login: &HostLogin,
        jump_host: Option<&str>,
        broker: &Arc<SshPrompts>,
        secrets: &SharedSecretSource,
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
                .wrap_err(ConnectError::Handshake)
        };
        let mut handle = step(broker, connect).await?;
        let prompts = Asker {
            prompts: broker,
            jump_host,
        };
        let mut shutdown = broker.shutdown_receiver();
        tokio::select! {
            result = authenticate(&mut handle, login, secrets, self, prompts) => result?,
            _ = shutdown.changed() => bail!(ConnectError::Cancelled),
        }
        let fingerprint = lock(&fingerprint).clone();
        Ok((handle, fingerprint))
    }

    /// A stream to `target` through `hops`, each logged in to over the one
    /// before. A failure says which jump host it was at.
    async fn through_jumps(
        &self,
        hops: &[JumpLogin],
        target: (&str, u16),
        broker: &Arc<SshPrompts>,
        secrets: &SharedSecretSource,
    ) -> Result<Tunnel> {
        // Nothing is connected while the way is known to be broken.
        let hosts = hops
            .iter()
            .enumerate()
            .map(|(ix, hop)| match hop {
                JumpLogin::Host { name, login } => Ok((name.as_str(), &**login)),
                JumpLogin::Deleted => Err(anyhow!(RouteFailure::JumpDeleted { number: ix + 1 })),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut tunnel = None;
        for (ix, (name, hop)) in hosts.iter().enumerate() {
            let (next_host, next_port) = hosts
                .get(ix + 1)
                .map_or(target, |(_, next)| (next.host.as_str(), next.port));
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
                    if find_cause::<RouteFailure>(&error).is_some() {
                        return error;
                    }
                    wrap(
                        RouteFailure::Jump {
                            name: name.to_string(),
                        },
                        error,
                    )
                })?;
            tunnel = Some(reached);
        }
        tunnel.ok_or_else(|| anyhow!(ConnectError::NoJumpHosts))
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
        secrets: &SharedSecretSource,
    ) -> Result<Tunnel> {
        let tunnel = match tunnel {
            Some(tunnel) => tunnel,
            None => Box::new(step(broker, tcp(&hop.host, hop.port, self.nat64)).await?),
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
                    let problem = match &error {
                        russh::Error::ChannelOpenFailure(
                            russh::ChannelOpenFailure::ConnectFailed,
                        ) => ForwardProblem::ConnectFailed,
                        russh::Error::ChannelOpenFailure(
                            russh::ChannelOpenFailure::AdministrativelyProhibited,
                        ) => ForwardProblem::Prohibited,
                        _ => ForwardProblem::ChannelFailed,
                    };
                    let failure = forward_failure(name, next, problem);
                    // Refused like a direct connection would be, so it is
                    // retried like one.
                    let error = if problem == ForwardProblem::ConnectFailed {
                        anyhow::Error::from(std::io::Error::new(
                            std::io::ErrorKind::ConnectionRefused,
                            failure.clone(),
                        ))
                    } else {
                        anyhow::Error::from(error)
                    };
                    wrap(failure, error)
                })
        };
        // Given up on while the jump host still tries to connect (a
        // firewall that drops what is sent its way): the jump host itself
        // answered, so say where it could not get to.
        let channel = step(broker, open).await.map_err(|error| {
            if !matches!(
                error.downcast_ref::<russh::Error>(),
                Some(russh::Error::ConnectionTimeout)
            ) {
                return error;
            }
            wrap(forward_failure(name, next, ForwardProblem::TimedOut), error)
        })?;
        Ok(Box::new(channel.into_stream()))
    }
}

/// That the jump host `name` could not open the way on to `next`.
fn forward_failure(name: &str, (host, port): (&str, u16), problem: ForwardProblem) -> RouteFailure {
    RouteFailure::JumpForward {
        name: name.to_string(),
        host: host.to_string(),
        port,
        problem,
    }
}

/// What carries a connection to the host: a socket, or a channel through a
/// jump host.
pub type Tunnel = Box<dyn ChannelStream>;

/// The key exchanges ShellRS offers, in OpenSSH's order: russh's own list
/// leaves out the NIST curves, which servers without curve25519 (Apache
/// SSHD, as many bastion hosts run) settle on with OpenSSH.
const KEX_ORDER: &[russh::kex::Name] = &[
    russh::kex::MLKEM768X25519_SHA256,
    russh::kex::CURVE25519,
    russh::kex::CURVE25519_PRE_RFC_8731,
    russh::kex::ECDH_SHA2_NISTP256,
    russh::kex::ECDH_SHA2_NISTP384,
    russh::kex::ECDH_SHA2_NISTP521,
    russh::kex::DH_GEX_SHA256,
    russh::kex::DH_G16_SHA512,
    russh::kex::DH_G18_SHA512,
    russh::kex::DH_G17_SHA512,
    russh::kex::DH_G15_SHA512,
    russh::kex::DH_G14_SHA256,
    russh::kex::EXTENSION_SUPPORT_AS_CLIENT,
    russh::kex::EXTENSION_SUPPORT_AS_SERVER,
    russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
    russh::kex::EXTENSION_OPENSSH_STRICT_KEX_AS_SERVER,
];

/// The smallest group a group exchange accepts, as OpenSSH's: russh asks
/// for 3072 bits, which Apache SSHD on an older Java cannot give (its
/// largest is 2048) and so ends the connection.
const MIN_DH_GROUP_BITS: usize = 2048;

/// Connects wherever OpenSSH with its default settings does.
fn ssh_config() -> Arc<client::Config> {
    let defaults = client::GexParams::default();
    Arc::new(client::Config {
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: 3,
        nodelay: true,
        preferred: russh::Preferred {
            kex: KEX_ORDER.into(),
            ..russh::Preferred::default()
        },
        gex: client::GexParams::new(
            MIN_DH_GROUP_BITS,
            defaults.preferred_group_size(),
            defaults.max_group_size(),
        )
        .expect("2048 bits is the smallest group russh allows"),
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
        _ = shutdown.changed() => bail!(ConnectError::Cancelled),
    }
}

/// A socket to `host:port`, failing the way russh's own connect does: a
/// name that does not resolve then counts as the network failing.
async fn tcp(host: &str, port: u16, nat64: bool) -> Result<TcpStream> {
    let socket = connect_socket(host, port, nat64)
        .await
        .map_err(russh::Error::IO)
        .wrap_err(ConnectError::Unreachable {
            host: host.to_string(),
            port,
        })?;
    // As russh's own connect does: the terminal's keystrokes go out at once.
    let _ = socket.set_nodelay(true);
    Ok(socket)
}

/// A socket to `host:port`, which `nat64` resolves an IPv4 address for:
/// see [`SshConnector::with_nat64`].
async fn connect_socket(host: &str, port: u16, nat64: bool) -> std::io::Result<TcpStream> {
    #[cfg(target_vendor = "apple")]
    if nat64 && let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        let addresses = super::nat64::addresses(ip, port).await;
        return TcpStream::connect(&addresses[..]).await;
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = nat64;
    TcpStream::connect((host, port)).await
}

/// A stream to `target` through `proxy`.
async fn through_proxy(
    proxy: &ProxyLogin,
    (host, port): (&str, u16),
    secrets: &SharedSecretSource,
    nat64: bool,
) -> Result<TcpStream> {
    let mut socket = connect_socket(&proxy.host, proxy.port, nat64)
        .await
        .map_err(|error| {
            wrap(
                RouteFailure::ProxyUnreachable {
                    host: proxy.host.clone(),
                    port: proxy.port,
                },
                russh::Error::IO(error),
            )
        })?;
    let _ = socket.set_nodelay(true);
    let password = match proxy.password() {
        Some(secret) => read_secret(secrets, &secret).await,
        None => None,
    };
    let auth = proxy.user.as_deref().map(|user| ProxyAuth {
        user,
        password: password.as_deref().map_or("", String::as_str),
    });
    handshake(&mut socket, proxy.kind, host, port, auth)
        .await
        .wrap_err(RouteFailure::Proxy)?;
    Ok(socket)
}

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

    pub fn prompt_activity_receiver(&self) -> watch::Receiver<bool> {
        self.prompt_activity.subscribe()
    }

    async fn ask(&self, kind: ConnectionPromptKind) -> Result<ConnectionPromptReply> {
        let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(request_id, sender);
        let _ = self.prompt_activity.send(true);
        let result = async {
            if !(self.events)(ConnectionPrompt::new(request_id, kind)) {
                bail!(ConnectError::ViewClosed);
            }
            let mut shutdown = self.shutdown.clone();
            tokio::select! {
                reply = receiver => reply.map_err(|_| anyhow!(ConnectError::PromptCancelled)),
                _ = shutdown.changed() => bail!(ConnectError::Cancelled),
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
pub async fn timeout_excluding_prompts<F>(
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
                        bail!(ConnectError::Cancelled);
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
                    bail!(ConnectError::Cancelled);
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

/// The lock of the trust file at `path`, the same for every connector of
/// it in the process: two connections that both save a key must not lose
/// one, whoever made their connectors.
fn known_hosts_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);
    lock(&LOCKS)
        .get_or_insert_with(HashMap::new)
        .entry(path.to_path_buf())
        .or_default()
        .clone()
}

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
        // A key never seen, or one other than the saved: a reinstalled
        // server, or someone posing as it. The user decides, and saying yes
        // to a changed key replaces the saved ones.
        let question = if known.is_empty() {
            ConnectionPromptKind::unknown_host(&self.host, self.port, algorithm, fingerprint)
        } else {
            let old = known
                .iter()
                .map(|(_, saved)| saved.fingerprint(HashAlg::Sha256).to_string())
                .collect();
            ConnectionPromptKind::host_key_changed(
                &self.host,
                self.port,
                algorithm,
                old,
                fingerprint,
            )
        };
        let reply = self
            .broker
            .ask(question.at_jump_host(self.jump_host.as_deref()))
            .await?;
        if !matches!(reply, ConnectionPromptReply::TrustAndSave) {
            return Ok(false);
        }
        let _guard = lock(&self.known_hosts_lock);
        let now = read_known_keys(&self.host, self.port, &self.known_hosts_path)?;
        if now.iter().any(|(_, saved)| saved == &key) {
            return Ok(true);
        }
        // The answer was about the keys saved then; another connection
        // may have saved one since.
        if !now
            .iter()
            .map(|(_, saved)| saved)
            .eq(known.iter().map(|(_, saved)| saved))
        {
            bail!(KnownHostsError::ChangedWhileSaving);
        }
        let write_failed = || {
            anyhow!(KnownHostsError::WriteFailed {
                path: self.known_hosts_path.clone(),
            })
        };
        if !known.is_empty() {
            forget_known_host(&self.host, self.port, &self.known_hosts_path)
                .map_err(|_| write_failed())?;
        }
        learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts_path)
            .map_err(|_| write_failed())?;
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

/// Drop every key saved for `host` at `port`, as `ssh-keygen -R` does: a
/// reinstalled server has new keys of every kind. A line naming it with a
/// hashed host name, which only a hand could have written there, stays.
fn forget_known_host(host: &str, port: u16, path: &Path) -> std::io::Result<()> {
    let name = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let contents = std::fs::read_to_string(path)?;
    let kept: String = contents
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with('#')
                || !line
                    .split_whitespace()
                    .next()
                    .is_some_and(|hosts| hosts.split(',').any(|entry| entry == name))
        })
        .map(|line| format!("{line}\n"))
        .collect();
    // In one step, so a crash halfway cannot lose the other hosts' keys.
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, kept)?;
    std::fs::rename(&temporary, path)
}

fn read_known_keys(host: &str, port: u16, path: &Path) -> Result<Vec<(usize, PublicKey)>> {
    if path.exists() {
        let contents =
            std::fs::read_to_string(path).wrap_err_with(|| KnownHostsError::ReadFailed {
                path: path.to_path_buf(),
            })?;
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
                bail!(KnownHostsError::Corrupt {
                    path: path.to_path_buf(),
                });
            }
        }
    }
    known_host_keys_path(host, port, path).map_err(|_| {
        anyhow!(KnownHostsError::Unreadable {
            path: path.to_path_buf(),
        })
    })
}

async fn authenticate(
    handle: &mut SshHandle,
    login: &HostLogin,
    secrets: &SharedSecretSource,
    connector: &SshConnector,
    broker: Asker<'_>,
) -> Result<()> {
    let user = login.user.as_str();
    let method = login.method;
    let first = handle
        .authenticate_none(user)
        .await
        .map_err(|_| anyhow!(AuthError::MethodsUnknown))?;
    if first.success() {
        return Ok(());
    }
    let mut methods = remaining_methods(first);
    let mut partial = false;

    // A login without a password tries the agent quietly and moves on; a
    // login that is only the agent says what went wrong with it.
    if matches!(method, LoginMethod::NoPassword | LoginMethod::Agent) {
        if methods.contains(&MethodKind::PublicKey) {
            match try_agent(handle, user, &connector.agent).await? {
                Ok(result) => {
                    if result.success() {
                        return Ok(());
                    }
                    partial = is_partial(&result);
                    methods = remaining_methods(result);
                }
                Err(problem) if method == LoginMethod::Agent => bail!(problem),
                Err(_) => {}
            }
            if method == LoginMethod::Agent && !partial {
                bail!(AuthError::AgentRejected);
            }
        } else if method == LoginMethod::Agent {
            bail!(AuthError::AgentNoPublicKey);
        }
    }

    if matches!(method, LoginMethod::NoPassword | LoginMethod::Key)
        && methods.contains(&MethodKind::PublicKey)
    {
        let paths = key_paths(login, connector.key_dir.as_deref())?;
        for path in paths {
            let Some(key) = load_private_key(&path, secrets, broker).await? else {
                continue;
            };
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .map_err(|_| anyhow!(AuthError::RsaHash))?
                .flatten();
            let result = handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|_| anyhow!(AuthError::KeyFailed))?;
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
            bail!(AuthError::KeyRejected);
        }
    }

    // Nothing typed is all a login without a password has. A server that
    // wants more is told no here, not answered with a question; a code it
    // asks for after a key is still asked, below.
    if method == LoginMethod::NoPassword && !partial {
        if methods.contains(&MethodKind::Password)
            || methods.contains(&MethodKind::KeyboardInteractive)
        {
            return Err(PasswordWanted.into());
        }
        bail!(AuthError::NoKeyAccepted);
    }

    if method == LoginMethod::Password || partial {
        // The saved password goes to the server once, by whichever method
        // asks for it first.
        let mut saved_offered = false;
        let mut saved_rejected = false;
        if methods.contains(&MethodKind::Password) {
            // Try what the host has saved before bothering anyone.
            if let Some(saved) = read_secret(secrets, &login.password).await {
                saved_offered = true;
                let result = handle
                    .authenticate_password(user, saved.to_string())
                    .await
                    .map_err(|_| anyhow!(AuthError::PasswordFailed))?;
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
            if !partial && methods.contains(&MethodKind::Password) {
                for _ in 0..AUTH_RETRIES {
                    let answer = ask_one_secret(
                        broker,
                        MissingCredential::Password {
                            rejected: saved_rejected,
                        },
                        AuthenticationRequest::Password { saved_rejected },
                    )
                    .await?;
                    let result = handle
                        .authenticate_password(user, answer.into_inner())
                        .await
                        .map_err(|_| anyhow!(AuthError::PasswordFailed))?;
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
            // A password login whose server takes passwords only this way
            // (ESXi) is asked for it as a single hidden question. After a
            // password that got partway, the question is a second factor.
            let mut password = if method == LoginMethod::Password && !partial {
                Some(KeyboardPassword {
                    saved: if saved_offered {
                        None
                    } else {
                        read_secret(secrets, &login.password).await
                    },
                    rejected: saved_rejected,
                })
            } else {
                None
            };
            for _ in 0..AUTH_RETRIES {
                if keyboard_interactive(handle, user, broker, password.as_mut()).await? {
                    return Ok(());
                }
            }
        }
    }
    bail!(AuthError::Failed)
}

/// What a password login brings to keyboard-interactive authentication.
struct KeyboardPassword {
    /// The saved password, until it is sent.
    saved: Option<Zeroizing<String>>,
    /// The server refused the saved password.
    rejected: bool,
}

type Agent = russh::keys::agent::client::AgentClient<
    Box<dyn russh::keys::agent::client::AgentStream + Send + Unpin>,
>;

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
        Err(error) => {
            return Ok(Err(AgentProblem::Unreachable(AgentUnreachable::Other(
                error.to_string(),
            ))));
        }
    };
    let hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(|_| anyhow!(AuthError::AgentHash))?
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
            _ => return Err(AgentProblem::Unreachable(AgentUnreachable::NoSocket)),
        },
        AgentLocation::None => {
            return Err(AgentProblem::Unreachable(AgentUnreachable::Unsupported));
        }
    };
    match tokio::time::timeout(AGENT_TIMEOUT, AgentClient::connect_uds(&path)).await {
        Ok(Ok(agent)) => Ok(agent.dynamic()),
        Ok(Err(russh::keys::Error::IO(error))) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(AgentProblem::Unreachable(AgentUnreachable::MissingSocket {
                path,
            }))
        }
        Ok(Err(error)) => Err(AgentProblem::Unreachable(AgentUnreachable::Other(
            error.to_string(),
        ))),
        Err(_) => Err(AgentProblem::Unreachable(AgentUnreachable::NoResponse)),
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
        AgentLocation::None => {
            return Err(AgentProblem::Unreachable(AgentUnreachable::Unsupported));
        }
    };
    match tokio::time::timeout(AGENT_TIMEOUT, AgentClient::connect_named_pipe(&pipe)).await {
        Ok(Ok(agent)) => Ok(agent.dynamic()),
        Ok(Err(russh::keys::Error::IO(error))) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(AgentProblem::Unreachable(
                AgentUnreachable::ServiceNotRunning,
            ))
        }
        Ok(Err(error)) => Err(AgentProblem::Unreachable(AgentUnreachable::Other(
            error.to_string(),
        ))),
        Err(_) => Err(AgentProblem::Unreachable(AgentUnreachable::NoResponse)),
    }
}

#[cfg(not(any(unix, windows)))]
async fn connect_agent(location: &AgentLocation) -> Result<Agent, AgentProblem> {
    let _ = location;
    Err(AgentProblem::Unreachable(AgentUnreachable::Unsupported))
}

fn key_paths(login: &HostLogin, key_dir: Option<&Path>) -> Result<Vec<PathBuf>> {
    if login.method == LoginMethod::Key {
        return login
            .key_path
            .clone()
            .map(|path| vec![path])
            .ok_or_else(|| anyhow!(AuthError::KeyNotChosen));
    }
    let Some(dir) = key_dir else {
        return Ok(Vec::new());
    };
    Ok(DEFAULT_KEY_FILES
        .into_iter()
        .map(|name| dir.join(name))
        .collect())
}

async fn load_private_key(
    path: &Path,
    secrets: &SharedSecretSource,
    broker: Asker<'_>,
) -> Result<Option<russh::keys::PrivateKey>> {
    if !path.exists() {
        return Ok(None);
    }
    match load_secret_key(path, None) {
        Ok(key) => return Ok(Some(key)),
        Err(russh::keys::Error::KeyIsEncrypted) => {}
        Err(_) => bail!(AuthError::KeyUnreadable {
            path: path.to_path_buf(),
        }),
    }
    // Passphrases are saved per key file, so one saved answer unlocks the same
    // key for every host that uses it.
    let mut saved_rejected = false;
    if let Some(saved) = read_secret(secrets, &SecretRef::passphrase(path)).await {
        if let Ok(key) = load_secret_key(path, Some(saved.as_str())) {
            return Ok(Some(key));
        }
        saved_rejected = true;
    }
    for _ in 0..AUTH_RETRIES {
        let answer = ask_one_secret(
            broker,
            MissingCredential::Passphrase {
                rejected: saved_rejected,
            },
            AuthenticationRequest::Passphrase {
                key_path: path.to_path_buf(),
                saved_rejected,
            },
        )
        .await?;
        if let Ok(key) = load_secret_key(path, Some(answer.expose())) {
            return Ok(Some(key));
        }
    }
    bail!(AuthError::KeyTooManyAttempts)
}

async fn ask_one_secret(
    broker: Asker<'_>,
    need: MissingCredential,
    request: AuthenticationRequest,
) -> Result<ConnectionSecret> {
    let reply = broker
        .ask_credential(need, ConnectionPromptKind::authentication(request))
        .await?;
    match reply {
        ConnectionPromptReply::Answers(mut answers) if answers.len() == 1 => Ok(answers.remove(0)),
        ConnectionPromptReply::Cancel => bail!(AuthError::Cancelled),
        _ => bail!(AuthError::InvalidReply),
    }
}

/// One keyboard-interactive exchange, `false` when the server refuses it.
///
/// For a password login, the exchange's first single hidden question asks
/// for the password: the saved one answers it unasked, as WinSCP's does,
/// and a connection that cannot ask says the password is what it lacks.
async fn keyboard_interactive(
    handle: &mut SshHandle,
    user: &str,
    broker: Asker<'_>,
    mut password: Option<&mut KeyboardPassword>,
) -> Result<bool> {
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|_| anyhow!(AuthError::KeyboardStartFailed))?;
    let mut password_asked = false;
    let mut saved_sent = false;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                if saved_sent && let Some(password) = password.as_deref_mut() {
                    password.rejected = true;
                }
                return Ok(false);
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let asks_password = password.is_some()
                    && !password_asked
                    && matches!(prompts.as_slice(), [prompt] if !prompt.echo);
                password_asked |= asks_password;
                let saved = password
                    .as_deref_mut()
                    .filter(|_| asks_password)
                    .and_then(|password| password.saved.take());
                let answers = if prompts.is_empty() {
                    // Only something to read, which OpenSSH passes on from
                    // PAM this way; the server waits for the empty reply.
                    Vec::new()
                } else if let Some(saved) = saved {
                    saved_sent = true;
                    vec![saved.to_string()]
                } else {
                    let rejected = password
                        .as_deref()
                        .is_some_and(|password| password.rejected);
                    let need = if asks_password {
                        MissingCredential::Password { rejected }
                    } else {
                        MissingCredential::KeyboardInteractive
                    };
                    let fields = prompts
                        .into_iter()
                        .map(|prompt| ConnectionPromptField::new(prompt.prompt, prompt.echo))
                        .collect();
                    let request = AuthenticationRequest::KeyboardInteractive {
                        name,
                        instructions,
                        fields,
                        saved_password_rejected: asks_password && rejected,
                    };
                    let reply = broker
                        .ask_credential(need, ConnectionPromptKind::authentication(request))
                        .await?;
                    let ConnectionPromptReply::Answers(answers) = reply else {
                        bail!(AuthError::KeyboardCancelled)
                    };
                    answers
                        .into_iter()
                        .map(|answer| answer.into_inner())
                        .collect()
                };
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|_| anyhow!(AuthError::KeyboardFailed))?;
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
    use super::{KnownHostsError, read_known_keys};
    #[test]
    fn malformed_known_hosts_is_blocked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        std::fs::write(&path, "this is not a key\n").unwrap();
        let error = read_known_keys("example.test", 22, &path).unwrap_err();
        assert_eq!(
            error.downcast_ref::<KnownHostsError>(),
            Some(&KnownHostsError::Corrupt { path })
        );
    }

    #[test]
    fn forgetting_a_host_drops_its_keys_and_keeps_the_rest() {
        use super::forget_known_host;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        let key = "AAAAC3NzaC1lZDI1NTE5AAAAIKjlGLd6uUkTpO5kSb3hPwdVvVGrN1J0yW3sKa3BpB2c";
        std::fs::write(
            &path,
            format!(
                "# kept as it is\n\
                 192.168.7.1 ssh-ed25519 {key}\n\
                 [192.168.7.1]:2222 ssh-ed25519 {key}\n\
                 192.168.7.10 ssh-ed25519 {key}\n\
                 192.168.7.1 ssh-rsa {key}\n\
                 alias,192.168.7.1 ssh-ed25519 {key}\n"
            ),
        )
        .unwrap();
        forget_known_host("192.168.7.1", 22, &path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!(
                "# kept as it is\n\
                 [192.168.7.1]:2222 ssh-ed25519 {key}\n\
                 192.168.7.10 ssh-ed25519 {key}\n"
            )
        );
        forget_known_host("192.168.7.1", 2222, &path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("# kept as it is\n192.168.7.10 ssh-ed25519 {key}\n")
        );
    }

    #[test]
    fn only_a_failing_network_counts_as_a_network_error() {
        use super::{MissingCredential, is_network_error};
        use std::io::{Error, ErrorKind};

        // What a refused or unresolvable connect looks like coming out of
        // russh: the socket's error wrapped, under the connector's context.
        let refused = crate::wrap(
            crate::ConnectError::Handshake,
            russh::Error::IO(Error::from(ErrorKind::ConnectionRefused)),
        );
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
        assert!(!is_network_error(&super::AuthError::Failed.into()));
    }

    #[test]
    fn connectors_of_one_trust_file_share_its_lock() {
        use super::{SecretRef, SshConnector};
        use crate::SecretSource;
        use std::sync::Arc;
        use zeroize::Zeroizing;

        struct NoSecrets;
        impl SecretSource for NoSecrets {
            fn secret(&self, _: &SecretRef) -> anyhow::Result<Option<Zeroizing<String>>> {
                Ok(None)
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("known_hosts");
        let connector = |path| SshConnector::new(path, Arc::new(NoSecrets));
        let (one, other) = (connector(path.clone()), connector(path));
        assert!(Arc::ptr_eq(&one.known_hosts_lock, &other.known_hosts_lock));
        let elsewhere = connector(directory.path().join("other"));
        assert!(!Arc::ptr_eq(
            &one.known_hosts_lock,
            &elsewhere.known_hosts_lock
        ));
    }

    #[test]
    fn default_keys_are_looked_for_only_where_told() {
        use super::{DEFAULT_KEY_FILES, HostLogin, LoginMethod, key_paths};
        use std::path::Path;

        let login = HostLogin::new("10.0.0.1", 22, "root", LoginMethod::NoPassword);
        assert!(key_paths(&login, None).unwrap().is_empty());
        let paths = key_paths(&login, Some(Path::new("/keys"))).unwrap();
        assert_eq!(paths.len(), DEFAULT_KEY_FILES.len());
        assert_eq!(paths[0], Path::new("/keys/id_ed25519"));
    }
}
