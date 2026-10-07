//! 终端主题: the colors a terminal draws with — its text, background,
//! cursor, selection and the 16 ANSI colors. The settings choose one for the
//! light appearance and one for the dark and set the one in effect as a
//! global; every terminal view reads it, so a change reaches open tabs at
//! once. Also what a program is told when it asks for a color (OSC 4, 10,
//! 11, 12).

use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::*;

/// A built-in theme. Its colors are `0xrrggbb`: data, like a picture's, not
/// theme roles, so they are the same whatever the app's appearance.
#[derive(Debug, PartialEq, Eq)]
pub struct TerminalTheme {
    /// What the settings file stores. Never changes once released.
    key: &'static str,
    name: &'static str,
    /// The appearance it is made for: the column it is listed in.
    mode: ThemeMode,
    foreground: u32,
    background: u32,
    cursor: u32,
    /// Opaque: drawn under selected text in place of its background.
    selection: u32,
    /// Black, red, green, yellow, blue, magenta, cyan, white, then their
    /// bright versions.
    ansi: [u32; 16],
}

impl TerminalTheme {
    /// The built-in themes: the light ones, then the dark ones.
    pub fn all() -> &'static [TerminalTheme] {
        &THEMES
    }

    /// The themes made for `mode`, in the order they are listed.
    pub fn for_mode(mode: ThemeMode) -> impl Iterator<Item = &'static TerminalTheme> {
        THEMES.iter().filter(move |theme| theme.mode == mode)
    }

    pub fn find(key: &str) -> Option<&'static TerminalTheme> {
        THEMES.iter().find(|theme| theme.key == key)
    }

    /// What terminals use for `mode` until another is chosen: the look they
    /// had before themes could be chosen, the app's own background and text.
    pub fn default_for(mode: ThemeMode) -> &'static TerminalTheme {
        Self::for_mode(mode)
            .next()
            .expect("each appearance has its themes")
    }

    pub fn key(&self) -> &'static str {
        self.key
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn mode(&self) -> ThemeMode {
        self.mode
    }

    pub fn foreground(&self) -> Hsla {
        rgb(self.foreground).into()
    }

    pub fn background(&self) -> Hsla {
        rgb(self.background).into()
    }

    pub fn cursor(&self) -> Hsla {
        rgb(self.cursor).into()
    }

    pub fn selection(&self) -> Hsla {
        rgb(self.selection).into()
    }

    /// One of the 256 colors a program picks by number: the 16 ANSI colors
    /// are the theme's, the 6×6×6 cube and the gray ramp above them xterm's.
    pub fn indexed(&self, index: u8) -> Hsla {
        rgb(self.indexed_rgb(index)).into()
    }

    /// The answer to a program asking for color `index` it has not set
    /// itself, numbered as alacritty numbers its colors: 0–255 by number,
    /// then the named ones (foreground, background, cursor …).
    pub fn query_color(&self, index: usize) -> Rgb {
        let value = match index {
            0..=255 => self.indexed_rgb(index as u8),
            _ if index == NamedColor::Background as usize => self.background,
            _ if index == NamedColor::Cursor as usize => self.cursor,
            // The foreground, and the dim and bright versions of it and of
            // the ANSI colors, which programs cannot ask for by OSC.
            _ => self.foreground,
        };
        Rgb {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }

    fn indexed_rgb(&self, index: u8) -> u32 {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let index = index - 16;
                let component = |value: u8| {
                    if value == 0 {
                        0
                    } else {
                        55 + value as u32 * 40
                    }
                };
                (component(index / 36) << 16)
                    | (component((index % 36) / 6) << 8)
                    | component(index % 6)
            }
            232..=255 => {
                let value = 8 + (index as u32 - 232) * 10;
                (value << 16) | (value << 8) | value
            }
        }
    }
}

/// The theme terminals draw with now, set by the settings for the app's
/// current appearance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalColors(&'static TerminalTheme);

impl Global for TerminalColors {}

impl TerminalColors {
    pub fn new(theme: &'static TerminalTheme) -> Self {
        Self(theme)
    }

    /// The theme in effect; before the settings set one, the default for
    /// the app's appearance.
    pub fn current(cx: &App) -> &'static TerminalTheme {
        cx.try_global::<Self>()
            .map(|colors| colors.0)
            .unwrap_or_else(|| TerminalTheme::default_for(cx.theme().mode))
    }

    pub fn theme(&self) -> &'static TerminalTheme {
        self.0
    }
}

