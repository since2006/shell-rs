use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::shared::RightClickMenu as _;
use gpui_kit::component::{
    ActiveTheme as _, IconName, IndexPath, Selectable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::DropdownMenu as _,
    notification::Notification,
    scroll::ScrollableElement as _,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    tab::{Tab, TabBar},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{
    commands,
    details::KubernetesDetails,
    model::{self, KubernetesCommand, Resource, ResourceKind},
    presentation::{reading, resource_menu, tone_color},
};
use crate::app::{CatalogIcon, KubernetesAction};
use crate::i18n::{UiLocale, t};
use crate::shared::confirm_danger;
use crate::terminal::{ExecTarget, exec_answer};

/// Retains a single terminal's scoped browser. A change of SSH target
/// clears its snapshots; every async result is guarded by a generation.
pub struct KubernetesPanel {
    target: Option<ExecTarget>,
    active: bool,
    context: Option<String>,
    contexts: Vec<String>,
    namespace: Option<String>,
    namespaces: Vec<String>,
    namespaces_ready: bool,
    kind: ResourceKind,
    /// The tab: 0 resources (the default, Pods), 1 nodes.
    section: usize,
    resource_kind: ResourceKind,
    rows: Rc<Vec<Resource>>,
    selected: Option<String>,
    /// The row right-clicked last, for the list's menu. The menu hangs off
    /// the list's box, not the rows: a context menu on a `uniform_list` row
    /// takes focus during layout and trips GPUI's focus assertion.
    menu_hit: Rc<RefCell<Option<(ResourceKind, String, String)>>>,
    context_select: Entity<SelectState<SearchableVec<SharedString>>>,
    namespace_select: Entity<SelectState<SearchableVec<SharedString>>>,
    kind_select: Entity<SelectState<Vec<SharedString>>>,
    search: Entity<InputState>,
    query: String,
    loading: bool,
    problem: Option<String>,
    generation: u64,
    task: Option<Task<()>>,
    details: Option<WeakEntity<KubernetesDetails>>,
    scroll: UniformListScrollHandle,
    dispatch: FocusHandle,
    focus: FocusHandle,
    window: AnyWindowHandle,
    _subscriptions: Vec<Subscription>,
}

impl KubernetesPanel {
    pub fn new(dispatch: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let context_select = cx.new(|cx| {
            SelectState::new(SearchableVec::<SharedString>::new(vec![]), None, window, cx)
                .searchable(true)
        });
        let namespace_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![t!("k8s.all")]),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
            .searchable(true)
        });
        let kind_select = cx.new(|cx| {
            SelectState::new(
                ResourceKind::ALL
                    .iter()
                    .map(|kind| SharedString::from(kind.label()))
                    .collect::<Vec<_>>(),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("k8s.search"))
                .clean_on_escape()
        });
        let mut subscriptions = Vec::new();
        let handle = dispatch.clone();
        subscriptions.push(cx.subscribe_in(
            &context_select,
            window,
            move |_, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    defer_dispatch(
                        &handle,
                        &KubernetesAction(KubernetesCommand::Context(value.to_string())),
                        window,
                        cx,
                    );
                }
            },
        ));
        let handle = dispatch.clone();
        subscriptions.push(cx.subscribe_in(
            &namespace_select,
            window,
            move |_, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let namespace = (value != &t!("k8s.all")).then(|| value.to_string());
                    defer_dispatch(
                        &handle,
                        &KubernetesAction(KubernetesCommand::Namespace(namespace)),
                        window,
                        cx,
                    );
                }
            },
        ));
        let handle = dispatch.clone();
        subscriptions.push(cx.subscribe_in(
            &kind_select,
            window,
            move |_, _, event: &SelectEvent<Vec<SharedString>>, window, cx| {
                if let SelectEvent::Confirm(Some(value)) = event
                    && let Some(kind) = ResourceKind::ALL
                        .iter()
                        .find(|kind| kind.label() == value.as_ref())
                {
                    defer_dispatch(
                        &handle,
                        &KubernetesAction(KubernetesCommand::Kind(*kind)),
                        window,
                        cx,
                    );
                }
            },
        ));
        subscriptions.push(
            cx.subscribe(&search, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().to_string();
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
            }),
        );
        subscriptions.push(
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.search.update(cx, |input, cx| {
                    input.set_placeholder(t!("k8s.search"), window, cx)
                });
                this.sync_selects(window, cx);
            }),
        );
        Self {
            target: None,
            active: false,
            context: None,
            contexts: Vec::new(),
            namespace: None,
            namespaces: Vec::new(),
            namespaces_ready: false,
            kind: ResourceKind::Pod,
            section: 0,
            resource_kind: ResourceKind::Pod,
            rows: Rc::new(Vec::new()),
            selected: None,
            menu_hit: Rc::new(RefCell::new(None)),
            context_select,
            namespace_select,
            kind_select,
            search,
            query: String::new(),
            loading: false,
            problem: None,
            generation: 0,
            task: None,
            details: None,
            scroll: UniformListScrollHandle::new(),
            dispatch,
            focus: cx.focus_handle(),
            window: window.window_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub fn set_target(&mut self, target: Option<ExecTarget>, active: bool, cx: &mut Context<Self>) {
        if self.target == target && self.active == active {
            return;
        }
        if self.target != target {
            if let Some(details) = self.details.take().and_then(|details| details.upgrade()) {
                details.update(cx, |details, cx| details.stop_reads(cx));
            }
            self.context = None;
            self.contexts.clear();
            self.namespace = None;
            self.namespaces.clear();
            self.namespaces_ready = false;
            self.rows = Rc::new(Vec::new());
            self.selected = None;
            self.scroll.scroll_to_item(0, ScrollStrategy::Top);
            let entity = cx.entity().downgrade();
            let window_handle = self.window;
            cx.defer(move |cx| {
                let _ = cx.update_window(window_handle, |_, window, cx| {
                    let _ = entity.update(cx, |this, cx| this.sync_selects(window, cx));
                });
            });
        }
        self.target = target;
        self.active = active;
        // The list reads when shown, on a change of scope, after a change
        // made here and on 刷新; it never refreshes itself (as Docker).
        self.read(true, cx);
        cx.notify();
    }

    pub fn command(
        &mut self,
        command: &KubernetesCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            KubernetesCommand::Section(section) if *section <= 1 => {
                if self.section != *section {
                    self.section = *section;
                    self.kind = match section {
                        0 => self.resource_kind,
                        _ => ResourceKind::Node,
                    };
                    self.rows = Rc::new(Vec::new());
                    self.selected = None;
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    self.read(false, cx);
                }
            }
            KubernetesCommand::Refresh => self.read(true, cx),
            KubernetesCommand::Context(context) if self.contexts.contains(context) => {
                if self.context.as_ref() != Some(context) {
                    self.context = Some(context.clone());
                    self.namespace = None;
                    self.namespaces.clear();
                    self.namespaces_ready = false;
                    self.rows = Rc::new(Vec::new());
                    self.selected = None;
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    self.read(true, cx);
                }
            }
            KubernetesCommand::Namespace(namespace)
                if namespace
                    .as_ref()
                    .is_none_or(|ns| self.namespaces.contains(ns)) =>
            {
                if &self.namespace != namespace {
                    self.namespace = namespace.clone();
                    self.rows = Rc::new(Vec::new());
                    self.selected = None;
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    self.read(false, cx);
                }
            }
            KubernetesCommand::Kind(kind) => {
                if self.kind != *kind {
                    self.section = 0;
                    self.resource_kind = *kind;
                    self.kind = *kind;
                    self.rows = Rc::new(Vec::new());
                    self.selected = None;
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    self.read(false, cx);
                }
            }
            KubernetesCommand::Changed { terminal, context } => {
                if self
                    .target
                    .as_ref()
                    .is_some_and(|target| target.terminal.0 == *terminal)
                    && self.context.as_ref() == Some(context)
                {
                    self.read(false, cx);
                }
            }
            KubernetesCommand::Details { namespace, name }
            | KubernetesCommand::Manage {
                namespace, name, ..
            } => {
                if matches!(command, KubernetesCommand::Manage { kind, .. } if *kind != self.kind) {
                    return;
                }
                if let (Some(target), Some(context), Some(resource)) = (
                    self.target.clone(),
                    self.context.clone(),
                    self.rows
                        .iter()
                        .find(|row| &row.namespace == namespace && &row.name == name)
                        .cloned(),
                ) {
                    self.selected = Some(resource.key());
                    let view = cx.new(|cx| {
                        KubernetesDetails::new(
                            resource.clone(),
                            context.clone(),
                            target,
                            self.dispatch.clone(),
                            window,
                            cx,
                        )
                    });
                    if let KubernetesCommand::Manage { operation, .. } = command {
                        view.update(cx, |view, cx| view.queue_operation(*operation, window, cx));
                    }
                    self.details = Some(view.downgrade());
                    let title = format!("Kubernetes · {context}");
                    let dispatch = self.dispatch.clone();
                    let focus = self.focus.clone();
                    window.open_dialog(cx, move |dialog, window, _| {
                        let (dispatch, escape) = (dispatch.clone(), dispatch.clone());
                        let focus = focus.clone();
                        dialog
                            .title(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(div().flex_1().min_w_0().truncate().child(title.clone()))
                                    .child(
                                        Button::new("k8s-detail-close")
                                            .ghost()
                                            .small()
                                            .icon(IconName::Close)
                                            .accessibility_label(t!("common.close"))
                                            .tooltip(t!("common.close"))
                                            .on_click(move |_, window, cx| {
                                                // The default DialogClose dispatches Cancel
                                                // through current focus, which may be outside
                                                // this dialog after terminal activation.
                                                defer_dispatch(
                                                    &dispatch,
                                                    &KubernetesAction(
                                                        KubernetesCommand::CloseDetails,
                                                    ),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                    ),
                            )
                            .w(rems(58.)
                                .to_pixels(window.rem_size())
                                .min(window.viewport_size().width * 0.94))
                            .max_h(window.viewport_size().height * 0.9)
                            .close_button(false)
                            .overlay_closable(false)
                            // Esc: with the YAML changed, ask before closing.
                            .on_cancel({
                                let view = view.clone();
                                move |_, window, cx| {
                                    if view.read(cx).unsaved_edit(cx).is_none() {
                                        return true;
                                    }
                                    defer_dispatch(
                                        &escape,
                                        &KubernetesAction(KubernetesCommand::CloseDetails),
                                        window,
                                        cx,
                                    );
                                    false
                                }
                            })
                            .on_close(move |_, window, cx| focus.focus(window, cx))
                            .child(view.clone())
                    });
                }
            }
            // Only the details dialog sends these, from on top: the
            // question's 放弃修改 waits until the question has closed.
            KubernetesCommand::CloseDetails | KubernetesCommand::DiscardDetails => {
                if matches!(command, KubernetesCommand::CloseDetails)
                    && let Some(name) = self
                        .details
                        .as_ref()
                        .and_then(WeakEntity::upgrade)
                        .and_then(|details| details.read(cx).unsaved_edit(cx))
                {
                    let handle = self.dispatch.clone();
                    confirm_danger(
                        t!("k8s.discard_title", name = name),
                        Some(t!("k8s.discard_description")),
                        t!("k8s.discard"),
                        Rc::new(move |window, cx| {
                            defer_dispatch(
                                &handle,
                                &KubernetesAction(KubernetesCommand::DiscardDetails),
                                window,
                                cx,
                            )
                        }),
                        window,
                        cx,
                    );
                    return;
                }
                self.details = None;
                window.close_dialog(cx);
                self.focus.focus(window, cx);
            }
            command => {
                if let Some(details) = self.details.as_ref().and_then(WeakEntity::upgrade) {
                    details.update(cx, |details, cx| details.command(command, window, cx));
                } else if matches!(command, KubernetesCommand::ConfirmMutation(_)) {
                    window
                        .push_notification(Notification::error(t!("k8s.confirmation_expired")), cx);
                }
            }
        }
        self.sync_selects(window, cx);
        cx.notify();
    }

    fn sync_selects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.context_select.update(cx, |state, cx| {
            state.set_items(
                self.contexts
                    .iter()
                    .cloned()
                    .map(SharedString::from)
                    .collect::<Vec<_>>()
                    .into(),
                window,
                cx,
            );
            if let Some(context) = &self.context {
                state.set_selected_value(&SharedString::from(context.clone()), window, cx);
            } else {
                state.set_selected_index(None, window, cx);
            }
        });
        self.namespace_select.update(cx, |state, cx| {
            let mut names = vec![t!("k8s.all")];
            names.extend(self.namespaces.iter().cloned().map(SharedString::from));
            state.set_items(names.into(), window, cx);
            let selected = self
                .namespace
                .clone()
                .map(SharedString::from)
                .unwrap_or_else(|| t!("k8s.all"));
            state.set_selected_value(&selected, window, cx);
        });
        self.kind_select.update(cx, |state, cx| {
            state.set_selected_index(
                ResourceKind::ALL
                    .iter()
                    .position(|kind| *kind == self.kind)
                    .map(IndexPath::new),
                window,
                cx,
            )
        });
    }

    fn read(&mut self, discover: bool, cx: &mut Context<Self>) {
        // Changing resource type can cancel discovery between context and
        // namespace replies. Finish discovering namespaces before any list.
        let discover = discover || !self.namespaces_ready;
        self.generation += 1;
        self.task = None;
        self.loading = false;
        let Some(target) = self.target.clone().filter(|_| self.active) else {
            return;
        };
        self.loading = true;
        self.problem = None;
        let retain_rows = !self.rows.is_empty();
        let (generation, window, context, namespace, kind) = (
            self.generation,
            self.window,
            self.context.clone(),
            self.namespace.clone(),
            self.kind,
        );
        self.task = Some(cx.spawn(async move |this, cx| {
            // Wait for a terminal still establishing its connection. Do not
            // retry RBAC, API or kubectl errors without an explicit refresh.
            let reply = loop {
                let command = if discover || context.is_none() {
                    commands::contexts_command()
                } else {
                    commands::list(
                        context.as_deref().unwrap_or_default(),
                        namespace.as_deref(),
                        kind,
                        None,
                    )
                };
                let Ok(reply) = this.update(cx, |_, cx| {
                    target.view.upgrade()?.read(cx).exec(command, cx)
                }) else {
                    return;
                };
                if let Some(reply) = reply {
                    break reply;
                }
                let _ = this.update(cx, |this, cx| {
                    this.problem = Some(t!("tools.not_connected").to_string());
                    cx.notify();
                });
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.problem = None;
                    cx.notify();
                }
            });
            let first = exec_answer(reply, cx)
                .await
                .unwrap_or_else(|| Err(t!("tools.not_connected").to_string()));
            let chosen = context;
            let mut selected_namespace = namespace;
            let result = async {
                let first = first?;
                if discover || chosen.is_none() {
                    let (current, contexts) = commands::contexts(&first)?;
                    let name = chosen
                        .filter(|name| contexts.contains(name))
                        .unwrap_or(current);
                    let _ = cx.update_window(window, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            if this.generation == generation {
                                if this.context.as_ref() != Some(&name) {
                                    this.rows = Rc::new(Vec::new());
                                }
                                this.context = Some(name.clone());
                                this.contexts = contexts;
                                this.sync_selects(window, cx);
                                cx.notify();
                            }
                        })
                    });
                    let namespaces = request(&target, commands::namespaces(&name), cx).await?;
                    let namespaces = super::query::fetch(
                        &target,
                        commands::ListQuery::new(&name, None, ResourceKind::Namespace),
                        Some(namespaces),
                        cx,
                        |_, _| true,
                    )
                    .await?;
                    let mut namespaces = namespaces
                        .iter()
                        .map(|value| {
                            value["metadata"]["name"]
                                .as_str()
                                .filter(|name| !name.is_empty())
                                .map(str::to_owned)
                                .ok_or_else(|| t!("k8s.incomplete").to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    namespaces.sort();
                    namespaces.dedup();
                    selected_namespace = selected_namespace.filter(|ns| namespaces.contains(ns));
                    let _ = cx.update_window(window, |_, window, cx| {
                        this.update(cx, |this, cx| {
                            if this.generation == generation {
                                this.namespace = selected_namespace.clone();
                                this.namespaces = namespaces;
                                this.namespaces_ready = true;
                                this.sync_selects(window, cx);
                                cx.notify();
                            }
                        })
                    });
                    let output = request(
                        &target,
                        commands::list(&name, selected_namespace.as_deref(), kind, None),
                        cx,
                    )
                    .await?;
                    read_rows(
                        &target,
                        &name,
                        selected_namespace.as_deref(),
                        kind,
                        &output,
                        cx,
                        |rows, cx| {
                            this.update(cx, |this, cx| {
                                if this.generation != generation {
                                    return false;
                                }
                                if !retain_rows {
                                    this.rows = Rc::new(rows);
                                    cx.notify();
                                }
                                true
                            })
                            .unwrap_or(false)
                        },
                    )
                    .await
                } else {
                    read_rows(
                        &target,
                        chosen.as_deref().unwrap_or_default(),
                        selected_namespace.as_deref(),
                        kind,
                        &first,
                        cx,
                        |rows, cx| {
                            this.update(cx, |this, cx| {
                                if this.generation != generation {
                                    return false;
                                }
                                if !retain_rows {
                                    this.rows = Rc::new(rows);
                                    cx.notify();
                                }
                                true
                            })
                            .unwrap_or(false)
                        },
                    )
                    .await
                }
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(rows) => this.rows = Rc::new(rows),
                    Err(why) => this.problem = Some(why),
                }
                cx.notify();
            });
        }));
    }
}

