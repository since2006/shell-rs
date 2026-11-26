//! 新建模型服务 and its edit dialog: its name, a preset that fills in the
//! API it speaks and its address, the API key, and its models, a row each,
//! typed in (添加模型) or read from the service (刷新模型). 测试连接, in the
//! footer as in 新建主机, asks the service one short question with what the
//! form holds.

use std::sync::mpsc;
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _, StyledExt as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    scroll::ScrollableElement as _,
    select::{Select, SelectEvent, SelectState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use shellrs_assistant::{ApiKey, ProviderConfig, ProviderError, ProviderErrorKind};

use super::core_protocol;
use super::describe::{draft_error, provider_error};
use super::presets::Preset;
use super::service::AssistantService;
use crate::app::CatalogIcon;
use crate::host::{AiProtocol, AiProviderDraft, AiProviderId, HostStore};
use crate::i18n::{t, tn};
use crate::shared::{dismiss_form_error, form_error_notification};

/// Which model service dialog to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderDialog {
    New,
    Edit(AiProviderId),
}

/// The form's two errands to the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Work {
    /// 测试连接
    Check,
    /// 刷新模型
    Fetch,
}

/// What 测试连接 or 刷新模型 is doing or came to.
#[derive(Clone, Debug, PartialEq)]
enum Status {
    Idle,
    Working(Work),
    Passed(SharedString),
    /// `common`: how many of them are of the families 刷新模型 adds, for a
    /// service that lists a great many (`Preset::common_models`).
    Fetched {
        found: usize,
        common: Option<usize>,
        added: usize,
    },
    Failed(Work, SharedString),
}

impl Status {
    fn working(&self) -> bool {
        matches!(self, Self::Working(_))
    }

    /// Whose it is: 刷新模型's is said under the models, 测试连接's at the
    /// foot of the form, by its button.
    fn of(&self) -> Option<Work> {
        match self {
            Self::Idle => None,
            Self::Working(work) | Self::Failed(work, _) => Some(*work),
            Self::Passed(_) => Some(Work::Check),
            Self::Fetched { .. } => Some(Work::Fetch),
        }
    }
}

/// One model: its name, to edit, and the id its row's elements are named by.
struct ModelRow {
    id: usize,
    input: Entity<InputState>,
}

fn model_input(name: &str, window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(t!("assistant.dialog.model_placeholder"))
            .default_value(name.to_string())
    })
}

/// The API types, in the order the form offers them.
const PROTOCOLS: [AiProtocol; 3] = [
    AiProtocol::Anthropic,
    AiProtocol::OpenAi,
    AiProtocol::OpenAiResponses,
];

/// What an API type is called: product names, the same in every language.
pub fn protocol_label(protocol: AiProtocol) -> SharedString {
    match protocol {
        AiProtocol::Anthropic => "Anthropic".into(),
        AiProtocol::OpenAi => "OpenAI".into(),
        AiProtocol::OpenAiResponses => "OpenAI Responses".into(),
    }
}

