//! 设置 › 键盘快捷键: the shortcuts people may change, the keys the rest of
//! the app keeps, and the keymap built from both.
//!
//! GPUI cannot take one binding out of its keymap, only clear all of it, and
//! that clears gpui-kit's bindings too (text fields, dialogs, lists …),
//! which cannot be registered again. So `init` keeps a copy of gpui-kit's
//! bindings, and every change rebuilds the keymap: that copy, then ours.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui_kit::component::{dock::ToggleZoom, kbd::Kbd};
use gpui_kit::*;
use serde::{Deserialize, Deserializer, Serialize};

use super::*;
use crate::i18n::t;
use crate::terminal::{TERMINAL_FIND_KEY_CONTEXT, TERMINAL_KEY_CONTEXT, terminal_key_bindings};

/// The groups of 设置 › 键盘快捷键, in the order the page shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortcutGroup {
    General,
    Tabs,
    Terminal,
    Editor,
}

impl ShortcutGroup {
    pub const ALL: [Self; 4] = [Self::General, Self::Tabs, Self::Terminal, Self::Editor];

    pub fn title(self) -> SharedString {
        match self {
            Self::General => t!("app.shortcut_group.general"),
            Self::Tabs => t!("app.shortcut_group.tabs"),
            Self::Terminal => t!("app.shortcut_group.terminal"),
            Self::Editor => t!("app.shortcut_group.editor"),
        }
    }
}

/// One shortcut people may change.
pub struct Shortcut {
    /// What the settings file stores. Never changes once released.
    id: &'static str,
    /// The key of what it is called.
    label: &'static str,
    group: ShortcutGroup,
    /// Where it works: anywhere, or in these key contexts.
    contexts: &'static [&'static str],
    mac: &'static str,
    other: &'static str,
    /// One binding for each digit from 1 to 9, the key standing for the
    /// digit: 切换到标签 1…9.
    numbered: bool,
    /// Taken from the system rather than bound in the window: it works
    /// while another app is in front (`system_hotkey`).
    system_wide: bool,
    /// The action, given the digit of a numbered shortcut.
    action: fn(usize) -> Box<dyn Action>,
}

const TERMINAL: &[&str] = &[TERMINAL_KEY_CONTEXT];
/// The find keys work from the terminal and from inside the find bar.
const TERMINAL_AND_FIND: &[&str] = &[TERMINAL_KEY_CONTEXT, TERMINAL_FIND_KEY_CONTEXT];
const EDITOR: &[&str] = &[EDITOR_CONTEXT];

/// What a shortcut is unless the user changed it. Other platforms keep Ctrl
/// with a letter for the shell inside a terminal, so the terminal's own
/// keys add Shift there, as other terminals do.
macro_rules! shortcut {
    ($id:literal, $label:literal, $group:ident, $contexts:expr, $mac:literal, $other:literal, $action:expr) => {
        Shortcut {
            id: $id,
            label: $label,
            group: ShortcutGroup::$group,
            contexts: $contexts,
            mac: $mac,
            other: $other,
            numbered: false,
            system_wide: false,
            action: |_| Box::new($action),
        }
    };
}

