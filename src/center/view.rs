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

use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui_kit::component::dock::{
    AnyDrag, BasePanel, BasePanelView as _, DockArea, DockEvent, DockPlacement, DropTarget,
    InsertTarget, NodeId, PaneRef, Panel, PanelEvent, PanelHandle, PanelId,
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

use super::drag::{DragPreview, Dragged, TabAlong};
use super::model::{PaneKind as _, TabId, TabTitle, Tabs};
use super::pane_controls::pane_controls;
use super::pane_skin::PaneSkin;
use super::pane_view::PaneView;
use super::places::tab_place;
use super::places::{TabPlace, TabPlaces};
use super::tab_bar::{LeadingControls, wheel_scrolls_tabs};
use crate::app::{
    ActivatePane, ActivateTab, CatalogIcon, CloseScope, CloseTab, CloseTabs, MergeTab, MovePane,
    MovePaneToNewTab, MoveTab, PaneContent, RenameTab,
};
use crate::i18n::t;
use crate::shared::ClosableTabTitle;

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
    /// Whether it is in front, which lets its dock area's word that a pane
    /// is active through (see `PaneView`).
    in_front: Arc<AtomicBool>,
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
    /// While a pane is dragged over another tab: that tab, and the wait
    /// before it comes forward to take the pane.
    hover: Option<(TabId, Task<()>)>,
    focus_handle: FocusHandle,
}

