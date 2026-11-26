//! The channels a live connection opens: the shell on a pseudo-terminal, a
//! command run beside it, and SFTP. Each fails with a [`ChannelError`].

use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

use anyhow::{Result, anyhow};
use russh::{Channel, ChannelMsg, Sig, client};
use tokio::io::{AsyncRead, AsyncWrite};

use super::connection::SshHandle;
use super::error::ChannelError;
use super::exec::ExecExit;

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

/// A command started on a channel of its own by [`open_exec`], its output
/// not read yet. It holds no borrow of the connection, so it can be read on
/// another runtime than the connection's, as an SFTP channel is.
pub struct ExecChannel {
    channel: Channel<client::Msg>,
}

impl std::fmt::Debug for ExecChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExecChannel")
    }
}

/// Open a channel and start `command` on it, with its standard input closed
/// so a program waiting for input ends instead of hanging. Gives up after
/// `timeout`; the command itself runs for as long as
/// [`ExecChannel::finish`] waits.
pub async fn open_exec(
    handle: &SshHandle,
    command: &str,
    timeout: Duration,
) -> Result<ExecChannel> {
    tokio::time::timeout(timeout, async {
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|_| anyhow!(ChannelError::ExecChannelFailed))?;
        channel
            .exec(true, command)
            .await
            .map_err(|_| anyhow!(ChannelError::ExecSendFailed))?;
        channel
            .eof()
            .await
            .map_err(|_| anyhow!(ChannelError::ExecSendFailed))?;
        Ok(ExecChannel { channel })
    })
    .await
    .unwrap_or_else(|_| Err(anyhow!(ChannelError::ExecTimeout)))
}

/// How much of each of a command's outputs to keep: its first `head` bytes
/// and its last `tail`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecLimits {
    pub head: usize,
    pub tail: usize,
}

/// The start and the end of one of a command's outputs, and how long it
/// was: a command may print far more than anyone reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptBytes {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    head_room: usize,
    tail_room: usize,
    total: u64,
}

impl KeptBytes {
    pub fn new(limits: ExecLimits) -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            head_room: limits.head,
            tail_room: limits.tail,
            total: 0,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len() as u64);
        let into_head = bytes.len().min(self.head_room - self.head.len());
        self.head.extend_from_slice(&bytes[..into_head]);
        let rest = &bytes[into_head..];
        if rest.len() >= self.tail_room {
            self.tail.clear();
            self.tail.extend(&rest[rest.len() - self.tail_room..]);
        } else {
            self.tail.extend(rest);
            let over = self.tail.len().saturating_sub(self.tail_room);
            self.tail.drain(..over);
        }
    }

    /// The first bytes, up to the head's limit.
    pub fn head(&self) -> &[u8] {
        &self.head
    }

    /// The last bytes after the head, up to the tail's limit.
    pub fn tail(&self) -> Vec<u8> {
        self.tail.iter().copied().collect()
    }

    /// How many bytes there were in all.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// How many bytes between the head and the tail were not kept.
    pub fn skipped(&self) -> u64 {
        self.total - (self.head.len() + self.tail.len()) as u64
    }
}

/// How a command on a channel of its own came to an end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecEnd {
    /// It ended: with its status or signal, when the server said.
    Exited(Option<ExecExit>),
    /// It was stopped: sent `TERM`, and its channel closed. A server that
    /// ignores signals (older OpenSSH) may leave it running.
    Stopped,
}

/// What a command on a channel of its own printed, and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutcome {
    pub stdout: KeptBytes,
    pub stderr: KeptBytes,
    pub end: ExecEnd,
}

