//! Foreground-owned polling hints. Parser threads still never wake GPUI.
use std::cell::Cell;
use std::time::Duration;

pub(super) struct PollSchedule {
    visible: Cell<bool>,
    wake: async_channel::Sender<()>,
}

impl PollSchedule {
    pub(super) fn new() -> (Self, async_channel::Receiver<()>) {
        let (wake, receiver) = async_channel::bounded(1);
        (
            Self {
                visible: Cell::new(true),
                wake,
            },
            receiver,
        )
    }

    pub(super) fn interactive(&self) {
        let _ = self.wake.try_send(());
    }

    pub(super) fn set_visible(&self, visible: bool) {
        if self.visible.replace(visible) != visible && visible {
            self.interactive();
        }
    }

    pub(super) fn delay(&self, empty_polls: u32) -> Duration {
        Duration::from_millis(if !self.visible.get() {
            100
        } else {
            // Back off after quiet polls; typing/focus interrupts this wait.
            match empty_polls {
                0..=3 => 16,
                4..=7 => 32,
                _ => 64,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_and_hidden_back_off_but_input_and_reveal_wake_immediately() {
        let (schedule, wake) = PollSchedule::new();
        assert_eq!(schedule.delay(0), Duration::from_millis(16));
        assert_eq!(schedule.delay(20), Duration::from_millis(64));
        schedule.set_visible(false);
        assert_eq!(schedule.delay(0), Duration::from_millis(100));
        assert!(wake.try_recv().is_err());
        schedule.interactive();
        schedule.interactive();
        assert!(wake.try_recv().is_ok());
        assert!(wake.try_recv().is_err());
        schedule.set_visible(true);
        assert!(wake.try_recv().is_ok());
        assert_eq!(schedule.delay(0), Duration::from_millis(16));
    }
}
