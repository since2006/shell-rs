//! A forwarding rule carried over a real SSH connection.
//!
//! One run is one thread with its own current-thread runtime, like an SFTP
//! worker: SSH handles are bound to the runtime that made them. The run owns
//! its own connection, apart from any terminal or SFTP tab of the host.

use std::{
    future::Future,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_channel::{Receiver, Sender};
use russh::ChannelOpenFailure;
use tokio::{
    io::AsyncWriteExt as _,
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    task::JoinSet,
};

use super::{
    ForwardCommand, ForwardEvent, ForwardTransport, ForwardTransportProvider,
    socks::{self, SocksError, SocksReply},
};
use crate::{
    connection::{ConnectionPrompt, ConnectionPromptKind},
    host::{ForwardEndpoint, ForwardKind, ForwardRule, HostLogin},
    i18n::{t, tn},
    ssh::{
        ForwardedTcpip, MissingCredential, SshConnectionConfig, SshConnector, SshHandle,
        SshPrompts, describe_login_error, is_network_error,
    },
};

/// How long to wait before each attempt to log in again after the connection
/// drops, the same steps an SFTP transfer takes.
const RECONNECT_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(3),
    Duration::from_secs(10),
];
/// An attempt to log in again is unattended, so it cannot wait on anyone.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the server gets to reach a target before the client is told no.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long this machine gets to reach the target of a remote forward.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a SOCKS client gets to say where it wants to go.
const SOCKS_TIMEOUT: Duration = Duration::from_secs(10);
/// How long logging out may take when the forward stops.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the number of carried connections is reported, when it changed.
const COUNT_INTERVAL: Duration = Duration::from_millis(250);

/// Carries forwards through the connector terminals and SFTP log in with, so
/// host trust and saved passwords are the same everywhere.
pub struct SshForwardTransportProvider {
    connector: SshConnector,
    reconnect_delays: Arc<[Duration]>,
}

impl SshForwardTransportProvider {
    pub fn new(connector: SshConnector) -> Self {
        Self {
            connector,
            reconnect_delays: Arc::new(RECONNECT_DELAYS),
        }
    }

    /// The same provider waiting differently between attempts to log in
    /// again; none at all turns reconnecting off.
    #[cfg(test)]
    pub(crate) fn with_reconnect_delays(mut self, delays: &[Duration]) -> Self {
        self.reconnect_delays = delays.into();
        self
    }
}

impl ForwardTransportProvider for SshForwardTransportProvider {
    fn create(&self, rule: &ForwardRule, login: &HostLogin) -> Box<dyn ForwardTransport> {
        Box::new(SshForwardTransport {
            connector: self.connector.clone(),
            config: SshConnectionConfig::from(login),
            kind: rule.kind,
            bind: rule.bind.clone(),
            target: rule.target.clone(),
            reconnect_delays: self.reconnect_delays.clone(),
        })
    }
}

struct SshForwardTransport {
    connector: SshConnector,
    config: SshConnectionConfig,
    kind: ForwardKind,
    bind: ForwardEndpoint,
    target: Option<ForwardEndpoint>,
    reconnect_delays: Arc<[Duration]>,
}

impl ForwardTransport for SshForwardTransport {
    fn run(self: Box<Self>, commands: Receiver<ForwardCommand>, events: Sender<ForwardEvent>) {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = events.send_blocking(ForwardEvent::Failed(
                    t!("forward.error.start_failed", error = error).to_string(),
                ));
                return;
            }
        };
        runtime.block_on(self.run_async(commands, events));
    }
}

/// What the host-key checks said along the way, which the connect error
/// alone does not tell apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HostTrust {
    /// The host is not in the trust file and nobody could be asked.
    unknown: bool,
    key_changed: bool,
}

/// Why a forward could not be brought up.
enum StartError {
    Login(anyhow::Error),
    /// The server would not listen for a remote forward.
    RemoteListen,
}

/// How carrying connections over one SSH connection ended.
enum CarryEnd {
    Stopped,
    ConnectionLost,
}

/// A logged-in connection that carries the forward.
struct Connection {
    handle: Arc<SshHandle>,
    /// Channels the server opens for a remote forward. It ends when the
    /// connection does, for any kind of forward.
    forwarded: mpsc::UnboundedReceiver<ForwardedTcpip>,
}

