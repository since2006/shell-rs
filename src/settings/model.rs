use gpui_kit::component::ThemeMode;
use gpui_kit::{SharedString, WindowAppearance};
use serde::{Deserialize, Deserializer, Serialize};

use std::collections::BTreeMap;

use crate::app::{ShortcutOverrides, ToolKind};
use crate::explorer::{FileIconStyle, FileSizeFormat, ShowHiddenFiles};
use crate::i18n::t;
use crate::sftp::{DEFAULT_PARALLEL_TRANSFERS, PARALLEL_TRANSFERS_RANGE};
use crate::terminal::{
    DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, FONT_SIZE_RANGE, HighlightRule, LINE_HEIGHT_RANGE,
    RightClick, SendDelay, SessionLogSettings, TerminalInteraction, TerminalTheme, default_rules,
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
    /// 外观 → 常规 → 界面字号.
    pub interface: InterfaceSettings,
    /// 外观 → 终端主题.
    pub terminal_theme: TerminalThemeSettings,
    pub terminal_font: TerminalFontSettings,
    /// 终端 → 终端交互.
    pub terminal_interaction: TerminalInteraction,
    /// 终端 → 发送延迟.
    pub send_delay: SendDelay,
    /// 终端 → 会话日志.
    pub session_log: SessionLogSettings,
    /// 终端 → 侧栏功能: which tools the right sidebar offers.
    pub sidebar_tools: SidebarTools,
    /// The right sidebar beside local terminals, switched from its tool
    /// buttons: the tool it shows, `None` while hidden. One for every local
    /// terminal, as a host keeps its own for its terminals.
    #[serde(with = "tool_by_key")]
    pub local_sidebar_tool: Option<ToolKind>,
    /// 终端 → 本地终端.
    pub local_terminal: LocalTerminalSettings,
    pub external_cli: ExternalCliSettings,
    pub update: UpdateSettings,
    /// 关于 → 隐私.
    pub analytics: AnalyticsSettings,
    /// The SFTP 大小 column, switched from the column title's menu rather
    /// than the settings page, as in WinSCP.
    pub file_size_format: FileSizeFormat,
    /// 显示隐藏文件 in the SFTP lists, each side on its own, switched from
    /// their toolbars and menus.
    pub show_hidden: ShowHiddenFiles,
    /// SFTP → 传输.
    pub transfers: TransferSettings,
    /// SFTP → 图标风格.
    pub file_icon_style: FileIconStyle,
    /// SFTP → 文件侧栏.
    pub file_sidebar: FileSidebarSettings,
    /// 终端 → 通知.
    pub notifications: NotificationSettings,
    /// 关键字高亮.
    pub terminal_highlight: TerminalHighlightSettings,
    /// 应用 → 窗口.
    pub window: WindowSettings,
    /// 键盘快捷键: what the user changed of them.
    pub shortcuts: ShortcutOverrides,
}

/// What 界面字号 offers, in pixels.
pub const INTERFACE_FONT_SIZE_RANGE: std::ops::RangeInclusive<f32> = 12.0..=20.0;
/// gpui-kit's own base font, which the interface had before it could change.
pub const DEFAULT_INTERFACE_FONT_SIZE: f32 = 16.0;

/// 外观 → 常规.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InterfaceSettings {
    /// 界面字号: the base font in pixels, which the interface's text and
    /// spacing follow. Terminals have their own.
    pub font_size: f32,
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_INTERFACE_FONT_SIZE,
        }
    }
}

/// SFTP → 传输.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferSettings {
    /// 最大同时传输数量: how many files of an upload or a download move at
    /// once; the rest wait their turn.
    pub parallel: usize,
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self {
            parallel: DEFAULT_PARALLEL_TRANSFERS,
        }
    }
}

/// SFTP → 文件侧栏: the right sidebar's 文件.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileSidebarSettings {
    /// 自动检测目录配置: on showing 文件, see whether the host's shell reports
    /// its folder and offer to set it up if not; also whether 文件 follows
    /// the terminal's folder to begin with. On unless turned off.
    pub detect_directory_follow: bool,
}

impl Default for FileSidebarSettings {
    fn default() -> Self {
        Self {
            detect_directory_follow: true,
        }
    }
}

