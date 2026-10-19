//! Retain unchanged terminal rows. Alacritty's damage is consumed only here,
//! under the same lock as the grid; cursor-only frames do not rescan links or
//! highlights. Content changes rescan only affected complete logical lines for
//! URLs and keyword colors. Selection changes reuse those detection results.

use std::rc::Rc;
use std::sync::OnceLock;

use alacritty_terminal::grid::{Dimensions, Indexed};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::search::{Match, RegexSearch};
use alacritty_terminal::term::{Term, TermDamage};
use alacritty_terminal::vte::ansi::CursorShape;
use gpui_kit::SharedString;

use super::engine::normalize_wide_character_selection;
use super::highlight::{HighlightColor, HighlightSet, ScreenHighlights};
use super::links::{LinkMarker, links_in_rows, visible_links};
use super::search::{MatchMarker, SearchMark, TerminalSearch};
use super::{TerminalCell, TerminalSnapshot};

pub(super) fn cache_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SHELLRS_DISABLE_TERMINAL_CACHE").is_none())
}

#[derive(Default)]
pub(super) struct SnapshotCache {
    raw: Vec<Vec<Cell>>,
    lines: Vec<Rc<[TerminalCell]>>,
    links: Rc<[SharedString]>,
    detected_links: Vec<(Match, SharedString)>,
    highlights: ScreenHighlights,
    search_matches: Vec<Match>,
    focused_match: Option<Match>,
    selection: Option<SelectionRange>,
    selection_cursor: Option<(Point, CursorShape)>,
    generation: u64,
    display_offset: usize,
    invalid: bool,
}

#[derive(Clone, Copy, PartialEq)]
struct Decoration {
    selected: bool,
    search: SearchMark,
    link: Option<usize>,
    highlight: Option<HighlightColor>,
}

impl Decoration {
    fn matches(self, cell: &TerminalCell) -> bool {
        self.selected == cell.selected
            && self.search == cell.search
            && self.link == cell.link
            && self.highlight == cell.highlight
    }
}

impl SnapshotCache {
    pub(super) fn invalidate(&mut self) {
        self.invalid = true;
    }

