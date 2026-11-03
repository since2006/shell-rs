//! The channels a live connection opens: the shell on a pseudo-terminal, a
//! command run beside it, and SFTP. Each fails with a [`ChannelError`].

use std::time::Duration;

use anyhow::{Result, anyhow};
use russh::{Channel, ChannelMsg, client};
use tokio::io::{AsyncRead, AsyncWrite};

use super::connection::SshHandle;
use super::error::ChannelError;

/// A byte stream on a connection, for a protocol beside the shell, or the
/// way through a jump host or proxy.
pub trait ChannelStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> ChannelStream for T {}

/// An SFTP channel on a connection, and the fingerprint of the host key that
/// connection checked, which resumed transfers verify.
pub struct SftpChannel {
    pub stream: Box<dyn ChannelStream>,
    pub fingerprint: String,
}

/// A terminal's size, in cells and in pixels. Pixels may be 0 when unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PtySize {
    pub columns: u32,
    pub rows: u32,
    pub width: u32,
    pub height: u32,
}

/// Open a session channel and start the user's shell on a pseudo-terminal
/// of type `term`.
pub async fn open_shell(
    handle: &SshHandle,
    term: &str,
    size: PtySize,
) -> Result<Channel<client::Msg>> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|_| anyhow!(ChannelError::SessionFailed))?;
    channel
        .request_pty(
            true,
            term,
            size.columns,
            size.rows,
            size.width,
            size.height,
            &[],
        )
        .await
        .map_err(|_| anyhow!(ChannelError::PtyRefused))?;
    channel
        .request_shell(true)
        .await
        .map_err(|_| anyhow!(ChannelError::ShellRefused))?;
    Ok(channel)
}

/// Tell the shell its terminal changed size.
pub async fn resize_pty(channel: &Channel<client::Msg>, size: PtySize) -> Result<()> {
    channel
        .window_change(size.columns, size.rows, size.width, size.height)
        .await
        .map_err(|_| anyhow!(ChannelError::ResizeFailed))
}

/// Type `bytes` into the shell.
pub async fn write_shell(channel: &Channel<client::Msg>, bytes: Vec<u8>) -> Result<()> {
    channel
        .data_bytes(bytes)
        .await
        .map_err(|_| anyhow!(ChannelError::WriteFailed))
}

/// Run `command` on a channel of its own and give back what it printed on
/// standard output, at most `limit` bytes of it; its standard error is
/// dropped. Gives up after `timeout`.
pub async fn run_exec(
    handle: &SshHandle,
    command: &str,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>> {
    tokio::time::timeout(timeout, async {
        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|_| anyhow!(ChannelError::ExecChannelFailed))?;
        channel
            .exec(true, command)
            .await
            .map_err(|_| anyhow!(ChannelError::ExecSendFailed))?;
        let mut output = Vec::new();
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    let room = limit.saturating_sub(output.len());
                    output.extend_from_slice(&data[..data.len().min(room)]);
                }
                Some(ChannelMsg::Failure) => return Err(anyhow!(ChannelError::ExecRefused)),
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
        Ok(output)
    })
    .await
    .unwrap_or_else(|_| Err(anyhow!(ChannelError::ExecTimeout)))
}

/// Open an SFTP channel. Gives up after `timeout`.
pub async fn open_sftp_channel(
    handle: &SshHandle,
    timeout: Duration,
) -> Result<Box<dyn ChannelStream>> {
    tokio::time::timeout(timeout, async {
        let mut channel = handle
            .channel_open_session()
            .await
            .map_err(|_| anyhow!(ChannelError::ExecChannelFailed))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|_| anyhow!(ChannelError::SftpRefused))?;
        // russh sends the request without waiting for the answer: wait here,
        // so a server without SFTP says so rather than ending the stream,
        // which would read as a dropped connection worth retrying.
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => break,
                Some(ChannelMsg::Failure | ChannelMsg::Eof | ChannelMsg::Close) | None => {
                    return Err(anyhow!(ChannelError::SftpRefused));
                }
                _ => {}
            }
        }
        Ok(Box::new(channel.into_stream()) as Box<dyn ChannelStream>)
    })
    .await
    .unwrap_or_else(|_| Err(anyhow!(ChannelError::SftpTimeout)))
}

/// Open a channel and run one probe command on it (see
/// [`HostOsProbe`](super::probe::HostOsProbe)). Best effort throughout: a
/// server that refuses the channel or the command gives `None`.
pub async fn open_probe(
    handle: &SshHandle,
    command: Option<&'static str>,
) -> Option<Channel<client::Msg>> {
    let command = command?;
    let channel = handle.channel_open_session().await.ok()?;
    // No reply wanted: the answer is the output, and waiting for the
    // acknowledgement would cost another round trip for nothing.
    channel.exec(false, command).await.ok()?;
    Some(channel)
}
