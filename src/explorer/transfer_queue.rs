//! The SFTP tab's transfer queue, as WinSCP keeps one: batches waiting
//! their turn, the one being transferred, and the ones that have ended.
//! Plain bookkeeping; the panel hands batches to the engine, which runs one
//! at a time and knows nothing of what waits behind it.

use std::{collections::HashSet, path::PathBuf};

use crate::sftp::{TransferDirection, TransferPhase, TransferProgress};

/// Why a batch left the queue before it was done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removal {
    /// 移出队列 after it stopped.
    TakenOff,
    /// 丢弃续传进度.
    Discarded,
    /// It could not be handed to the engine.
    NotStarted(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QueueId(pub u64);

/// What a batch copies, as the user confirmed it.
#[derive(Clone, Debug, PartialEq)]
pub enum TransferJob {
    Upload { paths: Vec<PathBuf>, target: String },
    Download { paths: Vec<String>, target: String },
}

impl TransferJob {
    pub fn direction(&self) -> TransferDirection {
        match self {
            TransferJob::Upload { .. } => TransferDirection::Upload,
            TransferJob::Download { .. } => TransferDirection::Download,
        }
    }

    /// The 来源 column: the one item, the folder the items share, or
    /// 多个位置.
    pub fn source_label(&self) -> String {
        let (paths, folders): (Vec<String>, Vec<String>) = match self {
            TransferJob::Upload { paths, .. } => paths
                .iter()
                .map(|path| {
                    (
                        path.display().to_string(),
                        path.parent()
                            .map(|folder| folder.display().to_string())
                            .unwrap_or_default(),
                    )
                })
                .unzip(),
            TransferJob::Download { paths, .. } => paths
                .iter()
                .map(|path| {
                    let folder = match path.rsplit_once('/') {
                        Some(("", _)) => "/".to_string(),
                        Some((folder, _)) => folder.to_string(),
                        None => String::new(),
                    };
                    (path.clone(), folder)
                })
                .unzip(),
        };
        match (paths.as_slice(), folders.first()) {
            ([one], _) => one.clone(),
            (_, Some(first)) if folders.iter().all(|folder| folder == first) => first.clone(),
            _ => "多个位置".to_string(),
        }
    }

    /// The 目标 column.
    pub fn target(&self) -> &str {
        match self {
            TransferJob::Upload { target, .. } | TransferJob::Download { target, .. } => target,
        }
    }
}

/// Where a batch stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueState {
    /// Waiting its turn.
    Pending,
    /// Handed to the engine: scanning, transferring, waiting on a question
    /// or reconnecting.
    Active,
    /// Stopped with its progress kept. It holds the head of the queue until
    /// the user resumes it, discards it or takes it off the queue: the
    /// engine keeps it for 继续, and starting another batch would drop it.
    Stopped,
    /// Done, some items perhaps skipped or failed.
    Done,
    /// Taken off the queue after it stopped, or its progress discarded.
    Removed,
}

impl QueueState {
    pub fn is_finished(self) -> bool {
        matches!(self, QueueState::Done | QueueState::Removed)
    }
}

pub struct QueueEntry {
    id: QueueId,
    job: TransferJob,
    started: bool,
    removed: Option<Removal>,
    progress: Option<TransferProgress>,
}

impl QueueEntry {
    pub fn id(&self) -> QueueId {
        self.id
    }

    pub fn job(&self) -> &TransferJob {
        &self.job
    }

    /// The engine's last word on this batch.
    pub fn progress(&self) -> Option<&TransferProgress> {
        self.progress.as_ref()
    }

    pub fn state(&self) -> QueueState {
        if self.removed.is_some() {
            return QueueState::Removed;
        }
        match self.progress.as_ref().map(TransferProgress::phase) {
            None if !self.started => QueueState::Pending,
            Some(TransferPhase::Stopped) => QueueState::Stopped,
            Some(TransferPhase::Completed) => QueueState::Done,
            _ => QueueState::Active,
        }
    }

    /// The 进度 column in words, for everything but a running transfer's
    /// bar, which shows the percentage beside it.
    pub fn status(&self) -> String {
        let progress = self.progress.as_ref();
        match self.state() {
            QueueState::Pending => "等待中".into(),
            QueueState::Active => match progress.map(TransferProgress::phase) {
                None | Some(TransferPhase::Scanning) => "正在扫描".into(),
                Some(TransferPhase::Waiting) => "等待回答".into(),
                Some(TransferPhase::Reconnecting) => progress
                    .and_then(TransferProgress::note)
                    .unwrap_or("正在重连")
                    .into(),
                Some(_) => percent(progress.map_or(0.0, TransferProgress::fraction)),
            },
            QueueState::Stopped => "已停止".into(),
            QueueState::Done => match progress.map_or(0, TransferProgress::failed) {
                0 => "已完成".into(),
                failed => format!("部分失败（{failed} 项）"),
            },
            QueueState::Removed => match &self.removed {
                Some(Removal::Discarded) => "已丢弃续传进度".into(),
                Some(Removal::NotStarted(reason)) => format!("无法开始：{reason}"),
                _ => "已移出".into(),
            },
        }
    }
}

