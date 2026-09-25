//! The app's end of the CLI socket. Listens for as long as the app runs and
//! answers each connection on a thread of its own; the work itself is the
//! backend's, so tests can put a fake behind the same server.

use std::{
    io::{self, BufReader},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

use tokio::sync::watch;

use super::protocol::{
    CliError, Envelope, ErrorCode, FrameKind, PROTOCOL_VERSION, Reply, Request, SessionInfo,
    TransferCounters, TransferSummary, parse_json, read_frame, write_frame, write_json,
};
use crate::session::{GroupId, Session, SessionGroup, SessionStore, matches_query};
use crate::ssh::ExecStream;

/// Does what a CLI request asks. Every method blocks: each request has a
/// thread of its own.
pub trait CliBackend: Send + Sync + 'static {
    /// Run `command` on the session's host, passing its output on as it
    /// comes; the remote exit code. When `output` fails, the caller is gone
    /// and the command should be abandoned.
    fn exec(
        &self,
        session: &Session,
        command: &str,
        output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    ) -> Result<i32, CliError>;

    /// Copy the local `source` to `destination` on the host, the way scp
    /// does. When `progress` fails, the caller is gone and the transfer
    /// should stop.
    fn upload(
        &self,
        session: &Session,
        source: &Path,
        destination: &str,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError>;

    /// Copy `source` on the host to the local `destination`, the way scp
    /// does.
    fn download(
        &self,
        session: &Session,
        source: &str,
        destination: &Path,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError>;
}

/// A saved session as the CLI sees it.
#[derive(Clone, Debug)]
pub struct CliTarget {
    info: SessionInfo,
    session: Session,
}

impl CliTarget {
    /// `group` is the session's full group path, if it has one.
    pub fn new(session: &Session, group: Option<String>) -> Self {
        Self {
            info: SessionInfo {
                id: session.public_id.to_string(),
                name: session.name.to_string(),
                group,
                user: session.user.to_string(),
                host: session.host.to_string(),
                port: session.port,
                os: session.os.map(|os| os.as_str().to_string()),
            },
            session: session.clone(),
        }
    }

    /// Every session in `store`, with its group path.
    pub fn all(store: &SessionStore) -> Vec<Self> {
        let groups: std::collections::HashMap<GroupId, &SessionGroup> = store
            .groups()
            .iter()
            .map(|group| (group.id, group))
            .collect();
        let path = |mut id: Option<GroupId>| {
            let mut names = Vec::new();
            while let Some(group) = id.and_then(|id| groups.get(&id)) {
                names.push(group.name.to_string());
                id = group.parent;
                // A cycle cannot be saved, but must not hang the app either.
                if names.len() > groups.len() {
                    break;
                }
            }
            names.reverse();
            (!names.is_empty()).then(|| names.join("/"))
        };
        store
            .sessions()
            .iter()
            .map(|session| Self::new(session, path(session.group)))
            .collect()
    }

    /// The session tree's search, plus the group path and the ID.
    fn matches(&self, query: &str) -> bool {
        let needle = query.trim().to_lowercase();
        matches_query(&self.session, query)
            || self
                .info
                .group
                .as_ref()
                .is_some_and(|group| group.to_lowercase().contains(&needle))
            || self.info.id.to_lowercase() == needle
    }
}

/// What the listener and the request threads share with the app.
struct Shared {
    enabled: AtomicBool,
    targets: RwLock<Vec<CliTarget>>,
    backend: Arc<dyn CliBackend>,
}

/// Listens on the CLI socket until dropped. Always listening, even with
/// 启用外部 CLI off, so a caller hears "not enabled" rather than "not
/// running" and can tell the user which of the two to fix.
pub struct CliServer {
    shared: Arc<Shared>,
    socket: PathBuf,
    shutdown: watch::Sender<bool>,
    listener: Option<JoinHandle<()>>,
}

impl CliServer {
    /// Take over `socket`, which must not be another running ShellRS's.
    /// Only the user running the app can connect: the socket is theirs
    /// alone, and every caller's user id is checked again.
    pub fn start(socket: PathBuf, backend: Arc<dyn CliBackend>) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            enabled: AtomicBool::new(false),
            targets: RwLock::new(Vec::new()),
            backend,
        });
        let (shutdown, stop) = watch::channel(false);
        let listener = listen(&socket, shared.clone(), stop)?;
        Ok(Self {
            shared,
            socket,
            shutdown,
            listener: Some(listener),
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.shared.enabled.store(enabled, Ordering::Release);
    }

    pub fn set_targets(&self, targets: Vec<CliTarget>) {
        *self
            .shared
            .targets
            .write()
            .unwrap_or_else(|error| error.into_inner()) = targets;
    }
}

impl Drop for CliServer {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(unix)]
fn listen(
    socket: &Path,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
) -> io::Result<JoinHandle<()>> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};

    if std::fs::symlink_metadata(socket).is_ok() {
        if UnixStream::connect(socket).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "另一个 ShellRS 已在提供外部 CLI",
            ));
        }
        // Left behind by a ShellRS that did not get to clean up.
        std::fs::remove_file(socket)?;
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    let owner = std::fs::metadata(socket)?.uid();
    listener.set_nonblocking(true)?;

    std::thread::Builder::new()
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
        })
}

