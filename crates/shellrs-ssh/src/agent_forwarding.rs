//! Terminal-owned forwarding of identities and signing only. Agent mutation,
//! provider loading and extensions never reach the local agent.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use russh::{Channel, ChannelMsg, client};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::{JoinHandle, JoinSet};

use super::connection::{Agent, AgentLocation, connect_agent};
use crate::AgentForwardError;

const MAX_CHANNELS: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FRAME: usize = 256 * 1024;
/// Four queued requests per channel; overflow closes the channel rather
/// than blocking the reader from noticing that the remote has disconnected.
const MAX_QUEUED_REQUESTS: usize = 4;
const FAILURE: u8 = 5;
const IDENTITIES_REQUEST: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const SIGN_RESPONSE: u8 = 14;

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

/// Terminal-owned permission to use the local agent. Dropping this guard
/// rejects new channels and closes every existing local agent connection.
pub struct AgentForwarding {
    enabled: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

impl AgentForwarding {
    pub(super) fn new(
        first: Agent,
        location: AgentLocation,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> (Self, AgentOpens) {
        let enabled = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = mpsc::channel::<AgentOpen>(MAX_CHANNELS);
        let worker_enabled = enabled.clone();
        let worker = tokio::spawn(async move {
            let mut first = Some(first);
            let mut channels = JoinSet::new();
            loop {
                tokio::select! {
                    _ = shutdown.changed() => break,
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
                            let _ = proxy_agent(&mut remote, &mut local).await;
                            drop(open._slot);
                        });
                    }
                    _ = channels.join_next(), if !channels.is_empty() => {}
                }
            }
            worker_enabled.store(false, Ordering::Release);
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
    pub async fn request(&mut self, channel: &mut Channel<client::Msg>) -> Result<()> {
        if self.worker.is_finished() {
            bail!(AgentForwardError::RequestFailed);
        }
        // Also revoke the capability if this future is cancelled mid-request.
        let mut pending = PendingRequest {
            enabled: self.enabled.clone(),
            worker: self.worker.abort_handle(),
            accepted: false,
        };
        self.enabled.store(true, Ordering::Release);
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            channel
                .agent_forward(true)
                .await
                .map_err(|_| anyhow!(AgentForwardError::RequestFailed))?;
            loop {
                if let Some(result) = forwarding_reply(channel.wait().await) {
                    return result.map_err(anyhow::Error::from);
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err(anyhow!(AgentForwardError::Timeout)));
        pending.accepted = result.is_ok();
        result
    }
}

/// Window/flow-control notifications may arrive before the request's reply.
fn forwarding_reply(message: Option<ChannelMsg>) -> Option<Result<(), AgentForwardError>> {
    match message {
        Some(ChannelMsg::Success) => Some(Ok(())),
        Some(ChannelMsg::Failure | ChannelMsg::Eof | ChannelMsg::Close) | None => {
            Some(Err(AgentForwardError::Refused))
        }
        _ => None,
    }
}

impl Drop for AgentForwarding {
    fn drop(&mut self) {
        self.enabled.store(false, Ordering::Release);
        self.worker.abort();
    }
}

struct PendingRequest {
    enabled: Arc<AtomicBool>,
    worker: tokio::task::AbortHandle,
    accepted: bool,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if !self.accepted {
            self.enabled.store(false, Ordering::Release);
            self.worker.abort();
        }
    }
}

/// The remote host may list keys and ask for signatures, but must never
/// administer the user's agent or ask it to load a provider. In particular,
/// extensions are not forwarded: this proxy does not implement session-bind.
async fn proxy_agent<R, L>(remote: &mut R, local: &mut L) -> std::io::Result<()>
where
    R: AsyncRead + AsyncWrite + Unpin,
    L: AsyncRead + AsyncWrite + Unpin,
{
    let (mut read, mut write) = tokio::io::split(remote);
    let (sender, mut receiver) = mpsc::channel(MAX_QUEUED_REQUESTS);
    let requests = async {
        loop {
            let request = read_frame(&mut read).await?;
            if sender.try_send(request).is_err() {
                return Err::<(), _>(malformed_frame());
            }
        }
    };
    let responses = async {
        while let Some(request) = receiver.recv().await {
            let kind = request[0];
            match kind {
                IDENTITIES_REQUEST if request.len() == 1 => {}
                SIGN_REQUEST if valid_sign_request(&request[1..]) => {}
                IDENTITIES_REQUEST | SIGN_REQUEST => return Err(malformed_frame()),
                _ => {
                    write_frame(&mut write, &[FAILURE]).await?;
                    continue;
                }
            }
            write_frame(local, &request).await?;
            drop(request);
            let response = read_frame(local).await?;
            if !valid_response(kind, &response) {
                return Err(malformed_frame());
            }
            write_frame(&mut write, &response).await?;
        }
        Ok(())
    };
    // Reading never waits for an agent response. EOF/close therefore also
    // cancels a pending signing confirmation, freeing this channel's slot.
    // Both futures are scoped here; neither can outlive the bridge.
    tokio::select! {
        result = requests => result,
        result = responses => result,
    }
}

fn malformed_frame() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid SSH agent frame")
}

async fn read_frame<R: AsyncRead + Unpin>(stream: &mut R) -> std::io::Result<Vec<u8>> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(malformed_frame());
    }
    let mut frame = vec![0; length];
    stream.read_exact(&mut frame).await?;
    Ok(frame)
}

