//! AI 助手 for the apps: the model services, kept and synced with the hosts
//! as on the desktop, and conversations, saved in `assistant.db` beside the
//! hosts' database and run on the core's runtime.
//!
//! A conversation is attached to a [`Connection`]. When the model calls a
//! tool, the app hears of it ([`AssistantEvent::ToolCall`]) and, asking the
//! user first when it says so, answers: the terminal's text it reads itself
//! (the screen is the app's), and a command or a file it has the session
//! run on the connection ([`AssistantSession::run_call`]), beside the shell
//! on a channel of its own, as the desktop's 后台执行 does. Typing a command
//! into the terminal is the desktop's alone for now.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::BoxFuture;
use shellrs_assistant as core;
use shellrs_assistant::{
    ApiKey, Capabilities, CommandAnswer, CommandEnd, ConversationChoice, NewConversation, Output,
    ProviderConfig, ReqwestTransport, Session, SessionEvent, SessionSetup, Store, SystemEnv,
    ToolAnswer, ToolCall, Transport,
};
use shellrs_hosts as hosts;
use shellrs_ssh::{self as ssh, SshHandle};
use tokio::sync::oneshot;

use crate::connection::Connection;
use crate::core::Core;
use crate::store::{HostStore, StoreError};
use crate::task;

/// How long the start of an exec or SFTP channel may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// The wire protocol a model service speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AiProtocol {
    /// Anthropic's Messages API: requests go to `/v1/messages`.
    Anthropic,
    /// OpenAI's Chat Completions API: `/v1/chat/completions`.
    OpenAi,
    /// OpenAI's Responses API: `/v1/responses`.
    OpenAiResponses,
}

impl From<hosts::AiProtocol> for AiProtocol {
    fn from(protocol: hosts::AiProtocol) -> Self {
        match protocol {
            hosts::AiProtocol::Anthropic => Self::Anthropic,
            hosts::AiProtocol::OpenAi => Self::OpenAi,
            hosts::AiProtocol::OpenAiResponses => Self::OpenAiResponses,
        }
    }
}

impl From<AiProtocol> for hosts::AiProtocol {
    fn from(protocol: AiProtocol) -> Self {
        match protocol {
            AiProtocol::Anthropic => Self::Anthropic,
            AiProtocol::OpenAi => Self::OpenAi,
            AiProtocol::OpenAiResponses => Self::OpenAiResponses,
        }
    }
}

impl From<AiProtocol> for core::Protocol {
    fn from(protocol: AiProtocol) -> Self {
        match protocol {
            AiProtocol::Anthropic => Self::Anthropic,
            AiProtocol::OpenAi => Self::OpenAi,
            AiProtocol::OpenAiResponses => Self::OpenAiResponses,
        }
    }
}

/// Where a service's requests go: `base_url` followed by the protocol's
/// path, without its `/v1` when the address already names a version
/// (`…/v1`, `…/api/paas/v4`). For a form to show under the address.
#[uniffi::export]
pub fn ai_endpoint(protocol: AiProtocol, base_url: String) -> String {
    core::endpoint_url(&base_url, core::Protocol::from(protocol).path())
}

/// A saved model service. Its key is in the keychain, never here.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AiProvider {
    pub public_id: String,
    pub name: String,
    pub protocol: AiProtocol,
    /// What the protocol's path goes after ([`ai_endpoint`]).
    pub base_url: String,
    /// Never empty, in the user's order: the first is used when a
    /// conversation names none.
    pub models: Vec<String>,
}

impl From<&hosts::AiProvider> for AiProvider {
    fn from(provider: &hosts::AiProvider) -> Self {
        Self {
            public_id: provider.public_id.to_string(),
            name: provider.name.to_string(),
            protocol: provider.protocol.into(),
            base_url: provider.base_url.to_string(),
            models: provider.models.iter().map(ToString::to_string).collect(),
        }
    }
}

/// A model service as its form commits it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AiProviderDraft {
    pub name: String,
    pub protocol: AiProtocol,
    pub base_url: String,
    /// Trimmed, each once, empty ones left out; at least one.
    pub models: Vec<String>,
}

/// What stands in the way of saving a model service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AiProviderProblem {
    Name,
    /// Missing, or not `http://` or `https://`.
    BaseUrl,
    /// No model is named.
    Model,
}

impl From<hosts::AiProviderDraftError> for AiProviderProblem {
    fn from(error: hosts::AiProviderDraftError) -> Self {
        match error {
            hosts::AiProviderDraftError::Name => Self::Name,
            hosts::AiProviderDraftError::BaseUrl => Self::BaseUrl,
            hosts::AiProviderDraftError::Model => Self::Model,
        }
    }
}