/// 终端 → 侧栏功能: the tools the user offers in the right sidebar or takes
/// out of it, by `ToolKind::key`; a tool not in it has its default. So a
/// tool added later comes with its own default, and a key this version does
/// not know (a newer one's tool) is kept for the version that does. A value
/// that is not a yes or no is dropped on its own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SidebarTools(BTreeMap<String, bool>);

impl<'de> Deserialize<'de> for SidebarTools {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        Ok(Self(
            values
                .into_iter()
                .filter_map(|(key, value)| Some((key, value.as_bool()?)))
                .collect(),
        ))
    }
}

impl SidebarTools {
    pub fn shows(&self, tool: ToolKind) -> bool {
        self.0
            .get(tool.key())
            .copied()
            .unwrap_or(tool.shown_by_default())
    }

    /// Offer `tool`, or not. Its default is stored as no change.
    pub fn set(&mut self, tool: ToolKind, shown: bool) {
        if shown == tool.shown_by_default() {
            self.0.remove(tool.key());
        } else {
            self.0.insert(tool.key().to_owned(), shown);
        }
    }

    /// Whether every tool this version knows has its default.
    pub fn is_default(&self) -> bool {
        ToolKind::ALL
            .iter()
            .all(|tool| self.shows(*tool) == tool.shown_by_default())
    }

    /// Every tool this version knows back to its default.
    pub fn reset(&mut self) {
        for tool in ToolKind::ALL {
            self.0.remove(tool.key());
        }
    }
}

/// A tool by its `ToolKind::key`. A key this version does not know, or a
/// value that is not a key, reads as no tool: one stray value must not
/// cost the rest of the file.
mod tool_by_key {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::app::ToolKind;

    pub fn serialize<S: Serializer>(
        tool: &Option<ToolKind>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        tool.map(ToolKind::key).serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ToolKind>, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(value.as_str().and_then(ToolKind::from_key))
    }
}

/// 应用 → 窗口: what the next launch restores of the main window. Both on
/// until the user turns them off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    /// 记住窗口大小: its size, and whether it was maximized.
    pub remember_size: bool,
    /// 记住窗口位置: where it was, on which screen.
    pub remember_position: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            remember_size: true,
            remember_position: true,
        }
    }
}

/// 外观 → 主题: by key, the theme used while the app is light and the one
/// while it is dark, and whether the app takes its colors or only the
/// terminals do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalThemeSettings {
    pub light: String,
    pub dark: String,
    /// 界面跟随主题. Off until the user turns it on: the app keeps
    /// gpui-kit's own neutral light or dark colors.
    pub app_follows: bool,
}

impl Default for TerminalThemeSettings {
    fn default() -> Self {
        Self {
            light: TerminalTheme::default_for(ThemeMode::Light).key().into(),
            dark: TerminalTheme::default_for(ThemeMode::Dark).key().into(),
            app_follows: false,
        }
    }
}

impl TerminalThemeSettings {
    /// The theme the app's colors come from for `mode`: the chosen one, or
    /// the default, ShellRS Light or Dark, when the app does not follow.
    pub fn app_theme(&self, mode: ThemeMode) -> &'static TerminalTheme {
        if self.app_follows {
            self.theme(mode)
        } else {
            TerminalTheme::default_for(mode)
        }
    }

    /// Whether both columns have their defaults, the switch aside.
    pub fn is_default_choice(&self) -> bool {
        let default = Self::default();
        self.light == default.light && self.dark == default.dark
    }

    /// Both columns back to their defaults, the switch left as it is.
    pub fn reset_choice(&mut self) {
        let default = Self::default();
        self.light = default.light;
        self.dark = default.dark;
    }

    /// The theme for `mode`. A key this version does not know (from a newer
    /// one, or a typo), or one of a theme for the other appearance, gives
    /// way to the default rather than failing the file.
    pub fn theme(&self, mode: ThemeMode) -> &'static TerminalTheme {
        let key = match mode {
            ThemeMode::Light => &self.light,
            ThemeMode::Dark => &self.dark,
        };
        TerminalTheme::find(key)
            .filter(|theme| theme.mode() == mode)
            .unwrap_or_else(|| TerminalTheme::default_for(mode))
    }

    /// Use `theme` for the appearance it is made for.
    pub fn choose(&mut self, theme: &TerminalTheme) {
        let key = theme.key().to_string();
        match theme.mode() {
            ThemeMode::Light => self.light = key,
            ThemeMode::Dark => self.dark = key,
        }
    }
}

