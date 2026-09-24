use gpui_kit::component::{
    dock::{PanelId, TabGroup},
    menu::PopupMenu,
};
use gpui_kit::*;

use crate::app::{
    CenterTab, CloseExplorer, CloseLocalTerminal, CloseScope, CloseTabs, CloseTerminal,
};

/// The close commands every center tab's context menu ends with: this tab,
/// and the ones to its left, to its right, the others and all of them.
///
/// `group` and `panel` locate the tab in its bar. They are read when the menu
/// opens, so a batch command that would close nothing is shown disabled.
pub fn close_tab_items(
    menu: PopupMenu,
    tab: CenterTab,
    group: Option<WeakEntity<TabGroup>>,
    panel: EntityId,
    cx: &App,
) -> PopupMenu {
    let target = PanelId::from(panel);
    let position = group.and_then(|group| group.upgrade()).and_then(|group| {
        let panels = group.read(cx).panels();
        panels
            .iter()
            .position(|panel| panel.panel_id(cx) == target)
            .map(|ix| (panels.len(), ix))
    });
    let is_empty =
        |scope: CloseScope| position.is_none_or(|(len, ix)| scope.targets(len, ix).is_empty());
    let batch = |scope| Box::new(CloseTabs { tab, scope });

    menu.menu("关闭", close_action(tab))
        .menu_with_disabled(
            "关闭左侧",
            batch(CloseScope::Left),
            is_empty(CloseScope::Left),
        )
        .menu_with_disabled(
            "关闭右侧",
            batch(CloseScope::Right),
            is_empty(CloseScope::Right),
        )
        .menu_with_disabled(
            "关闭其他",
            batch(CloseScope::Others),
            is_empty(CloseScope::Others),
        )
        .menu("关闭全部", batch(CloseScope::All))
}

fn close_action(tab: CenterTab) -> Box<dyn Action> {
    match tab {
        CenterTab::Terminal(id) => Box::new(CloseTerminal(id)),
        CenterTab::Explorer(id) => Box::new(CloseExplorer(id)),
        CenterTab::LocalTerminal(id) => Box::new(CloseLocalTerminal(id)),
    }
}
