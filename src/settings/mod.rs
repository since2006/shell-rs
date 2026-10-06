//! 设置: the settings tab, the settings it edits and how they are applied.

mod apply;
mod highlight_rules;
mod model;
mod settings_panel;
mod store;
mod terminal_themes;

pub use apply::apply;
pub use model::{
    AppSettings, Appearance, Choice, ExternalCliSettings, InterfaceLanguage, NotificationSettings,
    TerminalFontSettings, TerminalHighlightSettings, TerminalThemeSettings, UpdateSettings,
};
pub use settings_panel::{SettingsPanel, SettingsPanelEvent};
pub use store::{SettingsStore, SettingsStoreEvent};
