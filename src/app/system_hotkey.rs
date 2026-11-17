//! 显示或隐藏 ShellRS: the one shortcut that works while another app is in
//! front. The system hears its key, not the window's keymap: Carbon's hot
//! keys on macOS (no accessibility permission asked), `RegisterHotKey` on
//! Windows, X11 on Linux (not Wayland). A press waits until the workspace
//! asks for it (`take_press`), as the CLI's requests do.

use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use gpui_kit::{App, Global};

/// The system's hot keys, and the key taken from them now. Only the app
/// has it: tests leave it out, as a key taken from every other app on the
/// computer is not theirs to take.
struct SystemHotkey {
    manager: GlobalHotKeyManager,
    taken: Option<HotKey>,
}

impl Global for SystemHotkey {}

/// Start asking the system for keys; until then `take` keeps none.
pub(super) fn listen(cx: &mut App) {
    if let Ok(manager) = GlobalHotKeyManager::new() {
        cx.set_global(SystemHotkey {
            manager,
            taken: None,
        });
    }
}

/// Take `keys` (GPUI's, as `ctrl-alt-space`) from the system, letting go
/// of the key taken before. With none, or keys the system would not give
/// (another app has them), the shortcut is left without one.
pub(super) fn take(keys: Option<&str>, cx: &mut App) {
    if !cx.has_global::<SystemHotkey>() {
        return;
    }
    let wanted = keys.and_then(hotkey);
    let state = cx.global_mut::<SystemHotkey>();
    if state.taken == wanted {
        return;
    }
    if let Some(taken) = state.taken.take() {
        state.manager.unregister(taken).ok();
    }
    if let Some(wanted) = wanted
        && state.manager.register(wanted).is_ok()
    {
        state.taken = Some(wanted);
    }
}

/// Whether the system would give `keys`: tried, then given back. Without
/// the system's hot keys (tests) only whether it knows the key.
pub fn system_hotkey_available(keys: &str, cx: &mut App) -> bool {
    let Some(wanted) = hotkey(keys) else {
        return false;
    };
    if !cx.has_global::<SystemHotkey>() {
        return true;
    }
    let state = cx.global_mut::<SystemHotkey>();
    if state.taken == Some(wanted) {
        return true;
    }
    let free = state.manager.register(wanted).is_ok();
    if free {
        state.manager.unregister(wanted).ok();
    }
    free
}

/// Whether the shortcut was pressed since the last time this was asked.
pub fn take_system_hotkey_press() -> bool {
    GlobalHotKeyEvent::receiver()
        .try_iter()
        .filter(|event| event.state == HotKeyState::Pressed)
        .count()
        > 0
}

/// The system's hot key for GPUI's `keys`. The keys are named alike
/// (`space`, `f12`, `up`, `` ` ``), so the text carries over.
fn hotkey(keys: &str) -> Option<HotKey> {
    let keystroke = super::parse_keys(keys).ok()?;
    let modifiers = keystroke.modifiers;
    let mut text = String::new();
    for (held, name) in [
        (modifiers.control, "ctrl"),
        (modifiers.alt, "alt"),
        (modifiers.shift, "shift"),
        (modifiers.platform, "super"),
    ] {
        if held {
            text.push_str(name);
            text.push('+');
        }
    }
    text.push_str(&keystroke.key);
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::hotkey;

    #[test]
    fn keys_carry_over_to_the_systems_hot_keys() {
        for (keys, expected) in [
            ("alt-space", "alt+space"),
            ("ctrl-`", "ctrl+backquote"),
            ("cmd-shift-k", "shift+super+keyk"),
            ("ctrl-alt-f12", "ctrl+alt+f12"),
            ("f12", "f12"),
        ] {
            assert_eq!(hotkey(keys), expected.parse().ok(), "{keys}");
            assert!(hotkey(keys).is_some(), "{keys}");
        }
        // No key the system knows by that name.
        assert_eq!(hotkey("cmd-}"), None);
    }
}
