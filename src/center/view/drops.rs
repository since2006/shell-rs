//! Dragging tabs and pane headers about the center: what a drop on a pane
//! or a tab does, which drops show where they land, the wait before a tab
//! dragged over comes forward, and 选择标签, the menu of tabs to put
//! beside a tab.

use gpui_kit::component::dock::{AnyDrag, DropTarget};
use gpui_kit::component::{Placement, menu::PopupMenu};
use gpui_kit::*;

use super::super::drag::{Dragged, TabAlong, TabZone};
use super::super::layout::Layout;
use super::super::model::TabId;
use super::{CenterView, Chooser, HOVER_SWITCH, Spot, land};
use crate::app::{JoinTab, MergeTab, MovePane, MovePaneToNewTab, MoveTab, PaneContent, SwapPanes};
use crate::i18n::t;

/// What a drop on a pane does.
enum Outcome {
    /// A command for the workspace.
    Act(Box<dyn Action>),
    /// 选择标签: which tab goes on the `Placement` side of the tab.
    Choose(TabId, Placement),
}

const SIDES: [Placement; 4] = [
    Placement::Left,
    Placement::Right,
    Placement::Top,
    Placement::Bottom,
];

impl CenterView {
    /// What is being dragged, while a drag is on.
    fn dragged(&self, cx: &App) -> Option<Dragged> {
        self.dragging.get().filter(|_| cx.has_active_drag())
    }

    /// Before the frame is drawn: which drops on the panes of the tab in
    /// front would do something, and, with no drag on, nothing left over
    /// from the last.
    pub(super) fn refresh_drops(&mut self, cx: &App) {
        // A tab that only moves along the bar is a drag too, with no drop
        // on a pane: only with no drag on is its place on a tab forgotten.
        if !cx.has_active_drag() {
            self.tab_zone = None;
        }
        let Some(dragged) = self.dragged(cx) else {
            self.drops.set(Vec::new());
            return;
        };
        let Some(front) = self.tabs.active() else {
            return;
        };
        let Some(layout) = self.layout_of(front, cx) else {
            return;
        };
        let mut drops = Vec::new();
        for &onto in self.tabs.panes(front) {
            for placement in std::iter::once(None).chain(SIDES.map(Some)) {
                if self
                    .outcome(&layout, dragged, onto, placement, cx)
                    .is_some()
                {
                    drops.push((onto, placement));
                }
            }
        }
        self.drops.set(drops);
    }

    /// What dropping `dragged` on `placement`'s side of `onto` does, `onto`'s
    /// tab being laid out as `layout`; `None` for the middle. Nothing when
    /// it would leave a pane too small.
    ///
    /// A pane header: on a side, the pane goes there; in the middle of
    /// another pane of its tab, the two trade places. A tab of one pane:
    /// on a side of another tab's pane, its pane goes there; on a side of
    /// its own pane, 选择标签 asks which tab goes there instead.
    fn outcome(
        &self,
        layout: &Layout<PaneContent>,
        dragged: Dragged,
        onto: PaneContent,
        placement: Option<Placement>,
        cx: &App,
    ) -> Option<Outcome> {
        let tab = self.tabs.tab_of(onto)?;
        match (dragged, placement) {
            (Dragged::Pane(pane), None) => (pane != onto && self.tabs.tab_of(pane) == Some(tab))
                .then(|| Outcome::Act(Box::new(SwapPanes { pane, with: onto }))),
            (Dragged::Pane(pane), Some(side)) => {
                let within = self.tabs.tab_of(pane) == Some(tab);
                let allowed = pane != onto && (within || self.tabs.can_move_into(pane, tab));
                (allowed
                    && self.fits_landing(layout, Some(pane), Spot::Beside(onto, side), within, cx))
                .then(|| {
                    Outcome::Act(Box::new(MovePane {
                        pane,
                        onto,
                        placement: side,
                    }))
                })
            }
            (Dragged::Tab(from), Some(side)) if from == tab => (self.has_joiners(tab)
                && self.fits_landing(layout, None, Spot::Beside(onto, side), false, cx))
            .then_some(Outcome::Choose(tab, side)),
            (Dragged::Tab(from), Some(side)) => (self.tabs.can_merge(from, tab)
                && self.fits_landing(layout, None, Spot::Beside(onto, side), false, cx))
            .then(|| {
                Outcome::Act(Box::new(MergeTab {
                    from,
                    onto,
                    placement: side,
                }))
            }),
            (Dragged::Tab(_), None) => None,
        }
    }

    /// Whether `layout`, `pane` landed at `spot` in it, leaves every pane
    /// large enough; `None` is a pane from elsewhere.
    fn fits_landing(
        &self,
        layout: &Layout<PaneContent>,
        pane: Option<PaneContent>,
        spot: Spot<PaneContent>,
        within: bool,
        cx: &App,
    ) -> bool {
        let mut landed = layout.map(&Some);
        let spot = match spot {
            Spot::Beside(onto, side) => Spot::Beside(Some(onto), side),
            Spot::Edge(side) => Spot::Edge(side),
        };
        land(&mut landed, pane, spot, within) && self.fits(&landed, cx)
    }

