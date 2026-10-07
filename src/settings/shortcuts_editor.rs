//! 键盘快捷键: each shortcut with its keys, to change by pressing new ones,
//! turn off, or put back. Changes go straight to the settings; the keymap
//! is rebuilt from them (`app::apply_shortcuts`).

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, Size, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    setting::{RenderOptions, SettingField, SettingGroup, SettingItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    CatalogIcon, Shortcut, ShortcutGroup, ShortcutOverrides, check, fixed_bindings, key_caps,
    key_text,
};
use crate::i18n::t;

use super::SettingsStore;

/// The page's state: the shortcut whose new keys are being pressed, if any.
pub struct ShortcutsEditor {
    store: Entity<SettingsStore>,
    recording: Option<&'static Shortcut>,
    /// The box that says 按下组合键; while it has the keyboard, every key
    /// pressed is taken as the new shortcut instead of doing anything.
    focus_handle: FocusHandle,
    /// What had the keyboard before, given it back once the keys are in.
    previous_focus: Option<FocusHandle>,
    intercept: Option<Subscription>,
    _blur: Subscription,
}

impl ShortcutsEditor {
    pub fn new(store: Entity<SettingsStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        // Clicking anywhere else gives up, as Escape does.
        let blur = cx.on_blur(&focus_handle, window, |this, _, cx| this.stop(cx));
        Self {
            store,
            recording: None,
            focus_handle,
            previous_focus: None,
            intercept: None,
            _blur: blur,
        }
    }

    fn start(&mut self, shortcut: &'static Shortcut, window: &mut Window, cx: &mut Context<Self>) {
        self.previous_focus = window.focused(cx);
        self.recording = Some(shortcut);
        window.focus(&self.focus_handle, cx);
        // Before the keymap: a key bound to something, ⌘W say, would do it
        // before an element's own key listener heard of it.
        let editor = cx.weak_entity();
        let focus = self.focus_handle.clone();
        self.intercept = Some(cx.intercept_keystrokes(move |event, window, cx| {
            if !focus.is_focused(window) {
                return;
            }
            cx.stop_propagation();
            let keystroke = event.keystroke.clone();
            editor
                .update(cx, |editor, cx| editor.record(&keystroke, window, cx))
                .ok();
        }));
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.recording.take().is_some() {
            self.intercept = None;
            cx.notify();
        }
    }

    /// Done pressing keys: back to what had the keyboard before.
    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop(cx);
        if let Some(previous) = self.previous_focus.take() {
            window.focus(&previous, cx);
        }
    }

    fn record(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shortcut) = self.recording else {
            return;
        };
        // A modifier on its own is the start of a combination.
        if matches!(
            keystroke.key.as_str(),
            "shift" | "control" | "alt" | "platform" | "function"
        ) {
            return;
        }
        if keystroke.key == "escape" && !keystroke.modifiers.modified() {
            self.finish(window, cx);
            return;
        }
        let overrides = self.store.read(cx).settings().shortcuts;
        match check(shortcut, keystroke, &overrides, &fixed_bindings(cx)) {
            Ok(keys) => {
                self.change(|overrides| overrides.set(shortcut, keys), cx);
                self.finish(window, cx);
            }
            // Still listening, for another try.
            Err(refusal) => {
                let pressed = Keystroke {
                    modifiers: keystroke.modifiers,
                    key: keystroke.key.clone(),
                    key_char: None,
                };
                let message = refusal.message(&key_text(&pressed.unparse(), false));
                window.push_notification(Notification::warning(message), cx);
            }
        }
    }

    fn change(&self, change: impl FnOnce(&mut ShortcutOverrides), cx: &mut App) {
        self.store.update(cx, |store, cx| {
            store.update(|settings| change(&mut settings.shortcuts), cx)
        });
    }
}

/// The page: a group of shortcuts for each part of the app.
pub(super) fn shortcut_groups(editor: &Entity<ShortcutsEditor>) -> Vec<SettingGroup> {
    ShortcutGroup::ALL
        .into_iter()
        .map(|group| {
            SettingGroup::new()
                .title(group.title())
                .items(Shortcut::in_group(group).map(|shortcut| shortcut_item(editor, shortcut)))
        })
        .collect()
}