/// The app's light theme around a terminal: white, `neutral-950` text, the
/// `primary` cursor and the `selection` over white, with ANSI colors from
/// the same Tailwind palette, dark enough to read on white.
const SHELLRS_LIGHT: TerminalTheme = TerminalTheme {
    key: "shellrs-light",
    name: "ShellRS Light",
    mode: ThemeMode::Light,
    foreground: 0x0a0a0a,
    background: 0xffffff,
    cursor: 0x171717,
    selection: 0xcce2fe,
    ansi: [
        0x171717, 0xdc2626, 0x15803d, 0xa16207, 0x2563eb, 0xc026d3, 0x0e7490, 0xa3a3a3, 0x737373,
        0xef4444, 0x16a34a, 0xd97706, 0x3b82f6, 0xd946ef, 0x0891b2, 0xd4d4d4,
    ],
};

/// The app's dark theme around a terminal: its charcoal background and soft
/// text and cursor, the `selection` over the background, with ANSI colors
/// light enough to read on it; black is the app's borders.
const SHELLRS_DARK: TerminalTheme = TerminalTheme {
    key: "shellrs-dark",
    name: "ShellRS Dark",
    mode: ThemeMode::Dark,
    foreground: 0xe0e0e4,
    background: 0x1e1e1f,
    cursor: 0xe0e0e4,
    selection: 0x1e2c56,
    ansi: [
        0x3b3b3d, 0xf87171, 0x4ade80, 0xfacc15, 0x60a5fa, 0xe879f9, 0x22d3ee, 0xd4d4d4, 0x737373,
        0xfca5a5, 0x86efac, 0xfde047, 0x93c5fd, 0xf0abfc, 0x67e8f9, 0xfafafa,
    ],
};

/// Each column's themes in the order it lists them, its default first.
/// The light and dark versions of one theme share a row; a theme without the
/// other version comes after them.
static THEMES: [TerminalTheme; 20] = [
    SHELLRS_LIGHT,
    SOLARIZED_LIGHT,
    TOKYO_DAY,
    CATPPUCCIN_LATTE,
    FLEXOKI_LIGHT,
    ONE_HALF_LIGHT,
    GRUVBOX_LIGHT,
    KANAGAWA_LOTUS,
    ROSE_PINE_DAWN,
    EVERFOREST_LIGHT,
    SHELLRS_DARK,
    SOLARIZED_DARK,
    TOKYO_NIGHT,
    CATPPUCCIN_MOCHA,
    FLEXOKI_DARK,
    ONE_HALF_DARK,
    GRUVBOX_DARK,
    KANAGAWA_WAVE,
    DRACULA,
    NORD,
];

// The rest, each as its own project's terminal port has it.

/// https://raw.githubusercontent.com/altercation/solarized/master/README.md (MIT).
/// The palette table of the README, laid out as the official iTerm2 files
/// lay it out.
const SOLARIZED_LIGHT: TerminalTheme = TerminalTheme {
    key: "solarized-light",
    name: "Solarized Light",
    mode: ThemeMode::Light,
    foreground: 0x657b83,
    background: 0xfdf6e3,
    cursor: 0x657b83,
    selection: 0xeee8d5,
    ansi: [
        0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36,
        0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
    ],
};

/// https://raw.githubusercontent.com/folke/tokyonight.nvim/main/extras/ghostty/tokyonight_day (Apache-2.0).
/// The project calls it Tokyo Night Day.
const TOKYO_DAY: TerminalTheme = TerminalTheme {
    key: "tokyo-day",
    name: "Tokyo Day",
    mode: ThemeMode::Light,
    foreground: 0x3760bf,
    background: 0xe1e2e7,
    cursor: 0x3760bf,
    selection: 0xb7c1e3,
    ansi: [
        0xb4b5b9, 0xf52a65, 0x587539, 0x8c6c3e, 0x2e7de9, 0x9854f1, 0x007197, 0x6172b0, 0xa1a6c5,
        0xff4774, 0x5c8524, 0xa27629, 0x358aff, 0xa463ff, 0x007ea8, 0x3760bf,
    ],
};

/// https://raw.githubusercontent.com/catppuccin/ghostty/main/themes/catppuccin-latte.conf (MIT).
const CATPPUCCIN_LATTE: TerminalTheme = TerminalTheme {
    key: "catppuccin-latte",
    name: "Catppuccin Latte",
    mode: ThemeMode::Light,
    foreground: 0x4c4f69,
    background: 0xeff1f5,
    cursor: 0xdc8a78,
    selection: 0xd8dae1,
    ansi: [
        0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be, 0x6c6f85,
        0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
    ],
};