/// The shortcuts of 设置 › 键盘快捷键, in the order the page lists them.
/// Where macOS has ⌘, Windows and Linux have Ctrl+Shift, as Windows
/// Terminal, GNOME Terminal and Konsole do: Ctrl with a letter alone is a
/// control character, the shell's in a terminal, and Alt is its Meta.
pub static SHORTCUTS: [Shortcut; 37] = [
    shortcut!(
        "new-host",
        "app.shortcut.new_host",
        General,
        &[],
        "cmd-n",
        "ctrl-shift-n",
        NewHost
    ),
    shortcut!(
        "new-group",
        "app.shortcut.new_group",
        General,
        &[],
        "cmd-shift-n",
        "ctrl-shift-g",
        NewGroup
    ),
    shortcut!(
        "open-settings",
        "app.shortcut.open_settings",
        General,
        &[],
        "cmd-,",
        "ctrl-,",
        OpenSettings
    ),
    // Ctrl+Shift+B elsewhere: Ctrl+B is the shell's in a terminal, tmux's
    // prefix among others.
    shortcut!(
        "toggle-sidebar",
        "app.shortcut.toggle_sidebar",
        General,
        &[],
        "cmd-b",
        "ctrl-shift-b",
        ToggleHostPanel
    ),
    shortcut!(
        "toggle-tool-sidebar",
        "app.shortcut.toggle_tool_sidebar",
        General,
        &[],
        "cmd-alt-b",
        "ctrl-alt-b",
        ToggleToolSidebar
    ),
    // Xshell opens its file transfer on Ctrl+Alt+F; 文件 is that beside the
    // terminal, on its own connection. ⌥ as for the right sidebar (issue
    // #137).
    shortcut!(
        "toggle-files",
        "app.shortcut.toggle_files",
        General,
        &[],
        "cmd-alt-f",
        "ctrl-alt-f",
        ToggleTool(ToolKind::Files)
    ),
    shortcut!(
        "toggle-compose-pane",
        "app.shortcut.toggle_compose_pane",
        General,
        &[],
        "cmd-shift-e",
        "ctrl-shift-e",
        ToggleComposePane
    ),
    // ⌘↑ and ⌘↓ go to the top and bottom of a text field, and up a folder
    // and into one in SFTP. Elsewhere Win+↑ and Win+↓ are the system's own.
    shortcut!(
        "maximize-window",
        "app.shortcut.maximize_window",
        General,
        &[],
        "ctrl-cmd-up",
        "",
        MaximizeWindow
    ),
    shortcut!(
        "restore-window",
        "app.shortcut.restore_window",
        General,
        &[],
        "ctrl-cmd-down",
        "",
        RestoreWindow
    ),
    // A key taken from every other app as well: none until one is chosen.
    Shortcut {
        id: "show-hide-app",
        label: "app.shortcut.show_hide_app",
        group: ShortcutGroup::General,
        contexts: &[],
        mac: "",
        other: "",
        numbered: false,
        system_wide: true,
        action: |_| Box::new(ShowOrHideApp),
    },
    // 快速连接, the palette that finds a saved host and connects to it, in
    // a terminal too: ⌘P goes to anything in VS Code and Sublime Text,
    // Ctrl+Shift+P to Windows Terminal's palette. It took the place of
    // 聚焦搜索 (`focus-search`), gone with its action: keys the user gave
    // that one stay in the settings unused, as any id this version lacks.
    shortcut!(
        "quick-connect",
        "app.shortcut.quick_connect",
        General,
        &[],
        "cmd-p",
        "ctrl-shift-p",
        QuickConnect
    ),
    // In a terminal the same keys size its text instead, as terminals do.
    shortcut!(
        "zoom-in",
        "app.shortcut.zoom_in",
        General,
        &[],
        "cmd-=",
        "ctrl-=",
        ZoomIn
    ),
    shortcut!(
        "zoom-out",
        "app.shortcut.zoom_out",
        General,
        &[],
        "cmd--",
        "ctrl--",
        ZoomOut
    ),
    shortcut!(
        "zoom-reset",
        "app.shortcut.zoom_reset",
        General,
        &[],
        "cmd-0",
        "ctrl-0",
        ZoomReset
    ),
    shortcut!(
        "quit",
        "app.shortcut.quit",
        General,
        &[],
        "cmd-q",
        "ctrl-shift-q",
        Quit
    ),
    // Ctrl+Shift+T elsewhere, as Windows Terminal, GNOME Terminal and
    // Konsole open a tab: Ctrl+T is the shell's in a terminal (readline,
    // vim's indent), so it opened none from one (issue #163).
    shortcut!(
        "new-local-terminal",
        "app.shortcut.new_local_terminal",
        Tabs,
        &[],
        "cmd-t",
        "ctrl-shift-t",
        NewLocalTerminal
    ),
    // As Windows Terminal duplicates a tab (issue #137).
    shortcut!(
        "duplicate-terminal",
        "app.shortcut.duplicate_terminal",
        Tabs,
        &[],
        "cmd-shift-d",
        "ctrl-shift-d",
        DuplicateFrontTerminal
    ),
    // S for SFTP: Ctrl+Shift+F is 查找 in a terminal.
    shortcut!(
        "open-sftp",
        "app.shortcut.open_sftp",
        Tabs,
        &[],
        "cmd-shift-s",
        "ctrl-shift-s",
        OpenFrontTerminalSftp
    ),
    // R as a browser reloads (issue #151). Not ⌘R or Ctrl+R: the file
    // lists' 刷新, and in a terminal the shell's history search.
    shortcut!(
        "reconnect-terminal",
        "app.shortcut.reconnect_terminal",
        Tabs,
        &[],
        "cmd-shift-r",
        "ctrl-shift-r",
        ReconnectFrontTerminal
    ),
    // 同步浏览 in the SFTP tab in front. WinSCP's Ctrl+Alt+B shows or hides
    // the right sidebar here: none until one is chosen.
    shortcut!(
        "sync-browsing",
        "app.shortcut.sync_browsing",
        Tabs,
        &[],
        "",
        "",
        ToggleFrontSyncBrowsing
    ),
    // Ctrl+Shift+W elsewhere, as Windows Terminal, GNOME Terminal and
    // Konsole close a tab: Ctrl+W is the shell's in a terminal (readline,
    // vim's windows, nano's search), so it would close nothing there.
    shortcut!(
        "close-tab",
        "app.shortcut.close_tab",
        Tabs,
        &[],
        "cmd-w",
        "ctrl-shift-w",
        CloseActiveTab
    ),
    // macOS reports ⌘⇧] as ⌘}, as Safari and Terminal take it.
    shortcut!(
        "next-tab",
        "app.shortcut.next_tab",
        Tabs,
        &[],
        "cmd-}",
        "ctrl-tab",
        NextTab
    ),
    shortcut!(
        "previous-tab",
        "app.shortcut.previous_tab",
        Tabs,
        &[],
        "cmd-{",
        "ctrl-shift-tab",
        PreviousTab
    ),
    Shortcut {
        id: "switch-to-tab",
        label: "app.shortcut.switch_to_tab",
        group: ShortcutGroup::Tabs,
        contexts: &[],
        mac: "cmd-1",
        other: "alt-1",
        numbered: true,
        system_wide: false,
        action: |digit| Box::new(SwitchToTab(digit)),
    },
    shortcut!(
        "terminal-copy",
        "app.shortcut.terminal_copy",
        Terminal,
        TERMINAL,
        "cmd-c",
        "ctrl-shift-c",
        CopyTerminal
    ),
    shortcut!(
        "terminal-paste",
        "app.shortcut.terminal_paste",
        Terminal,
        TERMINAL,
        "cmd-v",
        "ctrl-shift-v",
        PasteTerminal
    ),
    shortcut!(
        "terminal-select-all",
        "app.shortcut.terminal_select_all",
        Terminal,
        TERMINAL,
        "cmd-a",
        "ctrl-shift-a",
        SelectAllTerminal
    ),
    shortcut!(
        "terminal-find",
        "app.shortcut.terminal_find",
        Terminal,
        TERMINAL_AND_FIND,
        "cmd-f",
        "ctrl-shift-f",
        FindInTerminal
    ),
    shortcut!(
        "terminal-find-next",
        "app.shortcut.terminal_find_next",
        Terminal,
        TERMINAL_AND_FIND,
        "cmd-g",
        "f3",
        FindNextInTerminal
    ),
    shortcut!(
        "terminal-find-previous",
        "app.shortcut.terminal_find_previous",
        Terminal,
        TERMINAL_AND_FIND,
        "cmd-shift-g",
        "shift-f3",
        FindPreviousInTerminal
    ),
    shortcut!(
        "terminal-clear",
        "app.shortcut.terminal_clear",
        Terminal,
        TERMINAL,
        "cmd-k",
        "ctrl-shift-k",
        ClearTerminal
    ),
    // Through the scrollback, as in xterm, GNOME Terminal and Alacritty;
    // one key everywhere, fn+↑ on a Mac's laptop keyboard (issue #148).
    shortcut!(
        "terminal-page-up",
        "app.shortcut.terminal_page_up",
        Terminal,
        TERMINAL,
        "shift-pageup",
        "shift-pageup",
        ScrollTerminalPageUp
    ),
    shortcut!(
        "terminal-page-down",
        "app.shortcut.terminal_page_down",
        Terminal,
        TERMINAL,
        "shift-pagedown",
        "shift-pagedown",
        ScrollTerminalPageDown
    ),
    // The interface zoom keys, which in a terminal size its text, as in
    // iTerm2 and Windows Terminal.
    shortcut!(
        "terminal-zoom-in",
        "app.shortcut.terminal_zoom_in",
        Terminal,
        TERMINAL_AND_FIND,
        "cmd-=",
        "ctrl-=",
        ZoomTerminalIn
    ),
    shortcut!(
        "terminal-zoom-out",
        "app.shortcut.terminal_zoom_out",
        Terminal,
        TERMINAL_AND_FIND,
        "cmd--",
        "ctrl--",
        ZoomTerminalOut
    ),
    shortcut!(
        "terminal-zoom-reset",
        "app.shortcut.terminal_zoom_reset",
        Terminal,
        TERMINAL_AND_FIND,
        "cmd-0",
        "ctrl-0",
        ZoomTerminalReset
    ),
    shortcut!(
        "editor-save",
        "app.shortcut.editor_save",
        Editor,
        EDITOR,
        "cmd-s",
        "ctrl-s",
        EditorShortcut(EditorCommand::Save)
    ),
];

