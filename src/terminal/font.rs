//! The terminal font: family, size and line height. The settings set it as a
//! global and every terminal view reads it, so a change reaches open tabs at
//! once. Also the preview of it that the settings page shows.

use std::{collections::HashSet, ops::RangeInclusive, sync::OnceLock};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use unicode_width::UnicodeWidthChar as _;

use super::TerminalColors;

/// The size terminals had before it could be set: the theme's `mono_md`.
pub const DEFAULT_FONT_SIZE: f32 = 13.;
/// The rows of 20 px that terminals had at the default size.
pub const DEFAULT_LINE_HEIGHT: f32 = 1.54;
/// Font sizes a terminal accepts, in pixels.
pub const FONT_SIZE_RANGE: RangeInclusive<f32> = 10.0..=28.0;
/// Line heights a terminal accepts, as multiples of the font size.
pub const LINE_HEIGHT_RANGE: RangeInclusive<f32> = 1.0..=2.0;

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalFont {
    /// `None` is the theme's monospace family.
    pub family: Option<SharedString>,
    pub size: Pixels,
    /// A multiple of `size`.
    pub line_height: f32,
}

impl Global for TerminalFont {}

impl Default for TerminalFont {
    fn default() -> Self {
        Self {
            family: None,
            size: px(DEFAULT_FONT_SIZE),
            line_height: DEFAULT_LINE_HEIGHT,
        }
    }
}

impl TerminalFont {
    /// The font the terminals use now.
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_default()
    }

    pub fn family(&self, cx: &App) -> SharedString {
        self.family
            .clone()
            .unwrap_or_else(|| cx.theme().mono_font_family.clone())
    }

    /// The height of one row, on whole device pixels.
    pub fn row_height(&self, window: &Window) -> Pixels {
        window.pixel_snap(self.size * self.line_height).max(px(1.))
    }
}

/// Whether the system has `family`. GPUI substitutes another font for a
/// family it cannot find, so a family from the settings file is only used
/// when it is installed. The list is read once per process.
pub fn is_font_installed(family: &str, cx: &App) -> bool {
    static INSTALLED: OnceLock<HashSet<String>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| cx.text_system().all_font_names().into_iter().collect())
        .contains(family)
}

/// The installed families fit for a terminal, alphabetically: every letter
/// as wide as the next, and narrower than a square, which leaves out symbol
/// fonts such as Webdings drawn on fixed squares. Hidden system families
/// (named with a leading dot) are left out.
///
/// Loads every family, around 150 ms on macOS, so it belongs off the main
/// thread. The answer is kept for the rest of the process.
pub fn monospace_font_families(text_system: &TextSystem) -> &'static [SharedString] {
    static MONOSPACE: OnceLock<Vec<SharedString>> = OnceLock::new();
    MONOSPACE.get_or_init(|| {
        let size = px(16.);
        text_system
            .all_font_names()
            .into_iter()
            .filter(|family| !family.starts_with('.'))
            .filter(|family| {
                let id = text_system.resolve_font(&font(family.clone()));
                match (
                    text_system.advance(id, size, 'i'),
                    text_system.advance(id, size, 'M'),
                ) {
                    (Ok(narrow), Ok(wide)) => {
                        (narrow.width - wide.width).abs() < px(0.01) && wide.width < size * 0.8
                    }
                    _ => false,
                }
            })
            .map(SharedString::from)
            .collect()
    })
}

/// Sample lines in the terminal font, laid out the way a terminal lays them
/// out: one glyph per cell, wide characters such as Chinese over two, rows
/// at the terminal's row height. Colors are the terminal's.
#[derive(IntoElement)]
pub struct TerminalFontPreview {
    id: ElementId,
    lines: &'static [&'static str],
}

impl TerminalFontPreview {
    pub fn new(id: impl Into<ElementId>, lines: &'static [&'static str]) -> Self {
        Self {
            id: id.into(),
            lines,
        }
    }
}

impl RenderOnce for TerminalFontPreview {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let font = TerminalFont::current(cx);
        let family = font.family(cx);
        let row_height = font.row_height(window);
        let lines = self.lines;
        let color = TerminalColors::current(cx).foreground();

        div()
            .id(self.id)
            .test_support()
            // What the preview shows, for tests and screen readers.
            .aria_label(format!(
                "{family} {} px，行高 {}",
                font.size.as_f32(),
                font.line_height
            ))
            .w_full()
            .h(row_height * lines.len() as f32)
            .overflow_hidden()
            .font_family(family)
            .text_size(font.size)
            .child(
                canvas(
                    move |_, window, _| {
                        let text_style = window.text_style();
                        let size = text_style.font_size.to_pixels(window.rem_size());
                        let run = |len| TextRun {
                            len,
                            font: text_style.font(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        };
                        let cell_width = window
                            .text_system()
                            .shape_line("M".into(), size, &[run(1)], None)
                            .width()
                            .max(px(1.));
                        lines
                            .iter()
                            .map(|line| {
                                let cells = in_cells(line);
                                let len = cells.len();
                                window.text_system().shape_line(
                                    cells.into(),
                                    size,
                                    &[run(len)],
                                    Some(cell_width),
                                )
                            })
                            .collect::<Vec<_>>()
                    },
                    move |bounds, lines, window, cx| {
                        for (row, line) in lines.iter().enumerate() {
                            let _ = line.paint(
                                point(bounds.left(), bounds.top() + row_height * row as f32),
                                row_height,
                                TextAlign::Left,
                                Some(bounds.size.width),
                                window,
                                cx,
                            );
                        }
                    },
                )
                .size_full(),
            )
    }
}

/// `line` as terminal cells: a wide character is followed by the blank its
/// second cell holds, as in the emulator's grid.
fn in_cells(line: &str) -> String {
    let mut cells = String::with_capacity(line.len() * 2);
    for character in line.chars() {
        cells.push(character);
        if character.width() == Some(2) {
            cells.push(' ');
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::in_cells;

    #[test]
    fn wide_characters_take_two_cells() {
        assert_eq!(in_cells("ab"), "ab");
        assert_eq!(in_cells("中文：a"), "中 文 ： a");
        // Box drawing is narrow, as the emulator counts it.
        assert_eq!(in_cells("┌─┐"), "┌─┐");
    }
}
