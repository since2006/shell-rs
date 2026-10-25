//! A pane's own controls: what goes with it, its connection's latency
//! (`title_suffix`), its buttons (SFTP, reconnect, refresh…) and its 「…」
//! menu. On the tab bar for a tab of one pane; on its header in a split,
//! each pane with its own.

use gpui_kit::component::dock::PanelHandle;
use gpui_kit::component::{
    IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::DropdownMenu as _,
};
use gpui_kit::*;

pub(super) fn pane_controls(panel: &PanelHandle, window: &mut Window, cx: &mut App) -> Div {
    let suffix = panel.title_suffix(window, cx);
    let buttons = panel.toolbar_buttons(window, cx).unwrap_or_default();
    let menu_panel = panel.panel();
    h_flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .children(suffix)
        .child(
            h_flex()
                .gap_1()
                .occlude()
                .children(
                    buttons
                        .into_iter()
                        .map(|button| button.xsmall().ghost().tab_stop(false)),
                )
                .child(
                    Button::new("menu")
                        .icon(IconName::Ellipsis)
                        .xsmall()
                        .ghost()
                        .tab_stop(false)
                        .dropdown_menu(move |menu, window, cx| {
                            menu_panel.dropdown_menu(menu, window, cx)
                        })
                        .anchor(Anchor::TopRight),
                ),
        )
}