impl Shortcut {
    pub fn find(id: &str) -> Option<&'static Shortcut> {
        SHORTCUTS.iter().find(|shortcut| shortcut.id == id)
    }

    pub fn in_group(group: ShortcutGroup) -> impl Iterator<Item = &'static Shortcut> {
        SHORTCUTS
            .iter()
            .filter(move |shortcut| shortcut.group == group)
    }

    pub fn id(&self) -> &'static str {
        self.id
    }

    pub fn label(&self) -> SharedString {
        t!(self.label)
    }

    pub fn is_numbered(&self) -> bool {
        self.numbered
    }

    pub fn is_system_wide(&self) -> bool {
        self.system_wide
    }

    /// Its key on this platform unless the user changed it; empty for
    /// none.
    pub fn default_keys(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            self.mac
        } else {
            self.other
        }
    }

    fn is_global(&self) -> bool {
        self.contexts.is_empty()
    }

    /// Whether `keys` for it go to the shell in a terminal rather than do
    /// it: Ctrl with a letter, for a shortcut that works anywhere.
    pub fn left_to_the_shell(&self, keys: &str) -> bool {
        self.is_global()
            && !self.system_wide
            && parse_keys(keys).is_ok_and(|keystroke| is_control_character(&keystroke))
    }

    /// Whether it and `other` can be in effect at the same place: both
    /// anywhere, or in a key context they share. Anywhere and a context
    /// may share a key; the context's wins there, as ⌘= does in a terminal.
    /// The system's key wins everywhere.
    fn shares_scope(&self, other: &Shortcut) -> bool {
        self.system_wide
            || other.system_wide
            || (self.is_global() && other.is_global())
            || self
                .contexts
                .iter()
                .any(|context| other.contexts.contains(context))
    }

    /// Each keystroke it is bound to under `keys`: one, or one per digit.
    fn keystrokes(&self, keys: &str) -> Vec<(Keystroke, usize)> {
        let Ok(keystroke) = parse_keys(keys) else {
            return Vec::new();
        };
        if !self.numbered {
            return vec![(keystroke, 0)];
        }
        (1..=9)
            .map(|digit| {
                let mut keystroke = keystroke.clone();
                keystroke.key = digit.to_string();
                (keystroke, digit)
            })
            .collect()
    }

    fn bindings(&self, overrides: &ShortcutOverrides, cx: &App) -> Vec<KeyBinding> {
        let Some(keys) = overrides.keys(self).filter(|_| !self.system_wide) else {
            return Vec::new();
        };
        let contexts: Vec<Option<&str>> = if self.is_global() {
            vec![None]
        } else {
            self.contexts.iter().copied().map(Some).collect()
        };
        let mut bindings = Vec::new();
        for context in contexts {
            for (keystroke, digit) in self.keystrokes(&keys) {
                bindings.extend(load_binding(
                    &unparse_keys(&keystroke),
                    (self.action)(digit),
                    context,
                    cx,
                ));
            }
        }
        bindings
    }
}

/// The user's changes to the shortcuts, by id: a key, or none for a
/// shortcut turned off. A shortcut not in it has its default. A key this
/// version cannot read is dropped on its own, keeping the rest; an id it
/// does not know is kept, for the version that does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ShortcutOverrides(BTreeMap<String, Option<String>>);

impl<'de> Deserialize<'de> for ShortcutOverrides {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        Ok(Self(
            values
                .into_iter()
                .filter_map(|(id, value)| match value {
                    serde_json::Value::Null => Some((id, None)),
                    serde_json::Value::String(keys) if parse_keys(&keys).is_ok() => {
                        Some((id, Some(keys)))
                    }
                    _ => None,
                })
                .collect(),
        ))
    }
}

impl ShortcutOverrides {
    /// What `shortcut` is bound to: the user's key, its default, or none
    /// when turned off or it has no default.
    pub fn keys(&self, shortcut: &Shortcut) -> Option<String> {
        match self.0.get(shortcut.id) {
            Some(keys) => keys.clone(),
            None => Some(shortcut.default_keys().to_string()).filter(|keys| !keys.is_empty()),
        }
    }

    pub fn is_default(&self, shortcut: &Shortcut) -> bool {
        !self.0.contains_key(shortcut.id)
    }

    pub fn is_disabled(&self, shortcut: &Shortcut) -> bool {
        matches!(self.0.get(shortcut.id), Some(None))
    }

    /// Bind `shortcut` to `keys`. Its default is stored as no change.
    pub fn set(&mut self, shortcut: &Shortcut, keys: String) {
        if same_keys(&keys, shortcut.default_keys()) {
            self.reset(shortcut);
        } else {
            self.0.insert(shortcut.id.to_string(), Some(keys));
        }
    }

    pub fn disable(&mut self, shortcut: &Shortcut) {
        self.0.insert(shortcut.id.to_string(), None);
    }

    pub fn reset(&mut self, shortcut: &Shortcut) {
        self.0.remove(shortcut.id);
    }
}

fn same_keys(a: &str, b: &str) -> bool {
    match (parse_keys(a), parse_keys(b)) {
        (Ok(a), Ok(b)) => same_keystroke(&a, &b),
        _ => false,
    }
}

/// The same key with the same modifiers; what the key types does not
/// matter (⌥S types ß on macOS).
fn same_keystroke(a: &Keystroke, b: &Keystroke) -> bool {
    a.modifiers == b.modifiers && a.key == b.key
}

/// Why a key cannot be a shortcut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A plain key, or Shift with one: it would stop typing it.
    NeedsModifier,
    /// 切换到标签 1…9 takes a modifier with a digit.
    NeedsDigit,
    /// Another shortcut, this id's, has it where this one works.
    UsedBy(&'static str),
    /// A text field, list or dialog has it.
    Reserved,
    /// The system would not give it to a shortcut that works in any app:
    /// another app has it, or the system knows no such key.
    Unavailable,
}

impl Refusal {
    /// What to tell the user who pressed `keys`.
    pub fn message(&self, keys: &str) -> String {
        match self {
            Self::NeedsModifier if cfg!(target_os = "macos") => {
                t!("app.shortcut_refusal.needs_modifier_mac").to_string()
            }
            Self::NeedsModifier => t!("app.shortcut_refusal.needs_modifier").to_string(),
            Self::NeedsDigit if cfg!(target_os = "macos") => {
                t!("app.shortcut_refusal.needs_digit_mac").to_string()
            }
            Self::NeedsDigit => t!("app.shortcut_refusal.needs_digit").to_string(),
            Self::UsedBy(id) => t!(
                "app.shortcut_refusal.used_by",
                keys = keys,
                label = Shortcut::find(id).map(Shortcut::label).unwrap_or_default()
            )
            .to_string(),
            Self::Reserved => t!("app.shortcut_refusal.reserved", keys = keys).to_string(),
            Self::Unavailable => t!("app.shortcut_refusal.unavailable", keys = keys).to_string(),
        }
    }
}

