use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, TitleBar,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, NewLocalTerminal, NewTemporaryConnection, OpenSettings, QuickConnect, ShowUpdate,
    ToggleCredentialList, ToggleForwardList, ToggleHostList, ToggleHostPanel, ToggleTheme,
};
use crate::i18n::t;
use crate::shared::tinted;
use crate::update::UpdateBadge;

use super::sidebar::SidebarMode;

/// The custom title bar (issue #170). On the left the app name, the
/// sidebar's lists and a new local terminal; on the right the connections
/// that need no saved host, the update when one waits, the theme and
/// 设置. Every button dispatches an action on `target` (the workspace's
/// focus handle) so the workspace handles it even when nothing is focused.
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
    let (lists, new_local, quick, temporary, theme, show_update, settings) = (
        target.clone(),
        target.clone(),
        target.clone(),
        target.clone(),
        target.clone(),
        target.clone(),
        target.clone(),
    );
    // A list's button shows that list, or hides the sidebar when it is the
    // one up, as the right sidebar's tool buttons do. Up, it says so, with
    // ⌘B, which hides the sidebar as well and brings the same list back.
    let list_button =
        |id: &'static str, icon: CatalogIcon, mode: SidebarMode, name: SharedString| {
            let showing = sidebar == Some(mode);
            let button = Button::new(id).icon(icon).selected(showing);
            if showing {
                button.tooltip_with_action(
                    t!("workspace.title_bar.hide_sidebar"),
                    &ToggleHostPanel,
                    None,
                )
            } else {
                button.tooltip(name)
            }
        };
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
                    h_flex()
                        .gap_1()
                        .on_mouse_down(MouseButton::Left, no_drag)
                        .child(
                            // Which list the sidebar shows. The choice stays
                            // marked while that list is up, and a hidden
                            // sidebar marks none.
                            ButtonGroup::new("sidebar-mode")
                                .ghost()
                                .small()
                                .child(
                                    list_button(
                                        "show-hosts",
                                        CatalogIcon::Server,
                                        SidebarMode::Hosts,
                                        t!("workspace.title_bar.hosts"),
                                    )
                                    .accessibility_label(t!("workspace.title_bar.hosts")),
                                )
                                .child(
                                    list_button(
                                        "show-forwards",
                                        CatalogIcon::ArrowLeftRight,
                                        SidebarMode::Forwards,
                                        t!("workspace.title_bar.forwards"),
                                    )
                                    // Forwards run whether or not their list
                                    // is showing, so the count is said here.
                                    .map(|button| {
                                        match forwards {
                                            0 => button.accessibility_label(t!(
                                                "workspace.title_bar.forwards"
                                            )),
                                            running => button
                                                .label(running.to_string())
                                                .accessibility_label(t!(
                                                    "workspace.title_bar.forwards_running",
                                                    count = running
                                                )),
                                        }
                                    }),
                                )
                                .child(
                                    list_button(
                                        "show-credentials",
                                        CatalogIcon::KeyRound,
                                        SidebarMode::Credentials,
                                        t!("workspace.title_bar.credentials"),
                                    )
                                    .accessibility_label(t!("workspace.title_bar.credentials")),
                                )
                                .on_click(move |picked: &Vec<usize>, window, cx| {
                                    match picked.first() {
                                        Some(0) => {
                                            lists.dispatch_action(&ToggleHostList, window, cx)
                                        }
                                        Some(1) => {
                                            lists.dispatch_action(&ToggleForwardList, window, cx)
                                        }
                                        Some(2) => {
                                            lists.dispatch_action(&ToggleCredentialList, window, cx)
                                        }
                                        _ => {}
                                    }
                                }),
                        )
                        .child(
                            // A command beside the lists, not one of them:
                            // outside their group, so it is never marked.
                            Button::new("new-local-terminal")
                                .ghost()
                                .small()
                                .icon(CatalogIcon::Terminal)
                                .accessibility_label(t!("workspace.title_bar.local_terminal"))
                                .tooltip_with_action(
                                    t!("workspace.title_bar.new_local_terminal"),
                                    &NewLocalTerminal,
                                    None,
                                )
                                .on_click(move |_, window, cx| {
                                    new_local.dispatch_action(&NewLocalTerminal, window, cx)
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
                    Button::new("title-quick-connect")
                        .ghost()
                        .small()
                        .icon(IconName::Search)
                        .tooltip(t!("workspace.title_bar.quick_connect"))
                        .accessibility_label(t!("workspace.title_bar.quick_connect"))
                        .on_click(move |_, window, cx| {
                            quick.dispatch_action(&QuickConnect, window, cx)
                        }),
                )
                .child(
                    // Saved hosts are made in the host list; up here is the
                    // quick way in, to a host that is not saved.
                    Button::new("temporary-connection")
                        .ghost()
                        .small()
                        .icon(CatalogIcon::Zap)
                        .accessibility_label(t!("workspace.title_bar.temporary_connection"))
                        .tooltip(t!("workspace.title_bar.temporary_connection_tooltip"))
                        .on_click(move |_, window, cx| {
                            temporary.dispatch_action(&NewTemporaryConnection, window, cx)
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
                            t!("workspace.title_bar.to_light")
                        } else {
                            t!("workspace.title_bar.to_dark")
                        })
                        .on_click(move |_, window, cx| {
                            theme.dispatch_action(&ToggleTheme, window, cx)
                        }),
                )
                .child(
                    Button::new("open-settings")
                        .ghost()
                        .small()
                        .icon(CatalogIcon::Settings)
                        .accessibility_label(t!("workspace.title_bar.settings"))
                        .tooltip_with_action(
                            t!("workspace.title_bar.open_settings"),
                            &OpenSettings,
                            None,
                        )
                        .on_click(move |_, window, cx| {
                            settings.dispatch_action(&OpenSettings, window, cx)
                        }),
                ),
        )
}