#[cfg(not(unix))]
fn listen(_: &Path, _: Arc<Shared>, _: watch::Receiver<bool>) -> io::Result<JoinHandle<()>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "此系统暂不支持外部 CLI",
    ))
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
            let Ok(reading) = stream.try_clone() else {
                return;
            };
            // A caller that hangs up mid-answer is no one's problem.
            let _ = serve(&mut BufReader::new(reading), &mut &stream, &shared);
        });
}

/// Read one request and answer it.
fn serve(
    reader: &mut impl io::Read,
    writer: &mut impl io::Write,
    shared: &Shared,
) -> io::Result<()> {
    let envelope: Envelope = match read_frame(reader)? {
        Some((FrameKind::Json, payload)) => match parse_json(&payload) {
            Ok(envelope) => envelope,
            Err(error) => {
                return reply_error(
                    writer,
                    CliError::new(ErrorCode::BadRequest, error.to_string()),
                );
            }
        },
        _ => return Ok(()),
    };
    if envelope.version != PROTOCOL_VERSION {
        return reply_error(
            writer,
            CliError::new(
                ErrorCode::VersionMismatch,
                "shellrs 命令与正在运行的 ShellRS 版本不同：请重新启动 ShellRS",
            ),
        );
    }
    if !shared.enabled.load(Ordering::Acquire) {
        return reply_error(
            writer,
            CliError::new(
                ErrorCode::NotEnabled,
                "ShellRS 未启用外部 CLI：请在 ShellRS 的 设置 → 外部 CLI 中打开「启用外部 CLI」",
            ),
        );
    }
    match envelope.request {
        Request::List { query } => {
            let targets = shared
                .targets
                .read()
                .unwrap_or_else(|error| error.into_inner());
            let sessions = targets
                .iter()
                .filter(|target| query.as_deref().is_none_or(|query| target.matches(query)))
                .map(|target| target.info.clone())
                .collect();
            write_json(writer, &Reply::Sessions { sessions })
        }
        Request::Exec { session, command } => {
            let target = match find(shared, &session) {
                Ok(target) => target,
                Err(error) => return reply_error(writer, error),
            };
            let result = shared
                .backend
                .exec(&target.session, &command, &mut |stream, bytes| {
                    let kind = match stream {
                        ExecStream::Stdout => FrameKind::Stdout,
                        ExecStream::Stderr => FrameKind::Stderr,
                    };
                    write_frame(writer, kind, bytes)
                });
            match result {
                Ok(code) => write_json(writer, &Reply::Exit { code }),
                Err(error) => reply_error(writer, error),
            }
        }
        Request::Upload {
            session,
            source,
            destination,
        } => {
            let target = match find(shared, &session) {
                Ok(target) => target,
                Err(error) => return reply_error(writer, error),
            };
            if !source.is_absolute() {
                return reply_error(writer, not_absolute(&source));
            }
            let result =
                shared
                    .backend
                    .upload(&target.session, &source, &destination, &mut |counters| {
                        write_json(writer, &Reply::Progress(counters))
                    });
            reply_transfer(writer, result)
        }
        Request::Download {
            session,
            source,
            destination,
        } => {
            let target = match find(shared, &session) {
                Ok(target) => target,
                Err(error) => return reply_error(writer, error),
            };
            if !destination.is_absolute() {
                return reply_error(writer, not_absolute(&destination));
            }
            let result =
                shared
                    .backend
                    .download(&target.session, &source, &destination, &mut |counters| {
                        write_json(writer, &Reply::Progress(counters))
                    });
            reply_transfer(writer, result)
        }
    }
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
                ErrorCode::SessionNotFound,
                format!("没有 ID 为 {id} 的会话：请用 shellrs list 查看"),
            )
        })
}

fn not_absolute(path: &Path) -> CliError {
    CliError::new(
        ErrorCode::BadRequest,
        format!("本地路径必须是绝对路径：{}", path.display()),
    )
}

fn reply_transfer(
    writer: &mut impl io::Write,
    result: Result<TransferSummary, CliError>,
) -> io::Result<()> {
    match result {
        Ok(summary) => write_json(writer, &Reply::TransferDone(summary)),
        Err(error) => reply_error(writer, error),
    }
}

fn reply_error(writer: &mut impl io::Write, error: CliError) -> io::Result<()> {
    write_json(
        writer,
        &Reply::Error {
            code: error.code,
            message: error.message,
        },
    )
}
