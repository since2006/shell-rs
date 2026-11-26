//! AI 助手 in the right sidebar: a conversation per SSH terminal, with the
//! model the user picked. Its turns run on the assistant's thread; the
//! panel runs what the model calls for on the terminal it is attached to,
//! asking first as the approval says, and polls the session for what
//! happened.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Side, Sizable as _,
    button::{Button, ButtonVariants as _},
    clipboard::Clipboard,
    h_flex,
    input::{
        InputEvent, InputGroup, InputGroupAddon, InputGroupAddonAlignment, InputGroupButton,
        Textarea, TextareaState,
    },
    menu::DropdownMenu as _,
    message_scroller::{MessageScroller, MessageScrollerState},
    shimmer::ShimmerText,
    text::TextView,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shellrs_assistant::{
    ApiKey, Approval, Capabilities, ConversationChoice, ConversationSummary, HostContext, Message,
    NewConversation, NoticeKind, Part, ProviderConfig, Remembered, ResultStatus, Role, RunIn,
    Session, SessionEvent, SessionFailure, SessionSetup, ToolAnswer, ToolCall, TurnEnd, Usage,
    command_kinds,
};

use super::core_protocol;
use super::describe::{provider_error, turn_end};
use super::models_menu::{default_model, model_groups, model_items};
use super::run::{RunningCommand, insert, read_file, run_beside, run_in_terminal, with_cwd};
use super::service::AssistantService;
use crate::analytics::Counter;
use crate::app::AiModelChoice;
use crate::app::{
    CatalogIcon, DenyAssistantCall, InsertAssistantCall, NewAiProvider, NewAssistantConversation,
    OpenAssistantConversation, OpenAssistantLink, OpenAssistantSettings, RunAssistantCall,
    SendAssistantMessage, SetConversationApproval, SetConversationRunIn, StopAssistant,
    ToggleAssistantCallDetails, UseAiModel,
};
use crate::host::{AiProvider, HostOs, HostStore};
use crate::i18n::t;
use crate::settings::{AssistantApproval, AssistantRunIn, Choice as _, SettingsStore};
use crate::terminal::RemoteTerminalId;
use crate::workspace::ToolTerminal;

/// What the panel tells the workspace: what to count.
pub enum AssistantPanelEvent {
    Count(Counter),
}

/// Where a call the model made stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// The user is asked first.
    Asking,
    Running,
    Done(ResultStatus),
}

struct CallState {
    call: ToolCall,
    dangerous: bool,
    phase: Phase,
    running: Option<RunningCommand>,
}

/// What the panel says under the conversation.
#[derive(Clone, Debug, PartialEq)]
enum Status {
    Retrying { attempt: u32, of: u32 },
    Failed(SharedString),
    Ended(TurnEnd),
}

/// A terminal's conversation.
struct Chat {
    terminal: ToolTerminal,
    conversation: Option<ConversationSummary>,
    messages: Vec<Message>,
    session: Option<Session>,
    /// The API key is being read to start a session.
    starting: bool,
    /// The session started and has not said `Opened` yet.
    opening: bool,
    /// What the user sent before the session could take it.
    pending: Option<(String, HostContext)>,
    /// The answer as it streams in.
    streaming: String,
    thinking: bool,
    calls: HashMap<String, CallState>,
    busy: bool,
    usage: Usage,
    context: Option<(u64, u64)>,
    /// A service that sends no usage was asked.
    usage_unknown: bool,
    status: Option<Status>,
    approval: Approval,
    run_in: RunIn,
    /// The user picked the execution mode in this conversation.
    run_in_chosen: bool,
    /// The model service picked for it, by public id, and which of its
    /// models.
    provider: Option<String>,
    model: Option<String>,
    expanded: HashSet<String>,
    scroller: Entity<MessageScrollerState>,
}

/// One row of the conversation, as drawn.
#[derive(Clone)]
enum Row {
    User(SharedString),
    Assistant {
        id: SharedString,
        text: SharedString,
    },
    Tool {
        id: SharedString,
        call: ToolCallView,
    },
    Notice(SharedString),
    Streaming {
        text: SharedString,
    },
    Thinking,
    Status(SharedString, bool),
    Continue,
}

#[derive(Clone)]
struct ToolCallView {
    label: SharedString,
    detail: Option<SharedString>,
    phase: Phase,
    dangerous: bool,
    asking: bool,
    /// 「执行，本对话不再问 …」: what else it lets run unasked.
    always: SharedString,
    /// Which programs it lets run, said when pointed at.
    always_tip: Option<SharedString>,
    command: bool,
    expanded: bool,
}

pub struct AssistantPanel {
    store: Entity<HostStore>,
    settings: Entity<SettingsStore>,
    service: Entity<AssistantService>,
    dispatch: FocusHandle,
    target: Option<ToolTerminal>,
    showing: bool,
    chats: HashMap<RemoteTerminalId, Chat>,
    composer: Entity<TextareaState>,
    pump: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AssistantPanelEvent> for AssistantPanel {}

/// The language the model answers in, by the name it understands.
fn language() -> &'static str {
    if crate::i18n::locale().starts_with("zh") {
        "Simplified Chinese"
    } else {
        "English"
    }
}

