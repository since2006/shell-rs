//! The round trip of a live SSH connection, as the terminal and SFTP tabs
//! show it beside their buttons.

use std::time::{Duration, Instant};

use super::SshHandle;

/// How often a live connection's round trip is measured.
pub const LATENCY_INTERVAL: Duration = Duration::from_secs(5);
/// A ping unanswered for this long reads as timed out.
const LATENCY_TIMEOUT: Duration = Duration::from_secs(5);

/// The last round trip measured on a remote connection: how long the server
/// took to answer, or that it did not answer in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Latency {
    Measured(Duration),
    TimedOut,
}

/// How a latency reads to someone typing: echo is instant below 100 ms,
/// noticeable up to 200 ms, and sluggish beyond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatencyLevel {
    Good,
    Fair,
    Poor,
}

impl Latency {
    pub fn level(self) -> LatencyLevel {
        match self {
            Latency::Measured(rtt) if rtt < Duration::from_millis(100) => LatencyLevel::Good,
            Latency::Measured(rtt) if rtt <= Duration::from_millis(200) => LatencyLevel::Fair,
            Latency::Measured(_) | Latency::TimedOut => LatencyLevel::Poor,
        }
    }
}

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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Latency, LatencyLevel};

    fn ms(millis: u64) -> Latency {
        Latency::Measured(Duration::from_millis(millis))
    }

    #[test]
    fn latency_levels_split_at_100_and_200_ms() {
        assert_eq!(ms(99).level(), LatencyLevel::Good);
        assert_eq!(ms(100).level(), LatencyLevel::Fair);
        assert_eq!(ms(200).level(), LatencyLevel::Fair);
        assert_eq!(ms(201).level(), LatencyLevel::Poor);
        assert_eq!(Latency::TimedOut.level(), LatencyLevel::Poor);
    }
}
