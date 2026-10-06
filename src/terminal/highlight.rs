//! 关键字高亮: rules that color what matches them in every terminal, and may
//! tell the user when new output matches while they look elsewhere.
//!
//! Only what is shown changes. A rule matches the text of the grid, what the
//! emulator made of the output with escape sequences and carriage returns
//! already applied, never the bytes: it cannot break a control sequence, and
//! the server's output and copied text stay as they were.
//!
//! The rows in view are matched again every frame, as find and links are, so
//! nothing has to follow scrolling, a resize's reflow or new output. The
//! rows a line wraps over are joined into one logical line first, so a word
//! wrapped onto the next row is still one match, before and after a resize.
//!
//! The `regex` crate matches, not Alacritty's grid search that find and
//! links use: its DFA cannot build `\b`, finds `^` only on the first line it
//! looks at, and sees the blanks that pad a row before `$`.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, Timeout as _};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Global, Hsla};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use super::search::MAX_WRAPPED_LINES;

/// The longest pattern a rule may have, in characters.
pub const PATTERN_LIMIT: usize = 500;

/// How long a line already told of stays quiet when it shows up again, the
/// way a program that redraws its lines (apt, docker pull) shows it.
const RETELL_AFTER: Duration = Duration::from_secs(300);

/// How many lines told of are remembered.
const TOLD_LIMIT: usize = 64;

/// One rule: what to match, how to show it, and whether to tell of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HighlightRule {
    pub pattern: String,
    pub kind: PatternKind,
    pub color: HighlightColor,
    pub bold: bool,
    /// Tell the user when a finished line of new output matches, while they
    /// cannot see the terminal.
    pub notify: bool,
}

/// How a rule's pattern reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// Literally, in upper and lower case alike unless it has a capital, as
    /// find has it.
    #[default]
    Keyword,
    /// As a regular expression, case and all; `(?i)` ignores case.
    Regex,
}

/// The colors a rule may show its matches in: the theme's, so they read on
/// a light background and a dark one alike, as the links' blue does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightColor {
    #[default]
    Red,
    Yellow,
    Green,
    Cyan,
    Blue,
    Magenta,
}

impl HighlightColor {
    /// Every color, in the order the dialog offers them.
    pub const ALL: [Self; 6] = [
        Self::Red,
        Self::Yellow,
        Self::Green,
        Self::Cyan,
        Self::Blue,
        Self::Magenta,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Red => "红",
            Self::Yellow => "黄",
            Self::Green => "绿",
            Self::Cyan => "青",
            Self::Blue => "蓝",
            Self::Magenta => "紫",
        }
    }

    pub fn hsla(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Red => theme.red,
            Self::Yellow => theme.yellow,
            Self::Green => theme.green,
            Self::Cyan => theme.cyan,
            Self::Blue => theme.blue,
            Self::Magenta => theme.magenta,
        }
    }
}

/// How a match shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightStyle {
    pub color: HighlightColor,
    pub bold: bool,
}

/// The rules a new installation starts with, none of them notifying.
pub fn default_rules() -> Vec<HighlightRule> {
    vec![
        HighlightRule {
            pattern: "ERROR".into(),
            kind: PatternKind::Keyword,
            color: HighlightColor::Red,
            bold: true,
            notify: false,
        },
        HighlightRule {
            pattern: "WARN".into(),
            kind: PatternKind::Keyword,
            color: HighlightColor::Yellow,
            bold: false,
            notify: false,
        },
        HighlightRule {
            pattern: r"\b\d{1,3}(\.\d{1,3}){3}\b".into(),
            kind: PatternKind::Regex,
            color: HighlightColor::Blue,
            bold: false,
            notify: false,
        },
    ]
}

impl HighlightRule {
    pub fn style(&self) -> HighlightStyle {
        HighlightStyle {
            color: self.color,
            bold: self.bold,
        }
    }

