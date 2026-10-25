//! Where each pane's tab stands in the tab bar, for the close commands of a
//! pane's menu: a command that would close nothing shows disabled. The menu
//! is built by the pane, which knows nothing of the tab bar, so the view
//! publishes the places here whenever they change.

use std::collections::HashMap;

use gpui_kit::*;

use crate::app::PaneContent;

/// A pane's place: how many tabs there are and where its tab is among
/// them, how many panes its tab holds, and whether it fills the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabPlace {
    pub count: usize,
    pub ix: usize,
    pub panes: usize,
    pub zoomed: bool,
}

#[derive(Default)]
pub(super) struct TabPlaces(pub(super) HashMap<PaneContent, TabPlace>);

impl Global for TabPlaces {}

/// The place of the tab `pane` is in, while it is open.
pub fn tab_place(pane: PaneContent, cx: &App) -> Option<TabPlace> {
    cx.try_global::<TabPlaces>()?.0.get(&pane).copied()
}
