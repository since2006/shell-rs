//! A batch's progress, kept the same way by uploads and downloads: what the
//! transfer queue shows, and the clock and byte counts behind it.

use std::time::{Duration, Instant};

use super::{
    EngineEvent, TransferDetail, TransferDirection, TransferOutcome, TransferPhase,
    TransferProgress, control::TransferControl, speed::TransferClock,
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
    /// The items moving now, the longest running first: several go at once.
    in_flight: Vec<InFlight>,
    next_id: u64,
    last_emit: Instant,
}

/// An item that has begun and not yet settled.
struct InFlight {
    id: FileId,
    source: String,
    target: String,
    bytes: u64,
    total: u64,
}

/// Names an item in flight to the meter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileId(u64);

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
            in_flight: Vec::new(),
            next_id: 0,
            last_emit: Instant::now(),
        }
    }

    pub fn emit(&self, control: &TransferControl) {
        let _ = control
            .events
            .try_send(EngineEvent::Progress(self.progress.clone()));
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

    /// An item starts, `size` bytes to move. The queue shows the item that
    /// has been in flight longest.
    pub fn begin(
        &mut self,
        source: String,
        target: String,
        size: u64,
        control: &TransferControl,
    ) -> FileId {
        let id = FileId(self.next_id);
        self.next_id += 1;
        self.in_flight.push(InFlight {
            id,
            source,
            target,
            bytes: 0,
            total: size,
        });
        self.show_current();
        self.emit(control);
        id
    }

    /// `length` more bytes of `file` went through, which leaves it at
    /// `file_bytes`.
    pub fn advance(
        &mut self,
        file: FileId,
        length: u64,
        file_bytes: u64,
        control: &TransferControl,
    ) {
        self.moved_bytes += length;
        if let Some(entry) = self.in_flight.iter_mut().find(|entry| entry.id == file) {
            entry.bytes = file_bytes;
        }
        let now = Instant::now();
        self.clock.record(now, self.moved_bytes);
        self.show_current();
        self.progress.bytes_per_second = self.clock.speed();
        self.progress.elapsed = self.clock.elapsed(now);
        if self.last_emit.elapsed() >= EMIT_INTERVAL {
            self.emit(control);
            self.last_emit = Instant::now();
        }
    }

    /// `file` ended as `detail` says; `size` is what it had to move.
    pub fn settle(
        &mut self,
        file: FileId,
        detail: TransferDetail,
        size: u64,
        control: &TransferControl,
    ) {
        self.in_flight.retain(|entry| entry.id != file);
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
        self.show_current();
        self.emit(control);
    }

    /// `file` goes without ending: scanning it again replaced it.
    pub fn forget(&mut self, file: FileId) {
        self.in_flight.retain(|entry| entry.id != file);
        self.show_current();
    }

    /// The items in flight were broken off, by a lost connection or a stop:
    /// the next run starts them over.
    pub fn abandon(&mut self) {
        self.in_flight.clear();
        self.show_current();
    }

    /// Say where the batch is: what is done, and the item in flight longest.
    /// Between items the last one stays up rather than flicker.
    fn show_current(&mut self) {
        self.progress.completed_bytes = self
            .in_flight
            .iter()
            .fold(self.finished_bytes, |sum, entry| {
                sum.saturating_add(entry.bytes)
            });
        if let Some(entry) = self.in_flight.first() {
            self.progress.current.clone_from(&entry.target);
            self.progress.current_source.clone_from(&entry.source);
            self.progress.current_bytes = entry.bytes;
            self.progress.current_total = entry.total;
        }
    }
}
