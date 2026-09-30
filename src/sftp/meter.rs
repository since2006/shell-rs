//! A batch's progress, kept the same way by uploads and downloads: what the
//! transfer queue shows, and the clock and byte counts behind it.

use std::time::{Duration, Instant};

use super::{
    SftpEvent, TransferDetail, TransferDirection, TransferOutcome, TransferPhase, TransferProgress,
    control::TransferControl, speed::TransferClock,
};

/// While bytes move, progress goes to the UI no more often than this.
const EMIT_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) struct TransferMeter {
    pub progress: TransferProgress,
    clock: TransferClock,
    /// Bytes this run has put through, which the speed is measured from. The
    /// part of a resumed file that was already there is not among them.
    moved_bytes: u64,
    /// The size of the files that are done.
    finished_bytes: u64,
    last_emit: Instant,
}

impl TransferMeter {
    pub fn new(direction: TransferDirection, total: usize, total_bytes: u64) -> Self {
        Self {
            progress: TransferProgress {
                direction,
                total,
                total_bytes,
                ..TransferProgress::default()
            },
            clock: TransferClock::default(),
            moved_bytes: 0,
            finished_bytes: 0,
            last_emit: Instant::now(),
        }
    }

    pub fn emit(&self, control: &TransferControl) {
        let _ = control
            .events
            .try_send(SftpEvent::Progress(self.progress.clone()));
    }

    /// Enter a phase and say so. Only transferring counts towards the time
    /// and the speed.
    pub fn phase(&mut self, phase: TransferPhase, control: &TransferControl) {
        let now = Instant::now();
        if phase == TransferPhase::Transferring {
            self.clock.run(now, self.moved_bytes);
        } else {
            self.clock.pause(now);
        }
        if phase != TransferPhase::Reconnecting {
            self.progress.note = None;
        }
        if phase == TransferPhase::Completed {
            self.progress.current.clear();
            self.progress.current_source.clear();
            self.progress.current_bytes = 0;
            self.progress.current_total = 0;
        }
        self.progress.phase = phase;
        self.progress.elapsed = self.clock.elapsed(now);
        self.progress.bytes_per_second = self.clock.speed();
        self.emit(control);
    }

    /// An item starts: it is the file in flight now, `size` bytes to move.
    pub fn begin(&mut self, source: String, target: String, size: u64, control: &TransferControl) {
        self.progress.current = target;
        self.progress.current_source = source;
        self.progress.current_bytes = 0;
        self.progress.current_total = size;
        self.emit(control);
    }

    /// `length` more bytes went through, which leaves the file in flight at
    /// `file_bytes`.
    pub fn advance(&mut self, length: u64, file_bytes: u64, control: &TransferControl) {
        self.moved_bytes += length;
        let now = Instant::now();
        self.clock.record(now, self.moved_bytes);
        self.progress.completed_bytes = self.finished_bytes.saturating_add(file_bytes);
        self.progress.current_bytes = file_bytes;
        self.progress.bytes_per_second = self.clock.speed();
        self.progress.elapsed = self.clock.elapsed(now);
        if self.last_emit.elapsed() >= EMIT_INTERVAL {
            self.emit(control);
            self.last_emit = Instant::now();
        }
    }

    /// An item ended as `detail` says; `size` is what it had to move.
    pub fn settle(&mut self, detail: TransferDetail, size: u64) {
        match detail.outcome() {
            TransferOutcome::Done => {
                self.progress.succeeded += 1;
                self.finished_bytes = self.finished_bytes.saturating_add(size);
            }
            TransferOutcome::Skipped => {
                self.progress.skipped += 1;
                self.progress.settled_bytes += size;
            }
            TransferOutcome::Failed => {
                self.progress.failed += 1;
                self.progress.settled_bytes += size;
            }
        }
        self.progress.details.push(detail);
    }

    /// The item is dealt with; on to the next one.
    pub fn next(&mut self, control: &TransferControl) {
        self.progress.completed_bytes = self.finished_bytes;
        self.phase(TransferPhase::Transferring, control);
    }
}
