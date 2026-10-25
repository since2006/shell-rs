//! 设置 › 键盘快捷键, and the tab keys it lists.

use gpui_kit::Keystroke;
use shellrs::app::{PaneContent, Shortcut};
use shellrs::terminal::RemoteTerminalId;

use crate::support::*;

fn default_keys(id: &str) -> &'static str {
    Shortcut::find(id).unwrap().default_keys()
}

/// cmd on macOS, ctrl elsewhere.
fn primary(key: &str) -> String {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    format!("{modifier}-{key}")
}

fn active_pane(workspace: &Entity<Workspace>, cx: &mut TestAppContext) -> Option<PaneContent> {
    cx.update(|cx| workspace.read(cx).active_pane())
}

fn terminal(id: u64) -> Option<PaneContent> {
    Some(PaneContent::Terminal(RemoteTerminalId(id)))
}

/// Open 设置 › 键盘快捷键, the fifth category.
fn open_shortcuts(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    in_frame(cx, handle, |window, cx| {
        window.within("settings").click("0-4", cx)
    });
}

/// Scroll the page down to the tab shortcuts, below the window's fold.
fn scroll_to_tab_shortcuts(cx: &mut TestAppContext, handle: WindowHandle<Root>) {
    in_frame(cx, handle, |window, cx| {
        let position = window.find("shortcut-keys-new-host").bounds().center();
        window.dispatch_event(
            gpui_kit::ScrollWheelEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-600.))),
                modifiers: gpui_kit::Modifiers::default(),
                touch_phase: gpui_kit::TouchPhase::Moved,
            }
            .to_platform_input(),
            cx,
        );
    });
}

fn overrides(workspace: &Entity<Workspace>, cx: &mut TestAppContext) -> serde_json::Value {
    cx.update(|cx| {
        serde_json::to_value(workspace.read(cx).settings().read(cx).settings().shortcuts).unwrap()
    })
}

#[gpui_kit::test]
fn tabs_are_switched_from_the_keyboard(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    assert_eq!(active_pane(&workspace, cx), terminal(INITIAL_WEB_TERMINAL));

    let switch = default_keys("switch-to-tab");
    let press = |keys: String, cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, cx| window.press(&keys, cx))
    };
    press(default_keys("next-tab").into(), cx);
    assert_eq!(
        active_pane(&workspace, cx),
        terminal(INITIAL_STAGING_TERMINAL)
    );
    // Around the end and back.
    press(default_keys("next-tab").into(), cx);
    assert_eq!(active_pane(&workspace, cx), terminal(INITIAL_WEB_TERMINAL));
    press(default_keys("previous-tab").into(), cx);
    assert_eq!(
        active_pane(&workspace, cx),
        terminal(INITIAL_STAGING_TERMINAL)
    );
    press(switch.into(), cx);
    assert_eq!(active_pane(&workspace, cx), terminal(INITIAL_WEB_TERMINAL));
    // 9 is the last; past the last, nothing.
    press(switch.replace('1', "9"), cx);
    assert_eq!(
        active_pane(&workspace, cx),
        terminal(INITIAL_STAGING_TERMINAL)
    );
    press(switch.replace('1', "5"), cx);
    assert_eq!(
        active_pane(&workspace, cx),
        terminal(INITIAL_STAGING_TERMINAL)
    );
}

#[gpui_kit::test]
fn a_shortcut_takes_the_keys_pressed_for_it(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_shortcuts(cx, handle);
    scroll_to_tab_shortcuts(cx, handle);
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("shortcut-keys-next-tab").visible());
        window.click("shortcut-keys-next-tab", cx);
    });
    in_frame(cx, handle, |window, cx| {
        let recorder = window.find("shortcut-recorder-next-tab");
        assert!(recorder.visible());
        assert_eq!(recorder.focused(), Some(true));
        window.press(&primary("j"), cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("shortcut-recorder-next-tab").is_none());
    });
    let pressed = Keystroke::parse(&primary("j")).unwrap().unparse();
    assert_eq!(
        overrides(&workspace, cx),
        serde_json::json!({ "next-tab": pressed })
    );

    // The new keys move on from the settings tab, the last, to the first;
    // the old ones no longer do anything.
    assert_eq!(active_pane(&workspace, cx), Some(PaneContent::Settings));
    in_frame(cx, handle, |window, cx| window.press(&primary("j"), cx));
    assert_eq!(active_pane(&workspace, cx), terminal(INITIAL_WEB_TERMINAL));
    in_frame(cx, handle, |window, cx| {
        window.press(default_keys("next-tab"), cx)
    });
    assert_eq!(active_pane(&workspace, cx), terminal(INITIAL_WEB_TERMINAL));
}

#[gpui_kit::test]
fn a_shortcut_turned_off_does_nothing_until_put_back(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_shortcuts(cx, handle);
    scroll_to_tab_shortcuts(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("shortcut-disable-next-tab", cx)
    });
    assert_eq!(
        overrides(&workspace, cx),
        serde_json::json!({ "next-tab": null })
    );
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.find("shortcut-keys-next-tab").label(), Some("无"));
        window.press(default_keys("next-tab"), cx);
    });
    assert_eq!(active_pane(&workspace, cx), Some(PaneContent::Settings));

    in_frame(cx, handle, |window, cx| {
        window.click("shortcut-reset-next-tab", cx)
    });
    assert_eq!(overrides(&workspace, cx), serde_json::json!({}));
    in_frame(cx, handle, |window, cx| {
        window.press(default_keys("next-tab"), cx)
    });
    assert_eq!(active_pane(&workspace, cx), terminal(INITIAL_WEB_TERMINAL));
}

