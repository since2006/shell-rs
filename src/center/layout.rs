//! How a tab lays its panes out, as data: a pane, or a row or column of
//! layouts, each with its share of the row or column.
//!
//! The tab's dock area draws it, and lets the dividers be dragged, but the
//! view decides every rearrangement here and hands the whole layout back to
//! the dock with each pane's size spelled out. Left to itself the dock lets
//! the sizes of a row drift when a pane leaves it or joins it from
//! elsewhere, and a few moves leave a pane a sliver.
//!
//! The rules:
//!
//! - a pane dropped beside another joins that pane's row or column, which
//!   then shares its space equally; beside a pane across its row, the two
//!   share the pane's place half and half;
//! - a pane leaving gives its space to the rest of its row, in proportion;
//! - two panes swap places, and nothing else moves;
//! - a row of one is no row, and a row in a row of the same direction is
//!   spliced into it.
//!
//! Every edit keeps these true, and `check` asserts them in the tests: a
//! row or column holds at least two layouts, none of its own direction, and
//! their shares are positive and add up to one.

use gpui_kit::Axis;
use gpui_kit::component::Placement;

#[derive(Clone, Debug, PartialEq)]
pub enum Layout<P> {
    Pane(P),
    /// Side by side for `Axis::Horizontal`, stacked for `Axis::Vertical`.
    Split {
        axis: Axis,
        children: Vec<(Layout<P>, f32)>,
    },
}

/// Which side of what a pane joins, before or after it.
fn before(side: Placement) -> bool {
    matches!(side, Placement::Left | Placement::Top)
}

impl<P: Copy + Eq + std::fmt::Debug> Layout<P> {
    /// A row or column of `children`, each with its share, however the
    /// shares add up: as a layout of the rules.
    pub fn split(axis: Axis, children: Vec<(Layout<P>, f32)>) -> Self {
        let mut layout = Self::Split { axis, children };
        layout.normalize();
        layout
    }

    /// The same layout with each pane `f` of it.
    pub fn map<Q>(&self, f: &impl Fn(P) -> Q) -> Layout<Q> {
        match self {
            Self::Pane(pane) => Layout::Pane(f(*pane)),
            Self::Split { axis, children } => Layout::Split {
                axis: *axis,
                children: children
                    .iter()
                    .map(|(child, share)| (child.map(f), *share))
                    .collect(),
            },
        }
    }

    /// The panes in reading order: left to right, then top to bottom.
    pub fn panes(&self) -> Vec<P> {
        let mut panes = Vec::new();
        self.collect(&mut panes);
        panes
    }

    fn collect(&self, panes: &mut Vec<P>) {
        match self {
            Self::Pane(pane) => panes.push(*pane),
            Self::Split { children, .. } => {
                for (child, _) in children {
                    child.collect(panes);
                }
            }
        }
    }

    pub fn contains(&self, pane: P) -> bool {
        match self {
            Self::Pane(own) => *own == pane,
            Self::Split { children, .. } => children.iter().any(|(child, _)| child.contains(pane)),
        }
    }

    /// Put `pane` on `side` of `beside`. Returns whether it went in.
    pub fn insert(&mut self, pane: P, beside: P, side: Placement) -> bool {
        if self.contains(pane) || !self.contains(beside) {
            return false;
        }
        self.insert_beside(pane, beside, side);
        self.normalize();
        true
    }

    fn insert_beside(&mut self, pane: P, beside: P, side: Placement) {
        if let Self::Split { axis, children } = self {
            let ix = children
                .iter()
                .position(|(child, _)| child.contains(beside))
                .expect("checked by the caller");
            if *axis == side.axis() && children[ix].0 == Self::Pane(beside) {
                // Into its row, which shares its space equally again.
                let at = if before(side) { ix } else { ix + 1 };
                children.insert(at, (Self::Pane(pane), 0.));
                equal_shares(children);
            } else {
                children[ix].0.insert_beside(pane, beside, side);
            }
            return;
        }
        // The pane itself, which the two share half and half.
        let new = Self::Pane(pane);
        let old = std::mem::replace(self, Self::Pane(beside));
        let children = if before(side) {
            vec![(new, 0.5), (old, 0.5)]
        } else {
            vec![(old, 0.5), (new, 0.5)]
        };
        *self = Self::Split {
            axis: side.axis(),
            children,
        };
    }

