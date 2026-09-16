use super::{DirectoryEntry, DirectoryListing, EntryKind, FileMetadata, RemotePath};
use crate::ssh::{SshConnectionConfig, SshConnector, SshHandle, SshPrompts};
use anyhow::{Result, anyhow, bail};
use russh_sftp::{
    client::{RawSftpSession, error::Error},
    protocol::{FileAttributes, OpenFlags, Packet, StatusCode},
};
use std::sync::Arc;

/// Small protocol seam, also implemented by deterministic fault-injection tests.
pub(crate) trait RemoteFs {
    async fn metadata(&self, path: &RemotePath) -> Result<Option<FileMetadata>>;
    async fn open(&self, path: &RemotePath, create: bool) -> Result<String>;
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
    fn atomic_replace(&self) -> bool;
}

pub(crate) struct SftpClient {
    raw: RawSftpSession,
    _ssh: Option<SshHandle>,
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
            Self::initialize(
                RawSftpSession::new(channel.into_stream()),
                Some(ssh),
                fingerprint,
            )
            .await
        })
        .await
        .map_err(|_| anyhow!("启动 SFTP 超时"))?
    }
    async fn initialize(
        raw: RawSftpSession,
        ssh: Option<SshHandle>,
        fingerprint: String,
    ) -> Result<Self> {
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
            fingerprint,
            atomic_replace,
            fsync,
        })
    }
    #[cfg(test)]
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
        let client =
            Self::initialize(RawSftpSession::new(stream), None, "local-test-key".into()).await?;
        Ok((client, child))
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
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
    pub async fn list(&self, path: &RemotePath) -> Result<DirectoryListing> {
        let path = self.canonicalize(path).await?;
        let handle = self.raw.opendir(path.as_str()).await?.handle;
        let result: Result<DirectoryListing> = async {
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
                            entries
                                .push(DirectoryEntry::new(entry.filename, metadata(&entry.attrs)));
                        }
                    }
                    Err(Error::Status(status)) if status.status_code == StatusCode::Eof => break,
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(DirectoryListing::new(path.as_str(), entries))
        }
        .await;
        let closed = self.raw.close(handle).await;
        let listing = result?;
        closed?;
        Ok(listing)
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
    fn atomic_replace(&self) -> bool {
        self.atomic_replace
    }
}
fn metadata(attrs: &FileAttributes) -> FileMetadata {
    let kind = if attrs.is_dir() {
        EntryKind::Directory
    } else if attrs.is_symlink() {
        EntryKind::Symlink
    } else if attrs.is_regular() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    FileMetadata::new(
        kind,
        attrs.size.unwrap_or(0),
        attrs.mtime,
        attrs.permissions,
    )
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
                Error::UnexpectedBehavior(message) => {
                    message == "sender dropped"
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

// russh-sftp v3 decodes wire strings lossily. Refuse ambiguous names before
// they can become a path for navigation or mutation.
fn checked_text(value: &str) -> Result<String> {
    if value.contains('\u{fffd}') {
        bail!("远端名称包含无法可靠表示的字符，已停止操作");
    }
    Ok(value.to_string())
}
