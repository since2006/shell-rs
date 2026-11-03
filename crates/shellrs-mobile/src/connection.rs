//! One logged-in connection, carrying the terminals and SFTP sessions
//! opened on it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use shellrs_ssh::{
    self as ssh, ConnectionPrompt, LATENCY_INTERVAL, Latency, SshConnectionConfig, SshConnector,
    SshHandle, SshPrompts,
};
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use crate::events::Events;
use crate::failure::{Failure, FailureLink, SshError};
use crate::prompt::{Prompt, PromptReply};
use crate::sftp::SftpSession;
use crate::task::{self, AbortOnDrop};
use crate::terminal::{Terminal, TerminalOptions};

/// How long a command may run, and how much of its output is kept.
const EXEC_TIMEOUT: Duration = Duration::from_secs(10);
const EXEC_OUTPUT_LIMIT: usize = 1 << 20;
/// How long closing waits for the server to hear of it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// What a connection says, in order.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectionEvent {
    /// A question for the user: answer it with [`Connection::answer`].
    Prompt { prompt: Prompt },
    /// Logged in; `fingerprint` is the host key's, as trusted.
    Connected { fingerprint: String },
    /// The latest round trip, every few seconds; `None` when the server
    /// did not answer in time. Only the newest waits to be taken.
    Latency { milliseconds: Option<u64> },
    /// The last event: logging in failed, the connection dropped, or
    /// `failure` is `None` after [`Connection::disconnect`].
    Closed { failure: Option<Failure> },
}

/// What a command printed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ExecOutput {
    /// Its standard output, up to 1 MiB; standard error is dropped.
    pub stdout: Vec<u8>,
}

/// A connection to a host. It logs in once; terminals and SFTP sessions
/// open on it as channels, which a bastion host allows where a second
/// login would not. It closes on [`disconnect`](Self::disconnect), or
/// once nothing holds it or anything opened on it, a call waiting on it
/// included.
#[derive(uniffi::Object)]
pub struct Connection {
    pub(crate) inner: Arc<Inner>,
}

/// What a connection's terminals and sessions hold on to.
pub(crate) struct Inner {
    pub(crate) runtime: Handle,
    pub(crate) connector: SshConnector,
    pub(crate) endpoint: String,
    pub(crate) data_dir: PathBuf,
    pub(crate) state: watch::Receiver<State>,
    events: Arc<Events<ConnectionEvent>>,
    prompts: Arc<SshPrompts>,
    /// Set when the app closes the connection.
    pub(crate) shutdown: watch::Sender<bool>,
}

#[derive(Clone)]
pub(crate) enum State {
    Connecting,
    Up {
        handle: Arc<SshHandle>,
        fingerprint: String,
    },
    Closed,
}

impl Connection {
    pub(crate) fn start(
        runtime: Handle,
        connector: SshConnector,
        login: ssh::HostLogin,
        data_dir: PathBuf,
    ) -> Arc<Self> {
        let (state_tx, state) = watch::channel(State::Connecting);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let events = Arc::new(Events::new());
        let prompts = Arc::new(SshPrompts::new(
            {
                let events = events.clone();
                Arc::new(move |prompt: ConnectionPrompt| {
                    events.push(ConnectionEvent::Prompt {
                        prompt: (&prompt).into(),
                    });
                    !events.is_finished()
                })
            },
            shutdown_rx.clone(),
        ));
        let inner = Arc::new(Inner {
            runtime: runtime.clone(),
            connector: connector.clone(),
            endpoint: login.endpoint(),
            data_dir,
            state,
            events: events.clone(),
            prompts: prompts.clone(),
            shutdown,
        });
        runtime.spawn(run(
            connector,
            login,
            prompts,
            state_tx,
            events,
            shutdown_rx,
        ));
        Arc::new(Self { inner })
    }
}

#[uniffi::export]
impl Connection {
    /// The next event; `None` after [`ConnectionEvent::Closed`].
    pub async fn next_event(&self) -> Option<ConnectionEvent> {
        self.inner.events.next().await
    }

    /// Answer the prompt `prompt_id`. An answer to a prompt no longer
    /// asked is dropped.
    pub fn answer(&self, prompt_id: u64, reply: PromptReply) {
        self.inner.prompts.respond(prompt_id, reply.into());
    }

    /// Open a shell on a pseudo-terminal; waits for logging in to finish.
    pub async fn open_terminal(&self, options: TerminalOptions) -> Result<Arc<Terminal>, SshError> {
        let handle = self.inner.up().await?;
        let size = options.size();
        let term = options.term;
        let channel = task::run(&self.inner.runtime, async move {
            ssh::open_shell(&handle, &term, size)
                .await
                .map_err(|error| SshError::ssh(&error))
        })
        .await?;
        Ok(Terminal::start(self.inner.clone(), channel))
    }

    /// Start an SFTP session on a channel of this connection; waits for it
    /// to be ready.
    pub async fn open_sftp(&self) -> Result<Arc<SftpSession>, SshError> {
        self.inner.up().await?;
        SftpSession::start(self.inner.clone()).await
    }