/// 关键字高亮: whether terminals color their text by the rules, and the
/// rules, in the order they win where they overlap. Off until the user turns
/// it on. A file without the rules gets the examples a new installation
/// starts with, ready once it is on; one the user emptied stays empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalHighlightSettings {
    /// 启用关键字高亮. Off, no rule colors anything or notifies.
    pub enabled: bool,
    #[serde(deserialize_with = "readable_rules")]
    pub rules: Vec<HighlightRule>,
}

impl Default for TerminalHighlightSettings {
    fn default() -> Self {
        Self {
            enabled: false,
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

/// 终端 → 本地终端.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalTerminalSettings {
    /// The program new and restarted local terminals run, a path or a
    /// command with any arguments, as typed. `None` is the system's default
    /// shell, found anew each time.
    pub shell: Option<String>,
    /// 保留终端内容: a 项目 terminal's last lines of output, kept when it
    /// closes and drawn again when it opens. On until the user turns it off.
    pub keep_output: bool,
}

impl Default for LocalTerminalSettings {
    fn default() -> Self {
        Self {
            shell: None,
            keep_output: true,
        }
    }
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

/// 关于 → 隐私.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalyticsSettings {
    /// 发送匿名使用统计. On unless the user turns it off.
    pub enabled: bool,
}

impl Default for AnalyticsSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Choice for Channel {
    const ALL: &'static [Self] = &[Self::Stable, Self::Beta];

    fn key(self) -> &'static str {
        Channel::key(self)
    }

    fn label(self) -> SharedString {
        match self {
            Self::Stable => t!("settings.channel.stable"),
            Self::Beta => "Beta".into(),
        }
    }
}

impl Choice for FileIconStyle {
    const ALL: &'static [Self] = &[Self::Monochrome, Self::Color];

    fn key(self) -> &'static str {
        match self {
            Self::Monochrome => "monochrome",
            Self::Color => "color",
        }
    }

    fn label(self) -> SharedString {
        match self {
            Self::Monochrome => t!("settings.file_icons.monochrome"),
            Self::Color => t!("settings.file_icons.color"),
        }
    }
}

impl Choice for RightClick {
    const ALL: &'static [Self] = &[Self::Nothing, Self::Paste, Self::Menu];

    fn key(self) -> &'static str {
        match self {
            Self::Nothing => "nothing",
            Self::Paste => "paste",
            Self::Menu => "menu",
        }
    }

    fn label(self) -> SharedString {
        match self {
            Self::Nothing => t!("settings.right_click.nothing"),
            Self::Paste => t!("settings.right_click.paste"),
            Self::Menu => t!("settings.right_click.menu"),
        }
    }
}

impl AppSettings {
    /// The settings with every number in the range the app accepts. Applied
    /// to what the file holds and to every change, so neither a hand-edited
    /// file nor a typed value can give the terminal a size of zero.
    pub fn normalized(mut self) -> Self {
        self.terminal_font = self.terminal_font.normalized();
        self.send_delay = self.send_delay.normalized();
        self.session_log = self.session_log.normalized();
        // Whole pixels in range; what is not a number gives the default.
        let size = self.interface.font_size;
        self.interface.font_size = if size.is_finite() {
            size.clamp(
                *INTERFACE_FONT_SIZE_RANGE.start(),
                *INTERFACE_FONT_SIZE_RANGE.end(),
            )
            .round()
        } else {
            DEFAULT_INTERFACE_FONT_SIZE
        };
        self.transfers.parallel = self.transfers.parallel.clamp(
            *PARALLEL_TRANSFERS_RANGE.start(),
            *PARALLEL_TRANSFERS_RANGE.end(),
        );
        self.local_terminal.shell = self
            .local_terminal
            .shell
            .map(|shell| shell.trim().to_string())
            .filter(|shell| !shell.is_empty());
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
    fn label(self) -> SharedString;

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|choice| choice.key() == key)
    }
}

