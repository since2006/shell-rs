//! The forwards that are running, and what each is doing.

use std::{collections::HashMap, time::Duration};

use async_channel::{Receiver, Sender, TryRecvError};
use gpui_kit::{Context, Entity, EventEmitter, SharedString, Subscription};

use super::{ForwardCommand, ForwardEvent, SharedForwardTransportProvider};
use crate::{
    connection::{ConnectionPrompt, ConnectionPromptReply},
    host::{ForwardId, HostId, HostStore, HostStoreEvent},
    i18n::{t, tn},
};

/// How often the workers' events are collected. Worker threads never wake a
/// foreground task themselves.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// What a forwarding rule is doing right now. Runtime only: every rule is
/// stopped when the application starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ForwardStatus {
    #[default]
    Stopped,
    /// Logging in to the server for the first time in this run.
    Connecting,
    /// Listening, and carrying `connections` connections.
    Running { connections: usize },
    /// The connection dropped; attempt `attempt` of `of` to log in again.
    Reconnecting { attempt: usize, of: usize },
    /// The forward ended on its own, and why.
    Failed(SharedString),
}

impl ForwardStatus {
    /// Whether a worker is carrying the rule or trying to.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            ForwardStatus::Connecting
                | ForwardStatus::Running { .. }
                | ForwardStatus::Reconnecting { .. }
        )
    }

    /// The status in words, as the list shows and announces it.
    pub fn label(&self) -> SharedString {
        match self {
            ForwardStatus::Stopped => t!("forward.status.stopped"),
            ForwardStatus::Connecting => t!("forward.status.connecting"),
            ForwardStatus::Running { connections: 0 } => t!("forward.status.running"),
            ForwardStatus::Running { connections } => {
                tn!("forward.status.running_with", *connections)
            }
            ForwardStatus::Reconnecting { attempt, of } => {
                t!("forward.status.reconnecting", attempt = attempt, of = of)
            }
            ForwardStatus::Failed(reason) => t!("forward.status.failed", reason = reason),
        }
    }
}

/// What the manager tells the workspace, beyond plain change notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardManagerEvent {
    /// A rule started, stopped, or moved between the states in between.
    StatusChanged(ForwardId),
    /// Logging in needs an answer. The number is the run the question
    /// belongs to: an answer for an earlier run of the rule is stale.
    PromptRequested(ForwardId, HostId, u64, ConnectionPrompt),
    /// A forward ended on its own, which the user may not be looking at.
    Failed(ForwardId, SharedString),
}

/// One run of a rule on its worker thread.
struct Link {
    /// Tells this run's questions from those of an earlier one.
    generation: u64,
    host: HostId,
    commands: Sender<ForwardCommand>,
    events: Receiver<ForwardEvent>,
    status: ForwardStatus,
    /// The run was told to stop and has not finished yet. It keeps its place
    /// until it has: its listening port is only free once it is gone.
    /// `Some(true)` when the rule is to start again afterwards.
    stopping: Option<bool>,
}

