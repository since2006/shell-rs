//! Place a new center tab beside its source, resolving live group membership
//! only when the command runs. DockArea remains the owner of the layout.

use std::sync::Arc;

use gpui_kit::component::dock::{BasePanelView, DockPlacement, InsertTarget, PaneRef};
use gpui_kit::*;

use crate::app::CenterTab;

use super::Workspace;

impl Workspace {
    /// Add a tab beside an explicit source, or beside the active center tab.
    /// The pinned Dock API registers new panels in the first group. Moving
    /// within this update lets its deferred activation see only the final
    /// membership, and restoring that group's selection preserves its tabs.
    pub(super) fn add_center_panel(
        &mut self,
        panel: Arc<dyn BasePanelView>,
        origin: Option<CenterTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = self.dock_area.read(cx);
        let target = area.layout(DockPlacement::Center).and_then(|tree| {
            origin
                .into_iter()
                .chain(self.active_tab)
                .find_map(|tab| {
                    let (_, panel) = self.center_tab_location(tab, cx)?;
                    tree.find_panel_node(panel)
                })
                .or_else(|| {
                    tree.panels().find_map(|panel| {
                        self.center_tab_for_panel(panel)?;
                        tree.find_panel_node(panel)
                    })
                })
        });
        let mut first_selection = None;
        if let Some(tree) = area.layout(DockPlacement::Center) {
            tree.root().walk(&mut |node| {
                if first_selection.is_none()
                    && let PaneRef::Tabs { panels, active_ix } = node.kind()
                    && let Some(panel) = panels.get(active_ix)
                {
                    first_selection = Some((node.id(), *panel, active_ix));
                }
            });
        }
        let id = panel.panel_id(cx);
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(panel, DockPlacement::Center, None, window, cx);
            let Some(target) = target else { return };
            let inserted_in = area
                .layout(DockPlacement::Center)
                .and_then(|tree| tree.find_panel_node(id));
            if inserted_in == Some(target) {
                return;
            }
            area.move_panel(
                id,
                InsertTarget::Tabs {
                    node: target,
                    ix: None,
                    activate: true,
                },
                window,
                cx,
            );
            if let Some((node, panel, ix)) = first_selection
                && area.layout(DockPlacement::Center).and_then(|tree| tree.find_node(node))
                    .is_some_and(|group| matches!(group.kind(), PaneRef::Tabs { panels, active_ix } if panels.get(active_ix) != Some(&panel)))
            {
                // Reinsert at the same index: this changes selection only,
                // preserving panel order and every existing group entity.
                area.move_panel(
                    panel,
                    InsertTarget::Tabs {
                        node,
                        ix: Some(ix),
                        activate: true,
                    },
                    window,
                    cx,
                );
            }
        });
    }
}
