//! A terminal's output kept for the next time it opens: the last lines of
//! its scrollback and screen, colours and all, written as the escape
//! sequences that draw them again in a new terminal.

use std::fmt::Write as _;

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// How many lines are kept.
pub const KEPT_LINES: usize = 1000;

/// What a cell's look depends on, of its flags.
const STYLE: Flags = Flags::BOLD
    .union(Flags::DIM)
    .union(Flags::ITALIC)
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::INVERSE)
    .union(Flags::HIDDEN)
    .union(Flags::STRIKEOUT);

/// The last `lines` lines of `term` that hold anything, from the history
/// on to the screen, as bytes that draw them again. A line the terminal
/// wrapped is written whole, to wrap again at the new terminal's width.
/// `None` while a full-screen program has the screen: its picture is not
/// output to keep.
pub fn kept_output<T: EventListener>(term: &Term<T>, lines: usize) -> Option<Vec<u8>> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return None;
    }
    let grid = term.grid();
    let has_text = |line: i32| !grid[Line(line)].is_clear();
    let top = term.topmost_line().0;
    let Some(last) = (top..=term.bottommost_line().0)
        .rev()
        .find(|line| has_text(*line))
    else {
        return Some(Vec::new());
    };
    let first = (top..=last)
        .find(|line| has_text(*line))
        .unwrap_or(last)
        .max(last + 1 - lines as i32);
    let columns = grid.columns();
    let mut out = String::new();
    for line in first..=last {
        let row = &grid[Line(line)];
        let wraps = row[Column(columns - 1)].flags.contains(Flags::WRAPLINE);
        // What is left of a line once its blank end is cut.
        let end = if wraps {
            columns
        } else {
            (0..columns)
                .rev()
                .find(|column| !is_blank(&row[Column(*column)]))
                .map_or(0, |column| column + 1)
        };
        let mut style: Option<(Color, Color, Flags)> = None;
        for column in 0..end {
            let cell = &row[Column(column)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let look = (cell.fg, cell.bg, cell.flags & STYLE);
            if style != Some(look) {
                write_style(&mut out, look);
                style = Some(look);
            }
            out.push(cell.c);
            for character in cell.zerowidth().unwrap_or_default() {
                out.push(*character);
            }
        }
        if style.is_some() {
            out.push_str("\x1b[0m");
        }
        if !wraps {
            out.push_str("\r\n");
        }
    }
    Some(out.into_bytes())
}

/// All of `term`'s output, the scrollback and the screen, as bytes that
/// draw it again: what a terminal connected again in its tab shows first
/// (issue #162). The new connection gets a new emulator, so nothing the
/// old programs switched on (mouse reporting, bracketed paste, cursor
/// keys) goes with it. A full-screen program's picture goes with the
/// connection it ran on; the lines it covered are kept, `term` left on
/// them. `None` when there is nothing.
pub fn output_for_reconnect<T: EventListener>(term: &mut Term<T>) -> Option<Vec<u8>> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        term.swap_alt();
    }
    let lines = term.grid().total_lines();
    kept_output(term, lines).filter(|output| !output.is_empty())
}

/// Whether a cell shows nothing: a space on the default background, with
/// nothing drawn over it.
fn is_blank(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.bg == Color::Named(NamedColor::Background)
        && !cell
            .flags
            .intersects(Flags::INVERSE | Flags::ALL_UNDERLINES | Flags::STRIKEOUT)
}

/// The SGR sequence that gives text this look, from the default one.
fn write_style(out: &mut String, (foreground, background, flags): (Color, Color, Flags)) {
    out.push_str("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::ALL_UNDERLINES, 4),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
    ] {
        if flags.intersects(flag) {
            let _ = write!(out, ";{code}");
        }
    }
    write_color(out, foreground, false);
    write_color(out, background, true);
    out.push('m');
}

