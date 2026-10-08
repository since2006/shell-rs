//! The terminal's scrollbar: gpui-kit's, over the scrollback.
//!
//! The grid is drawn afresh each frame and scrolls by whole lines, so there
//! is no scroll handle for gpui-kit's `Scrollbar` to follow. This one gives
//! it the history in pixels: a line is a row's height, the content is the
//! scrollbar's own height plus the history, and the offset is how far the
//! view is from the oldest line.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::component::scroll::ScrollbarHandle;
use gpui_kit::{Bounds, Pixels, Point, Size, point, px, size};

use super::TerminalSnapshot;
use super::engine::Scrollback;

#[derive(Clone)]
pub(super) struct ScrollbackHandle {
    scrollback: Scrollback,
    history_size: usize,
    display_offset: usize,
    line_height: Pixels,
    /// Where the scrollbar lies, which the canvas under it notes before the
    /// scrollbar is laid out in the same frame.
    viewport: Rc<Cell<Bounds<Pixels>>>,
}

impl ScrollbackHandle {
    pub(super) fn new(
        scrollback: Scrollback,
        snapshot: &TerminalSnapshot,
        line_height: Pixels,
    ) -> Self {
        Self {
            scrollback,
            history_size: snapshot.history_size,
            display_offset: snapshot.display_offset,
            line_height,
            viewport: Rc::default(),
        }
    }

    pub(super) fn set_viewport(&self, bounds: Bounds<Pixels>) {
        self.viewport.set(bounds);
    }
}

impl ScrollbarHandle for ScrollbackHandle {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.viewport.get()
    }

    fn offset(&self) -> Point<Pixels> {
        point(
            px(0.),
            offset(self.history_size, self.display_offset, self.line_height),
        )
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        self.scrollback
            .scroll_to(lines_from_top(offset.y, self.line_height));
    }

    fn content_size(&self) -> Size<Pixels> {
        let viewport = self.viewport.get().size;
        size(
            viewport.width,
            viewport.height + self.line_height * self.history_size as f32,
        )
    }
}

/// How far the view is scrolled down from the oldest line, as gpui-kit
/// counts it: zero at the top, negative below.
fn offset(history_size: usize, display_offset: usize, line_height: Pixels) -> Pixels {
    -(line_height * history_size.saturating_sub(display_offset) as f32)
}

/// The line at the top of the view for a scroll `offset`.
fn lines_from_top(offset: Pixels, line_height: Pixels) -> usize {
    (-offset / line_height).round().max(0.) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_offset_counts_whole_lines_from_the_oldest() {
        let line = px(17.);
        // At the bottom of 100 lines of history, 100 lines down.
        assert_eq!(offset(100, 0, line), px(-1700.));
        // At the top.
        assert_eq!(offset(100, 100, line), px(0.));
        assert_eq!(lines_from_top(offset(100, 40, line), line), 60);
        // A drag lands between lines; past the top is the top.
        assert_eq!(lines_from_top(px(-25.), line), 1);
        assert_eq!(lines_from_top(px(-26.), line), 2);
        assert_eq!(lines_from_top(px(5.), line), 0);
    }
}
