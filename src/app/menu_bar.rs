//! The macOS menu bar: ShellRS's own menu and 窗口, with the items and keys
//! every Mac app has. macOS gives ⌘H, ⌥⌘H and ⌘M to the items of the menu
//! bar, not to the app, so without one those keys did nothing (issue #88).
//!
//! GPUI writes each item's key from the keymap, and the names are
//! translated as the menus are made, so they are made again whenever the
//! keymap is rebuilt or the language changes. There is no 编辑 menu: the
//! terminal's ⌘C and ⌘V follow rules of their own, which the system's
//! `copy:` and `paste:` would go around. Nor is there 服务, which nothing
//! in ShellRS offers anything to.

use gpui_kit::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread as _, MainThreadMarker};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationIcon, NSAboutPanelOptionApplicationName,
    NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionCredits, NSAboutPanelOptionVersion,
    NSApplication, NSFont, NSFontAttributeName, NSLinkAttributeName, NSMutableParagraphStyle,
    NSParagraphStyleAttributeName, NSTextAlignment,
};
use objc2_foundation::{NSAttributedString, NSDictionary, NSString, NSURL};

use super::shortcuts::AppliedShortcuts;
use super::{
    AboutApp, HideApp, HideOtherApps, MinimizeWindow, OpenSettings, Quit, ShowAllApps, ZoomWindow,
};
use crate::i18n::{UiLocale, t};
use crate::update::build_info::{VERSION, WEBSITE};

/// Where 窗口 is in the menu bar, after ShellRS's own menu.
const WINDOW_MENU: isize = 1;

/// What the items do that the workspace does not: the app's and the
/// window's own, which work with nothing focused, and in 查找文件's window
/// as in the main one.
pub(super) fn handle_actions(cx: &mut App) {
    cx.on_action(|_: &AboutApp, _| show_about_panel());
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
    cx.on_action(|_: &MinimizeWindow, cx| in_front(cx, Window::minimize_window));
    cx.on_action(|_: &ZoomWindow, cx| in_front(cx, Window::zoom_window));
}

/// Show the menu bar, from now on in the interface's language and with the
/// keys in the keymap. The app's to call, not tests'.
pub fn show_menu_bar(cx: &mut App) {
    set_menu_bar(cx);
    cx.observe_global::<UiLocale>(set_menu_bar).detach();
    cx.observe_global::<AppliedShortcuts>(set_menu_bar).detach();
}

fn set_menu_bar(cx: &mut App) {
    cx.set_menus(menus());
    // GPUI hands macOS the window menu, which the system adds the open
    // windows to, only when it is called "Window"; in Chinese it is 窗口.
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(main_thread);
    let menu = app
        .mainMenu()
        .and_then(|bar| bar.itemAtIndex(WINDOW_MENU))
        .and_then(|item| item.submenu());
    app.setWindowsMenu(menu.as_deref());
}

/// The menu bar, in the interface's language.
fn menus() -> Vec<Menu> {
    vec![
        // macOS titles the first menu after the app, whatever it is called.
        Menu::new("ShellRS").items([
            MenuItem::action(t!("app.menu.about"), AboutApp),
            MenuItem::separator(),
            MenuItem::action(t!("app.menu.settings"), OpenSettings),
            MenuItem::separator(),
            MenuItem::action(t!("app.menu.hide"), HideApp),
            MenuItem::action(t!("app.menu.hide_others"), HideOtherApps),
            MenuItem::action(t!("app.menu.show_all"), ShowAllApps),
            MenuItem::separator(),
            MenuItem::action(t!("app.menu.quit"), Quit),
        ]),
        Menu::new(t!("app.menu.window")).items([
            MenuItem::action(t!("app.menu.minimize"), MinimizeWindow),
            MenuItem::action(t!("app.menu.zoom"), ZoomWindow),
        ]),
    ]
}

/// The system's about panel: ShellRS's name, icon and version, and a link
/// to the website. Given, not left to the system to read from the bundle:
/// outside one (`cargo run`) it would show the executable's name, its
/// folder's icon and no version.
fn show_about_panel() {
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(main_thread);
    let name = NSString::from_str("ShellRS");
    let version = NSString::from_str(VERSION);
    // Empty, the build number in brackets after the version is left out:
    // the version is what 设置 › 关于 shows too.
    let build = NSString::from_str("");
    let credits = website_link();
    let icon = app.applicationIconImage();
    // SAFETY: AppKit's constant keys, read on the main thread.
    let (name_key, version_key, build_key, credits_key, icon_key) = unsafe {
        (
            NSAboutPanelOptionApplicationName,
            NSAboutPanelOptionApplicationVersion,
            NSAboutPanelOptionVersion,
            NSAboutPanelOptionCredits,
            NSAboutPanelOptionApplicationIcon,
        )
    };
    let mut keys = vec![name_key, version_key, build_key, credits_key];
    let mut values: Vec<&AnyObject> = vec![&name, &version, &build, &credits];
    // The bundle's icon, or the logo set when there is none.
    if let Some(icon) = &icon {
        keys.push(icon_key);
        values.push(icon);
    }
    let options = NSDictionary::from_slices(&keys, &values);
    // SAFETY: each option holds the type AppKit documents for it.
    unsafe { app.orderFrontStandardAboutPanelWithOptions(&options) };
}

