//! How wide the two sidebars are. Each opens at its default width, or at
//! the width it was last dragged to; is dragged no narrower than its
//! minimum; and goes back to its default from 设置 › 应用 › 窗口.
//!
//! The widths are kept in the settings once a drag settles, and on quitting.
//! The workspace remembers what it last wrote there, so it tells its own
//! writes from a reset on the settings page.

use std::time::Duration;

use gpui_kit::component::dock::DockPlacement;
use gpui_kit::*;

use super::Workspace;
use crate::settings::SidebarWidths;

/// How long a drag has to settle before its width is written: the dock
/// reports every step.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// The left sidebar's width on a first launch and after a reset.
/// Dock geometry is an API boundary that takes `Pixels`.
pub(super) const SIDEBAR_WIDTH: Pixels = px(280.);
/// The narrowest the left sidebar can be dragged. The title bar's sidebar
/// toggle keeps clear of the list switch, which can be wider.
pub(super) const SIDEBAR_MIN_WIDTH: Pixels = px(180.);

/// The right sidebar's width on a first launch and after a reset: the
/// monitor's and the connections' cards are laid out for it.
pub(super) const TOOL_SIDEBAR_WIDTH: Pixels = px(320.);
/// The narrowest the right sidebar can be dragged.
pub(super) const TOOL_SIDEBAR_MIN_WIDTH: Pixels = px(220.);

/// One sidebar's place and widths.
#[derive(Clone, Copy)]
struct Sidebar {
    placement: DockPlacement,
    default: Pixels,
    min: Pixels,
}

const LEFT: Sidebar = Sidebar {
    placement: DockPlacement::Left,
    default: SIDEBAR_WIDTH,
    min: SIDEBAR_MIN_WIDTH,
};

const RIGHT: Sidebar = Sidebar {
    placement: DockPlacement::Right,
    default: TOOL_SIDEBAR_WIDTH,
    min: TOOL_SIDEBAR_MIN_WIDTH,
};

impl Sidebar {
    /// The width it opens at from a saved one.
    fn opening(self, saved: Option<f32>) -> Pixels {
        saved.map_or(self.default, px).max(self.min)
    }

    /// A width as the settings keep it: whole pixels, `None` at the default.
    fn saved(self, width: Pixels) -> Option<f32> {
        let width = f32::from(width).round();
        (width != f32::from(self.default)).then_some(width)
    }
}

/// The widths the sidebars open at, from the ones saved.
pub(super) fn opening_widths(saved: SidebarWidths) -> (Pixels, Pixels) {
    (LEFT.opening(saved.left), RIGHT.opening(saved.right))
}

impl Workspace {
    /// Undo a drag that took a sidebar below its narrowest. Runs whenever
    /// the dock area changes; the dock's own minimum is lower, and its drag
    /// callback is not ours to wrap. Set back within the same update, so no
    /// frame shows it narrower.
    pub(super) fn hold_sidebar_widths(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for sidebar in [LEFT, RIGHT] {
            let narrower = self
                .dock_area
                .read(cx)
                .dock_size(sidebar.placement)
                .is_some_and(|width| width < sidebar.min);
            if narrower {
                self.dock_area.update(cx, |area, cx| {
                    area.set_dock_size(sidebar.placement, sidebar.min, window, cx);
                });
            }
        }
    }

    /// The widths the sidebars have now, as the settings keep them.
    fn current_sidebar_widths(&self, cx: &App) -> SidebarWidths {
        let area = self.dock_area.read(cx);
        let width = |sidebar: Sidebar, kept: Option<f32>| {
            area.dock_size(sidebar.placement)
                .map_or(kept, |width| sidebar.saved(width))
        };
        SidebarWidths {
            left: width(LEFT, self.sidebar_widths.left),
            right: width(RIGHT, self.sidebar_widths.right),
        }
    }

    /// Write the widths once they have stayed as they are for a moment.
    /// Runs whenever the dock area changes.
    pub(super) fn remember_sidebar_widths(&mut self, cx: &mut Context<Self>) {
        if self.current_sidebar_widths(cx) == self.sidebar_widths {
            self.sidebar_widths_save = None;
            return;
        }
        self.sidebar_widths_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            _ = this.update(cx, |this, cx| this.save_sidebar_widths(cx));
        }));
    }

    /// Write the widths now, if they changed: as a drag settles, and as
    /// ShellRS quits, in case one has not.
    pub(super) fn save_sidebar_widths(&mut self, cx: &mut App) {
        self.sidebar_widths_save = None;
        let widths = self.current_sidebar_widths(cx);
        if widths == self.sidebar_widths {
            return;
        }
        self.sidebar_widths = widths;
        self.settings().update(cx, |store, cx| {
            store.update(|settings| settings.sidebar_widths = widths, cx)
        });
    }

    /// Take widths the settings page put in the settings, which only resets
    /// them. Runs whenever the settings change.
    pub(super) fn follow_sidebar_widths(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self.settings().read(cx).settings().sidebar_widths;
        if saved == self.sidebar_widths {
            return;
        }
        self.sidebar_widths = saved;
        self.sidebar_widths_save = None;
        let (left, right) = opening_widths(saved);
        self.dock_area.update(cx, |area, cx| {
            area.set_dock_size(DockPlacement::Left, left, window, cx);
            area.set_dock_size(DockPlacement::Right, right, window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{LEFT, RIGHT};
    use gpui_kit::px;

    #[test]
    fn a_sidebar_opens_at_its_saved_width_but_not_below_its_minimum() {
        assert_eq!(LEFT.opening(None), px(280.));
        assert_eq!(LEFT.opening(Some(240.)), px(240.));
        assert_eq!(LEFT.opening(Some(120.)), px(180.));
        assert_eq!(RIGHT.opening(None), px(320.));
        assert_eq!(RIGHT.opening(Some(200.)), px(220.));
        assert_eq!(RIGHT.opening(Some(480.)), px(480.));
    }

    #[test]
    fn the_default_width_is_saved_as_none() {
        assert_eq!(LEFT.saved(px(280.)), None);
        assert_eq!(LEFT.saved(px(279.8)), None);
        assert_eq!(LEFT.saved(px(240.4)), Some(240.));
        assert_eq!(RIGHT.saved(px(320.)), None);
        assert_eq!(RIGHT.saved(px(280.)), Some(280.));
    }
}