/// The key to store for `shortcut` when the user pressed `keystroke`, or
/// why it cannot be. `overrides` are the user's shortcuts now, `fixed` the
/// bindings that cannot be changed (gpui-kit's, and those of ours that
/// apply anywhere).
pub fn check(
    shortcut: &Shortcut,
    keystroke: &Keystroke,
    overrides: &ShortcutOverrides,
    fixed: &[KeyBinding],
) -> Result<String, Refusal> {
    let modifiers = keystroke.modifiers;
    let held = modifiers.platform || modifiers.control || modifiers.alt;
    let mut keystroke = Keystroke {
        modifiers,
        key: keystroke.key.clone(),
        key_char: None,
    };
    if shortcut.numbered {
        // Any digit stands for all nine.
        let digit = keystroke.key.parse::<u8>().ok();
        if !held || !digit.is_some_and(|digit| (1..=9).contains(&digit)) {
            return Err(Refusal::NeedsDigit);
        }
        keystroke.key = "1".into();
    } else if !held
        && !is_function_key(&keystroke.key)
        && !(modifiers.shift && is_editing_key(&keystroke.key))
    {
        return Err(Refusal::NeedsModifier);
    }
    let keys = unparse_keys(&keystroke);
    let pressed = shortcut.keystrokes(&keys);
    for other in SHORTCUTS.iter() {
        if std::ptr::eq(other, shortcut) || !other.shares_scope(shortcut) {
            continue;
        }
        let Some(other_keys) = overrides.keys(other) else {
            continue;
        };
        let taken = other.keystrokes(&other_keys);
        if pressed.iter().any(|(pressed, _)| {
            taken
                .iter()
                .any(|(taken, _)| same_keystroke(pressed, taken))
        }) {
            return Err(Refusal::UsedBy(other.id));
        }
    }
    // One that works anywhere would win over a text field's or a list's
    // key, so those keep theirs.
    if shortcut.is_global() {
        let reserved = fixed.iter().any(|binding| {
            let [first] = binding.keystrokes() else {
                return false;
            };
            pressed
                .iter()
                .any(|(pressed, _)| same_keystroke(pressed, first.inner()))
        });
        if reserved {
            return Err(Refusal::Reserved);
        }
    }
    // The terminal's own keys, as Shift+Insert pasting outside macOS, are
    // bound after the shortcuts and would win in a terminal.
    if shortcut.contexts.contains(&TERMINAL_KEY_CONTEXT) {
        let action = (shortcut.action)(0);
        let reserved = terminal_key_bindings().iter().any(|binding| {
            let [first] = binding.keystrokes() else {
                return false;
            };
            !binding.action().partial_eq(action.as_ref())
                && pressed
                    .iter()
                    .any(|(pressed, _)| same_keystroke(pressed, first.inner()))
        });
        if reserved {
            return Err(Refusal::Reserved);
        }
    }
    Ok(keys)
}

/// F1–F12, which type nothing. The keys above F12 are left out: hardly a
/// keyboard has them.
fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=12).contains(&number))
}

/// Keys that type nothing with Shift either, as Shift+Insert pastes on
/// Windows and Linux. Shift with an arrow stays out: it selects text in a
/// field, and programs in a terminal use it.
fn is_editing_key(key: &str) -> bool {
    matches!(
        key,
        "insert" | "delete" | "home" | "end" | "pageup" | "pagedown"
    )
}

/// The keys of a shortcut, one cap each, in the platform's order: ⌃⌥⇧⌘
/// on macOS, Ctrl Alt Shift Win elsewhere. A numbered shortcut's key
/// shows as 1…9.
pub fn key_caps(keys: &str, numbered: bool) -> Vec<SharedString> {
    let Ok(keystroke) = parse_keys(keys) else {
        return Vec::new();
    };
    let modifiers = keystroke.modifiers;
    let mac = cfg!(target_os = "macos");
    let mut caps: Vec<SharedString> = [
        (modifiers.control, "⌃", "Ctrl"),
        (modifiers.alt, "⌥", "Alt"),
        (modifiers.shift, "⇧", "Shift"),
        (modifiers.platform, "⌘", "Win"),
    ]
    .into_iter()
    .filter(|(held, _, _)| *held)
    .map(|(_, symbol, name)| SharedString::from(if mac { symbol } else { name }))
    .collect();
    caps.push(if numbered {
        "1…9".into()
    } else {
        Kbd::format(&Keystroke {
            modifiers: Modifiers::default(),
            key: keystroke.key,
            key_char: None,
        })
        .into()
    });
    caps
}

/// The keys as one piece of text, for messages: ⇧⌘N, or Ctrl+Shift+N.
pub fn key_text(keys: &str, numbered: bool) -> String {
    let separator = if cfg!(target_os = "macos") { "" } else { "+" };
    key_caps(keys, numbered).join(separator)
}

/// gpui-kit's own bindings, as they were before ours.
struct KitBindings(Vec<KeyBinding>);

impl Global for KitBindings {}

/// The changes the keymap was last built with.
#[derive(PartialEq)]
pub(super) struct AppliedShortcuts(ShortcutOverrides);

impl Global for AppliedShortcuts {}

/// Keep gpui-kit's bindings, right after `gpui_kit::init`, for rebuilding.
pub(super) fn keep_kit_bindings(cx: &mut App) {
    let bindings: Vec<KeyBinding> = cx.key_bindings().borrow().bindings().cloned().collect();
    cx.set_global(KitBindings(bindings));
}

/// Bind the keys: gpui-kit's, then ours with the user's changes. Does
/// nothing when they are the ones in place.
pub fn apply_shortcuts(overrides: &ShortcutOverrides, cx: &mut App) {
    if cx
        .try_global::<AppliedShortcuts>()
        .is_some_and(|applied| applied.0 == *overrides)
    {
        return;
    }
    let kit = cx
        .try_global::<KitBindings>()
        .map(|kit| kit.0.clone())
        .unwrap_or_default();
    let ours = key_bindings(overrides, cx);
    cx.clear_key_bindings();
    cx.bind_keys(kit);
    cx.bind_keys(ours);
    super::system_hotkey::take(overrides.keys(system_wide()).as_deref(), cx);
    cx.set_global(AppliedShortcuts(overrides.clone()));
}

