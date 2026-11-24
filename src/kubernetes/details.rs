use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::shared::RightClickMenu as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    h_flex,
    input::{Editor, EditorState, Input, InputEvent, InputState},
    notification::Notification,
    scroll::ScrollableElement as _,
    select::{Select, SelectEvent, SelectState},
    tab::{Tab, TabBar},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use unicode_width::UnicodeWidthStr as _;

use super::{
    commands::{self, LogOptions},
    logs::{self, LogLine, Token},
    model::{self, KubernetesCommand, Resource, ResourceKind},
    mutation::{self, ResourceOperation},
    panel::{defer_dispatch, request},
    presentation::{copy_button, reading, tone_color},
    relations::{self, Graph},
};
use crate::app::{CatalogIcon, CopyCommand, KubernetesAction};
use crate::i18n::{UiLocale, t};
use crate::shared::{
    amount, card, confirm_danger_with_cancel, detail_row, gauge, meter, numeric, soft_tag, tinted,
};
use crate::terminal::ExecTarget;

struct PreparedMutation {
    token: String,
    generation: u64,
    operation: ResourceOperation,
    script: String,
    source: Option<String>,
}

struct CachedRelations {
    key: String,
    read_at: std::time::Instant,
    values: Vec<(ResourceKind, Value)>,
}

