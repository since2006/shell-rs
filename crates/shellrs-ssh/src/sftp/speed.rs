//! How long a batch has spent transferring, and how fast it is going now.
//! Waiting on a question, reconnecting and being stopped count for neither:
//! the time is the batch's working time, and the speed is the recent one
//! rather than an average dragged down by every pause.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// The stretch of recent progress the speed is measured over.
const WINDOW: Duration = Duration::from_secs(3);
/// Too short a stretch gives a wild number; until there is this much, the
/// speed is not known yet.
const SHORTEST: Duration = Duration::from_millis(200);

#[derive(Debug, Default)]
pub(crate) struct TransferClock {
    /// Since when the batch has been transferring, while it is.
    running_since: Option<Instant>,
    /// Transferring time before `running_since`.
    earlier: Duration,
    /// The batch's byte count at recent moments, oldest first.
    samples: VecDeque<(Instant, u64)>,
}

impl TransferClock {
    /// Transferring starts, or starts again. `bytes` is the batch's count so
    /// far, which the speed is measured from.
    pub fn run(&mut self, now: Instant, bytes: u64) {
        if self.running_since.is_none() {
            self.running_since = Some(now);
            self.samples.clear();
            self.samples.push_back((now, bytes));
        }
    }

    /// Transferring stops for now: a question, a reconnect, the end.
    pub fn pause(&mut self, now: Instant) {
        if let Some(since) = self.running_since.take() {
            self.earlier += now.saturating_duration_since(since);
        }
        self.samples.clear();
    }

    /// `bytes` more have gone through, `bytes` being the batch's count.
    pub fn record(&mut self, now: Instant, bytes: u64) {
        if self.running_since.is_none() {
            return;
        }
        self.samples.push_back((now, bytes));
        // Keep one sample from before the window, to measure from.
        while self.samples.len() > 2
            && self
                .samples
                .get(1)
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) >= WINDOW)
        {
            self.samples.pop_front();
        }
    }

    pub fn elapsed(&self, now: Instant) -> Duration {
        self.earlier
            + self
                .running_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }

    /// Bytes per second over the last few seconds; 0 while not transferring
    /// or not measurable yet.
    pub fn speed(&self) -> u64 {
        let (Some((first_at, first)), Some((last_at, last))) =
            (self.samples.front(), self.samples.back())
        else {
            return 0;
        };
        let span = last_at.saturating_duration_since(*first_at);
        if span < SHORTEST {
            return 0;
        }
        (last.saturating_sub(*first) as f64 / span.as_secs_f64()) as u64
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::TransferClock;

    #[test]
    fn speed_follows_the_last_seconds_and_pauses_do_not_count() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut clock = TransferClock::default();
        clock.run(at(0), 0);
        // Not measurable yet.
        clock.record(at(100), 100);
        assert_eq!(clock.speed(), 0);
        // 1 MB/s for four seconds.
        for second in 1..=4 {
            clock.record(at(second * 1000), second * 1_000_000);
        }
        assert_eq!(clock.speed(), 1_000_000);
        // Then 3 MB/s: the window forgets the slower start.
        for second in 5..=8 {
            clock.record(at(second * 1000), 4_000_000 + (second - 4) * 3_000_000);
        }
        assert_eq!(clock.speed(), 3_000_000);
        assert_eq!(clock.elapsed(at(8000)), Duration::from_secs(8));

        // A question: time stands still and there is no speed.
        clock.pause(at(8000));
        assert_eq!(clock.speed(), 0);
        assert_eq!(clock.elapsed(at(20_000)), Duration::from_secs(8));
        // Answered: it goes on from where it was.
        clock.run(at(20_000), 16_000_000);
        clock.record(at(21_000), 18_000_000);
        assert_eq!(clock.speed(), 2_000_000);
        assert_eq!(clock.elapsed(at(21_000)), Duration::from_secs(9));
    }
}
