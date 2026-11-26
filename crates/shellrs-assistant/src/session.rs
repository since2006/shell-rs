//! A conversation with a model: the loop that asks, streams the answer,
//! hands the tools' calls to the app, waits for their answers and asks
//! again, until the model is done, the user stops it, or a limit is met.
//!
//! The app talks to it with commands ([`Session::send`],
//! [`Session::answer`], …) and reads what happens as events, pulled with
//! [`Session::next_event`] or [`Session::try_event`]: the app never runs
//! the loop, and the loop never waits on the app except for a tool's
//! answer.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use tokio::sync::{Notify, mpsc};

use crate::config::{Protocol, ProviderConfig};
use crate::danger::is_dangerous;
use crate::env::{SharedEnv, random_id};
use crate::error::{ProviderError, ProviderErrorKind};
use crate::http::{HttpRequest, Transport, TransportError};
use crate::message::{
    HostContext, Message, NoticeKind, Part, Raw, ResultStatus, Role, StopReason, Usage,
};
use crate::prompt::system_prompt;
use crate::provider::{self, Delta, Request};
use crate::remember::Remembered;
use crate::sse::SseParser;
use crate::store::{ConversationSummary, Store, TurnSaved};
use crate::tools::{
    Capabilities, ToolAnswer, ToolCall, ToolSpec, cancelled_call, invalid_call, tool_specs,
    write_answer,
};

/// The most requests one message of the user's leads to before the
/// assistant stops and asks whether to go on.
const MOST_REQUESTS: u32 = 30;
/// The most commands one message of the user's leads to before the same.
const MOST_COMMANDS: u32 = 15;
/// How many times a request that may pass later is sent again.
const MOST_RETRIES: u32 = 4;
/// The longest wait before sending again, whatever the service asks.
const LONGEST_WAIT: Duration = Duration::from_secs(60);
/// How much of an error answer is read.
const ERROR_LIMIT: usize = 64 * 1024;
/// How much of a list of models is read: OpenRouter's describes hundreds.
const MODELS_LIMIT: usize = 16 << 20;

/// How the user lets the assistant run commands and read files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Approval {
    /// Every call is asked about.
    Ask,
    /// Calls run unasked, but those marked dangerous are asked about.
    ApproveForMe,
    /// Nothing is asked about.
    FullAccess,
}

impl Approval {
    pub const ALL: [Self; 3] = [Self::Ask, Self::ApproveForMe, Self::FullAccess];

    /// How it is written in settings and in a conversation. Never changes.
    pub fn key(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::ApproveForMe => "approve_for_me",
            Self::FullAccess => "full_access",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|approval| approval.key() == key)
    }
}

/// A conversation to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewConversation {
    pub host_public_id: Option<String>,
    pub host_label: String,
}

/// Which conversation a session is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationChoice {
    /// One saved earlier, by its public id.
    Existing(String),
    /// A new one, saved once it has a turn.
    New(NewConversation),
}

/// What a session needs.
pub struct SessionSetup {
    pub provider: ProviderConfig,
    /// The model service's record, to say which one a conversation last
    /// used.
    pub provider_public_id: Option<String>,
    pub transport: Arc<dyn Transport>,
    pub env: SharedEnv,
    pub store: Store,
    pub conversation: ConversationChoice,
    /// The language to answer in, by its English name.
    pub language: String,
    pub capabilities: Capabilities,
    /// The approval from the settings, for a conversation that has none of
    /// its own.
    pub approval: Approval,
}

/// How a turn ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnEnd {
    Done,
    /// The model declined to go on.
    Refused,
    /// The user stopped it.
    Stopped,
    /// It took as many requests or commands as one message may: the user
    /// says whether to go on.
    StepLimit,
    /// The answer reached its most tokens.
    MaxTokens,
    /// The conversation is too long to go on with.
    ContextFull,
}

/// What went wrong in a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionFailure {
    Provider(ProviderError),
    /// The conversations' database failed; in English, for the log.
    Store(String),
    /// The conversation asked for is not saved.
    NotFound,
}

