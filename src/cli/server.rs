//! The app's end of the CLI socket. Listens for as long as the app runs and
//! answers each connection on a thread of its own; the work itself is the
//! backend's, so tests can put a fake behind the same server.

use std::{
    collections::VecDeque,
    io::{self, BufReader},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

#[cfg(unix)]
use tokio::sync::watch;

use super::link::OpenLink;
use super::manage::{
    CliChange, CredentialSecrets, HostSecrets, credential_details, credential_matches,
    credential_secrets, host_details, host_info, host_secrets, with_saved_credential_secrets,
    with_saved_passwords,
};
use super::protocol::{
    CliError, CredentialDetails, Envelope, ErrorCode, FrameKind, HostDetails, HostInfo,
    PROTOCOL_VERSION, Reply, Request, TransferCounters, TransferSummary, VersionOnly, parse_json,
    read_frame, write_frame, write_json,
};
use crate::host::{Host, HostLogin, HostStore, matches_query};
use crate::secrets::SecretRef;
use crate::ssh::ExecStream;

/// Does what a CLI request asks. Every method blocks: each request has a
/// thread of its own.
pub trait CliBackend: Send + Sync + 'static {
    /// Run `command` on the host, passing its output on as it
    /// comes; the remote exit code. When `output` fails, the caller is gone
    /// and the command should be abandoned.
    fn exec(
        &self,
        target: &CliTarget,
        command: &str,
        output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    ) -> Result<i32, CliError>;

    /// Copy the local `source` to `destination` on the host, the way scp
    /// does. When `progress` fails, the caller is gone and the transfer
    /// should stop.
    fn upload(
        &self,
        target: &CliTarget,
        source: &Path,
        destination: &str,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError>;

    /// Copy `source` on the host to the local `destination`, the way scp
    /// does.
    fn download(
        &self,
        target: &CliTarget,
        source: &str,
        destination: &Path,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError>;

    /// Bring the folder `destination` on the host in line with the local
    /// folder `source`: what changed is copied, what did not is left alone,
    /// and with `delete` what is not in `source` goes.
    fn sync(
        &self,
        target: &CliTarget,
        source: &Path,
        destination: &str,
        delete: bool,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError>;

    /// Whether the keychain (or, for a host not saved, memory) holds this
    /// entry. Only asked when one host or credential is shown.
    fn is_saved(&self, secret: &SecretRef) -> bool;
}

/// A saved host as the CLI sees it, with the login it connects with
/// resolved when the target list was built.
#[derive(Clone, Debug)]
pub struct CliTarget {
    info: HostInfo,
    details: HostDetails,
    secrets: HostSecrets,
    host: Host,
    login: HostLogin,
}

impl CliTarget {
    fn of(host: &Host, store: &HostStore) -> Self {
        Self {
            info: host_info(host, store),
            details: host_details(host, store),
            secrets: host_secrets(host, store),
            host: host.clone(),
            login: store.login_of(host),
        }
    }

    pub fn host(&self) -> &Host {
        &self.host
    }

    /// How a request logs in to the host.
    pub fn login(&self) -> &HostLogin {
        &self.login
    }

    /// Every host in `store`, with its group path, then the ones connected
    /// to without saving them. A request logs in to those afresh too, with
    /// the password kept in memory; a bastion host may refuse a login it
    /// gave out for one use.
    pub fn all(store: &HostStore) -> Vec<Self> {
        store
            .hosts()
            .iter()
            .chain(store.temporary_hosts())
            .map(|host| Self::of(host, store))
            .collect()
    }

    /// The host tree's search, plus the group path and the ID.
    fn matches(&self, query: &str) -> bool {
        let needle = query.trim().to_lowercase();
        matches_query(&self.host, query)
            || self
                .info
                .group
                .as_ref()
                .is_some_and(|group| group.to_lowercase().contains(&needle))
            || self.info.id.to_lowercase() == needle
    }
}

/// A saved credential as the CLI sees it.
#[derive(Clone, Debug)]
struct CliCredential {
    details: CredentialDetails,
    secrets: CredentialSecrets,
}

/// How long a change may wait for the app to take it up. Only the wait in
/// line counts: once taken, the change is being made and its outcome is
/// waited for however long the keychain takes, or an agent would try
/// again and make it twice.
const QUEUE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(10)
};

/// A change waiting for the app, with where its outcome goes.
struct PendingChange {
    id: u64,
    change: CliChange,
    reply: ChangeReply,
}

/// Where the app sends the outcome of a change it took up. Dropping it
/// unsent tells the request it will not come.
pub struct ChangeReply(mpsc::Sender<Result<Reply, CliError>>);

impl ChangeReply {
    pub fn send(self, outcome: Result<Reply, CliError>) {
        let _ = self.0.send(outcome);
    }
}

/// What the listener and the request threads share with the app.
struct Shared {
    enabled: AtomicBool,
    targets: RwLock<Vec<CliTarget>>,
    credentials: RwLock<Vec<CliCredential>>,
    backend: Arc<dyn CliBackend>,
    /// Set when ShellRS was opened again while this one runs, with the
    /// links it was opened with, until the app has come forward and opened
    /// them.
    activation: Mutex<Option<Vec<OpenLink>>>,
    /// Changes to hosts and credentials, oldest first, until the app takes
    /// them up: only the UI thread can change the store.
    changes: Mutex<VecDeque<PendingChange>>,
    next_change: AtomicU64,
    /// What kinds of command were served, for 匿名使用统计, until the app
    /// takes them.
    usage: Mutex<Vec<CliUse>>,
}

/// The kind of a `shellrs` command, as 匿名使用统计 counts them: nothing
/// of what it asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliUse {
    Exec,
    Upload,
    Download,
    Sync,
    Hosts,
    Credentials,
}

impl CliUse {
    fn of(request: &Request) -> Option<Self> {
        match request {
            Request::Exec { .. } => Some(Self::Exec),
            Request::Upload { .. } => Some(Self::Upload),
            Request::Download { .. } => Some(Self::Download),
            Request::Sync { .. } => Some(Self::Sync),
            Request::List { .. }
            | Request::ShowHost { .. }
            | Request::CreateHost { .. }
            | Request::UpdateHost { .. }
            | Request::DeleteHost { .. } => Some(Self::Hosts),
            Request::ListCredentials { .. }
            | Request::ShowCredential { .. }
            | Request::CreateCredential { .. }
            | Request::UpdateCredential { .. }
            | Request::DeleteCredential { .. } => Some(Self::Credentials),
            Request::Activate { .. } => None,
        }
    }
}

/// Listens on the CLI socket until dropped. Always listening, even with
/// 启用外部 CLI off, so a caller hears "not enabled" rather than "not
/// running" and can tell the user which of the two to fix.
pub struct CliServer {
    shared: Arc<Shared>,
    _listener: Listener,
}

impl CliServer {
    /// Take over `endpoint` (from [`crate::app::cli_endpoint`]), which must
    /// not be another running ShellRS's. Only the user running the app can
    /// connect.
    pub fn start(endpoint: PathBuf, backend: Arc<dyn CliBackend>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            enabled: AtomicBool::new(false),
            targets: RwLock::new(Vec::new()),
            credentials: RwLock::new(Vec::new()),
            backend,
            activation: Mutex::new(None),
            changes: Mutex::new(VecDeque::new()),
            next_change: AtomicU64::new(0),
            usage: Mutex::new(Vec::new()),
        });
        let listener = listen(endpoint, shared.clone())?;
        Ok(Self {
            shared,
            _listener: listener,
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::Release);
    }

    /// What the requests see of the hosts and credentials: `store` as it
    /// is now. Called whenever it changes.
    pub fn set_hosts(&self, store: &HostStore) {
        let credentials = store
            .credentials()
            .iter()
            .map(|credential| CliCredential {
                details: credential_details(credential, store),
                secrets: credential_secrets(credential),
            })
            .collect();
        *self
            .shared
            .targets
            .write()
            .unwrap_or_else(|error| error.into_inner()) = CliTarget::all(store);
        *self
            .shared
            .credentials
            .write()
            .unwrap_or_else(|error| error.into_inner()) = credentials;
    }

    /// The oldest change waiting, for the app to make and answer. One at a
    /// time: the next is only taken once this one is answered.
    pub fn take_change(&self) -> Option<(CliChange, ChangeReply)> {
        self.shared
            .changes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
            .map(|pending| (pending.change, pending.reply))
    }

    /// Whether ShellRS was opened again since this was last asked, and the
    /// links it was opened with, oldest first: `Some` and empty for a plain
    /// re-open. The request threads cannot reach the window, so the app asks
    /// here on a timer, then comes forward and opens them.
    pub fn take_activation(&self) -> Option<Vec<OpenLink>> {
        self.shared
            .activation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// The kinds of command served since this was last asked, oldest first.
    /// Asked on the same timer as `take_activation`.
    pub fn take_usage(&self) -> Vec<CliUse> {
        std::mem::take(
            &mut *self
                .shared
                .usage
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}

impl Drop for CliServer {
    /// The app is going: whatever waits for it to take up a change hears
    /// that it never will.
    fn drop(&mut self) {
        self.shared
            .changes
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
}

/// The listening thread, stopped when dropped. On Unix, the socket file
/// goes with it.
#[cfg(unix)]
struct Listener {
    socket: PathBuf,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl Drop for Listener {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// The socket is the user's alone, and every caller's user id is checked
/// again.
#[cfg(unix)]
fn listen(socket: PathBuf, shared: Arc<Shared>) -> io::Result<Listener> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};

    if std::fs::symlink_metadata(&socket).is_ok() {
        if UnixStream::connect(&socket).is_ok() {
            return Err(another_app());
        }
        // Left behind by a ShellRS that did not get to clean up.
        std::fs::remove_file(&socket)?;
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let owner = std::fs::metadata(&socket)?.uid();
    listener.set_nonblocking(true)?;

    let (shutdown, mut stop) = watch::channel(false);
    let thread = std::thread::Builder::new()
        .name("shellrs-cli".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let Ok(listener) = tokio::net::UnixListener::from_std(listener) else {
                    return;
                };
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            if let Ok((stream, _)) = accepted {
                                accept(stream, owner, &shared);
                            }
                        }
                        _ = stop.changed() => break,
                    }
                }
            });
        })?;
    Ok(Listener {
        socket,
        shutdown,
        thread: Some(thread),
    })
}

