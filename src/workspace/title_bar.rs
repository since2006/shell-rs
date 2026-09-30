use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, TitleBar,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, NewLocalTerminal, NewSession, ShowForwards, ShowSessions, ToggleSessionPanel,
    ToggleTheme,
};

use super::sidebar::SidebarMode;

/// The custom title bar: the app name and the sidebar's switch on the left,
/// window-level commands on the right. Every button dispatches an action on
/// `target` (the workspace's focus handle) so the workspace handles it even
/// when nothing is focused.
///
/// `sidebar` is what the left dock is showing, or `None` while it is hidden;
/// `forwards` is how many port forwards are running.
pub fn render_title_bar(
    sidebar: Option<SidebarMode>,
    forwards: usize,
    target: &FocusHandle,
    cx: &App,
) -> TitleBar {
    let dark = cx.theme().is_dark();
    let (mode, toggle, new_session, new_local, theme) = (
        target.clone(),
        target.clone(),
        target.clone(),
        target.clone(),
        target.clone(),
    );
    // The title bar drags the window on mouse down; keep clicks on its
    // controls from starting a drag.
    let no_drag = |_: &MouseDownEvent, _: &mut Window, cx: &mut App| cx.stop_propagation();
    TitleBar::new()
        .child(
            h_flex()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child("ShellRS"),
                )
                .child(
                    // Which list the sidebar shows. The choice stays marked
                    // while that list is up, and a hidden sidebar marks none.
                    div().on_mouse_down(MouseButton::Left, no_drag).child(
                        ButtonGroup::new("sidebar-mode")
                            .ghost()
                            .small()
                            .child(
                                Button::new("show-sessions")
                                    .icon(CatalogIcon::Server)
                                    .tooltip("主机")
                                    .accessibility_label("主机")
                                    .selected(sidebar == Some(SidebarMode::Sessions)),
                            )
                            .child(
                                Button::new("show-forwards")
                                    .icon(CatalogIcon::ArrowLeftRight)
                                    .tooltip("端口转发")
                                    .selected(sidebar == Some(SidebarMode::Forwards))
                                    // Forwards run whether or not their list
                                    // is showing, so the count is said here.
                                    .map(|button| match forwards {
                                        0 => button.accessibility_label("端口转发"),
                                        running => {
                                            button.label(running.to_string()).accessibility_label(
                                                format!("端口转发，{running} 条运行中"),
                                            )
                                        }
                                    }),
                            )
                            .on_click(move |picked: &Vec<usize>, window, cx| {
                                match picked.first() {
                                    Some(0) => mode.dispatch_action(&ShowSessions, window, cx),
                                    Some(_) => mode.dispatch_action(&ShowForwards, window, cx),
                                    None => {}
                                }
                            }),
                    ),
                ),
        )
        .child(
            h_flex()
                .justify_end()
                .px_2()
                .gap_1()
                .on_mouse_down(MouseButton::Left, no_drag)
                .child(
                    Button::new("toggle-sessions")
                        .ghost()
                        .small()
                        .icon(IconName::PanelLeft)
                        .tooltip(if sidebar.is_some() {
                            "隐藏侧栏"
                        } else {
                            "显示侧栏"
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
                        .label("新建主机…")
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
