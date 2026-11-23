//! Mouse reporting: what a program that asked for the mouse (vim with
//! `mouse=a`, htop, tmux) is told of a click, a drag or the wheel, in the
//! encodings xterm defines, and when it stops asking.

use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::{Params, Parser, Perform};

/// The button a report names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportButton {
    Left,
    Middle,
    WheelUp,
    WheelDown,
    /// Motion with no button held, which only 1003 asks for.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportKind {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    pub button: ReportButton,
    pub kind: ReportKind,
    /// The cell, counted from the top left of the screen from 0.
    pub column: usize,
    pub row: usize,
    pub alt: bool,
    pub control: bool,
}

/// Whether the program asked to hear of `kind`: 1000 reports presses,
/// releases and the wheel, 1002 motion while a button is held as well, 1003
/// all motion.
pub fn wants(mode: TermMode, kind: ReportKind, button_held: bool) -> bool {
    match kind {
        ReportKind::Press | ReportKind::Release => mode.intersects(TermMode::MOUSE_MODE),
        ReportKind::Motion => {
            mode.contains(TermMode::MOUSE_MOTION)
                || (button_held && mode.contains(TermMode::MOUSE_DRAG))
        }
    }
}

/// The bytes that tell the program of `report`. `None` where the encoding
/// in use cannot say it: the default one stops at column and row 223.
pub fn encode(report: MouseReport, mode: TermMode) -> Option<Vec<u8>> {
    let button = match report.button {
        ReportButton::Left => 0,
        ReportButton::Middle => 1,
        ReportButton::None => 3,
        ReportButton::WheelUp => 64,
        ReportButton::WheelDown => 65,
    };
    let modifiers = 8 * u32::from(report.alt) + 16 * u32::from(report.control);
    let motion = if report.kind == ReportKind::Motion {
        32
    } else {
        0
    };
    let column = report.column as u32 + 1;
    let row = report.row as u32 + 1;
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if report.kind == ReportKind::Release {
            'm'
        } else {
            'M'
        };
        let code = button + modifiers + motion;
        return Some(format!("\x1b[<{code};{column};{row}{end}").into_bytes());
    }
    // The older encodings cannot tell which button went up.
    let code = if report.kind == ReportKind::Release {
        3 + modifiers
    } else {
        button + modifiers + motion
    };
    let mut bytes = b"\x1b[M".to_vec();
    bytes.push(32 + code as u8);
    for value in [column, row] {
        let value = 32 + value;
        if mode.contains(TermMode::UTF8_MOUSE) {
            // Two UTF-8 bytes at most, as in xterm.
            let character = char::from_u32(value).filter(|_| value < 0x800)?;
            let mut buffer = [0; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        } else {
            bytes.push(u8::try_from(value).ok()?);
        }
    }
    Some(bytes)
}

/// What has Alacritty turn off every mode of mouse tracking.
pub(super) const TRACKING_OFF: &[u8] = b"\x1b[?1000;1002;1003l";

/// Turns all of mouse tracking off whenever a program turns off any of it,
/// as xterm and Windows Terminal do: 1000, 1002 and 1003 are one setting
/// there. Alacritty turns off only the mode named, so 1003 turned on and
/// then off as 1000 and 1002, as tmux 2.4 to 3.1 do on detaching, left
/// every motion reported: junk typed at the shell, and a drag selecting
/// nothing (issue #150).
///
/// It reads the output beside Alacritty, with the parser Alacritty reads it
/// with, and cuts it after each such sequence for `TRACKING_OFF` to go in:
/// in the output, so a synchronized update holds it back with the rest.
#[derive(Default)]
pub(super) struct TrackingOff {
    parser: Parser,
}

impl TrackingOff {
    /// `bytes` for `advance` to hand Alacritty, in pieces, with
    /// `TRACKING_OFF` after each that turns some tracking off.
    pub fn feed(&mut self, bytes: &[u8], mut advance: impl FnMut(&[u8])) {
        let mut rest = bytes;
        while !rest.is_empty() {
            let mut seen = SeenOff(false);
            let read = self.parser.advance_until_terminated(&mut seen, rest);
            advance(&rest[..read]);
            if seen.0 {
                advance(TRACKING_OFF);
            }
            rest = &rest[read..];
        }
    }
}

