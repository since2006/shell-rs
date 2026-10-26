//! The center: a tab bar, and under it the tab in front, whose panes its
//! own dock area lays out.
//!
//! The view owns which tabs there are and how each lays its panes out; the
//! workspace owns the panes themselves, opens them here and takes them out
//! when they close. The focus says which pane is in front: a pane that
//! comes forward is told `set_active(true)` and takes the focus, which is
//! how the workspace hears of it, and the panes of a tab that goes behind
//! are told `set_active(false)`, which their own dock area never tells
//! them.

use std::{cell::Cell, collections::HashMap, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::component::dock::{
    AnyDrag, BasePanel, BasePanelView, DockArea, DockEvent, DockLayout, DockPlacement, NodeId,
    PaneNode, PaneRef, Panel, PanelEvent, PanelHandle, PanelId, PanelInfo, PanelState,
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Placement, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{DropdownMenu as _, PopupMenu},
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::*;

use super::drag::{DragPreview, Dragged, Drops, TabAlong, TabZone};
use super::layout::Layout;
use super::model::{PaneKind as _, TabId, TabTitle, Tabs};
use super::pane_controls::pane_controls;
use super::pane_skin::PaneSkin;
use super::pane_view::{Activation, PaneView};
use super::places::tab_place;
use super::places::{TabPlace, TabPlaces};
use super::tab_bar::{LeadingControls, wheel_scrolls_tabs};
use crate::app::{
    ActivatePane, ActivateTab, CatalogIcon, ChooseTabToJoin, CloseScope, CloseTab, CloseTabs,
    EqualizePanes, PaneContent, RenameTab,
};
use crate::i18n::t;
use crate::shared::ClosableTabTitle;

mod drops;

pub enum CenterEvent {
    /// Tabs opened, closed, moved or brought to the front, or a tab's panes
    /// laid out anew.
    Changed,
}

/// A tab as 所有标签 lists it, taken when the tab bar is drawn.
struct TabRow {
    tab: TabId,
    label: SharedString,
    in_front: bool,
    /// For a tab of several panes, each pane and its name.
    panes: Vec<(PaneContent, SharedString)>,
}

type TakeOut = Rc<dyn Fn(&mut DockArea, &mut Window, &mut Context<DockArea>)>;

/// One tab's dock area.
struct TabArea {
    area: Entity<DockArea>,
    /// Whether its panes show a header: while it holds more than one.
    headers: Rc<Cell<bool>>,
    /// Which pane its dock area's word that a pane is active reaches (see
    /// `PaneView`).
    activation: Activation,
    _subscriptions: [Subscription; 2],
}

pub struct CenterView {
    tabs: Tabs<PaneContent>,
    areas: HashMap<TabId, TabArea>,
    panes: HashMap<PaneContent, PanelHandle>,
    /// How each pane is taken out of a dock area, which wants it by type.
    take_outs: HashMap<PaneContent, TakeOut>,
    scroll: ScrollHandle,
    /// The tab last brought into view in the tab bar.
    scrolled_to: Option<TabId>,
    /// What fills the center while it holds no tab: the start page.
    placeholder: AnyView,
    /// Where the tab bar's controls dispatch to.
    target: FocusHandle,
    controls: LeadingControls,
    /// While something is dragged over another tab: that tab, and the wait
    /// before it comes forward to take what is dragged.
    hover: Option<(TabId, Task<()>)>,
    /// What is being dragged, set as the drag starts; stale once no drag
    /// is on.
    dragging: Rc<Cell<Option<Dragged>>>,
    /// Where on a tab what is dragged would land, while it is over one.
    tab_zone: Option<(TabId, TabZone)>,
    /// Which drops on the panes of the tab in front would do something,
    /// for their skins to show only those.
    drops: Drops,
    /// 选择标签: the tabs to put beside a pane, at a place in the window.
    chooser: Option<Chooser>,
    focus_handle: FocusHandle,
}

/// The menu of tabs that can join a tab, open where it was asked for.
struct Chooser {
    menu: Entity<PopupMenu>,
    position: Point<Pixels>,
    _subscription: Subscription,
}

/// Where in a tab a pane lands.
#[derive(Clone, Copy, Debug)]
enum Spot<P> {
    /// On a side of one of its panes.
    Beside(P, Placement),
    /// Along a whole side of the tab.
    Edge(Placement),
}

/// Put `pane` in `layout` at `spot`, `pane` being in it already when it
/// moves `within`. Returns whether it landed.
fn land<P: Copy + Eq + std::fmt::Debug>(
    layout: &mut Layout<P>,
    pane: P,
    spot: Spot<P>,
    within: bool,
) -> bool {
    match spot {
        Spot::Beside(onto, side) if within => layout.move_beside(pane, onto, side),
        Spot::Beside(onto, side) => layout.insert(pane, onto, side),
        Spot::Edge(side) if within => {
            let before = layout.clone();
            if layout.remove(pane) && layout.insert_at_edge(pane, side) {
                return true;
            }
            *layout = before;
            false
        }
        Spot::Edge(side) => layout.insert_at_edge(pane, side),
    }
}

/// How long something dragged over a tab waits before that tab comes
/// forward.
const HOVER_SWITCH: Duration = Duration::from_millis(500);

/// How small a pane may become, in pixels: about 20 columns by 5 lines of a
/// terminal. A drop that would leave a pane smaller is not taken.
const MIN_PANE_WIDTH: f32 = 200.;
const MIN_PANE_HEIGHT: f32 = 100.;

impl EventEmitter<CenterEvent> for CenterView {}

impl CenterView {
    pub fn new(placeholder: AnyView, target: FocusHandle, cx: &mut Context<Self>) -> Self {
        Self {
            tabs: Tabs::default(),
            areas: HashMap::new(),
            panes: HashMap::new(),
            take_outs: HashMap::new(),
            scroll: ScrollHandle::new(),
            scrolled_to: None,
            placeholder,
            target,
            controls: LeadingControls::default(),
            hover: None,
            dragging: Rc::default(),
            tab_zone: None,
            drops: Drops::default(),
            chooser: None,
            focus_handle: cx.focus_handle(),
        }
    }

    // Reading.

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn active_tab(&self) -> Option<TabId> {
        self.tabs.active()
    }

    pub fn tab_of(&self, pane: PaneContent) -> Option<TabId> {
        self.tabs.tab_of(pane)
    }

    /// The tabs in order.
    pub fn tab_ids(&self) -> Vec<TabId> {
        self.tabs.ids().collect()
    }

    pub fn panes(&self, tab: TabId) -> &[PaneContent] {
        self.tabs.panes(tab)
    }

    /// Every pane, tab by tab, each tab's in reading order.
    pub fn all_panes(&self) -> Vec<PaneContent> {
        self.tabs.all_panes().collect()
    }

    /// The current pane of the tab in front.
    pub fn current_pane(&self) -> Option<PaneContent> {
        self.tabs.current_pane()
    }

    /// The panes on screen: the tab in front's, or only its zoomed one.
    pub fn visible_panes(&self, cx: &App) -> Vec<PaneContent> {
        let Some(front) = self.tabs.active() else {
            return Vec::new();
        };
        let panes = self.tabs.panes(front).to_vec();
        let Some(area) = self.areas.get(&front).map(|tab| tab.area.read(cx)) else {
            return panes;
        };
        let Some(zoomed) = area.zoomed_group() else {
            return panes;
        };
        let mut shown = Vec::new();
        if let Some(tree) = area.layout(DockPlacement::Center) {
            tree.root().walk(&mut |node| {
                if node.id() == zoomed
                    && let PaneRef::Tabs { panels, .. } = node.kind()
                {
                    shown.extend(panels.iter().copied());
                }
            });
        }
        shown
            .into_iter()
            .filter_map(|panel| self.pane_of(panel, cx))
            .collect()
    }

    /// The tabs a close command of `pane`'s tab takes.
    pub fn tabs_in_scope(&self, pane: PaneContent, scope: CloseScope) -> Vec<TabId> {
        let ids = self.tab_ids();
        let Some(ix) = self
            .tabs
            .tab_of(pane)
            .and_then(|tab| ids.iter().position(|id| *id == tab))
        else {
            return Vec::new();
        };
        scope
            .targets(ids.len(), ix)
            .into_iter()
            .map(|target| ids[target])
            .collect()
    }

    // Editing.

    /// Open `pane` in a tab of its own, last, in front.
    pub fn open<P: Panel>(
        &mut self,
        pane: PaneContent,
        panel: Entity<P>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = self.tabs.active() {
            self.hide(previous, window, cx);
        }
        let tab = self.tabs.open(pane);
        let entity = panel.clone();
        self.take_outs.insert(
            pane,
            Rc::new(move |area: &mut DockArea, window: &mut Window, cx| {
                area.remove_panel(entity.clone(), window, cx)
            }),
        );
        self.panes.insert(pane, PanelHandle::new(panel));
        self.new_area(tab, window, cx);
        // 设置 never shares its tab: dragged over it, nothing lands.
        if pane.stands_alone()
            && let Some(area) = self.areas.get(&tab)
        {
            area.area
                .update(cx, |area, cx| area.set_locked(true, window, cx));
        }
        self.put_in(pane, tab, window, cx);
        self.changed(cx);
    }

    /// Take a pane out that has been closed. Its tab goes with its last
    /// pane, and the tab or pane that comes forward in its place takes the
    /// focus.
    pub fn remove(&mut self, pane: PaneContent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab_of(pane) else {
            return;
        };
        let was_front = self.tabs.active() == Some(tab);
        let rest = self.layout_without(pane, cx);
        self.take_out(pane, tab, window, cx);
        self.panes.remove(&pane);
        self.take_outs.remove(&pane);
        cx.default_global::<TabPlaces>().0.remove(&pane);
        if self.tabs.close_pane(pane).is_some() {
            self.areas.remove(&tab);
        } else if let Some(rest) = rest {
            self.lay_out(tab, &rest, window, cx);
        }
        if was_front && let Some(front) = self.tabs.active() {
            self.show(front, window, cx);
        }
        self.changed(cx);
    }

    /// Put `pane` beside `onto`, on its `placement` side, in `onto`'s tab:
    /// another place in the same tab, or out of its own into that one. Its
    /// own tab goes with its last pane. Returns whether it moved.
    pub fn move_pane(
        &mut self,
        pane: PaneContent,
        onto: PaneContent,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(into) = self.tabs.tab_of(onto) else {
            return false;
        };
        self.place(pane, into, Spot::Beside(onto, placement), window, cx)
    }

    /// Drop the one pane of tab `from` beside `onto`, in `onto`'s tab.
    /// Only a tab of one pane, never 设置, goes into another.
    pub fn merge_tab(
        &mut self,
        from: TabId,
        onto: PaneContent,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(into) = self.tabs.tab_of(onto) else {
            return false;
        };
        if !self.tabs.can_merge(from, into) {
            return false;
        }
        let pane = self.tabs.panes(from)[0];
        self.move_pane(pane, onto, placement, window, cx)
    }

    /// Put `pane` along the whole `placement` side of `tab`. Returns
    /// whether it moved.
    pub fn join_tab(
        &mut self,
        pane: PaneContent,
        tab: TabId,
        placement: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.place(pane, tab, Spot::Edge(placement), window, cx)
    }

    /// Two panes of a tab trade places. Returns whether they did.
    pub fn swap_panes(
        &mut self,
        pane: PaneContent,
        with: PaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.tabs.tab_of(pane) else {
            return false;
        };
        if self.tabs.tab_of(with) != Some(tab) {
            return false;
        }
        let Some(mut layout) = self.layout_of(tab, cx) else {
            return false;
        };
        if !layout.swap(pane, with) {
            return false;
        }
        self.lay_out(tab, &layout, window, cx);
        self.tabs.focus(pane);
        self.show(tab, window, cx);
        self.changed(cx);
        true
    }

    /// 均分窗格: every row and column of the tab shares its space equally.
    pub fn equalize(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.panes(tab).len() < 2 {
            return;
        }
        if let Some(mut layout) = self.layout_of(tab, cx) {
            layout.equalize();
            self.lay_out(tab, &layout, window, cx);
            if self.tabs.active() == Some(tab) {
                self.show(tab, window, cx);
            }
            self.changed(cx);
        }
    }

    /// Give `pane` a tab of its own at `ix` among the tabs, in front.
    pub fn split_off(
        &mut self,
        pane: PaneContent,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.tabs.tab_of(pane) else {
            return;
        };
        if self.tabs.panes(from).len() == 1 {
            self.move_tab(from, ix, cx);
            self.activate_tab(from, window, cx);
            return;
        }
        if let Some(previous) = self.tabs.active() {
            self.hide(previous, window, cx);
        }
        let rest = self.layout_without(pane, cx);
        self.take_out(pane, from, window, cx);
        let Some(tab) = self.tabs.split_off(pane, ix) else {
            return;
        };
        if let Some(rest) = rest {
            self.lay_out(from, &rest, window, cx);
        }
        self.new_area(tab, window, cx);
        self.put_in(pane, tab, window, cx);
        self.show(tab, window, cx);
        self.changed(cx);
    }

    /// Move a tab to `ix` among the tabs.
    pub fn move_tab(&mut self, tab: TabId, ix: usize, cx: &mut Context<Self>) {
        self.tabs.move_tab(tab, ix);
        self.changed(cx);
    }

    /// Bring a tab to the front, its current pane taking the focus.
    pub fn activate_tab(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.active() == Some(tab) || !self.areas.contains_key(&tab) {
            return;
        }
        if let Some(previous) = self.tabs.active() {
            self.hide(previous, window, cx);
        }
        self.tabs.activate(tab);
        self.show(tab, window, cx);
        self.changed(cx);
    }

    /// Bring a pane to the front, its tab with it, and give it the focus.
    pub fn activate_pane(
        &mut self,
        pane: PaneContent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.tab_of(pane) else {
            return;
        };
        if let Some(previous) = self.tabs.active().filter(|previous| *previous != tab) {
            self.hide(previous, window, cx);
        }
        self.tabs.focus(pane);
        self.show(tab, window, cx);
        self.changed(cx);
    }

    /// The focus went into `pane`: it is its tab's current pane now.
    pub fn focus_entered(&mut self, pane: PaneContent, cx: &mut Context<Self>) {
        if self.tabs.current_pane() == Some(pane) {
            return;
        }
        self.tabs.focus(pane);
        cx.notify();
    }

    /// The tab `step` places from the one in front, around the ends.
    pub fn step(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.tab_ids();
        let Some(ix) = self
            .tabs
            .active()
            .and_then(|front| ids.iter().position(|id| *id == front))
        else {
            return;
        };
        if ids.len() > 1 {
            let next = (ix as isize + step).rem_euclid(ids.len() as isize) as usize;
            self.activate_tab(ids[next], window, cx);
        }
    }

    /// The tab at `position`, from 1; 9 is the last, as in browsers. A
    /// position past the last does nothing.
    pub fn switch_to(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.tab_ids();
        let ix = if position >= 9 {
            ids.len().checked_sub(1)
        } else {
            position.checked_sub(1).filter(|ix| *ix < ids.len())
        };
        if let Some(ix) = ix {
            self.activate_tab(ids[ix], window, cx);
        }
    }

    /// 放大窗格: fill the pane's tab with it, or put the tab's layout back.
    /// A tab of one pane has nothing to zoom.
    pub fn toggle_zoom(&mut self, pane: PaneContent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab_of(pane) else {
            return;
        };
        let zoomed = self.zoomed_pane(tab, cx);
        let several = self.tabs.panes(tab).len() > 1;
        let node = self.node_of(pane, cx);
        let Some(area) = self.areas.get(&tab).map(|area| area.area.clone()) else {
            return;
        };
        area.update(cx, |area, cx| {
            if zoomed.is_some() {
                area.set_zoomed_out(window, cx);
            }
            if zoomed != Some(pane)
                && several
                && let Some(node) = node
            {
                area.set_zoomed_in(node, window, cx);
            }
        });
        self.activate_pane(pane, window, cx);
    }

    /// Name a tab by hand, or with `None` after its panes again.
    pub fn rename_tab(&mut self, tab: TabId, name: Option<SharedString>, cx: &mut Context<Self>) {
        self.tabs.rename(tab, name);
        self.changed(cx);
    }

    /// What a tab is named after its panes, whatever it is named by hand.
    pub fn panes_label(&self, tab: TabId, cx: &App) -> SharedString {
        self.tabs
            .panes(tab)
            .iter()
            .filter_map(|pane| self.panes.get(pane))
            .map(|panel| {
                panel
                    .tab_name(cx)
                    .unwrap_or_else(|| panel.panel_name(cx).into())
            })
            .reduce(|names, name| t!("center.tab.joined_names", names = names, name = name))
            .unwrap_or_default()
    }

    pub fn set_controls(&mut self, controls: LeadingControls, cx: &mut Context<Self>) {
        if self.controls != controls {
            self.controls = controls;
            cx.notify();
        }
    }

    // Inside.

    /// A dock area for a new tab, in front.
    fn new_area(&mut self, tab: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let (area, headers) = PaneSkin::dock_area(
            format!("tab-{}", tab.0).into(),
            self.dragging.clone(),
            self.drops.clone(),
            window,
            cx,
        );
        let subscriptions = [
            cx.subscribe_in(
                &area,
                window,
                move |this, _, event: &DockEvent, window, cx| match event {
                    DockEvent::LayoutChanged => this.on_layout_changed(tab, cx),
                    DockEvent::DragDrop { item, target } => {
                        this.on_dropped_on_pane(tab, item, target, window, cx)
                    }
                },
            ),
            // A pane zoomed or zoomed out only redraws its area.
            cx.observe(&area, |this, _, cx| this.publish_places(cx)),
        ];
        self.areas.insert(
            tab,
            TabArea {
                area,
                headers,
                activation: Activation::default(),
                _subscriptions: subscriptions,
            },
        );
    }

    /// Hand a command to the workspace, once this update is over: its
    /// handler updates the center.
    fn dispatch(&self, action: Box<dyn Action>, window: &mut Window, cx: &mut App) {
        let target = self.target.clone();
        window.defer(cx, move |window, cx| {
            target.dispatch_action(&*action, window, cx)
        });
    }

    fn position(&self, tab: TabId) -> Option<usize> {
        self.tabs.ids().position(|candidate| candidate == tab)
    }

    /// What a tab is called, as text: by hand, or after its panes.
    pub fn tab_label(&self, tab: TabId, cx: &App) -> SharedString {
        match self.tabs.title(tab) {
            Some(TabTitle::Named(name)) => name,
            _ => self.panes_label(tab, cx),
        }
    }

    /// Put a pane alone into a new tab's dock area. Adding tells the pane
    /// it is active, which gives it the focus while the tab is in front.
    fn put_in(
        &mut self,
        pane: PaneContent,
        tab: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(panel), Some(area)) = (self.panes.get(&pane).cloned(), self.areas.get(&tab))
        else {
            return;
        };
        area.activation.grant(Some(pane));
        let view = Arc::new(PaneView::new(pane, panel, area.activation.clone()));
        area.area.clone().update(cx, |area, cx| {
            area.add_panel_view(view, DockPlacement::Center, None, window, cx);
        });
    }

    /// Take a pane out of its tab's dock area, without closing it.
    fn take_out(&mut self, pane: PaneContent, tab: TabId, window: &mut Window, cx: &mut App) {
        let (Some(take_out), Some(area)) =
            (self.take_outs.get(&pane).cloned(), self.areas.get(&tab))
        else {
            return;
        };
        area.area.update(cx, |area, cx| {
            // A zoomed pane leaving would leave its tab showing nothing.
            if area.is_zoomed() {
                area.set_zoomed_out(window, cx);
            }
            take_out(area, window, cx);
        });
    }

    /// The pane a group of `tab`'s dock area shows.
    fn pane_in_group(&self, tab: TabId, node: NodeId, cx: &App) -> Option<PaneContent> {
        let area = self.areas.get(&tab)?.area.read(cx);
        let mut panel = None;
        area.layout(DockPlacement::Center)?
            .root()
            .walk(&mut |candidate| {
                if candidate.id() == node
                    && let PaneRef::Tabs { panels, .. } = candidate.kind()
                {
                    panel = panels.first().copied();
                }
            });
        self.pane_of(panel?, cx)
    }

    /// The group of its tab's dock area that shows `pane`.
    fn node_of(&self, pane: PaneContent, cx: &App) -> Option<NodeId> {
        let tab = self.tabs.tab_of(pane)?;
        let panel = self.panes.get(&pane)?.panel_id(cx);
        self.areas
            .get(&tab)?
            .area
            .read(cx)
            .layout(DockPlacement::Center)?
            .find_panel_node(panel)
    }

    /// Put `pane` at `spot` in tab `into`: elsewhere in its own tab, or out
    /// of its tab into another, whose layout it then shares. Its own tab
    /// goes with its last pane. Returns whether it moved.
    fn place(
        &mut self,
        pane: PaneContent,
        into: TabId,
        spot: Spot<PaneContent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(from) = self.tabs.tab_of(pane) else {
            return false;
        };
        if from != into && !self.tabs.can_move_into(pane, into) {
            return false;
        }
        let Some(target) = self
            .layout_of(into, cx)
            .and_then(|mut layout| land(&mut layout, pane, spot, from == into).then_some(layout))
        else {
            return false;
        };
        // Nowhere a pane would be too small to use.
        if !self.fits(&target, cx) {
            return false;
        }
        if from == into {
            self.lay_out(into, &target, window, cx);
            self.tabs.focus(pane);
            self.show(into, window, cx);
            self.changed(cx);
            return true;
        }
        if let Some(previous) = self.tabs.active().filter(|previous| *previous != into) {
            self.hide(previous, window, cx);
        }
        let rest = self.layout_without(pane, cx);
        self.take_out(pane, from, window, cx);
        self.tabs.move_pane(pane, into);
        if self.tabs.panes(from).is_empty() {
            self.areas.remove(&from);
        } else if let Some(rest) = rest {
            self.lay_out(from, &rest, window, cx);
        }
        self.lay_out(into, &target, window, cx);
        self.show(into, window, cx);
        self.changed(cx);
        true
    }

    /// How `tab`'s panes are laid out now, at the sizes on screen.
    fn layout_of(&self, tab: TabId, cx: &App) -> Option<Layout<PaneContent>> {
        let area = self.areas.get(&tab)?.area.read(cx);
        let tree = area.layout(DockPlacement::Center)?;
        self.read_layout(tree.root(), &area.dump(cx).center, cx)
    }

    /// The layout of `pane`'s tab once `pane` has left it; `None` when it
    /// is the tab's last pane.
    fn layout_without(&self, pane: PaneContent, cx: &App) -> Option<Layout<PaneContent>> {
        let mut layout = self.layout_of(self.tabs.tab_of(pane)?, cx)?;
        layout.remove(pane).then_some(layout)
    }

    /// A node of a dock area's tree as a layout, its sizes read from the
    /// area's account of itself, which has the sizes on screen.
    fn read_layout(
        &self,
        node: &PaneNode,
        state: &PanelState,
        cx: &App,
    ) -> Option<Layout<PaneContent>> {
        match node.kind() {
            PaneRef::Tabs { panels, .. } => self.pane_of(*panels.first()?, cx).map(Layout::Pane),
            PaneRef::Split { axis, children, .. } => {
                let measured = match &state.info {
                    PanelInfo::Stack { sizes, .. } => sizes
                        .iter()
                        .map(|size| f32::from(*size))
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                let measured = (measured.len() == children.len()
                    && measured.iter().all(|size| *size > 0.))
                .then_some(measured);
                let children: Vec<_> = children
                    .iter()
                    .zip(&state.children)
                    .enumerate()
                    .filter_map(|(ix, (child, child_state))| {
                        let share = measured.as_ref().map_or(1., |sizes| sizes[ix]);
                        Some((self.read_layout(child, child_state, cx)?, share))
                    })
                    .collect();
                (!children.is_empty()).then(|| Layout::split(axis, children))
            }
            PaneRef::Tiles { .. } => None,
        }
    }

    /// Lay `tab`'s panes out as `layout` says, each at its size. The dock
    /// area takes the layout whole, so no pane's size is left to it.
    fn lay_out(
        &mut self,
        tab: TabId,
        layout: &Layout<PaneContent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(area) = self.areas.get(&tab) else {
            return;
        };
        // The panes laid out anew are told they are active; the view gives
        // the focus itself.
        area.activation.grant(None);
        let (width, height) = self.center_size(cx);
        let described = self.describe(layout, width, height, &area.activation, cx);
        area.area.clone().update(cx, |area, cx| {
            if area.is_zoomed() {
                area.set_zoomed_out(window, cx);
            }
            area.set_center(described, window, cx);
        });
    }

    /// `layout` as the dock area takes it, `width` by `height`.
    fn describe(
        &self,
        layout: &Layout<PaneContent>,
        width: f32,
        height: f32,
        activation: &Activation,
        cx: &App,
    ) -> DockLayout {
        match layout {
            Layout::Pane(pane) => {
                let group = DockLayout::tabs();
                match self.panes.get(pane) {
                    Some(handle) => {
                        let view: Arc<dyn BasePanelView> =
                            Arc::new(PaneView::new(*pane, handle.clone(), activation.clone()));
                        group.panel_view(view, cx)
                    }
                    None => group,
                }
            }
            Layout::Split { axis, children } => {
                let mut split = match axis {
                    Axis::Horizontal => DockLayout::h_split(),
                    Axis::Vertical => DockLayout::v_split(),
                };
                for (child, share) in children {
                    let (width, height, along) = match axis {
                        Axis::Horizontal => (width * share, height, width * share),
                        Axis::Vertical => (width, height * share, height * share),
                    };
                    // Before the center has been measured, the slots share
                    // the space evenly.
                    let size = (along >= 1.).then(|| px(along));
                    split = split.child(self.describe(child, width, height, activation, cx), size);
                }
                split
            }
        }
    }

    /// The size of a tab's panes all together, as last drawn.
    fn center_size(&self, cx: &App) -> (f32, f32) {
        let front = self.tabs.active().and_then(|tab| self.areas.get(&tab));
        front
            .into_iter()
            .chain(self.areas.values())
            .map(|area| area.area.read(cx).bounds().size)
            .find(|size| size.width > px(0.) && size.height > px(0.))
            .map_or((0., 0.), |size| {
                (f32::from(size.width), f32::from(size.height))
            })
    }

    /// Whether every pane of `layout` would be large enough to use. Before
    /// the center has been measured, anything fits.
    fn fits<P: Copy + Eq + std::fmt::Debug>(&self, layout: &Layout<P>, cx: &App) -> bool {
        let (width, height) = self.center_size(cx);
        width <= 0. || layout.fits(width, height, MIN_PANE_WIDTH, MIN_PANE_HEIGHT)
    }

    fn pane_of(&self, panel: PanelId, cx: &App) -> Option<PaneContent> {
        self.panes
            .iter()
            .find(|(_, handle)| handle.panel_id(cx) == panel)
            .map(|(pane, _)| *pane)
    }

    /// A tab comes forward: its current pane takes the focus.
    fn show(&self, tab: TabId, window: &mut Window, cx: &mut App) {
        if let Some(panel) = self
            .tabs
            .current(tab)
            .and_then(|pane| self.panes.get(&pane))
        {
            panel.set_active(true, window, cx);
        }
    }

    /// A tab goes behind: none of its panes is on screen any more.
    fn hide(&self, tab: TabId, window: &mut Window, cx: &mut App) {
        if let Some(area) = self.areas.get(&tab) {
            area.activation.grant(None);
        }
        for pane in self.tabs.panes(tab) {
            if let Some(panel) = self.panes.get(pane) {
                panel.set_active(false, window, cx);
            }
        }
    }

    /// A tab's dock area laid its panes out anew: their reading order.
    fn on_layout_changed(&mut self, tab: TabId, cx: &mut Context<Self>) {
        let Some(area) = self.areas.get(&tab) else {
            return;
        };
        let mut panels = Vec::new();
        if let Some(tree) = area.area.read(cx).layout(DockPlacement::Center) {
            tree.root().walk(&mut |node| {
                if let PaneRef::Tabs { panels: group, .. } = node.kind() {
                    panels.extend(group.iter().copied());
                }
            });
        }
        let order: Vec<PaneContent> = panels
            .into_iter()
            .filter_map(|panel| self.pane_of(panel, cx))
            .collect();
        if order.len() == self.tabs.panes(tab).len() {
            self.tabs.set_order(tab, order);
        }
        self.changed(cx);
    }

    /// After any change: pane headers where a tab holds several panes, the
    /// tabs' places for the panes' menus, and word to the workspace.
    fn changed(&mut self, cx: &mut Context<Self>) {
        for (tab, area) in &self.areas {
            let headers = self.tabs.panes(*tab).len() > 1;
            if area.headers.replace(headers) != headers {
                area.area.update(cx, |_, cx| cx.notify());
            }
        }
        self.publish_places(cx);
        cx.emit(CenterEvent::Changed);
        cx.notify();
    }

    /// Where each pane stands, for its menu.
    fn publish_places(&self, cx: &mut App) {
        let ids = self.tab_ids();
        let mut places = Vec::new();
        for (ix, tab) in ids.iter().enumerate() {
            let panes = self.tabs.panes(*tab);
            let zoomed = self.zoomed_pane(*tab, cx);
            let joinable = self.has_joiners(*tab);
            for pane in panes {
                let place = TabPlace {
                    tab: *tab,
                    count: ids.len(),
                    ix,
                    panes: panes.len(),
                    zoomed: zoomed == Some(*pane),
                    joinable,
                };
                places.push((*pane, place));
            }
        }
        cx.default_global::<TabPlaces>().0.extend(places);
    }

    /// The pane filling `tab`, while one is zoomed.
    fn zoomed_pane(&self, tab: TabId, cx: &App) -> Option<PaneContent> {
        let node = self.areas.get(&tab)?.area.read(cx).zoomed_group()?;
        self.pane_in_group(tab, node, cx)
    }

    fn render_tab_title(&self, tab: TabId, window: &mut Window, cx: &mut App) -> AnyElement {
        let panes = self.tabs.panes(tab);
        if let [pane] = panes
            && let Some(panel) = self.panes.get(pane)
        {
            return panel.title(window, cx);
        }
        // A tab of several panes: always the same mark in the accent color,
        // whatever its panes are and however they are connected; their own
        // headers say that.
        let first = panes[0];
        let names = panes
            .iter()
            .filter_map(|pane| self.panes.get(pane))
            .filter_map(|panel| panel.tab_name(cx))
            .collect::<Vec<_>>()
            .join("\n");
        ClosableTabTitle::new(
            ("pane-tab", tab.0),
            Icon::new(CatalogIcon::LayoutGrid)
                .small()
                .text_color(cx.theme().primary),
            self.tab_label(tab, cx),
        )
        .count(panes.len())
        .tooltip(names)
        .closable(("close-tab", tab.0), Box::new(CloseTab(tab)))
        .context_menu(move |menu, _, cx| tab_menu(menu, tab, first, cx))
        .into_any_element()
    }

    /// 所有标签: every tab, the one in front checked, and under a tab of
    /// several panes its panes, each a way to that pane.
    fn render_all_tabs(&self, cx: &App) -> impl IntoElement {
        let front = self.tabs.active();
        let rows: Vec<TabRow> = self
            .tabs
            .ids()
            .map(|tab| {
                let panes = self.tabs.panes(tab);
                let names = if panes.len() > 1 {
                    panes
                        .iter()
                        .filter_map(|pane| {
                            let name = self.panes.get(pane)?.tab_name(cx)?;
                            Some((*pane, name))
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                TabRow {
                    tab,
                    label: self.tab_label(tab, cx),
                    in_front: Some(tab) == front,
                    panes: names,
                }
            })
            .collect();
        let target = self.target.clone();
        Button::new("all-tabs")
            .icon(IconName::ChevronDown)
            .xsmall()
            .ghost()
            .tab_stop(false)
            .tooltip(t!("center.tab.all"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.action_context(target.clone()).scrollable(true);
                for row in &rows {
                    menu = menu.menu_with_check(
                        row.label.clone(),
                        row.in_front,
                        Box::new(ActivateTab(row.tab)),
                    );
                    for (pane, name) in &row.panes {
                        let name = name.clone();
                        menu = menu.menu_element(Box::new(ActivatePane(*pane)), move |_, cx| {
                            div()
                                .pl_4()
                                .text_color(cx.theme().muted_foreground)
                                .child(name.clone())
                        });
                    }
                }
                menu
            })
            .anchor(Anchor::TopRight)
    }

    /// The tab bar's trailing end: for a tab of one pane its controls (in a
    /// split each pane's header has its own), then 所有标签.
    fn render_toolbar(&self, window: &mut Window, cx: &mut App) -> AnyElement {
        let alone =
            self.tabs
                .active()
                .map(|tab| self.tabs.panes(tab))
                .and_then(|panes| match panes {
                    [pane] => self.panes.get(pane),
                    _ => None,
                });
        let controls = alone.map(|panel| pane_controls(panel, window, cx));
        h_flex()
            .items_center()
            .h_full()
            .px_2()
            .gap_1()
            .border_l_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.tab_bar)
            .children(controls)
            .child(self.render_all_tabs(cx))
            .into_any_element()
    }
}

/// The commands of a tab of several panes: its name, another tab beside it,
/// its panes' sizes, and closing it or the tabs around it.
fn tab_menu(menu: PopupMenu, tab: TabId, first: PaneContent, cx: &App) -> PopupMenu {
    let place = tab_place(first, cx);
    let is_empty = |scope: CloseScope| {
        place.is_none_or(|place| scope.targets(place.count, place.ix).is_empty())
    };
    let batch = |scope| Box::new(CloseTabs { tab: first, scope });
    let menu = menu.menu_with_icon(
        t!("center.tab.rename"),
        Icon::new(CatalogIcon::Pencil),
        Box::new(RenameTab(tab)),
    );
    split_items(menu, tab, place.is_some_and(|place| place.joinable))
        .menu(t!("center.tab.equalize"), Box::new(EqualizePanes(tab)))
        .separator()
        .menu(t!("common.close"), Box::new(CloseTab(tab)))
        .menu_with_disabled(
            t!("shared.tab_menu.close_left"),
            batch(CloseScope::Left),
            is_empty(CloseScope::Left),
        )
        .menu_with_disabled(
            t!("shared.tab_menu.close_right"),
            batch(CloseScope::Right),
            is_empty(CloseScope::Right),
        )
        .menu_with_disabled(
            t!("shared.tab_menu.close_others"),
            batch(CloseScope::Others),
            is_empty(CloseScope::Others),
        )
        .menu(t!("shared.tab_menu.close_all"), batch(CloseScope::All))
}

/// 向右分屏…, 向下分屏…: choose a tab to go beside `tab`. Disabled while no
/// tab could.
pub fn split_items(menu: PopupMenu, tab: TabId, joinable: bool) -> PopupMenu {
    let choose = |placement| Box::new(ChooseTabToJoin { tab, placement });
    menu.menu_with_disabled(
        t!("center.tab.split_right"),
        choose(Placement::Right),
        !joinable,
    )
    .menu_with_disabled(
        t!("center.tab.split_down"),
        choose(Placement::Bottom),
        !joinable,
    )
}

impl Render for CenterView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(front) = self.tabs.active() else {
            return div()
                .id("center")
                .size_full()
                .child(self.placeholder.clone())
                .into_any_element();
        };
        self.refresh_drops(cx);
        let ids = self.tab_ids();
        if self.scrolled_to != Some(front) {
            if let Some(ix) = ids.iter().position(|id| *id == front) {
                self.scroll.scroll_to_item(ix);
            }
            self.scrolled_to = Some(front);
        }
        let tabs: Vec<Tab> = ids
            .iter()
            .map(|&tab| {
                let label = self.tab_label(tab, cx);
                // Where what is dragged would land: a line on the side it
                // would stand, or the whole tab when it would go in.
                let zone = self.zone_shown(tab, cx);
                let shown = move |tab: StyleRefinement, cx: &App| match zone {
                    Some(TabZone::Before) => tab.border_l_2().border_color(cx.theme().drag_border),
                    Some(TabZone::After) => tab.border_r_2().border_color(cx.theme().drag_border),
                    Some(TabZone::Into) => tab.bg(cx.theme().tokens.drop_target),
                    None => tab,
                };
                let tab_element =
                    Tab::new()
                        .child(self.render_tab_title(tab, window, cx))
                        .selected(tab == front)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.activate_tab(tab, window, cx)
                        }))
                        .drag_over::<AnyDrag>(move |style, _, _, cx| shown(style, cx))
                        .drag_over::<TabAlong>(move |style, _, _, cx| shown(style, cx))
                        .on_drag_move(cx.listener(move |this, event, window, cx| {
                            this.on_drag_over_tab(tab, event, window, cx)
                        }))
                        .on_drag_move(cx.listener(move |this, event, _, cx| {
                            this.on_drag_along_tab(tab, event, cx)
                        }))
                        .on_drop(cx.listener(move |this, item: &AnyDrag, window, cx| {
                            if let Some(&dragged) = item.value().downcast_ref::<Dragged>() {
                                this.on_dropped_on_tab(tab, dragged, window, cx);
                            }
                        }))
                        .on_drop(cx.listener(move |this, along: &TabAlong, window, cx| {
                            this.on_dropped_along_tab(tab, along.0, window, cx)
                        }));
                // A tab of one pane can join another tab; one of several, or
                // 设置, only moves along the bar.
                let panes = self.tabs.panes(tab);
                if panes.len() == 1 && !panes[0].stands_alone() {
                    let dragging = self.dragging.clone();
                    tab_element.on_drag(AnyDrag::new(Dragged::Tab(tab)), move |_, _, _, cx| {
                        dragging.set(Some(Dragged::Tab(tab)));
                        cx.new(|_| DragPreview::new(label.clone()))
                    })
                } else {
                    let dragging = self.dragging.clone();
                    tab_element.on_drag(TabAlong(tab), move |_, _, _, cx| {
                        dragging.set(None);
                        cx.new(|_| DragPreview::new(label.clone()))
                    })
                }
            })
            .collect();
        let bar = TabBar::new("center-tabs")
            .track_scroll(&self.scroll)
            .prefix(self.controls.render(&self.target, cx))
            .children(tabs)
            .last_empty_space(
                div()
                    .id("tab-bar-empty-space")
                    .test_support()
                    .h_full()
                    .flex_grow(1.)
                    .min_w_16()
                    .drag_over::<AnyDrag>(|space, _, _, cx| space.bg(cx.theme().tokens.drop_target))
                    .drag_over::<TabAlong>(|space, _, _, cx| {
                        space.bg(cx.theme().tokens.drop_target)
                    })
                    .on_drop(cx.listener(|this, item: &AnyDrag, window, cx| {
                        if let Some(&dragged) = item.value().downcast_ref::<Dragged>() {
                            this.on_dropped_past_the_tabs(dragged, window, cx);
                        }
                    }))
                    .on_drop(cx.listener(|this, along: &TabAlong, window, cx| {
                        this.on_dropped_past_the_tabs(Dragged::Tab(along.0), window, cx)
                    })),
            )
            .suffix(self.render_toolbar(window, cx));
        let area = self.areas.get(&front).map(|tab| tab.area.clone());
        v_flex()
            .id("center")
            .track_focus(&self.focus_handle)
            .size_full()
            .child(
                wheel_scrolls_tabs(bar)
                    .id("center-tab-bar")
                    .test_support()
                    // With nothing focused, as after a dialog closes, the
                    // bar's buttons would dispatch nowhere: a press on the
                    // bar gives the center the keyboard first.
                    // Before the press reaches a button, which takes it.
                    .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                        if window.focused(cx).is_none() {
                            window.focus(&this.focus_handle, cx);
                        }
                    })),
            )
            .child(div().flex_1().min_h_0().children(area))
            .children(self.render_chooser())
            .into_any_element()
    }
}

// The center is the one panel of the workspace dock's center.

impl Focusable for CenterView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for CenterView {}

impl BasePanel for CenterView {
    fn panel_name(&self) -> &'static str {
        "CenterView"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for CenterView {
    fn zoom_control(&self, _: &App) -> Option<gpui_kit::component::dock::PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
