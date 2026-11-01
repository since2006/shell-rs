use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaRenderer, DockContext, DockSkin, DropIndicator, NodeId,
    PanelState, PanelStyle, TabGroupContext, TabGroupRenderer, TilesRenderer,
};
use gpui_kit::component::v_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// The workspace's dock appearance: gpui-kit's `DockSkin`, plus a view that
/// fills the center while it holds no tab.
///
/// The dock draws nothing for an empty center and `DockAreaRenderer` has no
/// empty-center hook, so the placeholder rides on `center_frame`. It is drawn
/// deferred, above the empty layout the dock paints there, whenever the
/// workspace last said the center is empty (`set_center_empty`); the
/// workspace keeps that in step with `DockEvent::LayoutChanged`.
pub(super) struct WorkspaceDockSkin {
    skin: Rc<DockSkin>,
    // The center's bars: always tabs, so a lone tab looks selected as it
    // does among others, while the sidebars keep their plain title. Center
    // bars hide the sidebar toggle, which the workspace title bar provides.
    center_skin: Rc<DockSkin>,
    /// The center's tab groups, which the workspace keeps in step with the
    /// layout (`set_center_groups`).
    center_groups: Rc<RefCell<Vec<NodeId>>>,
    area: WeakEntity<DockArea>,
    placeholder: AnyView,
    center_empty: Cell<bool>,
}

impl WorkspaceDockSkin {
    /// Build a `DockArea` wearing this skin, together with the handle its
    /// settings are changed through. `placeholder` is what the center shows
    /// while it is empty.
    pub(super) fn dock_area(
        id: &'static str,
        version: Option<usize>,
        placeholder: AnyView,
        window: &mut Window,
        cx: &mut App,
    ) -> (Entity<DockArea>, Rc<Self>) {
        let mut skin = None;
        let area = cx.new(|cx| {
            let this = Rc::new(Self {
                skin: DockSkin::new(cx),
                center_skin: DockSkin::new(cx),
                center_groups: Rc::default(),
                area: cx.weak_entity(),
                placeholder,
                center_empty: Cell::new(false),
            });
            skin = Some(this.clone());
            DockArea::new(id, version, window, cx).with_renderer(this)
        });
        // The closure above runs before `cx.new` returns.
        let skin = skin.expect("the skin is built inside the constructor");
        skin.center_skin.set_toggle_button_visible(false, cx);
        skin.center_skin.set_panel_style(PanelStyle::TabBar, cx);
        (area, skin)
    }

    pub(super) fn is_center_empty(&self) -> bool {
        self.center_empty.get()
    }

    pub(super) fn set_center_groups(&self, groups: Vec<NodeId>, cx: &mut App) {
        if *self.center_groups.borrow() != groups {
            self.center_groups.replace(groups);
            _ = self.area.update(cx, |_, cx| cx.notify());
        }
    }

    /// Show (`true`) or hide the placeholder. The skin is not an entity, so
    /// it redraws the area itself when the value changes.
    pub(super) fn set_center_empty(&self, empty: bool, cx: &mut App) {
        if self.center_empty.replace(empty) != empty {
            _ = self.area.update(cx, |_, cx| cx.notify());
        }
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
        self.skin
            .center_frame(window, cx)
            .relative()
            .when(self.center_empty.get(), |frame| {
                // Deferred, so it paints after the empty layout's own
                // background, which the dock draws later in the same frame.
                frame.child(deferred(
                    div().absolute().inset_0().child(self.placeholder.clone()),
                ))
            })
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
            center_skin: self.center_skin.tab_group_renderer(),
            center_groups: self.center_groups.clone(),
        })
    }

    fn tiles_renderer(&self) -> Rc<dyn TilesRenderer> {
        self.skin.tiles_renderer()
    }
}

/// A center tab bar, in a column as base lays the bars out, that a mouse
/// wheel scrolls sideways (#66).
///
/// The bar scrolls only along itself, dropping vertical deltas, while a
/// mouse wheel turns only vertically: tabs scrolled out of view could be
/// reached only with Shift held. A vertical turn is sent again as the
/// sideways one Shift makes. Only a wheel's, which comes in lines; a
/// trackpad swipes sideways itself.
fn wheel_scrolls_tabs(tabs: AnyElement) -> Div {
    v_flex()
        .on_scroll_wheel(|event, window, cx| {
            let ScrollDelta::Lines(lines) = event.delta else {
                return;
            };
            if lines.x != 0. || lines.y == 0. {
                return;
            }
            cx.stop_propagation();
            let sideways = ScrollWheelEvent {
                delta: ScrollDelta::Lines(point(lines.y, 0.)),
                ..event.clone()
            };
            window.defer(cx, move |window, cx| {
                window.dispatch_event(sideways.to_platform_input(), cx);
            });
        })
        .child(tabs)
}

/// Preserve the center tab strip's scrolling and native Dock behavior.
/// Dock's renderer still owns tabs, dragging, overflow, menus and focus.
struct WorkspaceTabBar {
    skin: Rc<dyn TabGroupRenderer>,
    center_skin: Rc<dyn TabGroupRenderer>,
    center_groups: Rc<RefCell<Vec<NodeId>>>,
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
        // 撰写窗格 draws its own title bar.
        if let [panel] = group.panels()
            && panel.panel_name(cx) == "ComposePane"
        {
            return Empty.into_any_element();
        }
        if self.center_groups.borrow().contains(&group.node()) {
            wheel_scrolls_tabs(self.center_skin.render_tab_bar(group, window, cx))
                .id(
                    if self.center_groups.borrow().first() == Some(&group.node()) {
                        ElementId::Name("center-tab-bar".into())
                    } else {
                        ("center-tab-bar", group.node().as_u64()).into()
                    },
                )
                .test_support()
                .into_any_element()
        } else {
            self.skin.render_tab_bar(group, window, cx)
        }
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
