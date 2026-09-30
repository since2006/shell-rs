//! Application-level wiring: actions, key bindings, assets.

mod actions;
mod assets;
mod paths;

pub use actions::*;
pub use assets::{AppAssets, CatalogIcon};
pub use paths::{cli_endpoint, cli_socket_path, data_dir, database_path, settings_path};

use gpui_kit::component::{Theme, dock::ToggleZoom};
use gpui_kit::*;

use crate::terminal::{TERMINAL_FIND_KEY_CONTEXT, TERMINAL_KEY_CONTEXT, terminal_key_bindings};

/// Key context of the session panel, for bindings that only apply there.
pub const SESSION_PANEL_CONTEXT: &str = "SessionPanel";
pub const RECENT_SESSIONS_CONTEXT: &str = "RecentSessions";
/// Key context of the port-forwarding list.
pub const FORWARD_PANEL_CONTEXT: &str = "ForwardPanel";
/// Key context of the credential list.
pub const CREDENTIAL_PANEL_CONTEXT: &str = "CredentialPanel";
/// Key contexts of the two SFTP file lists.
pub const LOCAL_FILE_LIST_CONTEXT: &str = "LocalFileList";
pub const REMOTE_FILE_LIST_CONTEXT: &str = "RemoteFileList";

/// Initialize GPUI Kit and everything global to the application.
pub fn init(cx: &mut App) {
    gpui_kit::init(cx);
    deepen_list_hover(cx);
    gpui_kit::component::set_locale("zh-CN");
    cx.bind_keys(key_bindings());
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
}

/// Use a stronger version of the theme's neutral list hover color throughout
/// the app.
pub(crate) fn deepen_list_hover(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    let hover = theme.list_hover;
    let deeper = hover.alpha(((hover.a + 0.2) * 1.2).min(1.0));
    theme.list_hover = deeper;
    theme.tokens.list_hover = deeper.into();
    Theme::sync_base(cx);
}

