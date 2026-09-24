//! Find in a terminal: a literal, smart-case search over the scrollback and
//! the screen.
//!
//! Only the visible rows are searched while rendering, so highlighting costs
//! a few dozen rows a frame however long the scrollback is. The whole buffer
//! is scanned only when the query changes or the user steps to another match,
//! which is also when the 「3/12」 position is refreshed.

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use gpui_kit::SharedString;

/// How far a highlighted match may reach above the first visible row along a
/// wrapped line. Longer wrapped lines are cut there, as in Alacritty.
const MAX_WRAPPED_LINES: i32 = 100;

/// Which way to step from the focused match: `Up` towards older output,
/// `Down` towards newer output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    Up,
    Down,
}

/// How a cell takes part in the open search, weakest first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SearchMark {
    #[default]
    None,
    Match,
    Focused,
}

/// Where the focused match sits among all matches, as of the last full scan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchPosition {
    current: Option<usize>,
    total: usize,
}

impl SearchPosition {
    pub fn new(current: Option<usize>, total: usize) -> Self {
        Self { current, total }
    }

    /// The one-based index of the focused match, if one is focused.
    pub fn current(&self) -> Option<usize> {
        self.current.map(|index| index + 1)
    }

    pub fn total(&self) -> usize {
        self.total
    }

    /// 「3/12」, 「无结果」, or the bare count once new output has moved the
    /// focused match away.
    pub fn label(&self) -> SharedString {
        match (self.total, self.current()) {
            (0, _) => "无结果".into(),
            (total, Some(current)) => format!("{current}/{total}").into(),
            (total, None) => format!("{total} 项").into(),
        }
    }
}

/// The open search of one terminal.
pub struct TerminalSearch {
    query: String,
    /// `None` for an empty query, which matches nothing.
    regex: Option<RegexSearch>,
    focused: Option<Match>,
    /// The scrollback length when `focused` was located. Every line pushed
    /// into the scrollback since then moves the match up one line.
    history_size: usize,
    position: SearchPosition,
}

impl TerminalSearch {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            regex: None,
            focused: None,
            history_size: 0,
            position: SearchPosition::new(None, 0),
        }
    }

    /// `None` while the query is empty.
    pub fn position(&self) -> Option<SearchPosition> {
        self.regex.as_ref().map(|_| self.position)
    }

    /// Search for `query` and focus the first match on screen, so the view
    /// stays put when it can. With none on screen, focus the newest match
    /// above the viewport, scrolling to it.
    pub fn set_query<T: alacritty_terminal::event::EventListener>(
        &mut self,
        query: &str,
        term: &mut Term<T>,
    ) {
        self.query = query.to_string();
        self.regex = (!query.is_empty())
            .then(|| RegexSearch::new(&escape_literal(query)).ok())
            .flatten();
        self.focused = None;
        let Some(regex) = self.regex.as_mut() else {
            self.position = SearchPosition::new(None, 0);
            return;
        };
        let (top, bottom) = (viewport_top(term), viewport_bottom(term));
        let survey = survey(all_matches(term, regex), None, top, bottom);
        let total = survey.count;
        let on_screen = survey
            .first_from_top
            .filter(|(_, found)| *found.start() <= bottom);
        let current = on_screen.or(survey.last_to_bottom).or(survey.first);
        self.focus(term, current, total);
    }

    /// Move the focus one match up or down, wrapping at either end. Without a
    /// focused match, start from the edge of the viewport.
    pub fn step<T: alacritty_terminal::event::EventListener>(
        &mut self,
        direction: SearchDirection,
        term: &mut Term<T>,
    ) {
        self.sync(term);
        let Some(regex) = self.regex.as_mut() else {
            return;
        };
        let survey = survey(
            all_matches(term, regex),
            self.focused.as_ref(),
            viewport_top(term),
            viewport_bottom(term),
        );
        let total = survey.count;
        let current = survey.step(direction);
        self.focus(term, current, total);
    }

    /// Search again for the current query, after the buffer was rewritten.
    pub fn refresh<T: alacritty_terminal::event::EventListener>(&mut self, term: &mut Term<T>) {
        let query = std::mem::take(&mut self.query);
        self.set_query(&query, term);
    }

    /// Follow the focused match as output pushes lines into the scrollback,
    /// and drop it once the text there no longer matches.
    fn sync<T>(&mut self, term: &Term<T>) {
        let history_size = term.history_size();
        let (Some(focused), Some(regex)) = (self.focused.take(), self.regex.as_mut()) else {
            self.history_size = history_size;
            return;
        };
        let shift = history_size as i32 - self.history_size as i32;
        self.history_size = history_size;
        let moved = shifted(&focused, shift);
        let still_there = moved.start().line >= term.topmost_line()
            && term.regex_search_right(regex, *moved.start(), *moved.end()) == Some(moved.clone());
        if still_there {
            self.focused = Some(moved);
        } else {
            self.position = SearchPosition::new(None, self.position.total);
        }
    }

    fn focus<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &mut Term<T>,
        current: Option<Indexed>,
        total: usize,
    ) {
        self.history_size = term.history_size();
        let (index, focused) = current.unzip();
        self.focused = focused;
        self.position = SearchPosition::new(index, total);
        if let Some(focused) = &self.focused {
            term.scroll_to_point(*focused.start());
        }
    }

    /// The matches that touch the viewport, in order, and the focused one.
    pub fn visible<T>(&mut self, term: &Term<T>) -> (Vec<Match>, Option<Match>) {
        self.sync(term);
        let Some(regex) = self.regex.as_mut() else {
            return (Vec::new(), None);
        };
        (visible_matches(term, regex), self.focused.clone())
    }
}

