//! A tab of several panes, as the 重命名标签 dialog edits it.

use gpui_kit::*;

use super::model::TabId;
use super::view::CenterView;
use crate::shared::RenamableTab;

/// One tab of a center, named by hand or after its panes.
pub struct TabNaming {
    center: WeakEntity<CenterView>,
    tab: TabId,
}

impl TabNaming {
    pub fn new(center: WeakEntity<CenterView>, tab: TabId) -> Self {
        Self { center, tab }
    }
}

impl RenamableTab for TabNaming {
    fn default_title(&self, cx: &App) -> SharedString {
        self.center
            .upgrade()
            .map(|center| center.read(cx).panes_label(self.tab, cx))
            .unwrap_or_default()
    }

    fn tab_title(&self, cx: &App) -> SharedString {
        self.center
            .upgrade()
            .map(|center| center.read(cx).tab_label(self.tab, cx))
            .unwrap_or_default()
    }

    fn set_custom_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>) {
        let tab = self.tab;
        _ = self
            .center
            .update(cx, |center, cx| center.rename_tab(tab, title, cx));
    }
}
