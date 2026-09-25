use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use gpui_kit::*;

use crate::app::{CatalogIcon, NewLocalTerminal, NewSession, ToggleSessionPanel, ToggleTheme};

/// The custom title bar: app name on the left, window-level commands on the
/// right. Every button dispatches an action on `target` (the workspace's
/// focus handle) so the workspace handles it even when nothing is focused.
pub fn render_title_bar(sessions_visible: bool, target: &FocusHandle, cx: &App) -> TitleBar {
    let dark = cx.theme().is_dark();
    let (toggle, new_session, new_local, theme) = (
        target.clone(),
        target.clone(),
        target.clone(),
        target.clone(),
    );
    TitleBar::new()
        .child(
            h_flex().gap_2().child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("ShellRS"),
            ),
        )
        .child(
            h_flex()
                .justify_end()
                .px_2()
                .gap_1()
                // The title bar drags the window on mouse down; keep clicks
                // on these controls from starting a drag.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new("toggle-sessions")
                        .ghost()
                        .small()
                        .icon(IconName::PanelLeft)
                        .tooltip(if sessions_visible {
                            "隐藏会话面板"
                        } else {
                            "显示会话面板"
                        })
                        .on_click(move |_, window, cx| {
                            toggle.dispatch_action(&ToggleSessionPanel, window, cx)
                        }),
                )
                .child(
                    Button::new("new-session")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label("新建会话…")
                        .on_click(move |_, window, cx| {
                            new_session.dispatch_action(&NewSession, window, cx)
                        }),
                )
                .child(
                    Button::new("new-local-terminal")
                        .ghost()
                        .small()
                        .icon(CatalogIcon::Terminal)
                        .label("本地终端")
                        .tooltip(if cfg!(target_os = "macos") {
                            "新建本地终端（⌘T）"
                        } else {
                            "新建本地终端（Ctrl+T）"
                        })
                        .on_click(move |_, window, cx| {
                            new_local.dispatch_action(&NewLocalTerminal, window, cx)
                        }),
                )
                .child(
                    Button::new("theme-toggle")
                        .ghost()
                        .small()
                        .icon(if dark { IconName::Sun } else { IconName::Moon })
                        .tooltip(if dark {
                            "切换为浅色"
                        } else {
                            "切换为深色"
                        })
                        .on_click(move |_, window, cx| {
                            theme.dispatch_action(&ToggleTheme, window, cx)
                        }),
                ),
        )
}