/// https://raw.githubusercontent.com/kepano/flexoki/main/iterm2/flexoki_light.itermcolors (MIT).
/// The selection is the kitty port's: this file selects by inverting.
const FLEXOKI_LIGHT: TerminalTheme = TerminalTheme {
    key: "flexoki-light",
    name: "Flexoki Light",
    mode: ThemeMode::Light,
    foreground: 0x100f0f,
    background: 0xfffcf0,
    cursor: 0x100f0f,
    selection: 0xcecdc3,
    ansi: [
        0x100f0f, 0xaf3029, 0x66800b, 0xad8301, 0x205ea6, 0xa02f6f, 0x24837b, 0x6f6e69, 0xb7b5ac,
        0xd14d41, 0x879a39, 0xd0a215, 0x4385be, 0xce5d97, 0x3aa99f, 0xcecdc3,
    ],
};

/// https://raw.githubusercontent.com/sonph/onehalf/master/iterm/OneHalfLight.itermcolors (MIT).
const ONE_HALF_LIGHT: TerminalTheme = TerminalTheme {
    key: "one-half-light",
    name: "One Half Light",
    mode: ThemeMode::Light,
    foreground: 0x383a42,
    background: 0xfafafa,
    cursor: 0xbfceff,
    selection: 0xbfceff,
    ansi: [
        0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x0184bc, 0xa626a4, 0x0997b3, 0xfafafa, 0x4f525e,
        0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff,
    ],
};

/// https://raw.githubusercontent.com/morhetz/gruvbox-contrib/master/termite/gruvbox-light (MIT).
/// No cursor there: it is the text. The selection is `bg3`, what gruvbox.vim
/// selects with when not inverting.
const GRUVBOX_LIGHT: TerminalTheme = TerminalTheme {
    key: "gruvbox-light",
    name: "Gruvbox Light",
    mode: ThemeMode::Light,
    foreground: 0x3c3836,
    background: 0xfbf1c7,
    cursor: 0x3c3836,
    selection: 0xbdae93,
    ansi: [
        0xfbf1c7, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0x7c6f64, 0x928374,
        0x9d0006, 0x79740e, 0xb57614, 0x076678, 0x8f3f71, 0x427b58, 0x3c3836,
    ],
};

/// https://raw.githubusercontent.com/rebelot/kanagawa.nvim/master/extras/ghostty/kanagawa-lotus (MIT).
const KANAGAWA_LOTUS: TerminalTheme = TerminalTheme {
    key: "kanagawa-lotus",
    name: "Kanagawa Lotus",
    mode: ThemeMode::Light,
    foreground: 0x545464,
    background: 0xf2ecbc,
    cursor: 0x43436c,
    selection: 0xc9cbd1,
    ansi: [
        0x1f1f28, 0xc84053, 0x6f894e, 0x77713f, 0x4d699b, 0xb35b79, 0x597b75, 0x545464, 0x8a8980,
        0xd7474b, 0x6e915f, 0x836f4a, 0x6693bf, 0x624c83, 0x5e857a, 0x43436c,
    ],
};

/// https://raw.githubusercontent.com/rose-pine/ghostty/main/dist/rose-pine-dawn (MIT).
const ROSE_PINE_DAWN: TerminalTheme = TerminalTheme {
    key: "rose-pine-dawn",
    name: "Rosé Pine Dawn",
    mode: ThemeMode::Light,
    foreground: 0x575279,
    background: 0xfaf4ed,
    cursor: 0x575279,
    selection: 0xdfdad9,
    ansi: [
        0xf2e9e1, 0xb4637a, 0x286983, 0xea9d34, 0x56949f, 0x907aa9, 0xd7827e, 0x575279, 0x9893a5,
        0xb4637a, 0x286983, 0xea9d34, 0x56949f, 0x907aa9, 0xd7827e, 0x575279,
    ],
};

/// https://raw.githubusercontent.com/sainnhe/everforest/master/colors/everforest.vim
/// and autoload/everforest.vim (MIT): the medium light palette, the 16
/// colors as the color scheme gives them to Vim's terminal, `bg_visual` for
/// the selection. It sets no cursor color by default: the cursor is the
/// text.
const EVERFOREST_LIGHT: TerminalTheme = TerminalTheme {
    key: "everforest-light",
    name: "Everforest Light",
    mode: ThemeMode::Light,
    foreground: 0x5c6a72,
    background: 0xfdf6e3,
    cursor: 0x5c6a72,
    selection: 0xeaedc8,
    ansi: [
        0x5c6a72, 0xf85552, 0x8da101, 0xdfa000, 0x3a94c5, 0xdf69ba, 0x35a77c, 0xe6e2cc, 0x5c6a72,
        0xf85552, 0x8da101, 0xdfa000, 0x3a94c5, 0xdf69ba, 0x35a77c, 0xe6e2cc,
    ],
};

