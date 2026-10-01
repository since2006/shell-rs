use gpui_kit::WindowAppearance;
use gpui_kit::component::ThemeMode;
use serde::{Deserialize, Serialize};

use crate::explorer::FileSizeFormat;
use crate::terminal::{DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE};

/// Everything the settings page changes. Each field falls back to its
/// default when the file does not mention it, so a file written by an older
/// version still loads.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub language: InterfaceLanguage,
    pub appearance: Appearance,
    pub terminal_font: TerminalFontSettings,
    pub external_cli: ExternalCliSettings,
    pub update: UpdateSettings,
    /// The SFTP 大小 column, switched from the column title's menu rather
    /// than the settings page, as in WinSCP.
    pub file_size_format: FileSizeFormat,
}

/// 外部 CLI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalCliSettings {
    /// Whether the `shellrs` command may use the saved sessions. Off until
    /// the user turns it on.
    pub enabled: bool,
}

/// 关于 → 更新.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// Look for a newer ShellRS in the background and download it. On
    /// unless the user turns it off.
    pub automatic: bool,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { automatic: true }
    }
}

impl AppSettings {
    /// The settings with every number in the range the app accepts. Applied
    /// to what the file holds and to every change, so neither a hand-edited
    /// file nor a typed value can give the terminal a size of zero.
    pub fn normalized(mut self) -> Self {
        self.terminal_font = self.terminal_font.normalized();
        self
    }
}

/// 终端 → 字体配置.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalFontSettings {
    /// `None` is the theme's monospace family (Menlo on macOS).
    pub family: Option<String>,
    /// Pixels.
    pub size: f32,
    /// A multiple of `size`.
    pub line_height: f32,
}

impl Default for TerminalFontSettings {
    fn default() -> Self {
        Self {
            family: None,
            size: DEFAULT_FONT_SIZE,
            line_height: DEFAULT_LINE_HEIGHT,
        }
    }
}

impl TerminalFontSettings {
    /// Whole pixels, and line heights to two decimals, each within the
    /// terminal's range. A value that is not a number falls back to its
    /// default.
    fn normalized(mut self) -> Self {
        let clamp = |value: f32, range: std::ops::RangeInclusive<f32>, default: f32| {
            if value.is_finite() {
                value.clamp(*range.start(), *range.end())
            } else {
                default
            }
        };
        self.size = clamp(self.size, FONT_SIZE_RANGE, DEFAULT_FONT_SIZE).round();
        self.line_height =
            (clamp(self.line_height, LINE_HEIGHT_RANGE, DEFAULT_LINE_HEIGHT) * 100.).round() / 100.;
        self.family = self.family.filter(|family| !family.trim().is_empty());
        self
    }
}

/// A setting with a fixed set of values, shown as a dropdown.
pub trait Choice: Copy + PartialEq + 'static {
    /// Every value, in the order the dropdown lists them.
    const ALL: &'static [Self];
    /// How the value is written in the settings file.
    fn key(self) -> &'static str;
    fn label(self) -> &'static str;

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|choice| choice.key() == key)
    }
}

/// 界面语言. Only gpui-kit's own strings follow it for now; ShellRS's copy is
/// still Chinese until it is translated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterfaceLanguage {
    #[serde(rename = "system")]
    System,
    // The default until the English copy exists: following an English
    // system would only mix a few English words into a Chinese window.
    #[default]
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
    #[serde(rename = "en")]
    English,
}

impl Choice for InterfaceLanguage {
    const ALL: &'static [Self] = &[Self::System, Self::SimplifiedChinese, Self::English];

    fn key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::SimplifiedChinese => "zh-CN",
            Self::English => "en",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::System => "跟随系统",
            Self::SimplifiedChinese => "简体中文",
            // A language is named in itself, so it can be found by someone
            // who cannot read the rest of the menu.
            Self::English => "English",
        }
    }
}

impl InterfaceLanguage {
    /// The locale the interface runs in. `system` is the operating system's
    /// preferred language tag, such as `zh-Hans-CN` or `en-US`; any Chinese
    /// gets Simplified Chinese, the only Chinese there is, and anything else
    /// English. When the system does not say, the interface stays Chinese.
    pub fn locale(self, system: Option<&str>) -> &'static str {
        match self {
            Self::SimplifiedChinese => "zh-CN",
            Self::English => "en",
            Self::System => match system {
                Some(tag) => {
                    let language = tag.split(['-', '_']).next().unwrap_or_default();
                    if language.eq_ignore_ascii_case("zh") {
                        "zh-CN"
                    } else {
                        "en"
                    }
                }
                None => "zh-CN",
            },
        }
    }
}

/// 应用外观.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[serde(rename = "dark")]
    Dark,
    #[serde(rename = "light")]
    Light,
    #[default]
    #[serde(rename = "system")]
    System,
}

