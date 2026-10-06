//! The app's colors from the chosen theme, so the window around a terminal
//! matches it: gpui-kit's default light or dark theme, recolored. Each gray
//! keeps its place between the default's background and text, now between
//! the theme's; each color becomes the theme's ANSI color of its hue. What
//! gpui-kit derives from these (hover, active, tab bar, status bar …)
//! follows. The theme's own default is gpui-kit's theme unchanged.

use std::rc::Rc;

use gpui_kit::component::{ThemeConfig, ThemeMode, ThemeRegistry, try_parse_color};
use gpui_kit::*;

use crate::terminal::TerminalTheme;

/// The name the app's theme has while `theme` is chosen, to tell whether it
/// is already in effect without building it again.
pub(super) fn ui_theme_name(theme: &'static TerminalTheme, cx: &App) -> SharedString {
    if is_default(theme) {
        default_config(theme.mode(), cx).name.clone()
    } else {
        theme.name().into()
    }
}

/// The app's theme while `theme` is chosen.
pub(super) fn ui_theme(theme: &'static TerminalTheme, cx: &App) -> Rc<ThemeConfig> {
    let default = default_config(theme.mode(), cx);
    if is_default(theme) {
        return default.clone();
    }
    Rc::new(recolor(default, theme))
}

fn is_default(theme: &'static TerminalTheme) -> bool {
    theme == TerminalTheme::default_for(theme.mode())
}

fn default_config(mode: ThemeMode, cx: &App) -> &Rc<ThemeConfig> {
    let registry = ThemeRegistry::global(cx);
    match mode {
        ThemeMode::Light => registry.default_light_theme(),
        ThemeMode::Dark => registry.default_dark_theme(),
    }
}

/// `default` with its colors moved onto `theme`. Everything else (radius,
/// shadow, syntax highlighting) stays the default's.
fn recolor(default: &ThemeConfig, theme: &TerminalTheme) -> ThemeConfig {
    let mut config = serde_json::to_value(default).expect("a theme is plain data");
    config["name"] = theme.name().into();
    config["is_default"] = false.into();
    if let Some(colors) = config["colors"].as_object_mut() {
        let color = |key: &str| {
            colors
                .get(key)
                .and_then(|value| value.as_str())
                .and_then(|value| try_parse_color(value).ok())
                .map(|color| color.to_rgb())
        };
        if let (Some(background), Some(foreground)) = (color("background"), color("foreground")) {
            let line = Line {
                from: (background, foreground),
                to: (theme.background().to_rgb(), theme.foreground().to_rgb()),
            };
            colors.retain(|key, value| {
                let Some(color) = value.as_str().and_then(|text| try_parse_color(text).ok()) else {
                    return true;
                };
                match recolor_one(key, color.to_rgb(), &line, theme) {
                    Some(color) => {
                        *value = hex(color).into();
                        true
                    }
                    // Left to what gpui-kit derives in its place.
                    None => false,
                }
            });
        }
    }
    serde_json::from_value(config).expect("written from a theme")
}

/// The line from background to text, in the default and in the theme.
struct Line {
    from: (Rgba, Rgba),
    to: (Rgba, Rgba),
}

/// How much of its text's contrast a theme keeps for the default's readable
/// grays, such as secondary text, at most what reads well anywhere.
const TEXT_CONTRAST_SHARE: f32 = 0.8;
const READABLE_CONTRAST: f32 = 4.5;

/// One of the default's colors in the theme, or `None` to leave it to
/// gpui-kit: the lighter versions of the base colors, and colored text on a
/// colored background, which in the theme could be the same color.
fn recolor_one(key: &str, color: Rgba, line: &Line, theme: &TerminalTheme) -> Option<Rgba> {
    let ansi = |index: u8| {
        let mut ansi = theme.indexed(index).to_rgb();
        ansi.a = color.a;
        Some(ansi)
    };
    if let Some(base) = key.strip_prefix("base.") {
        return match base {
            "red" => ansi(1),
            "green" => ansi(2),
            "yellow" => ansi(3),
            "blue" => ansi(4),
            "magenta" => ansi(5),
            "cyan" => ansi(6),
            _ => None,
        };
    }
    if is_gray(color) {
        let (background, foreground) = line.to;
        let (from_background, from_foreground) = line.from;
        let mut t = (gray_value(color) - gray_value(from_background))
            / (gray_value(from_foreground) - gray_value(from_background));
        // A gray the default gives text to read on its background stays
        // readable even where the theme's own text is soft.
        if contrast(color, from_background) >= READABLE_CONTRAST {
            let floor =
                (contrast(foreground, background) * TEXT_CONTRAST_SHARE).min(READABLE_CONTRAST);
            t = least_reaching(background, foreground, t, floor);
        }
        let mut mixed = mix(background, foreground, t);
        mixed.a = color.a;
        return Some(mixed);
    }
    if key.ends_with("foreground") {
        return None;
    }
    ansi(ansi_of_hue(color))
}

/// The ANSI color (red 1 … cyan 6) nearest in hue: 60° each, centered on
/// red at 0°, yellow, green, cyan, blue, magenta.
fn ansi_of_hue(color: Rgba) -> u8 {
    let hue = Hsla::from(color).h * 360.;
    match ((hue + 30.) / 60.).floor() as i32 % 6 {
        0 => 1,
        1 => 3,
        2 => 2,
        3 => 6,
        4 => 4,
        _ => 5,
    }
}

