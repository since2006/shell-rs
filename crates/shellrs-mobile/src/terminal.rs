//! A shell on a pseudo-terminal, over a channel of a connection.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use russh::{Channel, ChannelMsg, client};
use shellrs_ssh::{self as ssh, PtySize};
use tokio::sync::{Notify, mpsc};

use crate::connection::{Inner, State};
use crate::failure::{Failure, SshError};

/// The most output one [`TerminalEvent::Output`] carries.
const CHUNK: usize = 64 * 1024;
/// The most output kept for the app to take. Beyond it the oldest goes,
/// rather than the connection stalling: russh reads on only as fast as its
/// channels are emptied, and a stalled connection misses its keepalives.
const KEPT: usize = 4 * 1024 * 1024;
/// How long closing waits for the server to hear of it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TerminalOptions {
    /// The terminal type the shell is told, such as `xterm-256color`.
    pub term: String,
    pub columns: u32,
    pub rows: u32,
    /// The size in pixels, or 0 when unknown.
    pub width: u32,
    pub height: u32,
}

impl TerminalOptions {
    pub(crate) fn size(&self) -> PtySize {
        PtySize {
            columns: self.columns,
            rows: self.rows,
            width: self.width,
            height: self.height,
        }
    }
}

/// What a terminal says, in order.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TerminalEvent {
    /// What the shell printed, at most 64 KiB at a time, as it came: a
    /// character or escape sequence may be split between two.
    Output { bytes: Vec<u8> },
    /// The app fell behind and this much of the oldest output was dropped:
    /// what follows does not continue what came before.
    OutputDropped { bytes: u64 },
    /// The shell ended, with its exit code or the signal that ended it
    /// when the server says. The last event.
    Exited {
        code: Option<u32>,
        signal: Option<String>,
    },
    /// The terminal stopped working: its connection dropped, or the
    /// server refused what was typed. The last event.
    Failed { failure: Failure },
}

/// A remote shell. Type into it with [`write`](Self::write), take what it
/// prints from [`next_event`](Self::next_event). It closes on
/// [`end`](Self::end), when its connection does, or once nothing holds
/// it, a call waiting on it included.
#[derive(uniffi::Object)]
pub struct Terminal {
    /// Holds the connection open as long as the terminal is.
    _connection: Arc<Inner>,
    commands: mpsc::UnboundedSender<Command>,
    output: Arc<Output>,
}

enum Command {
    Write(Vec<u8>),
    Resize(PtySize),
    Close,
}

impl Terminal {
    pub(crate) fn start(connection: Arc<Inner>, channel: Channel<client::Msg>) -> Arc<Self> {
        let (commands, commands_rx) = mpsc::unbounded_channel();
        let output = Arc::new(Output::new());
        connection.runtime.spawn(pump(
            connection.clone(),
            channel,
            commands_rx,
            output.clone(),
        ));
        Arc::new(Self {
            _connection: connection,
            commands,
            output,
        })
    }
}

#[uniffi::export]
impl Terminal {
    /// Type `bytes`: keystrokes as the terminal encodes them, or a paste.
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), SshError> {
        self.send(Command::Write(bytes))
    }

    /// Tell the shell the terminal's new size.
    pub fn resize(&self, columns: u32, rows: u32, width: u32, height: u32) -> Result<(), SshError> {
        self.send(Command::Resize(PtySize {
            columns,
            rows,
            width,
            height,
        }))
    }

    /// Close the shell's channel. The connection stays. (Not `close`,
    /// which Kotlin's `AutoCloseable` takes.)
    pub fn end(&self) {
        let _ = self.commands.send(Command::Close);
    }

    /// The next event; `None` after the last, or once closed.
    pub async fn next_event(&self) -> Option<TerminalEvent> {
        self.output.next().await
    }
}

impl Terminal {
    fn send(&self, command: Command) -> Result<(), SshError> {
        self.commands.send(command).map_err(|_| SshError::Closed)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Close);
    }
}