/// What happens in a session, for the app to show.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionEvent {
    /// The conversation, as saved, once the session starts.
    Opened {
        conversation: ConversationSummary,
        messages: Vec<Message>,
        usage: Usage,
    },
    /// A message took its place: the user's, the model's finished answer,
    /// or the tools' answers.
    MessageAdded(Message),
    /// A request goes out: what streamed of an earlier try is void.
    RequestStarted,
    TextDelta(String),
    ThinkingDelta(String),
    /// The model called a tool: the app runs it, after asking the user
    /// when `needs_approval`, and answers with [`Session::answer`].
    ToolCall {
        id: String,
        call: ToolCall,
        needs_approval: bool,
        dangerous: bool,
    },
    ToolFinished {
        id: String,
        status: ResultStatus,
    },
    /// A request failed in a way that may pass, and goes again.
    Retrying {
        attempt: u32,
        of: u32,
        after: Duration,
        error: ProviderError,
    },
    /// What the conversation's requests took in all, and how much of the
    /// model's room the last one filled.
    Usage {
        total: Usage,
        context: u64,
        window: u64,
    },
    /// Older tool output is no longer sent, to keep room.
    Trimmed,
    ApprovalChanged(Approval),
    TurnEnded(TurnEnd),
    Failed(SessionFailure),
}

enum Command {
    Send {
        text: String,
        context: Option<HostContext>,
    },
    Answer {
        id: String,
        answer: ToolAnswer,
        remember: bool,
    },
    Stop,
    SetApproval(Approval),
}

/// The events not read yet. Text deltas in a row are joined: an app that
/// reads slowly loses nothing and gets fewer of them.
struct EventQueue {
    queue: Mutex<VecDeque<SessionEvent>>,
    notify: Notify,
    closed: AtomicBool,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<SessionEvent>> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push(&self, event: SessionEvent) {
        {
            let mut queue = self.lock();
            match (queue.back_mut(), event) {
                (Some(SessionEvent::TextDelta(last)), SessionEvent::TextDelta(more)) => {
                    last.push_str(&more)
                }
                (Some(SessionEvent::ThinkingDelta(last)), SessionEvent::ThinkingDelta(more)) => {
                    last.push_str(&more);
                }
                (_, event) => queue.push_back(event),
            }
        }
        self.notify.notify_one();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    fn try_pop(&self) -> Option<SessionEvent> {
        self.lock().pop_front()
    }
}

/// Answers a session's tool calls from any thread.
#[derive(Clone)]
pub struct Answerer(mpsc::UnboundedSender<Command>);

impl Answerer {
    pub fn answer(&self, id: impl Into<String>, answer: ToolAnswer, remember: bool) {
        let _ = self.0.send(Command::Answer {
            id: id.into(),
            answer,
            remember,
        });
    }
}

/// A conversation going on, on a runtime of the app's. Dropping it stops
/// the loop and any request it is waiting on.
pub struct Session {
    commands: mpsc::UnboundedSender<Command>,
    events: Arc<EventQueue>,
    task: tokio::task::AbortHandle,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Session {
    pub fn start(runtime: &tokio::runtime::Handle, setup: SessionSetup) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let events = Arc::new(EventQueue::new());
        let task = runtime
            .spawn(run(setup, receiver, events.clone()))
            .abort_handle();
        Self {
            commands,
            events,
            task,
        }
    }

    /// Ask the model, with what is known of the host now. Ignored while a
    /// turn goes on.
    pub fn send(&self, text: impl Into<String>, context: Option<HostContext>) {
        let _ = self.commands.send(Command::Send {
            text: text.into(),
            context,
        });
    }

    /// Answer a tool call. With `remember`, the calls of its kind (see
    /// `Remembered`) are not asked about again in this conversation.
    pub fn answer(&self, id: impl Into<String>, answer: ToolAnswer, remember: bool) {
        let _ = self.commands.send(Command::Answer {
            id: id.into(),
            answer,
            remember,
        });
    }

    /// What answers tool calls from elsewhere: a command that runs on
    /// another thread answers when it ends.
    pub fn answerer(&self) -> Answerer {
        Answerer(self.commands.clone())
    }

    /// Stop the turn going on: its request, and the calls not answered.
    /// The app stops the tools it runs itself.
    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    pub fn set_approval(&self, approval: Approval) {
        let _ = self.commands.send(Command::SetApproval(approval));
    }

    /// The next event, when there is one now.
    pub fn try_event(&self) -> Option<SessionEvent> {
        self.events.try_pop()
    }