impl AssistantPanel {
    pub fn new(
        store: Entity<HostStore>,
        settings: Entity<SettingsStore>,
        service: Entity<AssistantService>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .auto_grow(1, 8)
                .placeholder(t!("assistant.panel.placeholder"))
        });
        let subscriptions = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&service, |_, _, cx| cx.notify()),
            // 启用 AI 助手 and the default model.
            cx.observe(&settings, |_, _, cx| cx.notify()),
            // Queued through the window: the handler reads the text, so it
            // must not run while the input is being updated.
            cx.subscribe_in(&composer, window, |_, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { shift: false, .. } => {
                        window.dispatch_action(Box::new(SendAssistantMessage), cx);
                    }
                    // 发送 is off while there is nothing to send.
                    InputEvent::Change => cx.notify(),
                    _ => {}
                }
            }),
        ];
        Self {
            store,
            settings,
            service,
            dispatch,
            target: None,
            showing: false,
            chats: HashMap::new(),
            composer,
            pump: None,
            _subscriptions: subscriptions,
        }
    }

    /// Work beside `target`, the SSH terminal in front, while `showing`.
    pub fn set_target(
        &mut self,
        target: Option<ToolTerminal>,
        showing: bool,
        cx: &mut Context<Self>,
    ) {
        self.showing = showing;
        if let Some(terminal) = &target
            && showing
            && !self.chats.contains_key(&terminal.id)
        {
            let chat = self.new_chat(terminal.clone(), cx);
            self.chats.insert(terminal.id, chat);
        }
        // A chat follows its terminal across reconnects and renames.
        if let Some(terminal) = &target
            && let Some(chat) = self.chats.get_mut(&terminal.id)
        {
            chat.terminal = terminal.clone();
        }
        self.target = target;
        cx.notify();
    }

    /// Forget the chat of a terminal that closed, stopping its turn.
    pub fn terminal_closed(&mut self, terminal: RemoteTerminalId, cx: &mut Context<Self>) {
        if let Some(mut chat) = self.chats.remove(&terminal) {
            stop_calls(&mut chat);
        }
        cx.notify();
    }

    fn new_chat(&self, terminal: ToolTerminal, cx: &mut Context<Self>) -> Chat {
        let defaults = &self.settings.read(cx).settings().assistant;
        Chat {
            terminal,
            conversation: None,
            messages: Vec::new(),
            session: None,
            starting: false,
            opening: false,
            pending: None,
            streaming: String::new(),
            thinking: false,
            calls: HashMap::new(),
            busy: false,
            usage: Usage::default(),
            context: None,
            usage_unknown: false,
            status: None,
            approval: defaults.approval.core(),
            run_in: defaults.run_in.core(),
            run_in_chosen: false,
            provider: None,
            model: None,
            expanded: HashSet::new(),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
        }
    }

    fn current(&self) -> Option<RemoteTerminalId> {
        self.target.as_ref().map(|terminal| terminal.id)
    }

    fn chat_mut(&mut self) -> Option<&mut Chat> {
        let id = self.current()?;
        self.chats.get_mut(&id)
    }

    /// The model service a chat uses and its model: the one picked for it,
    /// else the settings' default, else the first service's first model.
    fn model_of(&self, chat: &Chat, cx: &App) -> Option<(AiProvider, SharedString)> {
        let store = self.store.read(cx);
        if let Some(provider) = chat
            .provider
            .as_deref()
            .and_then(|id| store.ai_provider_by_public_id(id))
        {
            let model = provider.model_or_first(chat.model.as_deref())?;
            return Some((provider.clone(), model));
        }
        let settings = &self.settings.read(cx).settings().assistant;
        default_model(
            store.ai_providers(),
            settings.provider.as_deref(),
            settings.model.as_deref(),
        )
        .map(|(provider, model)| (provider.clone(), model))
    }

    fn host_context(&self, chat: &Chat, cx: &App) -> HostContext {
        let host = self.store.read(cx).host(chat.terminal.host);
        HostContext {
            os: chat.terminal.os.map(|os| os.as_str().to_owned()),
            user: host.map(|host| host.user.to_string()),
            cwd: chat
                .terminal
                .view
                .upgrade()
                .and_then(|view| view.read(cx).directory(cx))
                .map(|directory| directory.path),
            shell_only: chat.terminal.shell_only,
            restarted: false,
        }
    }

    /// Send what the composer holds.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.send_text(text, cx) {
            self.composer
                .update(cx, |composer, cx| composer.set_value("", window, cx));
        }
    }

    /// Send `text` in the chat in front. False when it cannot go now.
    fn send_text(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        if !self.enabled(cx) {
            return false;
        }
        let Some(id) = self.current() else {
            return false;
        };
        let Some(chat) = self.chats.get(&id) else {
            return false;
        };
        if chat.busy || chat.starting {
            return false;
        }
        let context = self.host_context(chat, cx);
        let chat = self.chats.get_mut(&id).expect("looked up above");
        chat.status = None;
        chat.busy = true;
        cx.emit(AssistantPanelEvent::Count(Counter::AssistantMessage));
        match &chat.session {
            Some(session) => session.send(text, Some(context)),
            None => {
                chat.pending = Some((text, context));
                self.start_session(id, cx);
            }
        }
        self.ensure_pump(cx);
        cx.notify();
        true
    }

    /// Read the model service's key off the UI thread, then start a session.
    fn start_session(&mut self, id: RemoteTerminalId, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.get(&id) else {
            return;
        };
        let Some((provider, model)) = self.model_of(chat, cx) else {
            let chat = self.chats.get_mut(&id).expect("looked up above");
            chat.busy = false;
            chat.pending = None;
            chat.status = Some(Status::Failed(t!("assistant.panel.no_provider")));
            return;
        };
        if let Some(problem) = self.service.read(cx).problem() {
            let chat = self.chats.get_mut(&id).expect("looked up above");
            chat.busy = false;
            chat.pending = None;
            chat.status = Some(Status::Failed(problem));
            return;
        }
        let secrets = self.store.read(cx).secrets();
        let key = provider.key_secret();
        if let Some(chat) = self.chats.get_mut(&id) {
            chat.starting = true;
        }
        cx.spawn(async move |this, cx| {
            let key = cx
                .background_executor()
                .spawn(async move { secrets.get(&key).ok().flatten() })
                .await;
            let _ = this.update(cx, |this, cx| {
                let key = key
                    .map(|key| key.to_string())
                    .filter(|key| !key.is_empty())
                    .map(ApiKey::new);
                this.session_ready(id, provider, model, key, cx);
            });
        })
        .detach();
    }

    fn session_ready(
        &mut self,
        id: RemoteTerminalId,
        provider: AiProvider,
        model: SharedString,
        key: Option<ApiKey>,
        cx: &mut Context<Self>,
    ) {
        let service = self.service.read(cx);
        let (Some(services), Some(handle)) =
            (service.services().cloned(), service.handle().cloned())
        else {
            if let Some(chat) = self.chats.get_mut(&id) {
                chat.starting = false;
                chat.busy = false;
                chat.pending = None;
                chat.status = Some(Status::Failed(t!("assistant.service.unavailable")));
            }
            return;
        };
        let host_public_id = {
            let Some(chat) = self.chats.get(&id) else {
                return;
            };
            self.store
                .read(cx)
                .saved_host(chat.terminal.host)
                .map(|host| host.public_id.to_string())
        };
        let Some(chat) = self.chats.get_mut(&id) else {
            return;
        };
        chat.starting = false;
        let saved = chat.conversation.as_ref().filter(|conversation| {
            matches!(
                services.store.conversation(&conversation.public_id),
                Ok(Some(_))
            )
        });
        let conversation = match saved {
            Some(conversation) => ConversationChoice::Existing(conversation.public_id.clone()),
            None => ConversationChoice::New(NewConversation {
                host_public_id,
                host_label: chat.terminal.name.to_string(),
            }),
        };
        let setup = SessionSetup {
            provider: ProviderConfig {
                protocol: core_protocol(provider.protocol),
                base_url: provider.base_url.to_string(),
                model: model.to_string(),
                api_key: key,
            },
            provider_public_id: Some(provider.public_id.to_string()),
            transport: services.transport.clone(),
            env: services.env.clone(),
            store: services.store.clone(),
            conversation,
            language: language().into(),
            // A bastion host's link has no SFTP beside the shell.
            capabilities: Capabilities {
                run_command: true,
                read_file: !chat.terminal.shell_only,
            },
            approval: chat.approval,
        };
        let session = Session::start(&handle, setup);
        if let Some((text, context)) = chat.pending.take() {
            session.send(text, Some(context));
        }
        chat.session = Some(session);
        chat.opening = true;
        self.ensure_pump(cx);
        cx.notify();
    }

    /// Poll the sessions while any has something going on.
    fn ensure_pump(&mut self, cx: &mut Context<Self>) {
        if self.pump.is_some() {
            return;
        }
        self.pump = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let Ok(going) = this.update(cx, |this, cx| this.drain(cx)) else {
                    break;
                };
                if !going {
                    let _ = this.update(cx, |this, _| this.pump = None);
                    break;
                }
            }
        }));
    }

    /// Take what the sessions said; whether any has more coming.
    fn drain(&mut self, cx: &mut Context<Self>) -> bool {
        let ids: Vec<RemoteTerminalId> = self.chats.keys().copied().collect();
        let mut going = false;
        let mut changed = false;
        for id in ids {
            loop {
                let event = self
                    .chats
                    .get(&id)
                    .and_then(|chat| chat.session.as_ref())
                    .and_then(Session::try_event);
                let Some(event) = event else {
                    break;
                };
                changed = true;
                self.on_event(id, event, cx);
            }
            if let Some(chat) = self.chats.get(&id) {
                going |= chat.busy || chat.starting || chat.opening;
            }
        }
        if changed {
            cx.notify();
        }
        going
    }

    fn on_event(&mut self, id: RemoteTerminalId, event: SessionEvent, cx: &mut Context<Self>) {
        let Some(chat) = self.chats.get_mut(&id) else {
            return;
        };
        match event {
            SessionEvent::Opened {
                conversation,
                messages,
                usage,
            } => {
                chat.opening = false;
                chat.approval = conversation.approval.unwrap_or(chat.approval);
                if !chat.run_in_chosen
                    && let Some(run_in) = conversation.run_in
                {
                    chat.run_in = run_in;
                }
                chat.conversation = Some(conversation);
                chat.messages = messages;
                chat.usage = usage;
            }
            SessionEvent::MessageAdded(message) => {
                if message.role == Role::Assistant {
                    chat.streaming.clear();
                    chat.thinking = false;
                }
                let seq = message.seq as usize;
                if seq < chat.messages.len() {
                    chat.messages[seq] = message;
                } else {
                    chat.messages.push(message);
                }
            }
            SessionEvent::RequestStarted => {
                chat.streaming.clear();
                chat.thinking = false;
            }
            SessionEvent::TextDelta(text) => {
                chat.streaming.push_str(&text);
                if matches!(chat.status, Some(Status::Retrying { .. })) {
                    chat.status = None;
                }
            }
            SessionEvent::ThinkingDelta(_) => chat.thinking = true,
            SessionEvent::ToolCall {
                id: call_id,
                call,
                needs_approval,
                dangerous,
            } => {
                chat.calls.insert(
                    call_id.clone(),
                    CallState {
                        call,
                        dangerous,
                        phase: if needs_approval {
                            Phase::Asking
                        } else {
                            Phase::Running
                        },
                        running: None,
                    },
                );
                if !needs_approval {
                    self.start_call(id, &call_id, false, cx);
                }
            }
            SessionEvent::ToolFinished {
                id: call_id,
                status,
            } => {
                if let Some(call) = chat.calls.get_mut(&call_id) {
                    call.phase = Phase::Done(status);
                    call.running = None;
                }
            }
            SessionEvent::Retrying { attempt, of, .. } => {
                chat.streaming.clear();
                chat.status = Some(Status::Retrying { attempt, of });
            }
            SessionEvent::Usage {
                total,
                context,
                window,
            } => {
                chat.usage = total;
                chat.context = Some((context, window));
                chat.usage_unknown = false;
            }
            SessionEvent::Trimmed => {}
            SessionEvent::ApprovalChanged(approval) => chat.approval = approval,
            SessionEvent::TurnEnded(end) => {
                chat.busy = false;
                chat.streaming.clear();
                chat.thinking = false;
                stop_calls(chat);
                chat.status = (end != TurnEnd::Done).then_some(Status::Ended(end));
                let answered = chat
                    .messages
                    .iter()
                    .rev()
                    .find(|message| message.role == Role::Assistant)
                    .is_some_and(|message| message.usage.is_none());
                chat.usage_unknown = answered && chat.context.is_none();
                // The turn saved it: its title too, the first time.
                let saved = chat.conversation.as_ref().and_then(|conversation| {
                    self.service.read(cx).store().and_then(|store| {
                        store.conversation(&conversation.public_id).ok().flatten()
                    })
                });
                if let Some(saved) = saved
                    && let Some(chat) = self.chats.get_mut(&id)
                {
                    chat.conversation = Some(saved);
                }
                self.keep_run_in(id, cx);
            }
            SessionEvent::Failed(failure) => {
                chat.busy = false;
                chat.opening = false;
                chat.streaming.clear();
                stop_calls(chat);
                chat.status = Some(Status::Failed(match failure {
                    SessionFailure::Provider(error) => provider_error(&error),
                    SessionFailure::Store(error) => t!("assistant.error.store", error = error),
                    SessionFailure::NotFound => t!("assistant.error.conversation_gone"),
                }));
            }
        }
    }

    /// The execution mode picked in a conversation goes with it once it is
    /// saved.
    fn keep_run_in(&mut self, id: RemoteTerminalId, cx: &App) {
        let Some(chat) = self.chats.get(&id) else {
            return;
        };
        if !chat.run_in_chosen {
            return;
        }
        if let (Some(store), Some(conversation)) =
            (self.service.read(cx).store(), &chat.conversation)
        {
            let _ = store.set_run_in(&conversation.public_id, Some(chat.run_in));
        }
    }

    /// Run a call the model made, on the chat's terminal.
    fn start_call(
        &mut self,
        id: RemoteTerminalId,
        call_id: &str,
        remember: bool,
        cx: &mut Context<Self>,
    ) {
        let handle = self.service.read(cx).handle().cloned();
        let Some(chat) = self.chats.get_mut(&id) else {
            return;
        };
        let Some(answerer) = chat.session.as_ref().map(Session::answerer) else {
            return;
        };
        let Some(state) = chat.calls.get_mut(call_id) else {
            return;
        };
        state.phase = Phase::Running;
        let Some(view) = chat.terminal.view.upgrade() else {
            answerer.answer(
                call_id,
                ToolAnswer::Failed {
                    reason: t!("assistant.run.no_terminal").to_string(),
                },
                remember,
            );
            return;
        };
        match state.call.clone() {
            ToolCall::ReadTerminal { lines } => {
                let text = view.read(cx).recent_text(lines, cx);
                answerer.answer(
                    call_id,
                    ToolAnswer::Terminal {
                        text: text.text,
                        full_screen: text.full_screen,
                    },
                    remember,
                );
            }
            ToolCall::RunCommand {
                command,
                cwd,
                timeout,
            } => {
                let line = with_cwd(&command, cwd.as_deref());
                let terminal = chat.terminal.shell_only || chat.run_in == RunIn::Terminal;
                let answer = (call_id.to_owned(), answerer, remember);
                let running = match (terminal, handle) {
                    (false, Some(handle)) => run_beside(&view, &handle, line, timeout, answer, cx),
                    _ => run_in_terminal(&view, line, timeout, answer, cx),
                };
                state.running = Some(running);
                cx.emit(AssistantPanelEvent::Count(if terminal {
                    Counter::AssistantCommandTerminal
                } else {
                    Counter::AssistantCommandBackground
                }));
            }
            ToolCall::ReadFile { path } => match handle {
                Some(handle) => read_file(
                    &view,
                    &handle,
                    path,
                    (call_id.to_owned(), answerer, remember),
                    cx,
                ),
                None => answerer.answer(
                    call_id,
                    ToolAnswer::Failed {
                        reason: t!("assistant.run.no_files").to_string(),
                    },
                    remember,
                ),
            },
        }
    }

    /// The user's answer to a call they were asked about.
    pub fn run_call(&mut self, call_id: &str, remember: bool, cx: &mut Context<Self>) {
        let Some(id) = self.current() else {
            return;
        };
        let Some(chat) = self.chats.get(&id) else {
            return;
        };
        let Some(asked) = chat
            .calls
            .get(call_id)
            .filter(|call| call.phase == Phase::Asking)
        else {
            return;
        };
        // The model's other calls of the same reply, asked about before the
        // answer: those of the kind not asked about again run with it.
        let mut others = Vec::new();
        if remember {
            let mut remembered = Remembered::default();
            remembered.remember(&asked.call);
            others = chat
                .calls
                .iter()
                .filter(|(other, state)| {
                    other.as_str() != call_id
                        && state.phase == Phase::Asking
                        && remembered.covers(&state.call)
                })
                .map(|(other, _)| other.clone())
                .collect();
            others.sort();
        }
        self.start_call(id, call_id, remember, cx);
        for other in others {
            self.start_call(id, &other, false, cx);
        }
        cx.notify();
    }

    pub fn deny_call(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let Some(chat) = self.chat_mut() else {
            return;
        };
        let Some(state) = chat
            .calls
            .get_mut(call_id)
            .filter(|call| call.phase == Phase::Asking)
        else {
            return;
        };
        state.phase = Phase::Done(ResultStatus::Denied);
        if let Some(session) = &chat.session {
            session.answer(call_id, ToolAnswer::Denied { reason: None }, false);
        }
        cx.notify();
    }

    /// Put an asked-about command into the terminal for the user to run.
    pub fn insert_call(&mut self, call_id: &str, cx: &mut Context<Self>) {
        let Some(id) = self.current() else {
            return;
        };
        let Some(chat) = self.chats.get_mut(&id) else {
            return;
        };
        let Some(state) = chat
            .calls
            .get_mut(call_id)
            .filter(|call| call.phase == Phase::Asking)
        else {
            return;
        };
        let ToolCall::RunCommand { command, .. } = state.call.clone() else {
            return;
        };
        let windows = chat.terminal.os == Some(HostOs::Windows);
        let Some(view) = chat.terminal.view.upgrade() else {
            return;
        };
        let answer = match insert(&view, &command, windows, cx) {
            Ok(()) => ToolAnswer::Inserted { command },
            Err(reason) => ToolAnswer::Failed {
                reason: reason.to_string(),
            },
        };
        let chat = self.chats.get_mut(&id).expect("looked up above");
        if let Some(state) = chat.calls.get_mut(call_id) {
            state.phase = Phase::Running;
        }
        if let Some(session) = &chat.session {
            session.answer(call_id, answer, false);
        }
        cx.notify();
    }

    pub fn toggle_details(&mut self, call_id: &str, cx: &mut Context<Self>) {
        if let Some(chat) = self.chat_mut()
            && !chat.expanded.remove(call_id)
        {
            chat.expanded.insert(call_id.to_owned());
        }
        cx.notify();
    }

    /// Stop the turn going on, and the commands it runs.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = self.chat_mut() {
            if let Some(session) = &chat.session {
                session.stop();
            }
            stop_calls(chat);
            if chat.starting {
                chat.pending = None;
            }
        }
        cx.notify();
    }

    /// Begin a new conversation in the chat in front.
    pub fn new_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.current() else {
            return;
        };
        let Some(old) = self.chats.remove(&id) else {
            return;
        };
        let mut chat = self.new_chat(old.terminal.clone(), cx);
        chat.provider = old.provider.clone();
        chat.model = old.model.clone();
        drop(old);
        self.chats.insert(id, chat);
        cx.notify();
    }

    /// Open a saved conversation in the chat in front.
    pub fn open_conversation(&mut self, public_id: &str, cx: &mut Context<Self>) {
        let Some(id) = self.current() else {
            return;
        };
        let Some(store) = self.service.read(cx).store().cloned() else {
            return;
        };
        let (Ok(Some(summary)), Ok(messages), Ok(usage)) = (
            store.conversation(public_id),
            store.messages(public_id),
            store.usage(public_id),
        ) else {
            return;
        };
        let Some(old) = self.chats.remove(&id) else {
            return;
        };
        let mut chat = self.new_chat(old.terminal.clone(), cx);
        drop(old);
        chat.approval = summary.approval.unwrap_or(chat.approval);
        chat.run_in = summary.run_in.unwrap_or(chat.run_in);
        chat.provider = summary.provider_public_id.clone();
        chat.model = Some(summary.model.clone()).filter(|model| !model.is_empty());
        chat.messages = messages;
        chat.usage = usage;
        chat.conversation = Some(summary);
        self.chats.insert(id, chat);
        cx.notify();
    }

    /// A conversation was deleted: a chat showing it starts over.
    pub fn conversation_deleted(&mut self, public_id: Option<&str>, cx: &mut Context<Self>) {
        let ids: Vec<RemoteTerminalId> = self
            .chats
            .iter()
            .filter(|(_, chat)| {
                public_id.is_none_or(|public_id| {
                    chat.conversation
                        .as_ref()
                        .is_some_and(|conversation| conversation.public_id == public_id)
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(old) = self.chats.remove(&id) {
                let chat = self.new_chat(old.terminal.clone(), cx);
                self.chats.insert(id, chat);
            }
        }
        cx.notify();
    }

    /// The conversation in front, for the workspace to delete.
    pub fn current_conversation(&self) -> Option<ConversationSummary> {
        let id = self.current()?;
        self.chats.get(&id)?.conversation.clone()
    }

    pub fn set_approval(&mut self, approval: Approval, cx: &mut Context<Self>) {
        let store = self.service.read(cx).store().cloned();
        if let Some(chat) = self.chat_mut() {
            chat.approval = approval;
            match (&chat.session, &chat.conversation, store) {
                (Some(session), _, _) => session.set_approval(approval),
                (None, Some(conversation), Some(store)) => {
                    let _ = store.set_approval(&conversation.public_id, Some(approval));
                }
                _ => {}
            }
        }
        cx.notify();
    }

    pub fn approval(&self) -> Option<Approval> {
        let id = self.current()?;
        Some(self.chats.get(&id)?.approval)
    }

    pub fn set_run_in(&mut self, run_in: RunIn, cx: &mut Context<Self>) {
        let Some(id) = self.current() else {
            return;
        };
        if let Some(chat) = self.chats.get_mut(&id) {
            chat.run_in = run_in;
            chat.run_in_chosen = true;
        }
        self.keep_run_in(id, cx);
        cx.notify();
    }

    /// Use another model in the chat in front, from its next message on.
    pub fn use_model(&mut self, choice: &AiModelChoice, cx: &mut Context<Self>) {
        if let Some(chat) = self.chat_mut()
            && !chat.busy
        {
            chat.provider = Some(choice.provider.clone());
            chat.model = Some(choice.model.clone());
            chat.session = None;
        }
        cx.notify();
    }

    fn rows(&self, chat: &Chat) -> Vec<Row> {
        let mut rows = Vec::new();
        // Each call's answer, from the messages after it.
        let mut results: HashMap<&str, (ResultStatus, &str)> = HashMap::new();
        for message in &chat.messages {
            for part in &message.parts {
                if let Part::ToolResult {
                    call_id,
                    status,
                    output,
                    ..
                } = part
                {
                    results.insert(call_id, (*status, output));
                }
            }
        }
        for message in &chat.messages {
            match message.role {
                Role::User => {
                    let text = message.text();
                    if !text.is_empty() {
                        rows.push(Row::User(text.into()));
                    }
                }
                Role::Assistant => {
                    let text = message.text();
                    if !text.trim().is_empty() {
                        rows.push(Row::Assistant {
                            id: format!("assistant-text-{}", message.seq).into(),
                            text: text.into(),
                        });
                    }
                    for part in &message.parts {
                        match part {
                            Part::ToolCall {
                                id, name, input, ..
                            } => {
                                let state = chat.calls.get(id);
                                let result = results.get(id.as_str());
                                let phase = match (result, state) {
                                    (Some((status, _)), _) => Phase::Done(*status),
                                    (None, Some(state)) => state.phase,
                                    (None, None) => Phase::Done(ResultStatus::Cancelled),
                                };
                                rows.push(Row::Tool {
                                    id: id.clone().into(),
                                    call: ToolCallView {
                                        label: call_label(name, input),
                                        detail: call_detail(
                                            name,
                                            input,
                                            result.map(|(_, output)| *output),
                                        ),
                                        phase,
                                        dangerous: state.is_some_and(|state| state.dangerous),
                                        asking: phase == Phase::Asking,
                                        always: state
                                            .map(|state| always_label(&state.call))
                                            .unwrap_or_default(),
                                        always_tip: state.and_then(|state| always_tip(&state.call)),
                                        command: name == "run_command",
                                        expanded: chat.expanded.contains(id),
                                    },
                                });
                            }
                            Part::Notice { kind } => rows.push(Row::Notice(match kind {
                                NoticeKind::Stopped => t!("assistant.turn.stopped"),
                                NoticeKind::Refused => t!("assistant.turn.refused"),
                                NoticeKind::Trimmed => t!("assistant.turn.trimmed"),
                            })),
                            _ => {}
                        }
                    }
                }
                Role::ToolResults => {}
            }
        }
        if let Some((text, _)) = &chat.pending {
            rows.push(Row::User(text.clone().into()));
        }
        if !chat.streaming.is_empty() {
            rows.push(Row::Streaming {
                text: chat.streaming.clone().into(),
            });
        } else if chat.busy && !chat.calls.values().any(|call| call.phase == Phase::Asking) {
            rows.push(Row::Thinking);
        }
        match &chat.status {
            Some(Status::Retrying { attempt, of }) => rows.push(Row::Status(
                t!("assistant.panel.retrying", attempt = attempt, of = of),
                false,
            )),
            Some(Status::Failed(text)) => rows.push(Row::Status(text.clone(), true)),
            Some(Status::Ended(end)) => {
                if let Some(text) = turn_end(*end) {
                    rows.push(Row::Status(text, false));
                }
                if matches!(end, TurnEnd::StepLimit | TurnEnd::MaxTokens) {
                    rows.push(Row::Continue);
                }
            }
            None => {}
        }
        rows
    }

    /// Go on after a limit stopped the turn.
    pub fn go_on(&mut self, cx: &mut Context<Self>) {
        self.send_text(t!("assistant.panel.continue_message").to_string(), cx);
    }

    /// The conversation's title, its history and 新建对话.
    fn render_header(&self, chat: &Chat, cx: &mut Context<Self>) -> impl IntoElement {
        let title = chat
            .conversation
            .as_ref()
            .map(|conversation| conversation.title.clone())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| t!("assistant.panel.new_title").to_string());
        let store = self.service.read(cx).store().cloned();
        let host = self
            .store
            .read(cx)
            .saved_host(chat.terminal.host)
            .map(|host| host.public_id.to_string());
        let history: Vec<(SharedString, String)> = match (&store, &host) {
            (Some(store), Some(host)) => store
                .conversations(Some(host))
                .unwrap_or_default()
                .into_iter()
                .map(|conversation| {
                    let title = if conversation.title.is_empty() {
                        t!("assistant.panel.untitled")
                    } else {
                        conversation.title.clone().into()
                    };
                    (title, conversation.public_id)
                })
                .collect(),
            _ => Vec::new(),
        };
        let dispatch = self.dispatch.clone();
        h_flex()
            .px_3()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                div()
                    .id("assistant-title")
                    .test_support()
                    .aria_label(SharedString::from(title.clone()))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(title),
            )
            .child({
                let dispatch = dispatch.clone();
                Button::new("assistant-history")
                    .xsmall()
                    .ghost()
                    .icon(Icon::new(CatalogIcon::RotateCcwClock))
                    .tooltip(t!("assistant.panel.history"))
                    .disabled(history.is_empty())
                    .dropdown_menu(move |mut menu, _, _| {
                        menu = menu
                            .action_context(dispatch.clone())
                            .max_h(px(420.))
                            .scrollable(true);
                        for (title, id) in &history {
                            menu = menu.menu(
                                title.clone(),
                                Box::new(OpenAssistantConversation(id.clone())),
                            );
                        }
                        menu
                    })
            })
            .child(
                Button::new("assistant-new")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Plus)
                    .tooltip(t!("assistant.panel.new"))
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_action(&NewAssistantConversation, window, cx)
                    }),
            )
    }

    /// The composer, as in Codex: one frame with the text, the model below
    /// it and a round 发送 (停止 while a turn goes on); under the frame,
    /// where commands run, how they are approved, and the tokens used.
    fn render_footer(&self, chat: &Chat, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = chat.busy || chat.starting;
        let empty = self.composer.read(cx).value().trim().is_empty();
        let dispatch = self.dispatch.clone();
        let usage = if chat.usage_unknown && chat.usage.total() == 0 {
            t!("assistant.panel.usage_unknown")
        } else {
            t!(
                "assistant.panel.usage",
                tokens = short_count(chat.usage.total())
            )
        };
        let detail: SharedString = match chat.context {
            Some((context, window)) => t!(
                "assistant.panel.usage_detail_context",
                input = short_count(chat.usage.input),
                output = short_count(chat.usage.output),
                cache_read = short_count(chat.usage.cache_read),
                cache_write = short_count(chat.usage.cache_write),
                context = short_count(context),
                window = short_count(window)
            ),
            None => t!(
                "assistant.panel.usage_detail",
                input = short_count(chat.usage.input),
                output = short_count(chat.usage.output),
                cache_read = short_count(chat.usage.cache_read),
                cache_write = short_count(chat.usage.cache_write)
            ),
        };
        let groups = model_groups(self.store.read(cx).ai_providers());
        let chosen = self.model_of(chat, cx);
        let chosen_name = chosen.as_ref().map(|(provider, model)| {
            t!(
                "assistant.model_of",
                provider = provider.name,
                model = model
            )
        });
        let chosen = chosen.map(|(provider, model)| AiModelChoice {
            provider: provider.public_id.to_string(),
            model: model.to_string(),
        });
        let approval = AssistantApproval::of(chat.approval);
        let run_in = AssistantRunIn::of(chat.run_in);
        let shell_only = chat.terminal.shell_only;

        let model = {
            let dispatch = dispatch.clone();
            let label: SharedString = chosen
                .as_ref()
                .map(|choice| choice.model.clone().into())
                .unwrap_or_else(|| t!("assistant.panel.no_provider_short"));
            InputGroupButton::new("assistant-model")
                .label(label)
                .dropdown_caret(true)
                .max_w(rems(12.))
                .when_some(chosen_name, |button, name| button.tooltip(name))
                .disabled(chat.busy)
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                    let menu = menu
                        .action_context(dispatch.clone())
                        .max_h(px(420.))
                        .scrollable(true);
                    model_items(menu, &groups, chosen.as_ref(), |choice| {
                        Box::new(UseAiModel(choice))
                    })
                    .separator()
                    .menu(t!("assistant.panel.add_provider"), Box::new(NewAiProvider))
                })
        };
        let send = {
            let dispatch = dispatch.clone();
            if busy {
                InputGroupButton::new("assistant-stop")
                    .primary()
                    .icon(Icon::new(CatalogIcon::Square))
                    .tooltip(t!("assistant.panel.stop"))
                    .rounded_full()
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_action(&StopAssistant, window, cx)
                    })
            } else {
                InputGroupButton::new("assistant-send")
                    .primary()
                    .icon(IconName::ArrowUp)
                    .tooltip(t!("assistant.panel.send"))
                    .disabled(empty)
                    .rounded_full()
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_action(&SendAssistantMessage, window, cx)
                    })
            }
        };
        // 批准方式, as in Codex: a chip whose menu says what each way asks.
        let approval_chip = {
            let dispatch = dispatch.clone();
            InputGroupButton::new("assistant-approval")
                .secondary()
                .icon(Icon::new(approval_icon(approval)))
                .label(approval.label())
                .tooltip(t!("settings.assistant.approval"))
                .rounded_full()
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                    menu = menu
                        .action_context(dispatch.clone())
                        .check_side(Side::Right)
                        .label(t!("assistant.panel.approval_title"));
                    for choice in AssistantApproval::ALL {
                        let choice = *choice;
                        menu = menu.menu_element_with_check(
                            choice == approval,
                            Box::new(SetConversationApproval(choice)),
                            move |_, cx| approval_item(choice, cx),
                        );
                    }
                    menu
                })
        };
        let composer = InputGroup::new("assistant-composer")
            .small()
            // A container a tier softer than the controls in it.
            .rounded(cx.theme().radius_tokens().xl)
            .input(
                Textarea::new(&self.composer)
                    .min_h_12()
                    .aria_label(t!("assistant.panel.placeholder")),
            )
            .addon(
                InputGroupAddon::new("assistant-composer-tools")
                    .align(InputGroupAddonAlignment::BlockEnd)
                    .child(approval_chip)
                    .child(div().flex_1())
                    .child(model)
                    .child(send),
            );

        let choices = h_flex()
            .gap_1()
            .min_w_0()
            .flex_wrap()
            .when(!shell_only, |row| {
                let dispatch = dispatch.clone();
                row.child(
                    Button::new("assistant-run-in")
                        .xsmall()
                        .ghost()
                        .label(run_in.label())
                        .dropdown_caret(true)
                        .tooltip(t!("settings.assistant.run_in"))
                        .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                            menu = menu.action_context(dispatch.clone());
                            for choice in AssistantRunIn::ALL {
                                menu = menu.menu_with_check(
                                    choice.label(),
                                    *choice == run_in,
                                    Box::new(SetConversationRunIn(*choice)),
                                );
                            }
                            menu
                        }),
                )
            });
        v_flex().px_3().pt_1().pb_2().gap_1().child(composer).child(
            h_flex().gap_2().justify_between().child(choices).child(
                div()
                    .id("assistant-usage")
                    .test_support()
                    .aria_label(usage.clone())
                    .flex_none()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
                    })
                    .child(usage),
            ),
        )
    }

    /// 启用 AI 助手, in 设置 › AI 助手.
    fn enabled(&self, cx: &App) -> bool {
        self.settings.read(cx).settings().assistant.enabled
    }

    /// Turned off: where it is set up and turned on.
    fn render_disabled(&self, cx: &mut Context<Self>) -> AnyElement {
        let dispatch = self.dispatch.clone();
        v_flex()
            .id("assistant-disabled")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .text_center()
                    .child(t!("assistant.panel.disabled")),
            )
            .child(
                Button::new("assistant-open-settings")
                    .small()
                    .primary()
                    .label(t!("assistant.panel.open_settings"))
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_action(&OpenAssistantSettings, window, cx)
                    }),
            )
            .into_any_element()
    }

    fn render_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        let dispatch = self.dispatch.clone();
        v_flex()
            .id("assistant-no-provider")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .text_center()
                    .child(t!("assistant.panel.no_provider")),
            )
            .child(
                Button::new("assistant-add-provider")
                    .small()
                    .primary()
                    .label(t!("assistant.panel.add_provider"))
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_action(&NewAiProvider, window, cx)
                    }),
            )
            .into_any_element()
    }
}