/// How long a pane dragged over a tab waits before that tab comes forward.
const HOVER_SWITCH: Duration = Duration::from_millis(500);

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
        self.put_in(pane, tab, None, window, cx);
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
        self.take_out(pane, tab, window, cx);
        self.panes.remove(&pane);
        self.take_outs.remove(&pane);
        cx.default_global::<TabPlaces>().0.remove(&pane);
        if self.tabs.close_pane(pane).is_some() {
            self.areas.remove(&tab);
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
        let (Some(from), Some(into)) = (self.tabs.tab_of(pane), self.tabs.tab_of(onto)) else {
            return false;
        };
        if pane == onto {
            return false;
        }
        if from == into {
            let Some(target) = self.split_target(onto, placement, cx) else {
                return false;
            };
            let Some(panel) = self.panes.get(&pane).map(|panel| panel.panel_id(cx)) else {
                return false;
            };
            if let Some(area) = self.areas.get(&from) {
                area.area
                    .update(cx, |area, cx| area.move_panel(panel, target, window, cx));
            }
            self.tabs.focus(pane);
            self.changed(cx);
            return true;
        }
        if !self.tabs.can_move_into(pane, into) {
            return false;
        }
        if let Some(previous) = self.tabs.active().filter(|previous| *previous != into) {
            self.hide(previous, window, cx);
        }
        self.take_out(pane, from, window, cx);
        self.tabs.move_pane(pane, into);
        if self.tabs.panes(from).is_empty() {
            self.areas.remove(&from);
        }
        if let Some(area) = self.areas.get(&into) {
            area.in_front.store(true, Ordering::Relaxed);
        }
        self.put_in(pane, into, Some((onto, placement)), window, cx);
        self.show(into, window, cx);
        self.changed(cx);
        true
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
        self.take_out(pane, from, window, cx);
        let Some(tab) = self.tabs.split_off(pane, ix) else {
            return;
        };
        self.new_area(tab, window, cx);
        self.put_in(pane, tab, None, window, cx);
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
        let target = self.split_target(pane, Placement::Right, cx);
        let Some(area) = self.areas.get(&tab).map(|area| area.area.clone()) else {
            return;
        };
        area.update(cx, |area, cx| {
            if zoomed.is_some() {
                area.set_zoomed_out(window, cx);
            }
            if zoomed != Some(pane)
                && several
                && let Some(InsertTarget::Split { node, .. }) = target
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
        let (area, headers) = PaneSkin::dock_area(format!("tab-{}", tab.0).into(), window, cx);
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
                in_front: Arc::new(AtomicBool::new(true)),
                _subscriptions: subscriptions,
            },
        );
    }

    /// Something dragged was dropped on a side of one of `tab`'s panes. The
    /// middle takes nothing: a pane is never a tab of another.
    fn on_dropped_on_pane(
        &mut self,
        tab: TabId,
        item: &AnyDrag,
        target: &DropTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hover = None;
        let Some(&dragged) = item.value().downcast_ref::<Dragged>() else {
            return;
        };
        let DropTarget::Group {
            node,
            placement: Some(placement),
        } = *target
        else {
            return;
        };
        let Some(onto) = self.pane_in_group(tab, node, cx) else {
            return;
        };
        let action: Box<dyn Action> = match dragged {
            Dragged::Pane(pane) if pane != onto => Box::new(MovePane {
                pane,
                onto,
                placement,
            }),
            Dragged::Tab(from) if self.tabs.can_merge(from, tab) => Box::new(MergeTab {
                from,
                onto,
                placement,
            }),
            _ => return,
        };
        self.dispatch(action, window, cx);
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

    /// Something dragged was dropped on a tab: a tab moves to its place; a
    /// pane goes into it, on the right.
    fn on_dropped_on_tab(
        &mut self,
        tab: TabId,
        dragged: Dragged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hover = None;
        let action: Box<dyn Action> = match dragged {
            Dragged::Tab(moved) => match self.position(tab) {
                Some(ix) => Box::new(MoveTab { tab: moved, ix }),
                None => return,
            },
            Dragged::Pane(pane) => {
                let Some(&onto) = self.tabs.panes(tab).last() else {
                    return;
                };
                if !self.tabs.can_move_into(pane, tab) {
                    return;
                }
                Box::new(MovePane {
                    pane,
                    onto,
                    placement: Placement::Right,
                })
            }
        };
        self.dispatch(action, window, cx);
    }

    /// Something dragged was dropped past the last tab: a tab goes last, a
    /// pane to a tab of its own there.
    fn on_dropped_past_the_tabs(
        &mut self,
        dragged: Dragged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hover = None;
        let last = self.tabs.ids().count();
        let action: Box<dyn Action> = match dragged {
            Dragged::Tab(tab) => Box::new(MoveTab { tab, ix: last }),
            Dragged::Pane(pane) => Box::new(MovePaneToNewTab {
                pane,
                ix: Some(last),
            }),
        };
        self.dispatch(action, window, cx);
    }

    /// A pane dragged over a tab other than the one in front: after a
    /// moment that tab comes forward, so the pane can be dropped beside one
    /// of its panes. Leaving the tab calls it off.
    fn on_drag_over_tab(
        &mut self,
        tab: TabId,
        event: &DragMoveEvent<AnyDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let over = event.bounds.contains(&event.event.position);
        let hovering = self.hover.as_ref().map(|(hovered, _)| *hovered);
        if !over {
            if hovering == Some(tab) {
                self.hover = None;
            }
            return;
        }
        let is_pane = matches!(
            event.drag(cx).value().downcast_ref::<Dragged>(),
            Some(Dragged::Pane(_))
        );
        if !is_pane || hovering == Some(tab) || self.tabs.active() == Some(tab) {
            return;
        }
        let task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(HOVER_SWITCH).await;
            _ = this.update_in(cx, |this, window, cx| {
                this.hover = None;
                if cx.has_active_drag() {
                    this.activate_tab(tab, window, cx);
                }
            });
        });
        self.hover = Some((tab, task));
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

    /// Put a pane into a tab's dock area, beside another pane or, in a new
    /// tab, alone. Adding tells the pane it is active, which gives it the
    /// focus while the tab is in front.
    fn put_in(
        &mut self,
        pane: PaneContent,
        tab: TabId,
        beside: Option<(PaneContent, Placement)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(panel), Some(area)) = (self.panes.get(&pane).cloned(), self.areas.get(&tab))
        else {
            return;
        };
        let id = panel.panel_id(cx);
        let view = Arc::new(PaneView::new(pane, panel, area.in_front.clone()));
        let target = beside.and_then(|(onto, placement)| self.split_target(onto, placement, cx));
        area.area.clone().update(cx, |area, cx| {
            area.add_panel_view(view, DockPlacement::Center, None, window, cx);
            if let Some(target) = target {
                area.move_panel(id, target, window, cx);
            }
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

    /// Where a pane dropped on `placement`'s side of `onto` goes.
    fn split_target(
        &self,
        onto: PaneContent,
        placement: Placement,
        cx: &App,
    ) -> Option<InsertTarget> {
        let tab = self.tabs.tab_of(onto)?;
        let panel = self.panes.get(&onto)?.panel_id(cx);
        let node = self
            .areas
            .get(&tab)?
            .area
            .read(cx)
            .layout(DockPlacement::Center)?
            .find_panel_node(panel)?;
        Some(InsertTarget::Split {
            node,
            placement,
            size: None,
        })
    }

    fn pane_of(&self, panel: PanelId, cx: &App) -> Option<PaneContent> {
        self.panes
            .iter()
            .find(|(_, handle)| handle.panel_id(cx) == panel)
            .map(|(pane, _)| *pane)
    }

    /// A tab comes forward: its current pane takes the focus.
    fn show(&self, tab: TabId, window: &mut Window, cx: &mut App) {
        if let Some(area) = self.areas.get(&tab) {
            area.in_front.store(true, Ordering::Relaxed);
        }
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
            area.in_front.store(false, Ordering::Relaxed);
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
            for pane in panes {
                let place = TabPlace {
                    count: ids.len(),
                    ix,
                    panes: panes.len(),
                    zoomed: zoomed == Some(*pane),
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

/// The commands of a tab of several panes: its name, and closing it or the
/// tabs around it.
fn tab_menu(menu: PopupMenu, tab: TabId, first: PaneContent, cx: &App) -> PopupMenu {
    let place = tab_place(first, cx);
    let is_empty = |scope: CloseScope| {
        place.is_none_or(|place| scope.targets(place.count, place.ix).is_empty())
    };
    let batch = |scope| Box::new(CloseTabs { tab: first, scope });
    menu.menu_with_icon(
        t!("center.tab.rename"),
        Icon::new(CatalogIcon::Pencil),
        Box::new(RenameTab(tab)),
    )
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

impl Render for CenterView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(front) = self.tabs.active() else {
            return div()
                .id("center")
                .size_full()
                .child(self.placeholder.clone())
                .into_any_element();
        };
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
                let tab_element = Tab::new()
                    .child(self.render_tab_title(tab, window, cx))
                    .selected(tab == front)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.activate_tab(tab, window, cx)),
                    )
                    .drag_over::<AnyDrag>(|tab, _, _, cx| {
                        tab.border_l_2().border_color(cx.theme().drag_border)
                    })
                    .drag_over::<TabAlong>(|tab, _, _, cx| {
                        tab.border_l_2().border_color(cx.theme().drag_border)
                    })
                    .on_drag_move(cx.listener(move |this, event, window, cx| {
                        this.on_drag_over_tab(tab, event, window, cx)
                    }))
                    .on_drop(cx.listener(move |this, item: &AnyDrag, window, cx| {
                        if let Some(&dragged) = item.value().downcast_ref::<Dragged>() {
                            this.on_dropped_on_tab(tab, dragged, window, cx);
                        }
                    }))
                    .on_drop(cx.listener(move |this, along: &TabAlong, window, cx| {
                        this.on_dropped_on_tab(tab, Dragged::Tab(along.0), window, cx)
                    }));
                // A tab of one pane can join another tab; one of several, or
                // 设置, only moves along the bar.
                let panes = self.tabs.panes(tab);
                if panes.len() == 1 && !panes[0].stands_alone() {
                    tab_element.on_drag(AnyDrag::new(Dragged::Tab(tab)), move |_, _, _, cx| {
                        cx.new(|_| DragPreview::new(label.clone()))
                    })
                } else {
                    tab_element.on_drag(TabAlong(tab), move |_, _, _, cx| {
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