    /// The next event, waiting for it; `None` once the session ended and
    /// every event was read.
    pub async fn next_event(&self) -> Option<SessionEvent> {
        loop {
            let notified = self.events.notify.notified();
            if let Some(event) = self.events.try_pop() {
                return Some(event);
            }
            if self.events.closed.load(Ordering::SeqCst) {
                return self.events.try_pop();
            }
            notified.await;
        }
    }
}

async fn run(
    setup: SessionSetup,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: Arc<EventQueue>,
) {
    let mut conversation = match Conversation::open(setup) {
        Ok(conversation) => conversation,
        Err(failure) => {
            events.push(SessionEvent::Failed(failure));
            events.close();
            return;
        }
    };
    events.push(SessionEvent::Opened {
        conversation: conversation.summary.clone(),
        messages: conversation.messages.clone(),
        usage: conversation.total_usage(),
    });
    while let Some(command) = commands.recv().await {
        match command {
            Command::Send { text, context } => {
                conversation.ask(text, context, &events);
                conversation.turn(&mut commands, &events).await;
            }
            Command::SetApproval(approval) => conversation.set_approval(approval, &events),
            Command::Answer { .. } | Command::Stop => {}
        }
    }
    events.close();
}

/// What a request came to.
enum Requested {
    Answered(Answer),
    /// The user stopped it: the text that had come, if any.
    Stopped(Vec<Part>),
    Failed(ProviderError),
}

struct Answer {
    parts: Vec<Part>,
    raw: Option<Raw>,
    stop: StopReason,
    usage: Option<Usage>,
}

struct Conversation {
    provider: ProviderConfig,
    provider_public_id: Option<String>,
    transport: Arc<dyn Transport>,
    env: SharedEnv,
    store: Store,
    capabilities: Capabilities,
    system: String,
    tools: Vec<ToolSpec>,
    summary: ConversationSummary,
    /// Whether the store has it yet.
    created: bool,
    messages: Vec<Message>,
    /// How many of the messages are saved.
    saved: usize,
    approval: Approval,
    /// The calls the user said not to ask about again, and their kinds.
    remembered: Remembered,
    last_context: Option<HostContext>,
}

impl Conversation {
    fn open(setup: SessionSetup) -> Result<Self, SessionFailure> {
        let store_failure =
            |error: crate::error::StoreError| SessionFailure::Store(error.to_string());
        let (summary, mut messages, created) = match &setup.conversation {
            ConversationChoice::Existing(public_id) => {
                let summary = setup
                    .store
                    .conversation(public_id)
                    .map_err(store_failure)?
                    .ok_or(SessionFailure::NotFound)?;
                let messages = setup.store.messages(public_id).map_err(store_failure)?;
                (summary, messages, true)
            }
            ConversationChoice::New(new) => {
                let now = setup.env.now_ms();
                let summary = ConversationSummary {
                    public_id: random_id(setup.env.as_ref()),
                    host_public_id: new.host_public_id.clone(),
                    host_label: new.host_label.clone(),
                    title: String::new(),
                    title_by_user: false,
                    provider_public_id: setup.provider_public_id.clone(),
                    model: setup.provider.model.trim().to_owned(),
                    approval: None,
                    run_in: None,
                    created_ms: now,
                    updated_ms: now,
                    elided_before: None,
                };
                (summary, Vec::new(), false)
            }
        };
        let saved = messages.len();
        // Calls a turn left unanswered (ShellRS quit during it) get an
        // answer, or no protocol takes the conversation again.
        if let Some(last) = messages.last()
            && last.role == Role::Assistant
            && last.tool_calls().next().is_some()
        {
            let parts = last
                .tool_calls()
                .map(|(id, _, _)| result_part(id, cancelled_call()))
                .collect();
            let seq = messages.len() as i64;
            messages.push(Message {
                seq,
                role: Role::ToolResults,
                parts,
                raw: None,
                stop: None,
                usage: None,
                created_ms: setup.env.now_ms(),
            });
        }
        let last_context = messages.iter().rev().find_map(|message| {
            message.parts.iter().find_map(|part| match part {
                Part::Context { context } => Some(context.clone()),
                _ => None,
            })
        });
        Ok(Self {
            approval: summary.approval.unwrap_or(setup.approval),
            system: system_prompt(&setup.language, setup.capabilities),
            tools: tool_specs(setup.capabilities),
            capabilities: setup.capabilities,
            provider: setup.provider,
            provider_public_id: setup.provider_public_id,
            transport: setup.transport,
            env: setup.env,
            store: setup.store,
            summary,
            created,
            messages,
            saved,
            remembered: Remembered::default(),
            last_context,
        })
    }

    fn total_usage(&self) -> Usage {
        let mut total = Usage::default();
        for usage in self.messages.iter().filter_map(|message| message.usage) {
            total.add(&usage);
        }
        total
    }

    fn window(&self) -> u64 {
        self.provider.context_window()
    }

    fn push(
        &mut self,
        role: Role,
        parts: Vec<Part>,
        raw: Option<Raw>,
        stop: Option<StopReason>,
        usage: Option<Usage>,
        events: &EventQueue,
    ) {
        let message = Message {
            seq: self.messages.len() as i64,
            role,
            parts,
            raw,
            stop,
            usage,
            created_ms: self.env.now_ms(),
        };
        events.push(SessionEvent::MessageAdded(message.clone()));
        self.messages.push(message);
    }