fn is_gray(color: Rgba) -> bool {
    let high = color.r.max(color.g).max(color.b);
    let low = color.r.min(color.g).min(color.b);
    high - low < 0.02
}

fn gray_value(color: Rgba) -> f32 {
    (color.r + color.g + color.b) / 3.
}

/// `t` of the way from `from` to `to`, past either end if `t` is.
fn mix(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let channel = |a: f32, b: f32| (a + (b - a) * t).clamp(0., 1.);
    Rgba {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: 1.,
    }
}

/// The smallest step at or past `t` toward the text whose mix reaches
/// `target` contrast with the background; the text itself if none does.
fn least_reaching(background: Rgba, foreground: Rgba, t: f32, target: f32) -> f32 {
    let reaches = |t: f32| contrast(mix(background, foreground, t), background) >= target;
    if reaches(t) {
        return t;
    }
    let (mut low, mut high) = (t, 1.);
    for _ in 0..24 {
        let middle = (low + high) / 2.;
        if reaches(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    high
}

/// WCAG relative luminance.
fn luminance(color: Rgba) -> f32 {
    let linear = |value: f32| {
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `#rrggbbaa`, as gpui-kit reads it.
fn hex(color: Rgba) -> String {
    let byte = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b),
        byte(color.a)
    )
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
    use gpui_kit::{App, Rgba, TestAppContext, rgb};

    use super::{READABLE_CONTRAST, ansi_of_hue, contrast, ui_theme, ui_theme_name};
    use crate::terminal::TerminalTheme;

    fn color(value: u32) -> Rgba {
        rgb(value)
    }

    /// The same color within a step of 8-bit rounding: colors pass through
    /// hex and HSL on their way into the theme.
    fn assert_close(a: Rgba, b: Rgba, what: &str) {
        let near = |x: f32, y: f32| (x - y).abs() <= 1.5 / 255.;
        assert!(
            near(a.r, b.r) && near(a.g, b.g) && near(a.b, b.b),
            "{what}: {a:?} is not {b:?}"
        );
    }

    #[test]
    fn colors_go_to_the_ansi_color_of_their_hue() {
        // What the default themes use besides their base colors: Tailwind's
        // red, yellow, green, cyan and blue, the selection's blue.
        let cases = [
            (0xef4444, 1),
            (0xf87171, 1),
            (0xeab308, 3),
            (0xfacc15, 3),
            (0x22c55e, 2),
            (0x4ade80, 2),
            (0x06b6d4, 6),
            (0x22d3ee, 6),
            (0x55a0fc, 4),
            (0x1d4ed8, 4),
            (0x60a5fa, 4),
        ];
        for (value, ansi) in cases {
            assert_eq!(ansi_of_hue(color(value)), ansi, "{value:06x}");
        }
    }

    fn apply(theme: &'static TerminalTheme, cx: &mut App) {
        let config = ui_theme(theme, cx);
        assert_eq!(config.name, ui_theme_name(theme, cx));
        let global = Theme::global_mut(cx);
        match theme.mode() {
            ThemeMode::Light => global.light_theme = config,
            ThemeMode::Dark => global.dark_theme = config,
        }
        Theme::change(theme.mode(), None, cx);
    }

    #[gpui_kit::test]
    fn the_app_takes_each_themes_colors_and_keeps_text_readable(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            for theme in TerminalTheme::all() {
                apply(theme, cx);
                let app = cx.theme();
                assert_eq!(app.mode, theme.mode());
                let name = theme.name();
                assert_close(app.background.to_rgb(), theme.background().to_rgb(), name);
                assert_close(app.foreground.to_rgb(), theme.foreground().to_rgb(), name);
                let text = contrast(theme.foreground().to_rgb(), theme.background().to_rgb());
                let muted = contrast(app.muted_foreground.to_rgb(), app.background.to_rgb());
                assert!(
                    muted >= (text * 0.8).min(READABLE_CONTRAST) - 0.05,
                    "{}: secondary text {muted:.2}",
                    theme.name()
                );
                // The default keeps gpui-kit's own colors; the rest are the
                // theme's.
                if theme != TerminalTheme::default_for(theme.mode()) {
                    assert_close(app.danger.to_rgb(), theme.indexed(1).to_rgb(), name);
                    assert_close(app.success.to_rgb(), theme.indexed(2).to_rgb(), name);
                }
            }
        });
    }

    #[gpui_kit::test]
    fn the_default_themes_are_gpui_kits_own(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            let white = cx.theme().background;
            apply(&TerminalTheme::all()[1], cx);
            assert_ne!(cx.theme().background, white);
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                apply(TerminalTheme::default_for(mode), cx);
                assert_eq!(
                    cx.theme().theme_name().as_ref(),
                    match mode {
                        ThemeMode::Light => "Default Light",
                        ThemeMode::Dark => "Default Dark",
                    }
                );
            }
            apply(TerminalTheme::default_for(ThemeMode::Light), cx);
            assert_eq!(cx.theme().background, white);
        });
    }
}
