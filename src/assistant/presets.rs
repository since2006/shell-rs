//! 预设 in the model service dialog: the core's (`shellrs_assistant::Preset`)
//! with their names, and as the dialog's select lists them.

use gpui_kit::SharedString;
use gpui_kit::component::searchable_list::SearchableListItem;
pub use shellrs_assistant::Preset;

use crate::host::AiProtocol;
use crate::i18n::t;

/// A preset's name.
pub trait PresetLabel {
    fn label(self) -> SharedString;
}

impl PresetLabel for Preset {
    /// Product names are the same in every language.
    fn label(self) -> SharedString {
        match self {
            Self::Anthropic => "Anthropic".into(),
            Self::OpenAi => "OpenAI".into(),
            Self::DeepSeek => "DeepSeek".into(),
            Self::Qwen => t!("assistant.preset.qwen"),
            Self::Kimi => "Kimi".into(),
            Self::Glm => t!("assistant.preset.glm"),
            Self::Doubao => t!("assistant.preset.doubao"),
            Self::SiliconFlow => t!("assistant.preset.siliconflow"),
            Self::OpenRouter => "OpenRouter".into(),
            Self::Ollama => "Ollama".into(),
            Self::LmStudio => "LM Studio".into(),
            Self::Custom => t!("assistant.preset.custom"),
        }
    }
}

/// The API type, as a saved service names it, and address a preset fills
/// in; `None` for 自定义.
pub fn preset_service(preset: Preset) -> Option<(AiProtocol, &'static str)> {
    preset
        .service()
        .map(|(protocol, address)| (super::host_protocol(protocol), address))
}

/// A preset as the dialog's select lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresetItem(pub Preset);

impl SearchableListItem for PresetItem {
    type Value = Preset;

    fn title(&self) -> SharedString {
        self.0.label()
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}