impl Default for TerminalSearch {
    fn default() -> Self {
        Self::new()
    }
}

/// Tells, cell by cell in reading order, which cells of the display belong to
/// a match.
pub struct MatchMarker<'a> {
    matches: &'a [Match],
    focused: Option<&'a Match>,
    next: usize,
}

impl<'a> MatchMarker<'a> {
    /// `matches` must be in order, as the search returns them.
    pub fn new(matches: &'a [Match], focused: Option<&'a Match>) -> Self {
        Self {
            matches,
            focused,
            next: 0,
        }
    }

    /// The mark of the cell at `point`. Points must come in reading order.
    pub fn mark(&mut self, point: Point) -> SearchMark {
        while self.next < self.matches.len() && *self.matches[self.next].end() < point {
            self.next += 1;
        }
        if self.focused.is_some_and(|focused| focused.contains(&point)) {
            SearchMark::Focused
        } else if self
            .matches
            .get(self.next)
            .is_some_and(|found| found.contains(&point))
        {
            SearchMark::Match
        } else {
            SearchMark::None
        }
    }
}

/// Escape `query` so the search engine matches it literally.
pub fn escape_literal(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for character in query.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Every match in the scrollback and on the screen, oldest first. A lazy
/// iterator: a one-letter query can match millions of times, far too many to
/// collect.
fn all_matches<'a, T>(
    term: &'a Term<T>,
    regex: &'a mut RegexSearch,
) -> impl Iterator<Item = Match> + 'a {
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    RegexIter::new(start, end, Direction::Right, term, regex)
}

/// The matches that touch the viewport. The search reaches back along a line
/// wrapped across the top edge, so a match that starts above it still shows.
pub fn visible_matches<T>(term: &Term<T>, regex: &mut RegexSearch) -> Vec<Match> {
    let top = viewport_top(term);
    let start = term.line_search_left(top);
    let start = Point::new(start.line.max(top.line - MAX_WRAPPED_LINES), Column(0));
    let end = term.line_search_right(viewport_bottom(term));
    RegexIter::new(start, end, Direction::Right, term, regex).collect()
}

fn viewport_top<T>(term: &Term<T>) -> Point {
    Point::new(Line(-(term.grid().display_offset() as i32)), Column(0))
}

fn viewport_bottom<T>(term: &Term<T>) -> Point {
    let top = viewport_top(term);
    Point::new(
        top.line + (term.screen_lines() as i32 - 1),
        term.last_column(),
    )
}

fn shifted(found: &Match, lines: i32) -> Match {
    let mut start = *found.start();
    let mut end = *found.end();
    start.line -= lines;
    end.line -= lines;
    start..=end
}

/// A match with its zero-based index among all matches.
type Indexed = (usize, Match);

/// What one pass over the matches learns: enough to count them and to pick
/// the match to focus, without keeping them.
#[derive(Default)]
struct Survey {
    count: usize,
    first: Option<Indexed>,
    last: Option<Indexed>,
    /// The first match at or below the top of the viewport.
    first_from_top: Option<Indexed>,
    /// The last match at or above the bottom of the viewport.
    last_to_bottom: Option<Indexed>,
    /// The focused match's neighbours, when it was found.
    focused_found: bool,
    before_focused: Option<Indexed>,
    after_focused: Option<Indexed>,
}

