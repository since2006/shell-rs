//! The tab bar's own pieces: the controls ahead of the tabs and the mouse
//! wheel that scrolls them.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{CatalogIcon, ToggleComposePane, ToggleHostPanel};
use crate::i18n::t;

/// What the tab bar shows ahead of its tabs: the host sidebar's switch and
/// 撰写窗格's. The workspace says what they show; they dispatch to it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LeadingControls {
    /// Whether the host sidebar shows, or `None` for no switch.
    pub hosts_open: Option<bool>,
    /// Whether 撰写窗格 shows.
    pub compose_shown: bool,
}

impl LeadingControls {
    pub(super) fn render(self, target: &FocusHandle, cx: &App) -> impl IntoElement {
        let hosts = target.clone();
        let compose = target.clone();
        // Bordered like a tab bar prefix: the bar's bottom line runs under
        // it, and the first tab, drawn without a left border, is set off
        // from it.
        h_flex()
            .id("tab-bar-controls")
            .test_support()
            .flex_shrink_0()
            .h_full()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().tokens.tab_bar)
            .when_some(self.hosts_open, |row, open| {
                row.child(
                    Button::new("toggle-dock:Left")
                        .ghost()
                        .xsmall()
                        .icon(if open {
                            IconName::PanelLeft
                        } else {
                            IconName::PanelLeftOpen
                        })
                        .tooltip(if open {
                            t!("workspace.title_bar.hide_sidebar")
                        } else {
                            t!("workspace.title_bar.show_sidebar")
                        })
                        .on_click(move |_, window, cx| {
                            hosts.dispatch_action(&ToggleHostPanel, window, cx)
                        }),
                )
            })
            .child(
                Button::new("toggle-compose-pane")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(CatalogIcon::SquarePen))
                    .selected(self.compose_shown)
                    .toggled(self.compose_shown)
                    .tooltip_with_action(t!("workspace.compose.toggle"), &ToggleComposePane, None)
                    .on_click(move |_, window, cx| {
                        compose.dispatch_action(&ToggleComposePane, window, cx)
                    }),
            )
    }
}

/// The tab bar, made to scroll sideways under a mouse wheel (#66).
///
/// The bar scrolls only along itself, dropping vertical deltas, while a
/// mouse wheel turns only vertically: tabs scrolled out of view could be
/// reached only with Shift held. A vertical turn is sent again as the
/// sideways one Shift makes. Only a wheel's, which comes in lines; a
/// trackpad swipes sideways itself.
pub(super) fn wheel_scrolls_tabs(tabs: impl IntoElement) -> Div {
    v_flex()
        .on_scroll_wheel(|event, window, cx| {
            let ScrollDelta::Lines(lines) = event.delta else {
                return;
            };
            if lines.x != 0. || lines.y == 0. {
                return;
            }
            cx.stop_propagation();
            let sideways = ScrollWheelEvent {
                delta: ScrollDelta::Lines(point(lines.y, 0.)),
                ..event.clone()
            };
            window.defer(cx, move |window, cx| {
                window.dispatch_event(sideways.to_platform_input(), cx);
            });
        })
        .child(tabs)
}