/// Take 显示或隐藏 ShellRS's key from the system from now on, the one the
/// shortcuts were last applied with first. The app's to call, not tests'.
pub fn listen_for_system_hotkey(cx: &mut App) {
    super::system_hotkey::listen(cx);
    let keys = cx
        .try_global::<AppliedShortcuts>()
        .and_then(|applied| applied.0.keys(system_wide()));
    super::system_hotkey::take(keys.as_deref(), cx);
}

/// 显示或隐藏 ShellRS, the one shortcut the system hears.
fn system_wide() -> &'static Shortcut {
    SHORTCUTS
        .iter()
        .find(|shortcut| shortcut.system_wide)
        .expect("a system-wide shortcut")
}

/// The bindings no user change touches that a shortcut working anywhere
/// must not take: gpui-kit's, and ours that work anywhere.
pub fn fixed_bindings(cx: &App) -> Vec<KeyBinding> {
    let mut bindings = cx
        .try_global::<KitBindings>()
        .map(|kit| kit.0.clone())
        .unwrap_or_default();
    bindings.extend(fixed_global_bindings());
    bindings
}

/// Ours: the shortcuts, then the keys that stay. The ones that work
/// anywhere come first: for one key, a key context's binding added later
/// wins there, as the terminal's ⌘= and the preview's do.
fn key_bindings(overrides: &ShortcutOverrides, cx: &App) -> Vec<KeyBinding> {
    let mut bindings: Vec<KeyBinding> = SHORTCUTS
        .iter()
        .filter(|shortcut| shortcut.is_global())
        .flat_map(|shortcut| shortcut.bindings(overrides, cx))
        .collect();
    bindings.extend(fixed_global_bindings());
    bindings.extend(shell_keys_in_terminal(&bindings, cx));
    bindings.extend(
        SHORTCUTS
            .iter()
            .filter(|shortcut| !shortcut.is_global())
            .flat_map(|shortcut| shortcut.bindings(overrides, cx)),
    );
    bindings.extend(fixed_context_bindings());
    bindings
}

/// Ctrl with a letter is the shell's in a terminal (tmux's Ctrl+B,
/// readline's Ctrl+W and Ctrl+K), so a shortcut that works anywhere leaves
/// it there. The terminal's own shortcuts, bound after, still win in it.
fn shell_keys_in_terminal(anywhere: &[KeyBinding], cx: &App) -> Vec<KeyBinding> {
    anywhere
        .iter()
        .filter_map(|binding| {
            let [keystroke] = binding.keystrokes() else {
                return None;
            };
            let keystroke = keystroke.inner();
            if !is_control_character(keystroke) {
                return None;
            }
            load_binding(
                &unparse_keys(keystroke),
                Box::new(NoAction {}),
                Some(TERMINAL_KEY_CONTEXT),
                cx,
            )
        })
        .collect()
}

/// Ctrl with a letter alone: a control character in a terminal (Ctrl+T is
/// 0x14), which every program there may use. Ctrl+Shift with a letter is
/// the same character, so no program counts on it: the app's keys go there.
fn is_control_character(keystroke: &Keystroke) -> bool {
    let modifiers = keystroke.modifiers;
    keystroke.key.len() == 1
        && keystroke.key.bytes().all(|byte| byte.is_ascii_lowercase())
        && modifiers.control
        && !modifiers.alt
        && !modifiers.shift
        && !modifiers.platform
}

/// A binding for keys known only when it is made, as the user's.
/// `KeyBinding::new` takes only literals since gpui-pre 0.3.12; `load`
/// reads ⌘ for this system and maps the keys as the keyboard types them.
fn load_binding(
    keys: &str,
    action: Box<dyn Action>,
    context: Option<&str>,
    cx: &App,
) -> Option<KeyBinding> {
    let predicate = context
        .and_then(|context| KeyBindingContextPredicate::parse(context).ok())
        .map(Rc::new);
    KeyBinding::load(keys, action, predicate, false, None, cx).ok()
}

/// `cmd-` on macOS and `ctrl-` elsewhere, then `key`: a literal, as
/// `KeyBinding::new` takes.
macro_rules! primary {
    ($key:literal) => {
        if cfg!(target_os = "macos") {
            concat!("cmd-", $key)
        } else {
            concat!("ctrl-", $key)
        }
    };
}

fn fixed_global_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![KeyBinding::new("shift-escape", ToggleZoom, None)];
    // The keys of the menu bar's items (`menu_bar`) that every Mac app has.
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("cmd-alt-h", HideOtherApps, None),
            KeyBinding::new("cmd-m", MinimizeWindow, None),
        ]);
    }
    bindings
}

fn fixed_context_bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new(
            primary!("s"),
            KubernetesAction(crate::kubernetes::KubernetesCommand::SaveYaml),
            Some(crate::kubernetes::YAML_KEY_CONTEXT),
        ),
        // Over a previewed image, the interface zoom keys zoom the image;
        // 0 and 9 as in macOS Preview.
        KeyBinding::new(primary!("="), ZoomPreviewIn, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new(primary!("-"), ZoomPreviewOut, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new(
            primary!("0"),
            ActualSizePreview,
            Some(IMAGE_PREVIEW_CONTEXT),
        ),
        KeyBinding::new(primary!("9"), FitPreview, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("enter", ConnectSelected, Some(HOST_PANEL_CONTEXT)),
        KeyBinding::new("up", SelectPreviousHostRow, Some(HOST_TREE_CONTEXT)),
        KeyBinding::new("down", SelectNextHostRow, Some(HOST_TREE_CONTEXT)),
        KeyBinding::new("left", CollapseSelectedGroup, Some(HOST_TREE_CONTEXT)),
        KeyBinding::new("right", ExpandSelectedGroup, Some(HOST_TREE_CONTEXT)),
        KeyBinding::new("enter", ConnectSelected, Some(RECENT_HOSTS_CONTEXT)),
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
        KeyBinding::new(
            "escape",
            DismissTerminalFind,
            Some(TERMINAL_FIND_KEY_CONTEXT),
        ),
        // The 查找文件 window closes as a dialog would, and as a window does.
        KeyBinding::new("escape", CloseFindFiles, Some(FIND_FILES_CONTEXT)),
        KeyBinding::new(primary!("w"), CloseFindFiles, Some(FIND_FILES_CONTEXT)),
    ];
    bindings.extend(terminal_key_bindings());
    bindings.extend(file_list_key_bindings());
    bindings.extend(find_results_key_bindings());
    bindings
}

/// 查找文件's results take the remote file list's keys for what they offer:
/// moving and selecting, 定位 on Return, 编辑, 下载 and 删除. Bound at the
/// table, a level below the window, so the fields keep Return and the rest.
fn find_results_key_bindings() -> Vec<KeyBinding> {
    let table = format!("{FIND_FILES_CONTEXT} > DataTable");
    let remote = true;
    let bind = |keys: &'static str, command: ExplorerCommand| {
        KeyBinding::new(keys, ExplorerShortcut(command), Some(table.as_str()))
    };
    let mut bindings = vec![
        bind("enter", ExplorerCommand::Open { remote }),
        bind("f4", ExplorerCommand::Edit { remote, path: None }),
        bind("f5", ExplorerCommand::Transfer { remote }),
        bind("delete", ExplorerCommand::Delete { remote }),
        bind("f8", ExplorerCommand::Delete { remote }),
        #[cfg(target_os = "macos")]
        bind("cmd-backspace", ExplorerCommand::Delete { remote }),
        bind("space", ExplorerCommand::ToggleSelection { remote }),
        bind("insert", ExplorerCommand::ToggleSelection { remote }),
        bind(primary!("a"), ExplorerCommand::SelectAll { remote }),
        KeyBinding::new("left", NoAction {}, Some(table.as_str())),
        KeyBinding::new("right", NoAction {}, Some(table.as_str())),
    ];
    for (keys, motion, extend) in cursor_motion_keys() {
        bindings.push(bind(
            keys,
            ExplorerCommand::MoveCursor {
                remote,
                motion,
                extend,
            },
        ));
    }
    bindings
}