pub struct ProviderForm {
    store: Entity<HostStore>,
    service: Entity<AssistantService>,
    editing: Option<AiProviderId>,
    /// The preset the address is, which the form shows.
    preset: Preset,
    preset_select: Entity<SelectState<Vec<Preset>>>,
    protocol: AiProtocol,
    protocol_select: Entity<SelectState<Vec<SharedString>>>,
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    key: Entity<InputState>,
    models: Vec<ModelRow>,
    /// The id the next row gets: ids outlive the rows deleted.
    next_row: usize,
    models_scroll: ScrollHandle,
    status: Status,
    _work: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ProviderForm {
    pub fn new(
        dialog: ProviderDialog,
        store: Entity<HostStore>,
        service: Entity<AssistantService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editing = match dialog {
            ProviderDialog::Edit(id) => Some(id),
            ProviderDialog::New => None,
        };
        let draft = editing
            .and_then(|id| {
                store
                    .read(cx)
                    .ai_provider(id)
                    .map(|provider| provider.draft())
            })
            .unwrap_or_else(|| {
                let preset = Preset::OpenAi;
                let (protocol, address) = preset.service().unwrap_or_default();
                let none: [&str; 0] = [];
                AiProviderDraft::new(preset.label(), protocol, address, none)
            });
        let preset = Preset::of(&draft.base_url);
        let preset_select = cx.new(|cx| {
            SelectState::new(
                Preset::ALL.to_vec(),
                Some(IndexPath::new(preset_index(preset))),
                window,
                cx,
            )
        });
        let protocol_select = cx.new(|cx| {
            SelectState::new(
                PROTOCOLS.map(protocol_label).to_vec(),
                Some(IndexPath::new(protocol_index(draft.protocol))),
                window,
                cx,
            )
        });
        let input = |value: SharedString,
                     placeholder: SharedString,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let name = input(
            draft.name.clone(),
            t!("assistant.dialog.name_placeholder"),
            window,
            cx,
        );
        let base_url = input(draft.base_url.clone(), "https://".into(), window, cx);
        let models: Vec<ModelRow> = draft
            .models
            .iter()
            .enumerate()
            .map(|(id, model)| ModelRow {
                id,
                input: model_input(model, window, cx),
            })
            .collect();
        let key_placeholder = if editing.is_some() {
            t!("assistant.dialog.key_keep")
        } else {
            t!("assistant.dialog.key_optional")
        };
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(key_placeholder)
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &preset_select,
                window,
                |this, _, event: &SelectEvent<Vec<Preset>>, window, cx| {
                    if let SelectEvent::Confirm(Some(preset)) = event {
                        this.choose_preset(*preset, window, cx);
                    }
                },
            ),
            cx.subscribe(
                &protocol_select,
                |this, _, event: &SelectEvent<Vec<SharedString>>, cx| {
                    if let SelectEvent::Confirm(Some(label)) = event
                        && let Some(protocol) = PROTOCOLS
                            .into_iter()
                            .find(|protocol| protocol_label(*protocol) == *label)
                    {
                        this.protocol = protocol;
                        this.status = Status::Idle;
                        cx.notify();
                    }
                },
            ),
            // As the address is typed: where requests go, and which preset
            // it is.
            cx.subscribe_in(
                &base_url,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let preset = Preset::of(&this.base_url.read(cx).value());
                        if preset != this.preset && preset != Preset::Custom {
                            this.follow_name(preset, window, cx);
                        }
                        this.show_preset(preset, window, cx);
                        cx.notify();
                    }
                },
            ),
        ];
        Self {
            store,
            service,
            editing,
            preset,
            preset_select,
            protocol: draft.protocol,
            protocol_select,
            name,
            base_url,
            key,
            next_row: models.len(),
            models,
            models_scroll: ScrollHandle::new(),
            status: Status::Idle,
            _work: None,
            _subscriptions: subscriptions,
        }
    }

    /// Fill in a preset's API type, address and name; 自定义 empties the
    /// address, to be typed.
    fn choose_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_name(preset, window, cx);
        let address = match preset.service() {
            Some((protocol, address)) => {
                self.protocol = protocol;
                self.protocol_select.update(cx, |select, cx| {
                    select.set_selected_index(
                        Some(IndexPath::new(protocol_index(protocol))),
                        window,
                        cx,
                    )
                });
                address
            }
            None => "",
        };
        self.base_url
            .update(cx, |input, cx| input.set_value(address, window, cx));
        self.show_preset(preset, window, cx);
        self.status = Status::Idle;
        cx.notify();
    }

    /// The preset's name, while the form's is none of its own: empty, or
    /// one a preset gave. 自定义 empties it, to be typed.
    fn follow_name(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let given = Preset::ALL
            .into_iter()
            .any(|each| each != Preset::Custom && each.label() == name);
        if name.is_empty() || given {
            let name = match preset {
                Preset::Custom => SharedString::default(),
                preset => preset.label(),
            };
            self.name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
    }

    fn show_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        if self.preset == preset {
            return;
        }
        self.preset = preset;
        self.preset_select.update(cx, |select, cx| {
            select.set_selected_index(Some(IndexPath::new(preset_index(preset))), window, cx)
        });
    }

    /// 添加模型: an empty row at the end, focused and scrolled to.
    fn add_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.push_model("", window, cx);
        input.update(cx, |input, cx| input.focus(window, cx));
        self.models_scroll.scroll_to_item(self.models.len() - 1);
        cx.notify();
    }

    fn push_model(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = model_input(name, window, cx);
        self.models.push(ModelRow {
            id: self.next_row,
            input: input.clone(),
        });
        self.next_row += 1;
        input
    }

    fn remove_model(&mut self, id: usize, cx: &mut Context<Self>) {
        self.models.retain(|row| row.id != id);
        cx.notify();
    }

    /// The models' names, in order: empty rows left out.
    fn models(&self, cx: &App) -> Vec<SharedString> {
        self.models
            .iter()
            .map(|row| row.input.read(cx).value().trim().to_string())
            .filter(|name| !name.is_empty())
            .map(SharedString::from)
            .collect()
    }

    fn draft(&self, cx: &App) -> AiProviderDraft {
        AiProviderDraft::new(
            self.name.read(cx).value().to_string(),
            self.protocol,
            self.base_url.read(cx).value().to_string(),
            self.models(cx),
        )
    }

    /// The service as the form describes it, with the key typed or the one
    /// saved for the service being edited, read off the UI thread; then
    /// `work` with it, its answer polled until it comes.
    fn with_service<T: Send + 'static>(
        &mut self,
        model: SharedString,
        errand: Work,
        work: fn(&AssistantService, ProviderConfig) -> Option<mpsc::Receiver<T>>,
        done: fn(&mut Self, Option<T>, &mut Window, &mut Context<Self>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let base_url = self.base_url.read(cx).value().trim().to_string();
        let typed = self.key.read(cx).value().trim().to_string();
        let saved = self
            .editing
            .and_then(|id| {
                self.store
                    .read(cx)
                    .ai_provider(id)
                    .map(|provider| provider.key_secret())
            })
            .filter(|_| typed.is_empty());
        let secrets = self.store.read(cx).secrets();
        let service = self.service.clone();
        let protocol = core_protocol(self.protocol);
        self.status = Status::Working(errand);
        cx.notify();
        self._work = Some(cx.spawn_in(window, async move |this, cx| {
            let key = match saved {
                Some(secret) => {
                    cx.background_executor()
                        .spawn(async move {
                            secrets
                                .get(&secret)
                                .ok()
                                .flatten()
                                .map(|key| key.to_string())
                        })
                        .await
                }
                None => Some(typed),
            };
            let config = ProviderConfig {
                protocol,
                base_url,
                model: model.to_string(),
                api_key: key.filter(|key| !key.is_empty()).map(ApiKey::new),
            };
            let Ok(Some(answer)) = cx.update(|_, cx| work(service.read(cx), config)) else {
                let _ = this.update_in(cx, |this, window, cx| done(this, None, window, cx));
                return;
            };
            // Polled: the answer comes from the assistant's thread.
            let result = loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                match answer.try_recv() {
                    Ok(result) => break Some(result),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => break None,
                }
            };
            let _ = this.update_in(cx, |this, window, cx| done(this, result, window, cx));
        }));
    }

    /// 测试连接, with the first model.
    pub fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.working() {
            return;
        }
        let draft = match self.draft(cx).validated() {
            Ok(draft) => draft,
            Err(error) => {
                self.status = Status::Failed(Work::Check, draft_error(error));
                cx.notify();
                return;
            }
        };
        let model = draft.models[0].clone();
        self.with_service(
            model,
            Work::Check,
            AssistantService::check,
            |this, result: Option<Result<(), ProviderError>>, _, cx| {
                let model = this.models(cx).into_iter().next().unwrap_or_default();
                this.status = match result {
                    Some(Ok(())) => Status::Passed(model),
                    Some(Err(error)) => Status::Failed(Work::Check, provider_error(&error)),
                    None => Status::Failed(Work::Check, t!("assistant.service.unavailable")),
                };
                cx.notify();
            },
            window,
            cx,
        );
    }

    /// 刷新模型: add the models the service lists that the form lacks.
    fn fetch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.working() {
            return;
        }
        let mut draft = self.draft(cx);
        // Only the address matters: listing needs no name and no model.
        draft.name = "-".into();
        draft.models = vec!["-".into()];
        if let Err(error) = draft.validated() {
            self.status = Status::Failed(Work::Fetch, draft_error(error));
            cx.notify();
            return;
        }
        self.with_service(
            SharedString::default(),
            Work::Fetch,
            AssistantService::models,
            |this, result: Option<Result<Vec<String>, ProviderError>>, window, cx| {
                let failed = |reason| Status::Failed(Work::Fetch, reason);
                this.status = match result {
                    Some(Ok(found)) => {
                        let common = this.preset.common_models(&found);
                        let offered = common.as_ref().unwrap_or(&found);
                        let mut have = this.models(cx);
                        let mut added = 0;
                        for model in offered {
                            if !have.iter().any(|each| each.as_ref() == model) {
                                this.push_model(model, window, cx);
                                have.push(model.clone().into());
                                added += 1;
                            }
                        }
                        Status::Fetched {
                            found: found.len(),
                            common: common.as_ref().map(Vec::len),
                            added,
                        }
                    }
                    // A service without a list of its models.
                    Some(Err(error)) if error.kind == ProviderErrorKind::NotFound => {
                        failed(t!("assistant.dialog.no_model_list"))
                    }
                    Some(Err(error)) => failed(provider_error(&error)),
                    None => failed(t!("assistant.service.unavailable")),
                };
                cx.notify();
            },
            window,
            cx,
        );
    }

    /// Save, with the key typed. Returns whether the dialog may close.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let draft = match self.draft(cx).validated() {
            Ok(draft) => draft,
            Err(error) => {
                window.push_notification(form_error_notification(draft_error(error)), cx);
                return false;
            }
        };
        let key = self.key.read(cx).value().trim().to_string();
        let editing = self.editing;
        self.store.update(cx, |store, cx| {
            let id = match editing {
                Some(id) => {
                    store.update_ai_provider(id, draft, cx);
                    id
                }
                None => store.insert_ai_provider(draft, cx),
            };
            if !key.is_empty()
                && let Some(secret) = store.ai_provider(id).map(|provider| provider.key_secret())
            {
                store.save_secret(secret, Some(key), cx);
            }
        });
        true
    }

    pub fn focus_name(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Where requests go, under the address: the whole URL once there is
    /// one, else the path the address is followed by.
    fn endpoint_hint(&self, cx: &App) -> SharedString {
        let protocol = core_protocol(self.protocol);
        let base_url = self.base_url.read(cx).value().trim().to_string();
        let typed = base_url.split_once("://").is_some_and(|(scheme, rest)| {
            matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") && !rest.is_empty()
        });
        if typed {
            t!(
                "assistant.dialog.endpoint",
                url = shellrs_assistant::endpoint_url(&base_url, protocol.path())
            )
        } else {
            t!("assistant.dialog.endpoint_path", path = protocol.path())
        }
    }

    /// 模型: its label with 刷新模型 and 添加模型 at the right, a row per
    /// model, and what 刷新模型 came to.
    fn models_field(&self, cx: &Context<Self>) -> impl IntoElement {
        let working = self.status.working();
        let header = h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .child(
                // Drawn as a field's label is.
                h_flex()
                    .gap_1()
                    .text_sm()
                    .font_medium()
                    .child(t!("assistant.dialog.models"))
                    .child(div().text_color(cx.theme().danger).child("*")),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("provider-fetch-models")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(CatalogIcon::RefreshCw))
                            .label(t!("assistant.dialog.fetch"))
                            .loading(self.status == Status::Working(Work::Fetch))
                            .disabled(working)
                            .on_click(cx.listener(|this, _, window, cx| this.fetch(window, cx))),
                    )
                    .child(
                        Button::new("provider-add-model")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Plus)
                            .label(t!("assistant.dialog.add_model"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_model(window, cx)),
                            ),
                    ),
            );
        let list = if self.models.is_empty() {
            let empty = t!("assistant.dialog.no_models");
            div()
                .id("provider-models-empty")
                .test_support()
                .aria_label(empty.clone())
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(empty)
                .into_any_element()
        } else {
            let rows = self.models.iter().map(|row| {
                let id = row.id;
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&row.input).id(("provider-model", id)).small()),
                    )
                    .child(
                        Button::new(("provider-model-remove", id))
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(CatalogIcon::Trash))
                            .tooltip(t!("assistant.dialog.remove_model"))
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_model(id, cx))),
                    )
            });
            // The scrollbar on the part that does not scroll.
            div()
                .relative()
                .w_full()
                .child(
                    v_flex()
                        .id("provider-models")
                        .w_full()
                        .max_h(rems(12.))
                        .gap_1()
                        // The scrollbar's lane, so it never covers a button.
                        .pr_2()
                        .overflow_y_scroll()
                        .track_scroll(&self.models_scroll)
                        .children(rows),
                )
                .vertical_scrollbar(&self.models_scroll)
                .into_any_element()
        };
        v_flex()
            .w_full()
            .gap_1p5()
            .child(header)
            .child(list)
            .children(self.status_line(Work::Fetch, cx))
    }

    /// What `work` is doing or came to, while the status is its.
    fn status_line(&self, work: Work, cx: &App) -> Option<impl IntoElement + use<>> {
        if self.status.of() != Some(work) {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let (text, color, spinning) = match &self.status {
            Status::Idle => return None,
            Status::Working(Work::Check) => (t!("assistant.dialog.checking"), muted, true),
            Status::Working(Work::Fetch) => (t!("assistant.dialog.fetching"), muted, true),
            Status::Passed(model) => (
                t!("assistant.dialog.check_passed", model = model),
                cx.theme().success,
                false,
            ),
            Status::Fetched { found: 0, .. } => (t!("assistant.dialog.fetched_none"), muted, false),
            Status::Fetched {
                found,
                common: Some(0),
                ..
            } => (
                tn!("assistant.dialog.fetched_no_common", *found),
                muted,
                false,
            ),
            Status::Fetched {
                found,
                common: Some(common),
                added,
            } => (
                tn!(
                    "assistant.dialog.fetched_common",
                    *found,
                    common = common,
                    added = added
                ),
                cx.theme().success,
                false,
            ),
            Status::Fetched {
                found,
                common: None,
                added,
            } => (
                tn!("assistant.dialog.fetched", *found, added = added),
                cx.theme().success,
                false,
            ),
            Status::Failed(_, reason) => (reason.clone(), cx.theme().danger, false),
        };
        let id = match work {
            Work::Check => "provider-check-result",
            Work::Fetch => "provider-fetch-result",
        };
        Some(
            h_flex()
                .id(id)
                .test_support()
                .aria_label(text.clone())
                .gap_2()
                .text_sm()
                .text_color(color)
                .when(spinning, |line| line.child(Spinner::new().small()))
                .child(text),
        )
    }
}

