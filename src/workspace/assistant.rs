//! AI 助手's commands: the model services and 批准方式 of 设置 › AI 助手,
//! and the panel's, which go on to the panel beside the terminal in front.

use std::rc::Rc;

use gpui_kit::component::{WindowExt as _, button::ButtonVariant, dialog::DialogButtonProps};
use gpui_kit::*;

use super::Workspace;
use crate::app::{
    ContinueAssistant, DeleteAiProvider, DeleteAllAssistantConversations, DenyAssistantCall,
    EditAiProvider, InsertAssistantCall, NewAiProvider, NewAssistantConversation,
    OpenAssistantConversation, OpenAssistantLink, OpenAssistantSettings, OpenSettings,
    RunAssistantCall, SendAssistantMessage, SetAssistantApproval, SetConversationApproval,
    SetConversationRunIn, SetDefaultAiModel, StopAssistant, ToggleAssistantCallDetails, UseAiModel,
};
use crate::assistant::{AssistantPanel, AssistantServices, ProviderDialog, open_provider_dialog};
use crate::i18n::t;
use crate::settings::AssistantApproval;
use crate::shared::{DeleteHandler, confirm_danger};

impl Workspace {
    /// Use `services` for AI 助手: ShellRS's own, or a test's.
    pub fn set_assistant_services(
        &mut self,
        services: Result<AssistantServices, String>,
        cx: &mut Context<Self>,
    ) {
        self.assistant
            .update(cx, |assistant, cx| assistant.set_services(services, cx));
    }

    fn assistant_panel(&self, cx: &App) -> Entity<AssistantPanel> {
        self.tools.read(cx).assistant().clone()
    }

    pub(super) fn on_new_ai_provider(
        &mut self,
        _: &NewAiProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_provider_dialog(
            ProviderDialog::New,
            self.store.clone(),
            self.assistant.clone(),
            window,
            cx,
        );
    }

    /// 打开设置 in the panel that says AI 助手 is off: 设置 › AI 助手.
    pub(super) fn on_open_assistant_settings(
        &mut self,
        _: &OpenAssistantSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.on_open_settings(&OpenSettings, window, cx);
        if let Some(panel) = self.settings_tab().cloned() {
            panel.update(cx, |panel, cx| panel.show_assistant(cx));
        }
    }

