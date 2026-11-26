//! A conversation as the assistant keeps it, whatever the service: parts
//! any protocol can be built from, and beside them what the service sent,
//! to give back to that same model as it was.

use serde::{Deserialize, Serialize};

/// Who a message is from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// What the tools answered to the assistant's last calls.
    ToolResults,
}

impl Role {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::ToolResults => "tool_results",
        }
    }

    pub(crate) fn from_key(key: &str) -> Option<Self> {
        [Self::User, Self::Assistant, Self::ToolResults]
            .into_iter()
            .find(|role| role.key() == key)
    }
}

/// How a tool's call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Ok,
    Error,
    /// The user said no.
    Denied,
    /// The turn was stopped before the tool answered.
    Cancelled,
}

/// Where a command ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunIn {
    /// On a channel of its own beside the terminal's shell.
    Background,
    /// Typed into the terminal's shell.
    Terminal,
}

impl RunIn {
    pub const ALL: [Self; 2] = [Self::Background, Self::Terminal];

    /// How it is written in settings and in a conversation. Never changes.
    pub fn key(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Terminal => "terminal",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|run_in| run_in.key() == key)
    }
}

/// What is known about the host the terminal is on, sent to the model when
/// it changes. Never the address or the host's name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostContext {
    /// The system, as detected: `linux`, `macos`, `windows`…
    pub os: Option<String>,
    pub user: Option<String>,
    /// The folder the shell is in, as it reported.
    pub cwd: Option<String>,
    /// The connection carries the shell only: commands run in the terminal.
    pub shell_only: bool,
    /// The shell started again since the last message (a reconnect).
    pub restarted: bool,
}

/// A notice the conversation holds for the user, not sent to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// The model declined to go on.
    Refused,
    /// The turn was stopped by the user.
    Stopped,
    /// Older tool output was taken out to save room.
    Trimmed,
}

/// One part of a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// The model's reasoning, as far as the service shows it: shown, never
    /// sent to another model.
    Thinking {
        text: String,
    },
    ToolCall {
        /// The assistant's own id, `c<seq>_<n>`: valid in every protocol.
        id: String,
        /// The service's id for it, which its own content uses.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        status: ResultStatus,
        /// What the model was sent, already cut to size.
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit: Option<i64>,
        /// How long the output was before it was cut.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_bytes: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<RunIn>,
    },
    Context {
        context: HostContext,
    },
    Notice {
        kind: NoticeKind,
    },
    /// A part a newer ShellRS wrote: kept as it was, used by nobody here.
    #[serde(skip)]
    Unknown(serde_json::Value),
}

impl Part {
    /// Read a part stored as JSON, keeping one of a kind unknown here.
    pub(crate) fn from_stored(value: serde_json::Value) -> Self {
        serde_json::from_value(value.clone()).unwrap_or(Self::Unknown(value))
    }

    pub(crate) fn to_stored(&self) -> serde_json::Value {
        match self {
            Self::Unknown(value) => value.clone(),
            part => serde_json::to_value(part).unwrap_or_default(),
        }
    }
}

/// How many tokens a request took, as the service counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input not read from the cache.
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    /// All the input the request held: the conversation's size then.
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }

    pub fn total(&self) -> u64 {
        self.context() + self.output
    }

    pub fn add(&mut self, other: &Usage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

/// Why the model stopped writing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Refusal,
    /// The service paused a long turn; asking again goes on with it.
    PauseTurn,
    /// Anything else a service said, taken as the end of the turn.
    Other,
}

impl StopReason {
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::ToolUse => "tool_use",
            Self::MaxTokens => "max_tokens",
            Self::Refusal => "refusal",
            Self::PauseTurn => "pause_turn",
            Self::Other => "other",
        }
    }

    pub(crate) fn from_key(key: &str) -> Self {
        [
            Self::EndTurn,
            Self::ToolUse,
            Self::MaxTokens,
            Self::Refusal,
            Self::PauseTurn,
        ]
        .into_iter()
        .find(|stop| stop.key() == key)
        .unwrap_or(Self::Other)
    }
}

/// What the service sent, kept to give back to the same model as it was:
/// Anthropic's content with its reasoning signed, OpenAI's reasoning and
/// tool calls.
#[derive(Clone, Debug, PartialEq)]
pub struct Raw {
    /// The protocol, service and model it came from
    /// (`ProviderConfig::source`).
    pub source: String,
    pub content: serde_json::Value,
}

/// One message of a conversation.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// Its place in the conversation, from 0.
    pub seq: i64,
    pub role: Role,
    pub parts: Vec<Part>,
    pub raw: Option<Raw>,
    pub stop: Option<StopReason>,
    /// For a message of the assistant's, what the request for it took.
    pub usage: Option<Usage>,
    pub created_ms: i64,
}

impl Message {
    /// The text of its text parts, joined.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &serde_json::Value)> {
        self.parts.iter().filter_map(|part| match part {
            Part::ToolCall {
                id, name, input, ..
            } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_of_a_newer_kind_is_kept_as_it_was() {
        let value = serde_json::json!({"type": "image", "url": "data:…"});
        let part = Part::from_stored(value.clone());
        assert_eq!(part, Part::Unknown(value.clone()));
        assert_eq!(part.to_stored(), value);
    }

    #[test]
    fn parts_read_back_as_they_were_written() {
        let parts = [
            Part::Text { text: "hi".into() },
            Part::ToolCall {
                id: "c1_0".into(),
                provider_id: Some("toolu_1".into()),
                name: "run_command".into(),
                input: serde_json::json!({"command": "uptime"}),
            },
            Part::ToolResult {
                call_id: "c1_0".into(),
                status: ResultStatus::Ok,
                output: "up 3 days".into(),
                exit: Some(0),
                total_bytes: Some(9),
                mode: Some(RunIn::Background),
            },
            Part::Context {
                context: HostContext {
                    os: Some("linux".into()),
                    ..HostContext::default()
                },
            },
            Part::Notice {
                kind: NoticeKind::Stopped,
            },
        ];
        for part in parts {
            assert_eq!(Part::from_stored(part.to_stored()), part);
        }
    }

    #[test]
    fn stop_reasons_read_back() {
        for stop in [
            StopReason::EndTurn,
            StopReason::ToolUse,
            StopReason::MaxTokens,
            StopReason::Refusal,
            StopReason::PauseTurn,
        ] {
            assert_eq!(StopReason::from_key(stop.key()), stop);
        }
        assert_eq!(StopReason::from_key("stop_sequence"), StopReason::Other);
    }
}
