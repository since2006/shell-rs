//! 外观 → 终端主题: the light themes and the dark ones side by side, a card
//! each showing a few lines of a shell in its colors, the one in use marked.
//! Choosing a card writes the settings at once; which column is in effect
//! follows the app's appearance.

use gpui_kit::base::{Radio, RadioGroup};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, ThemeMode, h_flex,
    scroll::ScrollableElement as _, setting::SettingItem, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::terminal::{TerminalFont, TerminalTheme};

use super::{SettingsStore, TerminalThemeSettings};

/// How tall each column's list is, in rems: about three cards, the third
/// cut off so the list shows that it scrolls.
const LIST_HEIGHT: f32 = 24.;

/// What a card shows: a prompt and `ls -l`, as Debian colors them, in the
/// theme's green, blue and cyan. Each part names the ANSI color it is
/// drawn in, or the theme's text color.
const SAMPLE: &[&[(&str, Option<u8>)]] = &[
    &[
        ("root@shellrs", Some(2)),
        (":", None),
        ("~", Some(4)),
        ("$ ls -l", None),
    ],
    &[("drwxr-xr-x 2 root  ", None), ("boot", Some(4))],
    &[
        ("lrwxrwxrwx 1 root  ", None),
        ("bin", Some(6)),
        (" -> usr/bin", None),
    ],
];

/// The group's one item: both columns, and the page's 重置 putting both
/// back to the defaults.
pub(super) fn terminal_theme_item(store: &Entity<SettingsStore>) -> SettingItem {
    let (reader, dirty, reset) = (store.clone(), store.clone(), store.clone());
    SettingItem::render(move |_, _, cx| {
        let chosen = reader.read(cx).settings().terminal_theme;
        h_flex()
            .w_full()
            .items_start()
            .gap_4()
            .child(column(ThemeMode::Light, &chosen, &reader, cx))
            .child(column(ThemeMode::Dark, &chosen, &reader, cx))
    })
    .keywords(
        ["终端主题", "主题", "配色", "颜色", "浅色", "深色"]
            .into_iter()
            .chain(TerminalTheme::all().iter().map(TerminalTheme::name)),
    )
    .on_reset(
        move |cx| dirty.read(cx).settings().terminal_theme != TerminalThemeSettings::default(),
        move |_, cx| {
            reset.update(cx, |store, cx| {
                store.update(|settings| settings.terminal_theme = Default::default(), cx)
            });
        },
    )
}

/// One appearance's themes under its name, scrolling on their own.
fn column(
    mode: ThemeMode,
    chosen: &TerminalThemeSettings,
    store: &Entity<SettingsStore>,
    cx: &App,
) -> impl IntoElement {
    let title = match mode {
        ThemeMode::Light => "浅色",
        ThemeMode::Dark => "深色",
    };
    let chosen = chosen.theme(mode);
    let themes: Vec<_> = TerminalTheme::for_mode(mode).collect();
    let total = themes.len();
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(div().text_sm().font_medium().child(title))
        .child(
            div()
                .id(column_id("terminal-themes", mode))
                .w_full()
                .h(rems(LIST_HEIGHT))
                .child(
                    RadioGroup::new(column_id("terminal-theme-group", mode))
                        .axis(Axis::Vertical)
                        .flex()
                        .flex_col()
                        .gap_1()
                        // The scrollbar's lane, so it never covers a card's
                        // check mark.
                        .pr_3()
                        .children(themes.into_iter().enumerate().map(|(ix, theme)| {
                            card(theme, theme == chosen, (ix + 1, total), store, cx)
                        })),
                )
                .overflow_y_scrollbar()
                .id(column_id("terminal-themes-scroll", mode)),
        )
}

/// `prefix-light` or `prefix-dark`.
fn column_id(prefix: &str, mode: ThemeMode) -> ElementId {
    SharedString::from(format!("{prefix}-{}", mode.name())).into()
}

/// A theme's name over its sample. The chosen one stands out like a chosen
/// segment and carries the check mark.
fn card(
    theme: &'static TerminalTheme,
    checked: bool,
    (position, total): (usize, usize),
    store: &Entity<SettingsStore>,
    cx: &App,
) -> impl IntoElement {
    let colors = cx.theme();
    let store = store.clone();
    Radio::new(SharedString::from(format!(
        "terminal-theme-{}",
        theme.key()
    )))
    .checked(checked)
    .accessibility_label(theme.name())
    .set_position(position, total)
    .w_full()
    .flex()
    .flex_col()
    .gap_2()
    .p_3()
    .rounded(colors.radius_lg)
    // Every card has the border, so choosing one moves nothing.
    .border_1()
    .border_color(colors.transparent)
    .styles(|styles| styles.checked(|style| style.bg(colors.accent)))
    // Half of what choosing it would show.
    .when(!checked, |radio| {
        radio.hover(|style| style.bg(colors.accent.opacity(0.5)))
    })
    .focus_visible(|style| style.border_color(colors.ring))
    // A click chooses without taking the keyboard from where it was,
    // as gpui-component's radios do.
    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
    .on_change(move |_, _, _, cx| {
        store.update(cx, |store, cx| {
            store.update(|settings| settings.terminal_theme.choose(theme), cx)
        });
    })
    .child(
        h_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(theme.name()),
            )
            // A fixed slot, so the name does not move with the mark: a disc
            // in the theme's blue, the hue of selection, the check in the
            // page's color so it shows on the disc in light and dark.
            .child(
                div()
                    .flex_shrink_0()
                    .size_5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(colors.radius_full())
                    .when(checked, |slot| {
                        slot.bg(colors.blue).child(
                            Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(colors.background),
                        )
                    }),
            ),
    )
    .child(sample(theme, cx))
}

/// The sample lines in the terminal font and the theme's colors, framed by
/// a hairline so a background like the page's still shows its edge.
fn sample(theme: &'static TerminalTheme, cx: &App) -> impl IntoElement {
    let font = TerminalFont::current(cx);
    v_flex()
        .w_full()
        .px_3()
        .py_2()
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(theme.background())
        .text_color(theme.foreground())
        .font_family(font.family(cx))
        .text_sm()
        .overflow_hidden()
        .children(SAMPLE.iter().map(|parts| {
            let mut text = String::new();
            let mut highlights = Vec::new();
            for (part, color) in parts.iter() {
                if let Some(color) = color {
                    highlights.push((
                        text.len()..text.len() + part.len(),
                        HighlightStyle {
                            color: Some(theme.indexed(*color)),
                            ..HighlightStyle::default()
                        },
                    ));
                }
                text.push_str(part);
            }
            div()
                .whitespace_nowrap()
                .child(StyledText::new(text).with_highlights(highlights))
        }))
}
