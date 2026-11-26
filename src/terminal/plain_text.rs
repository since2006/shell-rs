//! What a terminal shows, as plain text: for the AI assistant to read, and
//! for nothing that draws it again.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Flags, LineLength as _};

/// The last lines a terminal shows, as plain text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalText {
    /// One line per line of output: a line the terminal wrapped is joined
    /// whole, the blank end of each is cut, there are no colours, and
    /// what was drawn hidden (a password some programs echo concealed) is
    /// blank.
    pub text: String,
    /// A full-screen program has the screen: the text is its picture, and
    /// there is no history above it.
    pub full_screen: bool,
}

/// The last `lines` rows of `term` that hold anything, from the history on
/// to the screen. The first line is taken whole when the terminal wrapped
/// it onto the rows counted, so the text may hold a few rows more.
pub fn recent_text<T: EventListener>(term: &Term<T>, lines: usize) -> TerminalText {
    let full_screen = term.mode().contains(TermMode::ALT_SCREEN);
    let grid = term.grid();
    let columns = grid.columns();
    let wraps = |line: i32| {
        grid[Line(line)][Column(columns - 1)]
            .flags
            .contains(Flags::WRAPLINE)
    };
    let has_text = |line: i32| !grid[Line(line)].is_clear();
    let top = term.topmost_line().0;
    let Some(last) = (top..=term.bottommost_line().0)
        .rev()
        .find(|line| has_text(*line))
    else {
        return TerminalText {
            text: String::new(),
            full_screen,
        };
    };
    let mut first = (top..=last)
        .find(|line| has_text(*line))
        .unwrap_or(last)
        .max(last + 1 - lines.max(1) as i32);
    while first > top && wraps(first - 1) {
        first -= 1;
    }
    let mut text = String::new();
    for line in first..=last {
        let row = &grid[Line(line)];
        for column in 0..row.line_length().0 {
            let cell = &row[Column(column)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            if cell.flags.contains(Flags::HIDDEN) {
                text.push(' ');
                continue;
            }
            text.push(cell.c);
            for character in cell.zerowidth().unwrap_or_default() {
                text.push(*character);
            }
        }
        if !wraps(line) {
            // A row holding a line's last character, cut of its blank end.
            let kept = text.trim_end_matches(' ').len();
            text.truncate(kept);
            text.push('\n');
        }
    }
    let kept = text.trim_end().len();
    text.truncate(kept);
    TerminalText { text, full_screen }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

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

    #[test]
    fn the_text_has_no_colours_and_wrapped_lines_whole() {
        let mut term = term(10, 4);
        feed(
            &mut term,
            b"$ ls\r\n\x1b[1;34msrc\x1b[0m  docs\r\n$ echo abcdefghijklmnop\r\nabcdefghijklmnop\r\n$ ",
        );

        let recent = recent_text(&term, 100);

        assert_eq!(
            recent.text,
            "$ ls\nsrc  docs\n$ echo abcdefghijklmnop\nabcdefghijklmnop\n$"
        );
        assert!(!recent.full_screen);
    }

    #[test]
    fn only_the_last_lines_are_taken_and_the_first_whole() {
        let mut term = term(10, 3);
        feed(&mut term, b"one\r\ntwo\r\nthree\r\n0123456789abcde\r\nlast");

        // The row before `last` ends a line wrapped from the row above it.
        assert_eq!(recent_text(&term, 2).text, "0123456789abcde\nlast");
        assert_eq!(recent_text(&term, 1).text, "last");
        assert_eq!(recent_text(&term, 4).text, "three\n0123456789abcde\nlast");
    }

    #[test]
    fn what_was_drawn_hidden_is_blank() {
        let mut term = term(20, 3);
        feed(&mut term, b"key: \x1b[8msecret\x1b[0m ok");

        assert_eq!(recent_text(&term, 10).text, "key:        ok");
    }

    #[test]
    fn a_full_screen_program_gives_its_picture() {
        let mut term = term(20, 3);
        feed(&mut term, b"$ top\r\n\x1b[?1049h\x1b[Hload 0.42");

        let recent = recent_text(&term, 100);

        assert_eq!(recent.text, "load 0.42");
        assert!(recent.full_screen);
    }

    #[test]
    fn an_empty_terminal_gives_nothing() {
        assert_eq!(recent_text(&term(20, 3), 10), TerminalText::default());
    }
}