/// The panel's credits: the website's address, centered, opening it when
/// clicked.
fn website_link() -> Retained<NSAttributedString> {
    let address = WEBSITE.trim_start_matches("https://");
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setAlignment(NSTextAlignment::Center);
    let font = NSFont::systemFontOfSize(NSFont::smallSystemFontSize());
    let url = NSURL::URLWithString(&NSString::from_str(WEBSITE));
    // SAFETY: AppKit's constant keys, read on the main thread.
    let (paragraph_key, font_key, link_key) = unsafe {
        (
            NSParagraphStyleAttributeName,
            NSFontAttributeName,
            NSLinkAttributeName,
        )
    };
    let mut keys = vec![paragraph_key, font_key];
    let mut values: Vec<&AnyObject> = vec![&paragraph, &font];
    if let Some(url) = &url {
        keys.push(link_key);
        values.push(url);
    }
    let attributes = NSDictionary::from_slices(&keys, &values);
    // SAFETY: each attribute holds the type AppKit documents for it.
    unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(address),
            Some(&attributes),
        )
    }
}

/// Do `act` to the window in front, once the window the action came
/// through is no longer being updated.
fn in_front(cx: &mut App, act: fn(&Window)) {
    cx.defer(move |cx| {
        if let Some(window) = cx.active_window() {
            _ = window.update(cx, |_, window, _| act(window));
        }
    });
}

#[cfg(test)]
mod tests {
    use gpui_kit::{App, Keystroke, MenuItem, TestAppContext};

    use super::menus;
    use crate::app::{Shortcut, ShortcutOverrides, apply_shortcuts};

    /// A menu's name, with its items' names and the keys they show.
    type Shown = (String, Vec<(String, Option<String>)>);

    /// Each menu as it shows, its keys as GPUI picks them: the first
    /// binding that works anywhere.
    fn shown(cx: &App) -> Vec<Shown> {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        menus()
            .into_iter()
            .map(|menu| {
                let items = menu
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        MenuItem::Action { name, action, .. } => {
                            let keys = keymap
                                .bindings_for_action(action.as_ref())
                                .find(|binding| binding.predicate().is_none())
                                .map(|binding| binding.keystrokes()[0].inner().unparse());
                            Some((name.to_string(), keys))
                        }
                        MenuItem::Separator | MenuItem::Submenu(_) | MenuItem::SystemMenu(_) => {
                            None
                        }
                    })
                    .collect();
                (menu.name.to_string(), items)
            })
            .collect()
    }

    /// `keys` as the keymap writes them back.
    fn written(keys: &str) -> Option<String> {
        Some(Keystroke::parse(keys).unwrap().unparse())
    }

    #[gpui_kit::test]
    fn the_items_show_the_keys_mac_apps_have(cx: &mut TestAppContext) {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        cx.update(|cx| {
            apply_shortcuts(&ShortcutOverrides::default(), cx);
            let expected: Vec<Shown> = vec![
                (
                    "ShellRS".into(),
                    vec![
                        ("About ShellRS".into(), None),
                        ("Settings…".into(), written("cmd-,")),
                        ("Hide ShellRS".into(), written("cmd-h")),
                        ("Hide Others".into(), written("cmd-alt-h")),
                        ("Show All".into(), None),
                        ("Quit ShellRS".into(), written("cmd-q")),
                    ],
                ),
                (
                    "Window".into(),
                    vec![("Minimize".into(), written("cmd-m")), ("Zoom".into(), None)],
                ),
            ];
            assert_eq!(shown(cx), expected);
        });
        crate::i18n::set_locale("zh-CN");
        cx.update(|cx| assert_eq!(shown(cx)[1].0, "窗口"));
    }

    #[gpui_kit::test]
    fn a_changed_shortcut_shows_in_the_menu(cx: &mut TestAppContext) {
        let quit = Shortcut::find("quit").unwrap();
        let quit_keys = |cx: &App| shown(cx)[0].1.last().unwrap().1.clone();
        cx.update(|cx| {
            let mut overrides = ShortcutOverrides::default();
            overrides.set(quit, "cmd-alt-q".into());
            apply_shortcuts(&overrides, cx);
            assert_eq!(quit_keys(cx), written("cmd-alt-q"));
            overrides.disable(quit);
            apply_shortcuts(&overrides, cx);
            assert_eq!(quit_keys(cx), None);
        });
    }
}