impl Render for ProviderForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .w_full()
            .child(
                Form::new()
                    .child(
                        Field::new()
                            .label(t!("assistant.dialog.name"))
                            .required(true)
                            .child(Input::new(&self.name).id("provider-name").small()),
                    )
                    .child(
                        Field::new().label(t!("assistant.dialog.preset")).child(
                            Select::new(&self.preset_select)
                                .id("provider-preset")
                                .small()
                                .accessibility_label(t!("assistant.dialog.preset")),
                        ),
                    )
                    .child(
                        Field::new().label(t!("assistant.dialog.protocol")).child(
                            Select::new(&self.protocol_select)
                                .id("provider-protocol")
                                .small()
                                .accessibility_label(t!("assistant.dialog.protocol")),
                        ),
                    )
                    .child(
                        Field::new()
                            .label(t!("assistant.dialog.base_url"))
                            .required(true)
                            .description_fn({
                                let endpoint = self.endpoint_hint(cx);
                                move |_, _| {
                                    div()
                                        .id("provider-endpoint")
                                        .test_support()
                                        .aria_label(endpoint.clone())
                                        .child(endpoint.clone())
                                }
                            })
                            .child(Input::new(&self.base_url).id("provider-base-url").small()),
                    )
                    .child(
                        Field::new().label(t!("assistant.dialog.key")).child(
                            Input::new(&self.key)
                                .id("provider-key")
                                .small()
                                .mask_toggle()
                                .content_type(InputContentType::Password),
                        ),
                    )
                    .child(
                        Field::new()
                            .label_indent(false)
                            .child(self.models_field(cx)),
                    ),
            )
            // By 测试连接, in the footer below.
            .children(self.status_line(Work::Check, cx))
    }
}

