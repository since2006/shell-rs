//! Terminal-owned forwarding. No agent protocol messages or keys are retained.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use russh::{Channel, ChannelMsg, client};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{JoinHandle, JoinSet};

use super::connection::{Agent, AgentLocation, connect_agent};
use crate::i18n::t;

const MAX_CHANNELS: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct AgentOpens {
    enabled: Arc<AtomicBool>,
    sender: mpsc::Sender<AgentOpen>,
    slots: Arc<Semaphore>,
}

struct AgentOpen {
    channel: Channel<client::Msg>,
    reply: client::ChannelOpenHandle,
    _slot: tokio::sync::OwnedSemaphorePermit,
}

impl AgentOpens {
    pub(super) fn open(&self, channel: Channel<client::Msg>, reply: client::ChannelOpenHandle) {
        // Dropping the reply rejects the channel. Never wait inside the SSH
        // handler: acceptance and stream traffic need that same event loop.
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            return;
        };
        let _ = self.sender.try_send(AgentOpen {
            channel,
            reply,
            _slot: slot,
        });
    }
}

pub(super) struct AgentForwarding {
    enabled: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

impl AgentForwarding {
    pub(super) fn new(first: Agent, location: AgentLocation) -> (Self, AgentOpens) {
        let enabled = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::channel::<AgentOpen>(MAX_CHANNELS);
        let worker = tokio::spawn(async move {
            let mut first = Some(first);
            let mut channels = JoinSet::new();
            loop {
                tokio::select! {
                    open = receiver.recv() => {
                        let Some(open) = open else { break };
                        let first = first.take();
                        let location = location.clone();
                        channels.spawn(async move {
                            let agent = match first {
                                Some(agent) => Ok(agent),
                                None => connect_agent(&location).await,
                            };
                            let Ok(agent) = agent else { return };
                            // Discovery is frozen for this terminal; each channel
                            // gets a separate connection to the same endpoint.
                            open.reply.accept().await;
                            let mut remote = open.channel.into_stream();
                            let mut local = agent.into_inner();
                            let _ = tokio::io::copy_bidirectional(&mut remote, &mut local).await;
                            drop(open._slot);
                        });
                    }
                    _ = channels.join_next(), if !channels.is_empty() => {}
                }
            }
            // JoinSet aborts remaining bridges on drop, including local connects.
        });
        (
            Self {
                enabled: enabled.clone(),
                worker,
            },
            AgentOpens {
                enabled,
                sender,
                slots: Arc::new(Semaphore::new(MAX_CHANNELS)),
            },
        )
    }

    /// Runs before PTY/shell requests so the first success/failure belongs to
    /// this request. A refusal never silently opens a terminal without forwarding.
    pub(super) async fn request(&mut self, channel: &mut Channel<client::Msg>) -> Result<()> {
        self.enabled.store(true, Ordering::Release);
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            channel.agent_forward(true).await?;
            match channel.wait().await {
                Some(ChannelMsg::Success) => Ok(()),
                _ => bail!(t!("ssh.agent.forward_refused")),
            }
        })
        .await
        .map_err(|_| anyhow!(t!("ssh.agent.forward_timeout")))?;
        if result.is_err() {
            self.enabled.store(false, Ordering::Release);
        }
        result
    }
}

impl Drop for AgentForwarding {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Release);
        self.worker.abort();
    }
}
