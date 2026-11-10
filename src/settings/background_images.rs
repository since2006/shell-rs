//! 外观 › 浅色背景图, 深色背景图: the picture behind the terminals' text for
//! each appearance, whether they show it, and how much of it shows through
//! the veil of the theme's background color. A chosen picture is copied
//! into the data directory, so the original can go.

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, io};

use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, ThemeMode, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    notification::Notification,
    setting::{SettingField, SettingGroup, SettingItem},
    slider::{Slider, SliderEvent, SliderState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::i18n::t;

use super::settings_panel::keywords;
use super::terminal_themes::{column_id, sample_lines};
use super::{DEFAULT_BACKGROUND_OPACITY, SettingsStore};

/// How long 透明度 rests before a change made without dragging, such as an
/// assistive step, is written; a drag is written when it lets go.
const OPACITY_SETTLE: Duration = Duration::from_millis(300);

/// The two appearances' 透明度 sliders, which keep their thumbs while the
/// settings change under them. The settings hold the rest.
pub struct BackgroundImagesEditor {
    store: Entity<SettingsStore>,
    light: OpacitySlider,
    dark: OpacitySlider,
    _store_changes: Subscription,
}

struct OpacitySlider {
    /// In percent.
    state: Entity<SliderState>,
    /// The write once the slider rests; while there is one, the settings
    /// are behind the slider.
    pending: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl BackgroundImagesEditor {
    pub fn new(store: Entity<SettingsStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let light = Self::slider(&store, ThemeMode::Light, cx);
        let dark = Self::slider(&store, ThemeMode::Dark, cx);
        // 重置, or the settings read anew, move the thumbs; the sliders' own
        // changes come back as what they show.
        let store_changes = cx.observe_in(&store, window, |this, store, window, cx| {
            let settings = store.read(cx).settings();
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                let slider = this.opacity_slider(mode);
                if slider.pending.is_some() {
                    continue;
                }
                let stored = percent(settings.terminal_background.for_mode(mode).opacity);
                let state = slider.state.clone();
                if state.read(cx).value().end() != stored {
                    state.update(cx, |state, cx| state.set_value(stored, window, cx));
                }
            }
        });
        Self {
            store,
            light,
            dark,
            _store_changes: store_changes,
        }
    }

    fn slider(
        store: &Entity<SettingsStore>,
        mode: ThemeMode,
        cx: &mut Context<Self>,
    ) -> OpacitySlider {
        let stored = store
            .read(cx)
            .settings()
            .terminal_background
            .for_mode(mode)
            .opacity;
        let state = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(percent(stored))
        });
        let subscriptions = [
            // Every move, for the preview; written once it rests.
            cx.observe(&state, move |this, _, cx| {
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(OPACITY_SETTLE).await;
                    let _ = this.update(cx, |this, cx| this.write_opacity(mode, cx));
                });
                this.opacity_slider_mut(mode).pending = Some(task);
                cx.notify();
            }),
            cx.subscribe(&state, move |this, _, event: &SliderEvent, cx| {
                if let SliderEvent::Release(_) = event {
                    this.write_opacity(mode, cx);
                }
            }),
        ];
        OpacitySlider {
            state,
            pending: None,
            _subscriptions: subscriptions,
        }
    }

    fn opacity_slider(&self, mode: ThemeMode) -> &OpacitySlider {
        match mode {
            ThemeMode::Light => &self.light,
            ThemeMode::Dark => &self.dark,
        }
    }

    fn opacity_slider_mut(&mut self, mode: ThemeMode) -> &mut OpacitySlider {
        match mode {
            ThemeMode::Light => &mut self.light,
            ThemeMode::Dark => &mut self.dark,
        }
    }

    /// 透明度 as the slider shows it, ahead of the settings while it moves.
    fn shown_opacity(&self, mode: ThemeMode, cx: &App) -> f32 {
        self.opacity_slider(mode).state.read(cx).value().end() / 100.
    }

    fn write_opacity(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        self.opacity_slider_mut(mode).pending = None;
        let opacity = self.shown_opacity(mode, cx);
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| settings.terminal_background.for_mode_mut(mode).opacity = opacity,
                cx,
            )
        });
    }

    fn reset_opacity(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.opacity_slider(mode).state.clone();
        state.update(cx, |state, cx| {
            state.set_value(percent(DEFAULT_BACKGROUND_OPACITY), window, cx)
        });
        self.write_opacity(mode, cx);
    }

    /// 选择图片…: the system's picker, then a copy of the picture for the
    /// appearance, shown from now on.
    pub(super) fn choose(&mut self, mode: ThemeMode, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(t!("settings.background.prompt")),
        });
        let dir = self.store.read(cx).backgrounds_dir();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(source) = paths.into_iter().next() else {
                return;
            };
            let copied = cx
                .background_spawn(async move { import(&source, dir.as_deref()) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match copied {
                Ok(image) => this.show(mode, Some(image), cx),
                Err(error) => window.push_notification(
                    Notification::error(t!(
                        "settings.background.import_failed",
                        error = error.to_string()
                    )),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// 清除: no picture for the appearance, and so none shown.
    pub(super) fn clear(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        self.show(mode, None, cx);
    }

    /// Show `image` for the appearance, or nothing; the copy it replaces
    /// goes.
    fn show(&mut self, mode: ThemeMode, image: Option<PathBuf>, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let (before, dir) = (
            store.settings().terminal_background,
            store.backgrounds_dir(),
        );
        let enabled = image.is_some();
        self.store.update(cx, |store, cx| {
            store.update(
                |settings| {
                    let background = settings.terminal_background.for_mode_mut(mode);
                    background.image = image.clone();
                    background.enabled = enabled;
                },
                cx,
            )
        });
        let replaced = before.for_mode(mode).image.clone();
        let other = match mode {
            ThemeMode::Light => &before.dark,
            ThemeMode::Dark => &before.light,
        };
        if let Some(replaced) = replaced.filter(|replaced| {
            dir.as_deref().is_some_and(|dir| replaced.starts_with(dir))
                && Some(replaced) != image.as_ref()
                && Some(replaced) != other.image.as_ref()
        }) {
            cx.background_spawn(async move {
                let _ = fs::remove_file(replaced);
            })
            .detach();
        }
    }
}

/// `opacity`, 0 to 1, in whole percent.
fn percent(opacity: f32) -> f32 {
    (opacity * 100.).round()
}

/// ShellRS's copy of the picture at `source` in `dir`, under a name of its
/// own before the picture's: the settings name the copy, and the original
/// can go. Settings kept in memory have no `dir` and use the picture where
/// it is.
fn import(source: &Path, dir: Option<&Path>) -> io::Result<PathBuf> {
    let Some(dir) = dir else {
        return Ok(source.to_path_buf());
    };
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    fs::create_dir_all(dir)?;
    let mut copy = uuid::Uuid::new_v4().simple().to_string();
    copy.push('-');
    copy.push_str(&name.to_string_lossy());
    let target = dir.join(copy);
    fs::copy(source, &target)?;
    Ok(target)
}

/// The name of the picture the user chose: the copy's, without the part
/// ShellRS put before it.
fn chosen_name(image: &Path) -> String {
    let name = image
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.split_once('-') {
        Some((id, original))
            if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            original.to_string()
        }
        _ => name,
    }
}

/// One appearance's group: 启用, 图片, 透明度 and the preview.
pub(super) fn background_group(
    editor: &Entity<BackgroundImagesEditor>,
    store: &Entity<SettingsStore>,
    mode: ThemeMode,
) -> SettingGroup {
    let (title, description) = match mode {
        ThemeMode::Light => (
            t!("settings.background.light"),
            t!("settings.background.light_description"),
        ),
        ThemeMode::Dark => (
            t!("settings.background.dark"),
            t!("settings.background.dark_description"),
        ),
    };
    let words = keywords(t!("settings.background.keywords"));
    let (reader, writer) = (store.clone(), store.clone());
    let (image_editor, image_store, image_reset) = (editor.clone(), store.clone(), editor.clone());
    let (opacity_editor, opacity_store, opacity_reset) =
        (editor.clone(), store.clone(), editor.clone());
    let (preview_editor, preview_store) = (editor.clone(), store.clone());
    SettingGroup::new()
        .title(title)
        .description(description)
        .items([
            SettingItem::new(
                t!("settings.background.enabled"),
                SettingField::switch(
                    move |cx| {
                        reader
                            .read(cx)
                            .settings()
                            .terminal_background
                            .for_mode(mode)
                            .enabled
                    },
                    move |on, cx| {
                        writer.update(cx, |store, cx| {
                            store.update(
                                |settings| {
                                    settings.terminal_background.for_mode_mut(mode).enabled = on
                                },
                                cx,
                            )
                        });
                    },
                )
                .default_value(false),
            )
            .keywords(words.clone()),
            SettingItem::new(
                t!("settings.background.image"),
                SettingField::render(move |_, window, cx| {
                    image_field(&image_editor, mode, window, cx)
                })
                .on_reset(
                    move |cx| {
                        image_store
                            .read(cx)
                            .settings()
                            .terminal_background
                            .for_mode(mode)
                            .image
                            .is_some()
                    },
                    move |_, cx| image_reset.update(cx, |editor, cx| editor.clear(mode, cx)),
                ),
            )
            .keywords(words.clone()),
            SettingItem::new(
                t!("settings.background.opacity"),
                SettingField::render(move |_, _, cx| opacity_field(&opacity_editor, mode, cx))
                    .on_reset(
                        move |cx| {
                            opacity_store
                                .read(cx)
                                .settings()
                                .terminal_background
                                .for_mode(mode)
                                .opacity
                                != DEFAULT_BACKGROUND_OPACITY
                        },
                        move |window, cx| {
                            opacity_reset
                                .update(cx, |editor, cx| editor.reset_opacity(mode, window, cx))
                        },
                    ),
            )
            .description(t!("settings.background.opacity_description"))
            .keywords(words.clone()),
            SettingItem::render(move |_, window, cx| {
                preview(&preview_editor, &preview_store, mode, window, cx)
            })
            .keywords(words),
        ])
}

/// The picture's name, or 未选择, then 选择图片… and, with a picture, 清除.
fn image_field(
    editor: &Entity<BackgroundImagesEditor>,
    mode: ThemeMode,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let this = editor.read(cx);
    let image = this
        .store
        .read(cx)
        .settings()
        .terminal_background
        .for_mode(mode)
        .image
        .clone();
    let name: SharedString = match &image {
        Some(image) => chosen_name(image).into(),
        None => t!("settings.background.none"),
    };
    h_flex()
        .gap_2()
        .child(
            div()
                .id(column_id("background-image", mode))
                .test_support()
                .aria_label(name.clone())
                .max_w(rems(14.))
                .truncate()
                .text_sm()
                .when(image.is_none(), |label| {
                    label.text_color(cx.theme().muted_foreground)
                })
                .child(name),
        )
        .child(
            Button::new(column_id("background-choose", mode))
                .outline()
                .small()
                .label(t!("settings.background.choose"))
                .on_click(window.listener_for(editor, move |editor, _, window, cx| {
                    editor.choose(mode, window, cx)
                })),
        )
        .when(image.is_some(), |field| {
            field.child(
                Button::new(column_id("background-clear", mode))
                    .ghost()
                    .small()
                    .icon(IconName::Close)
                    .tooltip(t!("settings.background.clear"))
                    .on_click(
                        window.listener_for(editor, move |editor, _, _, cx| editor.clear(mode, cx)),
                    ),
            )
        })
}

/// The slider and its percentage.
fn opacity_field(editor: &Entity<BackgroundImagesEditor>, mode: ThemeMode, cx: &mut App) -> Div {
    let this = editor.read(cx);
    let state = this.opacity_slider(mode).state.clone();
    let shown = percent(this.shown_opacity(mode, cx));
    h_flex()
        .w(rems(14.))
        .gap_3()
        .child(
            div()
                .id(column_id("background-opacity", mode))
                .test_support()
                .flex_1()
                .child(Slider::new(&state).horizontal()),
        )
        .child(
            div()
                .flex_none()
                .w(rems(2.5))
                .text_right()
                .text_sm()
                .child(format!("{shown}%")),
        )
}

/// A few lines of a shell in the appearance's theme over the picture and
/// its veil, as the terminals will show them; the veil follows the slider
/// as it moves.
fn preview(
    editor: &Entity<BackgroundImagesEditor>,
    store: &Entity<SettingsStore>,
    mode: ThemeMode,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let settings = store.read(cx).settings();
    let theme = settings.terminal_theme.theme(mode);
    let image = settings.terminal_background.for_mode(mode).image.clone();
    let opacity = editor.read(cx).shown_opacity(mode, cx);
    // The same load the terminals' picture waits on; nothing to tell until
    // it is done.
    let unreadable = image.as_ref().is_some_and(|image| {
        matches!(
            window.use_asset::<ImgResourceLoader>(&Resource::from(image.clone()), cx),
            Some(Err(_))
        )
    });
    v_flex()
        .w_full()
        .gap_2()
        .child(div().text_sm().child(t!("settings.preview")))
        .child(
            v_flex()
                .id(column_id("background-preview", mode))
                .test_support()
                .relative()
                .w_full()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(theme.background())
                .overflow_hidden()
                .when_some(image.filter(|_| !unreadable), |preview, image| {
                    preview
                        .child(
                            img(image)
                                .absolute()
                                .inset_0()
                                .size_full()
                                .object_fit(ObjectFit::Cover),
                        )
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .bg(theme.background().opacity(1. - opacity)),
                        )
                })
                .children(sample_lines(theme, cx))
                .when(unreadable, |preview| {
                    preview.child(
                        div()
                            .text_xs()
                            .text_color(theme.foreground().opacity(0.66))
                            .child(t!("settings.background.unreadable")),
                    )
                }),
        )
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it brings in `gpui_kit::*`, whose `test` shadows `#[test]`.
    use std::fs;
    use std::path::Path;

    use super::{chosen_name, import, percent};

    #[test]
    fn a_chosen_picture_is_copied_under_a_name_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("夕阳-sunset.JPG");
        fs::write(&source, b"picture").unwrap();
        let backgrounds = dir.path().join("data").join("backgrounds");

        let first = import(&source, Some(&backgrounds)).unwrap();
        let second = import(&source, Some(&backgrounds)).unwrap();
        assert_ne!(first, second, "each choice is its own copy");
        for copy in [&first, &second] {
            assert_eq!(copy.parent(), Some(backgrounds.as_path()));
            assert_eq!(fs::read(copy).unwrap(), b"picture");
            assert_eq!(chosen_name(copy), "夕阳-sunset.JPG");
        }
        // The original stays.
        assert!(source.exists());

        // Settings kept in memory use the picture where it is.
        assert_eq!(import(&source, None).unwrap(), source);
        assert_eq!(chosen_name(&source), "夕阳-sunset.JPG");
        assert_eq!(
            chosen_name(Path::new("/pictures/abc-def.png")),
            "abc-def.png"
        );

        assert!(import(&dir.path().join("gone.png"), Some(&backgrounds)).is_err());
    }

    #[test]
    fn opacity_is_shown_in_whole_percent() {
        assert_eq!(percent(0.3), 30.);
        assert_eq!(percent(0.333), 33.);
        assert_eq!(percent(1.), 100.);
    }
}