/// Stop the commands a chat runs, and answer the calls still asked about:
/// the turn is over.
fn stop_calls(chat: &mut Chat) {
    for state in chat.calls.values_mut() {
        if let Some(running) = state.running.as_mut() {
            running.stop();
        }
        if state.phase == Phase::Asking {
            state.phase = Phase::Done(ResultStatus::Cancelled);
        }
    }
}

/// 「执行，本对话不再问相似命令」: those of the same programs; a command
/// whose programs cannot be told is only itself again, and so is a file.
fn always_label(call: &ToolCall) -> SharedString {
    match call {
        ToolCall::RunCommand { command, .. } => match command_kinds(command) {
            Some(_) => t!("assistant.call.run_always_similar"),
            None => t!("assistant.call.run_always_command"),
        },
        ToolCall::ReadFile { .. } => t!("assistant.call.run_always_file"),
        // Never asked about.
        ToolCall::ReadTerminal { .. } => SharedString::default(),
    }
}

/// The programs 「相似命令」 stands for, said when its button is pointed at.
fn always_tip(call: &ToolCall) -> Option<SharedString> {
    let ToolCall::RunCommand { command, .. } = call else {
        return None;
    };
    command_kinds(command).map(|kinds| {
        t!(
            "assistant.call.run_always_similar_tip",
            // Program names read the same in every language.
            kinds = kinds.join(" / ")
        )
    })
}

