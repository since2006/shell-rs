use super::{SftpEvent, TransferAnswer, TransferChoice, TransferQuestion, TransferQuestionKind};
use anyhow::Result;
use async_channel::{Receiver, Sender};
use std::{
    collections::HashSet,
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;
static NEXT_QUESTION: AtomicU64 = AtomicU64::new(1);
#[derive(Debug)]
pub(crate) struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("传输已停止，进度已保留")
    }
}
impl std::error::Error for Cancelled {}

pub(crate) struct TransferControl {
    pub events: Sender<SftpEvent>,
    pub cancel: watch::Receiver<bool>,
    pub answers: Receiver<(u64, TransferAnswer)>,
}
impl TransferControl {
    pub fn check(&self) -> Result<()> {
        if *self.cancel.borrow() {
            Err(Cancelled.into())
        } else {
            Ok(())
        }
    }
    pub async fn run<T>(&self, future: impl Future<Output = Result<T>>) -> Result<T> {
        self.check()?;
        let mut cancel = self.cancel.clone();
        tokio::select! {
            result = future => result,
            _ = async {
                loop {
                    if *cancel.borrow_and_update() { break; }
                    if cancel.changed().await.is_err() { break; }
                }
            } => Err(Cancelled.into())
        }
    }
    pub async fn ask(
        &self,
        kind: TransferQuestionKind,
        path: &str,
        message: &str,
    ) -> Result<TransferAnswer> {
        self.check()?;
        let id = NEXT_QUESTION.fetch_add(1, Ordering::Relaxed);
        self.events
            .send(SftpEvent::Question(TransferQuestion::new(
                id, kind, path, message,
            )))
            .await?;
        self.run(async {
            loop {
                let (reply_id, answer) = self.answers.recv().await?;
                if reply_id == id {
                    return if answer.choice() == TransferChoice::Cancel {
                        Err(Cancelled.into())
                    } else {
                        Ok(answer)
                    };
                }
            }
        })
        .await
    }
}

/// Keeps two sessions from writing the same target at once, for as long as
/// it is held. `key` names the target: endpoint and remote path for uploads,
/// the local path for downloads.
pub(crate) struct TargetGuard(String);
static ACTIVE_TARGETS: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);
impl TargetGuard {
    pub fn acquire(key: String, busy: &str) -> Result<Self> {
        let mut active = ACTIVE_TARGETS.lock().unwrap_or_else(|e| e.into_inner());
        if !active.get_or_insert_with(HashSet::new).insert(key.clone()) {
            anyhow::bail!("{busy}");
        }
        Ok(Self(key))
    }
}
impl Drop for TargetGuard {
    fn drop(&mut self) {
        if let Some(active) = ACTIVE_TARGETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            active.remove(&self.0);
        }
    }
}
