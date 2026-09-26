use super::{DirectoryEntry, DirectoryListing, EntryKind, FileMetadata, RemotePath};
use crate::ssh::{SshConnectionConfig, SshConnector, SshHandle, SshPrompts};
use anyhow::{Result, anyhow, bail};
use futures::StreamExt as _;
use russh_sftp::{
    client::{RawSftpSession, error::Error},
    protocol::{FileAttributes, OpenFlags, Packet, StatusCode},
};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::watch,
};

/// Small protocol seam, also implemented by deterministic fault-injection tests.
pub(crate) trait RemoteFs {
    /// `lstat`: a link describes itself. `None` when the path does not exist.
    async fn metadata(&self, path: &RemotePath) -> Result<Option<FileMetadata>>;
    /// `stat`: follows links. `None` when the path or the link target is missing.
    async fn stat(&self, path: &RemotePath) -> Result<Option<FileMetadata>>;
    /// The entries of a directory without `.` and `..`, kinds as `lstat` reports them.
    async fn read_dir(&self, path: &RemotePath) -> Result<Vec<DirectoryEntry>>;
    async fn open(&self, path: &RemotePath, create: bool) -> Result<String>;
    /// Open an existing file for reading.
    async fn open_read(&self, path: &RemotePath) -> Result<String>;
    /// Up to `len` bytes at `offset`; may return fewer. `None` at end of file.
    async fn read(&self, handle: &str, offset: u64, len: u32) -> Result<Option<Vec<u8>>>;
    async fn sync(&self, handle: &str) -> Result<()>;
    async fn write(&self, handle: &str, offset: u64, bytes: Vec<u8>) -> Result<()>;
    async fn close(&self, handle: &str) -> Result<()>;
    async fn attributes(
        &self,
        handle: &str,
        modified: Option<u32>,
        permissions: Option<u32>,
    ) -> Result<()>;
    async fn mkdir(&self, path: &RemotePath) -> Result<()>;
    async fn symlink(&self, target: &str, path: &RemotePath) -> Result<()>;
    async fn readlink(&self, path: &RemotePath) -> Result<String>;
    async fn rename(&self, from: &RemotePath, to: &RemotePath, replace: bool) -> Result<()>;
    async fn remove(&self, path: &RemotePath) -> Result<()>;
    /// Remove an empty directory.
    async fn rmdir(&self, path: &RemotePath) -> Result<()>;
    /// Set the permission bits of a path (`setstat`, which follows links).
    async fn set_permissions(&self, path: &RemotePath, permissions: u32) -> Result<()>;
    fn atomic_replace(&self) -> bool;
}