/// The dialog pins its context, namespace, resource and SSH transport.
/// Closing it drops its reads; changing log options invalidates old replies.
pub(super) struct KubernetesDetails {
    resource: Resource,
    context: String,
    target: ExecTarget,
    dispatch: FocusHandle,
    tab: usize,
    details: Entity<EditorState>,
    snapshot: Option<Value>,
    viewing_yaml: String,
    editing: bool,
    /// The YAML editing started from: closing asks only when it changed.
    edit_base: String,
    operation_busy: bool,
    operation_error: Option<String>,
    removed_operation: Option<ResourceOperation>,
    requested_operation: Option<ResourceOperation>,
    prepared_mutation: Option<PreparedMutation>,
    overview: Vec<(&'static str, String)>,
    expanded: bool,
    usage: Option<super::metrics::Usage>,
    disk: Option<super::metrics::Disk>,
    metrics_problem: Option<String>,
    disk_problem: Option<String>,
    metrics_loading: bool,
    metrics_generation: u64,
    metrics_task: Option<Task<()>>,
    output: String,
    log_lines: Rc<Vec<LogLine>>,
    log_scroll: UniformListScrollHandle,
    /// The log line right-clicked last. The copy menu hangs off the list's
    /// box: on a `uniform_list` row it would take focus during layout.
    log_menu_hit: Rc<RefCell<Option<String>>>,
    log_measure: usize,
    live: bool,
    follow: bool,
    clipped: bool,
    query: Entity<InputState>,
    containers: Vec<String>,
    container_select: Entity<SelectState<Vec<SharedString>>>,
    tail_select: Entity<SelectState<Vec<SharedString>>>,
    since_select: Entity<SelectState<Vec<SharedString>>>,
    options: LogOptions,
    regex: bool,
    logs: Option<Rc<String>>,
    matches: usize,
    total: usize,
    filter_problem: Option<String>,
    problem: Option<String>,
    log_problem: Option<String>,
    pods: Rc<Vec<Resource>>,
    pod_problem: Option<String>,
    pods_loading: bool,
    has_selector: bool,
    pod_scroll: UniformListScrollHandle,
    loading: bool,
    logs_loading: bool,
    generation: u64,
    log_generation: u64,
    task: Option<Task<()>>,
    graph: Graph,
    relation_values: Vec<(ResourceKind, Value)>,
    relation_problems: Vec<String>,
    relations_ready: bool,
    relation_pages: [usize; 3],
    relation_cache: std::collections::VecDeque<CachedRelations>,
    relations_loading: bool,
    relation_task: Option<Task<()>>,
    selected_container: Option<String>,
    expected_uid: Option<String>,
    history: Vec<(Resource, Option<String>)>,
    log_task: Option<Task<()>>,
    pod_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl KubernetesDetails {
    /// The resource as confirmations name it: “Deployment prod/api”.
    fn scope(&self) -> String {
        let name = if self.resource.namespace.is_empty() {
            self.resource.name.clone()
        } else {
            format!("{}/{}", self.resource.namespace, self.resource.name)
        };
        format!("{} {name}", self.resource.kind.label())
    }

    /// The resource's name when its YAML has changes not sent yet. A save
    /// on its way keeps the draft until it is answered, so it does not count.
    pub(super) fn unsaved_edit(&self, cx: &App) -> Option<String> {
        (self.editing
            && !self.operation_busy
            && self.details.read(cx).value().as_ref() != self.edit_base.as_str())
        .then(|| self.scope())
    }

    pub(super) fn stop_reads(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.log_generation += 1;
        self.metrics_generation += 1;
        self.task = None;
        self.log_task = None;
        self.metrics_task = None;
        self.relation_task = None;
        self.pod_task = None;
        self.loading = false;
        self.logs_loading = false;
        self.metrics_loading = false;
        self.relations_loading = false;
        self.pods_loading = false;
        self.live = false;
        self.prepared_mutation = None;
        self.requested_operation = None;
        self.problem = Some(t!("tools.not_connected").to_string());
        cx.notify();
    }

    pub(super) fn queue_operation(
        &mut self,
        operation: ResourceOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading {
            self.requested_operation = Some(operation);
        } else {
            self.request_operation(operation, window, cx);
        }
    }

    fn request_operation(
        &mut self,
        operation: ResourceOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading
            || self.operation_busy
            || self.editing
            || self.removed_operation.is_some()
            || self.selected_container.is_some()
            || self.problem.is_some()
        {
            return;
        }
        let Some(original) = self.snapshot.clone() else {
            return;
        };
        if operation != ResourceOperation::Edit {
            self.prepare_mutation(operation, window, cx);
            return;
        }
        if !mutation::editable(self.resource.kind) {
            return;
        }
        let (resource, context) = (self.resource.clone(), self.context.clone());
        let generation = self.generation;
        self.operation_busy = true;
        self.operation_error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let manifest = mutation::manifest(&original);
                    // Refuse up front what could never be saved.
                    mutation::replace(&context, &resource, &original, &manifest)?;
                    mutation::yaml(&manifest)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation_busy = false;
                match result {
                    Err(why) => this.operation_error = Some(why),
                    Ok(yaml) => {
                        this.editing = true;
                        this.edit_base = yaml.clone();
                        this.tab = 4;
                        this.metrics_generation += 1;
                        this.metrics_task = None;
                        this.log_generation += 1;
                        this.log_task = None;
                        this.logs_loading = false;
                        this.details.update(cx, |state, cx| {
                            state.set_value(yaml, window, cx);
                            state.focus(window, cx);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn prepare_mutation(
        &mut self,
        operation: ResourceOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading
            || self.operation_busy
            || self.removed_operation.is_some()
            || self.selected_container.is_some()
            || self.problem.is_some()
        {
            return;
        }
        let Some(original) = self.snapshot.clone() else {
            return;
        };
        if operation == ResourceOperation::Edit && !self.editing {
            return;
        }
        let text = (operation == ResourceOperation::Edit)
            .then(|| self.details.read(cx).value().to_string());
        let confirmation_source = text.clone();
        let (resource, context, generation) =
            (self.resource.clone(), self.context.clone(), self.generation);
        self.prepared_mutation = None;
        self.operation_error = None;
        self.operation_busy = true;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match operation {
                        ResourceOperation::Edit => {
                            let value = mutation::edited(
                                text.as_deref().unwrap_or_default(),
                                &original,
                                &resource,
                            )?;
                            mutation::replace(&context, &resource, &original, &value)
                        }
                        ResourceOperation::Restart => {
                            mutation::restart(&context, &resource, &original)
                        }
                        ResourceOperation::Delete => {
                            mutation::delete(&context, &resource, &original)
                        }
                    }
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.operation_busy = false;
                if confirmation_source
                    .as_ref()
                    .is_some_and(|source| this.details.read(cx).value().as_ref() != source.as_str())
                {
                    this.operation_error = Some(t!("k8s.edit_changed").to_string());
                    cx.notify();
                    return;
                }
                match result {
                    Err(why) => this.operation_error = Some(why),
                    Ok(script) => {
                        let token = uuid::Uuid::new_v4().to_string();
                        let scope = this.scope();
                        let (title, verb, consequence) = match operation {
                            ResourceOperation::Edit => (
                                t!("k8s.save_title", name = scope),
                                t!("common.save"),
                                t!("k8s.save_effect"),
                            ),
                            ResourceOperation::Restart => (
                                t!("k8s.restart_title", name = scope),
                                t!("k8s.restart"),
                                if this.resource.kind == ResourceKind::Pod {
                                    t!("k8s.pod_restart_effect")
                                } else {
                                    t!("k8s.restart_effect")
                                },
                            ),
                            ResourceOperation::Delete => (
                                t!("k8s.delete_title", name = scope),
                                t!("common.delete"),
                                match this.resource.kind {
                                    ResourceKind::Namespace => t!("k8s.namespace_delete_effect"),
                                    ResourceKind::PersistentVolume
                                    | ResourceKind::PersistentVolumeClaim => {
                                        t!("k8s.volume_delete_effect")
                                    }
                                    ResourceKind::Pod => t!("k8s.pod_delete_effect"),
                                    _ => t!("k8s.delete_effect"),
                                },
                            ),
                        };
                        let description = t!(
                            "k8s.operation_scope",
                            context = this.context,
                            resource = scope,
                            effect = consequence
                        );
                        let handle = this.dispatch.clone();
                        let cancel_handle = handle.clone();
                        let cancel_token = token.clone();
                        this.prepared_mutation = Some(PreparedMutation {
                            token: token.clone(),
                            generation,
                            operation,
                            script,
                            source: confirmation_source,
                        });
                        confirm_danger_with_cancel(
                            title,
                            Some(description),
                            verb,
                            Rc::new(move |window, cx| {
                                defer_dispatch(
                                    &handle,
                                    &KubernetesAction(KubernetesCommand::ConfirmMutation(
                                        token.clone(),
                                    )),
                                    window,
                                    cx,
                                );
                            }),
                            Rc::new(move |window, cx| {
                                defer_dispatch(
                                    &cancel_handle,
                                    &KubernetesAction(KubernetesCommand::CancelMutation(
                                        cancel_token.clone(),
                                    )),
                                    window,
                                    cx,
                                )
                            }),
                            window,
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn commit_mutation(&mut self, token: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .prepared_mutation
            .as_ref()
            .is_some_and(|pending| pending.token == *token && pending.generation == self.generation)
        {
            return;
        }
        if self
            .prepared_mutation
            .as_ref()
            .and_then(|pending| pending.source.as_ref())
            .is_some_and(|source| self.details.read(cx).value().as_ref() != source.as_str())
        {
            self.prepared_mutation = None;
            self.operation_error = Some(t!("k8s.edit_changed").to_string());
            return;
        }
        let pending = self
            .prepared_mutation
            .take()
            .expect("confirmed mutation exists");
        let (target, context, handle) = (
            self.target.clone(),
            self.context.clone(),
            self.dispatch.clone(),
        );
        self.operation_busy = true;
        self.operation_error = None;
        self.metrics_generation += 1;
        self.metrics_task = None;
        self.log_generation += 1;
        self.log_task = None;
        // Once submitted, observe the outcome even if this inspector is closed.
        cx.spawn_in(window, async move |this, cx| {
            let result = request(&target, pending.script, cx)
                .await
                .and_then(|output| commands::answer(&output));
            let _ = cx.update(|window, cx| {
                match &result {
                    Ok(_) => window.push_notification(
                        Notification::success(match pending.operation {
                            ResourceOperation::Edit => t!("k8s.saved"),
                            ResourceOperation::Restart => t!("k8s.restart_requested"),
                            ResourceOperation::Delete => t!("k8s.delete_requested"),
                        }),
                        cx,
                    ),
                    Err(why) => window.push_notification(
                        Notification::error(t!("k8s.operation_failed", error = why)),
                        cx,
                    ),
                }
                let _ = this.update(cx, |this, cx| {
                    if this.generation != pending.generation {
                        return;
                    }
                    this.operation_busy = false;
                    match &result {
                        Err(why) => this.operation_error = Some(why.clone()),
                        Ok(_)
                            if pending.operation == ResourceOperation::Delete
                                || (pending.operation == ResourceOperation::Restart
                                    && this.resource.kind == ResourceKind::Pod) =>
                        {
                            this.removed_operation = Some(pending.operation);
                            this.editing = false;
                            this.snapshot = None;
                        }
                        Ok(_) => {
                            this.editing = false;
                            this.relation_cache.clear();
                            this.relations_ready = false;
                            this.read_details(window, cx);
                        }
                    }
                    cx.notify();
                });
                if result.is_ok() {
                    defer_dispatch(
                        &handle,
                        &KubernetesAction(KubernetesCommand::Changed {
                            terminal: target.terminal.0,
                            context,
                        }),
                        window,
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    pub fn new(
        resource: Resource,
        context: String,
        target: ExecTarget,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let details = cx.new(|cx| EditorState::new(window, cx).language("yaml"));
        let query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("k8s.log_search")));
        let container_select =
            cx.new(|cx| SelectState::new(Vec::<SharedString>::new(), None, window, cx));
        let tail_select = cx.new(|cx| {
            SelectState::new(
                ["100", "500", "1000", "5000", "10000"]
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
                Some(IndexPath::new(2)),
                window,
                cx,
            )
        });
        let since_select = cx.new(|cx| {
            SelectState::new(
                ["5m", "15m", "1h", "6h", "24h"]
                    .into_iter()
                    .map(SharedString::from)
                    .collect::<Vec<_>>(),
                Some(IndexPath::new(2)),
                window,
                cx,
            )
        });
        let mut subscriptions = Vec::new();
        let handle = dispatch.clone();
        subscriptions.push(cx.subscribe_in(
            &container_select,
            window,
            move |_, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    defer_dispatch(
                        &handle,
                        &KubernetesAction(KubernetesCommand::Container(value.to_string())),
                        window,
                        cx,
                    );
                }
            },
        ));
        let handle = dispatch.clone();
        subscriptions.push(cx.subscribe_in(
            &tail_select,
            window,
            move |_, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event
                    && let Ok(tail) = value.parse()
                {
                    defer_dispatch(
                        &handle,
                        &KubernetesAction(KubernetesCommand::Tail(tail)),
                        window,
                        cx,
                    );
                }
            },
        ));
        let handle = dispatch.clone();
        subscriptions.push(cx.subscribe_in(
            &since_select,
            window,
            move |_, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    defer_dispatch(
                        &handle,
                        &KubernetesAction(KubernetesCommand::Since(value.to_string())),
                        window,
                        cx,
                    );
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter_logs(true, window, cx);
                }
            },
        ));
        subscriptions.push(
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.query.update(cx, |input, cx| {
                    input.set_placeholder(t!("k8s.log_search"), window, cx)
                });
            }),
        );
        let mut view = Self {
            resource,
            context,
            target,
            dispatch,
            tab: 0,
            details,
            snapshot: None,
            viewing_yaml: String::new(),
            editing: false,
            edit_base: String::new(),
            operation_busy: false,
            operation_error: None,
            removed_operation: None,
            requested_operation: None,
            prepared_mutation: None,
            overview: Vec::new(),
            expanded: false,
            usage: None,
            disk: None,
            metrics_problem: None,
            disk_problem: None,
            metrics_loading: false,
            metrics_generation: 0,
            metrics_task: None,
            output: String::new(),
            log_lines: Rc::new(Vec::new()),
            log_scroll: UniformListScrollHandle::new(),
            log_menu_hit: Rc::new(RefCell::new(None)),
            log_measure: 0,
            live: false,
            follow: true,
            clipped: false,
            query,
            containers: Vec::new(),
            container_select,
            tail_select,
            since_select,
            options: LogOptions::default(),
            regex: false,
            logs: None,
            matches: 0,
            total: 0,
            filter_problem: None,
            problem: None,
            log_problem: None,
            pods: Rc::new(Vec::new()),
            pod_problem: None,
            pods_loading: false,
            has_selector: false,
            pod_scroll: UniformListScrollHandle::new(),
            loading: false,
            logs_loading: false,
            generation: 0,
            log_generation: 0,
            task: None,
            graph: Graph::default(),
            relation_values: Vec::new(),
            relation_problems: Vec::new(),
            relations_ready: false,
            relation_pages: [0; 3],
            relation_cache: std::collections::VecDeque::new(),
            relations_loading: false,
            relation_task: None,
            selected_container: None,
            expected_uid: None,
            history: Vec::new(),
            log_task: None,
            pod_task: None,
            _subscriptions: subscriptions,
        };
        view.read_details(window, cx);
        view
    }

    pub fn command(
        &mut self,
        command: &KubernetesCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.operation_busy
            || (self.removed_operation.is_some()
                && !matches!(command, KubernetesCommand::RelationBack))
        {
            return;
        }
        match command {
            KubernetesCommand::RequestOperation(operation) => {
                self.request_operation(*operation, window, cx)
            }
            KubernetesCommand::SaveYaml if self.editing => {
                self.prepare_mutation(ResourceOperation::Edit, window, cx)
            }
            KubernetesCommand::ConfirmMutation(token) => self.commit_mutation(token, window, cx),
            KubernetesCommand::CancelMutation(token) => {
                if self
                    .prepared_mutation
                    .as_ref()
                    .is_some_and(|pending| pending.token == *token)
                {
                    self.prepared_mutation = None;
                }
            }
            KubernetesCommand::CancelEdit => {
                self.editing = false;
                self.operation_error = None;
                self.prepared_mutation = None;
                self.details.update(cx, |state, cx| {
                    state.set_value(self.viewing_yaml.clone(), window, cx)
                });
            }
            KubernetesCommand::DetailTab(tab)
                if !self.editing
                    && *tab <= 5
                    && (*tab != 5
                        || matches!(
                            self.resource.kind,
                            ResourceKind::Node | ResourceKind::Pod
                        )) =>
            {
                self.tab = *tab;
                if self.tab != 5 {
                    self.metrics_generation += 1;
                    self.metrics_task = None;
                    self.metrics_loading = false;
                } else if self.metrics_task.is_none() && !self.loading {
                    self.read_metrics(window, cx);
                }
                if self.tab != 1 {
                    self.log_generation += 1;
                    self.log_task = None;
                    self.logs_loading = false;
                }
                if self.tab == 1
                    && self.resource.kind == ResourceKind::Pod
                    && (self.logs.is_none() || self.live)
                    && !self.logs_loading
                {
                    self.read_logs(window, cx);
                }
                if self.tab == 0
                    && self.has_selector
                    && !self.loading
                    && !self.pods_loading
                    && self.pods.is_empty()
                    && self.pod_problem.is_none()
                {
                    let selector = self
                        .relation_values
                        .iter()
                        .find(|(kind, value)| {
                            *kind == self.resource.kind
                                && value["metadata"]["name"].as_str() == Some(&self.resource.name)
                        })
                        .and_then(|(_, value)| model::pod_selector(value, self.resource.kind));
                    if let Some(selector) = selector {
                        self.read_pods(selector, window, cx);
                    }
                }
                if self.tab == 3
                    && !self.loading
                    && !self.relations_ready
                    && !self.relations_loading
                {
                    self.read_relations(window, cx);
                }
            }
            KubernetesCommand::ToggleProperties => self.expanded = !self.expanded,
            KubernetesCommand::RefreshMetrics if self.tab == 5 && !self.metrics_loading => {
                self.read_metrics(window, cx)
            }
            KubernetesCommand::RefreshRelations => {
                self.relation_cache
                    .retain(|entry| entry.key != self.resource.key());
                self.read_relations(window, cx);
            }
            KubernetesCommand::RelationPage { column, page } if *column < 3 => {
                self.relation_pages[*column] = *page
            }
            KubernetesCommand::RelationNode(key) if !self.editing => {
                if let Some(node) = self.graph.nodes.get(key).cloned() {
                    self.history
                        .push((self.resource.clone(), self.selected_container.clone()));
                    self.navigate(node.resource, node.container, node.uid, window, cx);
                }
            }
            KubernetesCommand::RelationBack if !self.editing => {
                if let Some((resource, container)) = self.history.pop() {
                    self.navigate(resource, container, None, window, cx);
                }
            }
            KubernetesCommand::Container(name) if self.containers.contains(name) => {
                if self.options.container != *name {
                    self.options.container = name.clone();
                    self.change_logs(window, cx);
                }
            }
            KubernetesCommand::Tail(tail) if [100, 500, 1000, 5000, 10000].contains(tail) => {
                if self.options.tail != *tail {
                    self.options.tail = *tail;
                    self.change_logs(window, cx);
                }
            }
            KubernetesCommand::Since(since)
                if ["5m", "15m", "1h", "6h", "24h"].contains(&since.as_str()) =>
            {
                if self.options.since != *since {
                    self.options.since = since.clone();
                    self.change_logs(window, cx);
                }
            }
            KubernetesCommand::Previous(previous) => {
                if self.options.previous != *previous {
                    self.options.previous = *previous;
                    if *previous {
                        self.live = false;
                    }
                    self.change_logs(window, cx);
                }
            }
            KubernetesCommand::Regex(regex) => {
                self.regex = *regex;
                self.filter_logs(true, window, cx);
            }
            KubernetesCommand::ReadLogs => self.read_logs(window, cx),
            KubernetesCommand::LiveLogs(live) if !self.options.previous => {
                self.live = *live;
                self.log_generation += 1;
                self.log_task = None;
                self.logs_loading = false;
                if self.live && self.tab == 1 {
                    self.read_logs(window, cx);
                }
            }
            KubernetesCommand::FollowLogs(follow) => {
                self.follow = *follow;
                if self.follow {
                    self.log_scroll.scroll_to_bottom();
                }
            }
            KubernetesCommand::RefreshDetails if !self.editing => {
                self.relation_cache
                    .retain(|entry| entry.key != self.resource.key());
                self.relations_ready = false;
                self.read_details(window, cx);
            }
            KubernetesCommand::RelatedPod(name) if !self.editing => {
                if let Some(pod) = self.pods.iter().find(|pod| &pod.name == name).cloned() {
                    self.history
                        .push((self.resource.clone(), self.selected_container.clone()));
                    self.tab = 0;
                    self.navigate(pod, None, None, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn change_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.log_generation += 1;
        self.log_task = None;
        self.logs_loading = false;
        self.logs = None;
        self.clipped = false;
        self.log_problem = None;
        self.filter_logs(true, window, cx);
        if self.tab == 1 {
            self.read_logs(window, cx);
        }
    }

    fn read_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepared_mutation = None;
        self.operation_error = None;
        self.metrics_generation += 1;
        self.metrics_task = None;
        self.metrics_loading = false;
        self.generation += 1;
        self.task = None;
        self.relation_values.retain(|(kind, value)| {
            *kind == self.resource.kind
                && value["metadata"]["name"].as_str() == Some(&self.resource.name)
                && value["metadata"]["namespace"].as_str().unwrap_or("") == self.resource.namespace
        });
        self.graph = relations::build(&self.resource, &self.relation_values);
        self.pod_task = None;
        self.relation_task = None;
        self.relations_loading = false;
        self.loading = true;
        self.problem = None;
        self.pods = Rc::new(Vec::new());
        self.pod_problem = None;
        self.pods_loading = false;
        self.has_selector = false;
        let (target, context, resource, generation) = (
            self.target.clone(),
            self.context.clone(),
            self.resource.clone(),
            self.generation,
        );
        let selected_container = self.selected_container.clone();
        let expected_uid = self.expected_uid.clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            let output = request(&target, commands::detail(&context, &resource), cx).await;
            let parsed_resource = resource.clone();
            let output = cx.background_executor().spawn(async move { let resource = parsed_resource; output
                .and_then(|output| commands::answer(&output))
                .and_then(|output| {
                    let value: Value = serde_json::from_str(&output).map_err(|error| error.to_string())?;
                    if resource.kind == ResourceKind::Secret {
                        model::secret_detail(&value)
                    } else if selected_container.as_ref().is_some_and(|name| !model::containers(&value).contains(name)) {
                        Err(t!("k8s.container_missing").to_string())
                    } else { Ok(value) }
                })
                .and_then(|value| {
                    if value["metadata"]["name"].as_str() != Some(&resource.name)
                        || (resource.kind.namespaced() && value["metadata"]["namespace"].as_str() != Some(&resource.namespace)) {
                        Err(t!("k8s.incomplete").to_string())
                    } else if expected_uid.as_ref().is_some_and(|uid| value["metadata"]["uid"].as_str() != Some(uid)) {
                        Err(t!("k8s.reference_changed").to_string())
                    } else { Ok(value) }
                }).and_then(|value| {
                    let selected = selected_container.as_ref().and_then(|name| ["containers", "initContainers", "ephemeralContainers"].into_iter()
                        .flat_map(|field| value["spec"][field].as_array().into_iter().flatten()).find(|container| container["name"].as_str() == Some(name)));
                    let yaml = mutation::yaml(selected.unwrap_or(&value))?;
                    Ok((value, yaml))
                }) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.generation != generation { return; }
                this.loading = false;
                match output {
                    Err(why) => this.problem = Some(why),
                    Ok((value, yaml)) => {
                        this.snapshot = Some(value.clone());
                        this.viewing_yaml = yaml.clone();
                        this.relation_values.retain(|(kind, item)| !(*kind == this.resource.kind
                            && item["metadata"]["name"].as_str() == Some(&this.resource.name)
                            && item["metadata"]["namespace"].as_str().unwrap_or("") == this.resource.namespace));
                        this.relation_values.push((this.resource.kind, value.clone()));
                        this.graph = relations::build(&this.resource, &this.relation_values);
                        this.relation_pages = [0; 3];
                        this.overview = model::overview(&value, this.resource.kind);
                        this.resource.status = model::status(&value, this.resource.kind);
                        let selected = this.selected_container.as_ref().and_then(|name| ["containers", "initContainers", "ephemeralContainers"].into_iter()
                            .flat_map(|field| value["spec"][field].as_array().into_iter().flatten()).find(|container| container["name"].as_str() == Some(name)));
                        if let Some(container) = selected {
                            this.overview = vec![("k8s.name", container["name"].as_str().unwrap_or("").into()),
                                ("k8s.images", container["image"].as_str().unwrap_or("—").into()), ("k8s.pod", this.resource.name.clone())];
                        }
                        this.details.update(cx, |state, cx| state.set_value(yaml, window, cx));
                        let names = model::containers(&value);
                        let selected = if names.contains(&this.options.container) { this.options.container.clone() }
                            else { value["metadata"]["annotations"]["kubectl.kubernetes.io/default-container"].as_str()
                                .filter(|name| names.iter().any(|container| container == name))
                                .map(str::to_owned).or_else(|| names.first().cloned()).unwrap_or_default() };
                        let changed = selected != this.options.container;
                        this.containers = names;
                        this.options.container = selected;
                        this.container_select.update(cx, |state, cx| {
                            state.set_items(this.containers.iter().cloned().map(SharedString::from).collect(), window, cx);
                            state.set_selected_index(this.containers.iter().position(|name| name == &this.options.container).map(IndexPath::new), window, cx);
                        });
                        if changed { this.change_logs(window, cx); }
                        if let Some(selector) = model::pod_selector(&value, this.resource.kind) {
                            this.has_selector = true;
                            if this.tab == 0 { this.read_pods(selector, window, cx); }
                        }
                        if this.tab == 3 && !this.relations_ready { this.read_relations(window, cx); }
                        if this.tab == 5 { this.read_metrics(window, cx); }
                        if let Some(operation) = this.requested_operation.take() { this.request_operation(operation, window, cx); }
                    }
                }
                cx.notify();
            });
        }));
    }

    fn navigate(
        &mut self,
        resource: Resource,
        container: Option<String>,
        expected_uid: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.relations_ready = false;
        self.resource = resource;
        self.selected_container = container;
        self.expected_uid = expected_uid;
        self.live = false;
        if self.selected_container.is_some() {
            self.tab = 0;
        }
        self.snapshot = None;
        self.viewing_yaml.clear();
        self.editing = false;
        self.prepared_mutation = None;
        self.operation_error = None;
        self.removed_operation = None;
        self.requested_operation = None;
        self.overview.clear();
        self.expanded = false;
        self.usage = None;
        self.disk = None;
        self.metrics_problem = None;
        self.disk_problem = None;
        self.metrics_generation += 1;
        self.metrics_task = None;
        self.metrics_loading = false;
        if self.tab == 5 && !matches!(self.resource.kind, ResourceKind::Node | ResourceKind::Pod) {
            self.tab = 0;
        }
        self.details
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.containers.clear();
        self.options.container = self.selected_container.clone().unwrap_or_default();
        self.log_generation += 1;
        self.log_task = None;
        self.logs_loading = false;
        self.logs = None;
        self.output.clear();
        self.log_lines = Rc::new(Vec::new());
        self.read_details(window, cx);
    }

    fn read_relations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.relation_task = None;
        self.relations_loading = true;
        self.relations_ready = false;
        self.relation_problems.clear();
        let (target, context, resource, generation) = (
            self.target.clone(),
            self.context.clone(),
            self.resource.clone(),
            self.generation,
        );
        let initial = self
            .relation_values
            .iter()
            .find(|(kind, value)| {
                *kind == resource.kind
                    && value["metadata"]["name"].as_str() == Some(&resource.name)
                    && value["metadata"]["namespace"].as_str().unwrap_or("") == resource.namespace
            })
            .cloned();
        let key = resource.key();
        let cached = self
            .relation_cache
            .iter()
            .find(|entry| {
                entry.key == key
                    && entry.read_at.elapsed() < Duration::from_secs(30)
                    && initial.as_ref().is_some_and(|(kind, root)| {
                        entry.values.iter().any(|(cached_kind, cached)| {
                            cached_kind == kind
                                && cached["metadata"]["name"] == root["metadata"]["name"]
                                && cached["metadata"]["namespace"] == root["metadata"]["namespace"]
                                && cached["metadata"]["uid"] == root["metadata"]["uid"]
                                && cached["metadata"]["resourceVersion"]
                                    == root["metadata"]["resourceVersion"]
                        })
                    })
            })
            .map(|entry| entry.values.clone());
        if let Some(mut values) = cached {
            if let Some(initial) = initial {
                values.retain(|(kind, value)| {
                    !(*kind == initial.0
                        && value["metadata"]["name"] == initial.1["metadata"]["name"]
                        && value["metadata"]["namespace"] == initial.1["metadata"]["namespace"])
                });
                values.push(initial);
            }
            self.graph = relations::build(&resource, &values);
            self.relation_values = values;
            self.relation_pages = [0; 3];
            self.relations_ready = true;
            self.relations_loading = false;
            cx.notify();
            return;
        }
        self.relation_task = Some(cx.spawn_in(window, async move |this, cx| {
            let selector = initial
                .as_ref()
                .and_then(|(_, value)| model::pod_selector(value, resource.kind));
            let claim = initial.as_ref().and_then(|(_, value)| {
                if resource.kind != ResourceKind::PersistentVolume {
                    return None;
                }
                Some((
                    value["spec"]["claimRef"]["name"]
                        .as_str()
                        .filter(|name| !name.is_empty())?
                        .to_owned(),
                    value["spec"]["claimRef"]["namespace"]
                        .as_str()
                        .filter(|namespace| !namespace.is_empty())?
                        .to_owned(),
                ))
            });
            let field = (resource.kind == ResourceKind::Node)
                .then(|| format!("spec.nodeName={}", resource.name))
                .or_else(|| {
                    claim
                        .as_ref()
                        .map(|(name, _)| format!("metadata.name={name}"))
                });
            let namespace = if resource.kind == ResourceKind::Namespace {
                Some(resource.name.as_str())
            } else if let Some((_, namespace)) = claim.as_ref() {
                Some(namespace.as_str())
            } else {
                (!resource.namespace.is_empty()).then_some(resource.namespace.as_str())
            };
            let mut values = initial.into_iter().collect::<Vec<_>>();
            let mut problems = Vec::new();
            for &kind in super::relation_scope::query_kinds(resource.kind) {
                if resource.kind == ResourceKind::PersistentVolume && claim.is_none() {
                    continue;
                }
                // Selectors filter Pods, never ownership of ReplicaSets or Jobs.
                if resource.kind == ResourceKind::Service
                    && kind == ResourceKind::Pod
                    && selector.is_none()
                {
                    continue;
                }
                if resource.kind == ResourceKind::PodDisruptionBudget
                    && values
                        .first()
                        .is_none_or(|(_, value)| !value["spec"]["selector"].is_object())
                {
                    continue;
                }
                let endpoint_selector = (kind == ResourceKind::EndpointSlice)
                    .then(|| format!("kubernetes.io/service-name={}", resource.name));
                let query_selector = if kind == ResourceKind::Pod {
                    selector.as_deref()
                } else {
                    endpoint_selector.as_deref()
                };
                let query = commands::ListQuery::new(&context, namespace, kind)
                    .selector(query_selector)
                    .field_selector(field.as_deref());
                let result = super::query::fetch(&target, query, None, cx, |_, cx| {
                    this.update_in(cx, |this, _, _| this.generation == generation)
                        .unwrap_or(false)
                })
                .await;
                match result {
                    Ok(items) => values.extend(items.into_iter().map(|item| (kind, item))),
                    Err(why) => problems.push(format!("{}: {why}", kind.label())),
                }
                if !this
                    .update_in(cx, |this, _, _| this.generation == generation)
                    .unwrap_or(false)
                {
                    return;
                }
            }
            let root = resource.clone();
            let (values, graph, cacheable) = cx
                .background_executor()
                .spawn(async move {
                    let graph = relations::build(&root, &values);
                    let bytes = values.iter().try_fold(0usize, |bytes, (_, value)| {
                        let bytes = bytes + serde_json::to_vec(value).ok()?.len();
                        (bytes <= 4 << 20).then_some(bytes)
                    });
                    let cacheable = bytes.is_some() && values.len() <= 512;
                    (values, graph, cacheable)
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                if cacheable && problems.is_empty() {
                    this.relation_cache.retain(|entry| entry.key != key);
                    this.relation_cache.push_back(CachedRelations {
                        key,
                        read_at: std::time::Instant::now(),
                        values: values.clone(),
                    });
                    while this.relation_cache.len() > 4 {
                        this.relation_cache.pop_front();
                    }
                }
                this.relation_values = values;
                this.graph = graph;
                this.relation_pages = [0; 3];
                this.relation_problems = problems;
                this.relations_ready = true;
                this.relations_loading = false;
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_relations(&self, cx: &App) -> AnyElement {
        let refresh = self.dispatch.clone();
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().text_sm().child(t!("k8s.relations")))
                    .child(
                        Button::new("k8s-relation-refresh")
                            .ghost()
                            .small()
                            .label(t!("tools.refresh"))
                            .loading(self.relations_loading)
                            .on_click(move |_, window, cx| {
                                defer_dispatch(
                                    &refresh,
                                    &KubernetesAction(KubernetesCommand::RefreshRelations),
                                    window,
                                    cx,
                                )
                            }),
                    ),
            )
            .when(self.relations_loading || self.loading, |body| {
                body.child(reading("k8s-relations-reading", cx))
            })
            .when(!self.relation_problems.is_empty(), |body| {
                let why = self.relation_problems.join("\n");
                body.child(
                    div()
                        .id("k8s-relations-error")
                        .test_support()
                        .aria_label(self.relation_problems.join("\n"))
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .tooltip(move |window, cx| Tooltip::new(why.clone()).build(window, cx))
                        .child(t!("k8s.relations_partial")),
                )
            })
            .when(self.graph.clipped, |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("k8s.relations_limit")),
                )
            })
            .child(super::relation_view::render(
                &self.graph,
                &self.selected_container.as_ref().map_or_else(
                    || self.resource.key(),
                    |name| format!("{}:container:{name}", self.resource.key()),
                ),
                &self.dispatch,
                self.relation_pages,
                cx,
            ))
            .into_any_element()
    }

    fn read_pods(&mut self, selector: String, window: &mut Window, cx: &mut Context<Self>) {
        self.pods_loading = true;
        let (target, context, namespace, generation) = (
            self.target.clone(),
            self.context.clone(),
            self.resource.namespace.clone(),
            self.generation,
        );
        let root = self.resource.clone();
        let snapshot = self.snapshot.clone();
        self.pod_task = Some(cx.spawn_in(window, async move |this, cx| {
            let mut values = snapshot
                .into_iter()
                .map(|value| (root.kind, value))
                .collect::<Vec<_>>();
            if root.kind == ResourceKind::Deployment {
                let query =
                    commands::ListQuery::new(&context, Some(&namespace), ResourceKind::ReplicaSet);
                let result = super::query::fetch(&target, query, None, cx, |_, cx| {
                    this.update_in(cx, |this, _, _| this.generation == generation)
                        .unwrap_or(false)
                })
                .await;
                match result {
                    Ok(items) => values.extend(
                        items
                            .into_iter()
                            .map(|value| (ResourceKind::ReplicaSet, value)),
                    ),
                    Err(why) => {
                        let _ = this.update_in(cx, |this, _, cx| {
                            if this.generation == generation {
                                this.pod_problem = Some(why);
                                this.pods_loading = false;
                                cx.notify();
                            }
                        });
                        return;
                    }
                }
            }
            let query = commands::ListQuery::new(&context, Some(&namespace), ResourceKind::Pod)
                .selector(Some(&selector));
            let result = super::query::fetch(&target, query, None, cx, |_, cx| {
                this.update_in(cx, |this, _, _| this.generation == generation)
                    .unwrap_or(false)
            })
            .await;
            let result = cx
                .background_executor()
                .spawn(async move {
                    result.and_then(|items| {
                        values.extend(
                            items
                                .iter()
                                .cloned()
                                .map(|value| (ResourceKind::Pod, value)),
                        );
                        relations::related_pods(&root, &values)
                    })
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                match result {
                    Ok(pods) => this.pods = Rc::new(pods),
                    Err(why) => this.pod_problem = Some(why),
                }
                this.pods_loading = false;
                cx.notify();
            });
        }));
    }

    fn read_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(command) = commands::logs(&self.context, &self.resource, &self.options, false)
        else {
            return;
        };
        self.log_generation += 1;
        self.log_task = None;
        self.logs_loading = true;
        self.log_problem = None;
        let (target, generation) = (self.target.clone(), self.log_generation);
        self.log_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let result = request(&target, commands::remote(&command), cx)
                    .await
                    .and_then(|output| commands::answer(&output));
                let keep_reading = this
                    .update_in(cx, |this, window, cx| {
                        if this.log_generation != generation {
                            return false;
                        }
                        let mut changed = this.logs_loading;
                        this.logs_loading = false;
                        match result {
                            Ok(text) => {
                                let (text, clipped) = logs::bounded(text);
                                changed |= this.clipped != clipped;
                                this.clipped = clipped;
                                if this.logs.as_ref().is_none_or(|old| old.as_str() != text) {
                                    changed = true;
                                    this.logs = Some(Rc::new(text));
                                    this.filter_logs(false, window, cx);
                                }
                            }
                            Err(why) => {
                                changed = true;
                                this.log_problem = Some(why);
                                this.live = false;
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                        this.live && this.tab == 1 && !this.options.previous
                    })
                    .unwrap_or(false);
                if !keep_reading {
                    break;
                }
                // One bounded request at a time. Closing/changing scope drops
                // this task, including its timer and reply receiver.
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        }));
    }

    fn filter_logs(&mut self, reset_scroll: bool, _: &mut Window, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value();
        let logs = self.logs.as_ref().map_or("", |text| text.as_str());
        self.total = logs.lines().count();
        match filter(logs, &query, self.regex) {
            Ok((text, matches)) => {
                self.matches = matches;
                self.filter_problem = None;
                if self.output != text {
                    self.log_lines = Rc::new(text.lines().map(LogLine::new).collect());
                    self.log_measure = self
                        .log_lines
                        .iter()
                        .enumerate()
                        .max_by_key(|(_, line)| line.text.width())
                        .map_or(0, |(ix, _)| ix);
                    self.output = text;
                    if self.live && self.follow {
                        self.log_scroll.scroll_to_bottom();
                    } else if reset_scroll {
                        self.log_scroll.scroll_to_item(0, ScrollStrategy::Top);
                    }
                }
            }
            Err(why) => {
                self.matches = 0;
                self.filter_problem = Some(why);
                self.output.clear();
                self.log_lines = Rc::new(Vec::new());
            }
        }
        cx.notify();
    }

    fn render_pods(&self, cx: &App) -> AnyElement {
        if self.tab == 0 && !self.has_selector && self.pods.is_empty() {
            return div().into_any_element();
        }
        let pods = self.pods.clone();
        let dispatch = self.dispatch.clone();
        v_flex()
            .gap_2()
            .when(self.resource.kind != ResourceKind::Pod, |body| {
                body.child(div().text_sm().child(t!("k8s.related_pods")))
                    .when(self.pods_loading || self.loading, |body| {
                        body.child(reading("k8s-pods-reading", cx))
                    })
                    .when(
                        !self.loading
                            && !self.pods_loading
                            && pods.is_empty()
                            && self.pod_problem.is_none(),
                        |body| {
                            body.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.has_selector {
                                        t!("k8s.no_pods")
                                    } else {
                                        t!("k8s.no_selector")
                                    }),
                            )
                        },
                    )
            })
            .when_some(self.pod_problem.clone(), |body, why| {
                body.child(error("k8s-pods-error", &why, cx))
            })
            .when(!pods.is_empty(), |body| {
                body.child(
                    div()
                        .h(rems(6.))
                        .child(
                            uniform_list("k8s-related-pods", pods.len(), move |range, _, cx| {
                                range
                                    .map(|ix| {
                                        let pod = &pods[ix];
                                        let handle = dispatch.clone();
                                        let name = pod.name.clone();
                                        h_flex()
                                            .px_1()
                                            .gap_2()
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "k8s-related:{}",
                                                    pod.name
                                                )))
                                                .ghost()
                                                .small()
                                                .flex_1()
                                                .min_w_0()
                                                .justify_start()
                                                .label(format!("{} · {}", pod.name, pod.summary))
                                                .on_click(move |_, window, cx| {
                                                    defer_dispatch(
                                                        &handle,
                                                        &KubernetesAction(
                                                            KubernetesCommand::RelatedPod(
                                                                name.clone(),
                                                            ),
                                                        ),
                                                        window,
                                                        cx,
                                                    )
                                                }),
                                            )
                                            .when_some(pod.status.clone(), |row, (label, tone)| {
                                                row.child(soft_tag(label, tone_color(tone, cx)))
                                            })
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .size_full()
                            .track_scroll(&self.pod_scroll),
                        )
                        .vertical_scrollbar(&self.pod_scroll),
                )
            })
            .into_any_element()
    }

    fn render_container(&self, _: &App) -> AnyElement {
        h_flex()
            .gap_2()
            .child(div().text_sm().child(t!("k8s.container")))
            .child(
                Select::new(&self.container_select)
                    .id("k8s-container")
                    .small()
                    .flex_1()
                    .disabled(self.containers.is_empty()),
            )
            .into_any_element()
    }

    fn render_logs(&self, cx: &App) -> AnyElement {
        if self.resource.kind != ResourceKind::Pod {
            return v_flex()
                .gap_3()
                .child(t!("k8s.choose_pod"))
                .child(self.render_pods(cx))
                .into_any_element();
        }
        let (read, previous, regex) = (
            self.dispatch.clone(),
            self.dispatch.clone(),
            self.dispatch.clone(),
        );
        let count = t!("k8s.log_count", count = self.matches, total = self.total);
        let live = self.dispatch.clone();
        let follow = self.dispatch.clone();
        let lines = self.log_lines.clone();
        let dispatch = self.dispatch.clone();
        let measure = self.log_measure;
        let (hit, clear_hit, menu_hit) = (
            self.log_menu_hit.clone(),
            self.log_menu_hit.clone(),
            self.log_menu_hit.clone(),
        );
        v_flex()
            .gap_3()
            .child(self.render_container(cx))
            .child(
                h_flex()
                    .gap_3()
                    .child(div().text_sm().child(t!("k8s.tail")))
                    .child(
                        Select::new(&self.tail_select)
                            .id("k8s-tail")
                            .small()
                            .w(rems(7.)),
                    )
                    .child(div().text_sm().child(t!("k8s.since")))
                    .child(
                        Select::new(&self.since_select)
                            .id("k8s-since")
                            .small()
                            .w(rems(7.)),
                    )
                    .child(
                        Checkbox::new("k8s-previous")
                            .label(t!("k8s.previous"))
                            .checked(self.options.previous)
                            .on_click(move |value, window, cx| {
                                defer_dispatch(
                                    &previous,
                                    &KubernetesAction(KubernetesCommand::Previous(*value)),
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(
                        Button::new("k8s-read-logs")
                            .small()
                            .label(t!("k8s.read_logs"))
                            .loading(self.logs_loading)
                            .disabled(self.options.container.is_empty() || self.loading)
                            .on_click(move |_, window, cx| {
                                defer_dispatch(
                                    &read,
                                    &KubernetesAction(KubernetesCommand::ReadLogs),
                                    window,
                                    cx,
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Input::new(&self.query)
                            .id("k8s-log-search")
                            .small()
                            .flex_1(),
                    )
                    .child(
                        Checkbox::new("k8s-regex")
                            .label(t!("k8s.regex"))
                            .checked(self.regex)
                            .on_click(move |value, window, cx| {
                                defer_dispatch(
                                    &regex,
                                    &KubernetesAction(KubernetesCommand::Regex(*value)),
                                    window,
                                    cx,
                                )
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        Checkbox::new("k8s-live")
                            .label(t!("k8s.live"))
                            .checked(self.live)
                            .disabled(
                                !self.live
                                    && (self.options.previous
                                        || self.loading
                                        || self.options.container.is_empty()),
                            )
                            .on_click(move |value, window, cx| {
                                defer_dispatch(
                                    &live,
                                    &KubernetesAction(KubernetesCommand::LiveLogs(*value)),
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(
                        Checkbox::new("k8s-follow")
                            .label(t!("k8s.follow"))
                            .checked(self.follow)
                            .on_click(move |value, window, cx| {
                                defer_dispatch(
                                    &follow,
                                    &KubernetesAction(KubernetesCommand::FollowLogs(*value)),
                                    window,
                                    cx,
                                )
                            }),
                    )
                    .child(soft_tag("INFO", cx.theme().success))
                    .child(soft_tag("WARN", cx.theme().warning))
                    .child(soft_tag("ERROR", cx.theme().danger))
                    .child(
                        copy_button("k8s-copy-logs", self.output.clone(), &self.dispatch)
                            .label(t!("k8s.copy_logs"))
                            .disabled(self.output.is_empty()),
                    ),
            )
            .child(
                div()
                    .id("k8s-log-count")
                    .test_support()
                    .aria_label(count.clone())
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(count),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("k8s.log_limit")),
            )
            .when_some(self.log_problem.clone(), |body, why| {
                body.child(error("k8s-logs-error", &why, cx))
            })
            .when(self.logs_loading, |body| {
                body.child(reading("k8s-logs-reading", cx))
            })
            .when(self.clipped, |body| {
                body.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(t!("k8s.log_clipped")),
                )
            })
            .when(
                self.logs.is_some()
                    && !self.logs_loading
                    && self.filter_problem.is_none()
                    && self.log_problem.is_none()
                    && self.matches == 0,
                |body| {
                    body.child(div().text_sm().child(if self.total == 0 {
                        t!("k8s.no_logs")
                    } else {
                        t!("k8s.no_log_matches")
                    }))
                },
            )
            .when_some(self.filter_problem.clone(), |body, why| {
                body.child(error("k8s-regex-error", &why, cx))
            })
            .child(
                div()
                    .id("k8s-log-output")
                    .test_support()
                    .aria_label(
                        self.log_lines
                            .iter()
                            .map(|line| line.text.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                    .h(rems(20.))
                    .min_w_0()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .child(
                        uniform_list("k8s-log-lines", lines.len(), move |range, _, cx| {
                            range
                                .map(|ix| {
                                    let line = &lines[ix];
                                    let highlights = line.spans.iter().map(|(range, token)| {
                                        (range.clone(), log_style(*token, cx))
                                    });
                                    h_flex()
                                        .gap_2()
                                        .px_2()
                                        .py_1()
                                        .text_xs()
                                        .whitespace_nowrap()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .id(SharedString::from(format!("k8s-log-line:{ix}")))
                                        .test_support()
                                        .aria_label(line.text.clone())
                                        .on_mouse_down(MouseButton::Right, {
                                            let hit = hit.clone();
                                            let value = line.raw.clone();
                                            move |_, _, _| {
                                                hit.replace(Some(value.clone()));
                                            }
                                        })
                                        .child(
                                            StyledText::new(line.text.clone())
                                                .with_highlights(highlights),
                                        )
                                })
                                .collect::<Vec<_>>()
                        })
                        .size_full()
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .with_width_from_item(Some(measure))
                        .track_scroll(&self.log_scroll),
                    )
                    .vertical_scrollbar(&self.log_scroll)
                    .capture_any_mouse_down(move |event, _, _| {
                        if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                            clear_hit.replace(None);
                        }
                    })
                    .context_menu(move |menu, _, _| match menu_hit.borrow().clone() {
                        Some(value) => menu.action_context(dispatch.clone()).menu(
                            t!("history.menu.copy"),
                            Box::new(KubernetesAction(KubernetesCommand::CopyValue(value))),
                        ),
                        None => menu,
                    }),
            )
            .into_any_element()
    }

    fn read_metrics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.metrics_generation += 1;
        self.metrics_task = None;
        let Some(detail) = self
            .relation_values
            .iter()
            .find(|(kind, value)| {
                *kind == self.resource.kind
                    && value["metadata"]["name"].as_str() == Some(&self.resource.name)
                    && value["metadata"]["namespace"].as_str().unwrap_or("")
                        == self.resource.namespace
            })
            .map(|(_, value)| value.clone())
        else {
            return;
        };
        let (generation, target, context, resource, selected) = (
            self.metrics_generation,
            self.target.clone(),
            self.context.clone(),
            self.resource.clone(),
            self.selected_container.clone(),
        );
        let detail = std::sync::Arc::new(detail);
        self.metrics_loading = true;
        self.metrics_task = Some(cx.spawn_in(window, async move |this, cx| {
            let mut disk_due = cx.background_executor().now();
            loop {
                if !this
                    .update(cx, |this, cx| {
                        if this.metrics_generation != generation || this.tab != 5 {
                            return false;
                        }
                        this.metrics_loading = true;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false)
                {
                    return;
                }
                let Some(command) = super::metrics::command(&context, &resource, false) else {
                    return;
                };
                let output = request(&target, command, cx).await;
                let (resource_copy, detail_copy, selected_copy) =
                    (resource.clone(), detail.clone(), selected.clone());
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        let output = commands::answer(&output?)?;
                        let value: Value =
                            serde_json::from_str(&output).map_err(|why| why.to_string())?;
                        super::metrics::usage(
                            &value,
                            &detail_copy,
                            &resource_copy,
                            selected_copy.as_deref(),
                        )
                        .filter(|usage| usage.cpu.is_some() || usage.memory.is_some())
                        .ok_or_else(|| t!("k8s.incomplete").to_string())
                    })
                    .await;
                let failed = result.is_err();
                let current = this
                    .update(cx, |this, cx| {
                        if this.metrics_generation != generation || this.tab != 5 {
                            return false;
                        }
                        match result {
                            Ok(usage) => {
                                this.usage = Some(usage);
                                this.metrics_problem = None;
                            }
                            Err(why) => this.metrics_problem = Some(why),
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
                // The slower kubelet summary read is independent of Metrics Server.
                if resource.kind == ResourceKind::Node && cx.background_executor().now() >= disk_due
                {
                    disk_due = cx.background_executor().now() + Duration::from_secs(30);
                    let output = request(
                        &target,
                        super::metrics::command(&context, &resource, true).unwrap(),
                        cx,
                    )
                    .await;
                    let name = resource.name.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            let output = commands::answer(&output?)?;
                            let value: Value =
                                serde_json::from_str(&output).map_err(|why| why.to_string())?;
                            super::metrics::disk(&value, &name)
                                .ok_or_else(|| t!("k8s.incomplete").to_string())
                        })
                        .await;
                    let current = this
                        .update(cx, |this, cx| {
                            if this.metrics_generation != generation || this.tab != 5 {
                                return false;
                            }
                            match result {
                                Ok(disk) => {
                                    this.disk = Some(disk);
                                    this.disk_problem = None;
                                }
                                Err(why) => this.disk_problem = Some(why),
                            }
                            cx.notify();
                            true
                        })
                        .unwrap_or(false);
                    if !current {
                        return;
                    }
                }
                if !this
                    .update(cx, |this, cx| {
                        if this.metrics_generation != generation || this.tab != 5 {
                            return false;
                        }
                        this.metrics_loading = false;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false)
                {
                    return;
                }
                // Serial polling, and back off when metrics are unavailable.
                cx.background_executor()
                    .timer(Duration::from_secs(if failed { 60 } else { 10 }))
                    .await;
                if !this
                    .update(cx, |this, _| {
                        this.metrics_generation == generation && this.tab == 5
                    })
                    .unwrap_or(false)
                {
                    return;
                }
            }
        }));
    }

    fn render_metrics(&self, cx: &App) -> AnyElement {
        let handle = self.dispatch.clone();
        let usage = self.usage.clone().unwrap_or_default();
        let share = |used: Option<f64>, total: Option<f64>| {
            used.zip(total)
                .filter(|(_, total)| *total > 0.)
                .map(|(used, total)| (used / total * 100.) as f32)
        };
        let cpu_percent = share(usage.cpu, usage.cpu_total);
        let memory_percent = share(
            usage.memory.map(|value| value as f64),
            usage.memory_total.map(|value| value as f64),
        );
        let cpu_amount = usage.cpu.map_or("—".into(), |cpu| {
            let used = format!("{cpu:.3}");
            usage
                .cpu_total
                .map_or(used.clone(), |total| format!("{used} / {total:.3}"))
        });
        let basis = if self.resource.kind == ResourceKind::Node {
            t!("k8s.of_capacity")
        } else {
            t!("k8s.of_limit")
        };
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("k8s.metrics_interval")),
                    )
                    .child(
                        Button::new("k8s-metrics-refresh")
                            .ghost()
                            .small()
                            .icon(CatalogIcon::RefreshCw)
                            .tooltip(t!("tools.refresh"))
                            .accessibility_label(t!("tools.refresh"))
                            .loading(self.metrics_loading)
                            .on_click(move |_, window, cx| {
                                defer_dispatch(
                                    &handle,
                                    &KubernetesAction(KubernetesCommand::RefreshMetrics),
                                    window,
                                    cx,
                                )
                            }),
                    ),
            )
            .when_some(self.metrics_problem.clone(), |body, why| {
                body.child(
                    div()
                        .id("k8s-metrics-error")
                        .test_support()
                        .aria_label(why.clone())
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("k8s.metrics_unavailable"))
                        .tooltip(move |window, cx| Tooltip::new(why.clone()).build(window, cx)),
                )
            })
            .child(
                card("k8s-cpu", IconName::Cpu, "CPU", None, cx).child(
                    h_flex()
                        .gap_4()
                        .child(gauge("k8s-cpu-gauge", cpu_percent, cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .child(meter("k8s-cpu-usage", basis.clone(), cpu_percent, cx))
                                .child(
                                    numeric(cpu_amount.clone(), cx)
                                        .id("k8s-cpu-amount")
                                        .test_support()
                                        .aria_label(cpu_amount)
                                        .text_xs(),
                                )
                                .when(usage.cpu_total.is_none(), |body| {
                                    body.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(t!("k8s.no_metric_total")),
                                    )
                                }),
                        ),
                ),
            )
            .child(
                card(
                    "k8s-memory",
                    IconName::MemoryStick,
                    t!("monitor.memory.title"),
                    None,
                    cx,
                )
                .child(
                    h_flex()
                        .gap_4()
                        .child(gauge("k8s-memory-gauge", memory_percent, cx))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .child(meter("k8s-memory-usage", basis, memory_percent, cx))
                                .map(|body| match (usage.memory, usage.memory_total) {
                                    (Some(used), Some(total)) => {
                                        let text = format!(
                                            "{} / {}",
                                            super::format::memory_bytes(used as f64),
                                            super::format::memory_bytes(total as f64)
                                        );
                                        body.child(
                                            numeric(text.clone(), cx)
                                                .id("k8s-memory-amount")
                                                .test_support()
                                                .aria_label(text)
                                                .text_xs(),
                                        )
                                    }
                                    (used, _) => body
                                        .child(
                                            numeric(
                                                used.map_or("—".into(), |bytes| {
                                                    super::format::memory_bytes(bytes as f64)
                                                }),
                                                cx,
                                            )
                                            .text_xs(),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(t!("k8s.no_metric_total")),
                                        ),
                                }),
                        ),
                ),
            )
            .when(self.resource.kind == ResourceKind::Node, |body| {
                body.child(
                    card(
                        "k8s-disk",
                        IconName::HardDrive,
                        t!("monitor.disks.title"),
                        None,
                        cx,
                    )
                    .map(|card| match &self.disk {
                        Some(disk) => card
                            .child(meter(
                                "k8s-disk-usage",
                                t!("k8s.node_filesystem"),
                                Some(disk.used as f32 / disk.total as f32 * 100.),
                                cx,
                            ))
                            .child(amount("k8s-disk-amount", disk.used, disk.total, cx)),
                        None if self.disk_problem.is_none() => card.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("tools.reading")),
                        ),
                        None => card,
                    })
                    .when_some(self.disk_problem.clone(), |card, why| {
                        card.child(
                            div()
                                .id("k8s-disk-error")
                                .test_support()
                                .aria_label(why.clone())
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("k8s.disk_unavailable"))
                                .tooltip(move |window, cx| {
                                    Tooltip::new(why.clone()).build(window, cx)
                                }),
                        )
                    }),
                )
            })
            .into_any_element()
    }

    fn render_overview(&self, cx: &App) -> AnyElement {
        let handle = self.dispatch.clone();
        let text = self
            .overview
            .iter()
            .map(|(key, value)| format!("{}: {value}", t!(*key)))
            .collect::<Vec<_>>()
            .join("\n");
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(t!("k8s.copy_hint")),
                    )
                    .child(
                        copy_button("k8s-copy-overview", text, &self.dispatch)
                            .label(t!("k8s.copy_overview")),
                    ),
            )
            .when(self.loading && self.overview.is_empty(), |body| {
                body.child(reading("k8s-details-reading", cx))
            })
            .children([false, true].into_iter().filter_map(|metadata| {
                let mut rows = self
                    .overview
                    .iter()
                    .filter(|(key, _)| super::properties::metadata(key) == metadata)
                    .filter(|(key, _)| {
                        self.expanded
                            || !matches!(
                                *key,
                                "k8s.labels"
                                    | "k8s.conditions"
                                    | "k8s.node_conditions"
                                    | "k8s.summary"
                            )
                    })
                    .collect::<Vec<_>>();
                if !metadata {
                    rows.sort_by_key(|(key, _)| super::properties::priority(key));
                    if !self.expanded {
                        rows.truncate(8);
                    }
                }
                if rows.is_empty() {
                    return None;
                }
                Some(
                    v_flex()
                        .gap_1p5()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if metadata {
                                    t!("k8s.metadata")
                                } else {
                                    t!("k8s.key_properties")
                                }),
                        )
                        .child(
                            v_flex()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded(cx.theme().radius)
                                .children(rows.into_iter().enumerate().map(
                                    |(ix, (key, value))| {
                                        let menu_handle = self.dispatch.clone();
                                        let copy = value.clone();
                                        detail_row(
                                            SharedString::from(format!("k8s-overview:{key}"))
                                                .into(),
                                            t!(*key),
                                            value.clone().into(),
                                            cx,
                                        )
                                        .when(ix > 0, |row| {
                                            row.border_t_1().border_color(cx.theme().border)
                                        })
                                        .context_menu(
                                            move |menu, _, _| {
                                                menu.action_context(menu_handle.clone()).menu(
                                                    t!("history.menu.copy"),
                                                    Box::new(KubernetesAction(
                                                        KubernetesCommand::CopyValue(copy.clone()),
                                                    )),
                                                )
                                            },
                                        )
                                    },
                                )),
                        )
                        .into_any_element(),
                )
            }))
            .child(
                Button::new("k8s-more-properties")
                    .ghost()
                    .small()
                    .label(if self.expanded {
                        t!("k8s.fewer_properties")
                    } else {
                        t!("k8s.more_properties")
                    })
                    .on_click(move |_, window, cx| {
                        defer_dispatch(
                            &handle,
                            &KubernetesAction(KubernetesCommand::ToggleProperties),
                            window,
                            cx,
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_operations(&self, cx: &App) -> AnyElement {
        if self.selected_container.is_some() || self.removed_operation.is_some() {
            return div().into_any_element();
        }
        let enabled = !self.loading
            && !self.operation_busy
            && !self.editing
            && self.removed_operation.is_none()
            && self.problem.is_none()
            && self.selected_container.is_none()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|value| mutation::identity(value, &self.resource).is_ok());
        h_flex()
            .gap_1()
            .flex_shrink_0()
            .children(
                [
                    ResourceOperation::Edit,
                    ResourceOperation::Restart,
                    ResourceOperation::Delete,
                ]
                .into_iter()
                .filter(|operation| match operation {
                    ResourceOperation::Edit => mutation::editable(self.resource.kind),
                    ResourceOperation::Restart => mutation::restart_kind(self.resource.kind),
                    ResourceOperation::Delete => mutation::deletable(self.resource.kind),
                })
                .map(|operation| {
                    let handle = self.dispatch.clone();
                    let (id, icon) = match operation {
                        ResourceOperation::Edit => ("k8s-edit", CatalogIcon::Pencil),
                        ResourceOperation::Restart => ("k8s-restart", CatalogIcon::RotateCw),
                        ResourceOperation::Delete => ("k8s-delete", CatalogIcon::Trash),
                    };
                    let blocker = (operation == ResourceOperation::Restart)
                        .then(|| {
                            self.snapshot.as_ref().and_then(|value| {
                                mutation::restart_blocker(self.resource.kind, value)
                            })
                        })
                        .flatten();
                    let button = Button::new(id)
                        .ghost()
                        .small()
                        .icon(icon)
                        .disabled(!enabled || blocker.is_some())
                        .tooltip(blocker.unwrap_or_else(|| operation.label()))
                        .accessibility_label(operation.label())
                        .on_click(move |_, window, cx| {
                            defer_dispatch(
                                &handle,
                                &KubernetesAction(KubernetesCommand::RequestOperation(operation)),
                                window,
                                cx,
                            )
                        });
                    if operation == ResourceOperation::Delete {
                        button.custom(tinted(cx.theme().danger, cx))
                    } else {
                        button
                    }
                }),
            )
            .into_any_element()
    }

    fn render_yaml(&self, cx: &App) -> AnyElement {
        let save = self.dispatch.clone();
        let cancel = self.dispatch.clone();
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.editing {
                        t!("k8s.edit_yaml_note")
                    } else {
                        t!("k8s.yaml")
                    }),
            )
            .when(self.loading, |body| {
                body.child(reading("k8s-details-reading", cx))
            })
            .child(
                div().id("k8s-detail-yaml").test_support().child(
                    Editor::new(&self.details)
                        .aria_label(if self.editing {
                            t!("k8s.edit_yaml")
                        } else {
                            t!("k8s.yaml")
                        })
                        .readonly(!self.editing || self.operation_busy)
                        .h(rems(24.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs(),
                ),
            )
            .when(self.editing, |body| {
                body.child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("k8s-cancel-edit")
                                .ghost()
                                .small()
                                .label(t!("common.cancel"))
                                .disabled(self.operation_busy)
                                .on_click(move |_, window, cx| {
                                    defer_dispatch(
                                        &cancel,
                                        &KubernetesAction(KubernetesCommand::CancelEdit),
                                        window,
                                        cx,
                                    )
                                }),
                        )
                        .child(
                            Button::new("k8s-save-yaml")
                                .small()
                                .label(t!("k8s.save_yaml"))
                                .loading(self.operation_busy)
                                .on_click(move |_, window, cx| {
                                    defer_dispatch(
                                        &save,
                                        &KubernetesAction(KubernetesCommand::SaveYaml),
                                        window,
                                        cx,
                                    )
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_commands(&self, cx: &App) -> AnyElement {
        let dispatch = self.dispatch.clone();
        v_flex()
            .gap_3()
            .when(self.resource.kind == ResourceKind::Pod, |body| {
                body.child(self.render_container(cx))
            })
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("k8s.command_note")),
            )
            .children(
                commands::suggestions(&self.context, &self.resource, &self.options)
                    .into_iter()
                    .map(|(key, command)| {
                        let handle = dispatch.clone();
                        let copy = command.clone();
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().text_sm().child(t!(key)))
                                    .child(
                                        Button::new(SharedString::from(format!("k8s-copy:{key}")))
                                            .ghost()
                                            .small()
                                            .label(t!("history.menu.copy"))
                                            .on_click(move |_, window, cx| {
                                                defer_dispatch(
                                                    &handle,
                                                    &CopyCommand(copy.clone()),
                                                    window,
                                                    cx,
                                                )
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("k8s-command:{key}")))
                                    .test_support()
                                    .aria_label(command.clone())
                                    .p_2()
                                    .bg(cx.theme().muted)
                                    .rounded(cx.theme().radius)
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_xs()
                                    .child(command),
                            )
                    }),
            )
            .into_any_element()
    }
}

impl Render for KubernetesDetails {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = self.dispatch.clone();

        let back = self.dispatch.clone();
        let refresh = self.dispatch.clone();
        let name = if self.resource.namespace.is_empty() {
            self.resource.name.clone()
        } else {
            format!("{}/{}", self.resource.namespace, self.resource.name)
        };
        let resource = format!("{} · {name} · {}", self.resource.kind.label(), self.context);
        let full_scope = resource.clone();
        let save = self.dispatch.clone();
        v_flex()
            .id("k8s-details")
            .test_support()
            // The YAML's keys are bound here, in the dialog; the workspace
            // that carries the command out is not on its path, so the
            // command goes on to the panel, as its buttons send theirs.
            .when(self.editing, |body| {
                body.key_context(super::YAML_KEY_CONTEXT).on_action(
                    move |action: &KubernetesAction, window, cx| {
                        defer_dispatch(&save, action, window, cx)
                    },
                )
            })
            .gap_3()
            .min_w_0()
            .when(!self.history.is_empty(), |body| {
                body.child(
                    Button::new("k8s-detail-back")
                        .ghost()
                        .small()
                        .label(t!("k8s.back"))
                        .disabled(self.operation_busy || self.editing)
                        .on_click(move |_, window, cx| {
                            defer_dispatch(
                                &back,
                                &KubernetesAction(KubernetesCommand::RelationBack),
                                window,
                                cx,
                            )
                        }),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .id("k8s-detail-scope")
                                    .test_support()
                                    .aria_label(resource.clone())
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .tooltip(move |window, cx| {
                                        Tooltip::new(full_scope.clone()).build(window, cx)
                                    })
                                    .child(resource),
                            )
                            .when_some(self.selected_container.clone(), |body, name| {
                                body.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} · {name}", t!("k8s.container"))),
                                )
                            }),
                    )
                    .when_some(
                        self.resource
                            .status
                            .clone()
                            .filter(|_| self.removed_operation.is_none()),
                        |row, (label, tone)| row.child(soft_tag(label, tone_color(tone, cx))),
                    )
                    .child(self.render_operations(cx))
                    .child(
                        Button::new("k8s-detail-refresh")
                            .ghost()
                            .small()
                            .icon(CatalogIcon::RefreshCw)
                            .tooltip(t!("tools.refresh"))
                            .accessibility_label(t!("tools.refresh"))
                            .loading(self.loading || self.operation_busy)
                            .disabled(self.editing || self.removed_operation.is_some())
                            .on_click(move |_, window, cx| {
                                defer_dispatch(
                                    &refresh,
                                    &KubernetesAction(KubernetesCommand::RefreshDetails),
                                    window,
                                    cx,
                                )
                            }),
                    ),
            )
            .child(
                TabBar::new("k8s-detail-tabs")
                    .underline()
                    .small()
                    .selected_index(self.tab)
                    .child(Tab::new().label(t!("k8s.details")).disabled(
                        self.editing || self.removed_operation.is_some() || self.operation_busy,
                    ))
                    .child(Tab::new().label(t!("k8s.logs")).disabled(
                        self.editing || self.removed_operation.is_some() || self.operation_busy,
                    ))
                    .child(Tab::new().label(t!("k8s.commands")).disabled(
                        self.editing || self.removed_operation.is_some() || self.operation_busy,
                    ))
                    .child(Tab::new().label(t!("k8s.relations")).disabled(
                        self.editing || self.removed_operation.is_some() || self.operation_busy,
                    ))
                    .child(
                        Tab::new()
                            .label("YAML")
                            .disabled(self.removed_operation.is_some() || self.operation_busy),
                    )
                    .when(
                        matches!(self.resource.kind, ResourceKind::Node | ResourceKind::Pod),
                        |tabs| {
                            tabs.child(Tab::new().label(t!("k8s.performance")).disabled(
                                self.editing
                                    || self.removed_operation.is_some()
                                    || self.operation_busy,
                            ))
                        },
                    )
                    .on_click(move |ix, window, cx| {
                        defer_dispatch(
                            &handle,
                            &KubernetesAction(KubernetesCommand::DetailTab(*ix)),
                            window,
                            cx,
                        )
                    }),
            )
            .when_some(self.problem.clone(), |body, why| {
                body.child(error("k8s-detail-error", &why, cx))
            })
            .when_some(self.operation_error.clone(), |body, why| {
                body.child(error("k8s-operation-error", &why, cx))
            })
            .map(|body| {
                if self.removed_operation.is_some() {
                    body.child(
                        div()
                            .id("k8s-operation-submitted")
                            .test_support()
                            .aria_label(
                                if self.removed_operation == Some(ResourceOperation::Restart) {
                                    t!("k8s.restart_requested")
                                } else {
                                    t!("k8s.delete_requested")
                                },
                            )
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                if self.removed_operation == Some(ResourceOperation::Restart) {
                                    t!("k8s.restart_requested")
                                } else {
                                    t!("k8s.delete_requested")
                                },
                            ),
                    )
                } else {
                    match self.tab {
                        1 => body.child(self.render_logs(cx)),
                        2 => body.child(self.render_commands(cx)),
                        3 => body.child(self.render_relations(cx)),
                        4 => body.child(self.render_yaml(cx)),
                        5 => body.child(self.render_metrics(cx)),
                        _ => body
                            .child(self.render_overview(cx))
                            .child(self.render_pods(cx)),
                    }
                }
            })
    }
}

fn log_style(token: Token, cx: &App) -> HighlightStyle {
    let syntax = &cx.theme().highlight_theme.style.syntax;
    let color = match token {
        Token::Timestamp | Token::Debug => cx.theme().muted_foreground,
        Token::Info => cx.theme().success,
        Token::Warning => cx.theme().warning,
        Token::Error => cx.theme().danger,
        Token::Key => {
            return syntax.style("property").unwrap_or(HighlightStyle {
                color: Some(cx.theme().primary),
                ..Default::default()
            });
        }
        Token::String => {
            return syntax.style("string").unwrap_or(HighlightStyle {
                color: Some(cx.theme().success),
                ..Default::default()
            });
        }
        Token::Number => {
            return syntax.style("number").unwrap_or(HighlightStyle {
                color: Some(cx.theme().warning),
                ..Default::default()
            });
        }
        Token::Literal => {
            return syntax.style("boolean").unwrap_or(HighlightStyle {
                color: Some(cx.theme().primary),
                ..Default::default()
            });
        }
    };
    HighlightStyle {
        color: Some(color),
        font_weight: matches!(token, Token::Error | Token::Warning).then_some(FontWeight::SEMIBOLD),
        ..Default::default()
    }
}

fn error(id: &'static str, why: &str, cx: &App) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .aria_label(why.to_owned())
        .text_sm()
        .text_color(cx.theme().danger)
        .child(why.to_owned())
        .into_any_element()
}

/// Searches only the retrieved log snapshot. Query text never reaches a shell.
pub(super) fn filter(logs: &str, query: &str, regex: bool) -> Result<(String, usize), String> {
    let pattern = if regex && !query.is_empty() {
        Some(
            regex::RegexBuilder::new(query)
                .size_limit(1024 * 1024)
                .build()
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let lines = logs
        .lines()
        .filter(|line| {
            let display = super::format::log_line(line);
            pattern.as_ref().map_or_else(
                || display.contains(query),
                |pattern| pattern.is_match(&display),
            )
        })
        .collect::<Vec<_>>();
    let count = lines.len();
    Ok((lines.join("\n"), count))
}