/// Owns every running forward. The rules themselves live in the host
/// store; this holds only what a run adds: a worker and its status.
pub struct ForwardManager {
    store: Entity<HostStore>,
    provider: SharedForwardTransportProvider,
    links: HashMap<ForwardId, Link>,
    /// Why a rule's last run ended, until it starts again or is deleted.
    failures: HashMap<ForwardId, SharedString>,
    /// What last went wrong with one connection of a running forward.
    problems: HashMap<ForwardId, SharedString>,
    next_generation: u64,
    polling: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ForwardManagerEvent> for ForwardManager {}

impl ForwardManager {
    pub fn new(
        store: Entity<HostStore>,
        provider: SharedForwardTransportProvider,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.forget_removed(cx)),
            cx.subscribe(&store, |this, _, event: &HostStoreEvent, cx| match event {
                // The rule listens or connects somewhere else now.
                HostStoreEvent::ForwardSettingsChanged(id) => this.restart(*id, cx),
                // The server moved, or logs in differently.
                HostStoreEvent::ConnectionSettingsChanged(host) => {
                    let running: Vec<_> = this
                        .links
                        .iter()
                        .filter(|(_, link)| link.host == *host)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in running {
                        this.restart(id, cx);
                    }
                }
                HostStoreEvent::PersistFailed(_) => {}
            }),
        ];
        Self {
            store,
            provider,
            links: HashMap::new(),
            failures: HashMap::new(),
            problems: HashMap::new(),
            next_generation: 1,
            polling: false,
            _subscriptions: subscriptions,
        }
    }

    /// What the rule is doing. A rule that is being stopped already reads
    /// as stopped, or as connecting when it is about to start again.
    pub fn status(&self, id: ForwardId) -> ForwardStatus {
        match self.links.get(&id) {
            Some(link) => match link.stopping {
                None => link.status.clone(),
                Some(true) => ForwardStatus::Connecting,
                Some(false) => ForwardStatus::Stopped,
            },
            None => match self.failures.get(&id) {
                Some(reason) => ForwardStatus::Failed(reason.clone()),
                None => ForwardStatus::Stopped,
            },
        }
    }

    pub fn is_active(&self, id: ForwardId) -> bool {
        self.status(id).is_active()
    }

    /// How many rules are running or trying to.
    pub fn active_count(&self) -> usize {
        self.links.keys().filter(|id| self.is_active(**id)).count()
    }

    /// What last went wrong with one connection of a running forward, such
    /// as a target the server could not reach. The forward itself is fine.
    pub fn problem(&self, id: ForwardId) -> Option<SharedString> {
        self.problems.get(&id).cloned()
    }

    /// The run a question has to belong to for its answer to count.
    pub fn generation(&self, id: ForwardId) -> Option<u64> {
        self.links
            .get(&id)
            .filter(|link| link.stopping.is_none())
            .map(|link| link.generation)
    }

    /// Start every rule marked to start with the application.
    pub fn start_automatic(&mut self, cx: &mut Context<Self>) {
        let automatic: Vec<_> = self
            .store
            .read(cx)
            .forwards()
            .iter()
            .filter(|rule| rule.auto_start)
            .map(|rule| rule.id)
            .collect();
        for id in automatic {
            self.start(id, cx);
        }
    }

    pub fn start(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        if let Some(link) = self.links.get_mut(&id) {
            // Still winding down: start once that is done.
            if link.stopping == Some(false) {
                link.stopping = Some(true);
                self.changed(id, cx);
            }
            return;
        }
        let store = self.store.read(cx);
        let Some(rule) = store.forward(id) else {
            return;
        };
        let Some(login) = store.login(rule.host) else {
            return;
        };
        let host_id = rule.host;
        let transport = self.provider.create(rule, &login);
        let (commands, command_receiver) = async_channel::unbounded();
        let (event_sender, events) = async_channel::unbounded();
        self.failures.remove(&id);
        self.problems.remove(&id);
        if let Err(error) = std::thread::Builder::new()
            .name("shellrs-forward".into())
            .spawn(move || transport.run(command_receiver, event_sender))
        {
            self.fail(id, t!("forward.error.start_failed", error = error), cx);
            return;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.links.insert(
            id,
            Link {
                generation,
                host: host_id,
                commands,
                events,
                status: ForwardStatus::Connecting,
                stopping: None,
            },
        );
        self.changed(id, cx);
        self.poll_while_running(cx);
    }

    pub fn stop(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        self.failures.remove(&id);
        self.problems.remove(&id);
        if let Some(link) = self.links.get_mut(&id) {
            if link.stopping.is_none() {
                let _ = link.commands.try_send(ForwardCommand::Stop);
            }
            link.stopping = Some(false);
        }
        self.changed(id, cx);
    }

    /// Stop a running rule and start it again as it stands now. A rule that
    /// is not running stays that way.
    pub fn restart(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        if link.stopping == Some(false) {
            return;
        }
        if link.stopping.is_none() {
            let _ = link.commands.try_send(ForwardCommand::Stop);
        }
        link.stopping = Some(true);
        self.problems.remove(&id);
        self.changed(id, cx);
    }

    pub fn reply_to_prompt(
        &self,
        id: ForwardId,
        generation: u64,
        request_id: u64,
        reply: ConnectionPromptReply,
    ) {
        if let Some(link) = self.links.get(&id)
            && link.generation == generation
            && link.stopping.is_none()
        {
            let _ = link
                .commands
                .try_send(ForwardCommand::PromptReply { request_id, reply });
        }
    }

    /// Rules deleted from the store, on their own or with their host,
    /// stop running and leave nothing behind.
    fn forget_removed(&mut self, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let removed: Vec<ForwardId> = self
            .links
            .keys()
            .chain(self.failures.keys())
            .filter(|id| store.forward(**id).is_none())
            .copied()
            .collect();
        for id in removed {
            self.failures.remove(&id);
            self.problems.remove(&id);
            if let Some(link) = self.links.get_mut(&id) {
                if link.stopping.is_none() {
                    let _ = link.commands.try_send(ForwardCommand::Stop);
                }
                link.stopping = Some(false);
            }
            self.changed(id, cx);
        }
    }

    fn changed(&mut self, id: ForwardId, cx: &mut Context<Self>) {
        cx.emit(ForwardManagerEvent::StatusChanged(id));
        cx.notify();
    }

    fn fail(&mut self, id: ForwardId, reason: SharedString, cx: &mut Context<Self>) {
        self.failures.insert(id, reason.clone());
        self.problems.remove(&id);
        cx.emit(ForwardManagerEvent::Failed(id, reason));
        self.changed(id, cx);
    }

    /// Collect the workers' events on a timer for as long as one is running.
    fn poll_while_running(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                let running = this.update(cx, |this, cx| {
                    this.collect_events(cx);
                    this.polling = !this.links.is_empty();
                    this.polling
                });
                if !matches!(running, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn collect_events(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<ForwardId> = self.links.keys().copied().collect();
        for id in ids {
            // The link may be replaced along the way: a run that ends in
            // order to restart hands its place to the next one.
            while let Some(link) = self.links.get(&id) {
                match link.events.try_recv() {
                    Ok(event) => self.on_event(id, event, cx),
                    Err(TryRecvError::Empty) => break,
                    // The worker is gone without a last word.
                    Err(TryRecvError::Closed) => {
                        self.finish(id, Some(t!("forward.error.ended")), cx);
                        break;
                    }
                }
            }
        }
    }

    fn on_event(&mut self, id: ForwardId, event: ForwardEvent, cx: &mut Context<Self>) {
        let Some(link) = self.links.get_mut(&id) else {
            return;
        };
        // A run that was told to stop only has its end left to report.
        if link.stopping.is_some() {
            if matches!(event, ForwardEvent::Stopped | ForwardEvent::Failed(_)) {
                self.finish(id, None, cx);
            }
            return;
        }
        let status = match event {
            ForwardEvent::Connecting => ForwardStatus::Connecting,
            ForwardEvent::Prompt(prompt) => {
                let (host, generation) = (link.host, link.generation);
                cx.emit(ForwardManagerEvent::PromptRequested(
                    id, host, generation, prompt,
                ));
                return;
            }
            ForwardEvent::Listening => ForwardStatus::Running { connections: 0 },
            ForwardEvent::Connections(connections) => {
                // Not a change of state, so observers redraw but nobody is
                // told: on a busy proxy the count moves all the time. A
                // count from before the connection dropped is ignored.
                if matches!(link.status, ForwardStatus::Running { .. }) {
                    link.status = ForwardStatus::Running { connections };
                    cx.notify();
                }
                return;
            }
            ForwardEvent::ConnectionFailed(problem) => {
                self.problems.insert(id, problem.into());
                cx.notify();
                return;
            }
            ForwardEvent::Reconnecting { attempt, of, .. } => {
                ForwardStatus::Reconnecting { attempt, of }
            }
            ForwardEvent::Failed(reason) => {
                self.finish(id, Some(reason.into()), cx);
                return;
            }
            ForwardEvent::Stopped => {
                self.finish(id, None, cx);
                return;
            }
        };
        if link.status != status {
            link.status = status;
            self.changed(id, cx);
        }
    }

    /// A run ended: on its own with `failure`, or because it was told to.
    fn finish(&mut self, id: ForwardId, failure: Option<SharedString>, cx: &mut Context<Self>) {
        let Some(link) = self.links.remove(&id) else {
            return;
        };
        match (link.stopping, failure) {
            (Some(true), _) => self.start(id, cx),
            (Some(false), _) | (None, None) => self.changed(id, cx),
            (None, Some(reason)) => self.fail(id, reason, cx),
        }
    }
}
