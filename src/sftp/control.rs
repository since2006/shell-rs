use super::{SftpEvent, UploadAnswer, UploadChoice, UploadQuestion, UploadQuestionKind};
use anyhow::Result;
use async_channel::{Receiver, Sender};
use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
};
use tokio::sync::watch;
static NEXT_QUESTION: AtomicU64 = AtomicU64::new(1);
#[derive(Debug)]
pub(crate) struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("上传已停止，进度已保留")
    }
}
impl std::error::Error for Cancelled {}

pub(crate) struct UploadControl {
    pub events: Sender<SftpEvent>,
    pub cancel: watch::Receiver<bool>,
    pub answers: Receiver<(u64, UploadAnswer)>,
}
impl UploadControl {
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
        kind: UploadQuestionKind,
        path: &str,
        message: &str,
    ) -> Result<UploadAnswer> {
        self.check()?;
        let id = NEXT_QUESTION.fetch_add(1, Ordering::Relaxed);
        self.events
            .send(SftpEvent::Question(UploadQuestion::new(
                id, kind, path, message,
            )))
            .await?;
        self.run(async {
            loop {
                let (reply_id, answer) = self.answers.recv().await?;
                if reply_id == id {
                    return if answer.choice() == UploadChoice::Cancel {
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