    /// Put `pane` along the whole `side` of the layout. Returns whether it
    /// went in.
    pub fn insert_at_edge(&mut self, pane: P, side: Placement) -> bool {
        if self.contains(pane) {
            return false;
        }
        let new = Self::Pane(pane);
        match self {
            Self::Split { axis, children } if *axis == side.axis() => {
                let at = if before(side) { 0 } else { children.len() };
                children.insert(at, (new, 0.));
                equal_shares(children);
            }
            _ => {
                let old = std::mem::replace(self, Self::Pane(pane));
                let children = if before(side) {
                    vec![(new, 0.5), (old, 0.5)]
                } else {
                    vec![(old, 0.5), (new, 0.5)]
                };
                *self = Self::Split {
                    axis: side.axis(),
                    children,
                };
            }
        }
        self.normalize();
        true
    }

    /// Take `pane` out; the rest of its row takes its space. The last pane
    /// stays. Returns whether it went.
    pub fn remove(&mut self, pane: P) -> bool {
        if !self.contains(pane) || *self == Self::Pane(pane) {
            return false;
        }
        self.remove_from(pane);
        self.normalize();
        true
    }

    fn remove_from(&mut self, pane: P) {
        let Self::Split { children, .. } = self else {
            return;
        };
        let Some(ix) = children.iter().position(|(child, _)| child.contains(pane)) else {
            return;
        };
        if children[ix].0 == Self::Pane(pane) {
            let (_, share) = children.remove(ix);
            let rest = 1. - share;
            for (_, other) in children.iter_mut() {
                *other = if rest > f32::EPSILON {
                    *other / rest
                } else {
                    0.
                };
            }
            if rest <= f32::EPSILON {
                equal_shares(children);
            }
        } else {
            children[ix].0.remove_from(pane);
        }
    }

    /// Move `pane` to `side` of `beside`. Returns whether it moved.
    pub fn move_beside(&mut self, pane: P, beside: P, side: Placement) -> bool {
        if pane == beside || !self.contains(pane) || !self.contains(beside) {
            return false;
        }
        let before = self.clone();
        if self.remove(pane) && self.insert(pane, beside, side) {
            return true;
        }
        *self = before;
        false
    }

    /// `a` where `b` was and `b` where `a` was. Returns whether they
    /// swapped.
    pub fn swap(&mut self, a: P, b: P) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        self.swap_panes(a, b);
        true
    }

    fn swap_panes(&mut self, a: P, b: P) {
        match self {
            Self::Pane(pane) if *pane == a => *pane = b,
            Self::Pane(pane) if *pane == b => *pane = a,
            Self::Pane(_) => {}
            Self::Split { children, .. } => {
                for (child, _) in children {
                    child.swap_panes(a, b);
                }
            }
        }
    }

    /// Every row and column shares its space equally.
    pub fn equalize(&mut self) {
        if let Self::Split { children, .. } = self {
            equal_shares(children);
            for (child, _) in children {
                child.equalize();
            }
        }
    }

    /// Each pane's width and height, the whole layout being `width` by
    /// `height`.
    pub fn pane_sizes(&self, width: f32, height: f32) -> Vec<(P, f32, f32)> {
        let mut sizes = Vec::new();
        self.collect_sizes(width, height, &mut sizes);
        sizes
    }

    fn collect_sizes(&self, width: f32, height: f32, sizes: &mut Vec<(P, f32, f32)>) {
        match self {
            Self::Pane(pane) => sizes.push((*pane, width, height)),
            Self::Split { axis, children } => {
                for (child, share) in children {
                    match axis {
                        Axis::Horizontal => child.collect_sizes(width * share, height, sizes),
                        Axis::Vertical => child.collect_sizes(width, height * share, sizes),
                    }
                }
            }
        }
    }

    /// Whether every pane is at least `min_width` by `min_height`, the
    /// whole layout being `width` by `height`.
    pub fn fits(&self, width: f32, height: f32, min_width: f32, min_height: f32) -> bool {
        self.pane_sizes(width, height)
            .iter()
            .all(|(_, width, height)| *width + 0.5 >= min_width && *height + 0.5 >= min_height)
    }

    /// Bring the layout back to the rules after an edit: rows of one are
    /// their one layout, rows within rows of the same direction are spliced
    /// in, and shares add up to one.
    fn normalize(&mut self) {
        let Self::Split { axis, children } = self else {
            return;
        };
        let axis = *axis;
        let mut flat = Vec::with_capacity(children.len());
        for (mut child, share) in std::mem::take(children) {
            child.normalize();
            match child {
                Self::Split {
                    axis: inner,
                    children: grandchildren,
                } if inner == axis => {
                    flat.extend(
                        grandchildren
                            .into_iter()
                            .map(|(grandchild, inner_share)| (grandchild, inner_share * share)),
                    );
                }
                child => flat.push((child, share)),
            }
        }
        let total: f32 = flat.iter().map(|(_, share)| share.max(0.)).sum();
        if total > f32::EPSILON {
            for (_, share) in &mut flat {
                *share = share.max(0.) / total;
            }
        } else {
            equal_shares(&mut flat);
        }
        if flat.len() == 1 {
            *self = flat.pop().expect("one child").0;
        } else {
            *children = flat;
        }
    }
}