type Incoming = mpsc::UnboundedReceiver<(TcpStream, SocketAddr)>;

impl SshForwardTransport {
    async fn run_async(&self, commands: Receiver<ForwardCommand>, events: Sender<ForwardEvent>) {
        let (shutdown_tx, shutdown) = watch::channel(false);
        let trust = Arc::new(Mutex::new(HostTrust::default()));
        // The first login may ask; logging in again after a drop may not, so
        // a forward in the background never raises a dialog on its own.
        let asking = {
            let (events, trust) = (events.clone(), trust.clone());
            Arc::new(SshPrompts::new(
                Arc::new(move |prompt: ConnectionPrompt| {
                    if matches!(prompt.kind(), ConnectionPromptKind::HostKeyChanged(_)) {
                        lock(&trust).key_changed = true;
                    }
                    events.try_send(ForwardEvent::Prompt(prompt)).is_ok()
                }),
                shutdown.clone(),
            ))
        };
        let silent = {
            let trust = trust.clone();
            Arc::new(
                SshPrompts::new(
                    Arc::new(move |prompt: ConnectionPrompt| {
                        match prompt.kind() {
                            ConnectionPromptKind::UnknownHost(_) => lock(&trust).unknown = true,
                            ConnectionPromptKind::HostKeyChanged(_) => {
                                lock(&trust).key_changed = true
                            }
                            ConnectionPromptKind::Authentication(_) => {}
                        }
                        // Nobody answers: the question fails the login.
                        false
                    }),
                    shutdown.clone(),
                )
                .non_interactive(),
            )
        };
        let router = {
            let asking = asking.clone();
            tokio::spawn(async move {
                while let Ok(command) = commands.recv().await {
                    match command {
                        ForwardCommand::PromptReply { request_id, reply } => {
                            asking.respond(request_id, reply)
                        }
                        ForwardCommand::Stop => break,
                    }
                }
                // Told to stop, or the list that owned the forward is gone.
                shutdown_tx.send_replace(true);
                asking.cancel_all();
            })
        };

        let outcome = self
            .serve(&events, shutdown.clone(), asking, silent, &trust)
            .await;
        router.abort();
        let last = match outcome {
            Err(reason) if !*shutdown.borrow() => ForwardEvent::Failed(reason),
            _ => ForwardEvent::Stopped,
        };
        let _ = events.send(last).await;
    }

    /// Bring the forward up and keep it up until it is stopped (`Ok`) or
    /// cannot go on (`Err` with the reason).
    async fn serve(
        &self,
        events: &Sender<ForwardEvent>,
        mut shutdown: watch::Receiver<bool>,
        asking: Arc<SshPrompts>,
        silent: Arc<SshPrompts>,
        trust: &Mutex<HostTrust>,
    ) -> Result<(), String> {
        // Listening comes before logging in: a port that is taken is known
        // at once, before anyone is asked for a password. The listeners then
        // stay for the whole run, so nothing else can take the port while
        // the connection is being re-established.
        let mut acceptors = JoinSet::new();
        let mut incoming = match self.kind {
            ForwardKind::Remote => None,
            ForwardKind::Local | ForwardKind::Dynamic => {
                let (sender, receiver) = mpsc::unbounded_channel();
                for listener in bind_listeners(&self.bind).await? {
                    acceptors.spawn(accept_into(listener, sender.clone()));
                }
                Some(receiver)
            }
        };

        let mut first = true;
        let mut attempt = 0;
        loop {
            let login = if first {
                let _ = events.send(ForwardEvent::Connecting).await;
                turning_away(
                    self.connect(asking.clone(), None),
                    &mut incoming,
                    &mut shutdown,
                )
                .await
            } else {
                turning_away(
                    self.connect(silent.clone(), Some(RECONNECT_TIMEOUT)),
                    &mut incoming,
                    &mut shutdown,
                )
                .await
            };
            let Some(login) = login else {
                return Ok(());
            };
            match login {
                Ok(mut connection) => {
                    first = false;
                    attempt = 0;
                    let _ = events.send(ForwardEvent::Listening).await;
                    let end = self
                        .carry(&mut connection, &mut incoming, events, &mut shutdown)
                        .await;
                    self.close(connection).await;
                    if matches!(end, CarryEnd::Stopped) {
                        return Ok(());
                    }
                }
                Err(error) => {
                    let reason = self.describe_start_error(&error, *lock(trust));
                    if first {
                        return Err(reason);
                    }
                    let retry = match &error {
                        StartError::Login(error) => is_network_error(error),
                        // The server may still be holding the port for the
                        // connection that just died.
                        StartError::RemoteListen => true,
                    };
                    if !retry {
                        return Err(
                            t!("forward.error.reconnect_failed", reason = reason).to_string()
                        );
                    }
                }
            }

            let Some(delay) = next_reconnect(attempt, &self.reconnect_delays) else {
                return Err(if self.reconnect_delays.is_empty() {
                    t!("forward.error.connection_lost").to_string()
                } else {
                    tn!("forward.error.reconnects_used", self.reconnect_delays.len()).to_string()
                });
            };
            attempt += 1;
            let _ = events
                .send(ForwardEvent::Reconnecting {
                    attempt,
                    of: self.reconnect_delays.len(),
                    delay,
                })
                .await;
            if turning_away(tokio::time::sleep(delay), &mut incoming, &mut shutdown)
                .await
                .is_none()
            {
                return Ok(());
            }
        }
    }