/// Hand a connection from the same user to a thread of its own.
#[cfg(unix)]
fn accept(stream: tokio::net::UnixStream, owner: u32, shared: &Arc<Shared>) {
    if !stream
        .peer_cred()
        .is_ok_and(|credentials| credentials.uid() == owner)
    {
        return;
    }
    let Ok(stream) = stream.into_std() else {
        return;
    };
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let shared = shared.clone();
    let _ = std::thread::Builder::new()
        .name("shellrs-cli-request".into())
        .spawn(move || {
            // A caller that hangs up mid-answer is no one's problem.
            let _ = serve(&mut BufReader::new(&stream), &mut &stream, &shared);
        });
}

/// The listening thread, stopped when dropped.
#[cfg(windows)]
struct Listener {
    pipe: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(windows)]
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let Some(thread) = self.thread.take() else {
            return;
        };
        // The thread waits in ConnectNamedPipe; a caller wakes it. Until it
        // has gone: between two pipe instances there is nothing to connect
        // to, so one knock may not be heard.
        while !thread.is_finished() {
            let _ = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.pipe);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = thread.join();
    }
}

/// A named pipe only the current user can open, and only from this
/// machine. Its name is the machine's, not a file of the user's, so it is
/// created as the first instance: if anyone already holds it, the server
/// does not start.
#[cfg(windows)]
fn listen(pipe: PathBuf, shared: Arc<Shared>) -> io::Result<Listener> {
    use super::pipe_windows;

    let first = pipe_windows::create_instance(&pipe, true).map_err(|error| {
        if pipe_windows::is_taken(&error) {
            another_app()
        } else {
            error
        }
    })?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name("shellrs-cli".into())
        .spawn({
            let (pipe, stop) = (pipe.clone(), stop.clone());
            move || {
                let mut waiting = first;
                loop {
                    let connected = pipe_windows::wait_for_client(&waiting);
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    // The next instance first, so a caller arriving now
                    // finds the pipe busy rather than gone.
                    let next = loop {
                        match pipe_windows::create_instance(&pipe, false) {
                            Ok(next) => break Some(next),
                            Err(_) if !stop.load(Ordering::Acquire) => {
                                std::thread::sleep(std::time::Duration::from_millis(100))
                            }
                            Err(_) => break None,
                        }
                    };
                    let Some(next) = next else {
                        break;
                    };
                    let current = std::mem::replace(&mut waiting, next);
                    // Otherwise the caller came and went already.
                    if connected.is_ok() {
                        answer(std::fs::File::from(current), &shared);
                    }
                }
            }
        })?;
    Ok(Listener {
        pipe,
        stop,
        thread: Some(thread),
    })
}

