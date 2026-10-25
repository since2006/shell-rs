use gpui_kit::component::menu::PopupMenu;
use gpui_kit::*;

use crate::app::{
    CloseEditor, CloseExplorer, CloseLocalTerminal, CloseScope, CloseSettings, CloseTabs,
    CloseTerminal, MovePaneToNewTab, PaneContent, ZoomPane,
};
use crate::center::tab_place;
use crate::i18n::t;

/// The commands every center pane's menu ends with. A pane alone in its tab
/// closes as the tab: it, and the tabs to its left, to its right, the
/// others and all of them. A pane among several moves to a tab of its own,
/// fills its tab, or closes alone; its tab's own menu closes tabs.
///
/// Where the tab of `pane` stands is read when the menu opens, so a batch
/// command that would close nothing is shown disabled.
pub fn close_tab_items(menu: PopupMenu, pane: PaneContent, cx: &App) -> PopupMenu {
    let place = tab_place(pane, cx);
    if let Some(place) = place.filter(|place| place.panes > 1) {
        let zoom = if place.zoomed {
            t!("center.pane.restore")
        } else {
            t!("center.pane.maximize")
        };
        return menu
            .menu(
                t!("center.pane.move_to_new_tab"),
                Box::new(MovePaneToNewTab { pane, ix: None }),
            )
            .menu(zoom, Box::new(ZoomPane(pane)))
            .separator()
            .menu(t!("common.close"), close_action(pane));
    }
    let position = place.map(|place| (place.count, place.ix));
    let is_empty =
        |scope: CloseScope| position.is_none_or(|(len, ix)| scope.targets(len, ix).is_empty());
    let batch = |scope| Box::new(CloseTabs { tab: pane, scope });

    menu.menu(t!("common.close"), close_action(pane))
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

fn close_action(tab: PaneContent) -> Box<dyn Action> {
    match tab {
        PaneContent::Terminal(id) => Box::new(CloseTerminal(id)),
        PaneContent::Explorer(id) => Box::new(CloseExplorer(id)),
        PaneContent::LocalTerminal(id) => Box::new(CloseLocalTerminal(id)),
        PaneContent::Settings => Box::new(CloseSettings),
        PaneContent::Editor(id) => Box::new(CloseEditor(id)),
    }
}
