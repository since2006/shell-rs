//! An object's events, waiting for the app to take them.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use tokio::sync::Notify;

/// Events in order, taken one at a time by `next`. Nothing is ever lost
/// for want of room, so a question for the user is never mistaken for a
/// closed window; what repeats (a latency reading) replaces its older self.
pub(crate) struct Events<T> {
    state: Mutex<State<T>>,
    ready: Notify,
}

struct State<T> {
    queue: VecDeque<T>,
    closed: bool,
}

impl<T> Events<T> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                closed: false,
            }),
            ready: Notify::new(),
        }
    }

    /// Queue `event`, unless the events have ended.
    pub(crate) fn push(&self, event: T) {
        self.push_replacing(event, |_| false);
    }

    /// Queue `event` in place of those still waiting that `stale` picks.
    pub(crate) fn push_replacing(&self, event: T, stale: impl Fn(&T) -> bool) {
        {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.queue.retain(|queued| !stale(queued));
            state.queue.push_back(event);
        }
        self.ready.notify_waiters();
    }

    /// Queue `event` as the last one: `next` gives `None` once it is taken.
    pub(crate) fn finish(&self, event: Option<T>) {
        {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.queue.extend(event);
            state.closed = true;
        }
        self.ready.notify_waiters();
    }

    /// Whether the last event has been queued.
    pub(crate) fn is_finished(&self) -> bool {
        self.lock().closed
    }

    /// The next event, waiting for one; `None` after the last.
    pub(crate) async fn next(&self) -> Option<T> {
        loop {
            let notified = self.ready.notified();
            tokio::pin!(notified);
            // Listening before looking, so an event queued in between still
            // wakes this.
            notified.as_mut().enable();
            {
                let mut state = self.lock();
                if let Some(event) = state.queue.pop_front() {
                    return Some(event);
                }
                if state.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::Events;

    #[tokio::test]
    async fn events_come_in_order_and_end_after_the_last() {
        let events = Events::new();
        events.push(1);
        events.push(2);
        events.finish(Some(3));
        events.push(4);
        assert_eq!(events.next().await, Some(1));
        assert_eq!(events.next().await, Some(2));
        assert_eq!(events.next().await, Some(3));
        assert_eq!(events.next().await, None);
    }

    #[tokio::test]
    async fn a_newer_reading_replaces_the_one_still_waiting() {
        let events = Events::new();
        events.push(("prompt", 1));
        events.push_replacing(("latency", 2), |event| event.0 == "latency");
        events.push_replacing(("latency", 3), |event| event.0 == "latency");
        assert_eq!(events.next().await, Some(("prompt", 1)));
        assert_eq!(events.next().await, Some(("latency", 3)));
    }

    #[tokio::test]
    async fn a_waiting_taker_wakes_for_an_event() {
        let events = std::sync::Arc::new(Events::new());
        let taker = tokio::spawn({
            let events = events.clone();
            async move { events.next().await }
        });
        tokio::task::yield_now().await;
        events.push(7);
        assert_eq!(taker.await.unwrap(), Some(7));
    }
}