    fn ask(&mut self, text: String, context: Option<HostContext>, events: &EventQueue) {
        let mut parts = Vec::new();
        if let Some(context) = context
            && self.last_context.as_ref() != Some(&context)
        {
            parts.push(Part::Context {
                context: context.clone(),
            });
            self.last_context = Some(context);
        }
        parts.push(Part::Text { text });
        self.push(Role::User, parts, None, None, None, events);
    }

    fn set_approval(&mut self, approval: Approval, events: &EventQueue) {
        self.approval = approval;
        self.summary.approval = Some(approval);
        if self.created
            && let Err(error) = self
                .store
                .set_approval(&self.summary.public_id, Some(approval))
        {
            events.push(SessionEvent::Failed(SessionFailure::Store(
                error.to_string(),
            )));
        }
        events.push(SessionEvent::ApprovalChanged(approval));
    }

    async fn turn(&mut self, commands: &mut mpsc::UnboundedReceiver<Command>, events: &EventQueue) {
        let mut requests = 0;
        let mut commands_run = 0;
        let end = loop {
            if let Some(end) = self.make_room(events) {
                break end;
            }
            if requests == MOST_REQUESTS {
                break TurnEnd::StepLimit;
            }
            requests += 1;
            match self.request(commands, events).await {
                Requested::Stopped(mut parts) => {
                    parts.push(Part::Notice {
                        kind: NoticeKind::Stopped,
                    });
                    self.push(Role::Assistant, parts, None, None, None, events);
                    break TurnEnd::Stopped;
                }
                Requested::Failed(error) => {
                    self.save(events);
                    events.push(SessionEvent::Failed(SessionFailure::Provider(error)));
                    return;
                }
                Requested::Answered(mut answer) => {
                    if answer.stop == StopReason::MaxTokens {
                        // A call cut off may be cut anywhere: none of its
                        // calls run, and its content can't go back as it is.
                        answer
                            .parts
                            .retain(|part| !matches!(part, Part::ToolCall { .. }));
                        answer.raw = None;
                    }
                    if answer.stop == StopReason::Refusal {
                        answer.parts.push(Part::Notice {
                            kind: NoticeKind::Refused,
                        });
                    }
                    self.push(
                        Role::Assistant,
                        answer.parts,
                        answer.raw,
                        Some(answer.stop),
                        answer.usage,
                        events,
                    );
                    if let Some(usage) = answer.usage {
                        events.push(SessionEvent::Usage {
                            total: self.total_usage(),
                            context: usage.context() + usage.output,
                            window: self.window(),
                        });
                    }
                    let calls: Vec<(String, String, serde_json::Value)> = self
                        .messages
                        .last()
                        .map(|message| {
                            message
                                .tool_calls()
                                .map(|(id, name, input)| {
                                    (id.to_owned(), name.to_owned(), input.clone())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if !calls.is_empty() {
                        let (results, stopped, ran) = self.run_tools(calls, commands, events).await;
                        commands_run += ran;
                        self.push(Role::ToolResults, results, None, None, None, events);
                        if stopped {
                            break TurnEnd::Stopped;
                        }
                        if commands_run >= MOST_COMMANDS {
                            break TurnEnd::StepLimit;
                        }
                        continue;
                    }
                    match answer.stop {
                        StopReason::PauseTurn => continue,
                        StopReason::MaxTokens => break TurnEnd::MaxTokens,
                        StopReason::Refusal => break TurnEnd::Refused,
                        _ => break TurnEnd::Done,
                    }
                }
            }
        };
        self.save(events);
        events.push(SessionEvent::TurnEnded(end));
    }

    /// Keep the conversation within the model's room: past three quarters
    /// of it, tool output before the last two questions is no longer sent;
    /// past nine tenths, the conversation cannot go on.
    fn make_room(&mut self, events: &EventQueue) -> Option<TurnEnd> {
        let usage = self
            .messages
            .iter()
            .rev()
            .find_map(|message| message.usage)?;
        let used = usage.context() + usage.output;
        let window = self.window();
        if used * 10 >= window * 9 {
            return Some(TurnEnd::ContextFull);
        }
        if used * 4 >= window * 3 {
            let questions: Vec<i64> = self
                .messages
                .iter()
                .filter(|message| message.role == Role::User)
                .map(|message| message.seq)
                .collect();
            if questions.len() >= 3 {
                let before = questions[questions.len() - 2];
                if self
                    .summary
                    .elided_before
                    .is_none_or(|elided| elided < before)
                {
                    self.summary.elided_before = Some(before);
                    events.push(SessionEvent::Trimmed);
                }
            }
        }
        None
    }

    async fn request(
        &mut self,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        events: &EventQueue,
    ) -> Requested {
        let http = provider::build(&Request {
            config: &self.provider,
            system: &self.system,
            tools: &self.tools,
            messages: &self.messages,
            elided_before: self.summary.elided_before,
        });
        let mut attempt = 0;
        loop {
            events.push(SessionEvent::RequestStarted);
            let mut assembler = Assembler::new(self.messages.len() as i64);
            let outcome = {
                let streaming = stream(
                    self.transport.clone(),
                    http.clone(),
                    self.provider.protocol,
                    &mut assembler,
                    events,
                );
                tokio::pin!(streaming);
                loop {
                    tokio::select! {
                        result = &mut streaming => break Some(result),
                        command = commands.recv() => match command {
                            Some(Command::Stop) | None => break None,
                            Some(Command::SetApproval(approval)) => self.set_approval(approval, events),
                            Some(_) => {}
                        },
                    }
                }
            };
            match outcome {
                None => return Requested::Stopped(assembler.partial()),
                Some(Ok(())) => return Requested::Answered(assembler.finish(&self.provider)),
                Some(Err(error)) if error.is_retryable() && attempt < MOST_RETRIES => {
                    let after = error
                        .retry_after
                        .filter(|after| *after <= LONGEST_WAIT)
                        .unwrap_or_else(|| self.backoff(attempt));
                    attempt += 1;
                    events.push(SessionEvent::Retrying {
                        attempt,
                        of: MOST_RETRIES,
                        after,
                        error,
                    });
                    if self.wait_or_stop(after, commands, events).await {
                        return Requested::Stopped(Vec::new());
                    }
                }
                Some(Err(error)) => return Requested::Failed(error),
            }
        }
    }

    /// 1, 2, 4, 8 seconds, give or take a fifth.
    fn backoff(&self, attempt: u32) -> Duration {
        let base = 1000u64 << attempt.min(5);
        let jitter = (self.env.random_u64() % 401) as i64 - 200;
        Duration::from_millis((base as i64 + base as i64 * jitter / 1000) as u64)
    }

    /// Wait, or stop when told: whether told.
    async fn wait_or_stop(
        &mut self,
        wait: Duration,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        events: &EventQueue,
    ) -> bool {
        let sleep = tokio::time::sleep(wait);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => return false,
                command = commands.recv() => match command {
                    Some(Command::Stop) | None => return true,
                    Some(Command::SetApproval(approval)) => self.set_approval(approval, events),
                    Some(_) => {}
                },
            }
        }
    }

    /// Hand the calls to the app and wait for its answers: what the tools
    /// answered, whether the user stopped the turn, and how many commands
    /// ran.
    async fn run_tools(
        &mut self,
        calls: Vec<(String, String, serde_json::Value)>,
        commands: &mut mpsc::UnboundedReceiver<Command>,
        events: &EventQueue,
    ) -> (Vec<Part>, bool, u32) {
        let mut written = vec![None; calls.len()];
        let mut pending: HashMap<String, (usize, ToolCall)> = HashMap::new();
        for (index, (id, name, input)) in calls.iter().enumerate() {
            match ToolCall::parse(name, input, self.capabilities) {
                Err(reason) => {
                    let answer = invalid_call(&reason);
                    events.push(SessionEvent::ToolFinished {
                        id: id.clone(),
                        status: answer.status,
                    });
                    written[index] = Some(answer);
                }
                Ok(call) => {
                    let (needs_approval, dangerous) = self.policy(&call);
                    events.push(SessionEvent::ToolCall {
                        id: id.clone(),
                        call: call.clone(),
                        needs_approval,
                        dangerous,
                    });
                    pending.insert(id.clone(), (index, call));
                }
            }
        }
        let mut stopped = false;
        let mut ran = 0;
        while !pending.is_empty() {
            match commands.recv().await {
                Some(Command::Answer {
                    id,
                    answer,
                    remember,
                }) => {
                    let Some((index, call)) = pending.remove(&id) else {
                        continue;
                    };
                    if remember {
                        self.remembered.remember(&call);
                    }
                    if matches!(answer, ToolAnswer::Command(_)) {
                        ran += 1;
                    }
                    let answer = write_answer(&answer);
                    events.push(SessionEvent::ToolFinished {
                        id,
                        status: answer.status,
                    });
                    written[index] = Some(answer);
                }
                Some(Command::SetApproval(approval)) => self.set_approval(approval, events),
                Some(Command::Stop) | None => {
                    stopped = true;
                    for (index, _) in pending.values() {
                        written[*index] = Some(cancelled_call());
                    }
                    break;
                }
                Some(Command::Send { .. }) => {}
            }
        }
        let parts = calls
            .iter()
            .zip(written)
            .map(|((id, _, _), answer)| result_part(id, answer.unwrap_or_else(cancelled_call)))
            .collect();
        (parts, stopped, ran)
    }

    /// Whether a call is asked about, and whether it is marked dangerous.
    fn policy(&self, call: &ToolCall) -> (bool, bool) {
        let dangerous = match call {
            ToolCall::ReadTerminal { .. } => return (false, false),
            ToolCall::RunCommand { command, .. } => is_dangerous(command),
            ToolCall::ReadFile { path } => is_sensitive(path),
        };
        if self.remembered.covers(call) {
            return (false, dangerous);
        }
        let needs = match self.approval {
            Approval::Ask => true,
            Approval::ApproveForMe => dangerous,
            Approval::FullAccess => false,
        };
        (needs, dangerous)
    }

    /// Save what the store has not got yet. A failure is said and the
    /// messages are tried again with the next turn.
    fn save(&mut self, events: &EventQueue) {
        if self.saved == self.messages.len() {
            return;
        }
        let now = self.env.now_ms();
        let title = (self.summary.title.is_empty() && !self.summary.title_by_user)
            .then(|| title_of(&self.messages))
            .flatten();
        if !self.created {
            self.summary.title = title.clone().unwrap_or_default();
            if let Err(error) = self.store.create(&self.summary) {
                events.push(SessionEvent::Failed(SessionFailure::Store(
                    error.to_string(),
                )));
                return;
            }
            self.created = true;
        }
        let saved = TurnSaved {
            updated_ms: now,
            title: title.as_deref(),
            provider_public_id: self.provider_public_id.as_deref(),
            model: self.provider.model.trim(),
            elided_before: self.summary.elided_before,
        };
        match self
            .store
            .append(&self.summary.public_id, &self.messages[self.saved..], saved)
        {
            Ok(()) => {
                self.saved = self.messages.len();
                self.summary.updated_ms = now;
                if let Some(title) = title {
                    self.summary.title = title;
                }
            }
            Err(error) => events.push(SessionEvent::Failed(SessionFailure::Store(
                error.to_string(),
            ))),
        }
    }
}

fn result_part(call_id: &str, written: crate::tools::Written) -> Part {
    Part::ToolResult {
        call_id: call_id.to_owned(),
        status: written.status,
        output: written.output,
        exit: written.exit,
        total_bytes: written.total_bytes,
        mode: written.mode,
    }
}

/// A file that may hold keys or passwords: asked about even when the
/// assistant approves for the user.
fn is_sensitive(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower.contains(".ssh/")
        || lower.contains(".aws/")
        || lower.contains(".kube/")
        || lower.contains(".gnupg/")
        || lower.contains(".docker/config")
        || [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".kdbx"]
            .iter()
            .any(|end| name.ends_with(end))
        || name.starts_with(".env")
        || name.starts_with("id_")
        || [
            "shadow",
            "gshadow",
            ".netrc",
            ".pgpass",
            ".git-credentials",
            "credentials",
            "kubeconfig",
        ]
        .contains(&name)
        || name.contains("secret")
        || name.contains("password")
        || name.contains("token")
}

/// A conversation's first question, cut to a line short enough for a list.
fn title_of(messages: &[Message]) -> Option<String> {
    let question = messages
        .iter()
        .find(|message| message.role == Role::User)?
        .text();
    let line = question
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut title: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        title.push('…');
    }
    Some(title)
}

/// Ask the model service one short question, to see that the address, the
/// key and the model work: what a form's 测试连接 does. Retries nothing.
pub async fn check_provider(
    provider: &ProviderConfig,
    transport: Arc<dyn Transport>,
) -> Result<(), ProviderError> {
    let question = Message {
        seq: 0,
        role: Role::User,
        parts: vec![Part::Text {
            text: "Reply with the word OK.".into(),
        }],
        raw: None,
        stop: None,
        usage: None,
        created_ms: 0,
    };
    let mut request = provider::build(&Request {
        config: provider,
        system: "You check that a connection works. Reply with the word OK.",
        tools: &[],
        messages: &[question],
        elided_before: None,
    });
    // A word back is all that is wanted, and no tools.
    let mut body = request.json();
    if provider.protocol == Protocol::Anthropic {
        body["max_tokens"] = serde_json::json!(16);
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("tools");
        object.remove("tool_choice");
    }
    request.body = body.to_string().into_bytes();
    let events = EventQueue::new();
    let mut assembler = Assembler::new(0);
    stream(
        transport,
        request,
        provider.protocol,
        &mut assembler,
        &events,
    )
    .await
}

/// The models a service lists (`GET /v1/models`), in its order and each
/// once: what a form's 刷新模型 fills in. Retries nothing.
pub async fn list_models(
    provider: &ProviderConfig,
    transport: Arc<dyn Transport>,
) -> Result<Vec<String>, ProviderError> {
    let mut url = provider.models_endpoint();
    // Anthropic lists 20 a page unless asked for more.
    if provider.protocol == Protocol::Anthropic {
        url.push_str("?limit=1000");
    }
    let request = HttpRequest {
        url,
        headers: provider::auth_headers(provider),
        body: Vec::new(),
    };
    let response = transport.get(request).await.map_err(transport_failure)?;
    let mut body = response.body;
    let answer = read_some(&mut body, MODELS_LIMIT).await;
    if response.status != 200 {
        return Err(ProviderError::from_answer(
            response.status,
            &answer,
            response.retry_after,
        ));
    }
    model_ids(&answer).ok_or_else(|| {
        ProviderError::new(ProviderErrorKind::Protocol)
            .with_message("the list of models was not JSON")
    })
}

/// The models of a list as services give it: `{"data": [{"id"}]}` mostly,
/// also `{"models": [{"name"}]}` or a bare list.
fn model_ids(answer: &[u8]) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_slice(answer).ok()?;
    let list = value
        .get("data")
        .or_else(|| value.get("models"))
        .unwrap_or(&value)
        .as_array()?;
    let mut ids: Vec<String> = Vec::new();
    for item in list {
        let id = item
            .get("id")
            .or_else(|| item.get("name"))
            .unwrap_or(item)
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        if let Some(id) = id
            && !ids.iter().any(|seen| seen == id)
        {
            ids.push(id.to_owned());
        }
    }
    Some(ids)
}

/// One request: send it, and read its stream into `assembler`.
async fn stream(
    transport: Arc<dyn Transport>,
    request: HttpRequest,
    protocol: Protocol,
    assembler: &mut Assembler,
    events: &EventQueue,
) -> Result<(), ProviderError> {
    let response = transport.post(request).await.map_err(transport_failure)?;
    let mut body = response.body;
    if response.status != 200 {
        let answer = read_some(&mut body, ERROR_LIMIT).await;
        return Err(ProviderError::from_answer(
            response.status,
            &answer,
            response.retry_after,
        ));
    }
    let streaming = response
        .content_type
        .as_deref()
        .is_some_and(|kind| kind.contains("event-stream"));
    if !streaming {
        let answer = read_some(&mut body, ERROR_LIMIT).await;
        let error = if serde_json::from_slice::<serde_json::Value>(&answer)
            .is_ok_and(|value| value.get("error").is_some())
        {
            ProviderError::from_stream(&answer)
        } else {
            ProviderError::new(ProviderErrorKind::Protocol).with_message(
                String::from_utf8_lossy(&answer)
                    .chars()
                    .take(300)
                    .collect::<String>(),
            )
        };
        return Err(error);
    }
    let mut parser = SseParser::new();
    let mut decoder = provider::decoder(protocol);
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(transport_failure)?;
        let parsed = parser.push(&chunk).map_err(|_| {
            ProviderError::new(ProviderErrorKind::Protocol)
                .with_message("a line of the answer was too long")
        })?;
        for event in parsed {
            for delta in decoder.event(&event)? {
                assembler.apply(delta, events);
            }
        }
        if decoder.done() {
            break;
        }
    }
    if let Some(event) = parser.finish() {
        for delta in decoder.event(&event)? {
            assembler.apply(delta, events);
        }
    }
    assembler.raw = decoder.raw();
    if !decoder.done() && assembler.stop.is_none() {
        return Err(
            ProviderError::new(ProviderErrorKind::Network).with_message("the answer broke off")
        );
    }
    Ok(())
}

