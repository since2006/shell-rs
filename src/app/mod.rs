//! Application-level wiring: actions, key bindings, assets.

mod actions;
mod assets;
mod paths;

pub use actions::*;
pub use assets::{AppAssets, CatalogIcon};
pub use paths::{data_dir, database_path, known_hosts_path};

use gpui_kit::component::dock::ToggleZoom;
use gpui_kit::*;

use crate::terminal::{TERMINAL_KEY_CONTEXT, terminal_key_bindings};

/// Key context of the session panel, for bindings that only apply there.
pub const SESSION_PANEL_CONTEXT: &str = "SessionPanel";

/// Initialize GPUI Kit and everything global to the application.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    gpui_kit::component::set_locale("zh-CN");
    cx.bind_keys(key_bindings());
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
}

fn key_bindings() -> Vec<KeyBinding> {
    #[cfg(target_os = "macos")]
    const PRIMARY: &str = "cmd";
    #[cfg(not(target_os = "macos"))]
    const PRIMARY: &str = "ctrl";

    let primary = |key: &str| format!("{PRIMARY}-{key}");
    let mut bindings = vec![
        KeyBinding::new(&primary("n"), NewSession, None),
        KeyBinding::new(&primary("shift-n"), NewGroup, None),
        KeyBinding::new(&primary("b"), ToggleSessionPanel, None),
        KeyBinding::new(&primary("k"), FocusSearch, None),
        KeyBinding::new(&primary("t"), NewLocalTerminal, None),
        KeyBinding::new(&primary("w"), CloseActiveTab, None),
        KeyBinding::new(&primary("="), ZoomIn, None),
        KeyBinding::new(&primary("-"), ZoomOut, None),
        KeyBinding::new(&primary("0"), ZoomReset, None),
        KeyBinding::new(&primary("q"), Quit, None),
        KeyBinding::new("shift-escape", ToggleZoom, None),
        KeyBinding::new("f5", UploadSelectedFiles, Some("LocalFileList")),
        KeyBinding::new("space", ToggleUploadSelection, Some("LocalFileList")),
        KeyBinding::new(&primary("a"), SelectAllUploadFiles, Some("LocalFileList")),
        KeyBinding::new("enter", ConnectSelected, Some(SESSION_PANEL_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", CopyTerminal, Some(TERMINAL_KEY_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-v", PasteTerminal, Some(TERMINAL_KEY_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-c", CopyTerminal, Some(TERMINAL_KEY_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-v", PasteTerminal, Some(TERMINAL_KEY_CONTEXT)),
    ];
    bindings.extend(terminal_key_bindings());
    bindings
}