/// The keys that move the cursor in a file list, each alone and with Shift
/// to extend the selection.
fn cursor_motion_keys() -> [(&'static str, crate::explorer::CursorMotion, bool); 12] {
    use crate::explorer::CursorMotion;
    [
        ("up", CursorMotion::Up, false),
        ("shift-up", CursorMotion::Up, true),
        ("down", CursorMotion::Down, false),
        ("shift-down", CursorMotion::Down, true),
        ("pageup", CursorMotion::PageUp, false),
        ("shift-pageup", CursorMotion::PageUp, true),
        ("pagedown", CursorMotion::PageDown, false),
        ("shift-pagedown", CursorMotion::PageDown, true),
        ("home", CursorMotion::Home, false),
        ("shift-home", CursorMotion::Home, true),
        ("end", CursorMotion::End, false),
        ("shift-end", CursorMotion::End, true),
    ]
}

/// WinSCP's file-panel keys, bound once per pane with the side baked into the
/// command. The movement keys are bound one level deeper, at the table, so
/// they replace `DataTable`'s own single-row selection keys.
fn file_list_key_bindings() -> Vec<KeyBinding> {
    let mut bindings = Vec::new();
    for (context, remote) in [
        (LOCAL_FILE_LIST_CONTEXT, false),
        (REMOTE_FILE_LIST_CONTEXT, true),
    ] {
        let table = format!("{context} > DataTable");
        let bind = |keys: &'static str, command: ExplorerCommand, context: &str| {
            KeyBinding::new(keys, ExplorerShortcut(command), Some(context))
        };
        // 显示隐藏文件: Finder's ⌘⇧. (macOS reports it as ⌘>), WinSCP's
        // Ctrl+Alt+H elsewhere.
        let side = crate::explorer::PaneSide::from_remote(remote);
        #[cfg(target_os = "macos")]
        bindings.push(KeyBinding::new(
            "cmd->",
            ToggleHiddenFiles(side),
            Some(context),
        ));
        #[cfg(not(target_os = "macos"))]
        bindings.push(KeyBinding::new(
            "ctrl-alt-h",
            ToggleHiddenFiles(side),
            Some(context),
        ));
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
        // 查找文件, as ⌘F finds in a terminal; the remote side only, as in
        // WinSCP.
        if remote {
            bindings.push(bind(primary!("f"), ExplorerCommand::FindFiles, context));
            // Files copied in File Explorer or Finder, uploaded (issue #129);
            // WinSCP's Shift+Insert as well outside macOS.
            #[cfg(not(target_os = "macos"))]
            bindings.push(bind("shift-insert", ExplorerCommand::PasteFiles, context));
            bindings.push(bind(primary!("v"), ExplorerCommand::PasteFiles, context));
        }
        bindings.extend([
            bind("backspace", ExplorerCommand::Up { remote }, context),
            bind(primary!("\\"), ExplorerCommand::Root { remote }, context),
            bind(primary!("r"), ExplorerCommand::Refresh { remote }, context),
            bind("alt-left", ExplorerCommand::Back { remote }, context),
            bind("alt-right", ExplorerCommand::Forward { remote }, context),
            // WinSCP's Ctrl+B already toggles the host panel here.
            bind(
                primary!("d"),
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
            // WinSCP's 编辑.
            bind("f4", ExplorerCommand::Edit { remote, path: None }, context),
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
                primary!("a"),
                ExplorerCommand::SelectAll { remote },
                context,
            ),
            // WinSCP's 打开目录/书签.
            bind(
                primary!("o"),
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
        for (keys, motion, extend) in cursor_motion_keys() {
            bindings.push(bind(
                keys,
                ExplorerCommand::MoveCursor {
                    remote,
                    motion,
                    extend,
                },
                &table,
            ));
        }
    }
    bindings
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Action as _, KeyBinding, Keystroke, NoAction, TestAppContext};

    use super::{
        Refusal, SHORTCUTS, Shortcut, ShortcutOverrides, check, key_caps, shell_keys_in_terminal,
    };
    use crate::app::{CloseActiveTab, parse_keys, unparse_keys};

    fn shortcut(id: &str) -> &'static Shortcut {
        Shortcut::find(id).unwrap()
    }

    fn keys(text: &str) -> Keystroke {
        parse_keys(text).unwrap()
    }

    fn written(text: &str) -> String {
        unparse_keys(&keys(text))
    }

    fn key_bindings(overrides: &ShortcutOverrides, cx: &mut TestAppContext) -> Vec<KeyBinding> {
        cx.update(|cx| super::key_bindings(overrides, cx))
    }

    /// cmd on macOS, ctrl elsewhere, as the defaults are.
    fn primary(key: &str) -> String {
        let modifier = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        format!("{modifier}-{key}")
    }

    /// Each binding as its keys, action and context.
    fn described(bindings: &[KeyBinding]) -> Vec<(String, String, Option<String>)> {
        bindings
            .iter()
            .map(|binding| {
                (
                    unparse_keys(binding.keystrokes()[0].inner()),
                    binding.action().name().to_string(),
                    binding.predicate().map(|predicate| predicate.to_string()),
                )
            })
            .collect()
    }

    fn position(bindings: &[(String, String, Option<String>)], action: &str) -> usize {
        bindings
            .iter()
            .position(|(_, name, _)| name == action)
            .unwrap_or_else(|| panic!("{action} is not bound"))
    }

    /// No default would be refused if the user pressed it: none takes
    /// another's key where both work, or one the app keeps.
    #[test]
    fn every_default_is_free_where_it_works() {
        let overrides = ShortcutOverrides::default();
        let fixed = super::fixed_global_bindings();
        for shortcut in SHORTCUTS
            .iter()
            .filter(|shortcut| !shortcut.system_wide && !shortcut.default_keys().is_empty())
        {
            let pressed = keys(shortcut.default_keys());
            assert!(
                check(shortcut, &pressed, &overrides, &fixed).is_ok(),
                "{}",
                shortcut.id
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        for (ix, shortcut) in SHORTCUTS.iter().enumerate() {
            assert!(
                SHORTCUTS[ix + 1..]
                    .iter()
                    .all(|other| other.id != shortcut.id),
                "{} twice",
                shortcut.id
            );
        }
    }

    #[gpui_kit::test]
    fn the_defaults_bind_each_shortcut_once_and_a_context_wins_its_own_key(
        cx: &mut TestAppContext,
    ) {
        let bindings = described(&key_bindings(&ShortcutOverrides::default(), cx));
        assert!(bindings.contains(&(
            written(shortcut("new-host").default_keys()),
            "shellrs::NewHost".into(),
            None
        )));
        // 切换到标签 1…9: one binding a digit.
        assert_eq!(
            bindings
                .iter()
                .filter(|(_, name, _)| name == "shellrs::SwitchToTab")
                .count(),
            9
        );
        // The find keys work in the terminal and its find bar.
        assert_eq!(
            bindings
                .iter()
                .filter(|(_, name, _)| name == "shellrs::FindInTerminal")
                .count(),
            2
        );
        // The same key in a context is bound after the one for anywhere,
        // so it wins there.
        assert!(
            position(&bindings, "shellrs::ZoomPreviewIn") > position(&bindings, "shellrs::ZoomIn")
        );
        assert!(
            position(&bindings, "shellrs::ZoomTerminalIn") > position(&bindings, "shellrs::ZoomIn")
        );
    }

    #[gpui_kit::test]
    fn a_change_rebinds_turns_off_or_goes_back(cx: &mut TestAppContext) {
        let new_host = shortcut("new-host");
        let mut overrides = ShortcutOverrides::default();
        overrides.set(new_host, primary("j"));
        let bindings = described(&key_bindings(&overrides, cx));
        assert_eq!(
            bindings[position(&bindings, "shellrs::NewHost")].0,
            written(&primary("j"))
        );
        overrides.disable(new_host);
        assert!(overrides.is_disabled(new_host));
        let bindings = described(&key_bindings(&overrides, cx));
        assert!(
            !bindings
                .iter()
                .any(|(_, name, _)| name == "shellrs::NewHost")
        );
        // Its default again is no change at all.
        overrides.set(new_host, new_host.default_keys().into());
        assert!(overrides.is_default(new_host));
        assert_eq!(overrides, ShortcutOverrides::default());
    }

    #[test]
    fn a_change_this_version_cannot_read_costs_only_itself() {
        let overrides: ShortcutOverrides = serde_json::from_str(
            r#"{"new-host": "cmd-j", "quit": null, "close-tab": 7, "later": "cmd-l"}"#,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&overrides).unwrap(),
            serde_json::json!({"new-host": "cmd-j", "quit": null, "later": "cmd-l"})
        );
        assert_eq!(overrides.keys(shortcut("quit")), None);
        assert!(overrides.is_default(shortcut("close-tab")));
    }

    #[gpui_kit::test]
    fn ctrl_with_a_letter_stays_the_shells_in_a_terminal(cx: &mut TestAppContext) {
        let mut overrides = ShortcutOverrides::default();
        overrides.set(shortcut("new-local-terminal"), "ctrl-t".into());
        overrides.set(shortcut("toggle-sidebar"), "ctrl-shift-b".into());
        overrides.set(shortcut("terminal-copy"), "ctrl-c".into());
        let bindings = described(&key_bindings(&overrides, cx));
        let in_terminal = |keys: &str| {
            bindings.iter().position(|(bound, name, context)| {
                bound == keys
                    && name == NoAction.name()
                    && context.as_deref() == Some(super::TERMINAL_KEY_CONTEXT)
            })
        };
        assert!(in_terminal("ctrl-t").is_some());
        // Ctrl+Shift with a letter is the app's, as in other terminals.
        assert!(in_terminal("ctrl-shift-b").is_none());
        // The terminal's own shortcut is bound after, and wins there.
        assert!(in_terminal("ctrl-c").is_none());
        assert!(position(&bindings, "shellrs::CopyTerminal") > in_terminal("ctrl-t").unwrap());
    }

    /// A key that does one thing anywhere and another in a terminal is
    /// left only to the zoom keys, which size what they are in (字号).
    /// Clearing a terminal and focusing the search shared ⌘K until issue
    /// #163; no other program does that.
    #[test]
    fn only_the_zoom_keys_mean_another_thing_in_a_terminal() {
        let zoom = |shortcut: &Shortcut| shortcut.id.contains("zoom");
        for anywhere in SHORTCUTS.iter().filter(|shortcut| shortcut.is_global()) {
            for there in SHORTCUTS.iter().filter(|shortcut| !shortcut.is_global()) {
                for (mine, theirs) in [(anywhere.mac, there.mac), (anywhere.other, there.other)] {
                    if !mine.is_empty() && mine == theirs {
                        assert!(
                            zoom(anywhere) && zoom(there),
                            "{} and {} share {mine}",
                            anywhere.id,
                            there.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ctrl_with_a_letter_anywhere_is_left_to_the_shell() {
        let sidebar = shortcut("toggle-sidebar");
        assert!(sidebar.left_to_the_shell("ctrl-b"));
        for keys in [
            "ctrl-shift-b",
            "cmd-b",
            "alt-b",
            "ctrl-alt-b",
            "ctrl-,",
            "ctrl-1",
        ] {
            assert!(!sidebar.left_to_the_shell(keys), "{keys}");
        }
        // The terminal's own win there, and the system's takes the key
        // before any terminal.
        assert!(!shortcut("terminal-paste").left_to_the_shell("ctrl-v"));
        assert!(!shortcut("show-hide-app").left_to_the_shell("ctrl-j"));
        // No default is: the app's keys are ⌘ on macOS and Ctrl+Shift
        // elsewhere, so each works in a terminal too (issue #163).
        for shortcut in &SHORTCUTS {
            for keys in [shortcut.mac, shortcut.other] {
                assert!(!shortcut.left_to_the_shell(keys), "{}: {keys}", shortcut.id);
            }
        }
    }

    #[gpui_kit::test]
    fn tabs_open_and_close_from_a_terminal_with_the_default_keys_everywhere(
        cx: &mut TestAppContext,
    ) {
        for id in ["close-tab", "new-local-terminal"] {
            let tab = shortcut(id);
            for keys in [tab.mac, tab.other] {
                let binding = KeyBinding::new(keys, CloseActiveTab, None);
                let shells = cx.update(|cx| shell_keys_in_terminal(&[binding], cx));
                assert!(shells.is_empty(), "{id}: {keys}");
            }
        }
    }

    #[gpui_kit::test]
    fn the_system_wide_shortcut_has_no_key_until_one_is_chosen(cx: &mut TestAppContext) {
        let show = shortcut("show-hide-app");
        let mut overrides = ShortcutOverrides::default();
        assert_eq!(overrides.keys(show), None);
        assert!(!overrides.is_disabled(show));
        overrides.set(show, "alt-space".into());
        assert_eq!(overrides.keys(show).as_deref(), Some("alt-space"));
        // The system hears it, not the keymap.
        assert!(
            !described(&key_bindings(&overrides, cx))
                .iter()
                .any(|(_, name, _)| name == "shellrs::ShowOrHideApp")
        );
        // It takes the key from everywhere, so no shortcut can share one.
        let copy = shortcut("terminal-copy");
        assert_eq!(
            check(show, &keys(copy.default_keys()), &overrides, &[]),
            Err(Refusal::UsedBy("terminal-copy"))
        );
        assert_eq!(
            check(copy, &keys("alt-space"), &overrides, &[]),
            Err(Refusal::UsedBy("show-hide-app"))
        );
        // ⌃⌘↑ and ⌃⌘↓ are free on macOS; elsewhere the system's Win+↑ and
        // Win+↓ do it.
        for id in ["maximize-window", "restore-window"] {
            let window = shortcut(id);
            if cfg!(target_os = "macos") {
                let default = window.default_keys();
                assert_eq!(
                    check(window, &keys(default), &ShortcutOverrides::default(), &[]),
                    Ok(written(default))
                );
            } else {
                assert_eq!(ShortcutOverrides::default().keys(window), None);
            }
        }
    }

    #[test]
    fn keys_need_a_modifier_and_must_be_free_where_they_work() {
        let overrides = ShortcutOverrides::default();
        let fixed = [KeyBinding::new(primary!("z"), NoAction, Some("Input"))];
        let new_host = shortcut("new-host");
        let check = |shortcut, pressed: &str| check(shortcut, &keys(pressed), &overrides, &fixed);

        assert_eq!(check(new_host, "j"), Err(Refusal::NeedsModifier));
        assert_eq!(check(new_host, "shift-j"), Err(Refusal::NeedsModifier));
        assert_eq!(check(new_host, "f5"), Ok("f5".into()));
        assert_eq!(check(new_host, "f13"), Err(Refusal::NeedsModifier));
        assert_eq!(check(new_host, "alt-j"), Ok("alt-j".into()));
        // Shift with a key that types nothing, as Shift+Insert pastes; alone
        // such a key has its own work, and Shift with an arrow selects.
        let paste = shortcut("terminal-paste");
        assert_eq!(check(paste, "shift-insert"), Ok("shift-insert".into()));
        // Outside macOS the terminal pastes on it already, so another of its
        // shortcuts cannot have it.
        let clear = shortcut("terminal-clear");
        assert_eq!(
            check(clear, "shift-insert"),
            if cfg!(target_os = "macos") {
                Ok("shift-insert".into())
            } else {
                Err(Refusal::Reserved)
            }
        );
        // And Ctrl+Insert copies there (issue #158).
        assert_eq!(
            check(clear, "ctrl-insert"),
            if cfg!(target_os = "macos") {
                Ok("ctrl-insert".into())
            } else {
                Err(Refusal::Reserved)
            }
        );
        assert_eq!(
            check(new_host, "shift-pagedown"),
            Ok("shift-pagedown".into())
        );
        assert_eq!(check(paste, "insert"), Err(Refusal::NeedsModifier));
        assert_eq!(check(paste, "shift-up"), Err(Refusal::NeedsModifier));
        // Taken by another shortcut working anywhere.
        let terminal = shortcut("new-local-terminal");
        assert_eq!(
            check(new_host, terminal.default_keys()),
            Err(Refusal::UsedBy("new-local-terminal"))
        );
        // A text field's key.
        assert_eq!(check(new_host, &primary("z")), Err(Refusal::Reserved));
        let fixed = [KeyBinding::new("shift-home", NoAction, Some("Input"))];
        assert_eq!(
            super::check(new_host, &keys("shift-home"), &overrides, &fixed),
            Err(Refusal::Reserved)
        );
        // A terminal's key may also work anywhere, where it does something
        // else; inside a terminal the terminal's wins.
        let find = shortcut("terminal-find");
        assert_eq!(
            check(new_host, find.default_keys()),
            Ok(written(find.default_keys()))
        );
        // Two of the terminal's own cannot share one.
        let clear = shortcut("terminal-clear");
        assert_eq!(
            check(shortcut("terminal-copy"), clear.default_keys()),
            Err(Refusal::UsedBy("terminal-clear"))
        );

        // 切换到标签 1…9 takes any digit for all nine, and holds them all.
        let tabs = shortcut("switch-to-tab");
        assert_eq!(check(tabs, &primary("j")), Err(Refusal::NeedsDigit));
        assert_eq!(check(tabs, "5"), Err(Refusal::NeedsDigit));
        assert_eq!(check(tabs, "ctrl-alt-5"), Ok("ctrl-alt-1".into()));
        assert_eq!(
            check(new_host, &tabs.default_keys().replace('1', "4")),
            Err(Refusal::UsedBy("switch-to-tab"))
        );
    }

    /// ⌘H, ⌥⌘H and ⌘M are the menu bar's, as in every Mac app: no
    /// shortcut has them, and none can take them.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_menu_bars_keys_stay_its_own() {
        let fixed = super::fixed_global_bindings();
        let overrides = ShortcutOverrides::default();
        for pressed in ["cmd-h", "cmd-alt-h", "cmd-m"] {
            assert_eq!(
                check(shortcut("new-host"), &keys(pressed), &overrides, &fixed),
                Err(Refusal::Reserved),
                "{pressed}"
            );
        }
    }

    #[test]
    fn keys_show_as_caps_in_the_platforms_order() {
        if cfg!(target_os = "macos") {
            assert_eq!(key_caps("cmd-shift-n", false), ["⇧", "⌘", "N"]);
            assert_eq!(key_caps("cmd-alt-b", false), ["⌥", "⌘", "B"]);
            assert_eq!(key_caps("cmd-1", true), ["⌘", "1…9"]);
        } else {
            assert_eq!(key_caps("ctrl-shift-n", false), ["Ctrl", "Shift", "N"]);
            assert_eq!(key_caps("alt-1", true), ["Alt", "1…9"]);
        }
        assert_eq!(key_caps("f3", false), ["F3"]);
    }
}