impl ExecChannel {
    /// Read the command's output until its channel closes, or until `stop`
    /// completes, which stops it. The exit status can come after the last
    /// output, so the channel is read until it closes, not just to its EOF.
    pub async fn finish(
        mut self,
        limits: ExecLimits,
        stop: impl Future<Output = ()>,
    ) -> Result<ExecOutcome> {
        let mut stdout = KeptBytes::new(limits);
        let mut stderr = KeptBytes::new(limits);
        let mut exit = None;
        let mut stop = std::pin::pin!(stop);
        loop {
            tokio::select! {
                message = self.channel.wait() => match message {
                    Some(ChannelMsg::Data { data }) => stdout.push(&data),
                    Some(ChannelMsg::ExtendedData { data, ext: 1 }) => stderr.push(&data),
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        exit = Some(ExecExit::Code(exit_status));
                    }
                    Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                        exit = Some(ExecExit::Signal(format!("{signal_name:?}")));
                    }
                    Some(ChannelMsg::Failure) => return Err(anyhow!(ChannelError::ExecRefused)),
                    Some(ChannelMsg::Close) => break,
                    // The connection went away under the command: once it
                    // said how it ended, that is the end.
                    None if exit.is_some() => break,
                    None => return Err(anyhow!(ChannelError::ExecLost)),
                    Some(_) => {}
                },
                () = &mut stop => {
                    let _ = self.channel.signal(Sig::TERM).await;
                    let _ = self.channel.close().await;
                    return Ok(ExecOutcome { stdout, stderr, end: ExecEnd::Stopped });
                }
            }
        }
        Ok(ExecOutcome {
            stdout,
            stderr,
            end: ExecEnd::Exited(exit),
        })
    }
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Weak};

    use tokio::sync::watch;
    use zeroize::Zeroizing;

    use super::*;
    use crate::testing::{ExecReply, ReplyEnd, TEST_PASSWORD, TEST_USER, TestServer};
    use crate::{
        ConnectionPromptReply, HostLogin, LoginMethod, SecretRef, SecretSource,
        SshConnectionConfig, SshConnector, SshPrompts,
    };

    const LIMITS: ExecLimits = ExecLimits {
        head: 1024,
        tail: 1024,
    };

    struct Saved;

    impl SecretSource for Saved {
        fn secret(&self, _: &SecretRef) -> Result<Option<Zeroizing<String>>> {
            Ok(Some(Zeroizing::new(TEST_PASSWORD.into())))
        }
    }

    /// Log in to `server`, trusting its key. The login is over before this
    /// returns, so dropping its shutdown switch cancels nothing.
    async fn connect(server: &TestServer, data: &std::path::Path) -> SshHandle {
        let connector = SshConnector::new(data.join("known_hosts"), Arc::new(Saved));
        let login = HostLogin::new("127.0.0.1", server.port, TEST_USER, LoginMethod::Password);
        let (_shutdown, shutdown) = watch::channel(false);
        let broker = Arc::new_cyclic(|broker: &Weak<SshPrompts>| {
            let broker = broker.clone();
            SshPrompts::new(
                Arc::new(move |prompt| {
                    if let Some(broker) = broker.upgrade() {
                        broker.respond(prompt.request_id(), ConnectionPromptReply::TrustAndSave);
                    }
                    true
                }),
                shutdown,
            )
        });
        let (handle, _) = tokio::time::timeout(
            Duration::from_secs(20),
            connector.connect(&SshConnectionConfig::from(&login), broker),
        )
        .await
        .expect("in time")
        .unwrap();
        handle
    }

    fn never() -> std::future::Pending<()> {
        std::future::pending()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_command_gives_its_outputs_apart_and_its_status() {
        let data = tempfile::tempdir().unwrap();
        let server = TestServer::start(data.path().into()).await.unwrap();
        server.reply_with(
            "make",
            ExecReply {
                stdout: "built\n".into(),
                stderr: "warning: unused\n".into(),
                end: ReplyEnd::Status(2),
            },
        );
        let handle = connect(&server, data.path()).await;

        let outcome = open_exec(&handle, "make", Duration::from_secs(5))
            .await
            .unwrap()
            .finish(LIMITS, never())
            .await
            .unwrap();

        assert_eq!(outcome.stdout.head(), b"built\n");
        assert_eq!(outcome.stderr.head(), b"warning: unused\n");
        assert_eq!(outcome.end, ExecEnd::Exited(Some(ExecExit::Code(2))));
        assert_eq!(server.seen().commands, ["make"]);
        crate::hang_up(handle, "test finished").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stopping_a_command_signals_it_and_says_so() {
        let data = tempfile::tempdir().unwrap();
        let server = TestServer::start(data.path().into()).await.unwrap();
        server.reply_with(
            "tail -f log",
            ExecReply {
                stdout: "first line\n".into(),
                stderr: String::new(),
                end: ReplyEnd::Runs,
            },
        );
        let handle = connect(&server, data.path()).await;

        let outcome = open_exec(&handle, "tail -f log", Duration::from_secs(5))
            .await
            .unwrap()
            .finish(LIMITS, tokio::time::sleep(Duration::from_millis(300)))
            .await
            .unwrap();

        assert_eq!(outcome.end, ExecEnd::Stopped);
        assert_eq!(outcome.stdout.head(), b"first line\n");
        let signalled = async {
            while server.seen().signals.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), signalled)
            .await
            .expect("the server heard the signal");
        assert_eq!(server.seen().signals, ["TERM"]);
        crate::hang_up(handle, "test finished").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_command_the_server_refuses_fails_as_refused() {
        let data = tempfile::tempdir().unwrap();
        let server = TestServer::start(data.path().into()).await.unwrap();
        server.reply_with(
            "forbidden",
            ExecReply {
                stdout: String::new(),
                stderr: String::new(),
                end: ReplyEnd::Refused,
            },
        );
        let handle = connect(&server, data.path()).await;

        let error = open_exec(&handle, "forbidden", Duration::from_secs(5))
            .await
            .unwrap()
            .finish(LIMITS, never())
            .await
            .unwrap_err();

        assert_eq!(
            crate::error::find_cause::<ChannelError>(&error),
            Some(&ChannelError::ExecRefused)
        );
        crate::hang_up(handle, "test finished").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connection_closing_under_a_command_is_lost_not_an_exit() {
        let data = tempfile::tempdir().unwrap();
        let server = TestServer::start(data.path().into()).await.unwrap();
        server.reply_with(
            "sleep 600",
            ExecReply {
                stdout: String::new(),
                stderr: String::new(),
                end: ReplyEnd::Runs,
            },
        );
        let handle = connect(&server, data.path()).await;
        let channel = open_exec(&handle, "sleep 600", Duration::from_secs(5))
            .await
            .unwrap();

        // On another runtime than the connection's, as the assistant reads it.
        let reading = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(channel.finish(LIMITS, never()))
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        crate::hang_up(handle, "test finished").await;
        let error = tokio::task::spawn_blocking(move || reading.join().unwrap())
            .await
            .unwrap()
            .unwrap_err();

        assert_eq!(
            crate::error::find_cause::<ChannelError>(&error),
            Some(&ChannelError::ExecLost)
        );
    }

    #[test]
    fn kept_bytes_keep_the_start_and_the_end_and_count_the_rest() {
        let mut kept = KeptBytes::new(ExecLimits { head: 4, tail: 3 });
        for chunk in [&b"ab"[..], b"cdef", b"ghij", b"k"] {
            kept.push(chunk);
        }
        assert_eq!(kept.head(), b"abcd");
        assert_eq!(kept.tail(), b"ijk");
        assert_eq!(kept.total(), 11);
        assert_eq!(kept.skipped(), 4);

        let mut short = KeptBytes::new(ExecLimits { head: 4, tail: 3 });
        short.push(b"abcdef");
        assert_eq!((short.head(), short.tail()), (&b"abcd"[..], b"ef".to_vec()));
        assert_eq!(short.skipped(), 0);

        let mut big = KeptBytes::new(ExecLimits { head: 2, tail: 2 });
        big.push(b"0123456789");
        assert_eq!((big.head(), big.tail()), (&b"01"[..], b"89".to_vec()));
        assert_eq!(big.skipped(), 6);
    }
}
