//! A short-lived geometry transaction around native Dock moves.
//!
//! gpui-base 0.6.1 can lose a measured slot when it flattens a newly
//! introduced split. Keep the native result's topology and panel handles,
//! but supply complete sizes from the geometry the user actually dragged.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use gpui_kit::base::PANEL_MIN_SIZE;
use gpui_kit::component::{
    Placement, WindowExt as _,
    dock::{
        DockArea, DockLayout, DockPlacement, DragPanel, InsertTarget, NodeId, PaneNode, PaneRef,
        PaneTree, PanelId,
    },
};
use gpui_kit::{App, Axis, Bounds, Pixels, Size, WeakEntity, Window, px, size};

#[derive(Clone, Copy, PartialEq)]
struct Metrics {
    viewport: Size<Pixels>,
    rem: Pixels,
    scale: f32,
}
impl Metrics {
    fn of(window: &Window) -> Self {
        Self {
            viewport: window.viewport_size(),
            rem: window.rem_size(),
            scale: window.scale_factor(),
        }
    }
}

struct DragSnapshot {
    session: u64,
    panel: PanelId,
    source: NodeId,
    target: NodeId,
    tree: PaneTree,
    bounds: HashMap<NodeId, Bounds<Pixels>>,
    metrics: Metrics,
}

#[derive(Default)]
pub(super) struct DockLayoutRepair {
    bounds: RefCell<HashMap<NodeId, Bounds<Pixels>>>,
    metrics: Cell<Option<Metrics>>,
    pending: RefCell<Option<DragSnapshot>>,
    generation: Cell<u64>,
}

impl DockLayoutRepair {
    /// The center wrapper's prepaint runs before its child measurements.
    /// A frame replaces its cache, so hidden and removed nodes cannot retain
    /// geometry across sidebar changes, panel zoom, or font changes.
    pub(super) fn begin_frame(&self, window: &Window) {
        let metrics = Metrics::of(window);
        if self.metrics.replace(Some(metrics)) != Some(metrics) {
            self.cancel();
        }
        self.bounds.borrow_mut().clear();
    }

    pub(super) fn interaction_generation(&self) -> u64 {
        self.generation.get()
    }

    pub(super) fn record(&self, node: NodeId, bounds: Bounds<Pixels>, window: &Window) {
        let metrics = Metrics::of(window);
        if self.metrics.replace(Some(metrics)) != Some(metrics) {
            self.bounds.borrow_mut().clear();
            self.cancel();
        }
        self.bounds.borrow_mut().insert(node, bounds);
    }