async fn read_some(
    body: &mut futures::stream::BoxStream<'static, Result<Vec<u8>, TransportError>>,
    limit: usize,
) -> Vec<u8> {
    let mut answer = Vec::new();
    while answer.len() < limit {
        match body.next().await {
            Some(Ok(chunk)) => answer.extend_from_slice(&chunk),
            _ => break,
        }
    }
    answer.truncate(limit);
    answer
}

fn transport_failure(error: TransportError) -> ProviderError {
    match error {
        TransportError::Network(message) => {
            ProviderError::new(ProviderErrorKind::Network).with_message(message)
        }
        TransportError::Timeout => ProviderError::new(ProviderErrorKind::Timeout),
    }
}

/// A call as it streams in.
#[derive(Default)]
struct PendingCall {
    provider_id: String,
    name: String,
    json: String,
}

/// An answer put together from its deltas.
struct Assembler {
    seq: i64,
    parts: Vec<Part>,
    calls: Vec<PendingCall>,
    usage: Option<Usage>,
    stop: Option<StopReason>,
    raw: Option<serde_json::Value>,
}

impl Assembler {
    fn new(seq: i64) -> Self {
        Self {
            seq,
            parts: Vec::new(),
            calls: Vec::new(),
            usage: None,
            stop: None,
            raw: None,
        }
    }