    /// Run `command` on a channel of its own, as the user's shell would.
    pub async fn exec(&self, command: String) -> Result<ExecOutput, SshError> {
        let handle = self.inner.up().await?;
        task::run(&self.inner.runtime, async move {
            ssh::run_exec(&handle, &command, EXEC_OUTPUT_LIMIT, EXEC_TIMEOUT)
                .await
                .map(|stdout| ExecOutput { stdout })
                .map_err(|error| SshError::ssh(&error))
        })
        .await
    }

    /// Whether the connection still works, without waiting for its
    /// keepalive: the round trip in milliseconds. Ask when the app comes
    /// back to the foreground or the network changes; a connection that
    /// no longer works fails, and soon closes.
    pub async fn check_alive(&self) -> Result<u64, SshError> {
        let State::Up { handle, .. } = self.inner.state.borrow().clone() else {
            return Err(SshError::NotConnected);
        };
        task::run(&self.inner.runtime, async move {
            match ssh::round_trip(&handle).await {
                Latency::Measured(rtt) => Ok(rtt.as_millis() as u64),
                Latency::TimedOut if handle.is_closed() => Err(SshError::NotConnected),
                Latency::TimedOut => Err(SshError::Failed {
                    failure: Failure {
                        links: vec![FailureLink::TimedOut],
                        network: true,
                    },
                }),
            }
        })
        .await
    }

    /// Close the connection, and everything opened on it. Also stops
    /// logging in. (Not `close`, which Kotlin's `AutoCloseable` takes.)
    pub fn disconnect(&self) {
        self.inner.shutdown.send_replace(true);
    }
}

impl Inner {
    /// The connection once logged in; fails if it closes first.
    pub(crate) async fn up(&self) -> Result<Arc<SshHandle>, SshError> {
        let mut state = self.state.clone();
        let state = state
            .wait_for(|state| !matches!(state, State::Connecting))
            .await
            .map_err(|_| SshError::NotConnected)?;
        match &*state {
            State::Up { handle, .. } => Ok(handle.clone()),
            State::Connecting | State::Closed => Err(SshError::NotConnected),
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}

/// The connection's life: log in, then measure and watch it until it
/// drops or is closed.
async fn run(
    connector: SshConnector,
    login: ssh::HostLogin,
    prompts: Arc<SshPrompts>,
    state: watch::Sender<State>,
    events: Arc<Events<ConnectionEvent>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let closing = Closing {
        state,
        events: events.clone(),
    };
    let config = SshConnectionConfig::from(&login);
    let (handle, fingerprint, mut forwarded) =
        match connector.connect_forwarding(&config, prompts).await {
            Ok(connected) => connected,
            Err(error) => {
                let closed_by_user = *shutdown.borrow();
                closing.close((!closed_by_user).then(|| Failure::ssh(&error)));
                return;
            }
        };
    let handle = Arc::new(handle);
    closing.state.send_replace(State::Up {
        handle: handle.clone(),
        fingerprint: fingerprint.clone(),
    });
    events.push(ConnectionEvent::Connected { fingerprint });
    let _latency = AbortOnDrop(tokio::spawn(measure(handle.clone(), events.clone())));
    let failure = loop {
        tokio::select! {
            // Ends with the connection: its handler holds the sender.
            channel = forwarded.recv() => match channel {
                Some(_) => continue,
                None => break Some(Failure::lost()),
            },
            // Closed, or everything holding it dropped.
            _ = async { shutdown.wait_for(|closed| *closed).await.is_ok() } => {
                let _ = tokio::time::timeout(
                    CLOSE_TIMEOUT,
                    handle.disconnect(russh::Disconnect::ByApplication, "", "en"),
                )
                .await;
                break None;
            }
        }
    };
    closing.close(failure);
}

/// Closes a connection however its task ends: also when the runtime drops
/// it, as the core goes.
struct Closing {
    state: watch::Sender<State>,
    events: Arc<Events<ConnectionEvent>>,
}

impl Closing {
    fn close(&self, failure: Option<Failure>) {
        self.state.send_replace(State::Closed);
        self.events
            .finish(Some(ConnectionEvent::Closed { failure }));
    }
}

impl Drop for Closing {
    fn drop(&mut self) {
        // Nothing more once closed above.
        self.close(Some(Failure::lost()));
    }
}

/// Measure the round trip every few seconds, the first at once.
async fn measure(handle: Arc<SshHandle>, events: Arc<Events<ConnectionEvent>>) {
    let mut ticker = tokio::time::interval(LATENCY_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let milliseconds = match ssh::round_trip(&handle).await {
            Latency::Measured(rtt) => Some(rtt.as_millis() as u64),
            Latency::TimedOut => None,
        };
        events.push_replacing(ConnectionEvent::Latency { milliseconds }, |event| {
            matches!(event, ConnectionEvent::Latency { .. })
        });
    }
}