    /// The pattern, ready to match; or why it cannot be, as the dialog says
    /// it.
    pub fn compile(&self) -> Result<Regex, &'static str> {
        if self.pattern.trim().is_empty() {
            return Err("请输入要匹配的内容");
        }
        if self.pattern.chars().count() > PATTERN_LIMIT {
            return Err("内容不能超过 500 个字符");
        }
        let built = match self.kind {
            PatternKind::Keyword => RegexBuilder::new(&regex::escape(&self.pattern))
                .case_insensitive(!self.pattern.chars().any(char::is_uppercase))
                .build(),
            PatternKind::Regex => Regex::new(&self.pattern),
        };
        built.map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => "正则表达式过于复杂",
            _ => "正则表达式写法有误",
        })
    }
}

/// Whether `regex` finds some text in `text`. A match of nothing, as `\b`
/// or `x*` make, colors nothing and tells of nothing.
fn finds_text(regex: &Regex, text: &str) -> bool {
    regex.find_iter(text).any(|found| !found.is_empty())
}

/// The rules, compiled once for every terminal. A rule that does not compile
/// (only a hand-edited settings file has one) is left out of matching.
#[derive(Debug, Default)]
pub struct HighlightSet {
    rules: Vec<HighlightRule>,
    compiled: Vec<Result<Regex, &'static str>>,
}

// Shared by the UI thread and every terminal's parser thread.
const _: fn() = || {
    fn shared<T: Send + Sync>() {}
    shared::<HighlightSet>();
};

impl HighlightSet {
    pub fn new(rules: Vec<HighlightRule>) -> Self {
        let compiled = rules.iter().map(HighlightRule::compile).collect();
        Self { rules, compiled }
    }

    /// The rules this set was compiled from, in their order.
    pub fn rules(&self) -> &[HighlightRule] {
        &self.rules
    }

    /// Why the rule at `ix` matches nothing, when it does not compile.
    pub fn error(&self, ix: usize) -> Option<&'static str> {
        self.compiled.get(ix)?.as_ref().err().copied()
    }

    /// Whether any rule tells of the lines it matches.
    pub fn notifies(&self) -> bool {
        self.notifying().next().is_some()
    }

    /// The rules that compiled, by their index.
    fn active(&self) -> impl DoubleEndedIterator<Item = (usize, &HighlightRule, &Regex)> {
        self.rules
            .iter()
            .zip(&self.compiled)
            .enumerate()
            .filter_map(|(ix, (rule, compiled))| Some((ix, rule, compiled.as_ref().ok()?)))
    }

    fn notifying(&self) -> impl Iterator<Item = (usize, &HighlightRule, &Regex)> {
        self.active().filter(|(_, rule, _)| rule.notify)
    }

    /// How each cell in view shows. Nothing while a full-screen program has
    /// the alternate screen: it redraws itself, and an editor's `error`
    /// identifier is not news.
    pub fn screen<T>(&self, term: &Term<T>) -> ScreenHighlights {
        let columns = term.columns();
        let rows = term.screen_lines();
        let mut screen = ScreenHighlights {
            columns,
            styles: Vec::new(),
        };
        if self.active().next().is_none() || term.mode().contains(TermMode::ALT_SCREEN) {
            return screen;
        }
        screen.styles = vec![None; columns * rows];
        let top = Line(-(term.grid().display_offset() as i32));
        let bottom = Line(top.0 + rows as i32 - 1);
        // From where the line the top row is part of starts, so a match that
        // starts above the view shows its part in it.
        let mut line = line_start(term, top);
        while line <= bottom {
            let end = line_end(term, line);
            let text = LineText::read(term, line, end);
            // The last rule first, so an earlier one paints over it where
            // they overlap.
            for (_, rule, regex) in self.active().rev() {
                for found in regex.find_iter(&text.text) {
                    screen.mark(&text, found.range(), rule.style(), top);
                }
            }
            line = Line(end.0 + 1);
        }
        screen
    }
}

/// The application's rules, which `settings::apply` sets. The terminal
/// module reads them here rather than depending on settings.
#[derive(Clone, Default)]
pub struct TerminalHighlights(Arc<HighlightSet>);

