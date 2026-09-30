//! WinSCP-style multi-selection over one directory listing, kept by name so it
//! survives sorting and refreshes.

use std::collections::HashSet;

/// The `..` row: it can hold the cursor but is never part of a selection.
const PARENT: &str = "..";

/// How a row click changes the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickMode {
    /// A plain click: select only this row.
    Replace,
    /// ⌘/Ctrl-click: add or remove this row.
    Toggle,
    /// Shift-click: select the rows from the anchor to this one.
    Extend,
}

/// A keyboard cursor movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMotion {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    names: HashSet<String>,
    cursor: Option<String>,
    anchor: Option<String>,
}

impl Selection {
    pub fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Apply a row click. `order` is the displayed row order, `..` included.
    pub fn click(&mut self, name: &str, mode: ClickMode, order: &[&str]) {
        if !order.contains(&name) {
            return;
        }
        match mode {
            ClickMode::Replace => {
                self.names = selectable(name).into_iter().collect();
                self.anchor = Some(name.to_string());
            }
            ClickMode::Toggle => {
                if let Some(name) = selectable(name)
                    && !self.names.remove(&name)
                {
                    self.names.insert(name);
                }
                self.anchor = Some(name.to_string());
            }
            ClickMode::Extend => {
                let anchor = self
                    .anchor
                    .clone()
                    .or_else(|| self.cursor.clone())
                    .unwrap_or_else(|| name.to_string());
                self.names = range(order, &anchor, name);
                self.anchor = Some(anchor);
            }
        }
        self.cursor = Some(name.to_string());
    }

    /// Move the cursor, replacing the selection with the new row, or with the
    /// range from the anchor when `extend`. `page` is the number of visible
    /// rows. Returns the new cursor index.
    pub fn move_cursor(
        &mut self,
        motion: CursorMotion,
        page: usize,
        extend: bool,
        order: &[&str],
    ) -> Option<usize> {
        let last = order.len().checked_sub(1)?;
        let current = self
            .cursor
            .as_deref()
            .and_then(|cursor| order.iter().position(|name| *name == cursor));
        let page = page.max(1);
        let next = match (motion, current) {
            (CursorMotion::Home, _) => 0,
            (CursorMotion::End, _) => last,
            (CursorMotion::Down | CursorMotion::PageDown, None) => 0,
            (CursorMotion::Up | CursorMotion::PageUp, None) => last,
            (CursorMotion::Up, Some(ix)) => ix.saturating_sub(1),
            (CursorMotion::Down, Some(ix)) => (ix + 1).min(last),
            (CursorMotion::PageUp, Some(ix)) => ix.saturating_sub(page),
            (CursorMotion::PageDown, Some(ix)) => (ix + page).min(last),
        };
        let name = order[next];
        if extend {
            self.click(name, ClickMode::Extend, order);
        } else {
            self.click(name, ClickMode::Replace, order);
        }
        Some(next)
    }

    /// Space / Insert: add or remove the cursor row.
    pub fn toggle_cursor(&mut self, order: &[&str]) {
        if let Some(cursor) = self.cursor.clone() {
            self.click(&cursor, ClickMode::Toggle, order);
        }
    }

    pub fn select_all(&mut self, order: &[&str]) {
        self.names = order.iter().filter_map(|name| selectable(name)).collect();
    }

