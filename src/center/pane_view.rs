//! A pane as its tab's dock area holds it.

use std::{
    any::Any,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui_kit::component::dock::{BasePanelView, PanelHandle, PanelId, PanelState, TabGroup};
use gpui_kit::*;

use crate::app::PaneContent;

/// A pane's handle, which passes the dock's word that the pane is active on
/// only while its tab is in front.
///
/// The dock tells a pane it is active a frame after it is added or shown,
/// and the pane takes the focus then. A tab behind is neither drawn nor
/// told it went behind by its dock area, so without the gate a pane opened
/// into a tab behind, or added while its tab went behind, would take the
/// focus off screen, where no action reaches the workspace.
#[derive(Clone)]
pub(super) struct PaneView {
    pane: PaneContent,
    handle: PanelHandle,
    /// Whether the pane's tab is in front, shared by the tab's panes.
    in_front: Arc<AtomicBool>,
}

impl PaneView {
    pub(super) fn new(pane: PaneContent, handle: PanelHandle, in_front: Arc<AtomicBool>) -> Self {
        Self {
            pane,
            handle,
            in_front,
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
        if !active || self.in_front.load(Ordering::Relaxed) {
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
