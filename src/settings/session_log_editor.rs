//! 设置 › 终端 › 会话日志: the folder logs go under, the file name template,
//! and the format of each line's time. Every change goes to the settings at
//! once; a connection takes them when it starts.

use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::Button,
    h_flex,
    input::{Input, InputEvent, InputState},
    setting::RenderOptions,
};
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::i18n::t;
use crate::terminal::{
    DEFAULT_LOG_TEMPLATE, DEFAULT_LOG_TIMESTAMP_FORMAT, SessionLogSettings, default_log_directory,
};

use super::SettingsStore;

/// The three text fields of 会话日志, which keep their carets while the
/// settings change under them.
pub struct SessionLogEditor {
    store: Entity<SettingsStore>,
    directory: Entity<InputState>,
    template: Entity<InputState>,
    timestamp_format: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl SessionLogEditor {
    pub fn new(store: Entity<SettingsStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = stored(&store, cx);
        let directory = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(default_log_directory().to_string_lossy().into_owned())
                .default_value(settings.directory.clone().unwrap_or_default())
        });
        let template = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(DEFAULT_LOG_TEMPLATE)
                .default_value(settings.template.clone())
        });
        let timestamp_format = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(DEFAULT_LOG_TIMESTAMP_FORMAT)
                .default_value(settings.timestamp_format.clone())
        });
        let subscriptions = vec![
            cx.subscribe(&directory, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.write_directory(input.read(cx).value().to_string(), cx);
                }
            }),
            cx.subscribe(&template, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.write_template(input.read(cx).value().to_string(), cx);
                }
            }),
            cx.subscribe(&timestamp_format, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.write_timestamp_format(input.read(cx).value().to_string(), cx);
                }
            }),
            // The fields' own edits come back the same, but for the blanks
            // around them; anything else (重置, the settings file read anew)
            // shows in them.
            cx.observe_in(&store, window, |this, store, window, cx| {
                let settings = stored(&store, cx);
                this.show(
                    settings.directory.clone().unwrap_or_default(),
                    settings.template.clone(),
                    settings.timestamp_format.clone(),
                    window,
                    cx,
                );
            }),
        ];
        Self {
            store,
            directory,
            template,
            timestamp_format,
            _subscriptions: subscriptions,
        }
    }

    fn show(
        &mut self,
        directory: String,
        template: String,
        timestamp_format: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (input, value) in [
            (&self.directory, directory),
            (&self.template, template),
            (&self.timestamp_format, timestamp_format),
        ] {
            if input.read(cx).value().as_str() != value {
                input.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
            }
        }
    }

    /// Choose the folder with the system's picker.
    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(t!("settings.session_log.browse_prompt")),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.set_directory(Some(path.to_string_lossy().into_owned()), window, cx)
            })
            .ok();
        })
        .detach();
    }

    /// The folder from the picker or 重置; `None` is the default.
    pub(super) fn set_directory(
        &mut self,
        directory: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = directory.clone().unwrap_or_default();
        self.directory
            .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
        self.write_directory(value, cx);
    }

    /// The default template, from 重置.
    pub(super) fn set_template(
        &mut self,
        template: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.template.update(cx, |input, cx| {
            input.set_value(template.to_string(), window, cx)
        });
        self.write_template(template.to_string(), cx);
    }

    /// The default timestamp format, from 重置.
    pub(super) fn set_timestamp_format(
        &mut self,
        format: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.timestamp_format.update(cx, |input, cx| {
            input.set_value(format.to_string(), window, cx)
        });
        self.write_timestamp_format(format.to_string(), cx);
    }

    fn write_directory(&self, directory: String, cx: &mut Context<Self>) {
        let directory = directory.trim().to_string();
        let directory = (!directory.is_empty()).then_some(directory);
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| settings.session_log.directory = directory.clone(),
                cx,
            )
        });
    }

    fn write_template(&self, template: String, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| settings.session_log.template = template.clone(),
                cx,
            )
        });
    }

    fn write_timestamp_format(&self, format: String, cx: &mut Context<Self>) {
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| settings.session_log.timestamp_format = format.clone(),
                cx,
            )
        });
    }
}

fn stored(store: &Entity<SettingsStore>, cx: &App) -> SessionLogSettings {
    store.read(cx).settings().session_log
}

/// 保存目录: the field across the row, with the system's folder picker
/// beside it.
pub(super) fn directory_field(
    editor: &Entity<SessionLogEditor>,
    options: &RenderOptions,
    cx: &App,
) -> Div {
    let this = editor.read(cx);
    let browse_editor = editor.downgrade();
    h_flex()
        .w_full()
        .gap_2()
        .child(
            Input::new(&this.directory)
                .id("session-log-directory")
                .aria_label(t!("settings.session_log.directory"))
                .with_size(options.size())
                .font_family(cx.theme().mono_font_family.clone())
                .flex_1()
                .min_w_0(),
        )
        .child(
            Button::new("browse-session-log")
                .outline()
                .with_size(options.size())
                .icon(Icon::new(CatalogIcon::FolderTree))
                .label(t!("settings.session_log.browse"))
                .on_click(move |_, window, cx| {
                    browse_editor
                        .update(cx, |editor, cx| editor.browse(window, cx))
                        .ok();
                }),
        )
}

/// 文件名模板.
pub(super) fn template_field(
    editor: &Entity<SessionLogEditor>,
    options: &RenderOptions,
    cx: &App,
) -> Div {
    div().w_full().child(
        Input::new(&editor.read(cx).template)
            .id("session-log-template")
            .aria_label(t!("settings.session_log.template"))
            .with_size(options.size())
            .font_family(cx.theme().mono_font_family.clone()),
    )
}

/// 每行时间戳的格式.
pub(super) fn timestamp_format_field(
    editor: &Entity<SessionLogEditor>,
    options: &RenderOptions,
    cx: &App,
) -> Div {
    div().w_full().child(
        Input::new(&editor.read(cx).timestamp_format)
            .id("session-log-timestamp-format")
            .aria_label(t!("settings.session_log.timestamp_format"))
            .with_size(options.size())
            .font_family(cx.theme().mono_font_family.clone()),
    )
}