/// Answer a connected caller on a thread of its own. Closing the pipe
/// afterwards keeps what the caller has not read yet: only
/// `DisconnectNamedPipe` would throw it away.
#[cfg(windows)]
fn answer(pipe: std::fs::File, shared: &Arc<Shared>) {
    let shared = shared.clone();
    let _ = std::thread::Builder::new()
        .name("shellrs-cli-request".into())
        .spawn(move || {
            // A caller that hangs up mid-answer is no one's problem.
            let _ = serve(&mut BufReader::new(&pipe), &mut &pipe, &shared);
        });
}

#[cfg(not(any(unix, windows)))]
struct Listener;

#[cfg(not(any(unix, windows)))]
fn listen(_: PathBuf, _: Arc<Shared>) -> io::Result<Listener> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "此系统暂不支持外部 CLI",
    ))
}

fn another_app() -> io::Error {
    io::Error::new(io::ErrorKind::AddrInUse, "另一个 ShellRS 已在提供外部 CLI")
}

/// Read one request and answer it.
fn serve(
    reader: &mut impl io::Read,
    writer: &mut impl io::Write,
    shared: &Shared,
) -> io::Result<()> {
    let Some((FrameKind::Json, payload)) = read_frame(reader)? else {
        return Ok(());
    };
    let reply = respond(&payload, writer, shared).unwrap_or_else(|error| Reply::Error {
        code: error.code,
        message: error.message,
    });
    write_json(writer, &reply)
}