impl Survey {
    /// The match one step away. From a focused match, move next to it and
    /// wrap at either end; without one, take the first match from the top of
    /// the viewport going down, or the last from its bottom going up.
    fn step(self, direction: SearchDirection) -> Option<Indexed> {
        match (self.focused_found, direction) {
            (true, SearchDirection::Down) => self.after_focused.or(self.first),
            (true, SearchDirection::Up) => self.before_focused.or(self.last),
            (false, SearchDirection::Down) => self.first_from_top.or(self.first),
            (false, SearchDirection::Up) => self.last_to_bottom.or(self.last),
        }
    }
}

fn survey(
    matches: impl Iterator<Item = Match>,
    focused: Option<&Match>,
    top: Point,
    bottom: Point,
) -> Survey {
    let mut survey = Survey::default();
    for (index, found) in matches.enumerate() {
        survey.count += 1;
        let indexed = || Some((index, found.clone()));
        if survey.first.is_none() {
            survey.first = indexed();
        }
        if survey.first_from_top.is_none() && *found.start() >= top {
            survey.first_from_top = indexed();
        }
        if *found.start() <= bottom {
            survey.last_to_bottom = indexed();
        }
        if Some(&found) == focused {
            survey.focused_found = true;
        } else if survey.focused_found {
            if survey.after_focused.is_none() {
                survey.after_focused = indexed();
            }
        } else if focused.is_some() {
            survey.before_focused = indexed();
        }
        survey.last = Some((index, found));
    }
    survey
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(line: i32, column: usize) -> Point {
        Point::new(Line(line), Column(column))
    }

    fn found(line: i32, start: usize, end: usize) -> Match {
        point(line, start)..=point(line, end)
    }

    #[test]
    fn literal_queries_escape_regex_syntax() {
        assert_eq!(escape_literal("a.b"), r"a\.b");
        assert_eq!(escape_literal("[x]*"), r"\[x\]\*");
        assert_eq!(escape_literal("日志"), "日志");
    }

    #[test]
    fn position_labels_read_as_the_bar_shows_them() {
        assert_eq!(SearchPosition::new(Some(2), 12).label(), "3/12");
        assert_eq!(SearchPosition::new(None, 0).label(), "无结果");
        assert_eq!(SearchPosition::new(None, 4).label(), "4 项");
    }

    fn step(
        matches: &[Match],
        focused: Option<usize>,
        direction: SearchDirection,
    ) -> Option<usize> {
        let (top, bottom) = (point(0, 0), point(4, 9));
        let focused = focused.map(|index| &matches[index]);
        survey(matches.iter().cloned(), focused, top, bottom)
            .step(direction)
            .map(|(index, _)| index)
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        let matches = [found(-3, 0, 1), found(0, 0, 1), found(2, 0, 1)];
        assert_eq!(step(&matches, Some(0), SearchDirection::Down), Some(1));
        assert_eq!(step(&matches, Some(2), SearchDirection::Down), Some(0));
        assert_eq!(step(&matches, Some(0), SearchDirection::Up), Some(2));
        assert_eq!(step(&matches, Some(1), SearchDirection::Up), Some(0));
    }

    #[test]
    fn stepping_without_a_focus_starts_at_the_viewport() {
        let matches = [found(-3, 0, 1), found(0, 0, 1), found(6, 0, 1)];
        assert_eq!(step(&matches, None, SearchDirection::Down), Some(1));
        assert_eq!(step(&matches, None, SearchDirection::Up), Some(1));
        assert_eq!(step(&[], None, SearchDirection::Up), None);
        let above = [found(-3, 0, 1)];
        assert_eq!(step(&above, None, SearchDirection::Down), Some(0));
    }

    #[test]
    fn marks_follow_display_order_and_prefer_the_focus() {
        let matches = [found(0, 1, 2), found(0, 4, 4), found(1, 0, 0)];
        let mut marker = MatchMarker::new(&matches, Some(&matches[1]));
        let marks: Vec<_> = (0..2)
            .flat_map(|line| (0..5).map(move |column| point(line, column)))
            .map(|point| marker.mark(point))
            .collect();
        use SearchMark::{Focused as F, Match as M, None as N};
        assert_eq!(marks, [N, M, M, N, F, M, N, N, N, N]);
    }
}