pub(super) async fn request(
    target: &ExecTarget,
    command: String,
    cx: &mut AsyncApp,
) -> Result<String, String> {
    let reply = cx
        .update(|cx| {
            target
                .view
                .upgrade()
                .and_then(|view| view.read(cx).exec(command, cx))
        })
        .ok_or_else(|| t!("tools.not_connected").to_string())?;
    exec_answer(reply, cx)
        .await
        .unwrap_or_else(|| Err(t!("tools.not_connected").to_string()))
}

async fn read_rows(
    target: &ExecTarget,
    context: &str,
    namespace: Option<&str>,
    kind: ResourceKind,
    output: &str,
    cx: &mut AsyncApp,
    mut progress: impl FnMut(Vec<Resource>, &mut AsyncApp) -> bool,
) -> Result<Vec<Resource>, String> {
    let mut rows = Vec::new();
    let mut problem = None;
    super::query::fetch(
        target,
        commands::ListQuery::new(context, namespace, kind),
        Some(output.into()),
        cx,
        |page, cx| {
            match model::resource_items(page, kind) {
                Ok(page) => rows.extend(page),
                Err(why) => {
                    problem = Some(why);
                    return false;
                }
            }
            rows.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
            progress(rows.clone(), cx)
        },
    )
    .await
    .map_err(|why| problem.take().unwrap_or(why))?;
    Ok(rows)
}
impl Focusable for KubernetesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for KubernetesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.rows.clone();
        let visible = Rc::new(
            rows.iter()
                .enumerate()
                .filter(|(_, row)| row.matches(&self.query))
                .map(|(ix, _)| ix)
                .collect::<Vec<_>>(),
        );
        let refresh = self.dispatch.clone();
        let dispatch = self.dispatch.clone();
        let selected = self.selected.clone();
        let sections = self.dispatch.clone();
        let (hit, clear_hit, menu_hit, menu_handle) = (
            self.menu_hit.clone(),
            self.menu_hit.clone(),
            self.menu_hit.clone(),
            self.dispatch.clone(),
        );
        let count = t!(
            "k8s.count",
            count = visible.len(),
            namespaces = self.namespaces.len()
        );
        v_flex()
            .id("kubernetes")
            .test_support()
            .track_focus(&self.focus)
            .size_full()
            .min_w_0()
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        h_flex().gap_2().child(TabBar::new("k8s-sections")
                            .flex_1().min_w_0()
                            .underline()
                            .small()
                            .selected_index(self.section)
                            .child(Tab::new().label(t!("k8s.resources")))
                            .child(Tab::new().label(t!("k8s.nodes")))
                            .on_click(move |ix, window, cx| {
                                defer_dispatch(
                                    &sections,
                                    &KubernetesAction(KubernetesCommand::Section(*ix)),
                                    window,
                                    cx,
                                )
                            }))
                            .child(
                                Button::new("k8s-refresh")
                                    .ghost()
                                    .small()
                                    .icon(CatalogIcon::RefreshCw)
                                    .tooltip(t!("tools.refresh"))
                                    .accessibility_label(t!("tools.refresh"))
                                    .loading(self.loading)
                                    .on_click(move |_, window, cx| {
                                        defer_dispatch(
                                            &refresh,
                                            &KubernetesAction(KubernetesCommand::Refresh),
                                            window,
                                            cx,
                                        )
                                    }),
                            ),
                    )
                    .child(
                        scope_field(t!("k8s.context"), Select::new(&self.context_select)
                            .id("k8s-context")
                            .accessibility_label(t!("k8s.context"))
                            .flex_1().min_w_0()
                            .small()
                            .disabled(self.contexts.is_empty())),
                    )
                    .when(self.kind.namespaced(), |header| header.child(
                        scope_field(t!("k8s.namespace"), Select::new(&self.namespace_select)
                            .id("k8s-namespace")
                            .accessibility_label(t!("k8s.namespace"))
                            .flex_1().min_w_0()
                            .small()
                            .disabled(self.context.is_none() || !self.namespaces_ready)),
                    ))
                    .when(self.section == 0, |header| {
                        header
                            .child(scope_field(t!("k8s.kind"), Select::new(&self.kind_select).id("k8s-kind")
                                    .accessibility_label(t!("k8s.kind"))
                                    .flex_1().min_w_0().small()))
                    })
                    .child(Input::new(&self.search).id("k8s-search").small())
                    .child(
                        div()
                            .id("k8s-count")
                            .test_support()
                            .aria_label(count.clone())
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count),
                    )
                    .when(self.loading && !self.rows.is_empty(), |header| {
                        header.child(reading("k8s-reading", cx))
                    })
                    .when_some(self.problem.clone(), |header, why| {
                        header.child(
                            div()
                                .id("k8s-error")
                                .test_support()
                                .aria_label(why.clone())
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(t!("k8s.failed", error = why)),
                        )
                    }),
            )
            .child(
                div()
                    .id("k8s-list")
                    .flex_1()
                    .min_h_0()
                    .map(|body| {
                        if visible.is_empty() && self.problem.is_some() && !self.loading {
                            body
                        } else if visible.is_empty() {
                            if self.loading {
                                body.child(reading("k8s-list-reading", cx))
                            } else {
                                body.child(
                                    div()
                                        .p_3()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(if self.query.trim().is_empty() { t!("k8s.empty") } else { t!("k8s.no_matches") }),
                                )
                            }
                        } else {
                            body.child(
                                uniform_list(
                                    "k8s-resources",
                                    visible.len(),
                                    move |range, _, cx| {
                                        range
                                            .map(|ix| {
                                                let resource = &rows[visible[ix]];
                                                let action =
                                                    KubernetesAction(KubernetesCommand::Details {
                                                        namespace: resource.namespace.clone(),
                                                        name: resource.name.clone(),
                                                    });
                                                let handle = dispatch.clone();
                                                let key = resource.key();
                                                let scope = if resource.namespace.is_empty() {
                                                    resource.summary.clone()
                                                } else {
                                                    format!("{} · {}", resource.namespace, resource.summary)
                                                };
                                                let menu_resource = (resource.kind, resource.namespace.clone(), resource.name.clone());
                                                let hit = hit.clone();
                                                let more_resource = menu_resource.clone();
                                                let more_handle = dispatch.clone();
                                                let (status, tone) = resource.status.clone()
                                                    .unwrap_or(("—".into(), model::Tone::Quiet));
                                                let full_scope = scope.clone();
                                                let full_status = status.clone();
                                                let full_name = resource.name.clone();
                                                div()
                                                    .id(SharedString::from(key.clone()))
                                                    .test_support()
                                                    .aria_label(format!("{}/{} · {}", resource.namespace, resource.name, resource.summary))
                                                    .w_full()
                                                    .child(h_flex().gap_1()
                                                        .child(Button::new(SharedString::from(format!("{key}:open")))
                                                            .ghost()
                                                            .small()
                                                            .selected(selected.as_deref() == Some(key.as_str()))
                                                            .flex_1().min_w_0()
                                                            // UniformList measures one row: optional status must not change its height.
                                                            .h(rems(3.5))
                                                            .px_3()
                                                            .justify_start()
                                                            .tooltip(t!("k8s.open_details"))
                                                            .child(v_flex().flex_1().min_w_0().gap_1()
                                                                .child(h_flex().gap_2()
                                                                    .child(div().id(SharedString::from(format!("{key}:name"))).test_support()
                                                                        .flex_1().min_w_0().truncate().text_sm()
                                                                        .font_weight(FontWeight::MEDIUM)
                                                                        .tooltip(move |window, cx| Tooltip::new(full_name.clone()).build(window, cx))
                                                                        .child(resource.name.clone()))
                                                                    .child(div().id(SharedString::from(format!("{key}:status"))).test_support().w(rems(5.)).flex_shrink_0().text_right().text_xs()
                                                                        .truncate().text_color(tone_color(tone, cx))
                                                                        .tooltip(move |window, cx| Tooltip::new(full_status.clone()).build(window, cx)).child(status)))
                                                                .child(div().id(SharedString::from(format!("{key}:summary"))).test_support()
                                                                    .text_xs().text_color(cx.theme().muted_foreground).truncate()
                                                                    .tooltip(move |window, cx| Tooltip::new(full_scope.clone()).build(window, cx))
                                                                    .child(scope)))
                                                            .on_click(move |_, window, cx| defer_dispatch(&handle, &action, window, cx)))
                                                        .child(Button::new(SharedString::from(format!("{key}:more"))).ghost().small().icon(IconName::Ellipsis)
                                                            .tooltip(t!("k8s.resource_actions")).accessibility_label(t!("k8s.resource_actions"))
                                                            .dropdown_menu(move |menu, _, _| resource_menu(menu, more_resource.0, &more_resource.1, &more_resource.2, &more_handle))))
                                                    .on_mouse_down(MouseButton::Right, move |_, _, _| {
                                                        hit.replace(Some(menu_resource.clone()));
                                                    })
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                )
                                .size_full()
                                .track_scroll(&self.scroll),
                            )
                        }
                    })
                    .vertical_scrollbar(&self.scroll)
                    .capture_any_mouse_down(move |event, _, _| {
                        if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                            clear_hit.replace(None);
                        }
                    })
                    .context_menu(move |menu, _, _| match menu_hit.borrow().as_ref() {
                        Some((kind, namespace, name)) => {
                            resource_menu(menu, *kind, namespace, name, &menu_handle)
                        }
                        None => menu,
                    }),
            )
    }
}

/// Scope controls share one label lane and leave the rest to the selector.
fn scope_field(label: SharedString, field: impl IntoElement) -> Div {
    h_flex()
        .gap_2()
        .child(div().w(rems(5.5)).flex_shrink_0().text_sm().child(label))
        .child(field)
}

// Select subscriptions run inside the panel update. Route commands only
// after that update ends, including clicks originating in a dialog.
pub(super) fn defer_dispatch<A: Action + Clone>(
    target: &FocusHandle,
    action: &A,
    window: &mut Window,
    cx: &mut App,
) {
    let target = target.clone();
    let action = action.clone();
    window.defer(cx, move |window, cx| {
        target.dispatch_action(&action, window, cx)
    });
}