impl Global for TerminalHighlights {}

impl TerminalHighlights {
    pub fn new(rules: Vec<HighlightRule>) -> Self {
        Self(Arc::new(HighlightSet::new(rules)))
    }

    /// The rules in effect; none before settings are applied.
    pub fn current(cx: &App) -> Arc<HighlightSet> {
        cx.try_global::<Self>()
            .map(|highlights| highlights.0.clone())
            .unwrap_or_default()
    }

    pub fn set(&self) -> &Arc<HighlightSet> {
        &self.0
    }
}

/// The rules a terminal's parser thread checks new lines against, swapped
/// by the engine when they change.
pub(super) type SharedHighlights = Arc<Mutex<Arc<HighlightSet>>>;

pub(super) fn current(shared: &SharedHighlights) -> Arc<HighlightSet> {
    shared
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// How each cell in view shows, by its row on screen and column.
pub struct ScreenHighlights {
    columns: usize,
    /// Row by row; empty when nothing is highlighted.
    styles: Vec<Option<HighlightStyle>>,
}

impl ScreenHighlights {
    pub fn style(&self, row: usize, column: usize) -> Option<HighlightStyle> {
        self.styles
            .get(row * self.columns + column)
            .copied()
            .flatten()
    }

    /// Show the characters of `text` in the byte range `found` with
    /// `style`, those in view: the view's top row is `top`.
    fn mark(&mut self, text: &LineText, found: Range<usize>, style: HighlightStyle, top: Line) {
        let first = text.cells.partition_point(|cell| cell.offset < found.start);
        let last = text.cells.partition_point(|cell| cell.offset < found.end);
        for cell in &text.cells[first..last] {
            let width = if cell.wide { 2 } else { 1 };
            for column in cell.point.column.0..cell.point.column.0 + width {
                let row = cell.point.line.0 - top.0;
                if row < 0 || column >= self.columns {
                    continue;
                }
                if let Some(slot) = self.styles.get_mut(row as usize * self.columns + column) {
                    *slot = Some(style);
                }
            }
        }
    }
}

/// A character of a logical line: where it starts in the text, the cell it
/// is in, and whether it takes that cell's neighbour too.
struct LineCell {
    offset: usize,
    point: Point,
    wide: bool,
}

/// One logical line of the grid, the rows it wraps over joined, as text.
struct LineText {
    text: String,
    /// Each character's cell, in order.
    cells: Vec<LineCell>,
}

impl LineText {
    /// The text of rows `first..=last`, without the blanks that pad the
    /// last one. A wide character's spacer is no character; a concealed one
    /// reads as a blank, so it neither matches nor shows in a notification.
    fn read<T>(term: &Term<T>, first: Line, last: Line) -> Self {
        let mut text = String::new();
        let mut cells = Vec::new();
        for line in first.0..=last.0 {
            let row = &term.grid()[Line(line)];
            for column in 0..row.len() {
                let cell = &row[Column(column)];
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                cells.push(LineCell {
                    offset: text.len(),
                    point: Point::new(Line(line), Column(column)),
                    wide: cell.flags.contains(Flags::WIDE_CHAR),
                });
                if cell.flags.contains(Flags::HIDDEN) {
                    text.push(' ');
                    continue;
                }
                text.push(cell.c);
                if let Some(zerowidth) = cell.zerowidth() {
                    text.extend(zerowidth);
                }
            }
        }
        let end = text.trim_end_matches(' ').len();
        text.truncate(end);
        while cells.last().is_some_and(|cell| cell.offset >= end) {
            cells.pop();
        }
        Self { text, cells }
    }
}

/// Whether the row `line` goes on in the next one.
fn wraps<T>(term: &Term<T>, line: Line) -> bool {
    let row = &term.grid()[line];
    row.len() > 0 && row[Column(row.len() - 1)].flags.contains(Flags::WRAPLINE)
}

/// The first row of the logical line that `line` is part of, at most
/// `MAX_WRAPPED_LINES` above it.
fn line_start<T>(term: &Term<T>, line: Line) -> Line {
    let top = term.topmost_line();
    let mut start = line;
    while start > top && line.0 - start.0 < MAX_WRAPPED_LINES && wraps(term, Line(start.0 - 1)) {
        start = Line(start.0 - 1);
    }
    start
}

/// The last row of the logical line that `line` is part of, at most
/// `MAX_WRAPPED_LINES` below it.
fn line_end<T>(term: &Term<T>, line: Line) -> Line {
    let bottom = term.bottommost_line();
    let mut end = line;
    while end < bottom && end.0 - line.0 < MAX_WRAPPED_LINES && wraps(term, end) {
        end = Line(end.0 + 1);
    }
    end
}

/// The text of the logical line the cursor is on.
fn cursor_line<T>(term: &Term<T>) -> String {
    let line = term.grid().cursor.point.line;
    LineText::read(term, line_start(term, line), line_end(term, line)).text
}

/// Feed `bytes` to the emulator. With `watching`, also take the text of
/// every line a line feed finishes, just before it does: the cursor is then
/// still on that line, whose text is final, carriage returns and colors
/// applied. Not on the alternate screen, whose programs redraw rather than
/// print, nor while a synchronized update holds its output back, as the
/// grid then still shows what came before.
pub(super) fn advance_watching<T: EventListener>(
    processor: &mut Processor,
    term: &mut Term<T>,
    bytes: &[u8],
    watching: bool,
) -> Vec<String> {
    if !watching {
        processor.advance(term, bytes);
        return Vec::new();
    }
    let mut finished = Vec::new();
    let mut rest = bytes;
    while let Some(newline) = rest.iter().position(|byte| *byte == b'\n') {
        processor.advance(term, &rest[..newline]);
        if !term.mode().contains(TermMode::ALT_SCREEN)
            && !processor.sync_timeout().pending_timeout()
        {
            finished.push(cursor_line(term));
        }
        // The line feed starts the next part.
        rest = &rest[newline..];
        processor.advance(term, &rest[..1]);
        rest = &rest[1..];
    }
    processor.advance(term, rest);
    finished
}

/// A finished line of new output that a notifying rule matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeywordHit {
    pub pattern: String,
    pub line: String,
}

