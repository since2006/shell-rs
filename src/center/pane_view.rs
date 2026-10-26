//! A pane as its tab's dock area holds it.

use std::{
    any::Any,
    sync::{Arc, Mutex},
};

use gpui_kit::component::dock::{BasePanelView, PanelHandle, PanelId, PanelState, TabGroup};
use gpui_kit::*;

use crate::app::PaneContent;

/// Which pane of a tab, if any, the dock's next word that a pane is active
/// reaches: a pane just opened in a tab of its own. Shared by the tab's
/// panes; the word reaches the pane once.
#[derive(Clone, Default)]
pub(super) struct Activation(Arc<Mutex<Option<PaneContent>>>);

impl Activation {
    /// Let the word through to `pane`, or with `None` to no pane.
    pub(super) fn grant(&self, pane: Option<PaneContent>) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = pane;
    }

    /// Whether the word reaches `pane`, which uses it up.
    fn take(&self, pane: PaneContent) -> bool {
        let mut granted = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *granted == Some(pane) {
            *granted = None;
            return true;
        }
        false
    }
}

/// A pane's handle, which passes the dock's word that the pane is active on
/// only where the view granted it.
///
/// The dock tells a pane it is active a frame after it is added, and the
/// pane takes the focus then. That is right for a pane opened in a new tab
/// in front, and wrong everywhere else: a tab behind is neither drawn nor
/// told it went behind by its dock area, so a pane added while its tab went
/// behind would take the focus off screen, where no action reaches the
/// workspace; and a tab laid out anew tells every one of its panes at once.
/// The view gives the focus itself when a tab comes forward or a pane
/// moves.
#[derive(Clone)]
pub(super) struct PaneView {
    pane: PaneContent,
    handle: PanelHandle,
    activation: Activation,
}

impl PaneView {
    pub(super) fn new(pane: PaneContent, handle: PanelHandle, activation: Activation) -> Self {
        Self {
            pane,
            handle,
            activation,
        }
    }

    pub(super) fn pane(&self) -> PaneContent {
        self.pane
    }

    /// Recover it from the dock's handle.
    pub(super) fn of(panel: &Arc<dyn BasePanelView>) -> Option<&Self> {
        panel.as_any().downcast_ref::<Self>()
    }

    pub(super) fn handle(&self) -> &PanelHandle {
        &self.handle
    }
}

impl BasePanelView for PaneView {
    fn panel_name(&self, cx: &App) -> &'static str {
        self.handle.panel_name(cx)
    }

    fn panel_id(&self, cx: &App) -> PanelId {
        self.handle.panel_id(cx)
    }

    fn closable(&self, cx: &App) -> bool {
        self.handle.closable(cx)
    }

    fn zoomable(&self, cx: &App) -> bool {
        self.handle.zoomable(cx)
    }

    fn visible(&self, cx: &App) -> bool {
        self.handle.visible(cx)
    }

    fn set_active(&self, active: bool, window: &mut Window, cx: &mut App) {
        if !active || self.activation.take(self.pane) {
            self.handle.set_active(active, window, cx);
        }
    }

    fn set_zoomed(&self, zoomed: bool, window: &mut Window, cx: &mut App) {
        self.handle.set_zoomed(zoomed, window, cx);
    }

    fn on_added_to(&self, group: WeakEntity<TabGroup>, window: &mut Window, cx: &mut App) {
        self.handle.on_added_to(group, window, cx);
    }

    fn on_removed(&self, window: &mut Window, cx: &mut App) {
        self.handle.on_removed(window, cx);
    }

    fn view(&self) -> AnyView {
        self.handle.view()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.handle.focus_handle(cx)
    }

    fn dump(&self, cx: &App) -> PanelState {
        self.handle.dump(cx)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