fn equal_shares<T>(children: &mut [(T, f32)]) {
    let share = 1. / children.len().max(1) as f32;
    for (_, own) in children {
        *own = share;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use Placement::{Bottom, Left, Right, Top};

    type L = Layout<u32>;

    fn pane(pane: u32) -> L {
        Layout::Pane(pane)
    }

    fn row(children: Vec<L>) -> L {
        let share = 1. / children.len() as f32;
        Layout::split(
            Axis::Horizontal,
            children.into_iter().map(|child| (child, share)).collect(),
        )
    }

    fn column(children: Vec<L>) -> L {
        let share = 1. / children.len() as f32;
        Layout::split(
            Axis::Vertical,
            children.into_iter().map(|child| (child, share)).collect(),
        )
    }

    /// 1 and 2 above, 3 and 4 below: two columns, as dragging builds it.
    fn grid() -> L {
        row(vec![
            column(vec![pane(1), pane(3)]),
            column(vec![pane(2), pane(4)]),
        ])
    }

    fn check(layout: &L) {
        let Layout::Split { axis, children } = layout else {
            return;
        };
        assert!(children.len() >= 2, "a row of one: {layout:?}");
        let total: f32 = children.iter().map(|(_, share)| share).sum();
        assert!(
            (total - 1.).abs() < 1e-4,
            "shares add up to {total}: {layout:?}"
        );
        for (child, share) in children {
            assert!(*share > 0., "a share of nothing: {layout:?}");
            if let Layout::Split { axis: inner, .. } = child {
                assert_ne!(inner, axis, "a row in a row: {layout:?}");
            }
            check(child);
        }
    }

    /// Each pane's size in a 1200 by 800 tab, rounded.
    fn sizes(layout: &L) -> Vec<(u32, u32, u32)> {
        check(layout);
        layout
            .pane_sizes(1200., 800.)
            .into_iter()
            .map(|(pane, width, height)| (pane, width.round() as u32, height.round() as u32))
            .collect()
    }

    #[test]
    fn a_pane_beside_another_shares_its_place_half_and_half() {
        let mut layout = pane(1);
        assert!(layout.insert(2, 1, Right));
        assert_eq!(sizes(&layout), [(1, 600, 800), (2, 600, 800)]);
        assert!(layout.insert(3, 1, Bottom));
        assert_eq!(
            sizes(&layout),
            [(1, 600, 400), (3, 600, 400), (2, 600, 800)]
        );
    }

    #[test]
    fn a_pane_joining_a_row_shares_it_equally() {
        let mut layout = pane(1);
        layout.insert(2, 1, Right);
        layout.insert(3, 2, Right);
        assert_eq!(
            sizes(&layout),
            [(1, 400, 800), (2, 400, 800), (3, 400, 800)]
        );
        layout.insert(4, 1, Left);
        assert_eq!(layout.panes(), [4, 1, 2, 3]);
        assert!(sizes(&layout).iter().all(|(_, width, _)| *width == 300));
    }

    #[test]
    fn moving_in_a_grid_keeps_every_pane_usable() {
        // 3 to the right of 1: 1's column is 1 alone now, so 3 joins the
        // row, which shares itself equally.
        let mut layout = grid();
        assert!(layout.move_beside(3, 1, Right));
        assert_eq!(
            sizes(&layout),
            [(1, 400, 800), (3, 400, 800), (2, 400, 400), (4, 400, 400)]
        );
        // Then 2 below 1: 4 has 2's column alone, and 1 and 2 share 1's
        // place.
        assert!(layout.move_beside(2, 1, Bottom));
        assert_eq!(
            sizes(&layout),
            [(1, 400, 400), (2, 400, 400), (3, 400, 800), (4, 400, 800)]
        );
    }

    #[test]
    fn a_pane_leaving_gives_its_space_to_its_row() {
        let mut layout = Layout::split(
            Axis::Horizontal,
            vec![(pane(1), 0.5), (pane(2), 0.25), (pane(3), 0.25)],
        );
        assert!(layout.remove(1));
        assert_eq!(sizes(&layout), [(2, 600, 800), (3, 600, 800)]);
        // A column left with one pane is that pane.
        let mut layout = grid();
        assert!(layout.remove(3));
        assert_eq!(
            sizes(&layout),
            [(1, 600, 800), (2, 600, 400), (4, 600, 400)]
        );
        // The last pane stays.
        let mut layout = pane(1);
        assert!(!layout.remove(1));
        assert_eq!(layout, pane(1));
    }

    #[test]
    fn two_panes_swap_and_nothing_else_moves() {
        let mut layout = grid();
        assert!(layout.swap(3, 2));
        assert_eq!(layout.panes(), [1, 2, 3, 4]);
        assert_eq!(
            sizes(&layout),
            [(1, 600, 400), (2, 600, 400), (3, 600, 400), (4, 600, 400)]
        );
        assert!(!layout.swap(1, 1));
        assert!(!layout.swap(1, 9));
    }

    #[test]
    fn a_pane_at_an_edge_runs_along_the_whole_side() {
        let mut layout = grid();
        assert!(layout.insert_at_edge(5, Right));
        assert_eq!(
            sizes(&layout),
            [
                (1, 400, 400),
                (3, 400, 400),
                (2, 400, 400),
                (4, 400, 400),
                (5, 400, 800)
            ]
        );
        assert!(layout.insert_at_edge(6, Top));
        assert_eq!(layout.panes()[0], 6);
        assert_eq!(sizes(&layout)[0], (6, 1200, 400));
        assert!(!layout.insert_at_edge(6, Left));
    }

    #[test]
    fn equalizing_evens_out_every_row_and_column() {
        let mut layout = Layout::split(
            Axis::Horizontal,
            vec![
                (pane(1), 0.7),
                (
                    Layout::split(Axis::Vertical, vec![(pane(2), 0.9), (pane(3), 0.1)]),
                    0.3,
                ),
            ],
        );
        layout.equalize();
        assert_eq!(
            sizes(&layout),
            [(1, 600, 800), (2, 600, 400), (3, 600, 400)]
        );
    }

    #[test]
    fn a_layout_tells_whether_its_panes_fit() {
        let mut layout = pane(1);
        for (pane, beside) in [(2, 1), (3, 2), (4, 3), (5, 4), (6, 5)] {
            layout.insert(pane, beside, Right);
        }
        // Six in a row of 1200: 200 each.
        assert!(layout.fits(1200., 800., 200., 100.));
        layout.insert(7, 6, Right);
        assert!(!layout.fits(1200., 800., 200., 100.));
    }

    #[test]
    fn an_edit_that_cannot_be_done_changes_nothing() {
        let mut layout = grid();
        let before = layout.clone();
        assert!(!layout.insert(1, 2, Right));
        assert!(!layout.insert(5, 9, Right));
        assert!(!layout.move_beside(1, 1, Right));
        assert!(!layout.move_beside(1, 9, Right));
        assert!(!layout.remove(9));
        assert_eq!(layout, before);
    }

    #[test]
    fn rows_within_rows_are_spliced_and_rows_of_one_unwrapped() {
        let layout = Layout::split(
            Axis::Horizontal,
            vec![
                (
                    Layout::split(Axis::Horizontal, vec![(pane(1), 0.5), (pane(2), 0.5)]),
                    0.5,
                ),
                (Layout::split(Axis::Vertical, vec![(pane(3), 1.)]), 0.5),
            ],
        );
        assert_eq!(
            sizes(&layout),
            [(1, 300, 800), (2, 300, 800), (3, 600, 800)]
        );
    }

    /// Random moves, swaps, removals and insertions keep the rules and
    /// lose no pane.
    #[test]
    fn any_sequence_of_edits_keeps_the_rules() {
        let sides = [Left, Right, Top, Bottom];
        let mut state = 0x9e37_79b9_u64;
        let mut next = |below: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % below.max(1) as u64) as usize
        };
        let mut layout = pane(0);
        let mut panes = vec![0];
        let mut fresh = 1;
        for _ in 0..2000 {
            let a = panes[next(panes.len())];
            let b = panes[next(panes.len())];
            let side = sides[next(4)];
            match next(5) {
                0 if panes.len() < 9 => {
                    assert!(layout.insert(fresh, a, side));
                    panes.push(fresh);
                    fresh += 1;
                }
                1 if panes.len() > 1 => {
                    assert!(layout.remove(a));
                    panes.retain(|pane| *pane != a);
                }
                2 => {
                    assert_eq!(layout.move_beside(a, b, side), a != b);
                }
                3 => {
                    assert_eq!(layout.swap(a, b), a != b);
                }
                _ => layout.equalize(),
            }
            check(&layout);
            let mut present = layout.panes();
            present.sort();
            let mut expected = panes.clone();
            expected.sort();
            assert_eq!(present, expected);
        }
    }
}