/// Give the session tree three times the list hover's contrast against its sidebar.
/// The recent-session list keeps the shared hover color unchanged.
pub(crate) fn session_tree_selection_color(theme: &Theme) -> Hsla {
    let sidebar = theme.sidebar;
    let hover = sidebar.blend(theme.list_hover);
    let mut selected = hover.alpha(1.0);
    selected.l = (sidebar.l + (hover.l - sidebar.l) * 3.0).clamp(0.0, 1.0);
    selected
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
        KeyBinding::new(&primary(","), OpenSettings, None),
        KeyBinding::new(&primary("="), ZoomIn, None),
        KeyBinding::new(&primary("-"), ZoomOut, None),
        KeyBinding::new(&primary("0"), ZoomReset, None),
        KeyBinding::new(&primary("q"), Quit, None),
        KeyBinding::new("shift-escape", ToggleZoom, None),
        KeyBinding::new("enter", ConnectSelected, Some(SESSION_PANEL_CONTEXT)),
        KeyBinding::new("enter", ConnectSelected, Some(RECENT_SESSIONS_CONTEXT)),
        KeyBinding::new("enter", ToggleSelectedForward, Some(FORWARD_PANEL_CONTEXT)),
        KeyBinding::new("up", SelectPreviousForward, Some(FORWARD_PANEL_CONTEXT)),
        KeyBinding::new("down", SelectNextForward, Some(FORWARD_PANEL_CONTEXT)),
        KeyBinding::new(
            "enter",
            EditSelectedCredential,
            Some(CREDENTIAL_PANEL_CONTEXT),
        ),
        KeyBinding::new(
            "up",
            SelectPreviousCredential,
            Some(CREDENTIAL_PANEL_CONTEXT),
        ),
        KeyBinding::new("down", SelectNextCredential, Some(CREDENTIAL_PANEL_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-c", CopyTerminal, Some(TERMINAL_KEY_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-v", PasteTerminal, Some(TERMINAL_KEY_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-c", CopyTerminal, Some(TERMINAL_KEY_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-v", PasteTerminal, Some(TERMINAL_KEY_CONTEXT)),
        // In a terminal ⌘K clears, as in other macOS terminals; everywhere
        // else it still focuses the session search. Inside a terminal, Ctrl
        // with a letter belongs to the shell, so other platforms add Shift, as
        // they do for copy and paste.
        #[cfg(target_os = "macos")]
        KeyBinding::new("cmd-k", ClearTerminal, Some(TERMINAL_KEY_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-shift-k", ClearTerminal, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new(
            "escape",
            DismissTerminalFind,
            Some(TERMINAL_FIND_KEY_CONTEXT),
        ),
    ];
    // The find keys work from the terminal and from inside the find bar.
    #[cfg(target_os = "macos")]
    let find_keys = ["cmd-f", "cmd-g", "cmd-shift-g"];
    #[cfg(not(target_os = "macos"))]
    let find_keys = ["ctrl-shift-f", "f3", "shift-f3"];
    for context in [TERMINAL_KEY_CONTEXT, TERMINAL_FIND_KEY_CONTEXT] {
        bindings.extend([
            KeyBinding::new(find_keys[0], FindInTerminal, Some(context)),
            KeyBinding::new(find_keys[1], FindNextInTerminal, Some(context)),
            KeyBinding::new(find_keys[2], FindPreviousInTerminal, Some(context)),
        ]);
    }
    bindings.extend(terminal_key_bindings());
    bindings.extend(file_list_key_bindings());
    bindings
}

/// WinSCP's file-panel keys, bound once per pane with the side baked into the
/// command. The movement keys are bound one level deeper, at the table, so
/// they replace `DataTable`'s own single-row selection keys.
fn file_list_key_bindings() -> Vec<KeyBinding> {
    use crate::explorer::CursorMotion;
    #[cfg(target_os = "macos")]
    const PRIMARY: &str = "cmd";
    #[cfg(not(target_os = "macos"))]
    const PRIMARY: &str = "ctrl";

    let mut bindings = Vec::new();
    for (context, remote) in [
        (LOCAL_FILE_LIST_CONTEXT, false),
        (REMOTE_FILE_LIST_CONTEXT, true),
    ] {
        let table = format!("{context} > DataTable");
        let bind = |keys: &str, command: ExplorerCommand, context: &str| {
            KeyBinding::new(keys, ExplorerShortcut(command), Some(context))
        };
        // Several keys per command: the last one registered is the one
        // tooltips show, so WinSCP's key goes last.
        #[cfg(target_os = "macos")]
        bindings.extend([
            bind("cmd-up", ExplorerCommand::Up { remote }, context),
            bind("cmd-[", ExplorerCommand::Back { remote }, context),
            bind("cmd-]", ExplorerCommand::Forward { remote }, context),
            bind("cmd-backspace", ExplorerCommand::Delete { remote }, context),
            bind("cmd-i", ExplorerCommand::Properties { remote }, context),
            // ⌘H hides the app on macOS; Finder's home is ⌘⇧H.
            bind("cmd-shift-h", ExplorerCommand::Home { remote }, context),
            // Finder's 前往文件夹.
            bind(
                "cmd-shift-g",
                ExplorerCommand::OpenDirectory { remote },
                context,
            ),
        ]);
        #[cfg(not(target_os = "macos"))]
        bindings.push(bind("ctrl-h", ExplorerCommand::Home { remote }, context));
        bindings.extend([
            bind("backspace", ExplorerCommand::Up { remote }, context),
            bind(
                &format!("{PRIMARY}-\\"),
                ExplorerCommand::Root { remote },
                context,
            ),
            bind(
                &format!("{PRIMARY}-r"),
                ExplorerCommand::Refresh { remote },
                context,
            ),
            bind("alt-left", ExplorerCommand::Back { remote }, context),
            bind("alt-right", ExplorerCommand::Forward { remote }, context),
            // WinSCP's Ctrl+B already toggles the session panel here.
            bind(
                &format!("{PRIMARY}-d"),
                ExplorerCommand::AddBookmark { remote, path: None },
                context,
            ),
            bind("f2", ExplorerCommand::Rename { remote }, context),
            bind(
                "f7",
                ExplorerCommand::New {
                    remote,
                    kind: crate::explorer::NewEntryKind::Folder,
                },
                context,
            ),
            bind("delete", ExplorerCommand::Delete { remote }, context),
            bind("f8", ExplorerCommand::Delete { remote }, context),
            bind("f9", ExplorerCommand::Properties { remote }, context),
            bind("f5", ExplorerCommand::Transfer { remote }, context),
            bind(
                "space",
                ExplorerCommand::ToggleSelection { remote },
                context,
            ),
            bind(
                "insert",
                ExplorerCommand::ToggleSelection { remote },
                context,
            ),
            bind(
                &format!("{PRIMARY}-a"),
                ExplorerCommand::SelectAll { remote },
                context,
            ),
            // WinSCP's 打开目录/书签.
            bind(
                &format!("{PRIMARY}-o"),
                ExplorerCommand::OpenDirectory { remote },
                context,
            ),
            bind("enter", ExplorerCommand::Open { remote }, context),
            #[cfg(target_os = "macos")]
            bind("cmd-down", ExplorerCommand::Open { remote }, context),
            bind(
                "tab",
                ExplorerCommand::FocusPane { remote: !remote },
                &table,
            ),
            bind(
                "shift-tab",
                ExplorerCommand::FocusPane { remote: !remote },
                &table,
            ),
            KeyBinding::new("left", NoAction {}, Some(&table)),
            KeyBinding::new("right", NoAction {}, Some(&table)),
        ]);
        for (key, motion) in [
            ("up", CursorMotion::Up),
            ("down", CursorMotion::Down),
            ("pageup", CursorMotion::PageUp),
            ("pagedown", CursorMotion::PageDown),
            ("home", CursorMotion::Home),
            ("end", CursorMotion::End),
        ] {
            for extend in [false, true] {
                let keys = if extend {
                    format!("shift-{key}")
                } else {
                    key.to_string()
                };
                bindings.push(bind(
                    &keys,
                    ExplorerCommand::MoveCursor {
                        remote,
                        motion,
                        extend,
                    },
                    &table,
                ));
            }
        }
    }
    bindings
}
