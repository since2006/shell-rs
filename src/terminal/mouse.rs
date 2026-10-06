//! Mouse reporting: what a program that asked for the mouse (vim with
//! `mouse=a`, htop, tmux) is told of a click, a drag or the wheel, in the
//! encodings xterm defines.

use alacritty_terminal::term::TermMode;

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

#[cfg(test)]
mod tests {
    use super::*;

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