#[gpui_kit::test]
fn keys_that_are_taken_or_plain_are_refused_and_escape_gives_up(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    open_shortcuts(cx, handle);
    scroll_to_tab_shortcuts(cx, handle);
    in_frame(cx, handle, |window, cx| {
        window.click("shortcut-keys-next-tab", cx)
    });
    // A plain letter, another shortcut's keys, a text field's: each said
    // and refused, still listening. ⌘W does not close the tab meanwhile.
    for keys in [
        "j".to_string(),
        default_keys("new-host").to_string(),
        default_keys("close-tab").to_string(),
        primary("z"),
    ] {
        in_frame(cx, handle, |window, cx| window.press(&keys, cx));
        in_frame(cx, handle, |window, _| {
            assert!(
                window.find("shortcut-recorder-next-tab").visible(),
                "{keys}"
            );
        });
    }
    in_frame(cx, handle, |window, cx| {
        assert_eq!(window.notifications(cx).len(), 4);
    });
    assert_eq!(active_pane(&workspace, cx), Some(PaneContent::Settings));
    in_frame(cx, handle, |window, cx| window.press("escape", cx));
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("shortcut-recorder-next-tab").is_none());
        assert!(window.find("shortcut-keys-next-tab").visible());
    });
    assert_eq!(overrides(&workspace, cx), serde_json::json!({}));
}

#[gpui_kit::test]
fn the_system_wide_shortcut_starts_with_no_key_and_takes_one_the_system_knows(
    cx: &mut TestAppContext,
) {
    let (handle, workspace) = open_workspace(cx);
    open_shortcuts(cx, handle);
    in_frame(cx, handle, |window, cx| {
        assert_eq!(
            window.find("shortcut-keys-show-hide-app").label(),
            Some("无")
        );
        window.click("shortcut-keys-show-hide-app", cx);
    });
    // No key the system has a name for: said and refused, still listening.
    in_frame(cx, handle, |window, cx| window.press("ctrl-alt-+", cx));
    in_frame(cx, handle, |window, cx| {
        assert!(window.find("shortcut-recorder-show-hide-app").visible());
        assert_eq!(window.notifications(cx).len(), 1);
        window.press("alt-space", cx);
    });
    in_frame(cx, handle, |window, _| {
        assert!(window.try_find("shortcut-recorder-show-hide-app").is_none());
    });
    assert_eq!(
        overrides(&workspace, cx),
        serde_json::json!({ "show-hide-app": "alt-space" })
    );
}

#[gpui_kit::test]
fn the_zoom_keys_size_a_terminals_text_and_the_interface_elsewhere(cx: &mut TestAppContext) {
    let (handle, workspace) = open_workspace(cx);
    let terminal_size = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            workspace
                .read(cx)
                .settings()
                .read(cx)
                .settings()
                .terminal_font
                .size
        })
    };
    // What the interface is drawn at, and what 外观 › 界面字号 holds.
    let interface_size = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let saved = workspace
                .read(cx)
                .settings()
                .read(cx)
                .settings()
                .interface
                .font_size;
            assert_eq!(cx.theme().font_size.as_f32(), saved);
            saved
        })
    };
    in_frame(cx, handle, |window, cx| {
        window.click(("terminal-tab", INITIAL_WEB_TERMINAL), cx)
    });
    in_frame(cx, handle, |window, _| {
        assert_eq!(
            window.find(("terminal", INITIAL_WEB_TERMINAL)).focused(),
            Some(true)
        );
    });
    let press = |keys: String, cx: &mut TestAppContext| {
        in_frame(cx, handle, |window, cx| window.press(&keys, cx))
    };

    // 外观 › 界面字号 by default: 16, or 14 on Windows.
    let default = DEFAULT_INTERFACE_FONT_SIZE;

    // In a terminal: its text, a pixel a step, kept in the settings.
    press(primary("="), cx);
    assert_eq!((terminal_size(cx), interface_size(cx)), (14., default));
    press(primary("-"), cx);
    press(primary("-"), cx);
    assert_eq!((terminal_size(cx), interface_size(cx)), (12., default));
    // No smaller than 设置 › 终端 › 字号 goes.
    for _ in 0..5 {
        press(primary("-"), cx);
    }
    assert_eq!(terminal_size(cx), 10.);
    press(primary("0"), cx);
    assert_eq!((terminal_size(cx), interface_size(cx)), (13., default));

    // Elsewhere, the interface: two pixels a step, kept in the settings.
    in_frame(cx, handle, |window, cx| window.click("open-settings", cx));
    press(primary("="), cx);
    assert_eq!((terminal_size(cx), interface_size(cx)), (13., default + 2.));
    // No larger than 设置 › 外观 › 界面字号 goes.
    for _ in 0..3 {
        press(primary("="), cx);
    }
    assert_eq!(interface_size(cx), 20.);
    press(primary("0"), cx);
    assert_eq!((terminal_size(cx), interface_size(cx)), (13., default));
    // The settings' own one pixel steps, which the keys step on from.
    let settings = cx.update(|cx| workspace.read(cx).settings().clone());
    cx.update(|cx| {
        settings.update(cx, |settings, cx| {
            settings.update(|settings| settings.interface.font_size = 15., cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(interface_size(cx), 15.);
    press(primary("-"), cx);
    assert_eq!(interface_size(cx), 13.);
}