/// https://raw.githubusercontent.com/altercation/solarized/master/README.md (MIT).
/// The palette table of the README, laid out as the official iTerm2 files
/// lay it out.
const SOLARIZED_DARK: TerminalTheme = TerminalTheme {
    key: "solarized-dark",
    name: "Solarized Dark",
    mode: ThemeMode::Dark,
    foreground: 0x839496,
    background: 0x002b36,
    cursor: 0x839496,
    selection: 0x073642,
    ansi: [
        0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36,
        0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
    ],
};

/// https://raw.githubusercontent.com/folke/tokyonight.nvim/main/extras/ghostty/tokyonight_night (Apache-2.0).
const TOKYO_NIGHT: TerminalTheme = TerminalTheme {
    key: "tokyo-night",
    name: "Tokyo Night",
    mode: ThemeMode::Dark,
    foreground: 0xc0caf5,
    background: 0x1a1b26,
    cursor: 0xc0caf5,
    selection: 0x283457,
    ansi: [
        0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868,
        0xff899d, 0x9fe044, 0xfaba4a, 0x8db0ff, 0xc7a9ff, 0xa4daff, 0xc0caf5,
    ],
};

/// https://raw.githubusercontent.com/catppuccin/ghostty/main/themes/catppuccin-mocha.conf (MIT).
const CATPPUCCIN_MOCHA: TerminalTheme = TerminalTheme {
    key: "catppuccin-mocha",
    name: "Catppuccin Mocha",
    mode: ThemeMode::Dark,
    foreground: 0xcdd6f4,
    background: 0x1e1e2e,
    cursor: 0xf5e0dc,
    selection: 0x353749,
    ansi: [
        0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8, 0x585b70,
        0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
    ],
};

/// https://raw.githubusercontent.com/kepano/flexoki/main/iterm2/flexoki_dark.itermcolors (MIT).
/// The selection is the kitty port's: this file selects by inverting.
const FLEXOKI_DARK: TerminalTheme = TerminalTheme {
    key: "flexoki-dark",
    name: "Flexoki Dark",
    mode: ThemeMode::Dark,
    foreground: 0xcecdc3,
    background: 0x100f0f,
    cursor: 0xcecdc3,
    selection: 0x403e3c,
    ansi: [
        0x100f0f, 0xd14d41, 0x879a39, 0xd0a215, 0x4385be, 0xce5d97, 0x3aa99f, 0x878580, 0x575653,
        0xaf3029, 0x66800b, 0xad8301, 0x205ea6, 0xa02f6f, 0x24837b, 0xcecdc3,
    ],
};

/// https://raw.githubusercontent.com/sonph/onehalf/master/iterm/OneHalfDark.itermcolors (MIT).
/// Bright black is Windows Terminal's `#5a6374`: here it is the
/// background, which hides text in it.
const ONE_HALF_DARK: TerminalTheme = TerminalTheme {
    key: "one-half-dark",
    name: "One Half Dark",
    mode: ThemeMode::Dark,
    foreground: 0xdcdfe4,
    background: 0x282c34,
    cursor: 0xa3b3cc,
    selection: 0x474e5d,
    ansi: [
        0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4, 0x5a6374,
        0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xdcdfe4,
    ],
};

/// https://raw.githubusercontent.com/morhetz/gruvbox-contrib/master/termite/gruvbox-dark (MIT).
/// No cursor there: it is the text. The selection is `bg3`, what gruvbox.vim
/// selects with when not inverting.
const GRUVBOX_DARK: TerminalTheme = TerminalTheme {
    key: "gruvbox-dark",
    name: "Gruvbox Dark",
    mode: ThemeMode::Dark,
    foreground: 0xebdbb2,
    background: 0x282828,
    cursor: 0xebdbb2,
    selection: 0x665c54,
    ansi: [
        0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984, 0x928374,
        0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
    ],
};

/// https://raw.githubusercontent.com/rebelot/kanagawa.nvim/master/extras/ghostty/kanagawa-wave (MIT).
const KANAGAWA_WAVE: TerminalTheme = TerminalTheme {
    key: "kanagawa-wave",
    name: "Kanagawa Wave",
    mode: ThemeMode::Dark,
    foreground: 0xdcd7ba,
    background: 0x1f1f28,
    cursor: 0xc8c093,
    selection: 0x2d4f67,
    ansi: [
        0x16161d, 0xc34043, 0x76946a, 0xc0a36e, 0x7e9cd8, 0x957fb8, 0x6a9589, 0xc8c093, 0x727169,
        0xe82424, 0x98bb6c, 0xe6c384, 0x7fb4ca, 0x938aa9, 0x7aa89f, 0xdcd7ba,
    ],
};

