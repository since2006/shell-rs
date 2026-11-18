//! What an SSH host's terminal types once logged in (issue #138): its lines,
//! one at a time, each once the output before it has paused. They then come
//! after the prompt, as a user would type them, and a program that throws
//! away what was typed ahead (a bastion host's menu) still gets them.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::{SharedTerminalTransportFactory, TerminalTransport, TerminalTransportFactory};

/// How long the output has to pause before the next line is typed.
pub(super) const QUIET: Duration = Duration::from_millis(500);

/// How long a line waits at most: for a shell that prints nothing, or
/// output that never pauses.
const LONGEST_WAIT: Duration = Duration::from_secs(3);

/// `factory`'s connections, each typing `lines` once its shell is up.
pub fn typing_on_login(
    factory: SharedTerminalTransportFactory,
    lines: Vec<String>,
) -> SharedTerminalTransportFactory {
    if lines.is_empty() {
        factory
    } else {
        std::sync::Arc::new(TypingOnLogin { factory, lines })
    }
}

struct TypingOnLogin {
    factory: SharedTerminalTransportFactory,
    lines: Vec<String>,
}

impl TerminalTransportFactory for TypingOnLogin {
    fn create(&self) -> Box<dyn TerminalTransport> {
        self.factory.create()
    }

    fn login_commands(&self) -> &[String] {
        &self.lines
    }
}

/// The lines one connection has still to type, and when the next is due.
/// The terminal's parser thread keeps it: it sees the output as it comes.
#[derive(Debug, Default)]
pub(super) struct LoginTyping {
    lines: VecDeque<String>,
    /// When the shell came up or the last line was typed; `None` until the
    /// shell is up.
    since: Option<Instant>,
    /// When output last came after that.
    output: Option<Instant>,
}

impl LoginTyping {
    pub(super) fn new(lines: &[String]) -> Self {
        Self {
            lines: lines.iter().cloned().collect(),
            ..Self::default()
        }
    }

    /// The shell is up: the first line waits from now.
    pub(super) fn started(&mut self, now: Instant) {
        self.since = Some(now);
        self.output = None;
    }

    /// Output came: the next line waits for it to pause.
    pub(super) fn output(&mut self, now: Instant) {
        if self.since.is_some() {
            self.output = Some(now);
        }
    }

    /// Something has the terminal's input for now (a ZMODEM transfer): the
    /// next line waits for a pause after it, however long it takes.
    pub(super) fn hold(&mut self, now: Instant) {
        if self.since.is_some() {
            self.since = Some(now);
            self.output = Some(now);
        }
    }

    /// When the next line is due; `None` before the shell is up and once
    /// every line is typed.
    pub(super) fn deadline(&self) -> Option<Instant> {
        let since = self.since.filter(|_| !self.lines.is_empty())?;
        let longest = since + LONGEST_WAIT;
        Some(match self.output {
            Some(output) => (output + QUIET).min(longest),
            None => longest,
        })
    }

    /// What to type by `now`: the next line and Enter, once it is due.
    pub(super) fn due(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.deadline()? > now {
            return None;
        }
        let mut typed = self.lines.pop_front()?.into_bytes();
        typed.push(b'\r');
        self.since = Some(now);
        self.output = None;
        Some(typed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typing(lines: &[&str]) -> LoginTyping {
        LoginTyping::new(
            &lines
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn each_line_waits_for_the_output_before_it_to_pause() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut login = typing(&["sudo -i", "", "cd /srv"]);
        // Nothing before the shell is up.
        assert_eq!(login.deadline(), None);
        assert_eq!(login.due(at(10_000)), None);

        login.started(at(0));
        login.output(at(100));
        login.output(at(400));
        assert_eq!(login.due(at(899)), None);
        assert_eq!(login.deadline(), Some(at(900)));
        assert_eq!(login.due(at(900)), Some(b"sudo -i\r".to_vec()));
        // The next waits for the echo and what follows to pause.
        assert_eq!(login.deadline(), Some(at(3_900)));
        login.output(at(950));
        assert_eq!(login.due(at(1_449)), None);
        // A blank line is an Enter.
        assert_eq!(login.due(at(1_450)), Some(b"\r".to_vec()));
        login.output(at(1_500));
        assert_eq!(login.due(at(2_000)), Some(b"cd /srv\r".to_vec()));
        // Nothing left.
        assert_eq!(login.deadline(), None);
        assert_eq!(login.due(at(60_000)), None);
    }

    #[test]
    fn a_line_waits_only_so_long_for_silence_or_a_pause() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        // A shell that prints nothing.
        let mut login = typing(&["ls", "pwd"]);
        login.started(at(0));
        assert_eq!(login.due(at(2_999)), None);
        assert_eq!(login.due(at(3_000)), Some(b"ls\r".to_vec()));
        // Output that never pauses.
        for ms in (3_100..6_000).step_by(100) {
            login.output(at(ms));
            assert_eq!(login.due(at(ms)), None, "{ms}");
        }
        login.output(at(6_000));
        assert_eq!(login.due(at(6_000)), Some(b"pwd\r".to_vec()));
    }

    #[test]
    fn a_transfer_holds_the_next_line_until_it_is_over() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut login = typing(&["rz", "ls"]);
        login.started(at(0));
        login.output(at(100));
        assert_eq!(login.due(at(600)), Some(b"rz\r".to_vec()));
        for ms in (1_000..10_000).step_by(1_000) {
            login.hold(at(ms));
            assert_eq!(login.due(at(ms)), None, "{ms}");
        }
        assert_eq!(login.due(at(9_499)), None);
        assert_eq!(login.due(at(9_500)), Some(b"ls\r".to_vec()));
    }

    #[test]
    fn a_terminal_that_types_nothing_never_waits() {
        let mut login = typing(&[]);
        let now = Instant::now();
        login.started(now);
        login.output(now);
        assert_eq!(login.deadline(), None);
        assert_eq!(login.due(now + LONGEST_WAIT), None);
    }
}
