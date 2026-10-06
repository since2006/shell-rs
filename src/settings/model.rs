use gpui_kit::WindowAppearance;
use gpui_kit::component::ThemeMode;
use serde::{Deserialize, Deserializer, Serialize};

use crate::explorer::{FileSizeFormat, ShowHiddenFiles};
use crate::terminal::{
    DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, FONT_SIZE_RANGE, HighlightRule, LINE_HEIGHT_RANGE,
    default_rules,
};
use crate::update::Channel;

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
    /// 显示隐藏文件 in the SFTP lists, each side on its own, switched from
    /// their toolbars and menus.
    pub show_hidden: ShowHiddenFiles,
    /// 终端 → 通知.
    pub notifications: NotificationSettings,
    /// 关键字高亮.
    pub terminal_highlight: TerminalHighlightSettings,
}

/// 关键字高亮: the rules every terminal colors its text by, in the order
/// they win where they overlap. A file without them gets the examples a new
/// installation starts with; one the user emptied stays empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalHighlightSettings {
    #[serde(deserialize_with = "readable_rules")]
    pub rules: Vec<HighlightRule>,
}

impl Default for TerminalHighlightSettings {
    fn default() -> Self {
        Self {
            rules: default_rules(),
        }
    }
}

/// The rules the file holds that this version can read. One it cannot (a
/// color from a newer version, a hand-made typo) is dropped on its own:
/// failing the whole file would put every setting back to its default.
fn readable_rules<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<HighlightRule>, D::Error> {
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

/// 终端 → 通知: what a terminal may tell the user about while they look
/// elsewhere. Both on until the user turns them off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationSettings {
    /// 程序发送的通知: OSC 9 and 777.
    pub programs: bool,
    /// 响铃时通知.
    pub bell: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            programs: true,
            bell: true,
        }
    }
}

/// 外部 CLI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExternalCliSettings {
    /// Whether the `shellrs` command may use the saved hosts. Off until
    /// the user turns it on.
    pub enabled: bool,
}

/// 关于 → 应用更新.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    /// 自动升级: look for a newer ShellRS in the background and download
    /// it. On unless the user turns it off.
    pub automatic: bool,
    /// 更新渠道. A release build starts on the channel it was built for.
    pub channel: Channel,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            automatic: true,
            channel: Channel::of_this_build().unwrap_or(Channel::Stable),
        }
    }
}

impl Choice for Channel {
    const ALL: &'static [Self] = &[Self::Stable, Self::Beta];

    fn key(self) -> &'static str {
        Channel::key(self)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Stable => "稳定版",
            Self::Beta => "Beta",
        }
    }
}

impl AppSettings {
    /// The settings with every number in the range the app accepts. Applied
    /// to what the file holds and to every change, so neither a hand-edited
    /// file nor a typed value can give the terminal a size of zero.
    pub fn normalized(mut self) -> Self {
        self.terminal_font = self.terminal_font.normalized();
        // A rule with nothing to match matches nothing.
        self.terminal_highlight
            .rules
            .retain(|rule| !rule.pattern.trim().is_empty());
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
        // Hidden files are left out unless the file says otherwise, for
        // each side on its own.
        assert_eq!(
            AppSettings::default().show_hidden,
            ShowHiddenFiles::default()
        );
        let settings: AppSettings =
            serde_json::from_str(r#"{"show_hidden":{"remote":true}}"#).unwrap();
        assert!(settings.show_hidden.remote);
        assert!(!settings.show_hidden.local);
    }

    #[test]
    fn notifications_are_on_unless_the_file_says_otherwise() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.notifications.programs && settings.notifications.bell);
        let settings: AppSettings =
            serde_json::from_str(r#"{"notifications":{"bell":false}}"#).unwrap();
        assert!(settings.notifications.programs && !settings.notifications.bell);
    }

    #[test]
    fn highlight_rules_start_as_examples_and_stay_as_the_user_leaves_them() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.terminal_highlight.rules, default_rules());
        let settings: AppSettings =
            serde_json::from_str(r#"{"terminal_highlight":{"rules":[]}}"#).unwrap();
        assert!(settings.terminal_highlight.rules.is_empty());
        let json = serde_json::to_value(AppSettings::default()).unwrap();
        assert_eq!(
            json["terminal_highlight"]["rules"][0],
            serde_json::json!({
                "pattern": "ERROR",
                "kind": "keyword",
                "color": "red",
                "bold": true,
                "notify": false
            })
        );
        assert_eq!(json["terminal_highlight"]["rules"][2]["kind"], "regex");
    }

    #[test]
    fn a_rule_this_version_cannot_read_costs_only_itself() {
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "appearance": "dark",
                "terminal_highlight": {"rules": [
                    {"pattern": "FATAL", "color": "orange"},
                    {"pattern": "OOM", "color": "magenta", "notify": true},
                    7
                ]}
            }"#,
        )
        .unwrap();
        assert_eq!(settings.appearance, Appearance::Dark);
        let rules = &settings.terminal_highlight.rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "OOM");
        assert!(rules[0].notify && !rules[0].bold);
    }

    #[test]
    fn a_rule_with_nothing_to_match_is_dropped() {
        let mut settings = AppSettings::default();
        settings.terminal_highlight.rules[1].pattern = "  ".into();
        let rules = settings.normalized().terminal_highlight.rules;
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.pattern.as_str())
                .collect::<Vec<_>>(),
            ["ERROR", r"\b\d{1,3}(\.\d{1,3}){3}\b"]
        );
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
    fn the_update_channel_is_the_builds_until_one_is_chosen() {
        // A development build has no channel of its own.
        let settings: AppSettings = serde_json::from_str(r#"{"update":{}}"#).unwrap();
        assert_eq!(settings.update.channel, Channel::Stable);
        let settings: AppSettings =
            serde_json::from_str(r#"{"update":{"channel":"beta"}}"#).unwrap();
        assert_eq!(settings.update.channel, Channel::Beta);
        assert_eq!(
            serde_json::to_value(&settings.update).unwrap()["channel"],
            "beta"
        );
        assert_eq!(Channel::from_key("beta"), Some(Channel::Beta));
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
