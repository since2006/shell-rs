use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    checkbox::Checkbox,
    form::{Field, Form},
    h_flex,
    input::{Input, InputEvent, InputState},
    radio::{Radio, RadioGroup},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{
    diagram::{ForwardDiagram, explain, purpose, ssh_flag, typical_use},
    forward_panel::kind_icon,
};
use crate::host::{
    DEFAULT_BIND_HOST, ForwardDraft, ForwardEndpoint, ForwardId, ForwardKind, HostId, HostStore,
    join_sentences,
};
use crate::i18n::t;
use crate::shared::{commit_footer, dismiss_form_error, form_error_notification, parse_port};

/// The dialog's width in rems: room for the three stops of the diagram side
/// by side, and for three kind cards that each hold their line of purpose
/// (an icon and thirteen characters) without folding it.
const DIALOG_WIDTH: f32 = 42.;
/// Where the dialog starts and how much of the window's height it may take
/// before its body scrolls, as fractions of that height. It has more to show
/// than the other dialogs, so it starts higher than their tenth of the way
/// down, and fits the default window without scrolling.
const DIALOG_TOP: f32 = 0.05;
const DIALOG_MAX_HEIGHT: f32 = 0.9;
/// How far, in rems, a small `Radio` sets its content in from its own left
/// edge: the dot (`size_3p5`) and the gap after it (`gap_x_2`). A kind card's
/// line of purpose is pulled back by this much, to run under the dot too.
const RADIO_DOT_COLUMN: f32 = 1.375;

/// The body of the new/edit forward dialog. Owns the field states and
/// validates on commit; the store is only touched when validation passes.
///
/// The kind cards, the diagram and the sentence under it are all driven by
/// the same fields, so what the picture shows is what would be saved.
pub struct ForwardForm {
    store: Entity<HostStore>,
    editing: Option<ForwardId>,
    /// Whether the rule being edited is running, in which case saving a
    /// change to what it does restarts it.
    editing_active: bool,
    kind: ForwardKind,
    /// Counts the changes of kind. Each starts the diagram's flow over at
    /// the new entrance.
    flow: u64,
    name: Entity<InputState>,
    host: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the host select's rows.
    host_ids: Vec<HostId>,
    bind_host: Entity<InputState>,
    bind_port: Entity<InputState>,
    target_host: Entity<InputState>,
    target_port: Entity<InputState>,
    auto_start: bool,
    _subscriptions: Vec<Subscription>,
}

