//! Keys as text (`cmd-shift-n`, `ctrl-alt-1`, `f5`), as the settings file,
//! the keymap and the system hot key spell them.
//!
//! Since gpui-pre 0.3.12 GPUI asks which system the text is for: ⌘ is
//! `cmd` on macOS, `win` on Windows and `super` on Linux. ShellRS builds for
//! one system at a time and picks its keys by `cfg!(target_os)` already, and
//! GPUI's test platform reports the same system, so it is this build's.

use gpui_kit::{InvalidKeystrokeError, Keystroke, OperatingSystem};

/// The system this build spells keys for.
const KEYS_OS: OperatingSystem = if cfg!(target_os = "macos") {
    OperatingSystem::Mac
} else if cfg!(target_os = "windows") {
    OperatingSystem::Windows
} else {
    OperatingSystem::Linux
};

/// The keystroke `keys` stands for.
pub fn parse_keys(keys: &str) -> Result<Keystroke, InvalidKeystrokeError> {
    Keystroke::parse_with_os(keys, KEYS_OS)
}

/// The keystroke as text, as `parse_keys` reads it back.
pub fn unparse_keys(keystroke: &Keystroke) -> String {
    keystroke.unparse(KEYS_OS)
}

#[cfg(test)]
mod tests {
    use super::{parse_keys, unparse_keys};

    #[gpui_kit::test]
    fn the_system_is_the_one_gpui_reports(cx: &mut gpui_kit::TestAppContext) {
        assert_eq!(super::KEYS_OS, cx.update(|cx| cx.platform_info().os));
    }

    #[test]
    fn keys_read_back_as_written() {
        let platform = if cfg!(target_os = "macos") {
            "cmd"
        } else if cfg!(target_os = "windows") {
            "win"
        } else {
            "super"
        };
        for keys in [
            "ctrl-alt-shift-n".to_string(),
            format!("{platform}-shift-t"),
            "f5".into(),
        ] {
            assert_eq!(unparse_keys(&parse_keys(&keys).unwrap()), keys);
        }
    }
}
