//! What is dragged about the center, and what follows the pointer.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::model::TabId;
use crate::app::PaneContent;

/// Dragged onto a pane's edge, it lands there; onto the tab bar, it moves
/// along it. Carried as the dock's `AnyDrag`, so a tab's dock area shows
/// where it would land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dragged {
    /// A tab of one pane, which can join another tab.
    Tab(TabId),
    /// A pane, by its header.
    Pane(PaneContent),
}

/// A tab that only moves along the tab bar: one of several panes, or 设置.
/// Not an `AnyDrag`, so no dock area takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TabAlong(pub(super) TabId);

/// What follows the pointer: the name of what is dragged.
pub(super) struct DragPreview {
    label: SharedString,
}

impl DragPreview {
    pub(super) fn new(label: SharedString) -> Self {
        Self { label }
    }
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .max_w(rems(20.))
            .whitespace_nowrap()
            .text_ellipsis()
            .text_sm()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .shadow_md()
            .child(self.label.clone())
    }
}
