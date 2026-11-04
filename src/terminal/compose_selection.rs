//! Controlled tab checkboxes for manual compose membership.
use crate::app::{CenterTab, SetComposeTarget};
use crate::i18n::t;
use gpui_kit::component::{Disableable as _, Sizable as _, checkbox::Checkbox, h_flex};
use gpui_kit::*;

#[derive(Clone, PartialEq)]
pub(crate) struct ComposeSelection {
    selected: bool,
    available: bool,
    target: FocusHandle,
}
impl ComposeSelection {
    pub(crate) fn new(selected: bool, available: bool, target: FocusHandle) -> Self {
        Self {
            selected,
            available,
            target,
        }
    }
    pub(super) fn lead(
        selection: Option<Self>,
        tab: CenterTab,
        name: SharedString,
        mark: impl IntoElement,
    ) -> AnyElement {
        let Some(selection) = selection else {
            return mark.into_any_element();
        };
        h_flex()
            .gap_1()
            .child(selection.render(tab, name))
            .child(mark)
            .into_any_element()
    }
    fn render(self, tab: CenterTab, name: SharedString) -> impl IntoElement {
        let target = self.target;
        let id = match tab {
            CenterTab::Terminal(id) => ("compose-remote", id.0),
            CenterTab::LocalTerminal(id) => ("compose-local", id.0),
            _ => unreachable!("only terminals have compose checkboxes"),
        };
        let label = t!("workspace.compose.target", name = name);
        h_flex().size_5().justify_center().flex_shrink_0().child(
            Checkbox::new(id)
                .small()
                .checked(self.selected)
                .disabled(!self.available)
                .accessibility_label(label.clone())
                .tooltip(if self.available {
                    label
                } else {
                    t!("workspace.compose.unavailable")
                })
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_change(move |selected, window, cx| {
                    cx.stop_propagation();
                    target.dispatch_action(&SetComposeTarget::new(tab, *selected), window, cx);
                }),
        )
    }
}