/// Checks the lines a terminal finishes against the notifying rules, on its
/// parser thread.
pub(super) struct LineWatcher {
    set: Arc<HighlightSet>,
    /// The lines told of lately, by rule, and when.
    told: VecDeque<(usize, String, Instant)>,
}

impl LineWatcher {
    pub fn new(set: Arc<HighlightSet>) -> Self {
        Self {
            set,
            told: VecDeque::new(),
        }
    }

    /// Follow the rules as they change. What was told under the old ones is
    /// forgotten.
    pub fn sync(&mut self, set: Arc<HighlightSet>) {
        if !Arc::ptr_eq(&self.set, &set) {
            self.set = set;
            self.told.clear();
        }
    }

    pub fn is_watching(&self) -> bool {
        self.set.notifies()
    }

    /// The first of `lines` a notifying rule matches, unless that rule told
    /// of the same line in the last five minutes. One at most: a burst of
    /// output is one notice.
    pub fn check(&mut self, lines: Vec<String>, now: Instant) -> Option<KeywordHit> {
        self.told
            .retain(|(_, _, at)| now.saturating_duration_since(*at) < RETELL_AFTER);
        for line in lines {
            for (ix, rule, regex) in self.set.notifying() {
                if !finds_text(regex, &line)
                    || self
                        .told
                        .iter()
                        .any(|(told, text, _)| *told == ix && *text == line)
                {
                    continue;
                }
                if self.told.len() == TOLD_LIMIT {
                    self.told.pop_front();
                }
                self.told.push_back((ix, line.clone(), now));
                return Some(KeywordHit {
                    pattern: rule.pattern.clone(),
                    line,
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::term::Config;

    use super::*;
    use crate::terminal::TerminalSize;

    fn keyword(pattern: &str, color: HighlightColor) -> HighlightRule {
        HighlightRule {
            pattern: pattern.into(),
            color,
            ..HighlightRule::default()
        }
    }

    fn regex(pattern: &str, color: HighlightColor) -> HighlightRule {
        HighlightRule {
            kind: PatternKind::Regex,
            ..keyword(pattern, color)
        }
    }

    fn notifying(rule: HighlightRule) -> HighlightRule {
        HighlightRule {
            notify: true,
            ..rule
        }
    }

    /// A terminal fed by one parser, as a terminal's parser thread feeds it.
    struct Screen {
        term: Term<VoidListener>,
        processor: Processor,
    }

    impl Screen {
        fn new(columns: usize, rows: usize) -> Self {
            Self {
                term: Term::new(
                    Config::default(),
                    &TerminalSize::new(columns, rows, 8, 16),
                    VoidListener,
                ),
                processor: Processor::new(),
            }
        }

        fn feed(&mut self, text: &str) {
            self.processor.advance(&mut self.term, text.as_bytes());
        }

        /// The lines finished while feeding `text`, as the parser thread
        /// collects them.
        fn feed_watching(&mut self, text: &str) -> Vec<String> {
            advance_watching(&mut self.processor, &mut self.term, text.as_bytes(), true)
        }

        fn resize(&mut self, columns: usize, rows: usize) {
            self.term.resize(TerminalSize::new(columns, rows, 8, 16));
        }

        /// The highlighted runs on screen in reading order, with their
        /// color. A run goes on from the end of one row to the start of the
        /// next, as a wrapped match does.
        fn marks(&self, rules: Vec<HighlightRule>) -> Vec<(String, HighlightColor)> {
            let screen = HighlightSet::new(rules).screen(&self.term);
            let offset = self.term.grid().display_offset() as i32;
            let mut runs: Vec<(String, HighlightColor)> = Vec::new();
            let mut open = false;
            for row in 0..self.term.screen_lines() {
                let line = &self.term.grid()[Line(row as i32 - offset)];
                for column in 0..self.term.columns() {
                    let cell = &line[Column(column)];
                    match screen.style(row, column) {
                        Some(style) => {
                            if !open || runs.last().is_some_and(|(_, color)| *color != style.color)
                            {
                                runs.push((String::new(), style.color));
                            }
                            open = true;
                            if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                                runs.last_mut().unwrap().0.push(cell.c);
                            }
                        }
                        None => open = false,
                    }
                }
            }
            runs
        }
    }

    fn runs(marks: &[(&str, HighlightColor)]) -> Vec<(String, HighlightColor)> {
        marks
            .iter()
            .map(|(text, color)| (text.to_string(), *color))
            .collect()
    }

    use HighlightColor::{Blue, Red, Yellow};

    #[test]
    fn keywords_are_literal_and_smart_case_regexes_are_as_written() {
        let compile = |rule: HighlightRule| rule.compile().unwrap();
        let dotted = compile(keyword("a.b", Red));
        assert!(dotted.is_match("x a.b y") && !dotted.is_match("axb"));
        assert!(compile(regex("a.b", Red)).is_match("axb"));
        // A keyword in lower case finds any case; with a capital, its own.
        assert!(compile(keyword("error", Red)).is_match("ERROR: x"));
        assert!(!compile(keyword("ERROR", Red)).is_match("error: x"));
        assert!(!compile(regex("error", Red)).is_match("ERROR"));
        assert!(compile(regex("(?i)error", Red)).is_match("ERROR"));
        let address = compile(default_rules().remove(2));
        assert!(address.is_match("from 10.0.0.12 port 22"));
        assert!(!address.is_match("1234.5.6.7"));
    }

    #[test]
    fn a_rule_that_cannot_match_says_why() {
        assert_eq!(
            keyword(" ", Red).compile().err(),
            Some("请输入要匹配的内容")
        );
        assert_eq!(
            regex("(ERROR", Red).compile().err(),
            Some("正则表达式写法有误")
        );
        assert_eq!(
            regex(r"(\w{1000}){1000}", Red).compile().err(),
            Some("正则表达式过于复杂")
        );
        let long = "x".repeat(PATTERN_LIMIT + 1);
        assert_eq!(
            keyword(&long, Red).compile().err(),
            Some("内容不能超过 500 个字符")
        );
        let set = HighlightSet::new(vec![regex("(", Red), keyword("ok", Red)]);
        assert_eq!(set.error(0), Some("正则表达式写法有误"));
        assert_eq!(set.error(1), None);
    }

    #[test]
    fn matches_are_marked_where_they_show() {
        let mut screen = Screen::new(30, 4);
        screen.feed("ok ERROR x WARN\r\n");
        assert_eq!(
            screen.marks(vec![keyword("ERROR", Red), keyword("WARN", Yellow)]),
            runs(&[("ERROR", Red), ("WARN", Yellow)])
        );
        // A word split between two colors is one word on screen.
        screen.feed("E\x1b[31mRR\x1b[0mOR\r\n");
        assert_eq!(
            screen.marks(vec![keyword("ERROR", Red)]),
            runs(&[("ERROR", Red), ("ERROR", Red)])
        );
    }

    #[test]
    fn line_anchors_and_word_boundaries_hold_on_every_line() {
        let mut screen = Screen::new(30, 5);
        screen.feed("a ERROR\r\nERROR b\r\n2024-10 ERROR\r\n");
        assert_eq!(
            screen.marks(vec![regex("^ERROR", Red)]),
            runs(&[("ERROR", Red)])
        );
        // The blanks padding a row are not text before `$`.
        assert_eq!(
            screen.marks(vec![regex("ERROR$", Red)]),
            runs(&[("ERROR", Red), ("ERROR", Red)])
        );
        assert_eq!(
            screen.marks(vec![regex(r"^\d+", Blue)]),
            runs(&[("2024", Blue)])
        );
        assert_eq!(
            screen.marks(vec![regex(r"\bRR", Red)]),
            Vec::<(String, HighlightColor)>::new()
        );
    }

    #[test]
    fn a_wrapped_match_is_marked_on_both_rows_before_and_after_a_resize() {
        // `1234567ERR` and `OR tail`.
        let mut screen = Screen::new(10, 6);
        screen.feed("1234567ERROR tail\r\n");
        let rules = || vec![keyword("ERROR", Red)];
        assert_eq!(screen.marks(rules()), runs(&[("ERROR", Red)]));
        // Narrowed, Alacritty reflows them to `1234567E`, pushed into the
        // scrollback, and `RROR tai` on the screen's top row.
        screen.resize(8, 6);
        assert_eq!(screen.marks(rules()), runs(&[("RROR", Red)]));
        screen.term.scroll_display(Scroll::Delta(1));
        assert_eq!(screen.marks(rules()), runs(&[("ERROR", Red)]));
        // Widened, one row again.
        screen.term.scroll_display(Scroll::Bottom);
        screen.resize(40, 6);
        assert_eq!(screen.marks(rules()), runs(&[("ERROR", Red)]));
    }

    #[test]
    fn a_match_starting_above_the_view_shows_its_part_in_it() {
        // `xxxxE` and `RROR` in the scrollback, `next` and `last` on screen.
        let mut screen = Screen::new(5, 2);
        screen.feed("xxxxERROR\r\nnext\r\nlast");
        assert!(screen.marks(vec![keyword("ERROR", Red)]).is_empty());
        // Scrolled back a row, the view starts at `RROR`.
        screen.term.scroll_display(Scroll::Delta(1));
        assert_eq!(
            screen.marks(vec![keyword("ERROR", Red)]),
            runs(&[("RROR", Red)])
        );
    }

    #[test]
    fn wide_characters_take_both_cells() {
        let mut screen = Screen::new(20, 2);
        screen.feed("连接错误 x");
        assert_eq!(
            screen.marks(vec![keyword("错误", Red)]),
            runs(&[("错误", Red)])
        );
    }

    #[test]
    fn the_first_rule_wins_where_rules_overlap() {
        let mut screen = Screen::new(30, 2);
        screen.feed("OutOfMemoryError");
        assert_eq!(
            screen.marks(vec![keyword("Memory", Blue), regex(r"\w+Error", Red)]),
            runs(&[("OutOf", Red), ("Memory", Blue), ("Error", Red)])
        );
    }

    #[test]
    fn nothing_is_marked_on_the_alternate_screen() {
        let mut screen = Screen::new(20, 3);
        screen.feed("\x1b[?1049hERROR");
        assert!(screen.marks(vec![keyword("ERROR", Red)]).is_empty());
        screen.feed("\x1b[?1049l");
        screen.feed("ERROR");
        assert_eq!(
            screen.marks(vec![keyword("ERROR", Red)]),
            runs(&[("ERROR", Red)])
        );
    }

    #[test]
    fn a_line_is_taken_when_its_line_feed_comes() {
        let mut screen = Screen::new(40, 4);
        assert!(screen.feed_watching("prompt$ ERR").is_empty());
        assert_eq!(screen.feed_watching("OR boom\r\n"), ["prompt$ ERROR boom"]);
        // Overwritten by a carriage return, the first text is gone.
        assert_eq!(screen.feed_watching("ERROR\rok   \r\n"), ["ok"]);
        assert_eq!(screen.feed_watching("a\nb\n"), ["a", " b"]);
    }

    #[test]
    fn a_wrapped_line_is_taken_whole() {
        let mut screen = Screen::new(10, 4);
        assert_eq!(
            screen.feed_watching("0123456789ERROR\r\n"),
            ["0123456789ERROR"]
        );
    }

    #[test]
    fn full_screen_programs_and_synchronized_updates_are_not_watched() {
        let mut screen = Screen::new(20, 4);
        assert!(screen.feed_watching("\x1b[?1049hERROR\r\n").is_empty());
        assert_eq!(
            screen.feed_watching("\x1b[?1049lback\r\n").len(),
            1,
            "the main screen is watched again"
        );
        assert!(screen.feed_watching("\x1b[?2026hERROR\r\n").is_empty());
    }

    #[test]
    fn a_notifying_rule_tells_of_a_line_once_in_a_while() {
        let set = Arc::new(HighlightSet::new(vec![
            keyword("WARN", Yellow),
            notifying(keyword("ERROR", Red)),
        ]));
        let mut watcher = LineWatcher::new(set);
        assert!(watcher.is_watching());
        let start = Instant::now();
        let lines = |lines: &[&str]| lines.iter().map(|line| line.to_string()).collect();
        assert_eq!(watcher.check(lines(&["WARN low disk"]), start), None);
        assert_eq!(
            watcher.check(lines(&["ok", "1 ERROR a", "2 ERROR b"]), start),
            Some(KeywordHit {
                pattern: "ERROR".into(),
                line: "1 ERROR a".into()
            })
        );
        // Redrawn, the same line is not news; another one is.
        assert_eq!(watcher.check(lines(&["1 ERROR a"]), start), None);
        assert!(watcher.check(lines(&["2 ERROR b"]), start).is_some());
        let later = start + RETELL_AFTER;
        assert!(watcher.check(lines(&["1 ERROR a"]), later).is_some());
        // New rules start afresh.
        watcher.sync(Arc::new(HighlightSet::new(vec![notifying(keyword(
            "ERROR", Red,
        ))])));
        assert!(watcher.check(lines(&["1 ERROR a"]), later).is_some());
        watcher.sync(Arc::new(HighlightSet::new(vec![keyword("ERROR", Red)])));
        assert!(!watcher.is_watching());
    }

    #[test]
    fn a_match_of_nothing_tells_of_nothing() {
        let set = Arc::new(HighlightSet::new(vec![notifying(regex(r"\b", Red))]));
        let mut watcher = LineWatcher::new(set);
        assert_eq!(
            watcher.check(vec!["any words".into()], Instant::now()),
            None
        );
    }
}
