//! The round trip of a live SSH connection, as the terminal and SFTP tabs
//! show it beside their buttons.

use std::time::{Duration, Instant};

use crate::connection::Latency;

use super::SshHandle;

/// How often a live connection's round trip is measured.
pub const LATENCY_INTERVAL: Duration = Duration::from_secs(5);
/// A ping unanswered for this long reads as timed out.
const LATENCY_TIMEOUT: Duration = Duration::from_secs(5);

/// One round trip on `handle`, measured with `keepalive@openssh.com`, which
/// every server answers (a refusal is an answer too). The ping queues behind
/// whatever the connection is sending, so during a flood of output or a
/// transfer the reading rises: that is the responsiveness the user gets.
pub async fn round_trip(handle: &SshHandle) -> Latency {
    let sent = Instant::now();
    match tokio::time::timeout(LATENCY_TIMEOUT, handle.send_ping()).await {
        Ok(Ok(())) => Latency::Measured(sent.elapsed()),
        _ => Latency::TimedOut,
    }
}
