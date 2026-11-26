//! AI 助手: the assistant core (`shellrs-assistant`) as the app uses it,
//! and what is the app's own: the thread its conversations run on, the
//! right sidebar's panel, the model service dialog, running its commands on
//! a terminal's connection or in the terminal, and the words for all of it.

mod describe;
mod models_menu;
mod panel;
mod presets;
mod provider_dialog;
mod run;
mod service;

pub use describe::{draft_error, provider_error};
pub use models_menu::{default_model, model_groups, model_items};
pub use panel::{AssistantPanel, AssistantPanelEvent};
pub use provider_dialog::{ProviderDialog, open_provider_dialog, protocol_label};
pub use service::{AssistantService, AssistantServices};
pub use shellrs_assistant::{
    Approval, ConversationSummary, Protocol, ProviderError, ProviderErrorKind, RunIn, Store,
    Transport, TurnEnd,
};

use crate::host::AiProtocol;

/// A saved service's protocol as the core names it.
pub fn core_protocol(protocol: AiProtocol) -> Protocol {
    match protocol {
        AiProtocol::Anthropic => Protocol::Anthropic,
        AiProtocol::OpenAi => Protocol::OpenAi,
        AiProtocol::OpenAiResponses => Protocol::OpenAiResponses,
    }
}