/// The mark of a way of approving, in its chip and its menu.
fn approval_icon(approval: AssistantApproval) -> CatalogIcon {
    match approval {
        AssistantApproval::Ask => CatalogIcon::Hand,
        AssistantApproval::ApproveForMe => CatalogIcon::ShieldCheck,
        AssistantApproval::FullAccess => CatalogIcon::ShieldAlert,
    }
}

/// A way of approving in the chip's menu: its mark, its name, and what it
/// asks about. 完全访问权限 is in the danger color: nothing is asked.
fn approval_item(choice: AssistantApproval, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let full = choice == AssistantApproval::FullAccess;
    let (title, detail) = if full {
        (theme.danger, theme.danger)
    } else {
        (theme.foreground, theme.muted_foreground)
    };
    // gpui-kit names no custom menu item: the content carries the name.
    h_flex()
        .id(SharedString::from(format!(
            "assistant-approval-{}",
            choice.key()
        )))
        .test_support()
        .aria_label(choice.label())
        // A width of its own, for the description to wrap in: the menu's
        // row takes the width of what it holds and would run one long line
        // past its edge.
        .w(rems(16.))
        .flex_none()
        .items_start()
        .gap_2()
        .py_1()
        .child(
            div()
                .flex_none()
                .pt_1()
                .child(Icon::new(approval_icon(choice)).small().text_color(title)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().text_color(title).child(choice.label()))
                .child(
                    div()
                        .text_xs()
                        .text_color(detail)
                        .child(choice.description()),
                ),
        )
        .into_any_element()
}