impl ForwardForm {
    pub fn new(
        editing: Option<ForwardId>,
        editing_active: bool,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (draft, host_ids, host_labels) = {
            let read = store.read(cx);
            let draft = editing
                .and_then(|id| read.forward(id))
                .map(|rule| rule.draft());
            let hosts = read.hosts();
            (
                draft,
                hosts.iter().map(|host| host.id).collect::<Vec<_>>(),
                hosts
                    .iter()
                    .map(|host| {
                        t!(
                            "forward.dialog.host_option",
                            name = host.name,
                            endpoint = host.endpoint()
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let kind = draft.as_ref().map(|draft| draft.kind).unwrap_or_default();
        // A new rule names no host: which server a port is opened through is
        // for the user to pick, not for a default to pick for them.
        let host_ix = draft
            .as_ref()
            .and_then(|draft| host_ids.iter().position(|id| *id == draft.host));
        let bind = draft.as_ref().map(|draft| draft.bind.clone());
        let target = draft.as_ref().and_then(|draft| draft.target.clone());
        let port_text = |port: Option<u16>| port.map(|port| port.to_string()).unwrap_or_default();

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("forward.dialog.name_placeholder"))
                .default_value(draft.as_ref().map(|d| d.name.clone()).unwrap_or_default())
        });
        let host = cx.new(|cx| {
            SelectState::new(host_labels, host_ix.map(IndexPath::new), window, cx).searchable(true)
        });
        let bind_host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(DEFAULT_BIND_HOST)
                .default_value(
                    bind.as_ref()
                        .map(|bind| bind.host.clone())
                        .unwrap_or_else(|| DEFAULT_BIND_HOST.into()),
                )
        });
        let bind_port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(bind_port_placeholder(kind))
                .default_value(port_text(bind.as_ref().map(|bind| bind.port)))
        });
        let target_host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(target_host_placeholder(kind))
                .default_value(
                    target
                        .as_ref()
                        .map(|target| target.host.clone())
                        .unwrap_or_default(),
                )
        });
        let target_port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(target_port_placeholder(kind))
                .default_value(port_text(target.as_ref().map(|target| target.port)))
        });

        // The diagram and the sentence follow the fields as they are typed.
        let redraw =
            |_: &mut Self, _: Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            };
        let subscriptions = vec![
            cx.subscribe(&bind_host, redraw),
            cx.subscribe(&bind_port, redraw),
            cx.subscribe(&target_host, redraw),
            cx.subscribe(&target_port, redraw),
        ];

        Self {
            store,
            editing,
            editing_active,
            kind,
            flow: 0,
            name,
            host,
            host_ids,
            bind_host,
            bind_port,
            target_host,
            target_port,
            auto_start: draft.as_ref().is_some_and(|draft| draft.auto_start),
            _subscriptions: subscriptions,
        }
    }

    /// Put the keyboard where a new rule starts: its listening port.
    fn focus_bind_port(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.bind_port
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn set_kind(&mut self, kind: ForwardKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.kind == kind {
            return;
        }
        self.kind = kind;
        self.flow += 1;
        self.bind_port.update(cx, |input, cx| {
            input.set_placeholder(bind_port_placeholder(kind), window, cx)
        });
        self.target_host.update(cx, |input, cx| {
            input.set_placeholder(target_host_placeholder(kind), window, cx)
        });
        self.target_port.update(cx, |input, cx| {
            input.set_placeholder(target_port_placeholder(kind), window, cx)
        });
        cx.notify();
    }

    fn selected_host(&self, cx: &App) -> Option<usize> {
        self.host
            .read(cx)
            .selected_index(cx)
            .map(|ix| ix.row)
            .filter(|ix| *ix < self.host_ids.len())
    }

    /// What the form holds right now, not yet checked. A port that is not a
    /// number reads as 0, which no rule may have.
    fn draft(&self, cx: &App) -> Option<ForwardDraft> {
        let host = *self.host_ids.get(self.selected_host(cx)?)?;
        let endpoint = |host: &Entity<InputState>, port: &Entity<InputState>| {
            ForwardEndpoint::new(
                host.read(cx).value().trim().to_string(),
                parse_port(&port.read(cx).value()).unwrap_or(0),
            )
        };
        let target = self
            .kind
            .has_target()
            .then(|| endpoint(&self.target_host, &self.target_port));
        Some(
            ForwardDraft::new(
                self.kind,
                host,
                endpoint(&self.bind_host, &self.bind_port),
                target,
            )
            .with_name(self.name.read(cx).value().trim().to_string())
            .with_auto_start(self.auto_start),
        )
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let draft = match self.draft(cx) {
            Some(draft) => draft
                .validated()
                .map_err(|error| SharedString::from(error.to_string())),
            None => Err(t!("forward.dialog.choose_host_error")),
        };
        let draft = match draft {
            Ok(draft) => draft,
            Err(error) => {
                window.push_notification(form_error_notification(error), cx);
                return false;
            }
        };
        let editing = self.editing;
        let saved = self.store.update(cx, |store, cx| match editing {
            Some(id) => store.update_forward(id, draft, cx),
            None => store.insert_forward(draft, cx).is_some(),
        });
        if !saved {
            // The host, or the rule itself, was deleted meanwhile.
            window.push_notification(form_error_notification(t!("forward.dialog.host_gone")), cx);
            return false;
        }
        true
    }

    /// An endpoint as the diagram and the sentence show it while it is being
    /// typed: whatever is missing reads as `…`.
    fn endpoint_text(host: &Entity<InputState>, port: &Entity<InputState>, cx: &App) -> String {
        let host = host.read(cx).value();
        let port = port.read(cx).value();
        let (host, port) = (host.trim(), port.trim());
        let shown = |text: &str| if text.is_empty() { "…" } else { text }.to_string();
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{}", shown(port))
        } else {
            format!("{}:{}", shown(host), shown(port))
        }
    }

    fn render_kinds(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = ForwardKind::ALL.iter().position(|kind| *kind == self.kind);
        RadioGroup::horizontal("forward-kind")
            .selected_index(selected)
            .on_change(cx.listener(|this, ix: &usize, window, cx| {
                if let Some(kind) = ForwardKind::ALL.get(*ix) {
                    this.set_kind(*kind, window, cx);
                }
            }))
            .children(
                ForwardKind::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(kind_ix, kind)| {
                        let checked = kind == self.kind;
                        Radio::new(kind.as_str())
                            .label(t!(
                                "forward.dialog.kind_card",
                                kind = kind.label(),
                                flag = ssh_flag(kind)
                            ))
                            .small()
                            // Three cards sharing one row, equally wide and equally
                            // tall; the chosen one keeps its outline, so the choice
                            // shows without the dot alone. The group wraps its row,
                            // so a card must not ask for the width of its text: that
                            // put the third card on a row of its own.
                            .flex_1()
                            .min_w_0()
                            .self_stretch()
                            .p_3()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(if checked { theme.primary } else { theme.border })
                            .child(
                                // The purpose takes the card's whole width,
                                // not only the column beside the dot.
                                h_flex()
                                    .items_start()
                                    .gap_1()
                                    .ml(rems(-RADIO_DOT_COLUMN))
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(kind_icon(kind).xsmall())
                                    // Zero wide, growing and clipped, so a line
                                    // too long folds inside the card instead of
                                    // widening it (a growable item otherwise
                                    // counts at the width of its text, as in the
                                    // forward list's rows).
                                    .child(
                                        div()
                                            .id(("forward-purpose", kind_ix))
                                            .test_support()
                                            .w_0()
                                            .flex_grow(1.)
                                            .overflow_x_hidden()
                                            .child(purpose(kind)),
                                    ),
                            )
                    }),
            )
    }

    fn render_explanation(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bind = Self::endpoint_text(&self.bind_host, &self.bind_port, cx);
        let target = Self::endpoint_text(&self.target_host, &self.target_port, cx);
        let sentence = explain(self.kind, &bind, &target);
        v_flex()
            .gap_3()
            .p_3()
            .rounded(theme.radius)
            .bg(theme.muted)
            .child(ForwardDiagram::new(self.kind, bind, target, self.flow))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .id("forward-explanation")
                            .test_support()
                            .aria_label(sentence.clone())
                            .text_sm()
                            .child(sentence),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(typical_use(self.kind)),
                    ),
            )
    }

    fn render_fields(&self) -> impl IntoElement {
        let (bind_label, target_label) = match self.kind {
            ForwardKind::Local => (
                t!("forward.dialog.bind_local"),
                t!("forward.dialog.target_from_server"),
            ),
            ForwardKind::Remote => (
                t!("forward.dialog.bind_server"),
                t!("forward.dialog.target_from_local"),
            ),
            ForwardKind::Dynamic => (t!("forward.dialog.bind_socks"), SharedString::default()),
        };
        Form::new()
            .columns(4)
            .child(
                Field::new()
                    .label(t!("forward.dialog.name"))
                    .col_span(4)
                    .child(Input::new(&self.name).id("forward-name").small()),
            )
            .child(
                Field::new()
                    .label(t!("forward.dialog.host"))
                    .required(true)
                    .col_span(4)
                    .child(
                        Select::new(&self.host)
                            .id("forward-host")
                            .placeholder(t!("forward.dialog.choose_host"))
                            .small(),
                    ),
            )
            .child(
                Field::new()
                    .label(bind_label)
                    .required(true)
                    .col_span(3)
                    .child(Input::new(&self.bind_host).id("forward-bind-host").small()),
            )
            .child(
                Field::new()
                    .label(t!("forward.dialog.port"))
                    .required(true)
                    .child(Input::new(&self.bind_port).id("forward-bind-port").small()),
            )
            .when(self.kind.has_target(), |form| {
                form.child(
                    Field::new()
                        .label(target_label)
                        .required(true)
                        .col_span(3)
                        .child(
                            Input::new(&self.target_host)
                                .id("forward-target-host")
                                .small(),
                        ),
                )
                .child(
                    Field::new()
                        .label(t!("forward.dialog.port"))
                        .required(true)
                        .child(
                            Input::new(&self.target_port)
                                .id("forward-target-port")
                                .small(),
                        ),
                )
            })
    }
}