    /// Pointer/key commands supersede any queued focus restoration.
    pub(super) fn cancel(&self) {
        self.pending.borrow_mut().take();
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    pub(super) fn pending_cancel(&self) {
        self.pending.borrow_mut().take();
    }

    pub(super) fn capture(
        &self,
        drag: &DragPanel,
        target: NodeId,
        area: &DockArea,
        window: &Window,
    ) {
        let Some(tree) = area.layout(DockPlacement::Center) else {
            return;
        };
        if tree.find_panel_node(drag.panel()) != Some(drag.source())
            || tree.find_node(target).is_none()
        {
            return;
        }
        let mut pending = self.pending.borrow_mut();
        if let Some(snapshot) = pending.as_mut()
            && snapshot.session == drag.drag_session_id()
        {
            snapshot.target = target;
            return;
        }
        let nodes = tree.node_ids();
        let mut bounds = self.bounds.borrow_mut();
        bounds.retain(|node, _| nodes.contains(node));
        // Hidden/zoomed subtrees have no trustworthy current measurement.
        // They must not inherit a size from an earlier viewport.
        if nodes.iter().any(|node| !bounds.contains_key(node)) {
            return;
        }
        *pending = Some(DragSnapshot {
            session: drag.drag_session_id(),
            panel: drag.panel(),
            source: drag.source(),
            target,
            tree: tree.clone(),
            bounds: bounds.clone(),
            metrics: Metrics::of(window),
        });
    }

    /// Called in the same effects turn as Dock's LayoutChanged. Taking the
    /// snapshot before set_center makes its own event a no-op.
    pub(super) fn repair(
        self: &Rc<Self>,
        weak: &WeakEntity<DockArea>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(area) = weak.upgrade() else { return };
        let nodes = area
            .read(cx)
            .layout(DockPlacement::Center)
            .map(PaneTree::node_ids)
            .unwrap_or_default();
        self.bounds
            .borrow_mut()
            .retain(|node, _| nodes.contains(node));
        let Some(snapshot) = self.pending.borrow_mut().take() else {
            return;
        };
        if snapshot.metrics != Metrics::of(window) {
            return;
        }
        let prepared = {
            let area = area.read(cx);
            let Some(after) = area.layout(DockPlacement::Center) else {
                return;
            };
            let Some(placement) = snapshot.moved_beside(after) else {
                return;
            };
            let Some(root) = snapshot.bounds.get(&snapshot.tree.root().id()) else {
                return;
            };
            let Some(target) = snapshot.bounds.get(&snapshot.target) else {
                return;
            };
            let Some(moved) = after.find_panel_node(snapshot.panel) else {
                return;
            };
            let geometry = MoveGeometry {
                before: &snapshot.bounds,
                target: snapshot.target,
                moved,
                placement,
                target_size: target.size,
            };
            let Some(layout) = geometry.layout(after.root(), root.size, area, cx) else {
                return;
            };
            let zoomed = area
                .zoomed_group()
                .and_then(|id| after.find_node(id))
                .and_then(active_panel);
            (layout, zoomed)
        };
        self.generation.set(self.generation.get().wrapping_add(1));
        let generation = self.generation.get();
        area.update(cx, |area, cx| area.set_center(prepared.0, window, cx));
        self.bounds.borrow_mut().clear();
        let expected = area.read(cx).layout(DockPlacement::Center).cloned();
        let repair = Rc::downgrade(self);
        let area = area.downgrade();
        // Native reconciliation activates each rebuilt group in deferred
        // callbacks. Restore the actual drag target after those callbacks.
        window.on_next_frame(move |window, cx| {
            let (Some(repair), Some(area)) = (repair.upgrade(), area.upgrade()) else {
                return;
            };
            if repair.generation.get() != generation
                || snapshot.metrics != Metrics::of(window)
                || window.has_active_dialog(cx)
                || window.has_active_sheet(cx)
                || crate::app_lock::is_locked(cx)
            {
                return;
            }
            let valid = area
                .read(cx)
                .layout(DockPlacement::Center)
                .is_some_and(|tree| {
                    expected.as_ref().is_some_and(|expected| {
                        tree.root().id() == expected.root().id()
                            && same_selection(expected.root(), tree.root())
                    })
                });
            if !valid {
                return;
            }
            // Commands can move focus without pointer/key input or changing
            // the center tree (for example opening the compose pane). Only
            // supersede focus produced by the rebuilt center itself.
            if window.focused(cx).is_some() {
                let area = area.read(cx);
                let center_focused = area.layout(DockPlacement::Center).is_some_and(|tree| {
                    tree.panels().any(|panel| {
                        area.panel(panel).is_some_and(|panel| {
                            panel.focus_handle(cx).contains_focused(window, cx)
                        })
                    })
                });
                if !center_focused {
                    return;
                }
            }
            if let Some(zoomed) = prepared.1
                && let Some(node) = area
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .and_then(|tree| tree.find_panel_node(zoomed))
            {
                area.update(cx, |area, cx| area.set_zoomed_in(node, window, cx));
            }
            let panel = area.read(cx).panel(snapshot.panel).cloned();
            if let Some(panel) = panel {
                panel.set_active(true, window, cx);
            }
        });
    }
}

fn active_panel(node: &PaneNode) -> Option<PanelId> {
    match node.kind() {
        PaneRef::Tabs { panels, active_ix } => panels.get(active_ix).copied(),
        _ => None,
    }
}

impl DragSnapshot {
    /// Infer the committed native edge, instead of duplicating its hit test.
    /// Reorders, tab merges, sidebar changes, opens and closes cannot match.
    fn moved_beside(&self, after: &PaneTree) -> Option<Placement> {
        let mut before_panels: Vec<_> = self.tree.panels().collect();
        let mut after_panels: Vec<_> = after.panels().collect();
        before_panels.sort();
        after_panels.sort();
        if before_panels != after_panels {
            return None;
        }
        let moved = after.find_panel_node(self.panel)?;
        if moved == self.source || self.tree.find_node(moved).is_some() {
            return None;
        }
        if !matches!(after.find_node(moved)?.kind(), PaneRef::Tabs { panels, .. } if panels == [self.panel])
        {
            return None;
        }
        let mut placement = None;
        after.root().walk(&mut |node| {
            if let PaneRef::Split { axis, children, .. } = node.kind()
                && let Some(source_ix) = children.iter().position(|child| child.id() == moved)
                && let Some(target_ix) = children.iter().position(|child| child.id() == self.target)
                && source_ix.abs_diff(target_ix) == 1
            {
                placement = Some(match (axis, source_ix < target_ix) {
                    (Axis::Horizontal, true) => Placement::Left,
                    (Axis::Horizontal, false) => Placement::Right,
                    (Axis::Vertical, true) => Placement::Top,
                    (Axis::Vertical, false) => Placement::Bottom,
                });
            }
        });
        let placement = placement?;
        // A generic LayoutChanged can be emitted by many commands. Accept
        // only the topology produced by this exact native edge move. IDs of
        // new containers differ, so compare their structure and membership.
        let mut expected = self.tree.clone();
        expected.move_panel(
            self.panel,
            InsertTarget::Split {
                node: self.target,
                placement,
                size: None,
            },
        );
        same_topology(expected.root(), after.root()).then_some(placement)
    }
}

fn same_topology(left: &PaneNode, right: &PaneNode) -> bool {
    match (left.kind(), right.kind()) {
        (PaneRef::Tabs { panels: left, .. }, PaneRef::Tabs { panels: right, .. }) => left == right,
        (
            PaneRef::Split {
                axis: left_axis,
                children: left,
                ..
            },
            PaneRef::Split {
                axis: right_axis,
                children: right,
                ..
            },
        ) => {
            left_axis == right_axis
                && left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_topology(left, right))
        }
        _ => false,
    }
}