    /// Log in, and for a remote forward ask the server to listen.
    async fn connect(
        &self,
        prompts: Arc<SshPrompts>,
        timeout: Option<Duration>,
    ) -> Result<Connection, StartError> {
        let login = self.connector.connect_forwarding(&self.config, prompts);
        let (handle, _, forwarded) = match timeout {
            Some(timeout) => tokio::time::timeout(timeout, login)
                .await
                .unwrap_or_else(|_| Err(russh::Error::ConnectionTimeout.into())),
            None => login.await,
        }
        .map_err(StartError::Login)?;
        if self.kind == ForwardKind::Remote
            && let Err(error) = handle
                .tcpip_forward(self.bind.bare_host(), self.bind.port.into())
                .await
        {
            let _ = tokio::time::timeout(
                CLOSE_TIMEOUT,
                handle.disconnect(russh::Disconnect::ByApplication, "", ""),
            )
            .await;
            return Err(match error {
                russh::Error::RequestDenied => StartError::RemoteListen,
                error => StartError::Login(error.into()),
            });
        }
        Ok(Connection {
            handle: Arc::new(handle),
            forwarded,
        })
    }

    /// Carry connections until the forward is stopped or the SSH connection
    /// is gone.
    async fn carry(
        &self,
        connection: &mut Connection,
        incoming: &mut Option<Incoming>,
        events: &Sender<ForwardEvent>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> CarryEnd {
        // Each task ends with what went wrong for its connection, if that
        // is worth telling.
        let mut connections: JoinSet<Option<String>> = JoinSet::new();
        let mut reported = 0;
        let mut count = tokio::time::interval(COUNT_INTERVAL);
        let end = loop {
            tokio::select! {
                _ = shutdown.changed() => break CarryEnd::Stopped,
                forwarded = connection.forwarded.recv() => match forwarded {
                    // The handler feeding this is gone, and with it the
                    // connection: this is how an idle forward finds out.
                    None => break CarryEnd::ConnectionLost,
                    Some(open) => match &self.target {
                        Some(target)
                            if self.kind == ForwardKind::Remote
                                && open.connected_port == u32::from(self.bind.port) =>
                        {
                            connections.spawn(carry_remote(open, target.clone()));
                        }
                        // Not something this forward asked for.
                        _ => {
                            open.reply
                                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                                .await
                        }
                    },
                },
                Some((socket, peer)) = next_incoming(incoming) => {
                    let handle = connection.handle.clone();
                    match (self.kind, &self.target) {
                        (ForwardKind::Local, Some(target)) => {
                            connections.spawn(carry_local(handle, socket, peer, target.clone()));
                        }
                        _ => {
                            connections.spawn(carry_dynamic(handle, socket, peer));
                        }
                    }
                }
                Some(ended) = connections.join_next(), if !connections.is_empty() => {
                    if let Ok(Some(problem)) = ended {
                        let _ = events.send(ForwardEvent::ConnectionFailed(problem)).await;
                    }
                }
                _ = count.tick() => {
                    if connections.len() != reported {
                        reported = connections.len();
                        let _ = events.send(ForwardEvent::Connections(reported)).await;
                    }
                }
            }
        };
        // Inside the runtime: a channel stream closes itself on drop by
        // spawning a task.
        connections.shutdown().await;
        if reported != 0 {
            let _ = events.send(ForwardEvent::Connections(0)).await;
        }
        end
    }

    /// Log out. Dropping the handle is not enough while a channel is open:
    /// each holds a sender that keeps the connection alive.
    async fn close(&self, connection: Connection) {
        let logout = async {
            if self.kind == ForwardKind::Remote {
                let _ = connection
                    .handle
                    .cancel_tcpip_forward(self.bind.bare_host(), self.bind.port.into())
                    .await;
            }
            let _ = connection
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
        };
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, logout).await;
    }