/// https://raw.githubusercontent.com/dracula/windows-terminal/master/dracula.json (MIT).
const DRACULA: TerminalTheme = TerminalTheme {
    key: "dracula",
    name: "Dracula",
    mode: ThemeMode::Dark,
    foreground: 0xf8f8f2,
    background: 0x282a36,
    cursor: 0xf8f8f2,
    selection: 0x44475a,
    ansi: [
        0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2, 0x6272a4,
        0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
    ],
};

/// https://raw.githubusercontent.com/nordtheme/alacritty/main/src/nord.yaml (MIT).
const NORD: TerminalTheme = TerminalTheme {
    key: "nord",
    name: "Nord",
    mode: ThemeMode::Dark,
    foreground: 0xd8dee9,
    background: 0x2e3440,
    cursor: 0xd8dee9,
    selection: 0x4c566a,
    ansi: [
        0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0, 0x4c566a,
        0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
    ],
};

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use alacritty_terminal::vte::ansi::{NamedColor, Rgb};
    use gpui_kit::component::ThemeMode;

    use super::TerminalTheme;

    /// WCAG relative luminance.
    fn luminance(color: u32) -> f32 {
        let channel = |shift: u32| {
            let value = ((color >> shift) & 0xff) as f32 / 255.;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
    }

    fn contrast(a: u32, b: u32) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn keys_are_unique_and_each_theme_suits_its_column() {
        let mut keys = HashSet::new();
        for theme in TerminalTheme::all() {
            assert!(keys.insert(theme.key), "{} twice", theme.key);
            assert_eq!(TerminalTheme::find(theme.key), Some(theme));
            assert_eq!(
                luminance(theme.background) > 0.5,
                theme.mode == ThemeMode::Light,
                "{} has a background of the other appearance",
                theme.name
            );
            // Solarized is soft by design, about 4:1; this catches a text
            // color swapped for another.
            assert!(
                contrast(theme.foreground, theme.background) >= 3.,
                "{}'s text is hard to read",
                theme.name
            );
        }
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let default = TerminalTheme::default_for(mode);
            assert_eq!(default.mode, mode);
            assert_eq!(TerminalTheme::for_mode(mode).next(), Some(default));
        }
    }

    #[test]
    fn the_default_themes_colors_read_on_their_background() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let theme = TerminalTheme::default_for(mode);
            // Black, white and their bright versions are meant to fade into
            // one background or the other.
            for index in (1..=6).chain(9..=14) {
                let ratio = contrast(theme.ansi[index], theme.background);
                assert!(ratio >= 3., "{} color {index}: {ratio:.2}", theme.name);
            }
            // zsh's suggestions and many prompts' quiet parts are bright
            // black.
            assert!(contrast(theme.ansi[8], theme.background) >= 3.);
        }
    }

    #[test]
    fn colors_above_the_sixteen_are_xterms() {
        let theme = TerminalTheme::default_for(ThemeMode::Light);
        assert_eq!(theme.indexed_rgb(16), 0x000000);
        assert_eq!(theme.indexed_rgb(21), 0x0000ff);
        assert_eq!(theme.indexed_rgb(196), 0xff0000);
        assert_eq!(theme.indexed_rgb(231), 0xffffff);
        assert_eq!(theme.indexed_rgb(232), 0x080808);
        assert_eq!(theme.indexed_rgb(255), 0xeeeeee);
        assert_eq!(theme.indexed_rgb(4), theme.ansi[4]);
    }

    #[test]
    fn programs_asking_for_colors_are_told_the_themes() {
        let light = TerminalTheme::default_for(ThemeMode::Light);
        let rgb = |value: u32| Rgb {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        };
        // vim asks for the background to choose its light or dark colors.
        assert_eq!(
            light.query_color(NamedColor::Background as usize),
            rgb(0xffffff)
        );
        assert_eq!(
            light.query_color(NamedColor::Foreground as usize),
            rgb(light.foreground)
        );
        assert_eq!(
            light.query_color(NamedColor::Cursor as usize),
            rgb(light.cursor)
        );
        assert_eq!(light.query_color(1), rgb(light.ansi[1]));
        assert_eq!(light.query_color(232), rgb(0x080808));
        let dark = TerminalTheme::default_for(ThemeMode::Dark);
        assert_eq!(
            dark.query_color(NamedColor::Background as usize),
            rgb(0x1e1e1f)
        );
    }
}
