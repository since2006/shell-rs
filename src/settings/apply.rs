use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::*;

use crate::explorer::{FileIconStyle, FileSizeFormat, ShowHiddenFiles};
use crate::i18n::UiLocale;
use crate::terminal::{
    SendDelay, SessionLogSettings, TerminalColors, TerminalFont, TerminalHighlights,
    TerminalInteraction, is_font_installed,
};

use super::AppSettings;
use super::app_theme::{ui_theme, ui_theme_name};

/// Bring the window in line with the settings: the theme chosen for the
/// appearance, in the app and its terminals, the interface's font size, the
/// locale, the terminal font,
/// mouse and highlight rules, the keyboard shortcuts, the SFTP size format and
/// icons, how many files SFTP moves at once, and whether it shows hidden
/// files.
/// Does nothing where they already agree, so it can run on every settings
/// change and every change of the system appearance.
pub fn apply(settings: AppSettings, window: &mut Window, cx: &mut App) {
    // Here, not only on a settings change: following the system, the theme
    // changes with the appearance.
    let mode = settings.appearance.theme_mode(window.appearance());
    let chosen = settings.terminal_theme.theme(mode);
    let app = settings.terminal_theme.app_theme(mode);
    if cx.theme().mode != mode || *cx.theme().theme_name() != ui_theme_name(app, cx) {
        let config = ui_theme(app, cx);
        let theme = Theme::global_mut(cx);
        match mode {
            ThemeMode::Light => theme.light_theme = config,
            ThemeMode::Dark => theme.dark_theme = config,
        }
        Theme::change(mode, Some(window), cx);
        // The theme change resets the list hover to the theme's own.
        crate::app::deepen_list_hover(cx);
    }

    // 界面字号: the base font the interface's sizes follow. After the theme,
    // whose change keeps it.
    let font_size = px(settings.interface.font_size);
    if cx.theme().font_size != font_size {
        Theme::global_mut(cx).font_size = font_size;
        Theme::sync_base(cx);
        window.refresh();
    }

    let colors = TerminalColors::new(chosen);
    if cx.try_global::<TerminalColors>() != Some(&colors) {
        cx.set_global(colors);
        window.refresh();
    }

    // Text is translated where it is drawn, so a redraw has it in the new
    // language; entities that hand text to gpui-kit to keep observe
    // `UiLocale` and give it again.
    let locale = settings.language.resolved();
    if cx.try_global::<UiLocale>() != Some(&UiLocale(locale)) {
        crate::i18n::set_locale(locale);
        cx.set_global(UiLocale(locale));
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

    // Read when the mouse acts, so nothing is redrawn.
    if cx.try_global::<TerminalInteraction>() != Some(&settings.terminal_interaction) {
        cx.set_global(settings.terminal_interaction);
    }

    // Read when input is queued and a connection starts, so nothing is
    // redrawn either.
    if cx.try_global::<SendDelay>() != Some(&settings.send_delay) {
        cx.set_global(settings.send_delay);
    }
    if cx.try_global::<SessionLogSettings>() != Some(&settings.session_log) {
        cx.set_global(settings.session_log.clone());
    }

    // Compiled once here for every terminal, which observe it; only when
    // the rules change, not on every change of the system appearance.
    let highlight = settings.terminal_highlight;
    let unchanged = cx
        .try_global::<TerminalHighlights>()
        .is_some_and(|current| {
            current
                .set()
                .is_made_of(highlight.enabled, &highlight.rules)
        });
    if !unchanged {
        cx.set_global(TerminalHighlights::new(highlight.enabled, highlight.rules));
    }

    // Rebuilds the keymap only when the user's changes differ from the
    // ones it was built with.
    crate::app::apply_shortcuts(&settings.shortcuts, cx);

    if cx.try_global::<FileSizeFormat>() != Some(&settings.file_size_format) {
        cx.set_global(settings.file_size_format);
        window.refresh();
    }

    if cx.try_global::<FileIconStyle>() != Some(&settings.file_icon_style) {
        cx.set_global(settings.file_icon_style);
        window.refresh();
    }

    // For the SFTP worker threads, which have no app to look in.
    crate::sftp::set_parallel_transfers(settings.transfers.parallel);

    // The SFTP lists observe it and list their rows again.
    if cx.try_global::<ShowHiddenFiles>() != Some(&settings.show_hidden) {
        cx.set_global(settings.show_hidden);
    }
}