fn bind_port_placeholder(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::Local => "8080",
        ForwardKind::Remote => "9000",
        ForwardKind::Dynamic => "1080",
    }
}

fn target_host_placeholder(kind: ForwardKind) -> SharedString {
    match kind {
        ForwardKind::Local | ForwardKind::Dynamic => t!("forward.dialog.target_placeholder"),
        ForwardKind::Remote => t!("forward.dialog.target_placeholder_remote"),
    }
}

fn target_port_placeholder(kind: ForwardKind) -> &'static str {
    match kind {
        ForwardKind::Local | ForwardKind::Dynamic => "3306",
        ForwardKind::Remote => "3000",
    }
}

/// What the user should know about the listening end before saving: who
/// else can reach it, and what it takes to listen there.
fn bind_notes(kind: ForwardKind, host: &str, port: Option<u16>) -> Vec<SharedString> {
    let mut notes = Vec::new();
    let bind = ForwardEndpoint::new(host.trim().to_string(), port.unwrap_or(0));
    if !host.trim().is_empty() && !bind.is_loopback() {
        notes.push(match kind {
            ForwardKind::Remote => t!("forward.dialog.gateway_ports_note"),
            ForwardKind::Local | ForwardKind::Dynamic => t!("forward.dialog.not_loopback_note"),
        });
    }
    if port.is_some_and(|port| port < 1024) {
        notes.push(match kind {
            ForwardKind::Remote => t!("forward.dialog.privileged_port_remote"),
            ForwardKind::Local | ForwardKind::Dynamic => t!("forward.dialog.privileged_port"),
        });
    }
    notes
}