fn write_color(out: &mut String, color: Color, background: bool) {
    let base = if background { 40 } else { 30 };
    match color {
        Color::Named(named) => {
            // A dim colour is the plain one drawn dim, which the flag says.
            let dim = NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize;
            let index = if dim.contains(&(named as usize)) {
                named.to_bright() as usize
            } else {
                named as usize
            };
            match index {
                0..=7 => {
                    let _ = write!(out, ";{}", base + index);
                }
                8..=15 => {
                    let _ = write!(out, ";{}", base + 60 + index - 8);
                }
                // The theme's own foreground and background: the default.
                _ => {}
            }
        }
        Color::Indexed(index) => {
            let _ = write!(out, ";{};5;{index}", base + 8);
        }
        Color::Spec(rgb) => {
            let _ = write!(out, ";{};2;{};{};{}", base + 8, rgb.r, rgb.g, rgb.b);
        }
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::{Processor, Rgb};

    use super::*;
    use crate::terminal::TerminalSize;

    fn term(columns: usize, rows: usize) -> Term<VoidListener> {
        Term::new(
            Config::default(),
            &TerminalSize::new(columns, rows, 8, 16),
            VoidListener,
        )
    }

    fn feed(term: &mut Term<VoidListener>, bytes: &[u8]) {
        let mut processor: Processor = Processor::new();
        processor.advance(term, bytes);
    }

    /// The text of each line with any, top to bottom.
    fn text(term: &Term<VoidListener>) -> Vec<String> {
        let grid = term.grid();
        (term.topmost_line().0..=term.bottommost_line().0)
            .map(|line| {
                let row = &grid[Line(line)];
                (0..grid.columns())
                    .map(|column| row[Column(column)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .filter(|line| !line.is_empty())
            .collect()
    }

    #[test]
    fn the_output_draws_again_with_its_colours() {
        let mut old = term(20, 5);
        feed(
            &mut old,
            b"$ ls\r\n\x1b[1;34msrc\x1b[0m  \x1b[38;2;255;0;0mred\x1b[0m\r\n\x1b[42mok\x1b[0m\r\n$ ",
        );
        let kept = kept_output(&old, KEPT_LINES).unwrap();

        let mut new = term(20, 5);
        feed(&mut new, &kept);
        assert_eq!(text(&new), ["$ ls", "src  red", "ok", "$"]);
        let row = &new.grid()[Line(1)];
        assert_eq!(row[Column(0)].fg, Color::Named(NamedColor::Blue));
        assert!(row[Column(0)].flags.contains(Flags::BOLD));
        assert_eq!(row[Column(5)].fg, Color::Spec(Rgb { r: 255, g: 0, b: 0 }));
        assert_eq!(
            new.grid()[Line(2)][Column(0)].bg,
            Color::Named(NamedColor::Green)
        );
        // Each line ends on the default look, the cursor on a new line.
        assert!(kept.ends_with(b"\r\n"));
        assert_eq!(new.grid().cursor.point.line, Line(4));
    }

    #[test]
    fn only_the_last_lines_are_kept_and_wrapped_lines_wrap_again() {
        let mut old = term(10, 4);
        for number in 0..30 {
            feed(&mut old, format!("line {number}\r\n").as_bytes());
        }
        let kept = kept_output(&old, 3).unwrap();
        let mut new = term(10, 4);
        feed(&mut new, &kept);
        assert_eq!(text(&new), ["line 27", "line 28", "line 29"]);

        // A line wider than the terminal, kept whole: wider now, it is one.
        let mut old = term(10, 4);
        feed(&mut old, b"abcdefghijklmno\r\n");
        let kept = kept_output(&old, KEPT_LINES).unwrap();
        let mut new = term(20, 4);
        feed(&mut new, &kept);
        assert_eq!(text(&new), ["abcdefghijklmno"]);
    }

    #[test]
    fn wide_characters_and_an_empty_terminal() {
        let mut old = term(10, 4);
        feed(&mut old, "会议纪要.md\r\n".as_bytes());
        let mut new = term(20, 4);
        feed(&mut new, &kept_output(&old, KEPT_LINES).unwrap());
        assert_eq!(text(&new)[0].replace(' ', ""), "会议纪要.md");

        assert_eq!(kept_output(&term(10, 4), KEPT_LINES), Some(Vec::new()));
    }

    #[test]
    fn a_full_screen_program_keeps_nothing() {
        let mut old = term(10, 4);
        feed(&mut old, b"$ vim\r\n\x1b[?1049h~\r\n~");
        assert_eq!(kept_output(&old, KEPT_LINES), None);
    }

    /// Reconnecting in the tab (issue #162) keeps all of the scrollback,
    /// not just the last `KEPT_LINES`, and the shell's lines a full-screen
    /// program covered, but none of the modes the programs set.
    #[test]
    fn reconnecting_keeps_everything_but_the_modes() {
        let mut old = term(20, 5);
        for number in 0..KEPT_LINES + 500 {
            feed(&mut old, format!("line {number}\r\n").as_bytes());
        }
        feed(
            &mut old,
            b"$ vim\r\n\x1b[?2004h\x1b[?1000h\x1b[?1h\x1b[?1049h~ vim's screen\r\n~",
        );
        let kept = output_for_reconnect(&mut old).unwrap();
        let mut new = term(20, 5);
        feed(&mut new, &kept);
        let lines = text(&new);
        assert_eq!(lines.len(), KEPT_LINES + 501);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines.last().unwrap(), "$ vim");
        assert!(!new.mode().intersects(
            TermMode::ALT_SCREEN
                | TermMode::BRACKETED_PASTE
                | TermMode::MOUSE_REPORT_CLICK
                | TermMode::APP_CURSOR
        ));

        assert_eq!(output_for_reconnect(&mut term(10, 4)), None);
    }
}