    fn apply(&mut self, delta: Delta, events: &EventQueue) {
        match delta {
            Delta::Text(text) => {
                match self.parts.last_mut() {
                    Some(Part::Text { text: last }) => last.push_str(&text),
                    _ => self.parts.push(Part::Text { text: text.clone() }),
                }
                events.push(SessionEvent::TextDelta(text));
            }
            Delta::Thinking(text) => {
                match self.parts.last_mut() {
                    Some(Part::Thinking { text: last }) => last.push_str(&text),
                    _ => self.parts.push(Part::Thinking { text: text.clone() }),
                }
                events.push(SessionEvent::ThinkingDelta(text));
            }
            Delta::ToolStart { index, id, name } => {
                if self.calls.len() <= index {
                    self.calls.resize_with(index + 1, PendingCall::default);
                }
                self.calls[index].provider_id = id;
                self.calls[index].name = name;
            }
            Delta::ToolInput { index, json } => {
                if let Some(call) = self.calls.get_mut(index) {
                    call.json.push_str(&json);
                }
            }
            Delta::Usage(usage) => self.usage = Some(usage),
            Delta::Stop(stop) => self.stop = Some(stop),
        }
    }

    /// What came before the user stopped it: text, without half a call.
    fn partial(self) -> Vec<Part> {
        self.parts
            .into_iter()
            .filter(|part| match part {
                Part::Text { text } | Part::Thinking { text } => !text.is_empty(),
                _ => false,
            })
            .collect()
    }