/// Carry out one request. Output and progress go to `writer` as they come;
/// what comes back is the reply that ends the connection, or the error sent
/// in its place.
fn respond(
    payload: &[u8],
    writer: &mut impl io::Write,
    shared: &Shared,
) -> Result<Reply, CliError> {
    let bad_request = |error: io::Error| CliError::new(ErrorCode::BadRequest, error.to_string());
    let envelope = parse_json::<Envelope>(payload);
    // Before the checks below: coming forward means the same in every
    // version, and it is ShellRS being opened again, not the external CLI.
    if let Ok(Envelope {
        request: Request::Activate { open },
        ..
    }) = &envelope
    {
        shared
            .activation
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_or_insert_with(Vec::new)
            .extend(open.clone());
        return Ok(Reply::Activated);
    }
    // The version before the request, which another version may spell in
    // a way this one cannot read.
    let version = parse_json::<VersionOnly>(payload).map_err(bad_request)?;
    if version.version != PROTOCOL_VERSION {
        return Err(CliError::new(
            ErrorCode::VersionMismatch,
            "shellrs 命令与正在运行的 ShellRS 版本不同：\
             请重新启动 ShellRS，或在 设置 → 外部 CLI 中更新 CLI",
        ));
    }
    let envelope = envelope.map_err(bad_request)?;
    if !shared.enabled.load(Ordering::Acquire) {
        return Err(CliError::new(
            ErrorCode::NotEnabled,
            "ShellRS 未启用外部 CLI：请在 ShellRS 的 设置 → 外部 CLI 中打开「启用外部 CLI」",
        ));
    }
    if let Some(usage) = CliUse::of(&envelope.request) {
        shared
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(usage);
    }
    match envelope.request {
        Request::List { query } => {
            let targets = shared
                .targets
                .read()
                .unwrap_or_else(|error| error.into_inner());
            let hosts = targets
                .iter()
                .filter(|target| query.as_deref().is_none_or(|query| target.matches(query)))
                .map(|target| target.info.clone())
                .collect();
            Ok(Reply::Hosts { hosts })
        }
        Request::Exec { host, command } => {
            let target = find(shared, &host)?;
            let code = shared
                .backend
                .exec(&target, &command, &mut |stream, bytes| {
                    let kind = match stream {
                        ExecStream::Stdout => FrameKind::Stdout,
                        ExecStream::Stderr => FrameKind::Stderr,
                    };
                    write_frame(writer, kind, bytes)
                })?;
            Ok(Reply::Exit { code })
        }
        Request::Upload {
            host,
            source,
            destination,
        } => {
            let target = find(shared, &host)?;
            if !source.is_absolute() {
                return Err(not_absolute(&source));
            }
            shared
                .backend
                .upload(&target, &source, &destination, &mut |counters| {
                    write_json(writer, &Reply::Progress(counters))
                })
                .map(Reply::TransferDone)
        }
        Request::Download {
            host,
            source,
            destination,
        } => {
            let target = find(shared, &host)?;
            if !destination.is_absolute() {
                return Err(not_absolute(&destination));
            }
            shared
                .backend
                .download(&target, &source, &destination, &mut |counters| {
                    write_json(writer, &Reply::Progress(counters))
                })
                .map(Reply::TransferDone)
        }
        Request::Sync {
            host,
            source,
            destination,
            delete,
        } => {
            let target = find(shared, &host)?;
            if !source.is_absolute() {
                return Err(not_absolute(&source));
            }
            shared
                .backend
                .sync(&target, &source, &destination, delete, &mut |counters| {
                    write_json(writer, &Reply::Progress(counters))
                })
                .map(Reply::TransferDone)
        }
        Request::ShowHost { host } => {
            let target = find(shared, &host)?;
            let saved = |secret: &SecretRef| shared.backend.is_saved(secret);
            Ok(Reply::Host(with_saved_passwords(
                target.details,
                &target.secrets,
                saved,
            )))
        }
        Request::ListCredentials { query } => {
            let credentials = shared
                .credentials
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .filter(|credential| {
                    query
                        .as_deref()
                        .is_none_or(|query| credential_matches(&credential.details, query))
                })
                .map(|credential| credential.details.clone())
                .collect();
            Ok(Reply::Credentials { credentials })
        }
        Request::ShowCredential { credential } => {
            let found = shared
                .credentials
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .find(|known| known.details.id == credential.trim())
                .cloned()
                .ok_or_else(|| {
                    CliError::new(
                        ErrorCode::CredentialNotFound,
                        format!(
                            "没有 ID 为 {credential} 的凭据：请用 shellrs credentials list 查看"
                        ),
                    )
                })?;
            let saved = |secret: &SecretRef| shared.backend.is_saved(secret);
            Ok(Reply::Credential(with_saved_credential_secrets(
                found.details,
                &found.secrets,
                saved,
            )))
        }
        Request::CreateHost { fields } => change(shared, CliChange::CreateHost(fields)),
        Request::UpdateHost { host, fields } => {
            change(shared, CliChange::UpdateHost { host, fields })
        }
        Request::DeleteHost { host, force } => {
            change(shared, CliChange::DeleteHost { host, force })
        }
        Request::CreateCredential { fields } => change(shared, CliChange::CreateCredential(fields)),
        Request::UpdateCredential { credential, fields } => {
            change(shared, CliChange::UpdateCredential { credential, fields })
        }
        Request::DeleteCredential { credential } => {
            change(shared, CliChange::DeleteCredential { credential })
        }
        // Answered above, before anything was checked.
        Request::Activate { .. } => Ok(Reply::Activated),
    }
}