fn preset_index(preset: Preset) -> usize {
    Preset::ALL
        .iter()
        .position(|each| *each == preset)
        .unwrap_or_default()
}

fn protocol_index(protocol: AiProtocol) -> usize {
    PROTOCOLS
        .iter()
        .position(|each| *each == protocol)
        .unwrap_or_default()
}

pub fn open_provider_dialog(
    dialog: ProviderDialog,
    store: Entity<HostStore>,
    service: Entity<AssistantService>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| ProviderForm::new(dialog, store, service, window, cx));
    let editing = matches!(dialog, ProviderDialog::Edit(_));
    let title = if editing {
        t!("assistant.dialog.title_edit")
    } else {
        t!("assistant.dialog.title_new")
    };
    let commit_label = if editing {
        t!("common.save")
    } else {
        t!("assistant.dialog.create")
    };
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, cx| {
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .child(form.clone())
                .footer(form_footer(&form, commit_label.clone(), cx))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    if !editing {
        form.update(cx, |form, cx| form.focus_name(window, cx));
    }
}

/// 测试连接 on the left, 「取消」 and the commit on the right, as in 新建主机.
fn form_footer(form: &Entity<ProviderForm>, commit_label: SharedString, cx: &App) -> DialogFooter {
    let status = &form.read(cx).status;
    DialogFooter::new()
        .w_full()
        .justify_between()
        .child(
            Button::new("provider-check")
                .label(t!("assistant.dialog.check"))
                .icon(CatalogIcon::Plug)
                .small()
                .loading(*status == Status::Working(Work::Check))
                .disabled(status.working())
                .on_click({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.check(window, cx))
                }),
        )
        // The dialog's own parts send 取消 and the commit from inside it,
        // wherever the focus is.
        .child(
            h_flex()
                .gap_2()
                .child(
                    DialogClose::new()
                        .child(Button::new("cancel").label(t!("common.cancel")).small()),
                )
                .child(
                    DialogAction::new()
                        .child(Button::new("commit").primary().label(commit_label).small()),
                ),
        )
}
