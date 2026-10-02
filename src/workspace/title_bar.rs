use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, TitleBar,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, NewHost, NewLocalTerminal, ShowCredentials, ShowForwards, ShowHosts, ShowUpdate,
    ToggleHostPanel, ToggleTheme,
};
use crate::shared::tinted;
use crate::update::UpdateBadge;

use super::sidebar::SidebarMode;

/// The custom title bar: the app name and the sidebar's switch on the left,
/// window-level commands on the right. Every button dispatches an action on
/// `target` (the workspace's focus handle) so the workspace handles it even
/// when nothing is focused.
///
/// `sidebar` is what the left dock is showing, or `None` while it is hidden;
/// `forwards` is how many port forwards are running; `update` is the
/// newer ShellRS waiting to be installed, when there is one.
pub fn render_title_bar(
    sidebar: Option<SidebarMode>,
    forwards: usize,
    update: Option<UpdateBadge>,
    target: &FocusHandle,
    cx: &App,
) -> TitleBar {
    let dark = cx.theme().is_dark();
    let (mode, toggle, new_host, new_local, theme, show_update) = (
        target.clone(),
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
                                Button::new("show-hosts")
                                    .icon(CatalogIcon::Server)
                                    .tooltip("主机")
                                    .accessibility_label("主机")
                                    .selected(sidebar == Some(SidebarMode::Hosts)),
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
                            .child(
                                Button::new("show-credentials")
                                    .icon(CatalogIcon::KeyRound)
                                    .tooltip("凭据")
                                    .accessibility_label("凭据")
                                    .selected(sidebar == Some(SidebarMode::Credentials)),
                            )
                            .on_click(move |picked: &Vec<usize>, window, cx| {
                                match picked.first() {
                                    Some(0) => mode.dispatch_action(&ShowHosts, window, cx),
                                    Some(1) => mode.dispatch_action(&ShowForwards, window, cx),
                                    Some(2) => mode.dispatch_action(&ShowCredentials, window, cx),
                                    _ => {}
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
                    Button::new("toggle-hosts")
                        .ghost()
                        .small()
                        .icon(IconName::PanelLeft)
                        .tooltip(if sidebar.is_some() {
                            "隐藏侧栏"
                        } else {
                            "显示侧栏"
                        })
                        .on_click(move |_, window, cx| {
                            toggle.dispatch_action(&ToggleHostPanel, window, cx)
                        }),
                )
                .child(
                    Button::new("new-host")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .label("新建主机…")
                        .on_click(move |_, window, cx| {
                            new_host.dispatch_action(&NewHost, window, cx)
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
                .children(update.map(|update| {
                    // The one coloured button up here, in the colour of
                    // what it says: the only sign that an update waits.
                    Button::new("update-available")
                        .custom(tinted(
                            if update.trouble {
                                cx.theme().warning
                            } else {
                                cx.theme().success
                            },
                            cx,
                        ))
                        .small()
                        .icon(CatalogIcon::CircleArrowUp)
                        .tooltip(update.label.clone())
                        .accessibility_label(update.label)
                        .on_click(move |_, window, cx| {
                            show_update.dispatch_action(&ShowUpdate, window, cx)
                        })
                }))
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