fn same_selection(left: &PaneNode, right: &PaneNode) -> bool {
    match (left.kind(), right.kind()) {
        (
            PaneRef::Tabs {
                panels: left,
                active_ix: left_active,
            },
            PaneRef::Tabs {
                panels: right,
                active_ix: right_active,
            },
        ) => left == right && left_active == right_active,
        (
            PaneRef::Split {
                axis: left_axis,
                children: left,
                ..
            },
            PaneRef::Split {
                axis: right_axis,
                children: right,
                ..
            },
        ) => {
            left_axis == right_axis
                && left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_selection(left, right))
        }
        _ => false,
    }
}

struct MoveGeometry<'a> {
    before: &'a HashMap<NodeId, Bounds<Pixels>>,
    target: NodeId,
    moved: NodeId,
    placement: Placement,
    target_size: Size<Pixels>,
}

fn along(extent: Size<Pixels>, axis: Axis) -> Pixels {
    match axis {
        Axis::Horizontal => extent.width,
        Axis::Vertical => extent.height,
    }
}

impl MoveGeometry<'_> {
    fn affected(&self, node: &PaneNode) -> bool {
        let mut affected = false;
        node.walk(&mut |node| affected |= node.id() == self.target || node.id() == self.moved);
        affected
    }

    fn preferred(&self, node: &PaneNode, axis: Axis) -> Option<Pixels> {
        if node.id() == self.target || node.id() == self.moved {
            let value = along(self.target_size, axis);
            return Some(if self.placement.axis() == axis {
                value / 2.
            } else {
                value
            });
        }
        if let Some(bounds) = self.before.get(&node.id()) {
            return Some(along(bounds.size, axis));
        }
        match node.kind() {
            PaneRef::Split {
                axis: child_axis,
                children,
                ..
            } => {
                let values = children
                    .iter()
                    .map(|child| self.preferred(child, axis))
                    .collect::<Option<Vec<_>>>()?;
                Some(if axis == child_axis {
                    values.iter().copied().fold(Pixels::ZERO, |a, b| a + b)
                } else {
                    values.into_iter().max()?
                })
            }
            _ => None,
        }
    }

    fn layout(
        &self,
        node: &PaneNode,
        extent: Size<Pixels>,
        area: &DockArea,
        cx: &App,
    ) -> Option<DockLayout> {
        match node.kind() {
            PaneRef::Tabs { panels, active_ix } => {
                let mut layout = DockLayout::tabs();
                for panel in panels {
                    layout = layout.panel_view(area.panel(*panel)?.clone(), cx);
                }
                Some(layout.active_index(active_ix))
            }
            PaneRef::Split { axis, children, .. } => {
                let preferred = children
                    .iter()
                    .map(|child| self.preferred(child, axis).map(|v| v.as_f32()))
                    .collect::<Option<Vec<_>>>()?;
                let affected = children
                    .iter()
                    .map(|child| self.affected(child))
                    .collect::<Vec<_>>();
                let minima = children
                    .iter()
                    .map(|child| minimum_extent(child, axis))
                    .collect::<Vec<_>>();
                let sizes = allocate(along(extent, axis).as_f32(), &preferred, &affected, &minima)?;
                let mut layout = match axis {
                    Axis::Horizontal => DockLayout::h_split(),
                    Axis::Vertical => DockLayout::v_split(),
                };
                for (child, value) in children.iter().zip(sizes) {
                    let value = px(value);
                    let child_extent = match axis {
                        Axis::Horizontal => size(value, extent.height),
                        Axis::Vertical => size(extent.width, value),
                    };
                    layout = layout.child(self.layout(child, child_extent, area, cx)?, Some(value));
                }
                Some(layout)
            }
            PaneRef::Tiles { .. } => None,
        }
    }
}