    /// Whether a pane from elsewhere fits along `side` of `tab`.
    fn fits_at_edge(&self, tab: TabId, side: Placement, cx: &App) -> bool {
        self.layout_of(tab, cx)
            .is_some_and(|layout| self.fits_landing(&layout, None, Spot::Edge(side), false, cx))
    }

    /// The tabs that can go into `tab`: each of one pane, never 设置.
    fn joiners(&self, tab: TabId) -> Vec<TabId> {
        self.tabs
            .ids()
            .filter(|other| self.tabs.can_merge(*other, tab))
            .collect()
    }

    pub(super) fn has_joiners(&self, tab: TabId) -> bool {
        self.tabs.ids().any(|other| self.tabs.can_merge(other, tab))
    }

    /// Something dragged was dropped on a side of one of `tab`'s panes, or
    /// in its middle.
    pub(super) fn on_dropped_on_pane(
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
        let DropTarget::Group { node, placement } = *target else {
            return;
        };
        let Some(onto) = self.pane_in_group(tab, node, cx) else {
            return;
        };
        let Some(layout) = self.layout_of(tab, cx) else {
            return;
        };
        match self.outcome(&layout, dragged, onto, placement, cx) {
            Some(Outcome::Act(action)) => self.dispatch(action, window, cx),
            Some(Outcome::Choose(tab, side)) => {
                let position = window.mouse_position();
                self.open_chooser(tab, side, position, window, cx);
            }
            None => {}
        }
    }

    /// Where on `tab`, `bounds` on screen, something dragged at `position`
    /// lands. A tab that can join it: into it in the middle half, before or
    /// after it in the quarters at either end. Another tab: before or after
    /// it, by halves. A pane header that can go into it: into it anywhere.
    fn zone_at(
        &self,
        tab: TabId,
        dragged: Dragged,
        bounds: Bounds<Pixels>,
        position: Point<Pixels>,
    ) -> Option<TabZone> {
        let along = if bounds.size.width > px(0.) {
            (position.x - bounds.left()) / bounds.size.width
        } else {
            0.5
        };
        match dragged {
            Dragged::Pane(pane) => self.tabs.can_move_into(pane, tab).then_some(TabZone::Into),
            Dragged::Tab(moved) if moved == tab => None,
            Dragged::Tab(moved) if self.tabs.can_merge(moved, tab) => Some(match along {
                along if along < 0.25 => TabZone::Before,
                along if along > 0.75 => TabZone::After,
                _ => TabZone::Into,
            }),
            Dragged::Tab(_) => Some(if along < 0.5 {
                TabZone::Before
            } else {
                TabZone::After
            }),
        }
    }

