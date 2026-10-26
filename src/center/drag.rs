//! What is dragged about the center, and what follows the pointer.

use std::{cell::RefCell, rc::Rc};

use gpui_kit::component::{ActiveTheme as _, Placement};
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

/// Where on a tab something dragged over it lands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabZone {
    /// A tab, moving along the bar: before this one.
    Before,
    /// Into this tab, along its right side.
    Into,
    /// A tab, moving along the bar: after this one.
    After,
}

/// The drops on the panes of the tab in front that would do something,
/// each a pane and the side it is dropped on (`None` for the middle).
/// Worked out by the view while something is dragged, and read by the
/// panes' skins, which show a drop only where it would land.
#[derive(Clone, Default)]
pub(super) struct Drops(Rc<RefCell<Vec<PaneDrop>>>);

/// A pane, and the side of it something is dropped on.
pub(super) type PaneDrop = (PaneContent, Option<Placement>);

impl Drops {
    pub(super) fn set(&self, drops: Vec<PaneDrop>) {
        *self.0.borrow_mut() = drops;
    }

    pub(super) fn allows(&self, pane: PaneContent, placement: Option<Placement>) -> bool {
        self.0.borrow().contains(&(pane, placement))
    }
}

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