    pub(super) fn read<T: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &mut Term<T>,
        search: &mut Option<TerminalSearch>,
        url_search: &mut RegexSearch,
        highlights: &HighlightSet,
        generation: u64,
    ) -> TerminalSnapshot {
        let columns = term.columns();
        let rows = term.screen_lines();
        let offset = term.grid().display_offset();
        let reset = !cache_enabled()
            || self.invalid
            || self.generation != generation
            || self.raw.len() != rows
            || self.raw.first().map(Vec::len) != Some(columns)
            || self.display_offset != offset;
        let mut damaged = vec![reset; rows];
        let mut full_damage = false;
        match term.damage() {
            TermDamage::Full => {
                damaged.fill(true);
                full_damage = true;
            }
            TermDamage::Partial(lines) => {
                for line in lines {
                    if let Some(dirty) = damaged.get_mut(line.line) {
                        *dirty = true;
                    }
                }
            }
        }
        let old_wraps: Vec<_> = self.raw.iter().map(|row| wraps(row)).collect();
        self.raw.resize_with(rows, Vec::new);
        self.lines.resize_with(rows, || Rc::from([]));
        let mut changed = vec![false; rows];
        for row in 0..rows {
            if !damaged[row] {
                continue;
            }
            let raw = &term.grid()[Line(row as i32 - offset as i32)];
            if reset || !raw.into_iter().eq(self.raw[row].iter()) {
                self.raw[row] = raw.into_iter().cloned().collect();
                changed[row] = true;
            }
        }
        let content = term.renderable_content();
        let selection = content.selection;
        let point = content.cursor.point;
        let shape = content.cursor.shape;
        let selection_cursor = selection.map(|_| (point, shape));
        let content_changed = reset || full_damage || changed.iter().any(|changed| *changed);
        if content_changed {
            if reset || full_damage {
                changed.fill(true);
                self.detected_links = visible_links(term, url_search);
                self.highlights = highlights.screen(term);
            } else {
                expand_changed_lines(&mut changed, &old_wraps, &self.raw);
                for range in changed_ranges(&changed) {
                    let first = Line(range.start as i32 - offset as i32);
                    let last = Line((range.end - 1) as i32 - offset as i32);
                    self.detected_links
                        .retain(|(found, _)| found.end().line < first || found.start().line > last);
                    self.detected_links.extend(links_in_rows(
                        term,
                        url_search,
                        range.start,
                        range.end - 1,
                    ));
                }
                self.detected_links.sort_by_key(|(found, _)| *found.start());
                self.detected_links.dedup();
                highlights.update_screen(term, &mut self.highlights, &changed);
            }
            (self.search_matches, self.focused_match) = search
                .as_mut()
                .map(|search| search.visible(term))
                .unwrap_or_default();
        }
        if content_changed
            || self.selection != selection
            || self.selection_cursor != selection_cursor
        {
            let mut marker = MatchMarker::new(&self.search_matches, self.focused_match.as_ref());
            let mut links = LinkMarker::new(self.detected_links.clone());
            for (row, raw) in self.raw.iter().enumerate() {
                let decorations: Vec<_> = raw
                    .iter()
                    .enumerate()
                    .map(|(col, cell)| {
                        let indexed = Indexed {
                            point: Point::new(Line(row as i32 - offset as i32), Column(col)),
                            cell,
                        };
                        Decoration {
                            selected: selection.is_some_and(|selection| {
                                selection.contains_cell(&indexed, point, shape)
                            }),
                            search: marker.mark(indexed.point),
                            link: links.mark(indexed.point, cell.hyperlink()),
                            highlight: self.highlights.color(row, col),
                        }
                    })
                    .collect();
                let previous = &self.lines[row];
                if !changed[row]
                    && previous.len() == columns
                    && decorations
                        .iter()
                        .zip(previous.iter())
                        .all(|(decoration, cell)| decoration.matches(cell))
                {
                    continue;
                }
                let mut cells: Vec<_> = raw
                    .iter()
                    .zip(decorations)
                    .map(|(cell, decoration)| {
                        let mut character = cell.c.to_string();
                        if let Some(zerowidth) = cell.zerowidth() {
                            character.extend(zerowidth);
                        }
                        TerminalCell {
                            character,
                            foreground: cell.fg,
                            background: cell.bg,
                            flags: cell.flags,
                            selected: decoration.selected,
                            search: decoration.search,
                            link: decoration.link,
                            highlight: decoration.highlight,
                        }
                    })
                    .collect();
                normalize_wide_character_selection(&mut cells, columns);
                if cells.as_slice() != previous.as_ref() {
                    self.lines[row] = cells.into();
                }
            }
            let links = links.into_links();
            if links.as_slice() != self.links.as_ref() {
                self.links = links.into();
            }
        }
        let cursor_row = point.line.0 + offset as i32;
        let snapshot = TerminalSnapshot {
            columns,
            rows,
            lines: self.lines.clone(),
            cursor_row: (0..rows as i32)
                .contains(&cursor_row)
                .then_some(cursor_row as usize),
            cursor_column: point.column.0,
            cursor_shape: shape,
            cursor_blinking: term.cursor_style().blinking,
            display_offset: offset,
            history_size: term.grid().history_size(),
            colors: std::array::from_fn(|index| content.colors[index]),
            links: self.links.clone(),
        };
        self.generation = generation;
        self.display_offset = offset;
        self.selection = selection;
        self.selection_cursor = selection_cursor;
        self.invalid = false;
        term.reset_damage();
        snapshot
    }
}

fn wraps(row: &[Cell]) -> bool {
    row.last()
        .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE))
}