/// Reserve the native minimum for all descendants along a split's axis.
fn minimum_extent(node: &PaneNode, axis: Axis) -> f32 {
    match node.kind() {
        PaneRef::Split {
            axis: child_axis,
            children,
            ..
        } => {
            let sizes = children.iter().map(|child| minimum_extent(child, axis));
            if axis == child_axis {
                sizes.sum()
            } else {
                sizes.fold(PANEL_MIN_SIZE.as_f32(), f32::max)
            }
        }
        _ => PANEL_MIN_SIZE.as_f32(),
    }
}

/// Preserve unaffected slots. Space freed by removing a group goes to the
/// moved/target branch in proportion to its old geometry. If the container
/// cannot fit those preferences, shrink proportionally above native minima.
/// If space is below the sum of minima, retain minimum sizes and allow the
/// native Dock clipping policy; never invent negative or overlapping slots.
fn allocate(
    available: f32,
    preferred: &[f32],
    affected: &[bool],
    minima: &[f32],
) -> Option<Vec<f32>> {
    if preferred.is_empty()
        || preferred.len() != affected.len()
        || preferred.len() != minima.len()
        || !available.is_finite()
        || available <= 0.
        || preferred
            .iter()
            .chain(minima)
            .any(|v| !v.is_finite() || *v <= 0.)
    {
        return None;
    }
    let minimum: f32 = minima.iter().sum();
    let available = available.max(minimum);
    let mut sizes: Vec<_> = preferred
        .iter()
        .zip(minima)
        .map(|(v, min)| v.max(*min))
        .collect();
    let total: f32 = sizes.iter().sum();
    if total > available {
        let flexible = total - minimum;
        let remaining = available - minimum;
        for (value, min) in sizes.iter_mut().zip(minima) {
            *value = min + (*value - min) * remaining / flexible;
        }
    } else {
        let any = affected.iter().any(|v| *v);
        let weight: f32 = sizes
            .iter()
            .zip(affected)
            .filter(|(_, affected)| !any || **affected)
            .map(|(v, _)| *v)
            .sum();
        for (value, affected) in sizes.iter_mut().zip(affected) {
            if !any || *affected {
                *value += (available - total) * *value / weight;
            }
        }
    }
    Some(sizes)
}

#[cfg(test)]
mod tests {
    use super::allocate;
    #[test]
    fn preserves_other_slots_and_fills_vacated_space() {
        assert_eq!(
            allocate(600., &[100., 100., 200.], &[true, true, false], &[100.; 3]),
            Some(vec![200., 200., 200.])
        );
        assert_eq!(
            allocate(800., &[100., 300., 200.], &[true, true, false], &[100.; 3]),
            Some(vec![150., 450., 200.])
        );
    }
    #[test]
    fn respects_minima_without_negative_or_unknown_sizes() {
        assert_eq!(
            allocate(400., &[50., 400.], &[true, true], &[100.; 2]),
            Some(vec![100., 300.])
        );
        assert_eq!(
            allocate(199., &[100., 100.], &[true, true], &[100.; 2]),
            Some(vec![100., 100.])
        );
        assert!(allocate(400., &[f32::NAN, 200.], &[true, true], &[100.; 2]).is_none());
        assert_eq!(
            allocate(500., &[100., 500.], &[true, true], &[200., 100.]),
            Some(vec![200., 300.])
        );
    }
}