    /// Something dragged moved over a tab: where it would land there, and,
    /// held over the middle of a tab it can go into, that tab comes forward
    /// after a moment so it can be dropped beside one of its panes. Leaving
    /// the middle calls it off.
    pub(super) fn on_drag_over_tab(
        &mut self,
        tab: TabId,
        event: &DragMoveEvent<AnyDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dragged = event.drag(cx).value().downcast_ref::<Dragged>().copied();
        let zone = dragged
            .filter(|_| event.bounds.contains(&event.event.position))
            .and_then(|dragged| self.zone_at(tab, dragged, event.bounds, event.event.position));
        self.set_zone(tab, zone, cx);
        let hovering = self.hover.as_ref().map(|(hovered, _)| *hovered);
        if zone != Some(TabZone::Into) {
            if hovering == Some(tab) {
                self.hover = None;
            }
            return;
        }
        if hovering == Some(tab) || self.tabs.active() == Some(tab) {
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

    /// A tab that only moves along the bar moved over a tab: before or
    /// after it, by halves.
    pub(super) fn on_drag_along_tab(
        &mut self,
        tab: TabId,
        event: &DragMoveEvent<TabAlong>,
        cx: &mut Context<Self>,
    ) {
        let moved = event.drag(cx).0;
        let over = event.bounds.contains(&event.event.position);
        let zone = (over && moved != tab).then(|| {
            let middle = event.bounds.left() + event.bounds.size.width / 2.;
            if event.event.position.x < middle {
                TabZone::Before
            } else {
                TabZone::After
            }
        });
        self.set_zone(tab, zone, cx);
    }

    fn set_zone(&mut self, tab: TabId, zone: Option<TabZone>, cx: &mut Context<Self>) {
        let next = match zone {
            Some(zone) => Some((tab, zone)),
            None if self.tab_zone.is_some_and(|(over, _)| over == tab) => None,
            None => return,
        };
        if self.tab_zone != next {
            self.tab_zone = next;
            cx.notify();
        }
    }

    /// Where on `tab` what is dragged would land now, for the tab to show.
    /// Into it only when it fits there.
    pub(super) fn zone_shown(&self, tab: TabId, cx: &App) -> Option<TabZone> {
        let (over, zone) = self.tab_zone?;
        if over != tab {
            return None;
        }
        if zone == TabZone::Into && !self.fits_at_edge(tab, Placement::Right, cx) {
            return None;
        }
        Some(zone)
    }

    /// Something dragged was dropped on a tab: a tab moves before or after
    /// it, or goes into it along its right side, as does a pane header.
    pub(super) fn on_dropped_on_tab(
        &mut self,
        tab: TabId,
        dragged: Dragged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hover = None;
        let zone = self
            .tab_zone
            .take()
            .filter(|(over, _)| *over == tab)
            .map(|(_, zone)| zone);
        let action: Box<dyn Action> = match (dragged, zone) {
            (Dragged::Tab(moved), Some(zone @ (TabZone::Before | TabZone::After))) => {
                let Some(ix) = self.reorder_ix(moved, tab, zone == TabZone::After) else {
                    return;
                };
                Box::new(MoveTab { tab: moved, ix })
            }
            (dragged, Some(TabZone::Into)) => {
                let pane = match dragged {
                    Dragged::Pane(pane) => pane,
                    Dragged::Tab(moved) => match self.tabs.panes(moved) {
                        [pane] => *pane,
                        _ => return,
                    },
                };
                if !self.tabs.can_move_into(pane, tab)
                    || !self.fits_at_edge(tab, Placement::Right, cx)
                {
                    return;
                }
                Box::new(JoinTab {
                    pane,
                    tab,
                    placement: Placement::Right,
                })
            }
            _ => return,
        };
        self.dispatch(action, window, cx);
    }

    /// A tab that only moves along the bar was dropped on a tab.
    pub(super) fn on_dropped_along_tab(
        &mut self,
        tab: TabId,
        moved: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let zone = self
            .tab_zone
            .take()
            .filter(|(over, _)| *over == tab)
            .map(|(_, zone)| zone);
        let Some(ix) = zone.and_then(|zone| self.reorder_ix(moved, tab, zone == TabZone::After))
        else {
            return;
        };
        self.dispatch(Box::new(MoveTab { tab: moved, ix }), window, cx);
    }

    /// Where `moved` goes among the tabs to stand before, or `after`,
    /// `target`, counted once it has left its own place.
    fn reorder_ix(&self, moved: TabId, target: TabId, after: bool) -> Option<usize> {
        let from = self.position(moved)?;
        let to = self.position(target)? + usize::from(after);
        Some(if from < to { to - 1 } else { to })
    }

    /// Something dragged was dropped past the last tab: a tab goes last, a
    /// pane to a tab of its own there.
    pub(super) fn on_dropped_past_the_tabs(
        &mut self,
        dragged: Dragged,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hover = None;
        self.tab_zone = None;
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

    /// 选择标签 at `position`: the tabs that can go along the `placement`
    /// side of `tab`, each a way to put it there. When none would fit,
    /// they are listed but disabled, and the menu says why.
    pub fn open_chooser(
        &mut self,
        tab: TabId,
        placement: Placement,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rows: Vec<(PaneContent, SharedString)> = self
            .joiners(tab)
            .into_iter()
            .filter_map(|joiner| match self.tabs.panes(joiner) {
                [pane] => Some((*pane, self.tab_label(joiner, cx))),
                _ => None,
            })
            .collect();
        if rows.is_empty() {
            return;
        }
        let fits = self.fits_at_edge(tab, placement, cx);
        let title = match placement {
            Placement::Left => t!("center.chooser.left"),
            Placement::Right => t!("center.chooser.right"),
            Placement::Top => t!("center.chooser.top"),
            Placement::Bottom => t!("center.chooser.bottom"),
        };
        // Its commands go to the tab's pane in front, as does the focus
        // when it closes.
        let context = self
            .tabs
            .current(tab)
            .and_then(|pane| self.panes.get(&pane))
            .map_or_else(|| self.focus_handle.clone(), |panel| panel.focus_handle(cx));
        let menu = PopupMenu::build(window, cx, move |menu, _, _| {
            let mut menu = menu
                .action_context(context.clone())
                .scrollable(true)
                .label(title.clone());
            if !fits {
                menu = menu.label(t!("center.chooser.too_small"));
            }
            for (pane, label) in &rows {
                menu = menu.menu_with_disabled(
                    label.clone(),
                    Box::new(JoinTab {
                        pane: *pane,
                        tab,
                        placement,
                    }),
                    !fits,
                );
            }
            menu
        });
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.chooser = None;
            cx.notify();
        });
        window.focus(&menu.focus_handle(cx), cx);
        self.chooser = Some(Chooser {
            menu,
            position,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// 选择标签, while it is open, above everything.
    pub(super) fn render_chooser(&self) -> Option<AnyElement> {
        let chooser = self.chooser.as_ref()?;
        Some(
            deferred(
                anchored()
                    .position(chooser.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(chooser.menu.clone()),
            )
            .with_priority(gpui_kit::base::POPUP_PRIORITY)
            .into_any_element(),
        )
    }
}