#[uniffi::export]
impl HostStore {
    /// The model services, in list order.
    pub fn ai_providers(&self) -> Vec<AiProvider> {
        self.shared
            .lock()
            .ai_providers()
            .iter()
            .map(AiProvider::from)
            .collect()
    }

    /// Save a model service, new without `public_id`, with `api_key` when
    /// given (an empty one keeps the key there is); waits for the keychain.
    /// Its public id.
    pub async fn save_ai_provider(
        &self,
        public_id: Option<String>,
        draft: AiProviderDraft,
        api_key: Option<String>,
    ) -> Result<String, StoreError> {
        let shared = self.shared.clone();
        task::run(&self.shared.runtime, async move {
            let (public_id, jobs) = {
                let mut data = shared.lock();
                if shared.is_closed() {
                    return Err(StoreError::Closed);
                }
                let checked = hosts::AiProviderDraft::new(
                    draft.name,
                    draft.protocol.into(),
                    draft.base_url,
                    draft.models,
                )
                .validated()
                .map_err(|error| StoreError::AiProvider {
                    problem: error.into(),
                })?;
                let id = match public_id {
                    Some(public_id) => {
                        let id = data
                            .ai_provider_by_public_id(&public_id)
                            .ok_or(StoreError::NotFound)?
                            .id;
                        data.update_ai_provider(id, checked);
                        id
                    }
                    None => data.insert_ai_provider(checked),
                };
                let provider = data.ai_provider(id).cloned().ok_or(StoreError::NotFound)?;
                if let Some(key) = api_key.filter(|key| !key.trim().is_empty()) {
                    data.save_secret(provider.key_secret(), Some(key.trim().to_owned()));
                }
                let jobs = data.take_secret_jobs();
                shared.settle(&mut data);
                (provider.public_id.to_string(), jobs)
            };
            shared.run_secret_jobs(jobs).await;
            Ok(public_id)
        })
        .await
    }

    /// Delete a model service and its key.
    pub fn delete_ai_provider(&self, public_id: String) -> Result<(), StoreError> {
        self.shared.with_data(|data| {
            let id = data
                .ai_provider_by_public_id(&public_id)
                .ok_or(StoreError::NotFound)?
                .id;
            data.remove_ai_provider(id);
            Ok(())
        })?
    }
}

/// How the user lets the assistant run commands and read files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Approval {
    /// Every call is asked about.
    Ask,
    /// Only those marked dangerous are asked about.
    ApproveForMe,
    /// Nothing is asked about.
    FullAccess,
}

impl From<Approval> for core::Approval {
    fn from(approval: Approval) -> Self {
        match approval {
            Approval::Ask => Self::Ask,
            Approval::ApproveForMe => Self::ApproveForMe,
            Approval::FullAccess => Self::FullAccess,
        }
    }
}

impl From<core::Approval> for Approval {
    fn from(approval: core::Approval) -> Self {
        match approval {
            core::Approval::Ask => Self::Ask,
            core::Approval::ApproveForMe => Self::ApproveForMe,
            core::Approval::FullAccess => Self::FullAccess,
        }
    }
}

/// What a conversation starts with.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AssistantStart {
    pub provider_public_id: String,
    /// Which of the service's models; its first when `None` or one it no
    /// longer lists.
    pub model: Option<String>,
    /// The saved host it is about; `None` for one not saved.
    pub host_public_id: Option<String>,
    /// The host's name or address, kept with the conversation.
    pub host_label: String,
    /// A saved conversation to go on with; `None` for a new one.
    pub conversation: Option<String>,
    pub approval: Approval,
    /// The language to answer in, by its English name: `English`,
    /// `Simplified Chinese`.
    pub language: String,
}

