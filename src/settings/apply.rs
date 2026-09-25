use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::*;

use crate::terminal::{TerminalFont, is_font_installed};

use super::AppSettings;

/// Bring the window in line with the settings: the theme, the locale and
/// the terminal font. Does nothing where they already agree, so it can run
/// on every settings change and every change of the system appearance.
pub fn apply(settings: AppSettings, window: &mut Window, cx: &mut App) {
    let mode = settings.appearance.theme_mode(window.appearance());
    if cx.theme().mode != mode {
        Theme::change(mode, Some(window), cx);
        // The theme change resets the list hover to the theme's own.
        crate::app::deepen_list_hover(cx);
    }

    let locale = settings
        .language
        .locale(sys_locale::get_locale().as_deref());
    if *gpui_kit::component::locale() != *locale {
        gpui_kit::component::set_locale(locale);
        window.refresh();
    }

    let font = settings.terminal_font;
    let font = TerminalFont {
        // A family uninstalled since it was chosen gives way to the default.
        family: font
            .family
            .filter(|family| is_font_installed(family, cx))
            .map(SharedString::from),
        size: px(font.size),
        line_height: font.line_height,
    };
    if cx.try_global::<TerminalFont>() != Some(&font) {
        cx.set_global(font);
        window.refresh();
    }
}