async fn write_frame<W: AsyncWrite + Unpin>(stream: &mut W, frame: &[u8]) -> std::io::Result<()> {
    stream.write_u32(frame.len() as u32).await?;
    stream.write_all(frame).await?;
    stream.flush().await
}

/// Consume one SSH string without allocation or unchecked length arithmetic.
fn take_string(body: &mut &[u8]) -> bool {
    let Some(prefix) = body.get(..4) else {
        return false;
    };
    let length = u32::from_be_bytes(prefix.try_into().expect("four-byte prefix")) as usize;
    let rest = &body[4..];
    let Some(tail) = rest.get(length..) else {
        return false;
    };
    *body = tail;
    true
}

fn valid_sign_request(mut body: &[u8]) -> bool {
    // string key_blob, string data, uint32 flags. The agent checks key and
    // signature algorithm semantics; the proxy checks the entire envelope.
    take_string(&mut body) && take_string(&mut body) && body.len() == 4
}

fn valid_response(request: u8, response: &[u8]) -> bool {
    let mut body = &response[1..];
    match response[0] {
        FAILURE => body.is_empty(),
        SIGN_RESPONSE if request == SIGN_REQUEST => take_string(&mut body) && body.is_empty(),
        IDENTITIES_ANSWER if request == IDENTITIES_REQUEST => {
            let Some(prefix) = body.get(..4) else {
                return false;
            };
            let count = u32::from_be_bytes(prefix.try_into().expect("four-byte prefix")) as usize;
            body = &body[4..];
            // Every identity has at least two string length prefixes.
            if count > body.len() / 8 {
                return false;
            }
            for _ in 0..count {
                if !take_string(&mut body) || !take_string(&mut body) {
                    return false;
                }
            }
            body.is_empty()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    fn proxy() -> (DuplexStream, DuplexStream, JoinHandle<std::io::Result<()>>) {
        let (remote, mut forwarded) = tokio::io::duplex(1024);
        let (agent, mut local) = tokio::io::duplex(1024);
        let task = tokio::spawn(async move { proxy_agent(&mut forwarded, &mut local).await });
        (remote, agent, task)
    }

    #[test]
    fn request_reply_ignores_unrelated_channel_messages() {
        assert!(
            forwarding_reply(Some(ChannelMsg::Data {
                data: b"notice".to_vec().into(),
            }))
            .is_none()
        );
        assert_eq!(forwarding_reply(Some(ChannelMsg::Success)), Some(Ok(())));
        for message in [
            Some(ChannelMsg::Failure),
            Some(ChannelMsg::Eof),
            Some(ChannelMsg::Close),
            None,
        ] {
            assert_eq!(
                forwarding_reply(message),
                Some(Err(AgentForwardError::Refused))
            );
        }
    }

    #[tokio::test]
    async fn queued_request_overflow_fails_closed() {
        let (mut remote, mut agent, task) = proxy();
        write_frame(&mut remote, &[IDENTITIES_REQUEST])
            .await
            .unwrap();
        assert_eq!(read_frame(&mut agent).await.unwrap(), [IDENTITIES_REQUEST]);
        // Hold the first response while the bounded reader queue fills up.
        let pipeline = [0, 0, 0, 1, IDENTITIES_REQUEST].repeat(MAX_QUEUED_REQUESTS + 1);
        remote.write_all(&pipeline).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        let mut byte = [0];
        assert_eq!(agent.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn remote_eof_cancels_an_unanswered_signing_request() {
        let (mut remote, mut agent, task) = proxy();
        let sign = [SIGN_REQUEST, 0, 0, 0, 1, 7, 0, 0, 0, 1, 8, 0, 0, 0, 0];
        write_frame(&mut remote, &sign).await.unwrap();
        assert_eq!(read_frame(&mut agent).await.unwrap(), sign);
        // The local agent might be waiting for user confirmation indefinitely.
        remote.shutdown().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        let mut byte = [0];
        assert_eq!(agent.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn pipelined_requests_keep_their_matching_responses() {
        let (mut remote, mut agent, task) = proxy();
        let sign = [SIGN_REQUEST, 0, 0, 0, 1, 7, 0, 0, 0, 1, 8, 0, 0, 0, 0];
        let mut pipeline = vec![0, 0, 0, 1, IDENTITIES_REQUEST];
        pipeline.extend_from_slice(&(sign.len() as u32).to_be_bytes());
        pipeline.extend_from_slice(&sign);
        remote.write_all(&pipeline).await.unwrap();
        assert_eq!(read_frame(&mut agent).await.unwrap(), [IDENTITIES_REQUEST]);
        let identities = [IDENTITIES_ANSWER, 0, 0, 0, 0];
        write_frame(&mut agent, &identities).await.unwrap();
        assert_eq!(read_frame(&mut agent).await.unwrap(), sign);
        let signature = [SIGN_RESPONSE, 0, 0, 0, 1, 9];
        write_frame(&mut agent, &signature).await.unwrap();
        assert_eq!(read_frame(&mut remote).await.unwrap(), identities);
        assert_eq!(read_frame(&mut remote).await.unwrap(), signature);
        task.abort();
    }

    #[tokio::test]
    async fn mutations_providers_and_extensions_are_rejected_locally() {
        let (mut remote, mut agent, task) = proxy();
        // add/remove/remove-all, smartcard add/remove, lock/unlock,
        // constrained add/provider-add, extension, and unknown messages.
        for kind in [17, 18, 19, 20, 21, 22, 23, 25, 26, 27, 255] {
            write_frame(&mut remote, &[kind, 0, 0, 0, 0]).await.unwrap();
            assert_eq!(read_frame(&mut remote).await.unwrap(), [FAILURE]);
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(20), agent.read_u8())
                .await
                .is_err()
        );
        // Rejected requests do not poison the following valid exchange.
        remote.write_all(&[0, 0]).await.unwrap();
        remote.write_all(&[0, 1, IDENTITIES_REQUEST]).await.unwrap();
        assert_eq!(read_frame(&mut agent).await.unwrap(), [IDENTITIES_REQUEST]);
        write_frame(&mut agent, &[IDENTITIES_ANSWER, 0, 0, 0, 0])
            .await
            .unwrap();
        assert_eq!(
            read_frame(&mut remote).await.unwrap(),
            [IDENTITIES_ANSWER, 0, 0, 0, 0]
        );
        task.abort();
    }

    #[tokio::test]
    async fn signing_frames_and_failure_replies_are_forwarded() {
        let (mut remote, mut agent, task) = proxy();
        let request = [SIGN_REQUEST, 0, 0, 0, 1, 7, 0, 0, 0, 1, 8, 0, 0, 0, 0];
        for response in [&[SIGN_RESPONSE, 0, 0, 0, 1, 9][..], &[FAILURE][..]] {
            write_frame(&mut remote, &request).await.unwrap();
            assert_eq!(read_frame(&mut agent).await.unwrap(), request);
            write_frame(&mut agent, response).await.unwrap();
            assert_eq!(read_frame(&mut remote).await.unwrap(), response);
        }
        task.abort();
    }

    #[tokio::test]
    async fn malformed_oversized_and_truncated_requests_close_without_reaching_agent() {
        let cases = [
            vec![0, 0, 0, 0],
            ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec(),
            vec![0, 0],
            vec![0, 0, 0, 8, SIGN_REQUEST, 0],
            vec![0, 0, 0, 2, IDENTITIES_REQUEST, 0],
            vec![0, 0, 0, 1, SIGN_REQUEST],
        ];
        for bytes in cases {
            let (mut remote, mut agent, task) = proxy();
            remote.write_all(&bytes).await.unwrap();
            remote.shutdown().await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            let mut byte = [0];
            assert_eq!(agent.read(&mut byte).await.unwrap(), 0);
            assert_eq!(remote.read(&mut byte).await.unwrap(), 0);
        }
    }

    #[tokio::test]
    async fn invalid_or_oversized_agent_replies_are_not_relayed() {
        let cases = [
            ((MAX_FRAME + 1) as u32).to_be_bytes().to_vec(),
            vec![0, 0, 0, 1, SIGN_RESPONSE],
            vec![0, 0, 0, 5, IDENTITIES_ANSWER, 0, 0, 0, 1],
            vec![0, 0, 0, 2, FAILURE, 0],
            vec![0, 0],
            vec![0, 0, 0, 5, IDENTITIES_ANSWER, 0],
        ];
        for bytes in cases {
            let (mut remote, mut agent, task) = proxy();
            write_frame(&mut remote, &[IDENTITIES_REQUEST])
                .await
                .unwrap();
            assert_eq!(read_frame(&mut agent).await.unwrap(), [IDENTITIES_REQUEST]);
            agent.write_all(&bytes).await.unwrap();
            agent.shutdown().await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            let mut byte = [0];
            assert_eq!(remote.read(&mut byte).await.unwrap(), 0);
        }
    }
}