/// One shortcut: its name, then its keys, 禁用 and 恢复默认. The page's
/// 重置 puts it back too.
fn shortcut_item(editor: &Entity<ShortcutsEditor>, shortcut: &'static Shortcut) -> SettingItem {
    let (field, dirty, reset) = (editor.clone(), editor.clone(), editor.clone());
    SettingItem::new(
        shortcut.label(),
        SettingField::render(move |options, window, cx| {
            render_field(&field, shortcut, options, window, cx)
        })
        .on_reset(
            move |cx| {
                !dirty
                    .read(cx)
                    .store
                    .read(cx)
                    .settings()
                    .shortcuts
                    .is_default(shortcut)
            },
            move |_, cx| {
                reset.update(cx, |editor, cx| {
                    editor.change(|overrides| overrides.reset(shortcut), cx)
                });
            },
        ),
    )
}

fn element_id(kind: &str, shortcut: &Shortcut) -> ElementId {
    SharedString::from(format!("shortcut-{kind}-{}", shortcut.id())).into()
}

fn render_field(
    editor: &Entity<ShortcutsEditor>,
    shortcut: &'static Shortcut,
    options: &RenderOptions,
    _: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let state = editor.read(cx);
    let overrides = state.store.read(cx).settings().shortcuts;
    let recording = state
        .recording
        .is_some_and(|recording| std::ptr::eq(recording, shortcut));
    let focus_handle = state.focus_handle.clone();
    let size = options.size();
    let keys = overrides.keys(shortcut);
    let disabled = overrides.is_disabled(shortcut);
    let is_default = overrides.is_default(shortcut);
    let theme = cx.theme();

    let keys_area = if recording {
        div()
            .id(element_id("recorder", shortcut))
            .test_support()
            .track_focus(&focus_handle)
            .map(|this| match size {
                Size::XSmall | Size::Small => this.h_6(),
                _ => this.h_8(),
            })
            .min_w(rems(8.))
            .px_2()
            .flex()
            .items_center()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.ring)
            .bg(theme.background)
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(t!("settings.shortcuts.recording"))
            .into_any_element()
    } else {
        let start = editor.clone();
        // What the caps show, for screen readers.
        let spoken = keys
            .as_deref()
            .map(|keys| key_text(keys, shortcut.is_numbered()))
            .unwrap_or_else(|| t!("settings.shortcuts.none").to_string());
        Button::new(element_id("keys", shortcut))
            .ghost()
            .with_size(size)
            .accessibility_label(spoken)
            .tooltip(t!("settings.shortcuts.change"))
            .child(key_caps_element(
                keys.as_deref(),
                shortcut.is_numbered(),
                cx,
            ))
            .on_click(move |_, window, cx| {
                start.update(cx, |editor, cx| editor.start(shortcut, window, cx))
            })
            .into_any_element()
    };

    let (toggle, reset) = (editor.clone(), editor.clone());
    h_flex()
        .gap_1()
        .child(keys_area)
        .child(
            Button::new(element_id("disable", shortcut))
                .ghost()
                .with_size(size)
                .icon(Icon::new(CatalogIcon::Ban))
                .selected(disabled)
                .tooltip(if disabled {
                    t!("settings.shortcuts.enable")
                } else {
                    t!("settings.shortcuts.disable")
                })
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |editor, cx| {
                        editor.change(
                            |overrides| {
                                if disabled {
                                    overrides.reset(shortcut)
                                } else {
                                    overrides.disable(shortcut)
                                }
                            },
                            cx,
                        )
                    })
                }),
        )
        .child(
            Button::new(element_id("reset", shortcut))
                .ghost()
                .with_size(size)
                .icon(Icon::new(CatalogIcon::RotateCcw))
                .tooltip(t!("settings.shortcuts.reset"))
                .disabled(is_default)
                .on_click(move |_, _, cx| {
                    reset.update(cx, |editor, cx| {
                        editor.change(|overrides| overrides.reset(shortcut), cx)
                    })
                }),
        )
        .into_any_element()
}

/// The keys as caps joined by +, as in ⌘ + N; 无 when turned off.
fn key_caps_element(keys: Option<&str>, numbered: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let Some(keys) = keys else {
        return div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(t!("settings.shortcuts.none"))
            .into_any_element();
    };
    let caps = key_caps(keys, numbered);
    let last = caps.len().saturating_sub(1);
    h_flex()
        .gap_1()
        .children(caps.into_iter().enumerate().flat_map(|(ix, cap)| {
            let cap = div()
                .h_5()
                .min_w_5()
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .text_xs()
                .text_color(theme.foreground)
                .child(cap)
                .into_any_element();
            let plus = (ix < last).then(|| {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("+")
                    .into_any_element()
            });
            std::iter::once(cap).chain(plus)
        }))
        .into_any_element()
}
