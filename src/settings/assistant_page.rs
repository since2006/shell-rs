//! 设置 › AI 助手: 启用 AI 助手; the model services, which sync, and this
//! device's own choices: the model new conversations use, 批准方式 and
//! 执行方式; and deleting every conversation.

use gpui_kit::component::{
    ActiveTheme as _, AxisExt as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::DropdownMenu as _,
    setting::{RenderOptions, SettingField, SettingGroup, SettingItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{AppSettings, AssistantApproval, AssistantRunIn, Choice, SettingsStore};
use crate::app::{
    AiModelChoice, DeleteAiProvider, DeleteAllAssistantConversations, EditAiProvider,
    NewAiProvider, SetAssistantApproval, SetDefaultAiModel,
};
use crate::assistant::{default_model, model_groups, model_items, protocol_label};
use crate::host::{AiProvider, HostStore};
use crate::i18n::{t, tn};

pub(super) fn assistant_groups(
    settings: &Entity<SettingsStore>,
    hosts: &Entity<HostStore>,
    choice_field: impl Fn(
        fn(&AppSettings) -> AssistantRunIn,
        fn(&mut AppSettings, AssistantRunIn),
    ) -> SettingField<SharedString>,
    cx: &App,
) -> Vec<SettingGroup> {
    let assistant = settings.read(cx).settings().assistant.clone();
    let providers = hosts.read(cx).ai_providers().to_vec();
    let (reader, writer) = (settings.clone(), settings.clone());
    let enabled_group = SettingGroup::new()
        .title(t!("settings.assistant.access"))
        .items([SettingItem::new(
            t!("settings.assistant.enabled"),
            SettingField::switch(
                move |cx| reader.read(cx).settings().assistant.enabled,
                move |enabled, cx| {
                    writer.update(cx, |store, cx| {
                        store.update(|settings| settings.assistant.enabled = enabled, cx)
                    });
                },
            )
            .default_value(false),
        )
        .description(t!("settings.assistant.enabled_description"))]);

    let mut items: Vec<SettingItem> = Vec::new();
    if let Some((provider, model)) = default_model(
        &providers,
        assistant.provider.as_deref(),
        assistant.model.as_deref(),
    ) {
        items.push(default_model_item(&providers, provider, model));
    }
    items.extend(providers.iter().map(provider_item));
    items.push(
        SettingItem::new(
            t!("settings.assistant.new_provider"),
            SettingField::render(|options, _, _| {
                Button::new("settings-new-ai-provider")
                    .outline()
                    .with_size(options.size())
                    .label(t!("settings.assistant.new_provider_button"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewAiProvider), cx))
            }),
        )
        .description(t!("settings.assistant.new_provider_description")),
    );
    let providers_group = SettingGroup::new()
        .title(t!("settings.assistant.providers"))
        .description(if providers.is_empty() {
            t!("settings.assistant.providers_empty")
        } else {
            t!("settings.assistant.providers_description")
        })
        .items(items);
    // Not a `SettingField::dropdown`: that one writes the choice at once, and
    // 完全访问权限 is confirmed first. It is drawn the same way.
    let approval = assistant.approval;
    let approval_group = SettingGroup::new()
        .title(t!("settings.assistant.approval"))
        .items([SettingItem::new(
            t!("settings.assistant.approval"),
            SettingField::render(move |options, _, _| {
                dropdown_button(
                    "settings-assistant-approval",
                    t!("settings.assistant.approval"),
                    approval.label(),
                    options,
                )
                .dropdown_menu_with_anchor(
                    Anchor::TopRight,
                    move |mut menu, _, _| {
                        for choice in AssistantApproval::ALL {
                            menu = menu.menu_with_check(
                                choice.label(),
                                *choice == approval,
                                Box::new(SetAssistantApproval(*choice)),
                            );
                        }
                        menu
                    },
                )
            }),
        )
        .description(approval.description())]);
    let run_in_group = SettingGroup::new()
        .title(t!("settings.assistant.run_in"))
        .items([SettingItem::new(
            t!("settings.assistant.run_in"),
            choice_field(
                |settings| settings.assistant.run_in,
                |settings, run_in| settings.assistant.run_in = run_in,
            ),
        )
        .description(t!("settings.assistant.run_in_description"))]);
    let conversations_group = SettingGroup::new()
        .title(t!("settings.assistant.conversations"))
        .items([SettingItem::new(
            t!("settings.assistant.delete_all"),
            SettingField::render(|options, _, _| {
                Button::new("settings-delete-assistant-conversations")
                    .outline()
                    .danger()
                    .with_size(options.size())
                    .label(t!("settings.assistant.delete_all_button"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(DeleteAllAssistantConversations), cx)
                    })
            }),
        )
        .description(t!("settings.assistant.delete_all_description"))]);
    vec![
        enabled_group,
        providers_group,
        approval_group,
        run_in_group,
        conversations_group,
    ]
}

/// 默认模型: every service's models to pick from, the one in use checked.
fn default_model_item(
    providers: &[AiProvider],
    provider: &AiProvider,
    model: SharedString,
) -> SettingItem {
    let label = t!(
        "assistant.model_of",
        provider = provider.name,
        model = model
    );
    let chosen = AiModelChoice {
        provider: provider.public_id.to_string(),
        model: model.to_string(),
    };
    let groups = model_groups(providers);
    SettingItem::new(
        t!("settings.assistant.default_model"),
        SettingField::render(move |options, _, _| {
            let (groups, chosen) = (groups.clone(), chosen.clone());
            dropdown_button(
                "settings-assistant-default-model",
                t!("settings.assistant.default_model"),
                label.clone(),
                options,
            )
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let menu = menu.max_h(px(420.)).scrollable(true);
                model_items(menu, &groups, Some(&chosen), |choice| {
                    Box::new(SetDefaultAiModel(choice))
                })
            })
        }),
    )
    .description(t!("settings.assistant.default_model_description"))
}

/// The button of a dropdown whose menu is our own (models grouped by service,
/// a choice confirmed first), drawn like `SettingField::dropdown`'s: the
/// caret, the menu opening to the left, and 「标题: 值」 for its name.
fn dropdown_button(
    id: &'static str,
    title: SharedString,
    value: SharedString,
    options: &RenderOptions,
) -> Button {
    Button::new(id)
        .when(options.layout().is_vertical(), |this| this.w_full())
        .accessibility_label(format!("{title}: {value}"))
        .label(value)
        .dropdown_caret(true)
        .outline()
        .disabled(options.is_disabled())
        .with_size(options.size())
}

fn provider_item(provider: &AiProvider) -> SettingItem {
    let id = provider.id;
    let detail = tn!(
        "settings.assistant.provider_detail",
        provider.models.len(),
        protocol = protocol_label(provider.protocol),
        address = provider.base_url
    );
    SettingItem::new(
        provider.name.clone(),
        SettingField::render(move |options, _, cx| {
            h_flex()
                .gap_1()
                .child(
                    Button::new(SharedString::from(format!("ai-provider-edit-{}", id.0)))
                        .outline()
                        .with_size(options.size())
                        .label(t!("settings.assistant.edit"))
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(Box::new(EditAiProvider(id)), cx)
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("ai-provider-delete-{}", id.0)))
                        .ghost()
                        .with_size(options.size())
                        .text_color(cx.theme().danger)
                        .label(t!("common.delete"))
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(Box::new(DeleteAiProvider(id)), cx)
                        }),
                )
        }),
    )
    .description(detail)
}
