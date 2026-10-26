//! How a tab's own dock area draws its panes: gpui-kit's `DockSkin`, but
//! with a pane header for a tab bar. Each pane is a group of its own, so a
//! group never shows tabs; while the tab holds one pane it shows no header
//! at all, the tab bar above naming the pane already.

use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui_kit::component::dock::{
    AnyDrag, BasePanelView, DockArea, DockAreaRenderer, DockContext, DockSkin, DropIndicator,
    NodeId, PanelState, TabGroupContext, TabGroupRenderer, TilesRenderer, ToggleZoom,
};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::drag::{DragPreview, Dragged, Drops};
use super::pane_controls::pane_controls;
use super::pane_view::PaneView;
use crate::i18n::t;

pub(super) struct PaneSkin {
    skin: Rc<DockSkin>,
    /// Whether the tab holds more than one pane, which the view keeps in
    /// step with the tab.
    headers: Rc<Cell<bool>>,
    /// Where a pane dragged by its header says it is dragged.
    dragging: Rc<Cell<Option<Dragged>>>,
    /// The drops that would do something, which alone show where they land.
    drops: Drops,
}

impl PaneSkin {
    /// A dock area for one tab, wearing this skin, and the switch for its
    /// pane headers.
    pub(super) fn dock_area(
        id: SharedString,
        dragging: Rc<Cell<Option<Dragged>>>,
        drops: Drops,
        window: &mut Window,
        cx: &mut App,
    ) -> (Entity<DockArea>, Rc<Cell<bool>>) {
        let headers = Rc::new(Cell::new(false));
        let area = cx.new(|cx| {
            let skin = Self {
                skin: DockSkin::new(cx),
                headers: headers.clone(),
                dragging,
                drops,
            };
            DockArea::new(id, None, window, cx).with_renderer(Rc::new(skin))
        });
        (area, headers)
    }
}

impl DockAreaRenderer for PaneSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.skin.frame(window, cx)
    }

    fn split_frame(
        &self,
        node: NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.skin.split_frame(node, axis, window, cx)
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.skin.center_frame(window, cx)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.skin.render_dock(dock, content, window, cx)
    }

    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        self.skin.build_placeholder(state, window, cx)
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(PaneHeader {
            skin: self.skin.tab_group_renderer(),
            headers: self.headers.clone(),
            dragging: self.dragging.clone(),
            drops: self.drops.clone(),
            refused: Cell::new(false),
        })
    }

    fn tiles_renderer(&self) -> Rc<dyn TilesRenderer> {
        self.skin.tiles_renderer()
    }
}

struct PaneHeader {
    skin: Rc<dyn TabGroupRenderer>,
    headers: Rc<Cell<bool>>,
    dragging: Rc<Cell<Option<Dragged>>>,
    drops: Drops,
    /// Whether the drop the group being drawn shows would do nothing: its
    /// content frame is asked first, then its drop indicator.
    refused: Cell<bool>,
}

impl TabGroupRenderer for PaneHeader {
    /// The frame, which carries 放大窗格 (`ToggleZoom`): in a tab of several
    /// panes, one fills the tab until it is zoomed out again. The skin's
    /// own frame would ask the panel whether it zooms through a handle the
    /// pane's `PaneView` hides, and refuse.
    fn frame(&self, group: &TabGroupContext, _: &mut Window, cx: &mut App) -> Stateful<Div> {
        let group = group.clone();
        let headers = self.headers.clone();
        div()
            .id("tab-panel")
            .bg(cx.theme().tokens.background)
            .on_action(move |_: &ToggleZoom, window, cx| {
                if headers.get() || group.is_zoomed() {
                    group.toggle_zoom(window, cx);
                }
            })
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        let pane = group
            .active_panel()
            .and_then(PaneView::of)
            .map(PaneView::pane);
        let refused = group.drop_indicator().is_some_and(|indicator| {
            pane.is_none_or(|pane| !self.drops.allows(pane, indicator.placement()))
        });
        self.refused.set(refused);
        self.skin.content_frame(group, window, cx)
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let Some(panel) = group.active_panel() else {
            return Empty.into_any_element();
        };
        if !self.headers.get() {
            return Empty.into_any_element();
        }
        let id = panel.panel_id(cx).as_u64();
        let Some(pane) = PaneView::of(panel) else {
            return Empty.into_any_element();
        };
        let title = pane.handle().title(window, cx);
        let label = pane.handle().tab_name(cx).unwrap_or_default();
        let dragging = self.dragging.clone();
        h_flex()
            .id(("pane-header", id))
            .test_support()
            // Dragged by its header, the pane goes beside another, or to a
            // tab of its own on the tab bar.
            .on_drag(
                AnyDrag::new(Dragged::Pane(pane.pane())),
                move |dragged: &AnyDrag, _, _, cx| {
                    dragging.set(dragged.value().downcast_ref::<Dragged>().copied());
                    cx.new(|_| DragPreview::new(label.clone()))
                },
            )
            .flex_shrink_0()
            .w_full()
            .h_8()
            .px_2()
            .bg(cx.theme().tokens.tab_bar)
            .border_b_1()
            .border_color(cx.theme().border)
            .text_sm()
            .gap_2()
            // A row, so the title keeps its own width, not the header's.
            .child(h_flex().flex_1().min_w_0().child(title))
            .child(pane_controls(pane.handle(), window, cx))
            // Zoomed, the pane says so, and how to put the others back.
            .when(group.is_zoomed(), |header| {
                let group = group.clone();
                header.child(
                    Button::new(("restore-pane", id))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Minimize)
                        .tooltip(t!("center.pane.restore"))
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            group.toggle_zoom(window, cx);
                        }),
                )
            })
            .into_any_element()
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.skin.render_active_panel(panel, group, window, cx)
    }

    /// Only a drop that would do something shows where it lands.
    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        if self.refused.take() {
            return None;
        }
        self.skin.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.skin.render_empty(group, window, cx)
    }
}