/// Hand a change to the app and wait for its outcome. Taken back, so
/// certainly not made, when the app does not take it up in time.
fn change(shared: &Shared, change: CliChange) -> Result<Reply, CliError> {
    let gone = || {
        CliError::new(
            ErrorCode::ConnectFailed,
            "ShellRS 在处理改动时关闭了：请重新打开 ShellRS，用 shellrs hosts list 查看改动是否已完成",
        )
    };
    let (sender, outcome) = mpsc::channel();
    let id = shared.next_change.fetch_add(1, Ordering::Relaxed);
    shared
        .changes
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push_back(PendingChange {
            id,
            change,
            reply: ChangeReply(sender),
        });
    match outcome.recv_timeout(QUEUE_TIMEOUT) {
        Ok(outcome) => return outcome,
        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(gone()),
        Err(mpsc::RecvTimeoutError::Timeout) => {}
    }
    {
        let mut changes = shared
            .changes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = changes.iter().position(|pending| pending.id == id) {
            changes.remove(index);
            return Err(CliError::new(
                ErrorCode::ConnectFailed,
                "ShellRS 没有及时处理这项改动，改动没有做：请稍后再试",
            ));
        }
    }
    // Taken up just now: being made.
    outcome.recv().unwrap_or_else(|_| Err(gone()))
}

fn find(shared: &Shared, id: &str) -> Result<CliTarget, CliError> {
    shared
        .targets
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .find(|target| target.info.id == id.trim())
        .cloned()
        .ok_or_else(|| {
            CliError::new(
                ErrorCode::HostNotFound,
                format!("没有 ID 为 {id} 的主机：请用 shellrs hosts list 查看"),
            )
        })
}

fn not_absolute(path: &Path) -> CliError {
    CliError::new(
        ErrorCode::BadRequest,
        format!("本地路径必须是绝对路径：{}", path.display()),
    )
}
