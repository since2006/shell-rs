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
            action: |_| Box::new($action),
        }
    };
}

/// The shortcuts of 设置 › 键盘快捷键, in the order the page lists them.
pub static SHORTCUTS: [Shortcut; 25] = [
    shortcut!(
        "new-host",
        "app.shortcut.new_host",
        General,
        &[],
        "cmd-n",
        "ctrl-n",
        NewHost
    ),
    shortcut!(
        "new-group",
        "app.shortcut.new_group",
        General,
        &[],
        "cmd-shift-n",
        "ctrl-shift-n",
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
    shortcut!(
        "toggle-sidebar",
        "app.shortcut.toggle_sidebar",
        General,
        &[],
        "cmd-b",
        "ctrl-b",
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
    // In a terminal ⌘K clears instead, as in other macOS terminals.
    shortcut!(
        "focus-search",
        "app.shortcut.focus_search",
        General,
        &[],
        "cmd-k",
        "ctrl-k",
        FocusSearch
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
        "ctrl-q",
        Quit
    ),
    shortcut!(
        "new-local-terminal",
        "app.shortcut.new_local_terminal",
        Tabs,
        &[],
        "cmd-t",
        "ctrl-t",
        NewLocalTerminal
    ),
    shortcut!(
        "close-tab",
        "app.shortcut.close_tab",
        Tabs,
        &[],
        "cmd-w",
        "ctrl-w",
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

    /// Its key on this platform unless the user changed it.
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

    /// Whether it and `other` can be in effect at the same place: both
    /// anywhere, or in a key context they share. Anywhere and a context
    /// may share a key; the context's wins there, as ⌘K does in a terminal.
    fn shares_scope(&self, other: &Shortcut) -> bool {
        (self.is_global() && other.is_global())
            || self
                .contexts
                .iter()
                .any(|context| other.contexts.contains(context))
    }

    /// Each keystroke it is bound to under `keys`: one, or one per digit.
    fn keystrokes(&self, keys: &str) -> Vec<(Keystroke, usize)> {
        let Ok(keystroke) = Keystroke::parse(keys) else {
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

    fn bindings(&self, overrides: &ShortcutOverrides) -> Vec<KeyBinding> {
        let Some(keys) = overrides.keys(self) else {
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
                let predicate = context
                    .and_then(|context| KeyBindingContextPredicate::parse(context).ok())
                    .map(Rc::new);
                if let Ok(binding) = KeyBinding::load(
                    &keystroke.unparse(),
                    (self.action)(digit),
                    predicate,
                    false,
                    None,
                    &DummyKeyboardMapper,
                ) {
                    bindings.push(binding);
                }
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
                    serde_json::Value::String(keys) if Keystroke::parse(&keys).is_ok() => {
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
    /// when turned off.
    pub fn keys(&self, shortcut: &Shortcut) -> Option<String> {
        match self.0.get(shortcut.id) {
            Some(keys) => keys.clone(),
            None => Some(shortcut.default_keys().to_string()),
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
    match (Keystroke::parse(a), Keystroke::parse(b)) {
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
    /// A plain key: it would stop typing it.
    NeedsModifier,
    /// 切换到标签 1…9 takes a modifier with a digit.
    NeedsDigit,
    /// Another shortcut, this id's, has it where this one works.
    UsedBy(&'static str),
    /// A text field, list or dialog has it.
    Reserved,
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
    } else if !held && !is_function_key(&keystroke.key) {
        return Err(Refusal::NeedsModifier);
    }
    let keys = keystroke.unparse();
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
    Ok(keys)
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=24).contains(&number))
}

/// The keys of a shortcut, one cap each, in the platform's order: ⌃⌥⇧⌘
/// on macOS, Ctrl Alt Shift Win elsewhere. A numbered shortcut's key
/// shows as 1…9.
pub fn key_caps(keys: &str, numbered: bool) -> Vec<SharedString> {
    let Ok(keystroke) = Keystroke::parse(keys) else {
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
struct AppliedShortcuts(ShortcutOverrides);

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
    cx.clear_key_bindings();
    cx.bind_keys(kit);
    cx.bind_keys(key_bindings(overrides));
    cx.set_global(AppliedShortcuts(overrides.clone()));
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
/// wins there, as the terminal's ⌘K and the preview's ⌘= do.
fn key_bindings(overrides: &ShortcutOverrides) -> Vec<KeyBinding> {
    let mut bindings: Vec<KeyBinding> = SHORTCUTS
        .iter()
        .filter(|shortcut| shortcut.is_global())
        .flat_map(|shortcut| shortcut.bindings(overrides))
        .collect();
    bindings.extend(fixed_global_bindings());
    bindings.extend(
        SHORTCUTS
            .iter()
            .filter(|shortcut| !shortcut.is_global())
            .flat_map(|shortcut| shortcut.bindings(overrides)),
    );
    bindings.extend(fixed_context_bindings());
    bindings
}

fn fixed_global_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("shift-escape", ToggleZoom, None)]
}

fn fixed_context_bindings() -> Vec<KeyBinding> {
    #[cfg(target_os = "macos")]
    const PRIMARY: &str = "cmd";
    #[cfg(not(target_os = "macos"))]
    const PRIMARY: &str = "ctrl";

    let primary = |key: &str| format!("{PRIMARY}-{key}");
    let mut bindings = vec![
        // Over a previewed image, the interface zoom keys zoom the image;
        // 0 and 9 as in macOS Preview.
        KeyBinding::new(&primary("="), ZoomPreviewIn, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new(&primary("-"), ZoomPreviewOut, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new(
            &primary("0"),
            ActualSizePreview,
            Some(IMAGE_PREVIEW_CONTEXT),
        ),
        KeyBinding::new(&primary("9"), FitPreview, Some(IMAGE_PREVIEW_CONTEXT)),
        KeyBinding::new("enter", ConnectSelected, Some(HOST_PANEL_CONTEXT)),
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
    ];
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
            // WinSCP's Ctrl+B already toggles the host panel here.
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

#[cfg(test)]
mod tests {
    use gpui_kit::{KeyBinding, Keystroke, NoAction};

    use super::{Refusal, SHORTCUTS, Shortcut, ShortcutOverrides, check, key_bindings, key_caps};

    fn shortcut(id: &str) -> &'static Shortcut {
        Shortcut::find(id).unwrap()
    }

    fn keys(text: &str) -> Keystroke {
        Keystroke::parse(text).unwrap()
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
                    binding.keystrokes()[0].inner().unparse(),
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

    #[test]
    fn the_defaults_bind_each_shortcut_once_and_a_context_wins_its_own_key() {
        let bindings = described(&key_bindings(&ShortcutOverrides::default()));
        assert!(bindings.contains(&(
            keys(&primary("n")).unparse(),
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
            position(&bindings, "shellrs::ClearTerminal")
                > position(&bindings, "shellrs::FocusSearch")
        );
        assert!(
            position(&bindings, "shellrs::ZoomPreviewIn") > position(&bindings, "shellrs::ZoomIn")
        );
        assert!(
            position(&bindings, "shellrs::ZoomTerminalIn") > position(&bindings, "shellrs::ZoomIn")
        );
    }

    #[test]
    fn a_change_rebinds_turns_off_or_goes_back() {
        let new_host = shortcut("new-host");
        let mut overrides = ShortcutOverrides::default();
        overrides.set(new_host, primary("j"));
        let bindings = described(&key_bindings(&overrides));
        assert_eq!(
            bindings[position(&bindings, "shellrs::NewHost")].0,
            keys(&primary("j")).unparse()
        );
        overrides.disable(new_host);
        assert!(overrides.is_disabled(new_host));
        let bindings = described(&key_bindings(&overrides));
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

    #[test]
    fn keys_need_a_modifier_and_must_be_free_where_they_work() {
        let overrides = ShortcutOverrides::default();
        let fixed = [KeyBinding::new(&primary("z"), NoAction, Some("Input"))];
        let new_host = shortcut("new-host");
        let check = |shortcut, pressed: &str| check(shortcut, &keys(pressed), &overrides, &fixed);

        assert_eq!(check(new_host, "j"), Err(Refusal::NeedsModifier));
        assert_eq!(check(new_host, "shift-j"), Err(Refusal::NeedsModifier));
        assert_eq!(check(new_host, "f5"), Ok("f5".into()));
        assert_eq!(check(new_host, "alt-j"), Ok("alt-j".into()));
        // Taken by another shortcut working anywhere.
        let terminal = shortcut("new-local-terminal");
        assert_eq!(
            check(new_host, terminal.default_keys()),
            Err(Refusal::UsedBy("new-local-terminal"))
        );
        // A text field's key.
        assert_eq!(check(new_host, &primary("z")), Err(Refusal::Reserved));
        // A terminal's key may also work anywhere, where it does something
        // else; inside a terminal the terminal's wins.
        let find = shortcut("terminal-find");
        assert_eq!(
            check(new_host, find.default_keys()),
            Ok(keys(find.default_keys()).unparse())
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