    fn describe_start_error(&self, error: &StartError, trust: HostTrust) -> String {
        match error {
            StartError::RemoteListen => {
                t!("forward.error.remote_listen", bind = self.bind).to_string()
            }
            StartError::Login(error) => describe_login_failure(error, trust),
        }
    }
}

/// Why logging in failed, in the words the list shows.
fn describe_login_failure(error: &anyhow::Error, trust: HostTrust) -> String {
    if trust.key_changed {
        return t!("forward.error.key_changed").to_string();
    }
    if trust.unknown {
        return t!("forward.error.unknown_key").to_string();
    }
    // Only an unattended login fails this way; an attended one asks.
    if let Some(need) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<MissingCredential>())
    {
        return match need {
            MissingCredential::Password { .. } => t!("forward.error.needs_password"),
            MissingCredential::Passphrase { .. } => t!("forward.error.needs_passphrase"),
            MissingCredential::KeyboardInteractive => t!("forward.error.needs_interactive"),
        }
        .to_string();
    }
    describe_login_error(error)
}

/// The wait before reconnect attempt number `attempt` (counting from 0), or
/// `None` once every attempt has been used.
fn next_reconnect(attempt: usize, delays: &[Duration]) -> Option<Duration> {
    delays.get(attempt).copied()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Listen on every address the bind host stands for (`localhost` is both
/// `127.0.0.1` and `::1`). One is enough; none is the error.
async fn bind_listeners(bind: &ForwardEndpoint) -> Result<Vec<TcpListener>, String> {
    let addresses = tokio::net::lookup_host((bind.bare_host(), bind.port))
        .await
        .map_err(|_| t!("forward.error.unresolved_bind", host = bind.host).to_string())?;
    let mut listeners = Vec::new();
    let mut failure = None;
    for address in addresses {
        match TcpListener::bind(address).await {
            Ok(listener) => listeners.push(listener),
            Err(error) => failure = Some(error),
        }
    }
    match (listeners.is_empty(), failure) {
        (false, _) => Ok(listeners),
        (true, Some(error)) => Err(describe_bind_error(&error, bind)),
        (true, None) => Err(t!("forward.error.unresolved_bind", host = bind.host).to_string()),
    }
}

fn describe_bind_error(error: &std::io::Error, bind: &ForwardEndpoint) -> String {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::AddrInUse => t!("forward.error.port_in_use", port = bind.port),
        ErrorKind::PermissionDenied => t!("forward.error.port_denied", port = bind.port),
        ErrorKind::AddrNotAvailable => t!("forward.error.no_such_address", host = bind.host),
        _ => t!("forward.error.bind_failed", bind = bind, error = error),
    }
    .to_string()
}

/// Hand every accepted connection to the run, for as long as it lasts.
async fn accept_into(
    listener: TcpListener,
    incoming: mpsc::UnboundedSender<(TcpStream, SocketAddr)>,
) {
    loop {
        match listener.accept().await {
            Ok(accepted) => {
                if incoming.send(accepted).is_err() {
                    return;
                }
            }
            // Out of file descriptors, most likely; give them time to free.
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

async fn next_incoming(incoming: &mut Option<Incoming>) -> Option<(TcpStream, SocketAddr)> {
    match incoming {
        Some(incoming) => incoming.recv().await,
        None => std::future::pending().await,
    }
}

/// Wait for `work` while there is no connection to carry anything over:
/// whoever connects to the listening port meanwhile is turned away at once
/// instead of being left to hang. `None` when the forward is told to stop.
async fn turning_away<T>(
    work: impl Future<Output = T>,
    incoming: &mut Option<Incoming>,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<T> {
    tokio::pin!(work);
    loop {
        tokio::select! {
            done = &mut work => return Some(done),
            _ = shutdown.changed() => return None,
            Some(refused) = next_incoming(incoming) => drop(refused),
        }
    }
}

/// `ssh -L`: the server connects to the target for a local client.
async fn carry_local(
    handle: Arc<SshHandle>,
    mut socket: TcpStream,
    peer: SocketAddr,
    target: ForwardEndpoint,
) -> Option<String> {
    let mut stream = match open_channel(&handle, target.bare_host(), target.port, peer).await {
        Ok(channel) => channel.into_stream(),
        Err(error) => return describe_open_error(error.as_ref(), &target.to_string()),
    };
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
    None
}

/// `ssh -D`: the client says where, the server connects there.
async fn carry_dynamic(
    handle: Arc<SshHandle>,
    mut socket: TcpStream,
    peer: SocketAddr,
) -> Option<String> {
    let request = match tokio::time::timeout(SOCKS_TIMEOUT, socks::negotiate(&mut socket)).await {
        Ok(Ok(request)) => request,
        // Not SOCKS is worth saying: something is pointed at this port that
        // should not be. A client that gives up or is refused is not.
        Ok(Err(SocksError::Version)) => {
            return Some(t!("forward.error.not_socks").to_string());
        }
        Ok(Err(_)) | Err(_) => return None,
    };
    let target = &request.target;
    match open_channel(&handle, &target.host, target.port, peer).await {
        Ok(channel) => {
            if socks::reply(&mut socket, request.version, SocksReply::Succeeded)
                .await
                .is_err()
            {
                return None;
            }
            let mut stream = channel.into_stream();
            if !request.leftover.is_empty() && stream.write_all(&request.leftover).await.is_err() {
                return None;
            }
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
            None
        }
        Err(error) => {
            let code = match error.as_ref() {
                Some(russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed)) => {
                    SocksReply::ConnectionRefused
                }
                Some(russh::Error::ChannelOpenFailure(
                    ChannelOpenFailure::AdministrativelyProhibited,
                )) => SocksReply::NotAllowed,
                _ => SocksReply::GeneralFailure,
            };
            let _ = socks::reply(&mut socket, request.version, code).await;
            // A site that cannot be reached is the client's everyday
            // business; a server that refuses to forward at all is not.
            match error.as_ref() {
                Some(russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed)) => None,
                error => describe_open_error(error, &format!("{}:{}", target.host, target.port)),
            }
        }
    }
}

/// `ssh -R`: someone connected to the server's port; this machine connects
/// to the target. The channel is accepted only once the target answers, so
/// a target that is down looks refused from the other end, not half-open.
async fn carry_remote(open: ForwardedTcpip, target: ForwardEndpoint) -> Option<String> {
    let dial = TcpStream::connect((target.bare_host(), target.port));
    let mut socket = match tokio::time::timeout(DIAL_TIMEOUT, dial).await {
        Ok(Ok(socket)) => socket,
        failed => {
            open.reply.reject(ChannelOpenFailure::ConnectFailed).await;
            let reason = match failed {
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    t!("forward.error.local_refused").to_string()
                }
                Ok(Err(error)) => error.to_string(),
                _ => t!("forward.error.local_timed_out").to_string(),
            };
            return Some(
                t!(
                    "forward.error.local_target_failed",
                    target = target,
                    reason = reason
                )
                .to_string(),
            );
        }
    };
    open.reply.accept().await;
    let mut stream = open.channel.into_stream();
    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
    None
}

/// Ask the server to connect to `host:port`. `Err(None)` when it did not
/// answer in time.
async fn open_channel(
    handle: &SshHandle,
    host: &str,
    port: u16,
    peer: SocketAddr,
) -> Result<russh::Channel<russh::client::Msg>, Option<russh::Error>> {
    let open = handle.channel_open_direct_tcpip(
        host,
        port.into(),
        peer.ip().to_string(),
        peer.port().into(),
    );
    match tokio::time::timeout(OPEN_TIMEOUT, open).await {
        Ok(Ok(channel)) => Ok(channel),
        Ok(Err(error)) => Err(Some(error)),
        Err(_) => Err(None),
    }
}

/// Why the server did not open a channel to `target`, when that is about
/// the target or the server's rules. `None` when the SSH connection itself
/// is failing: that is reported once for the forward, not per connection.
fn describe_open_error(error: Option<&russh::Error>, target: &str) -> Option<String> {
    let reason = match error {
        None => t!("forward.error.server_timed_out", target = target),
        Some(russh::Error::ChannelOpenFailure(reason)) => match reason {
            ChannelOpenFailure::AdministrativelyProhibited => {
                t!("forward.error.prohibited", target = target)
            }
            ChannelOpenFailure::ConnectFailed => {
                t!("forward.error.server_connect_failed", target = target)
            }
            ChannelOpenFailure::UnknownChannelType => t!("forward.error.unsupported"),
            ChannelOpenFailure::ResourceShortage => t!("forward.error.resource_shortage"),
            ChannelOpenFailure::Other { .. } => t!("forward.error.refused", target = target),
        },
        Some(_) => return None,
    };
    Some(reason.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

    use super::*;

    #[test]
    fn bind_errors_say_what_to_do_about_the_port() {
        let bind = ForwardEndpoint::new("127.0.0.1", 80);
        assert_eq!(
            describe_bind_error(&Error::from(ErrorKind::AddrInUse), &bind),
            "本机端口 80 已被占用"
        );
        assert!(
            describe_bind_error(&Error::from(ErrorKind::PermissionDenied), &bind)
                .contains("需要管理员权限")
        );
        let elsewhere = ForwardEndpoint::new("10.9.9.9", 8080);
        assert_eq!(
            describe_bind_error(&Error::from(ErrorKind::AddrNotAvailable), &elsewhere),
            "本机没有地址 10.9.9.9，无法监听"
        );
        assert!(
            describe_bind_error(&Error::other("boom"), &elsewhere)
                .starts_with("无法监听 10.9.9.9:8080：")
        );
    }

    #[test]
    fn a_refused_channel_says_whose_rule_or_target_it_was() {
        let open = |reason| russh::Error::ChannelOpenFailure(reason);
        assert_eq!(
            describe_open_error(Some(&open(ChannelOpenFailure::ConnectFailed)), "db:3306"),
            Some("服务器无法连接 db:3306".to_string())
        );
        assert!(
            describe_open_error(
                Some(&open(ChannelOpenFailure::AdministrativelyProhibited)),
                "db:3306"
            )
            .unwrap()
            .contains("AllowTcpForwarding")
        );
        assert_eq!(
            describe_open_error(None, "db:3306"),
            Some("服务器连接 db:3306 超时".to_string())
        );
        // The connection itself failing is not a per-connection problem.
        assert_eq!(
            describe_open_error(Some(&russh::Error::SendError), "db:3306"),
            None
        );
    }

    #[test]
    fn a_failed_login_is_explained_by_what_stood_in_the_way() {
        let error = anyhow::anyhow!("无法建立 SSH 连接，请检查地址、端口和主机密钥");
        let changed = HostTrust {
            key_changed: true,
            ..HostTrust::default()
        };
        assert!(describe_login_failure(&error, changed).starts_with("主机密钥与已保存的不一致"));
        let unknown = HostTrust {
            unknown: true,
            ..HostTrust::default()
        };
        assert!(describe_login_failure(&error, unknown).contains("尚未信任"));

        let no_password = anyhow::Error::from(MissingCredential::Password { rejected: false });
        assert_eq!(
            describe_login_failure(&no_password, HostTrust::default()),
            "需要输入密码，请手动启动"
        );
        let refused =
            anyhow::Error::from(russh::Error::IO(Error::from(ErrorKind::ConnectionRefused)));
        assert_eq!(
            describe_login_failure(&refused, HostTrust::default()),
            "连接被拒绝，该端口上没有服务在监听"
        );
    }

    #[test]
    fn reconnect_attempts_run_out() {
        assert_eq!(
            next_reconnect(0, &RECONNECT_DELAYS),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            next_reconnect(2, &RECONNECT_DELAYS),
            Some(Duration::from_secs(10))
        );
        assert_eq!(next_reconnect(3, &RECONNECT_DELAYS), None);
        assert_eq!(next_reconnect(0, &[]), None);
    }
}