/// `0.294` as `29%`.
pub fn percent(fraction: f32) -> String {
    format!("{}%", (fraction * 100.0).floor() as u32)
}

#[derive(Default)]
pub struct TransferQueue {
    entries: Vec<QueueEntry>,
    next_id: u64,
    selected: Option<QueueId>,
    expanded: HashSet<QueueId>,
}

impl TransferQueue {
    pub fn push(&mut self, job: TransferJob) -> QueueId {
        self.next_id += 1;
        let id = QueueId(self.next_id);
        self.entries.push(QueueEntry {
            id,
            job,
            started: false,
            removed: None,
            progress: None,
        });
        id
    }

    pub fn entries(&self) -> &[QueueEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn entry(&self, id: QueueId) -> Option<&QueueEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// The batch the engine has: running, or stopped and kept for 继续.
    pub fn head(&self) -> Option<&QueueEntry> {
        self.entries
            .iter()
            .find(|entry| matches!(entry.state(), QueueState::Active | QueueState::Stopped))
    }

    fn head_mut(&mut self) -> Option<&mut QueueEntry> {
        self.entries
            .iter_mut()
            .find(|entry| matches!(entry.state(), QueueState::Active | QueueState::Stopped))
    }

    /// The next batch to hand the engine, now marked as started: the first
    /// one waiting, unless another still holds the head.
    pub fn start_next(&mut self) -> Option<(QueueId, TransferJob)> {
        if self.head().is_some() {
            return None;
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.state() == QueueState::Pending)?;
        entry.started = true;
        Some((entry.id, entry.job.clone()))
    }

    /// The engine reports on the batch it has, which is the head. A report
    /// with no head, such as the empty one after 丢弃续传进度, is dropped.
    pub fn on_progress(&mut self, progress: TransferProgress) {
        if let Some(entry) = self.head_mut() {
            entry.progress = Some(progress);
        }
    }

    /// The head leaves the queue: it could not be handed over, or its
    /// progress is being discarded.
    pub fn abandon_head(&mut self, why: Removal) {
        if let Some(entry) = self.head_mut() {
            entry.removed = Some(why);
        }
    }

    /// What 移出队列 acts on: the selected batch unless it is running, else
    /// a stopped head.
    pub fn removable(&self) -> Option<QueueId> {
        let selected = self
            .selected
            .and_then(|id| self.entry(id))
            .filter(|entry| entry.state() != QueueState::Active);
        selected
            .or_else(|| {
                self.head()
                    .filter(|entry| entry.state() == QueueState::Stopped)
            })
            .map(QueueEntry::id)
    }

    /// Take a batch off the queue. One waiting or ended goes; a stopped head
    /// stays listed as taken off, and the queue moves on past it. A running
    /// batch has to stop first.
    pub fn remove(&mut self, id: QueueId) -> bool {
        let Some(index) = self.entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        match self.entries[index].state() {
            QueueState::Active => false,
            QueueState::Stopped => {
                self.entries[index].removed = Some(Removal::TakenOff);
                true
            }
            _ => {
                self.entries.remove(index);
                self.expanded.remove(&id);
                true
            }
        }
    }

    pub fn has_finished(&self) -> bool {
        self.entries.iter().any(|entry| entry.state().is_finished())
    }

    /// 清除已完成.
    pub fn clear_finished(&mut self) {
        let expanded = &mut self.expanded;
        self.entries.retain(|entry| {
            let keep = !entry.state().is_finished();
            if !keep {
                expanded.remove(&entry.id);
            }
            keep
        });
    }

    /// Batches not yet ended: waiting, running or stopped.
    pub fn unfinished_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| !entry.state().is_finished())
            .count()
    }

    /// Which way the batch at the head goes, or the next one, for questions
    /// and the close confirmation.
    pub fn direction(&self) -> Option<TransferDirection> {
        self.head()
            .or_else(|| {
                self.entries
                    .iter()
                    .find(|entry| entry.state() == QueueState::Pending)
            })
            .map(|entry| entry.job.direction())
    }

    pub fn select(&mut self, id: QueueId) {
        self.selected = Some(id);
    }

    pub fn is_selected(&self, id: QueueId) -> bool {
        self.selected == Some(id)
    }

    pub fn toggle_expanded(&mut self, id: QueueId) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
    }

    pub fn is_expanded(&self, id: QueueId) -> bool {
        self.expanded.contains(&id)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{QueueId, QueueState, Removal, TransferJob, TransferQueue};
    use crate::sftp::{TransferDetail, TransferPhase, TransferProgress};

    fn upload(name: &str) -> TransferJob {
        TransferJob::Upload {
            paths: vec![PathBuf::from(format!("/local/{name}"))],
            target: "/srv".into(),
        }
    }

    fn states(queue: &TransferQueue) -> Vec<QueueState> {
        queue.entries().iter().map(|entry| entry.state()).collect()
    }

    fn report(queue: &mut TransferQueue, phase: TransferPhase) {
        queue.on_progress(TransferProgress::new(phase));
    }

    #[test]
    fn batches_run_one_after_another() {
        let mut queue = TransferQueue::default();
        let first = queue.push(upload("a"));
        let second = queue.push(upload("b"));
        assert_eq!(states(&queue), [QueueState::Pending, QueueState::Pending]);

        assert_eq!(queue.start_next().map(|(id, _)| id), Some(first));
        // The first one holds the head until it is done.
        assert_eq!(queue.start_next(), None);
        report(&mut queue, TransferPhase::Transferring);
        assert_eq!(states(&queue), [QueueState::Active, QueueState::Pending]);
        assert_eq!(queue.unfinished_count(), 2);

        report(&mut queue, TransferPhase::Completed);
        assert_eq!(queue.start_next().map(|(id, _)| id), Some(second));
        report(&mut queue, TransferPhase::Completed);
        assert_eq!(states(&queue), [QueueState::Done, QueueState::Done]);
        assert_eq!(queue.unfinished_count(), 0);
        // The empty report 丢弃续传进度 answers with goes nowhere.
        report(&mut queue, TransferPhase::Completed);

        queue.clear_finished();
        assert!(queue.is_empty());
    }

    #[test]
    fn a_stopped_batch_holds_the_queue_until_resumed_or_taken_off() {
        let mut queue = TransferQueue::default();
        let first = queue.push(upload("a"));
        let second = queue.push(upload("b"));
        queue.start_next();
        report(&mut queue, TransferPhase::Stopped);
        assert_eq!(queue.start_next(), None);
        assert_eq!(queue.removable(), Some(first));

        // Resumed: running again, still first.
        report(&mut queue, TransferPhase::Transferring);
        assert_eq!(queue.removable(), None);
        assert!(!queue.remove(first));
        report(&mut queue, TransferPhase::Stopped);

        // Taken off: listed as such, and the next one's turn.
        assert!(queue.remove(first));
        assert_eq!(states(&queue), [QueueState::Removed, QueueState::Pending]);
        assert_eq!(queue.start_next().map(|(id, _)| id), Some(second));
    }

    #[test]
    fn waiting_and_ended_batches_can_be_removed_but_a_running_one_cannot() {
        let mut queue = TransferQueue::default();
        let first = queue.push(upload("a"));
        let second = queue.push(upload("b"));
        let third = queue.push(upload("c"));
        queue.start_next();
        report(&mut queue, TransferPhase::Transferring);

        queue.select(first);
        assert_eq!(queue.removable(), None);
        queue.select(second);
        assert_eq!(queue.removable(), Some(second));
        assert!(queue.remove(second));
        let left: Vec<QueueId> = queue.entries().iter().map(|entry| entry.id()).collect();
        assert_eq!(left, [first, third]);

        report(&mut queue, TransferPhase::Completed);
        queue.select(first);
        assert!(queue.has_finished());
        assert!(queue.remove(first));
        assert!(!queue.has_finished());
    }

    #[test]
    fn each_state_reads_as_winscp_words() {
        let mut queue = TransferQueue::default();
        let first = queue.push(upload("a"));
        queue.push(upload("b"));
        let status = |queue: &TransferQueue, index: usize| queue.entries()[index].status();
        assert_eq!(status(&queue, 0), "等待中");
        queue.start_next();
        assert_eq!(status(&queue, 0), "正在扫描");
        queue.on_progress(TransferProgress::new(TransferPhase::Transferring).with_bytes(294, 1000));
        assert_eq!(status(&queue, 0), "29%");
        report(&mut queue, TransferPhase::Waiting);
        assert_eq!(status(&queue, 0), "等待回答");
        report(&mut queue, TransferPhase::Stopped);
        assert_eq!(status(&queue, 0), "已停止");
        queue.abandon_head(Removal::Discarded);
        assert_eq!(status(&queue, 0), "已丢弃续传进度");
        assert_eq!(queue.entries()[0].id(), first);

        queue.start_next();
        queue.on_progress(
            TransferProgress::new(TransferPhase::Completed).with_details(vec![
                TransferDetail::done("/srv/a"),
                TransferDetail::failed("/srv/b", "permission denied"),
            ]),
        );
        assert_eq!(status(&queue, 1), "部分失败（1 项）");
    }

    #[test]
    fn the_source_is_the_item_its_folder_or_several_places() {
        let upload = |paths: &[&str]| TransferJob::Upload {
            paths: paths.iter().map(PathBuf::from).collect(),
            target: "/srv".into(),
        };
        assert_eq!(upload(&["/data/a.zip"]).source_label(), "/data/a.zip");
        assert_eq!(upload(&["/data/a.zip", "/data/b"]).source_label(), "/data");
        assert_eq!(upload(&["/data/a", "/tmp/b"]).source_label(), "多个位置");
        let download = TransferJob::Download {
            paths: vec!["/var/log".into(), "/var/tmp".into()],
            target: "/local".into(),
        };
        assert_eq!(download.source_label(), "/var");
        let root = TransferJob::Download {
            paths: vec!["/boot".into(), "/etc".into()],
            target: "/local".into(),
        };
        assert_eq!(root.source_label(), "/");
    }
}