pub(crate) struct SftpClient {
    raw: RawSftpSession,
    _ssh: Option<SshHandle>,
    /// Turns true once the server side of the SFTP stream has ended.
    closed: watch::Receiver<bool>,
    fingerprint: String,
    atomic_replace: bool,
    fsync: bool,
}
impl SftpClient {
    pub async fn connect(
        connector: &SshConnector,
        config: &SshConnectionConfig,
        prompts: Arc<SshPrompts>,
    ) -> Result<Self> {
        let (ssh, fingerprint) = connector.connect(config, prompts).await?;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let channel = ssh.channel_open_session().await?;
            channel.request_subsystem(true, "sftp").await?;
            Self::initialize(channel.into_stream(), Some(ssh), fingerprint).await
        })
        .await
        .map_err(|_| anyhow!("启动 SFTP 超时"))?
    }
    async fn initialize<S>(stream: S, ssh: Option<SshHandle>, fingerprint: String) -> Result<Self>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (closed_tx, closed) = watch::channel(false);
        let raw = RawSftpSession::new(WatchedStream {
            inner: stream,
            closed: closed_tx,
        });
        let version = raw.init().await?;
        if version.version != 3 {
            bail!("服务器未提供 SFTP v3");
        }
        let atomic_replace = version
            .extensions
            .get("posix-rename@openssh.com")
            .is_some_and(|v| v == "1");
        let fsync = version
            .extensions
            .get("fsync@openssh.com")
            .is_some_and(|v| v == "1");
        Ok(Self {
            raw,
            _ssh: ssh,
            closed,
            fingerprint,
            atomic_replace,
            fsync,
        })
    }
    /// Only the Unix tests have a local `sftp-server` to talk to.
    #[cfg(all(test, unix))]
    pub(super) async fn local_test_server(
        directory: &std::path::Path,
    ) -> Result<(Self, tokio::process::Child)> {
        let executable = ["/usr/libexec/sftp-server", "/usr/lib/openssh/sftp-server"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .ok_or_else(|| anyhow!("本机没有 OpenSSH sftp-server"))?;
        let mut child = tokio::process::Command::new(executable)
            .current_dir(directory)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stream = tokio::io::join(child.stdout.take().unwrap(), child.stdin.take().unwrap());
        let client = Self::initialize(stream, None, "local-test-key".into()).await?;
        Ok((client, child))
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Resolves once the server side of the SFTP session has gone, which
    /// russh-sftp otherwise only tells the next request. It does not hold on
    /// to the client, so dropping the client still closes the connection.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut closed = self.closed.clone();
        async move {
            // An error means the stream itself is gone: closed too.
            let _ = closed.wait_for(|closed| *closed).await;
        }
    }
    pub async fn canonicalize(&self, path: &RemotePath) -> Result<RemotePath> {
        let response = self.raw.realpath(path.as_str()).await?;
        RemotePath::new(checked_text(
            &response
                .files
                .first()
                .ok_or_else(|| anyhow!("服务器未返回目录路径"))?
                .filename,
        )?)
    }
    /// A directory for the browser: canonical path, owners, and what each
    /// link points to (resolved concurrently, one `stat` per link).
    pub async fn list(&self, path: &RemotePath) -> Result<DirectoryListing> {
        let path = self.canonicalize(path).await?;
        let entries = self.read_dir(&path).await?;
        let entries = futures::stream::iter(entries)
            .map(|entry| {
                let path = &path;
                async move {
                    if entry.metadata().kind() != EntryKind::Symlink {
                        return Ok(entry);
                    }
                    let target = self
                        .stat(&path.join(entry.name())?)
                        .await
                        .or_else(|error| {
                            if is_network_error(&error) {
                                Err(error)
                            } else {
                                Ok(None)
                            }
                        })?;
                    Ok::<_, anyhow::Error>(entry.with_target_kind(target.map(|m| m.kind())))
                }
            })
            .buffered(16)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        Ok(DirectoryListing::new(path.as_str(), entries))
    }
}
impl RemoteFs for SftpClient {
    async fn metadata(&self, path: &RemotePath) -> Result<Option<FileMetadata>> {
        match self.raw.lstat(path.as_str()).await {
            Ok(attrs) => Ok(Some(metadata(&attrs.attrs))),
            Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    async fn stat(&self, path: &RemotePath) -> Result<Option<FileMetadata>> {
        match self.raw.stat(path.as_str()).await {
            Ok(attrs) => Ok(Some(metadata(&attrs.attrs))),
            Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    async fn read_dir(&self, path: &RemotePath) -> Result<Vec<DirectoryEntry>> {
        let handle = self.raw.opendir(path.as_str()).await?.handle;
        let result: Result<Vec<DirectoryEntry>> = async {
            let mut entries = Vec::new();
            loop {
                match self.raw.readdir(&handle).await {
                    Ok(page) => {
                        for entry in page.files {
                            if matches!(entry.filename.as_str(), "." | "..") {
                                continue;
                            }
                            checked_text(&entry.filename)?;
                            path.join(&entry.filename)?;
                            let (owner, group) = parse_longname(&entry.longname)
                                .map(|(owner, group)| (Some(owner), Some(group)))
                                .unwrap_or_else(|| {
                                    (
                                        entry.attrs.uid.map(|id| id.to_string()),
                                        entry.attrs.gid.map(|id| id.to_string()),
                                    )
                                });
                            entries.push(
                                DirectoryEntry::new(entry.filename, metadata(&entry.attrs))
                                    .with_owner(owner, group),
                            );
                        }
                    }
                    Err(Error::Status(status)) if status.status_code == StatusCode::Eof => break,
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(entries)
        }
        .await;
        let closed = self.raw.close(handle).await;
        let entries = result?;
        closed?;
        Ok(entries)
    }
    async fn open(&self, path: &RemotePath, create: bool) -> Result<String> {
        let flags = if create {
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE
        } else {
            OpenFlags::WRITE
        };
        Ok(self
            .raw
            .open(path.as_str(), flags, FileAttributes::empty())
            .await?
            .handle)
    }
    async fn open_read(&self, path: &RemotePath) -> Result<String> {
        Ok(self
            .raw
            .open(path.as_str(), OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle)
    }
    async fn read(&self, handle: &str, offset: u64, len: u32) -> Result<Option<Vec<u8>>> {
        match self.raw.read(handle, offset, len).await {
            Ok(data) => Ok(Some(data.data)),
            Err(Error::Status(status)) if status.status_code == StatusCode::Eof => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    async fn sync(&self, handle: &str) -> Result<()> {
        if self.fsync {
            self.raw.fsync(handle).await?;
        }
        Ok(())
    }
    async fn write(&self, handle: &str, offset: u64, bytes: Vec<u8>) -> Result<()> {
        self.raw.write(handle, offset, bytes).await?;
        Ok(())
    }
    async fn close(&self, handle: &str) -> Result<()> {
        self.raw.close(handle).await?;
        Ok(())
    }

    async fn attributes(
        &self,
        handle: &str,
        modified: Option<u32>,
        permissions: Option<u32>,
    ) -> Result<()> {
        self.raw
            .fsetstat(
                handle,
                FileAttributes {
                    mtime: modified,
                    atime: modified,
                    permissions: permissions.map(|p| p & 0o777),
                    ..FileAttributes::empty()
                },
            )
            .await?;
        Ok(())
    }
    async fn mkdir(&self, path: &RemotePath) -> Result<()> {
        self.raw
            .mkdir(path.as_str(), FileAttributes::empty())
            .await?;
        Ok(())
    }
    async fn symlink(&self, target: &str, path: &RemotePath) -> Result<()> {
        self.raw.symlink(target, path.as_str()).await?;
        Ok(())
    }
    async fn readlink(&self, path: &RemotePath) -> Result<String> {
        checked_text(
            &self
                .raw
                .readlink(path.as_str())
                .await?
                .files
                .first()
                .ok_or_else(|| anyhow!("无法读取链接目标"))?
                .filename,
        )
    }
    async fn rename(&self, from: &RemotePath, to: &RemotePath, replace: bool) -> Result<()> {
        if replace {
            let mut payload = Vec::new();
            for value in [from.as_str(), to.as_str()] {
                payload.extend_from_slice(&(value.len() as u32).to_be_bytes());
                payload.extend_from_slice(value.as_bytes());
            }
            match self
                .raw
                .extended("posix-rename@openssh.com", payload)
                .await?
            {
                Packet::Status(status) if status.status_code == StatusCode::Ok => {}
                Packet::Status(status) => return Err(Error::Status(status).into()),
                _ => bail!("服务器返回了无效的重命名响应"),
            }
        } else {
            self.raw.rename(from.as_str(), to.as_str()).await?;
        }
        Ok(())
    }
    async fn remove(&self, path: &RemotePath) -> Result<()> {
        match self.raw.remove(path.as_str()).await {
            Ok(_) => Ok(()),
            Err(Error::Status(s)) if s.status_code == StatusCode::NoSuchFile => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    async fn rmdir(&self, path: &RemotePath) -> Result<()> {
        self.raw.rmdir(path.as_str()).await?;
        Ok(())
    }
    async fn set_permissions(&self, path: &RemotePath, permissions: u32) -> Result<()> {
        self.raw
            .setstat(
                path.as_str(),
                FileAttributes {
                    permissions: Some(permissions & 0o7777),
                    ..FileAttributes::empty()
                },
            )
            .await?;
        Ok(())
    }
    fn atomic_replace(&self) -> bool {
        self.atomic_replace
    }
}
fn metadata(attrs: &FileAttributes) -> FileMetadata {
    // Compare the whole type field: russh-sftp's `is_dir()` tests one bit,
    // which sockets and block devices also carry.
    let kind = match attrs.permissions.map(|mode| mode & 0o170_000) {
        Some(0o040_000) => EntryKind::Directory,
        Some(0o120_000) => EntryKind::Symlink,
        Some(0o100_000) => EntryKind::File,
        _ => EntryKind::Other,
    };
    FileMetadata::new(
        kind,
        attrs.size.unwrap_or(0),
        attrs.mtime,
        attrs.permissions,
    )
}
/// The SFTP stream, telling when the server side has ended. russh ends it
/// when the channel or the whole SSH connection closes, including when
/// keepalives go unanswered after the network drops; russh-sftp keeps that
/// to itself until the next request fails. Watching it lets an idle SFTP tab
/// notice at the same moment a terminal on the same host does.
struct WatchedStream<S> {
    inner: S,
    closed: watch::Sender<bool>,
}

impl<S: AsyncRead + Unpin> AsyncRead for WatchedStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let wanted = buf.remaining() > 0;
        let before = buf.filled().len();
        let poll = Pin::new(&mut this.inner).poll_read(cx, buf);
        // Nothing read into room for something is the end of the stream.
        if let Poll::Ready(result) = &poll
            && (result.is_err() || (wanted && buf.filled().len() == before))
        {
            this.closed.send_replace(true);
        }
        poll
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for WatchedStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

pub(crate) fn is_network_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if cause.downcast_ref::<russh::Error>().is_some_and(|e| {
            matches!(
                e,
                russh::Error::Disconnect
                    | russh::Error::HUP
                    | russh::Error::ConnectionTimeout
                    | russh::Error::KeepaliveTimeout
                    | russh::Error::InactivityTimeout
            )
        }) {
            return true;
        }
        cause.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe
            )
        }) || cause
            .downcast_ref::<Error>()
            .is_some_and(|error| match error {
                Error::IO(_) | Error::Timeout => true,
                // `session closed`: the SSH channel under the SFTP session is
                // gone, as when the network drops; russh-sftp says so before
                // sending anything more.
                Error::UnexpectedBehavior(message) => {
                    message == "sender dropped"
                        || message == "session closed"
                        || message.contains("SendError")
                        || message.contains("RecvError")
                }
                Error::Status(status) => matches!(
                    status.status_code,
                    StatusCode::ConnectionLost | StatusCode::NoConnection
                ),
                _ => false,
            })
    })
}

/// Owner and group from an SFTP v3 `longname`, the server's `ls -l` line:
/// `drwxr-xr-x    2 root     root         4096 Jan  1 12:00 name`. `None` when
/// the line does not have that shape; callers fall back to numeric ids.
pub(crate) fn parse_longname(line: &str) -> Option<(String, String)> {
    let mut fields = line.split_whitespace();
    let mode = fields.next()?;
    let mut chars = mode.chars();
    if !chars.next().is_some_and(|c| "-dlcbpsD".contains(c))
        || chars.by_ref().take(9).count() != 9
        || !mode
            .chars()
            .skip(1)
            .take(9)
            .all(|c| "rwxsStTl-".contains(c))
    {
        return None;
    }
    fields.next()?.parse::<u64>().ok()?;
    let owner = fields.next()?;
    let group = fields.next()?;
    fields.next()?.parse::<u64>().ok()?;
    Some((owner.to_string(), group.to_string()))
}

// russh-sftp v3 decodes wire strings lossily. Refuse ambiguous names before
// they can become a path for navigation or mutation.
fn checked_text(value: &str) -> Result<String> {
    if value.contains('\u{fffd}') {
        bail!("远端名称包含无法可靠表示的字符，已停止操作");
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod longname_tests {
    use super::parse_longname;

    #[test]
    fn parses_openssh_and_rejects_other_shapes() {
        assert_eq!(
            parse_longname("drwxr-xr-x    2 root     wheel        4096 Jan  1 12:00 bin"),
            Some(("root".into(), "wheel".into()))
        );
        assert_eq!(
            parse_longname("-rw-r--r--+   1 用户 staff 12 Sep 24 10:36 文件 甲.txt"),
            Some(("用户".into(), "staff".into()))
        );
        assert_eq!(
            parse_longname("lrwxrwxrwx 1 0 0 7 Apr 22  2024 bin -> usr/bin"),
            Some(("0".into(), "0".into()))
        );
        assert_eq!(parse_longname(""), None);
        assert_eq!(parse_longname("bin"), None);
        assert_eq!(parse_longname("drwxr-xr-x root root 4096 Jan 1 bin"), None);
        assert_eq!(
            parse_longname("xrwxr-xr-x 1 root root 1 Jan 1 12:00 x"),
            None
        );
    }
}

#[cfg(test)]
mod network_error_tests {
    use super::{Error, is_network_error};

    #[test]
    fn a_closed_session_is_a_lost_connection() {
        let closed = anyhow::Error::new(Error::UnexpectedBehavior("session closed".into()))
            .context("无法读取目录");
        assert!(is_network_error(&closed));
        let other = anyhow::Error::new(Error::UnexpectedBehavior("bad packet".into()));
        assert!(!is_network_error(&other));
    }
}