    fn finish(mut self, provider: &ProviderConfig) -> Answer {
        let has_calls = !self.calls.is_empty();
        for (index, call) in self.calls.into_iter().enumerate() {
            let json = if call.json.trim().is_empty() {
                "{}"
            } else {
                call.json.as_str()
            };
            // Input that is not JSON stays as its text: the call is then
            // answered as invalid, and the model calls again.
            let input =
                serde_json::from_str(json).unwrap_or(serde_json::Value::String(call.json.clone()));
            self.parts.push(Part::ToolCall {
                id: format!("c{}_{index}", self.seq),
                provider_id: Some(call.provider_id).filter(|id| !id.is_empty()),
                name: call.name,
                input,
            });
        }
        let mut stop = self.stop.unwrap_or(StopReason::EndTurn);
        // A service that says `stop` with calls in its answer still wants
        // them run.
        if has_calls && matches!(stop, StopReason::EndTurn | StopReason::Other) {
            stop = StopReason::ToolUse;
        }
        Answer {
            parts: self.parts,
            raw: self.raw.map(|content| Raw {
                source: provider.source(),
                content,
            }),
            stop,
            usage: self.usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_read_back() {
        for approval in Approval::ALL {
            assert_eq!(Approval::from_key(approval.key()), Some(approval));
        }
    }

    #[test]
    fn files_that_hold_secrets_are_marked() {
        for path in [
            "~/.ssh/id_ed25519",
            "/root/.ssh/authorized_keys",
            "/etc/shadow",
            "/srv/app/.env",
            "/srv/app/.env.production",
            "/etc/ssl/private/site.key",
            "cert.pem",
            "/home/u/.aws/credentials",
            "/root/.kube/config",
            "db_password.txt",
        ] {
            assert!(is_sensitive(path), "{path}");
        }
        for path in [
            "/etc/nginx/nginx.conf",
            "/var/log/syslog",
            "/etc/hosts",
            "README.md",
        ] {
            assert!(!is_sensitive(path), "{path}");
        }
    }

    #[test]
    fn a_title_is_the_first_line_of_the_first_question() {
        let question = Message {
            seq: 0,
            role: Role::User,
            parts: vec![Part::Text {
                text: "\n  Why is the disk full?\nmore".into(),
            }],
            raw: None,
            stop: None,
            usage: None,
            created_ms: 0,
        };
        assert_eq!(
            title_of(&[question]).as_deref(),
            Some("Why is the disk full?")
        );
    }
}