    /// Select exactly one row and put the cursor on it; nothing when absent.
    pub fn select_only(&mut self, name: &str, order: &[&str]) {
        self.click(name, ClickMode::Replace, order);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Forget rows that are no longer listed.
    pub fn retain(&mut self, order: &[&str]) {
        let present: HashSet<&str> = order.iter().copied().collect();
        self.names.retain(|name| present.contains(name.as_str()));
        for slot in [&mut self.cursor, &mut self.anchor] {
            if slot.as_deref().is_some_and(|name| !present.contains(name)) {
                *slot = None;
            }
        }
    }

    /// A selection rectangle over the rows `rows` of `order`, adding to
    /// `base` when the drag began with ⌘ or Shift held and replacing
    /// otherwise. The cursor goes to `cursor`, the row under the pointer.
    pub fn sweep(
        &mut self,
        base: Option<&Selection>,
        rows: Option<std::ops::RangeInclusive<usize>>,
        cursor: Option<usize>,
        order: &[&str],
    ) {
        let mut names = base.map(|base| base.names.clone()).unwrap_or_default();
        if let Some(rows) = rows {
            names.extend(order[rows].iter().filter_map(|name| selectable(name)));
        }
        self.names = names;
        if let Some(name) = cursor.and_then(|row| order.get(row)) {
            self.cursor = Some(name.to_string());
            self.anchor = Some(name.to_string());
        }
    }

    /// The selected names in display order: what an operation acts on.
    pub fn targets(&self, order: &[&str]) -> Vec<String> {
        order
            .iter()
            .filter(|name| self.names.contains(**name))
            .map(|name| name.to_string())
            .collect()
    }
}

/// The rows a selection rectangle covers, from where the drag began to where
/// the pointer is, both measured from the top of the first row and in
/// pixels. `None` when it covers none, above the first row or below the last.
pub fn swept_rows(
    from: f32,
    to: f32,
    row_height: f32,
    row_count: usize,
) -> Option<std::ops::RangeInclusive<usize>> {
    if row_height <= 0.0 || row_count == 0 {
        return None;
    }
    let (top, bottom) = (from.min(to), from.max(to));
    let last = row_count as f32 * row_height;
    if bottom < 0.0 || top >= last {
        return None;
    }
    let first = (top.max(0.0) / row_height).floor() as usize;
    let end = ((bottom.min(last - 0.5)) / row_height).floor() as usize;
    Some(first..=end.min(row_count - 1))
}

/// The row a point falls on, clamped to the listing.
pub fn row_at(y: f32, row_height: f32, row_count: usize) -> Option<usize> {
    if row_height <= 0.0 || row_count == 0 {
        return None;
    }
    Some(((y.max(0.0) / row_height).floor() as usize).min(row_count - 1))
}

fn selectable(name: &str) -> Option<String> {
    (name != PARENT).then(|| name.to_string())
}

fn range(order: &[&str], from: &str, to: &str) -> HashSet<String> {
    let (Some(start), Some(end)) = (
        order.iter().position(|name| *name == from),
        order.iter().position(|name| *name == to),
    ) else {
        return selectable(to).into_iter().collect();
    };
    order[start.min(end)..=start.max(end)]
        .iter()
        .filter_map(|name| selectable(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [&str; 5] = ["..", "a", "b", "c", "d"];

    fn names(selection: &Selection) -> Vec<String> {
        selection.targets(&ROWS)
    }

    #[test]
    fn clicks_replace_toggle_and_extend_from_the_anchor() {
        let mut selection = Selection::default();
        selection.click("b", ClickMode::Replace, &ROWS);
        assert_eq!(names(&selection), ["b"]);
        selection.click("d", ClickMode::Toggle, &ROWS);
        assert_eq!(names(&selection), ["b", "d"]);
        selection.click("b", ClickMode::Toggle, &ROWS);
        assert_eq!(names(&selection), ["d"]);
        assert_eq!(selection.cursor(), Some("b"));
        selection.click("a", ClickMode::Extend, &ROWS);
        assert_eq!(names(&selection), ["a", "b"]);
        selection.click("d", ClickMode::Extend, &ROWS);
        assert_eq!(names(&selection), ["b", "c", "d"]);
    }

    #[test]
    fn the_parent_row_takes_the_cursor_but_is_never_selected() {
        let mut selection = Selection::default();
        selection.click("..", ClickMode::Replace, &ROWS);
        assert!(selection.is_empty());
        assert_eq!(selection.cursor(), Some(".."));
        selection.click("b", ClickMode::Extend, &ROWS);
        assert_eq!(names(&selection), ["a", "b"]);
        selection.select_all(&ROWS);
        assert_eq!(names(&selection), ["a", "b", "c", "d"]);
    }

    #[test]
    fn keyboard_moves_replace_or_extend_and_clamp() {
        let mut selection = Selection::default();
        assert_eq!(
            selection.move_cursor(CursorMotion::Down, 2, false, &ROWS),
            Some(0)
        );
        assert!(selection.is_empty());
        selection.move_cursor(CursorMotion::Down, 2, false, &ROWS);
        assert_eq!(names(&selection), ["a"]);
        selection.move_cursor(CursorMotion::Down, 2, true, &ROWS);
        selection.move_cursor(CursorMotion::Down, 2, true, &ROWS);
        assert_eq!(names(&selection), ["a", "b", "c"]);
        selection.move_cursor(CursorMotion::Up, 2, true, &ROWS);
        assert_eq!(names(&selection), ["a", "b"]);
        assert_eq!(
            selection.move_cursor(CursorMotion::PageDown, 2, false, &ROWS),
            Some(4)
        );
        assert_eq!(
            selection.move_cursor(CursorMotion::Down, 2, false, &ROWS),
            Some(4)
        );
        assert_eq!(
            selection.move_cursor(CursorMotion::Home, 2, false, &ROWS),
            Some(0)
        );
        assert_eq!(
            selection.move_cursor(CursorMotion::End, 2, false, &[]),
            None
        );
    }

    #[test]
    fn space_toggles_the_cursor_row_and_retain_drops_missing_rows() {
        let mut selection = Selection::default();
        selection.select_only("c", &ROWS);
        selection.toggle_cursor(&ROWS);
        assert!(selection.is_empty());
        selection.toggle_cursor(&ROWS);
        selection.click("a", ClickMode::Toggle, &ROWS);
        selection.retain(&["..", "a", "b"]);
        assert_eq!(names(&selection), ["a"]);
        assert_eq!(selection.cursor(), Some("a"));
        selection.retain(&[".."]);
        assert!(selection.is_empty() && selection.cursor().is_none());
    }

    #[test]
    fn a_rectangle_selects_the_rows_it_crosses() {
        // Rows 20 px high: 0..20 is row 0, 20..40 row 1, and so on.
        assert_eq!(swept_rows(25.0, 65.0, 20.0, 5), Some(1..=3));
        // Upwards is the same rectangle.
        assert_eq!(swept_rows(65.0, 25.0, 20.0, 5), Some(1..=3));
        // Starting below the last row, in the empty space, and going up.
        assert_eq!(swept_rows(300.0, 70.0, 20.0, 5), Some(3..=4));
        // Past the top keeps the first row.
        assert_eq!(swept_rows(-30.0, 10.0, 20.0, 5), Some(0..=0));
        // Entirely in the empty space: nothing.
        assert_eq!(swept_rows(120.0, 300.0, 20.0, 5), None);
        assert_eq!(swept_rows(-40.0, -1.0, 20.0, 5), None);
        assert_eq!(row_at(300.0, 20.0, 5), Some(4));
        assert_eq!(row_at(-5.0, 20.0, 5), Some(0));
    }

    #[test]
    fn a_sweep_replaces_the_selection_or_adds_to_it_and_skips_the_parent() {
        let mut selection = Selection::default();
        selection.click("d", ClickMode::Replace, &ROWS);
        let before = selection.clone();

        selection.sweep(None, Some(0..=2), Some(2), &ROWS);
        assert_eq!(names(&selection), ["a", "b"]);
        assert_eq!(selection.cursor(), Some("b"));

        selection.sweep(Some(&before), Some(1..=2), Some(1), &ROWS);
        assert_eq!(names(&selection), ["a", "b", "d"]);

        // Shrinking the rectangle back gives rows up again.
        selection.sweep(Some(&before), Some(1..=1), Some(1), &ROWS);
        assert_eq!(names(&selection), ["a", "d"]);
        selection.sweep(None, None, None, &ROWS);
        assert!(selection.is_empty());
    }
}