    pub(super) fn on_edit_ai_provider(
        &mut self,
        action: &EditAiProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).ai_provider(action.0).is_some() {
            open_provider_dialog(
                ProviderDialog::Edit(action.0),
                self.store.clone(),
                self.assistant.clone(),
                window,
                cx,
            );
        }
    }

    pub(super) fn on_delete_ai_provider(
        &mut self,
        action: &DeleteAiProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(provider) = self.store.read(cx).ai_provider(action.0).cloned() else {
            return;
        };
        let (store, settings, id) = (self.store.clone(), self.settings().clone(), action.0);
        let public_id = provider.public_id.to_string();
        confirm_danger(
            t!("assistant.delete_provider.title", name = provider.name),
            Some(t!("assistant.delete_provider.description")),
            t!("common.delete"),
            Rc::new(move |_, cx| {
                store.update(cx, |store, cx| store.remove_ai_provider(id, cx));
                settings.update(cx, |settings, cx| {
                    if settings.settings().assistant.provider.as_deref() == Some(public_id.as_str())
                    {
                        settings.update(
                            |settings| {
                                settings.assistant.provider = None;
                                settings.assistant.model = None;
                            },
                            cx,
                        );
                    }
                });
            }),
            window,
            cx,
        );
    }

    /// 默认模型: the model new conversations use.
    pub(super) fn on_set_default_ai_model(
        &mut self,
        action: &SetDefaultAiModel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choice = action.0.clone();
        self.settings().update(cx, |settings, cx| {
            settings.update(
                |settings| {
                    settings.assistant.provider = Some(choice.provider);
                    settings.assistant.model = Some(choice.model);
                },
                cx,
            )
        });
    }

    /// Full access is asked about first: nothing is asked about after it.
    fn confirm_full_access(
        approval: AssistantApproval,
        current: AssistantApproval,
        then: DeleteHandler,
        window: &mut Window,
        cx: &mut App,
    ) {
        if approval == AssistantApproval::FullAccess && current != AssistantApproval::FullAccess {
            confirm_danger(
                t!("assistant.full_access.title"),
                Some(t!("assistant.full_access.description")),
                t!("assistant.full_access.confirm"),
                then,
                window,
                cx,
            );
        } else {
            then(window, cx);
        }
    }

    pub(super) fn on_set_assistant_approval(
        &mut self,
        action: &SetAssistantApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = self.settings().clone();
        let current = settings.read(cx).settings().assistant.approval;
        let approval = action.0;
        Self::confirm_full_access(
            approval,
            current,
            Rc::new(move |_, cx| {
                settings.update(cx, |settings, cx| {
                    settings.update(|settings| settings.assistant.approval = approval, cx)
                });
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_set_conversation_approval(
        &mut self,
        action: &SetConversationApproval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panel = self.assistant_panel(cx);
        let Some(current) = panel.read(cx).approval().map(AssistantApproval::of) else {
            return;
        };
        let approval = action.0;
        Self::confirm_full_access(
            approval,
            current,
            Rc::new(move |_, cx| {
                panel.update(cx, |panel, cx| panel.set_approval(approval.core(), cx));
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_set_conversation_run_in(
        &mut self,
        action: &SetConversationRunIn,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let run_in = action.0.core();
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.set_run_in(run_in, cx));
    }

    pub(super) fn on_use_ai_model(
        &mut self,
        action: &UseAiModel,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choice = action.0.clone();
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.use_model(&choice, cx));
    }

    pub(super) fn on_send_assistant_message(
        &mut self,
        _: &SendAssistantMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.send(window, cx));
    }

    pub(super) fn on_stop_assistant(
        &mut self,
        _: &StopAssistant,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.stop(cx));
    }

    pub(super) fn on_continue_assistant(
        &mut self,
        _: &ContinueAssistant,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.go_on(cx));
    }

    pub(super) fn on_new_assistant_conversation(
        &mut self,
        _: &NewAssistantConversation,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.new_conversation(cx));
    }

    pub(super) fn on_open_assistant_conversation(
        &mut self,
        action: &OpenAssistantConversation,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let public_id = action.0.clone();
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.open_conversation(&public_id, cx));
    }

    pub(super) fn on_delete_all_assistant_conversations(
        &mut self,
        _: &DeleteAllAssistantConversations,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (service, panel) = (self.assistant.clone(), self.assistant_panel(cx));
        confirm_danger(
            t!("assistant.delete_all.title"),
            Some(t!("assistant.delete_all.description")),
            t!("assistant.delete_all.confirm"),
            Rc::new(move |_, cx| {
                if let Some(store) = service.read(cx).store() {
                    let _ = store.delete_all();
                }
                panel.update(cx, |panel, cx| panel.conversation_deleted(None, cx));
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_run_assistant_call(
        &mut self,
        action: &RunAssistantCall,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let RunAssistantCall { id, remember } = action.clone();
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.run_call(&id, remember, cx));
    }

    pub(super) fn on_deny_assistant_call(
        &mut self,
        action: &DenyAssistantCall,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0.clone();
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.deny_call(&id, cx));
    }

    pub(super) fn on_insert_assistant_call(
        &mut self,
        action: &InsertAssistantCall,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0.clone();
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.insert_call(&id, cx));
    }

    pub(super) fn on_toggle_assistant_call_details(
        &mut self,
        action: &ToggleAssistantCallDetails,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0.clone();
        self.assistant_panel(cx)
            .update(cx, |panel, cx| panel.toggle_details(&id, cx));
    }

    /// A link of an answer: shown first, as the model may have been led to
    /// write one by what it read; only web addresses open.
    pub(super) fn on_open_assistant_link(
        &mut self,
        action: &OpenAssistantLink,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let url = action.0.clone();
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            return;
        }
        window.open_alert_dialog(cx, move |alert, _, _| {
            let url = url.clone();
            alert
                .title(t!("assistant.link.title"))
                .description(url.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(t!("assistant.link.open"))
                        .ok_variant(ButtonVariant::Primary)
                        .cancel_text(t!("common.cancel")),
                )
                .show_cancel(true)
                .on_ok(move |_, _, cx| {
                    cx.open_url(&url);
                    true
                })
        });
    }
}
