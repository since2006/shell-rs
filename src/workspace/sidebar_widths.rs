//! How wide the two sidebars are. Each opens at its default width, or at
//! the width it was last dragged to; is dragged no narrower than its
//! minimum; and goes back to its default from 设置 › 应用 › 窗口. The right
//! one is wider while it shows AI 助手, a conversation, and keeps that
//! width apart from the other tools'.
//!
//! The widths are kept in the settings once a drag settles, and on quitting.
//! The workspace remembers what it last wrote there, so it tells its own
//! writes from a reset on the settings page.

use std::time::Duration;

use gpui_kit::component::dock::DockPlacement;
use gpui_kit::*;

use super::Workspace;
use crate::app::ToolKind;
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
/// The right sidebar's width while it shows AI 助手, on a first launch and
/// after a reset: room for a conversation beside the terminal.
pub(super) const ASSISTANT_SIDEBAR_WIDTH: Pixels = px(420.);

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

/// The right sidebar while it shows AI 助手.
const ASSISTANT: Sidebar = Sidebar {
    placement: DockPlacement::Right,
    default: ASSISTANT_SIDEBAR_WIDTH,
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

/// The widths the sidebars open at, from the ones saved; the right one's
/// as it shows AI 助手 or as it shows another tool.
pub(super) fn opening_widths(saved: SidebarWidths, assistant: bool) -> (Pixels, Pixels) {
    let right = if assistant {
        ASSISTANT.opening(saved.assistant)
    } else {
        RIGHT.opening(saved.right)
    };
    (LEFT.opening(saved.left), right)
}

impl Workspace {
    /// Undo a drag that took a sidebar below its narrowest. Runs whenever
    /// the dock area changes; the dock's own minimum is lower, and its drag
    /// callback is not ours to wrap. Set back within the same update, so no
    /// frame shows it narrower.
    pub(super) fn hold_sidebar_widths(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for sidebar in [LEFT, self.right_sidebar()] {
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

    /// The right sidebar as it is sized now: for AI 助手 or for the rest.
    fn right_sidebar(&self) -> Sidebar {
        if self.assistant_width {
            ASSISTANT
        } else {
            RIGHT
        }
    }

    /// The widths the sidebars have now, as the settings keep them. The
    /// right sidebar's goes to the width it is sized for; the other is
    /// kept.
    fn current_sidebar_widths(&self, cx: &App) -> SidebarWidths {
        let area = self.dock_area.read(cx);
        let width = |sidebar: Sidebar, kept: Option<f32>| {
            area.dock_size(sidebar.placement)
                .map_or(kept, |width| sidebar.saved(width))
        };
        let kept = self.sidebar_widths;
        SidebarWidths {
            left: width(LEFT, kept.left),
            right: if self.assistant_width {
                kept.right
            } else {
                width(RIGHT, kept.right)
            },
            assistant: if self.assistant_width {
                width(ASSISTANT, kept.assistant)
            } else {
                kept.assistant
            },
        }
    }

    /// Size the right sidebar for the tool it shows: wider for AI 助手.
    /// Runs as the tool changes; a drag that has not settled is written to
    /// the width it was made at first.
    pub(super) fn fit_right_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let assistant = self.tools.read(cx).tool() == ToolKind::Assistant;
        if assistant == self.assistant_width {
            return;
        }
        self.save_sidebar_widths(cx);
        self.assistant_width = assistant;
        let (_, right) = opening_widths(self.sidebar_widths, assistant);
        self.dock_area.update(cx, |area, cx| {
            area.set_dock_size(DockPlacement::Right, right, window, cx);
        });
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
        let (left, right) = opening_widths(saved, self.assistant_width);
        self.dock_area.update(cx, |area, cx| {
            area.set_dock_size(DockPlacement::Left, left, window, cx);
            area.set_dock_size(DockPlacement::Right, right, window, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{ASSISTANT, LEFT, RIGHT};
    use gpui_kit::px;

    #[test]
    fn a_sidebar_opens_at_its_saved_width_but_not_below_its_minimum() {
        assert_eq!(LEFT.opening(None), px(280.));
        assert_eq!(LEFT.opening(Some(240.)), px(240.));
        assert_eq!(LEFT.opening(Some(120.)), px(180.));
        assert_eq!(RIGHT.opening(None), px(320.));
        assert_eq!(RIGHT.opening(Some(200.)), px(220.));
        assert_eq!(RIGHT.opening(Some(480.)), px(480.));
        assert_eq!(ASSISTANT.opening(None), px(420.));
        assert_eq!(ASSISTANT.opening(Some(200.)), px(220.));
    }

    #[test]
    fn the_default_width_is_saved_as_none() {
        assert_eq!(LEFT.saved(px(280.)), None);
        assert_eq!(LEFT.saved(px(279.8)), None);
        assert_eq!(LEFT.saved(px(240.4)), Some(240.));
        assert_eq!(RIGHT.saved(px(320.)), None);
        assert_eq!(RIGHT.saved(px(280.)), Some(280.));
        assert_eq!(ASSISTANT.saved(px(420.)), None);
        assert_eq!(ASSISTANT.saved(px(320.)), Some(320.));
    }
}