impl Choice for Appearance {
    const ALL: &'static [Self] = &[Self::Dark, Self::Light, Self::System];

    fn key(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Dark => "深色",
            Self::Light => "浅色",
            Self::System => "跟随系统",
        }
    }
}

impl Appearance {
    /// The theme to use while the window has `window` appearance.
    pub fn theme_mode(self, window: WindowAppearance) -> ThemeMode {
        match self {
            Self::Dark => ThemeMode::Dark,
            Self::Light => ThemeMode::Light,
            Self::System => window.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_spells_each_value_as_its_key() {
        fn check<T: Choice + Serialize + for<'de> Deserialize<'de> + std::fmt::Debug>() {
            for &choice in T::ALL {
                let json = serde_json::to_string(&choice).unwrap();
                assert_eq!(json, format!("\"{}\"", choice.key()));
                assert_eq!(serde_json::from_str::<T>(&json).unwrap(), choice);
                assert_eq!(T::from_key(choice.key()), Some(choice));
            }
        }
        check::<InterfaceLanguage>();
        check::<Appearance>();
        assert_eq!(Appearance::from_key("sepia"), None);
    }

    #[test]
    fn a_file_missing_a_setting_keeps_its_default() {
        let settings: AppSettings = serde_json::from_str(r#"{"appearance":"dark"}"#).unwrap();
        assert_eq!(settings.appearance, Appearance::Dark);
        assert_eq!(settings.language, InterfaceLanguage::default());
        let settings: AppSettings = serde_json::from_str(r#"{"later":1}"#).unwrap();
        assert_eq!(settings, AppSettings::default());
        assert_eq!(settings.file_size_format, FileSizeFormat::Kilobytes);
        let settings: AppSettings =
            serde_json::from_str(r#"{"file_size_format":"short"}"#).unwrap();
        assert_eq!(settings.file_size_format, FileSizeFormat::Short);
    }

    #[test]
    fn automatic_updates_are_on_unless_the_file_says_otherwise() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.update.automatic);
        let settings: AppSettings = serde_json::from_str(r#"{"update":{}}"#).unwrap();
        assert!(settings.update.automatic);
        let settings: AppSettings =
            serde_json::from_str(r#"{"update":{"automatic":false}}"#).unwrap();
        assert!(!settings.update.automatic);
    }

    #[test]
    fn terminal_font_numbers_are_kept_in_range_and_rounded() {
        let font = |size, line_height| TerminalFontSettings {
            family: Some("Menlo".into()),
            size,
            line_height,
        };
        let normalized = AppSettings {
            // What stepping 1.54 by 0.1 leaves behind.
            terminal_font: font(13.4, 1.54 + 0.1),
            ..AppSettings::default()
        }
        .normalized()
        .terminal_font;
        assert_eq!((normalized.size, normalized.line_height), (13., 1.64));

        let normalized = font(2., 9.).normalized();
        assert_eq!(
            (normalized.size, normalized.line_height),
            (*FONT_SIZE_RANGE.start(), *LINE_HEIGHT_RANGE.end())
        );
        let normalized = font(f32::NAN, f32::INFINITY).normalized();
        assert_eq!(
            (normalized.size, normalized.line_height),
            (DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT)
        );
        let blank = TerminalFontSettings {
            family: Some("  ".into()),
            ..TerminalFontSettings::default()
        };
        assert_eq!(blank.normalized().family, None);
        // The default already is what the terminal had before.
        assert_eq!(
            TerminalFontSettings::default().normalized(),
            TerminalFontSettings::default()
        );
    }

    #[test]
    fn following_the_system_language_picks_chinese_or_english() {
        let system = InterfaceLanguage::System;
        assert_eq!(system.locale(Some("zh-Hans-CN")), "zh-CN");
        assert_eq!(system.locale(Some("zh_TW")), "zh-CN");
        assert_eq!(system.locale(Some("ZH")), "zh-CN");
        assert_eq!(system.locale(Some("en-US")), "en");
        assert_eq!(system.locale(Some("ja-JP")), "en");
        assert_eq!(system.locale(None), "zh-CN");
        assert_eq!(InterfaceLanguage::English.locale(Some("zh-CN")), "en");
        assert_eq!(
            InterfaceLanguage::SimplifiedChinese.locale(Some("en-US")),
            "zh-CN"
        );
    }

    #[test]
    fn following_the_system_appearance_uses_the_window() {
        let system = Appearance::System;
        assert_eq!(system.theme_mode(WindowAppearance::Dark), ThemeMode::Dark);
        assert_eq!(
            system.theme_mode(WindowAppearance::VibrantLight),
            ThemeMode::Light
        );
        assert_eq!(
            Appearance::Light.theme_mode(WindowAppearance::Dark),
            ThemeMode::Light
        );
        assert_eq!(
            Appearance::Dark.theme_mode(WindowAppearance::Light),
            ThemeMode::Dark
        );
    }
}