/// 1234 as `1.2K`, 2_500_000 as `2.5M`.
fn short_count(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{:.1}K", count as f64 / 1_000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// What a call shows opened: the command in full as it runs (`$ …`) or
/// the file's path, then what came back once it has. A call still asked
/// about shows what it would run.
fn call_detail(
    name: &str,
    input: &serde_json::Value,
    output: Option<&str>,
) -> Option<SharedString> {
    let text = |field: &str| input.get(field).and_then(serde_json::Value::as_str);
    let head = match name {
        "run_command" => Some(format!(
            "$ {}",
            with_cwd(text("command").unwrap_or_default(), text("cwd"))
        )),
        "read_file" => text("path").map(str::to_owned),
        _ => None,
    };
    let output = output.filter(|output| !output.is_empty());
    match (head, output) {
        (Some(head), Some(output)) => Some(format!("{head}\n{output}").into()),
        (Some(head), None) => Some(head.into()),
        (None, output) => output.map(|output| output.to_owned().into()),
    }
}

/// What a call is, in a line.
fn call_label(name: &str, input: &serde_json::Value) -> SharedString {
    let text = |field: &str| {
        input
            .get(field)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    match name {
        "read_terminal" => t!(
            "assistant.call.read_terminal",
            lines = input
                .get("lines")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(200)
        ),
        "run_command" => text("command").into(),
        "read_file" => t!("assistant.call.read_file", path = text("path")),
        other => other.to_owned().into(),
    }
}

impl Render for AssistantPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(id) = self.current() else {
            return div().into_any_element();
        };
        if !self.enabled(cx) {
            return self.render_disabled(cx);
        }
        if self.store.read(cx).ai_providers().is_empty() {
            return self.render_empty(cx);
        }
        let Some(chat) = self.chats.get(&id) else {
            return div().into_any_element();
        };
        let rows = self.rows(chat);
        let scroller = chat.scroller.clone();
        let count = rows.len();
        scroller.update(cx, |state, cx| {
            let known = state.item_count();
            if count < known {
                state.reset(count, cx);
            } else if count > known {
                state.append(count - known, cx);
            } else if count > 0 {
                // The last row grows while it streams.
                state.remeasure_items(count - 1..count, cx);
            }
        });
        let header = self.render_header(chat, cx).into_any_element();
        let footer = self.render_footer(chat, cx).into_any_element();
        let dispatch = self.dispatch.clone();
        let theme = cx.theme().clone();
        let list = MessageScroller::new(("assistant-messages", id.0), scroller, move |ix, _, _| {
            render_row(rows.get(ix).cloned(), &dispatch, &theme)
        })
        .with_jump_button_label(t!("assistant.panel.jump"))
        // The rows space themselves: a run of calls sits close, messages
        // further apart. The scroller's own space under each row is wide.
        .with_row_style(StyleRefinement::default().pb_0())
        .flex_1()
        .min_h_0();
        v_flex()
            .id(("assistant", id.0))
            .test_support()
            .size_full()
            .child(header)
            .child(list)
            .child(footer)
            .into_any_element()
    }
}