/// 界面语言, for ShellRS's text and gpui-kit's own. It changes the window
/// at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterfaceLanguage {
    // A settings file written before there was English says `zh-CN`, as
    // the default was then, and stays Chinese.
    #[default]
    #[serde(rename = "system")]
    System,
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

    fn label(self) -> SharedString {
        match self {
            Self::System => t!("settings.choice.system"),
            // A language is named in itself, so it can be found by someone
            // who cannot read the rest of the menu.
            Self::SimplifiedChinese => "简体中文".into(), // i18n: keep
            Self::English => "English".into(),
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

    /// `locale` with the operating system's preferred language.
    pub fn resolved(self) -> &'static str {
        self.locale(sys_locale::get_locale().as_deref())
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

    fn label(self) -> SharedString {
        match self {
            Self::Dark => t!("settings.appearance.dark"),
            Self::Light => t!("settings.appearance.light"),
            Self::System => t!("settings.choice.system"),
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
        check::<RightClick>();
        check::<FileIconStyle>();
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
        // The SFTP lists show color icons, as in WinSCP, unless asked not to.
        assert_eq!(AppSettings::default().file_icon_style, FileIconStyle::Color);
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
        // 文件 looks for and follows the terminal's folder unless told not to.
        assert!(AppSettings::default().file_sidebar.detect_directory_follow);
        let settings: AppSettings = serde_json::from_str(r#"{"file_sidebar":{}}"#).unwrap();
        assert!(settings.file_sidebar.detect_directory_follow);
        let settings: AppSettings =
            serde_json::from_str(r#"{"file_sidebar":{"detect_directory_follow":false}}"#).unwrap();
        assert!(!settings.file_sidebar.detect_directory_follow);
    }

    /// 侧栏功能: every tool but GPU/NPU until the user says otherwise, and
    /// only what differs from that in the file.
    #[test]
    fn the_sidebar_offers_its_usual_tools_until_told_otherwise() {
        let tools = AppSettings::default().sidebar_tools;
        assert!(tools.is_default());
        assert!(tools.shows(ToolKind::Monitor));
        assert!(!tools.shows(ToolKind::Gpu));

        let mut tools = SidebarTools::default();
        tools.set(ToolKind::Gpu, true);
        tools.set(ToolKind::Docker, false);
        assert!(!tools.is_default());
        assert_eq!(
            serde_json::to_value(&tools).unwrap(),
            serde_json::json!({"docker": false, "gpu": true})
        );
        // Back to its default is no change at all.
        tools.set(ToolKind::Docker, true);
        assert_eq!(
            serde_json::to_value(&tools).unwrap(),
            serde_json::json!({"gpu": true})
        );
        tools.reset();
        assert_eq!(tools, SidebarTools::default());

        // A newer version's tool is kept; a value that is no yes or no
        // costs only itself.
        let settings: AppSettings = serde_json::from_str(
            r#"{"sidebar_tools":{"gpu":true,"k8s":true,"docker":"no","history":false}}"#,
        )
        .unwrap();
        let tools = settings.sidebar_tools;
        assert!(tools.shows(ToolKind::Gpu));
        assert!(tools.shows(ToolKind::Docker));
        assert!(!tools.shows(ToolKind::History));
        assert_eq!(
            serde_json::to_value(&tools).unwrap(),
            serde_json::json!({"gpu": true, "history": false, "k8s": true})
        );
        let mut tools = tools;
        tools.reset();
        assert!(tools.is_default());
        assert_eq!(
            serde_json::to_value(&tools).unwrap(),
            serde_json::json!({"k8s": true}),
            "not this version's to reset"
        );
    }

    /// The sidebar beside local terminals: hidden until opened, by key in
    /// the file, and hidden again for a value it does not know.
    #[test]
    fn the_local_sidebar_keeps_its_tool_by_key() {
        assert_eq!(AppSettings::default().local_sidebar_tool, None);
        assert!(ToolKind::Projects.shown_by_default());
        let settings = AppSettings {
            local_sidebar_tool: Some(ToolKind::Projects),
            ..AppSettings::default()
        };
        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["local_sidebar_tool"], "projects");
        let read: AppSettings = serde_json::from_value(json).unwrap();
        assert_eq!(read, settings);
        for stray in [r#""notes""#, "3", "{}", "null"] {
            let read: AppSettings = serde_json::from_str(&format!(
                r#"{{"local_sidebar_tool":{stray},"sidebar_tools":{{"gpu":true}}}}"#
            ))
            .unwrap();
            assert_eq!(read.local_sidebar_tool, None, "{stray}");
            assert!(read.sidebar_tools.shows(ToolKind::Gpu));
        }
    }

    /// 保留终端内容 is on in a file from before it, as in a new one.
    #[test]
    fn terminal_output_is_kept_unless_turned_off() {
        assert!(AppSettings::default().local_terminal.keep_output);
        let read: AppSettings =
            serde_json::from_str(r#"{"local_terminal":{"shell":"/bin/zsh"}}"#).unwrap();
        assert!(read.local_terminal.keep_output);
        let read: AppSettings =
            serde_json::from_str(r#"{"local_terminal":{"keep_output":false}}"#).unwrap();
        assert!(!read.local_terminal.keep_output);
    }

    /// 界面字号: gpui-kit's 16 until changed, whole pixels within 12–20.
    #[test]
    fn the_interface_font_stays_as_it_was_until_changed() {
        assert_eq!(AppSettings::default().interface.font_size, 16.);
        let settings: AppSettings = serde_json::from_str(r#"{"interface":{}}"#).unwrap();
        assert_eq!(settings.interface.font_size, 16.);
        for (written, kept) in [
            (14., 14.),
            (14.6, 15.),
            (8., 12.),
            (30., 20.),
            (f32::NAN, 16.),
        ] {
            let settings = AppSettings {
                interface: InterfaceSettings { font_size: written },
                ..AppSettings::default()
            };
            assert_eq!(settings.normalized().interface.font_size, kept, "{written}");
        }
    }

    /// 最大同时传输数量: three unless the file says otherwise, and never
    /// beyond what the settings page offers.
    #[test]
    fn transfers_run_three_at_once_unless_the_file_says_otherwise() {
        assert_eq!(AppSettings::default().transfers.parallel, 3);
        let settings: AppSettings = serde_json::from_str(r#"{"transfers":{}}"#).unwrap();
        assert_eq!(settings.transfers.parallel, 3);
        let settings: AppSettings =
            serde_json::from_str(r#"{"transfers":{"parallel":6}}"#).unwrap();
        assert_eq!(settings.normalized().transfers.parallel, 6);
        for (written, kept) in [(0, 1), (50, 10)] {
            let settings = AppSettings {
                transfers: TransferSettings { parallel: written },
                ..AppSettings::default()
            };
            assert_eq!(settings.normalized().transfers.parallel, kept);
        }
    }

    #[test]
    fn local_terminals_run_the_default_shell_unless_the_file_names_one() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.local_terminal.shell, None);
        let settings: AppSettings =
            serde_json::from_str(r#"{"local_terminal":{"shell":" /bin/zsh -l "}}"#).unwrap();
        assert_eq!(
            settings.normalized().local_terminal.shell.as_deref(),
            Some("/bin/zsh -l")
        );
        let blank = AppSettings {
            local_terminal: LocalTerminalSettings {
                shell: Some("  ".into()),
                ..LocalTerminalSettings::default()
            },
            ..AppSettings::default()
        };
        assert_eq!(blank.normalized(), AppSettings::default());
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
        // Off, with the examples each on and quiet, ready for when it is.
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(!settings.terminal_highlight.enabled);
        assert_eq!(settings.terminal_highlight.rules, default_rules());
        assert!(
            default_rules()
                .iter()
                .all(|rule| rule.enabled && !rule.notify)
        );
        let settings: AppSettings =
            serde_json::from_str(r#"{"terminal_highlight":{"enabled":true,"rules":[]}}"#).unwrap();
        assert!(settings.terminal_highlight.enabled);
        assert!(settings.terminal_highlight.rules.is_empty());
        let json = serde_json::to_value(AppSettings::default()).unwrap();
        assert_eq!(
            json["terminal_highlight"]["rules"][0],
            serde_json::json!({
                "enabled": true,
                "pattern": "ERROR",
                "note": "错误",
                "color": "#e5484d",
                "notify": false
            })
        );
    }

    #[test]
    fn a_rule_this_version_cannot_read_costs_only_itself() {
        let settings: AppSettings = serde_json::from_str(
            r##"{
                "appearance": "dark",
                "terminal_highlight": {"rules": [
                    {"pattern": "FATAL", "color": "orange"},
                    {"pattern": "OOM", "color": "#D946EF", "notify": true},
                    {"pattern": ""},
                    7
                ]}
            }"##,
        )
        .unwrap();
        assert_eq!(settings.appearance, Appearance::Dark);
        let rules = &settings.terminal_highlight.rules;
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].pattern, "OOM");
        assert_eq!(rules[0].color.to_hex(), "#d946ef");
        assert!(rules[0].enabled && rules[0].notify);
        // A row added and not filled in yet stays, to be filled in.
        assert_eq!(rules[1], HighlightRule::default());
    }

    #[test]
    fn terminal_themes_fall_back_to_the_default_for_their_appearance() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        let themes = &settings.terminal_theme;
        assert_eq!(themes.light, "shellrs-light");
        assert_eq!(themes.dark, "shellrs-dark");
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            assert_eq!(themes.theme(mode), TerminalTheme::default_for(mode));
        }

        // A key from a newer version, and a dark theme where a light one
        // belongs, each give way on their own side only.
        let settings: AppSettings = serde_json::from_str(
            r#"{"appearance":"dark","terminal_theme":{"light":"shellrs-dark","dark":"sepia"}}"#,
        )
        .unwrap();
        assert_eq!(settings.appearance, Appearance::Dark);
        let themes = &settings.terminal_theme;
        assert_eq!(
            themes.theme(ThemeMode::Light),
            TerminalTheme::default_for(ThemeMode::Light)
        );
        assert_eq!(
            themes.theme(ThemeMode::Dark),
            TerminalTheme::default_for(ThemeMode::Dark)
        );

        // Choosing a theme sets the side it is made for, by key.
        let mut themes = TerminalThemeSettings::default();
        let dark = TerminalTheme::for_mode(ThemeMode::Dark).last().unwrap();
        themes.choose(dark);
        assert_eq!(themes.theme(ThemeMode::Dark), dark);
        assert_eq!(
            themes.theme(ThemeMode::Light),
            TerminalTheme::default_for(ThemeMode::Light)
        );
        assert_eq!(
            serde_json::to_value(&themes).unwrap(),
            serde_json::json!({"light": "shellrs-light", "dark": dark.key(), "app_follows": false})
        );

        // The app stays neutral, the default's colors, gpui-kit's own,
        // unless told to follow; the terminals have the theme either way.
        assert_eq!(
            themes.app_theme(ThemeMode::Dark),
            TerminalTheme::default_for(ThemeMode::Dark)
        );
        let settings: AppSettings =
            serde_json::from_str(r#"{"terminal_theme":{"dark":"nord","app_follows":true}}"#)
                .unwrap();
        let themes = &settings.terminal_theme;
        assert_eq!(themes.theme(ThemeMode::Dark).key(), "nord");
        assert_eq!(themes.app_theme(ThemeMode::Dark).key(), "nord");
        assert!(!themes.is_default_choice());
    }

    #[test]
    fn the_window_is_remembered_unless_the_file_says_otherwise() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.window.remember_size && settings.window.remember_position);
        let settings: AppSettings =
            serde_json::from_str(r#"{"window":{"remember_position":false}}"#).unwrap();
        assert!(settings.window.remember_size && !settings.window.remember_position);
    }

    #[test]
    fn terminals_show_the_menu_and_copy_only_on_command_unless_the_file_says_otherwise() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(!settings.terminal_interaction.copy_on_select);
        assert_eq!(settings.terminal_interaction.right_click, RightClick::Menu);
        let settings: AppSettings =
            serde_json::from_str(r#"{"terminal_interaction":{"right_click":"paste"}}"#).unwrap();
        assert!(!settings.terminal_interaction.copy_on_select);
        assert_eq!(settings.terminal_interaction.right_click, RightClick::Paste);
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
    fn usage_statistics_are_sent_unless_the_file_says_otherwise() {
        let settings: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(settings.analytics.enabled);
        let settings: AppSettings = serde_json::from_str(r#"{"analytics":{}}"#).unwrap();
        assert!(settings.analytics.enabled);
        let settings: AppSettings =
            serde_json::from_str(r#"{"analytics":{"enabled":false}}"#).unwrap();
        assert!(!settings.analytics.enabled);
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
