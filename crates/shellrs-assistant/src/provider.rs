//! What the protocols share: a request built from the conversation, and a
//! stream read back into the same deltas.

use crate::anthropic;
use crate::config::{Protocol, ProviderConfig};
use crate::error::ProviderError;
use crate::http::HttpRequest;
use crate::message::{HostContext, Message, StopReason, Usage};
use crate::openai;
use crate::responses;
use crate::sse::SseEvent;
use crate::tools::ToolSpec;

/// What a request is built from.
pub(crate) struct Request<'a> {
    pub config: &'a ProviderConfig,
    pub system: &'a str,
    pub tools: &'a [ToolSpec],
    pub messages: &'a [Message],
    /// Tool output in messages before this one is sent as removed.
    pub elided_before: Option<i64>,
}

pub(crate) fn build(request: &Request<'_>) -> HttpRequest {
    match request.config.protocol {
        Protocol::Anthropic => anthropic::build(request),
        Protocol::OpenAi => openai::build(request),
        Protocol::OpenAiResponses => responses::build(request),
    }
}

/// Anthropic's API version, which every request of its protocol names.
pub(crate) const ANTHROPIC_VERSION: &str = "2023-06-01";

/// The headers that say who asks: the key as the protocol takes it, and
/// Anthropic's version.
pub(crate) fn auth_headers(config: &ProviderConfig) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if config.protocol == Protocol::Anthropic {
        headers.push(("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()));
    }
    let Some(key) = &config.api_key else {
        return headers;
    };
    let bearer = (
        "authorization".to_owned(),
        format!("Bearer {}", key.expose()),
    );
    match config.protocol {
        Protocol::Anthropic => {
            headers.push(("x-api-key".to_owned(), key.expose().to_owned()));
            // Services that speak the protocol often take the key as a
            // bearer token; Anthropic itself takes it as `x-api-key` only.
            if !config.is_anthropic() {
                headers.push(bearer);
            }
        }
        Protocol::OpenAi | Protocol::OpenAiResponses => headers.push(bearer),
    }
    headers
}

/// One piece of a streamed answer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Delta {
    Text(String),
    Thinking(String),
    /// A tool call starts, at `index` among the answer's calls.
    ToolStart {
        index: usize,
        id: String,
        name: String,
    },
    /// More of a call's input, as JSON text.
    ToolInput {
        index: usize,
        json: String,
    },
    /// What the request took so far.
    Usage(Usage),
    Stop(StopReason),
}

/// Reads a protocol's events into deltas.
pub(crate) trait Decoder: Send {
    fn event(&mut self, event: &SseEvent) -> Result<Vec<Delta>, ProviderError>;
    /// The stream said it is over.
    fn done(&self) -> bool;
    /// What to give back to the same model, kept beside the message.
    fn raw(&self) -> Option<serde_json::Value>;
}

pub(crate) fn decoder(protocol: Protocol) -> Box<dyn Decoder> {
    match protocol {
        Protocol::Anthropic => Box::new(anthropic::AnthropicDecoder::default()),
        Protocol::OpenAi => Box::new(openai::OpenAiDecoder::default()),
        Protocol::OpenAiResponses => Box::new(responses::ResponsesDecoder::default()),
    }
}

/// The context block ShellRS puts before a message: what is known of the
/// host, in a form the system prompt describes.
pub(crate) fn context_text(context: &HostContext) -> String {
    let mut text = String::from("<context>\n");
    if let Some(os) = &context.os {
        text.push_str(&format!("system: {os}\n"));
    }
    if let Some(user) = &context.user {
        text.push_str(&format!("user: {user}\n"));
    }
    if let Some(cwd) = &context.cwd {
        text.push_str(&format!("folder: {cwd}\n"));
    }
    if context.shell_only {
        text.push_str(
            "connection: carries the terminal only; commands run in the user's terminal\n",
        );
    }
    if context.restarted {
        text.push_str("the terminal reconnected since the last message: its shell started again\n");
    }
    text.push_str("</context>");
    text
}

/// The id a tool call goes by in a request: the service's own when its
/// content is sent back as it gave it, else the assistant's.
pub(crate) fn call_id<'a>(id: &'a str, provider_id: Option<&'a str>, replayed: bool) -> &'a str {
    match provider_id {
        Some(provider_id) if replayed => provider_id,
        _ => id,
    }
}