fn render_row(
    row: Option<Row>,
    dispatch: &FocusHandle,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let Some(row) = row else {
        return div().into_any_element();
    };
    let muted = theme.muted_foreground;
    // A message keeps 1rem from the next; a line between them, less.
    let message = || div().py_2().w_full();
    let block = || div().py_1().w_full();
    match row {
        Row::User(text) => message()
            .child(
                h_flex().justify_end().child(
                    div()
                        .max_w(relative(0.85))
                        .px_3()
                        .py_1p5()
                        .rounded(theme.radius_lg)
                        .bg(theme.secondary)
                        .text_sm()
                        .child(text),
                ),
            )
            .into_any_element(),
        Row::Assistant { id, text } => message()
            .text_sm()
            .child(markdown(id, text, dispatch))
            .into_any_element(),
        // A view of its own: the finished message that takes its place
        // starts afresh.
        Row::Streaming { text } => message()
            .text_sm()
            .child(markdown("assistant-streaming".into(), text, dispatch))
            .into_any_element(),
        Row::Thinking => block()
            .child(ShimmerText::new(t!("assistant.panel.thinking")).id("assistant-thinking"))
            .into_any_element(),
        Row::Notice(text) => block()
            .text_xs()
            .text_color(muted)
            .child(text)
            .into_any_element(),
        Row::Status(text, failure) => block()
            .child(
                div()
                    .id("assistant-status")
                    .test_support()
                    .aria_label(text.clone())
                    .text_xs()
                    .text_color(if failure { theme.danger } else { muted })
                    .child(text),
            )
            .into_any_element(),
        Row::Continue => {
            let dispatch = dispatch.clone();
            block()
                .child(
                    Button::new("assistant-continue")
                        .xsmall()
                        .outline()
                        .label(t!("assistant.panel.continue"))
                        .on_click(move |_, window, cx| {
                            dispatch.dispatch_action(&crate::app::ContinueAssistant, window, cx)
                        }),
                )
                .into_any_element()
        }
        Row::Tool { id, call } => render_call(id, call, dispatch, theme),
    }
}

