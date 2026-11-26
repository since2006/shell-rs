//! The ShellRS AI assistant's core, shared by the desktop app and the
//! mobile apps: the wire protocols model services speak (Anthropic's
//! Messages, OpenAI's Chat Completions and Responses), the loop of a
//! conversation, the rules of its tools (what is asked about, what is cut,
//! what is taken out), and the conversations' own database.
//!
//! It has no words: failures and events are typed, and the apps word them.
//! It reads no keychain and knows no saved host: the app gives it the
//! model service, its key and what the terminal can do, and runs the
//! tools itself.

mod anthropic;
mod config;
mod danger;
mod env;
mod error;
mod http;
mod message;
mod openai;
mod prompt;
mod provider;
mod redact;
mod remember;
mod responses;
mod session;
#[cfg(test)]
mod session_tests;
mod sse;
mod store;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod tools;

pub use config::{ApiKey, Protocol, ProviderConfig, endpoint_url};
pub use danger::{command_kinds, is_dangerous};
pub use env::{Env, FixedEnv, SharedEnv, SystemEnv};
pub use error::{ProviderError, ProviderErrorKind, StoreError};
pub use http::{HttpRequest, HttpResponse, ReqwestTransport, Transport, TransportError};
pub use message::{
    HostContext, Message, NoticeKind, Part, ResultStatus, Role, RunIn, StopReason, Usage,
};
pub use redact::redact;
pub use remember::Remembered;
pub use session::{
    Answerer, Approval, ConversationChoice, NewConversation, Session, SessionEvent, SessionFailure,
    SessionSetup, TurnEnd, check_provider, list_models,
};
pub use store::{ConversationSummary, Store};
pub use tools::{
    Capabilities, CommandAnswer, CommandEnd, DEFAULT_TIMEOUT, FILE_LIMIT, MOST_TIMEOUT,
    OUTPUT_HEAD, OUTPUT_TAIL, Output, RanAs, ToolAnswer, ToolCall,
};
