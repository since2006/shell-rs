use std::{rc::Rc, sync::Arc};

use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaRenderer, DockContext, DockSkin, DropIndicator, NodeId,
    PanelState, TabGroupContext, TabGroupRenderer, TilesRenderer,
};
use gpui_kit::*;

/// The workspace's dock appearance: gpui-kit's `DockSkin` for the sidebars,
/// with no bar over the panels that draw their own: the center, whose tab
/// bar is its own, and 撰写窗格.
pub(super) struct WorkspaceDockSkin {
    skin: Rc<DockSkin>,
}

impl WorkspaceDockSkin {
    /// Build a `DockArea` wearing this skin.
    pub(super) fn dock_area(
        id: &'static str,
        version: Option<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<DockArea> {
        cx.new(|cx| {
            let skin = Rc::new(Self {
                skin: DockSkin::new(cx),
            });
            DockArea::new(id, version, window, cx).with_renderer(skin)
        })
    }
}

impl DockAreaRenderer for WorkspaceDockSkin {
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
        Rc::new(WorkspaceTabBar {
            skin: self.skin.tab_group_renderer(),
        })
    }

    fn tiles_renderer(&self) -> Rc<dyn TilesRenderer> {
        self.skin.tiles_renderer()
    }
}

struct WorkspaceTabBar {
    skin: Rc<dyn TabGroupRenderer>,
}

impl TabGroupRenderer for WorkspaceTabBar {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.skin.frame(group, window, cx)
    }
    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.skin.content_frame(group, window, cx)
    }
    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        if let [panel] = group.panels()
            && matches!(panel.panel_name(cx), "CenterView" | "ComposePane")
        {
            return Empty.into_any_element();
        }
        self.skin.render_tab_bar(group, window, cx)
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
    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
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
