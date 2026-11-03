//! Several items of a batch at once, as WinSCP's parallel transfers take the
//! files of a batch one by one from a shared list. WinSCP opens more
//! connections for it; here the items share the batch's one SFTP session,
//! whose requests already go out without waiting for each other, so no
//! second login is asked of a bastion host. An item inside a folder of the
//! batch waits until the folder is done, as WinSCP's does.

use anyhow::Result;
use futures::{StreamExt as _, stream::FuturesUnordered};
use std::{
    collections::VecDeque,
    future::Future,
    ops::RangeInclusive,
    sync::atomic::{AtomicUsize, Ordering},
};

/// 最大同时传输数量 until the user changes it.
pub const DEFAULT_PARALLEL_TRANSFERS: usize = 3;
/// What 最大同时传输数量 offers.
pub const PARALLEL_TRANSFERS_RANGE: RangeInclusive<usize> = 1..=10;

/// 设置 › SFTP › 最大同时传输数量: how many items of a batch move at once,
/// in every SFTP tab and 文件 sidebar. The worker threads have no app to
/// read settings from, so `settings::apply` leaves it here; a running batch
/// goes by a new number from its next item on.
static PARALLEL_TRANSFERS: AtomicUsize = AtomicUsize::new(DEFAULT_PARALLEL_TRANSFERS);

pub fn set_parallel_transfers(count: usize) {
    PARALLEL_TRANSFERS.store(
        count.clamp(
            *PARALLEL_TRANSFERS_RANGE.start(),
            *PARALLEL_TRANSFERS_RANGE.end(),
        ),
        Ordering::Relaxed,
    );
}

pub(crate) fn parallel_transfers() -> usize {
    PARALLEL_TRANSFERS.load(Ordering::Relaxed)
}

/// Run the items `pending` lists, in its order, up to `limit()` at once.
/// `waits(next, running)` says whether `next` has to wait for one of the
/// items running now. An item ends with the items to run before the rest
/// (a folder scanned again on 重试), or with an error that ends the run, as
/// a lost connection or a stop does. No more start then, and the ones
/// running end on their own: a stop reaches each of them, which then closes
/// what it opened, and a lost connection fails them too. Those that do not
/// end well go back to the front of `pending`, in their order, for the next
/// run.
pub(crate) async fn run<F>(
    pending: &mut VecDeque<usize>,
    limit: impl Fn() -> usize,
    waits: impl Fn(usize, &[usize]) -> bool,
    mut start: impl FnMut(usize) -> F,
) -> Result<()>
where
    F: Future<Output = Result<Vec<usize>>>,
{
    let mut running: Vec<usize> = Vec::new();
    let mut tasks = FuturesUnordered::new();
    let result = loop {
        while running.len() < limit()
            && let Some(&next) = pending.front()
            && (running.is_empty() || !waits(next, &running))
        {
            pending.pop_front();
            running.push(next);
            let task = start(next);
            tasks.push(async move { (next, task.await) });
        }
        let Some((index, result)) = tasks.next().await else {
            break Ok(());
        };
        match result {
            Ok(first) => {
                running.retain(|&item| item != index);
                for item in first.into_iter().rev() {
                    pending.push_front(item);
                }
            }
            // The item that failed goes back with the rest.
            Err(error) => break Err(error),
        }
    };
    if result.is_err() {
        while let Some((index, result)) = tasks.next().await {
            if let Ok(first) = result {
                running.retain(|&item| item != index);
                for item in first.into_iter().rev() {
                    pending.push_front(item);
                }
            }
        }
    }
    for item in running.into_iter().rev() {
        pending.push_front(item);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, time::Duration};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap()
    }

    /// Items overlap up to the limit, and one that has to wait starts only
    /// once what it waits for has ended.
    #[test]
    fn items_overlap_and_wait_for_their_folder() {
        runtime().block_on(async {
            let mut pending: VecDeque<usize> = (0..20).collect();
            let log = RefCell::new(Vec::new());
            let live = RefCell::new(0usize);
            let most = RefCell::new(0usize);
            // Item 10 is a folder; 11 and 12 are in it.
            let waits =
                |next: usize, running: &[usize]| matches!(next, 11 | 12) && running.contains(&10);
            parallel_run(&mut pending, waits, &log, &live, &most)
                .await
                .unwrap();
            assert!(pending.is_empty());
            assert_eq!(*most.borrow(), 4);
            let log = log.into_inner();
            let ended = |item| log.iter().position(|e| *e == ("end", item)).unwrap();
            let began = |item| log.iter().position(|e| *e == ("begin", item)).unwrap();
            assert!(began(11) > ended(10));
            assert!(began(12) > ended(10));
            assert_eq!(log.iter().filter(|e| e.0 == "end").count(), 20);
        });
    }

    /// A failure ends the run: nothing more starts, the items running end
    /// on their own, and those that fail are first in line next time.
    #[test]
    fn an_error_lets_the_running_items_end_and_puts_the_failed_back() {
        runtime().block_on(async {
            let mut pending: VecDeque<usize> = (0..12).collect();
            let ended = RefCell::new(Vec::new());
            let result = run(
                &mut pending,
                || 8,
                |_, _| false,
                |item| {
                    let ended = &ended;
                    async move {
                        tokio::time::sleep(Duration::from_millis(if item == 3 { 1 } else { 50 }))
                            .await;
                        if item == 3 || item == 5 {
                            anyhow::bail!("connection lost");
                        }
                        ended.borrow_mut().push(item);
                        Ok(vec![])
                    }
                },
            )
            .await;
            assert!(result.is_err());
            let mut ended = ended.into_inner();
            ended.sort();
            assert_eq!(ended, vec![0, 1, 2, 4, 6, 7]);
            assert_eq!(Vec::from(pending), vec![3, 5, 8, 9, 10, 11]);
        });
    }

    async fn parallel_run(
        pending: &mut VecDeque<usize>,
        waits: impl Fn(usize, &[usize]) -> bool,
        log: &RefCell<Vec<(&'static str, usize)>>,
        live: &RefCell<usize>,
        most: &RefCell<usize>,
    ) -> Result<()> {
        run(
            pending,
            || 4,
            waits,
            |item| async move {
                log.borrow_mut().push(("begin", item));
                *live.borrow_mut() += 1;
                let now = *live.borrow();
                most.replace_with(|most| (*most).max(now));
                tokio::time::sleep(Duration::from_millis(10 + item as u64 % 3)).await;
                *live.borrow_mut() -= 1;
                log.borrow_mut().push(("end", item));
                Ok(vec![])
            },
        )
        .await
    }
}