/// Read the channel dry as output comes, and send what the app types.
async fn pump(
    connection: Arc<Inner>,
    mut channel: Channel<client::Msg>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    output: Arc<Output>,
) {
    let mut code = None;
    let mut signal = None;
    let end = loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Write(bytes)) => {
                    if let Err(error) = ssh::write_shell(&channel, bytes).await {
                        break failed(&connection, &error);
                    }
                }
                Some(Command::Resize(size)) => {
                    if let Err(error) = ssh::resize_pty(&channel, size).await {
                        break failed(&connection, &error);
                    }
                }
                Some(Command::Close) | None => {
                    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
                        let _ = channel.eof().await;
                        let _ = channel.close().await;
                    })
                    .await;
                    break None;
                }
            },
            message = channel.wait() => match message {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    output.push(&data);
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    signal = Some(format!("{signal_name:?}"));
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) => {
                    break Some(TerminalEvent::Exited { code, signal });
                }
                // The connection closed under it: as the app asked, or not.
                None if *connection.shutdown.borrow() => break None,
                None if code.is_none() && signal.is_none() && !connected(&connection) => {
                    break Some(TerminalEvent::Failed {
                        failure: Failure::lost(),
                    });
                }
                None => break Some(TerminalEvent::Exited { code, signal }),
                Some(_) => {}
            },
        }
    };
    output.finish(end);
}

/// The terminal's end over `error`: the connection's dropping, if it has.
fn failed(connection: &Inner, error: &anyhow::Error) -> Option<TerminalEvent> {
    let failure = if connected(connection) {
        Failure::ssh(error)
    } else {
        Failure::lost()
    };
    Some(TerminalEvent::Failed { failure })
}

fn connected(connection: &Inner) -> bool {
    match &*connection.state.borrow() {
        State::Up { handle, .. } => !handle.is_closed(),
        State::Connecting | State::Closed => false,
    }
}

/// The shell's output waiting for the app, merged into chunks.
struct Output {
    state: Mutex<OutputState>,
    ready: Notify,
}

#[derive(Default)]
struct OutputState {
    bytes: VecDeque<u8>,
    dropped: u64,
    end: Option<TerminalEvent>,
    finished: bool,
}

impl Output {
    fn new() -> Self {
        Self {
            state: Mutex::new(OutputState::default()),
            ready: Notify::new(),
        }
    }

    fn push(&self, data: &[u8]) {
        {
            let mut state = self.lock();
            state.bytes.extend(data);
            let excess = state.bytes.len().saturating_sub(KEPT);
            if excess > 0 {
                state.bytes.drain(..excess);
                state.dropped += excess as u64;
            }
        }
        self.ready.notify_waiters();
    }

    /// No more output; `end` is the last event, if any.
    fn finish(&self, end: Option<TerminalEvent>) {
        {
            let mut state = self.lock();
            state.end = end;
            state.finished = true;
        }
        self.ready.notify_waiters();
    }

    async fn next(&self) -> Option<TerminalEvent> {
        loop {
            let notified = self.ready.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = self.lock();
                if state.dropped > 0 {
                    let bytes = std::mem::take(&mut state.dropped);
                    return Some(TerminalEvent::OutputDropped { bytes });
                }
                if !state.bytes.is_empty() {
                    let take = state.bytes.len().min(CHUNK);
                    let bytes = state.bytes.drain(..take).collect();
                    return Some(TerminalEvent::Output { bytes });
                }
                if state.finished {
                    return state.end.take();
                }
            }
            notified.await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, OutputState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::{CHUNK, KEPT, Output, TerminalEvent};

    #[tokio::test]
    async fn output_comes_merged_in_chunks_and_then_the_end() {
        let output = Output::new();
        output.push(b"ab");
        output.push(b"cd");
        output.push(&vec![b'x'; CHUNK + 1]);
        output.finish(Some(TerminalEvent::Exited {
            code: Some(0),
            signal: None,
        }));
        let Some(TerminalEvent::Output { bytes }) = output.next().await else {
            panic!("output first");
        };
        assert_eq!(bytes.len(), CHUNK);
        assert!(bytes.starts_with(b"abcdx"));
        let Some(TerminalEvent::Output { bytes }) = output.next().await else {
            panic!("the rest of the output");
        };
        assert_eq!(bytes.len(), 5);
        assert_eq!(
            output.next().await,
            Some(TerminalEvent::Exited {
                code: Some(0),
                signal: None,
            })
        );
        assert_eq!(output.next().await, None);
    }

    #[tokio::test]
    async fn an_app_that_falls_behind_loses_the_oldest_output_and_is_told() {
        let output = Output::new();
        output.push(&vec![b'a'; KEPT]);
        output.push(b"newest");
        assert_eq!(
            output.next().await,
            Some(TerminalEvent::OutputDropped { bytes: 6 })
        );
        let mut kept = Vec::new();
        output.finish(None);
        while let Some(TerminalEvent::Output { bytes }) = output.next().await {
            kept.extend(bytes);
        }
        assert_eq!(kept.len(), KEPT);
        assert!(kept.ends_with(b"newest"));
    }
}