// The union of old and new wrapping boundaries also invalidates a logical
// line that was split or joined by an edit, including wide-char wrapping.
fn expand_changed_lines(changed: &mut [bool], old: &[bool], raw: &[Vec<Cell>]) {
    let mut first = 0;
    while first < changed.len() {
        let mut end = first + 1;
        while end < changed.len()
            && (old.get(end - 1).copied().unwrap_or(false) || wraps(&raw[end - 1]))
        {
            end += 1;
        }
        if changed[first..end].iter().any(|changed| *changed) {
            changed[first..end].fill(true);
        }
        first = end;
    }
}

fn changed_ranges(changed: &[bool]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut first = 0;
    while first < changed.len() {
        if !changed[first] {
            first += 1;
            continue;
        }
        let mut end = first + 1;
        while end < changed.len() && changed[end] {
            end += 1;
        }
        ranges.push(first..end);
        first = end;
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::super::links::url_search;
    use super::super::{HighlightRule, TerminalSize};
    use super::*;
    use alacritty_terminal::event::{Event, EventListener};
    use alacritty_terminal::grid::Scroll;
    use alacritty_terminal::index::Side;
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    struct Listener;
    impl EventListener for Listener {
        fn send_event(&self, _: Event) {}
    }

    fn terminal() -> Term<Listener> {
        Term::new(
            Config::default(),
            &TerminalSize::new(24, 6, 8, 16),
            Listener,
        )
    }

    fn feed(term: &mut Term<Listener>, bytes: &[u8]) {
        let mut processor: Processor = Processor::new();
        processor.advance(term, bytes);
    }

    fn read(cache: &mut SnapshotCache, term: &mut Term<Listener>) -> TerminalSnapshot {
        cache.read(
            term,
            &mut None,
            &mut url_search(),
            &HighlightSet::default(),
            1,
        )
    }

    #[test]
    fn typing_and_blink_frames_reuse_unchanged_rows() {
        let mut term = terminal();
        feed(&mut term, b"first\r\nsecond\r\nthird");
        let mut cache = SnapshotCache::default();
        let before = read(&mut cache, &mut term);
        let blink = read(&mut cache, &mut term);
        assert!(
            before
                .lines
                .iter()
                .zip(&blink.lines)
                .all(|(a, b)| Rc::ptr_eq(a, b))
        );
        feed(&mut term, b"!");
        let typed = read(&mut cache, &mut term);
        assert!(!Rc::ptr_eq(&before.lines[2], &typed.lines[2]));
        assert_eq!(
            before
                .lines
                .iter()
                .zip(&typed.lines)
                .filter(|(a, b)| Rc::ptr_eq(a, b))
                .count(),
            5
        );
        feed(&mut term, b"\x1b[1;1H");
        let moved = read(&mut cache, &mut term);
        assert!(
            typed
                .lines
                .iter()
                .zip(&moved.lines)
                .all(|(a, b)| Rc::ptr_eq(a, b))
        );
        assert_eq!(moved.cursor_row, Some(0));
    }

    #[test]
    fn edits_to_wrapped_links_and_highlights_match_full_scans() {
        let mut term = terminal();
        let mut cache = SnapshotCache::default();
        let highlights = HighlightSet::new(
            true,
            vec![HighlightRule {
                pattern: "ERROR|wrapped/path|中文".into(),
                ..HighlightRule::default()
            }],
        );
        for text in [
            "https://example.com/long/wrapped/path\r\nERROR 中文\r\nhttps://other.test/",
            "\x1b[1;23H!",           // Rewrite part of the first wrapped URL.
            "\x1b[2;1H\x1b[K",       // Remove its continuation.
            "\x1b[1;1H\x1b[2Kplain", // Split the old logical line.
            "\x1b[3;1Hhttps://new.test/long/another/wrapped/path",
            "\x1b[1;1H\x1b]8;id=same;https://osc.test\x07中文\x1b]8;;\x07",
            "\x1b[4;1H\x1b]8;id=same;https://osc.test\x07more\x1b]8;;\x07",
            "\x1b[3;1H\x1b[M", // Delete a row and move every later link.
            "\x1b[2;1H\x1b[L", // Insert a row.
        ] {
            feed(&mut term, text.as_bytes());
            let actual = cache.read(&mut term, &mut None, &mut url_search(), &highlights, 1);
            let expected = SnapshotCache::default().read(
                &mut term,
                &mut None,
                &mut url_search(),
                &highlights,
                1,
            );
            assert_eq!(actual.lines, expected.lines, "{text:?}");
            assert_eq!(actual.links, expected.links, "{text:?}");
        }
        term.selection = Some(Selection::new(
            SelectionType::Block,
            Point::new(Line(0), Column(0)),
            Side::Left,
        ));
        term.selection
            .as_mut()
            .unwrap()
            .update(Point::new(Line(4), Column(8)), Side::Right);
        let selected = cache.read(&mut term, &mut None, &mut url_search(), &highlights, 1);
        let blink = cache.read(&mut term, &mut None, &mut url_search(), &highlights, 1);
        assert!(Rc::ptr_eq(&selected.links, &blink.links));
        assert!(
            selected
                .lines
                .iter()
                .zip(&blink.lines)
                .all(|(a, b)| Rc::ptr_eq(a, b))
        );
    }

    #[test]
    fn incremental_snapshots_match_fresh_snapshots_across_terminal_operations() {
        let mut term = terminal();
        let mut cache = SnapshotCache::default();
        let mut search = Some(TerminalSearch::new());
        let highlights = HighlightSet::new(
            true,
            vec![HighlightRule {
                pattern: "ERROR".into(),
                ..HighlightRule::default()
            }],
        );
        for bytes in [
            "hello 中文 e\u{301}\r\nERROR",
            "\x1b[31mred\x1b[0m",
            "\r\nhttps://example.com/long/wrapped/path",
            "\x1b[1;1H\x1b[K",
            "\x1b]8;;https://example.org\x07link\x1b]8;;\x07",
            "\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7",
            "\x1b[?1049hfullscreen\x1b[2;3H中",
            "\x1b[?1049l",
            "\x1b]4;1;rgb:ff/00/00\x07",
            "\x1b[2J\x1b[Hlast ERROR",
        ] {
            feed(&mut term, bytes.as_bytes());
            let actual = cache.read(&mut term, &mut search, &mut url_search(), &highlights, 1);
            let expected = SnapshotCache::default().read(
                &mut term,
                &mut search,
                &mut url_search(),
                &highlights,
                1,
            );
            assert_eq!(actual.lines, expected.lines, "{bytes:?}");
            assert_eq!(actual.links, expected.links);
            assert_eq!(actual.colors, expected.colors);
        }
        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        ));
        term.selection
            .as_mut()
            .unwrap()
            .update(Point::new(Line(0), Column(8)), Side::Right);
        for step in 0..5 {
            match step {
                0 => {}
                1 => {
                    search.as_mut().unwrap().set_query("ERROR", &mut term);
                    cache.invalidate();
                }
                2 => {
                    term.selection = None;
                }
                3 => {
                    term.scroll_display(Scroll::Top);
                }
                _ => term.resize(TerminalSize::new(12, 4, 8, 16)),
            }
            let actual = cache.read(&mut term, &mut search, &mut url_search(), &highlights, 1);
            let expected = SnapshotCache::default().read(
                &mut term,
                &mut search,
                &mut url_search(),
                &highlights,
                1,
            );
            assert_eq!(actual.lines, expected.lines, "step {step}");
            assert_eq!(actual.links, expected.links);
        }
        let mut replacement = terminal();
        let actual = cache.read(
            &mut replacement,
            &mut None,
            &mut url_search(),
            &highlights,
            2,
        );
        assert!(actual.visible_text().trim().is_empty());
    }
}