/// What is known of the host, sent with a message when it changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct AssistantContext {
    pub os: Option<String>,
    pub user: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum MessageRole {
    User,
    Assistant,
    ToolResults,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CallStatus {
    Ok,
    Error,
    Denied,
    Cancelled,
}

impl From<core::ResultStatus> for CallStatus {
    fn from(status: core::ResultStatus) -> Self {
        match status {
            core::ResultStatus::Ok => Self::Ok,
            core::ResultStatus::Error => Self::Error,
            core::ResultStatus::Denied => Self::Denied,
            core::ResultStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AssistantToolCall {
    pub id: String,
    pub name: String,
    /// What the model passed, as JSON.
    pub input: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AssistantToolResult {
    pub call_id: String,
    pub status: CallStatus,
    /// What the model was sent.
    pub output: String,
}

/// A message of a conversation, as the app shows it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct AssistantMessage {
    pub seq: i64,
    pub role: MessageRole,
    pub text: String,
    pub calls: Vec<AssistantToolCall>,
    pub results: Vec<AssistantToolResult>,
}

impl From<&core::Message> for AssistantMessage {
    fn from(message: &core::Message) -> Self {
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for part in &message.parts {
            match part {
                core::Part::ToolCall {
                    id, name, input, ..
                } => calls.push(AssistantToolCall {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.to_string(),
                }),
                core::Part::ToolResult {
                    call_id,
                    status,
                    output,
                    ..
                } => results.push(AssistantToolResult {
                    call_id: call_id.clone(),
                    status: (*status).into(),
                    output: output.clone(),
                }),
                _ => {}
            }
        }
        Self {
            seq: message.seq,
            role: match message.role {
                core::Role::User => MessageRole::User,
                core::Role::Assistant => MessageRole::Assistant,
                core::Role::ToolResults => MessageRole::ToolResults,
            },
            text: message.text(),
            calls,
            results,
        }
    }
}

/// What the model called for.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AssistantCall {
    /// The app answers with the terminal's last `lines` lines:
    /// [`AssistantSession::answer_terminal`].
    ReadTerminal {
        lines: u32,
    },
    RunCommand {
        command: String,
        cwd: Option<String>,
        timeout_seconds: u64,
    },
    ReadFile {
        path: String,
    },
}

impl From<&ToolCall> for AssistantCall {
    fn from(call: &ToolCall) -> Self {
        match call {
            ToolCall::ReadTerminal { lines } => Self::ReadTerminal {
                lines: *lines as u32,
            },
            ToolCall::RunCommand {
                command,
                cwd,
                timeout,
            } => Self::RunCommand {
                command: command.clone(),
                cwd: cwd.clone(),
                timeout_seconds: timeout.as_secs(),
            },
            ToolCall::ReadFile { path } => Self::ReadFile { path: path.clone() },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TurnEnd {
    Done,
    Refused,
    Stopped,
    StepLimit,
    MaxTokens,
    ContextFull,
}

impl From<core::TurnEnd> for TurnEnd {
    fn from(end: core::TurnEnd) -> Self {
        match end {
            core::TurnEnd::Done => Self::Done,
            core::TurnEnd::Refused => Self::Refused,
            core::TurnEnd::Stopped => Self::Stopped,
            core::TurnEnd::StepLimit => Self::StepLimit,
            core::TurnEnd::MaxTokens => Self::MaxTokens,
            core::TurnEnd::ContextFull => Self::ContextFull,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ProviderErrorKind {
    KeyRejected,
    NotFound,
    QuotaExhausted,
    RateLimited,
    Overloaded,
    ContextTooLong,
    BadRequest,
    ServerError,
    Network,
    Timeout,
    Protocol,
}

impl From<core::ProviderErrorKind> for ProviderErrorKind {
    fn from(kind: core::ProviderErrorKind) -> Self {
        match kind {
            core::ProviderErrorKind::KeyRejected => Self::KeyRejected,
            core::ProviderErrorKind::NotFound => Self::NotFound,
            core::ProviderErrorKind::QuotaExhausted => Self::QuotaExhausted,
            core::ProviderErrorKind::RateLimited => Self::RateLimited,
            core::ProviderErrorKind::Overloaded => Self::Overloaded,
            core::ProviderErrorKind::ContextTooLong => Self::ContextTooLong,
            core::ProviderErrorKind::BadRequest => Self::BadRequest,
            core::ProviderErrorKind::ServerError => Self::ServerError,
            core::ProviderErrorKind::Network => Self::Network,
            core::ProviderErrorKind::Timeout => Self::Timeout,
            core::ProviderErrorKind::Protocol => Self::Protocol,
        }
    }
}

/// Why a conversation failed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AssistantFailure {
    /// The model service failed; `service_message` is what it said.
    Provider {
        kind: ProviderErrorKind,
        status: Option<u16>,
        service_message: Option<String>,
    },
    /// The conversations' database failed, in English.
    Store { detail: String },
    /// The conversation asked for is not saved.
    NotFound,
}

impl From<core::SessionFailure> for AssistantFailure {
    fn from(failure: core::SessionFailure) -> Self {
        match failure {
            core::SessionFailure::Provider(error) => Self::Provider {
                kind: error.kind.into(),
                status: error.status,
                service_message: error.message,
            },
            core::SessionFailure::Store(detail) => Self::Store { detail },
            core::SessionFailure::NotFound => Self::NotFound,
        }
    }
}

/// What happens in a conversation.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AssistantEvent {
    Opened {
        public_id: String,
        title: String,
        messages: Vec<AssistantMessage>,
    },
    MessageAdded {
        message: AssistantMessage,
    },
    /// A request goes out: what streamed of an earlier try is void.
    RequestStarted,
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolCall {
        id: String,
        call: AssistantCall,
        needs_approval: bool,
        dangerous: bool,
    },
    ToolFinished {
        id: String,
        status: CallStatus,
    },
    Retrying {
        attempt: u32,
        of: u32,
        after_ms: u64,
    },
    /// What the conversation's requests took, and how much of the model's
    /// room the last one filled.
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        context: u64,
        window: u64,
    },
    Trimmed,
    ApprovalChanged {
        approval: Approval,
    },
    TurnEnded {
        end: TurnEnd,
    },
    Failed {
        failure: AssistantFailure,
    },
}

/// A saved conversation.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ConversationInfo {
    pub public_id: String,
    pub host_public_id: Option<String>,
    pub host_label: String,
    pub title: String,
    pub model: String,
    pub updated_ms: i64,
}

impl From<core::ConversationSummary> for ConversationInfo {
    fn from(summary: core::ConversationSummary) -> Self {
        Self {
            public_id: summary.public_id,
            host_public_id: summary.host_public_id,
            host_label: summary.host_label,
            title: summary.title,
            model: summary.model,
            updated_ms: summary.updated_ms,
        }
    }
}

/// Why a conversation could not start, or the conversations be read.
#[derive(Debug, uniffi::Error)]
pub enum AssistantError {
    /// No model service has that public id.
    NoProvider,
    /// The connection is not logged in.
    NotConnected,
    /// The conversations' database or the HTTP client could not be had, in
    /// English.
    Unavailable { detail: String },
    /// The model service failed; `service_message` is what it said.
    Provider {
        kind: ProviderErrorKind,
        status: Option<u16>,
        service_message: Option<String>,
    },
    /// The core is gone.
    Closed,
    /// What should never happen: a bug in ShellRS.
    Internal { detail: String },
}

impl From<core::ProviderError> for AssistantError {
    fn from(error: core::ProviderError) -> Self {
        Self::Provider {
            kind: error.kind.into(),
            status: error.status,
            service_message: error.message,
        }
    }
}

impl std::fmt::Display for AssistantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for AssistantError {}

impl task::CallError for AssistantError {
    fn closed() -> Self {
        Self::Closed
    }

    fn internal(detail: String) -> Self {
        Self::Internal { detail }
    }
}

impl Core {
    /// A service as a form or a saved record describes it, with `api_key`
    /// when one is typed, else the key saved for `public_id`, read off the
    /// caller's thread.
    async fn provider_config(
        &self,
        protocol: AiProtocol,
        base_url: String,
        model: String,
        api_key: Option<String>,
        public_id: Option<String>,
    ) -> ProviderConfig {
        let typed = api_key
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty());
        let key = match typed {
            Some(key) => Some(key),
            None => {
                let saved = public_id.and_then(|id| {
                    self.store
                        .lock()
                        .ai_provider_by_public_id(&id)
                        .map(|provider| provider.key_secret())
                });
                match saved {
                    Some(key_ref) => {
                        let secrets = self.store.lock().secrets();
                        task::blocking(&self.handle, move || {
                            secrets
                                .get(&key_ref)
                                .ok()
                                .flatten()
                                .map(|key| key.to_string())
                        })
                        .await
                    }
                    None => None,
                }
            }
        };
        ProviderConfig {
            protocol: protocol.into(),
            base_url,
            model,
            api_key: key.filter(|key| !key.is_empty()).map(ApiKey::new),
        }
    }
}

/// The assistant's parts of a core: its conversations' database and how it
/// reaches model services, each made the first time it is needed.
pub(crate) struct AssistantParts {
    user_agent: String,
    store: OnceLock<Result<Store, String>>,
    transport: OnceLock<Result<Arc<dyn Transport>, String>>,
}

impl AssistantParts {
    pub(crate) fn new(user_agent: String) -> Self {
        Self {
            user_agent,
            store: OnceLock::new(),
            transport: OnceLock::new(),
        }
    }

    fn store(&self, data_dir: &std::path::Path) -> Result<Store, AssistantError> {
        self.store
            .get_or_init(|| {
                Store::open(&data_dir.join("assistant.db")).map_err(|error| error.to_string())
            })
            .clone()
            .map_err(|detail| AssistantError::Unavailable { detail })
    }

    fn transport(&self) -> Result<Arc<dyn Transport>, AssistantError> {
        self.transport
            .get_or_init(|| {
                ReqwestTransport::new(&self.user_agent)
                    .map(|transport| Arc::new(transport) as Arc<dyn Transport>)
            })
            .clone()
            .map_err(|detail| AssistantError::Unavailable { detail })
    }
}

#[uniffi::export]
impl Core {
    /// Start a conversation, new or saved, attached to `connection`:
    /// reads the model service's key from the keychain first.
    pub async fn start_assistant(
        &self,
        start: AssistantStart,
        connection: Arc<Connection>,
    ) -> Result<Arc<AssistantSession>, AssistantError> {
        let provider = self
            .store
            .lock()
            .ai_provider_by_public_id(&start.provider_public_id)
            .cloned()
            .ok_or(AssistantError::NoProvider)?;
        let store = self.assistant.store(&self.data_dir)?;
        let transport = self.assistant.transport()?;
        let model = provider
            .model_or_first(start.model.as_deref())
            .ok_or(AssistantError::NoProvider)?;
        let config = self
            .provider_config(
                AiProtocol::from(provider.protocol),
                provider.base_url.to_string(),
                model.to_string(),
                None,
                Some(provider.public_id.to_string()),
            )
            .await;
        let conversation = match start.conversation {
            Some(public_id) => ConversationChoice::Existing(public_id),
            None => ConversationChoice::New(NewConversation {
                host_public_id: start.host_public_id,
                host_label: start.host_label,
            }),
        };
        let setup = SessionSetup {
            provider: config,
            provider_public_id: Some(provider.public_id.to_string()),
            transport,
            env: Arc::new(SystemEnv),
            store,
            conversation,
            language: start.language,
            capabilities: Capabilities {
                run_command: true,
                read_file: true,
            },
            approval: start.approval.into(),
        };
        Ok(Arc::new(AssistantSession {
            session: Session::start(&self.handle, setup),
            connection: connection.inner.clone(),
            runtime: self.handle.clone(),
            calls: Mutex::new(HashMap::new()),
            running: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    /// 测试连接: ask the service one short question with `model`, with
    /// `api_key`, or the key saved for the service `public_id` when none is
    /// given.
    pub async fn check_ai_provider(
        &self,
        protocol: AiProtocol,
        base_url: String,
        model: String,
        api_key: Option<String>,
        public_id: Option<String>,
    ) -> Result<(), AssistantError> {
        let config = self
            .provider_config(protocol, base_url, model, api_key, public_id)
            .await;
        let transport = self.assistant.transport()?;
        task::run(&self.handle, async move {
            core::check_provider(&config, transport)
                .await
                .map_err(AssistantError::from)
        })
        .await
    }

    /// 刷新模型: the models the service lists (`GET /v1/models`), in its
    /// order; the key as for [`Self::check_ai_provider`]. A service without
    /// a list fails as `NotFound`.
    pub async fn ai_provider_models(
        &self,
        protocol: AiProtocol,
        base_url: String,
        api_key: Option<String>,
        public_id: Option<String>,
    ) -> Result<Vec<String>, AssistantError> {
        let config = self
            .provider_config(protocol, base_url, String::new(), api_key, public_id)
            .await;
        let transport = self.assistant.transport()?;
        task::run(&self.handle, async move {
            core::list_models(&config, transport)
                .await
                .map_err(AssistantError::from)
        })
        .await
    }

    /// The saved conversations about a host, or every one, the latest first.
    pub fn assistant_conversations(
        &self,
        host_public_id: Option<String>,
    ) -> Result<Vec<ConversationInfo>, AssistantError> {
        let store = self.assistant.store(&self.data_dir)?;
        store
            .conversations(host_public_id.as_deref())
            .map(|conversations| {
                conversations
                    .into_iter()
                    .map(ConversationInfo::from)
                    .collect()
            })
            .map_err(|error| AssistantError::Unavailable {
                detail: error.to_string(),
            })
    }

    /// A saved conversation's messages, to show before going on with it.
    pub fn assistant_messages(
        &self,
        public_id: String,
    ) -> Result<Vec<AssistantMessage>, AssistantError> {
        let store = self.assistant.store(&self.data_dir)?;
        store
            .messages(&public_id)
            .map(|messages| messages.iter().map(AssistantMessage::from).collect())
            .map_err(|error| AssistantError::Unavailable {
                detail: error.to_string(),
            })
    }

    pub fn delete_assistant_conversation(&self, public_id: String) -> Result<(), AssistantError> {
        let store = self.assistant.store(&self.data_dir)?;
        store
            .delete(&public_id)
            .map_err(|error| AssistantError::Unavailable {
                detail: error.to_string(),
            })
    }

    pub fn delete_all_assistant_conversations(&self) -> Result<(), AssistantError> {
        let store = self.assistant.store(&self.data_dir)?;
        store
            .delete_all()
            .map_err(|error| AssistantError::Unavailable {
                detail: error.to_string(),
            })
    }
}

/// A conversation going on, attached to a connection. It ends when the app
/// lets go of it or calls [`Self::end`].
#[derive(uniffi::Object)]
pub struct AssistantSession {
    session: Session,
    connection: Arc<crate::connection::Inner>,
    runtime: tokio::runtime::Handle,
    /// The calls the model made, by id, for the app to answer.
    calls: Mutex<HashMap<String, ToolCall>>,
    /// The commands running, to stop; each takes itself out when done.
    running: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
}

/// An SFTP channel beside the connection's shell.
struct ChannelOpener(Arc<SshHandle>);

impl ssh::sftp::SftpChannelOpener for ChannelOpener {
    fn open(&self) -> BoxFuture<'static, anyhow::Result<ssh::SftpChannel>> {
        let handle = self.0.clone();
        Box::pin(async move {
            let stream = ssh::open_sftp_channel(&handle, OPEN_TIMEOUT).await?;
            Ok(ssh::SftpChannel {
                stream,
                fingerprint: String::new(),
            })
        })
    }
}

fn output(kept: &ssh::KeptBytes) -> Output {
    Output {
        head: kept.head().to_vec(),
        tail: kept.tail(),
        skipped: kept.skipped(),
    }
}

#[uniffi::export]
impl AssistantSession {
    /// Ask, with what is known of the host now. Ignored while a turn goes
    /// on.
    pub fn send(&self, text: String, context: Option<AssistantContext>) {
        self.session.send(
            text,
            context.map(|context| core::HostContext {
                os: context.os,
                user: context.user,
                cwd: context.cwd,
                shell_only: false,
                restarted: false,
            }),
        );
    }

    /// Stop the turn going on and the commands it runs.
    pub fn stop(&self) {
        self.session.stop();
        for (_, stop) in self
            .running
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
        {
            let _ = stop.send(());
        }
    }

    pub fn set_approval(&self, approval: Approval) {
        self.session.set_approval(approval.into());
    }

    /// The next event; `None` once the session ended.
    pub async fn next_event(&self) -> Option<AssistantEvent> {
        let event = self.session.next_event().await?;
        Some(match event {
            SessionEvent::Opened {
                conversation,
                messages,
                ..
            } => AssistantEvent::Opened {
                public_id: conversation.public_id,
                title: conversation.title,
                messages: messages.iter().map(AssistantMessage::from).collect(),
            },
            SessionEvent::MessageAdded(message) => AssistantEvent::MessageAdded {
                message: AssistantMessage::from(&message),
            },
            SessionEvent::RequestStarted => AssistantEvent::RequestStarted,
            SessionEvent::TextDelta(text) => AssistantEvent::TextDelta { text },
            SessionEvent::ThinkingDelta(text) => AssistantEvent::ThinkingDelta { text },
            SessionEvent::ToolCall {
                id,
                call,
                needs_approval,
                dangerous,
            } => {
                let shown = AssistantCall::from(&call);
                self.calls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(id.clone(), call);
                AssistantEvent::ToolCall {
                    id,
                    call: shown,
                    needs_approval,
                    dangerous,
                }
            }
            SessionEvent::ToolFinished { id, status } => AssistantEvent::ToolFinished {
                id,
                status: status.into(),
            },
            SessionEvent::Retrying {
                attempt, of, after, ..
            } => AssistantEvent::Retrying {
                attempt,
                of,
                after_ms: after.as_millis() as u64,
            },
            SessionEvent::Usage {
                total,
                context,
                window,
            } => AssistantEvent::Usage {
                input: total.input,
                output: total.output,
                cache_read: total.cache_read,
                cache_write: total.cache_write,
                context,
                window,
            },
            SessionEvent::Trimmed => AssistantEvent::Trimmed,
            SessionEvent::ApprovalChanged(approval) => AssistantEvent::ApprovalChanged {
                approval: approval.into(),
            },
            SessionEvent::TurnEnded(end) => AssistantEvent::TurnEnded { end: end.into() },
            SessionEvent::Failed(failure) => AssistantEvent::Failed {
                failure: failure.into(),
            },
        })
    }

    /// Answer a `ReadTerminal` call with what the terminal shows.
    pub fn answer_terminal(&self, id: String, text: String, full_screen: bool) {
        self.session
            .answer(id, ToolAnswer::Terminal { text, full_screen }, false);
    }

    /// The user said no to a call.
    pub fn deny(&self, id: String) {
        self.session
            .answer(id, ToolAnswer::Denied { reason: None }, false);
    }

    /// The app put a command the model asked to run into the terminal for
    /// the user to run.
    pub fn answer_inserted(&self, id: String, command: String) {
        self.session
            .answer(id, ToolAnswer::Inserted { command }, false);
    }

    /// Run a `RunCommand` or `ReadFile` call on the connection, beside the
    /// shell. With `remember`, the calls of its kind are not asked about
    /// again in this conversation: the commands of the same programs, or
    /// the same file (`Remembered` in the assistant core).
    pub fn run_call(&self, id: String, remember: bool) {
        let Some(call) = self
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&id)
            .cloned()
        else {
            return;
        };
        let answerer = self.session.answerer();
        let connection = self.connection.clone();
        match call {
            ToolCall::ReadTerminal { .. } => {}
            ToolCall::RunCommand {
                command,
                cwd,
                timeout,
            } => {
                let line = match cwd {
                    Some(cwd) => format!("cd -- '{}' && {command}", cwd.replace('\'', r"'\''")),
                    None => command,
                };
                let (stop, stopped) = oneshot::channel::<()>();
                self.running
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(id.clone(), stop);
                let running = self.running.clone();
                self.runtime.spawn(async move {
                    let outcome = run_command(&connection, line, timeout, stopped).await;
                    running
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&id);
                    let answer = match outcome {
                        Ok(answer) => ToolAnswer::Command(Box::new(answer)),
                        Err(error) => ToolAnswer::Failed {
                            reason: format!("{error:#}"),
                        },
                    };
                    answerer.answer(id, answer, remember);
                });
            }
            ToolCall::ReadFile { path } => {
                self.runtime.spawn(async move {
                    let answer = match read_file(&connection, &path).await {
                        Ok(start) => ToolAnswer::File {
                            text: String::from_utf8_lossy(&start.bytes).into_owned(),
                            total_bytes: start.size,
                        },
                        Err(error) => ToolAnswer::Failed {
                            reason: format!("{error:#}"),
                        },
                    };
                    answerer.answer(id, answer, remember);
                });
            }
        }
    }

    /// End the conversation. (Not `close`, which Kotlin's `AutoCloseable`
    /// takes.)
    pub fn end(&self) {
        self.stop();
    }
}

impl Drop for AssistantSession {
    /// Let go of by the app: the commands it runs stop with it.
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run_command(
    connection: &crate::connection::Inner,
    command: String,
    timeout: Duration,
    stopped: oneshot::Receiver<()>,
) -> anyhow::Result<CommandAnswer> {
    let handle = connection
        .up()
        .await
        .map_err(|_| anyhow::anyhow!("the connection is not logged in"))?;
    let channel = ssh::open_exec(&handle, &command, OPEN_TIMEOUT).await?;
    let late = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let timed_out = late.clone();
    let stop = async move {
        tokio::select! {
            _ = stopped => {}
            () = tokio::time::sleep(timeout) => late.store(true, std::sync::atomic::Ordering::SeqCst),
        }
    };
    let limits = ssh::ExecLimits {
        head: core::OUTPUT_HEAD,
        tail: core::OUTPUT_TAIL,
    };
    let outcome = channel.finish(limits, stop).await?;
    let end = match outcome.end {
        ssh::ExecEnd::Exited(Some(ssh::ExecExit::Code(code))) => {
            CommandEnd::Exited(Some(i64::from(code)))
        }
        ssh::ExecEnd::Exited(Some(ssh::ExecExit::Signal(signal))) => CommandEnd::Signal(signal),
        ssh::ExecEnd::Exited(None) => CommandEnd::Exited(None),
        ssh::ExecEnd::Stopped if timed_out.load(std::sync::atomic::Ordering::SeqCst) => {
            CommandEnd::TimedOut
        }
        ssh::ExecEnd::Stopped => CommandEnd::Stopped,
    };
    Ok(CommandAnswer {
        command,
        run_in: core::RunIn::Background,
        stdout: output(&outcome.stdout),
        stderr: Some(output(&outcome.stderr)),
        end,
        ran_as: None,
    })
}

async fn read_file(
    connection: &crate::connection::Inner,
    path: &str,
) -> anyhow::Result<ssh::sftp::FileStart> {
    let handle = connection
        .up()
        .await
        .map_err(|_| anyhow::anyhow!("the connection is not logged in"))?;
    ssh::sftp::read_file_start(&ChannelOpener(handle), path, core::FILE_LIMIT as u64).await
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use shellrs_hosts::sync::SyncServices;
    use shellrs_sync::DeviceInfo;

    use super::*;
    use crate::core::CoreConfig;
    use crate::secrets::{Keychain, KeychainError};

    #[derive(Default)]
    struct Memory(Mutex<HashMap<String, String>>);

    impl Keychain for Memory {
        fn available(&self) -> bool {
            true
        }

        fn get(&self, account: String) -> Result<Option<String>, KeychainError> {
            Ok(self.0.lock().unwrap().get(&account).cloned())
        }

        fn set(&self, account: String, value: String) -> Result<(), KeychainError> {
            self.0.lock().unwrap().insert(account, value);
            Ok(())
        }

        fn delete(&self, account: String) -> Result<(), KeychainError> {
            self.0.lock().unwrap().remove(&account);
            Ok(())
        }
    }

    fn phone(data: &std::path::Path, keychain: &Arc<Memory>) -> Arc<Core> {
        let device = DeviceInfo {
            name: "phone".into(),
            platform: "ios".into(),
            app_version: "0.0.0".into(),
        };
        let config = CoreConfig {
            data_dir: data.display().to_string(),
            device_name: device.name.clone(),
            platform: device.platform.clone(),
            app_version: device.app_version.clone(),
        };
        Core::open(
            config,
            keychain.clone(),
            SyncServices::system("ShellRS/0.0.0", device),
        )
        .unwrap()
    }

    fn draft(models: &[&str]) -> AiProviderDraft {
        AiProviderDraft {
            name: "DeepSeek".into(),
            protocol: AiProtocol::OpenAi,
            base_url: "https://api.deepseek.com".into(),
            models: models.iter().map(|model| model.to_string()).collect(),
        }
    }

    #[test]
    fn a_model_service_is_saved_with_its_key_and_deleted_with_it() {
        let data = tempfile::tempdir().unwrap();
        let keychain = Arc::new(Memory::default());
        let core = phone(data.path(), &keychain);
        let store = core.store();

        let id = futures::executor::block_on(store.save_ai_provider(
            None,
            draft(&["deepseek-chat"]),
            Some("sk-test".into()),
        ))
        .unwrap();
        let account = format!("ai_key:{id}");
        assert_eq!(
            keychain.0.lock().unwrap().get(&account).map(String::as_str),
            Some("sk-test")
        );
        assert_eq!(store.ai_providers()[0].models, ["deepseek-chat"]);

        // Changed without a key: the key stays.
        futures::executor::block_on(store.save_ai_provider(
            Some(id.clone()),
            draft(&["deepseek-reasoner", " deepseek-chat "]),
            None,
        ))
        .unwrap();
        assert_eq!(
            store.ai_providers()[0].models,
            ["deepseek-reasoner", "deepseek-chat"]
        );
        assert!(keychain.0.lock().unwrap().contains_key(&account));

        let wrong = futures::executor::block_on(store.save_ai_provider(None, draft(&[" "]), None));
        assert!(matches!(
            wrong,
            Err(StoreError::AiProvider {
                problem: AiProviderProblem::Model
            })
        ));

        store.delete_ai_provider(id).unwrap();
        assert!(store.ai_providers().is_empty());
        // The keychain's delete runs on the core's threads.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while keychain.0.lock().unwrap().contains_key(&account)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!keychain.0.lock().unwrap().contains_key(&account));
        assert!(core.assistant_conversations(None).unwrap().is_empty());
    }
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;

    #[test]
    fn an_endpoint_is_the_address_and_the_protocols_path() {
        assert_eq!(
            ai_endpoint(
                AiProtocol::OpenAiResponses,
                "https://api.openai.com/".into()
            ),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            ai_endpoint(
                AiProtocol::OpenAi,
                "https://open.bigmodel.cn/api/paas/v4".into()
            ),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
    }
}