/// Whether the sequence just read turned some mouse tracking off.
struct SeenOff(bool);

impl Perform for SeenOff {
    /// `CSI ? Pm l`, read as Alacritty reads it: each parameter a mode, a
    /// sequence with too many intermediates nothing.
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.0 = action == 'l'
            && intermediates == b"?"
            && !ignore
            && params
                .iter()
                .any(|param| matches!(param[0], 1000 | 1002 | 1003));
    }

    fn terminated(&self) -> bool {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::Term;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    /// What `TrackingOff` hands on, chunk by chunk.
    fn pieces(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut off = TrackingOff::default();
        let mut pieces = Vec::new();
        for chunk in chunks {
            off.feed(chunk, |piece| pieces.push(piece.to_vec()));
        }
        pieces
    }

    fn bytes(pieces: &[&[u8]]) -> Vec<Vec<u8>> {
        pieces.iter().map(|piece| piece.to_vec()).collect()
    }

    /// The tracking modes on after `chunks`, through `TrackingOff` into
    /// Alacritty as the parser thread feeds it.
    fn tracking_after(chunks: &[&[u8]]) -> TermMode {
        let mut term = Term::new(Config::default(), &TermSize::new(80, 24), VoidListener);
        let mut processor: Processor = Processor::new();
        let mut off = TrackingOff::default();
        for chunk in chunks {
            off.feed(chunk, |piece| processor.advance(&mut term, piece));
        }
        *term.mode() & TermMode::MOUSE_MODE
    }

    #[test]
    fn turning_off_any_tracking_turns_off_all_of_it() {
        // What tmux 3.0a sends on detaching, with 1003 on.
        let detach: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1006l\x1b[?1005l\x1b[?1049l$ ";
        assert_eq!(
            pieces(&[detach]),
            bytes(&[
                b"\x1b[?1000l",
                TRACKING_OFF,
                b"\x1b[?1002l",
                TRACKING_OFF,
                b"\x1b[?1006l\x1b[?1005l\x1b[?1049l$ ",
            ])
        );
        // Several modes in one, split between chunks.
        assert_eq!(
            pieces(&[b"a\x1b[?10", b"06;1003lb"]),
            bytes(&[b"a\x1b[?10", b"06;1003l", TRACKING_OFF, b"b"])
        );

        assert_eq!(
            tracking_after(&[b"\x1b[?1006h\x1b[?1003h", detach]),
            TermMode::empty()
        );
        assert_eq!(
            tracking_after(&[b"\x1b[?1002h\x1b[?1000l"]),
            TermMode::empty()
        );
    }

    #[test]
    fn tracking_turned_on_again_stays_on() {
        // Off and on again in one chunk: on, in that order.
        assert_eq!(
            tracking_after(&[b"\x1b[?1003h\x1b[?1000l\x1b[?1003h"]),
            TermMode::MOUSE_MOTION
        );
        // In a synchronized update the off waits with the rest of it.
        let update: &[u8] = b"\x1b[?2026h\x1b[?1000l\x1b[?1002h";
        assert_eq!(
            tracking_after(&[b"\x1b[?1003h", update]),
            TermMode::MOUSE_MOTION
        );
        assert_eq!(
            tracking_after(&[b"\x1b[?1003h", update, b"\x1b[?2026l"]),
            TermMode::MOUSE_DRAG
        );
    }

    #[test]
    fn other_sequences_pass_as_they_are() {
        for sequence in [
            &b"\x1b[?1003h"[..],
            b"\x1b[?1004l\x1b[?1006l\x1b[?2004l",
            b"\x1b[1000l",
            // A title with the text in it.
            b"\x1b]0;[?1000l\x07",
            b"[?1000l",
        ] {
            assert_eq!(pieces(&[sequence]), bytes(&[sequence]));
        }
    }

    fn report(button: ReportButton, kind: ReportKind, column: usize, row: usize) -> MouseReport {
        MouseReport {
            button,
            kind,
            column,
            row,
            alt: false,
            control: false,
        }
    }

    fn sgr() -> TermMode {
        TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE
    }

    #[test]
    fn sgr_reports_name_the_button_both_ways_and_count_from_one() {
        let press = report(ReportButton::Left, ReportKind::Press, 2, 0);
        assert_eq!(encode(press, sgr()).unwrap(), b"\x1b[<0;3;1M");
        let release = report(ReportButton::Left, ReportKind::Release, 2, 0);
        assert_eq!(encode(release, sgr()).unwrap(), b"\x1b[<0;3;1m");
        let middle = report(ReportButton::Middle, ReportKind::Press, 0, 4);
        assert_eq!(encode(middle, sgr()).unwrap(), b"\x1b[<1;1;5M");
        let drag = report(ReportButton::Left, ReportKind::Motion, 9, 9);
        assert_eq!(encode(drag, sgr()).unwrap(), b"\x1b[<32;10;10M");
        let wheel = report(ReportButton::WheelDown, ReportKind::Press, 0, 0);
        assert_eq!(encode(wheel, sgr()).unwrap(), b"\x1b[<65;1;1M");
        let held = MouseReport {
            alt: true,
            control: true,
            ..report(ReportButton::WheelUp, ReportKind::Press, 0, 0)
        };
        assert_eq!(encode(held, sgr()).unwrap(), b"\x1b[<88;1;1M");
        // SGR has no limit.
        let far = report(ReportButton::Left, ReportKind::Press, 499, 299);
        assert_eq!(encode(far, sgr()).unwrap(), b"\x1b[<0;500;300M");
    }

    #[test]
    fn the_default_encoding_adds_32_and_stops_at_223() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        let press = report(ReportButton::Left, ReportKind::Press, 2, 0);
        assert_eq!(encode(press, mode).unwrap(), b"\x1b[M #!");
        // A release does not say which button.
        let release = MouseReport {
            control: true,
            ..report(ReportButton::Middle, ReportKind::Release, 2, 0)
        };
        assert_eq!(encode(release, mode).unwrap(), b"\x1b[M3#!");
        let edge = report(ReportButton::Left, ReportKind::Press, 222, 0);
        assert_eq!(encode(edge, mode).unwrap(), b"\x1b[M \xff!");
        let beyond = report(ReportButton::Left, ReportKind::Press, 223, 0);
        assert_eq!(encode(beyond, mode), None);
    }

    #[test]
    fn the_utf8_encoding_writes_large_coordinates_as_characters() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::UTF8_MOUSE;
        let near = report(ReportButton::Left, ReportKind::Press, 2, 0);
        assert_eq!(encode(near, mode).unwrap(), b"\x1b[M #!");
        let far = report(ReportButton::Left, ReportKind::Press, 299, 0);
        let mut expected = b"\x1b[M ".to_vec();
        expected.extend_from_slice("\u{14c}!".as_bytes());
        assert_eq!(encode(far, mode).unwrap(), expected);
        let beyond = report(ReportButton::Left, ReportKind::Press, 2015, 0);
        assert_eq!(encode(beyond, mode), None);
    }

    #[test]
    fn each_mode_asks_for_its_own_events() {
        let click = TermMode::MOUSE_REPORT_CLICK;
        let drag = TermMode::MOUSE_DRAG;
        let motion = TermMode::MOUSE_MOTION;
        for mode in [click, drag, motion] {
            assert!(wants(mode, ReportKind::Press, false));
            assert!(wants(mode, ReportKind::Release, true));
        }
        assert!(!wants(click, ReportKind::Motion, true));
        assert!(wants(drag, ReportKind::Motion, true));
        assert!(!wants(drag, ReportKind::Motion, false));
        assert!(wants(motion, ReportKind::Motion, false));
        assert!(!wants(TermMode::empty(), ReportKind::Press, false));
    }
}
