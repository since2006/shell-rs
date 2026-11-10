//! How user input typed in one go is spread out: a paste or a script of
//! lines sent a line at a time, and a character at a time within a line,
//! as slow network equipment needs to hear it.
//!
//! The settings are a global, like `TerminalInteraction`, read when the
//! input is queued.

use std::ops::RangeInclusive;

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

/// How long a wait before a line may be, in milliseconds.
pub const LINE_DELAY_RANGE: RangeInclusive<u32> = 0..=10_000;
/// How long a wait before a character may be, in milliseconds.
pub const CHARACTER_DELAY_RANGE: RangeInclusive<u32> = 0..=1_000;

/// 终端 → 发送延迟.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SendDelay {
    /// 行延迟: the wait after a line's Enter.
    pub line_ms: u32,
    /// 字符延迟: the wait between two characters of a line.
    pub character_ms: u32,
}

impl Global for SendDelay {}

impl SendDelay {
    /// What the terminals do now.
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    /// No waits at all: the input goes as one write, as it always did.
    pub fn idle(&self) -> bool {
        self.line_ms == 0 && self.character_ms == 0
    }

    /// The settings with both waits in the range the app accepts.
    pub fn normalized(mut self) -> Self {
        self.line_ms = self
            .line_ms
            .clamp(*LINE_DELAY_RANGE.start(), *LINE_DELAY_RANGE.end());
        self.character_ms = self
            .character_ms
            .clamp(*CHARACTER_DELAY_RANGE.start(), *CHARACTER_DELAY_RANGE.end());
        self
    }
}

/// The writes `text` is made of, with how long to wait before each, in
/// milliseconds. The first write is due at once; a line ends with `\r`, as
/// pressing Enter does, and waits `line_ms` after it; characters within a
/// line wait `character_ms` from one to the next. A text that ends with a
/// line break has nothing after its last Enter.
pub fn delayed_steps(text: &str, delay: SendDelay) -> Vec<(u64, Vec<u8>)> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let last = lines.len().saturating_sub(1);
    let mut steps: Vec<(u64, Vec<u8>)> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if delay.character_ms == 0 {
            if !line.is_empty() {
                steps.push((0, line.as_bytes().to_vec()));
            }
        } else {
            for character in line.chars() {
                let wait = if steps.is_empty() {
                    0
                } else {
                    u64::from(delay.character_ms)
                };
                let mut buffer = [0u8; 4];
                steps.push((wait, character.encode_utf8(&mut buffer).as_bytes().to_vec()));
            }
        }
        if index < last {
            steps.push((u64::from(delay.line_ms), b"\r".to_vec()));
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(text: &str, line_ms: u32, character_ms: u32) -> Vec<(u64, String)> {
        delayed_steps(
            text,
            SendDelay {
                line_ms,
                character_ms,
            },
        )
        .into_iter()
        .map(|(wait, bytes)| (wait, String::from_utf8(bytes).unwrap()))
        .collect()
    }

    #[test]
    fn nothing_waits_without_delays() {
        assert_eq!(
            steps("one\ntwo\n", 0, 0),
            [
                (0, "one".into()),
                (0, "\r".into()),
                (0, "two".into()),
                (0, "\r".into())
            ]
        );
    }

    #[test]
    fn a_line_waits_after_its_enter() {
        assert_eq!(
            steps("one\ntwo", 200, 0),
            [(0, "one".into()), (200, "\r".into()), (0, "two".into())]
        );
    }

    #[test]
    fn characters_wait_from_one_to_the_next() {
        assert_eq!(
            steps("ab\nc", 100, 20),
            [
                (0, "a".into()),
                (20, "b".into()),
                (100, "\r".into()),
                (20, "c".into())
            ]
        );
    }

    #[test]
    fn blank_lines_are_pressed_through() {
        assert_eq!(
            steps("a\n\nb", 50, 0),
            [
                (0, "a".into()),
                (50, "\r".into()),
                (50, "\r".into()),
                (0, "b".into())
            ]
        );
    }

    #[test]
    fn windows_and_old_mac_line_breaks_are_the_same() {
        assert_eq!(
            steps("a\r\nb\rc", 10, 0),
            [
                (0, "a".into()),
                (10, "\r".into()),
                (0, "b".into()),
                (10, "\r".into()),
                (0, "c".into())
            ]
        );
    }

    #[test]
    fn a_text_without_a_last_break_does_not_press_enter() {
        assert_eq!(steps("one", 10, 0), [(0, "one".into())]);
        assert_eq!(
            steps("one\n", 10, 0),
            [(0, "one".into()), (10, "\r".into())]
        );
    }
}