fn markdown(id: SharedString, text: SharedString, dispatch: &FocusHandle) -> impl IntoElement {
    let dispatch = dispatch.clone();
    TextView::markdown(ElementId::Name(id), text)
        .selectable(true)
        .stream_fade(true)
        // A model told by what it read to show `![](https://…?q=<secret>)`
        // would have the picture's server read the secret: no pictures.
        .image_source(|_| ImageSource::Resource(Resource::Embedded("".into())))
        .on_link_click(move |url, _, window, cx| {
            dispatch.dispatch_action(&OpenAssistantLink(url.to_string()), window, cx)
        })
        .code_block_actions(|block, _, _| {
            // gpui-kit scopes the id to its code block.
            Clipboard::new("assistant-copy")
                .value(block.code().to_string())
                .tooltip(t!("assistant.panel.copy"))
        })
}

fn render_call(
    id: SharedString,
    call: ToolCallView,
    dispatch: &FocusHandle,
    theme: &gpui_kit::component::Theme,
) -> AnyElement {
    let muted = theme.muted_foreground;
    let (status, color) = match call.phase {
        Phase::Asking => (t!("assistant.call.asking"), theme.warning),
        Phase::Running => (t!("assistant.call.running"), muted),
        Phase::Done(ResultStatus::Ok) => (t!("assistant.call.done"), theme.success),
        Phase::Done(ResultStatus::Error) => (t!("assistant.call.failed"), theme.danger),
        Phase::Done(ResultStatus::Denied) => (t!("assistant.call.denied"), muted),
        Phase::Done(ResultStatus::Cancelled) => (t!("assistant.call.cancelled"), muted),
    };
    let chip = {
        let dispatch = dispatch.clone();
        let id = id.clone();
        h_flex()
            .id(SharedString::from(format!("assistant-call-{id}")))
            .test_support()
            .aria_label(call.label.clone())
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(if call.asking && call.dangerous {
                theme.danger
            } else {
                theme.border
            })
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                dispatch.dispatch_action(&ToggleAssistantCallDetails(id.to_string()), window, cx)
            })
            .child(
                Icon::new(if call.command {
                    CatalogIcon::SquareTerminal
                } else {
                    CatalogIcon::ScrollText
                })
                .xsmall()
                .text_color(muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .child(call.label.clone()),
            )
            .child(div().text_xs().text_color(color).child(status))
    };
    // A run of calls stays together: 0.25rem between one and the next.
    let mut column = v_flex().py_0p5().gap_1().w_full().child(chip);
    if call.expanded
        && let Some(detail) = call.detail.clone()
    {
        column = column.child(
            div()
                .id(SharedString::from(format!("assistant-call-detail-{id}")))
                .test_support()
                .aria_label(SharedString::from(
                    detail.lines().next().unwrap_or_default().to_owned(),
                ))
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.muted)
                .font_family(theme.mono_font_family.clone())
                .text_xs()
                // Output reads as in a terminal, not spaced as prose.
                .line_height(rems(1.))
                .max_h(px(240.))
                .overflow_y_scroll()
                .child(detail),
        );
    }
    if call.asking {
        let (run, always, insert, deny) = (
            dispatch.clone(),
            dispatch.clone(),
            dispatch.clone(),
            dispatch.clone(),
        );
        let (run_id, always_id, insert_id, deny_id) = (
            id.to_string(),
            id.to_string(),
            id.to_string(),
            id.to_string(),
        );
        column = column.child(
            v_flex()
                .gap_1()
                .when(call.dangerous, |card| {
                    card.child(
                        div()
                            .text_xs()
                            .text_color(theme.danger)
                            .child(t!("assistant.call.dangerous")),
                    )
                })
                .child(
                    h_flex()
                        .gap_1()
                        .flex_wrap()
                        .child(
                            Button::new(SharedString::from(format!("assistant-run-{id}")))
                                .xsmall()
                                .map(|button| {
                                    if call.dangerous {
                                        button.danger()
                                    } else {
                                        button.primary()
                                    }
                                })
                                .label(t!("assistant.call.run"))
                                .on_click(move |_, window, cx| {
                                    run.dispatch_action(
                                        &RunAssistantCall {
                                            id: run_id.clone(),
                                            remember: false,
                                        },
                                        window,
                                        cx,
                                    )
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("assistant-run-always-{id}")))
                                .xsmall()
                                .outline()
                                .label(call.always.clone())
                                .when_some(call.always_tip.clone(), |button, tip| {
                                    button.tooltip(tip)
                                })
                                .on_click(move |_, window, cx| {
                                    always.dispatch_action(
                                        &RunAssistantCall {
                                            id: always_id.clone(),
                                            remember: true,
                                        },
                                        window,
                                        cx,
                                    )
                                }),
                        )
                        .when(call.command, |row| {
                            row.child(
                                Button::new(SharedString::from(format!("assistant-insert-{id}")))
                                    .xsmall()
                                    .outline()
                                    .label(t!("assistant.call.insert"))
                                    .on_click(move |_, window, cx| {
                                        insert.dispatch_action(
                                            &InsertAssistantCall(insert_id.clone()),
                                            window,
                                            cx,
                                        )
                                    }),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("assistant-deny-{id}")))
                                .xsmall()
                                .ghost()
                                .label(t!("assistant.call.deny"))
                                .on_click(move |_, window, cx| {
                                    deny.dispatch_action(
                                        &DenyAssistantCall(deny_id.clone()),
                                        window,
                                        cx,
                                    )
                                }),
                        ),
                ),
        );
    }
    column.into_any_element()
}