impl Render for ForwardForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut notes = bind_notes(
            self.kind,
            &self.bind_host.read(cx).value(),
            parse_port(&self.bind_port.read(cx).value()),
        );
        if self.editing_active {
            notes.push(t!("forward.dialog.restart_note"));
        }
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_4()
            .w_full()
            .child(self.render_kinds(cx))
            .child(self.render_explanation(cx))
            .child(self.render_fields())
            .child(
                Checkbox::new("forward-auto-start")
                    .label(t!("forward.dialog.auto_start"))
                    .checked(self.auto_start)
                    .small()
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.auto_start = *checked;
                        cx.notify();
                    })),
            )
            .when(!notes.is_empty(), |form| {
                form.child(
                    v_flex()
                        .id("forward-notes")
                        .test_support()
                        .aria_label(join_sentences(notes.clone()).unwrap_or_default())
                        .gap_1()
                        .text_sm()
                        .text_color(muted)
                        .children(notes.into_iter().map(|note| div().child(note))),
                )
            })
    }
}

/// Open the new-forward (`editing == None`) or edit-forward dialog.
/// `editing_active` says the rule being edited is running.
pub fn open_forward_dialog(
    editing: Option<ForwardId>,
    editing_active: bool,
    store: Entity<HostStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| ForwardForm::new(editing, editing_active, store, window, cx));
    let title = if editing.is_some() {
        t!("forward.dialog.title_edit")
    } else {
        t!("forward.dialog.title_new")
    };
    let commit_label = if editing.is_some() {
        t!("common.save")
    } else {
        t!("forward.dialog.create")
    };

    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, window, _| {
            dialog
                .title(title.clone())
                // Dialog geometry is an API boundary that takes `Pixels`;
                // the width follows the interface zoom through the rem.
                .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
                .margin_top(window.viewport_size().height * DIALOG_TOP)
                .max_h(window.viewport_size().height * DIALOG_MAX_HEIGHT)
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit_label.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    // Must follow `open_dialog` in the same update: the dialog host takes
    // focus synchronously, and only a focus requested after it sticks.
    if editing.is_none() {
        form.update(cx, |form, cx| form.focus_bind_port(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{ForwardKind, bind_notes};

    #[test]
    fn the_default_listening_address_needs_no_note() {
        assert!(bind_notes(ForwardKind::Local, "127.0.0.1", Some(8080)).is_empty());
        assert!(bind_notes(ForwardKind::Remote, "localhost", Some(9000)).is_empty());
        // Nothing typed yet is not a warning either.
        assert!(bind_notes(ForwardKind::Local, "", None).is_empty());
    }

    #[test]
    fn listening_beyond_loopback_says_who_can_reach_it() {
        let local = bind_notes(ForwardKind::Dynamic, "0.0.0.0", Some(1080));
        assert_eq!(local.len(), 1);
        assert!(local[0].contains("其他设备"));
        let remote = bind_notes(ForwardKind::Remote, "0.0.0.0", Some(9000));
        assert_eq!(remote.len(), 1);
        // What to set on the server, and that the default fails quietly.
        assert!(remote[0].contains("GatewayPorts clientspecified 或 yes"));
        assert!(remote[0].contains("默认是 no"));
    }

    #[test]
    fn a_privileged_port_says_what_it_takes() {
        let local = bind_notes(ForwardKind::Local, "127.0.0.1", Some(80));
        assert_eq!(local, ["1024 以下的端口通常需要管理员权限才能监听。"]);
        let remote = bind_notes(ForwardKind::Remote, "0.0.0.0", Some(443));
        assert_eq!(remote.len(), 2);
        assert!(remote[1].contains("root"));
    }
}
