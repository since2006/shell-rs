use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, TitleBar, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    form::{Field, Form},
    input::{Input, InputContentType, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeroize::Zeroizing;

use super::{AppLock, PasswordOperation};
use crate::i18n::t;

struct PasswordForm {
    service: Entity<AppLock>,
    operation: PasswordOperation,
    current: Entity<InputState>,
    new: Entity<InputState>,
    repeat: Entity<InputState>,
    error: Option<SharedString>,
    working: bool,
    dismissed: bool,
    _subscriptions: Vec<Subscription>,
}

impl PasswordForm {
    fn new(
        service: Entity<AppLock>,
        operation: PasswordOperation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let current = cx.new(|cx| InputState::new(window, cx).masked(true));
        let new = cx.new(|cx| InputState::new(window, cx).masked(true));
        let repeat = cx.new(|cx| InputState::new(window, cx).masked(true));
        let subscriptions = [&current, &new, &repeat]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.commit(window, cx);
                    }
                })
            })
            .collect();
        Self {
            service,
            operation,
            current,
            new,
            repeat,
            error: None,
            working: false,
            dismissed: false,
            _subscriptions: subscriptions,
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        let input = if self.operation == PasswordOperation::Enable {
            &self.new
        } else {
            &self.current
        };
        window.focus(&input.read(cx).focus_handle(cx), cx);
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.working {
            return false;
        }
        let current = Zeroizing::new(self.current.read(cx).value().to_string());
        let new = Zeroizing::new(self.new.read(cx).value().to_string());
        let sets_password = matches!(
            self.operation,
            PasswordOperation::Enable | PasswordOperation::Change
        );
        self.error = if sets_password && new.chars().count() < 8 {
            Some(t!("app_lock.too_short"))
        } else if sets_password && new.as_str() != self.repeat.read(cx).value().as_str() {
            Some(t!("app_lock.mismatch"))
        } else if self.operation != PasswordOperation::Enable && current.is_empty() {
            Some(t!("app_lock.enter_password"))
        } else {
            None
        };
        if self.error.is_some() {
            cx.notify();
            return false;
        }
        let job = self.service.update(cx, |service, cx| {
            service.submit(self.operation, current, new, cx)
        });
        self.working = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.working = false;
                match result {
                    Ok(()) => {
                        for input in [&this.current, &this.new, &this.repeat] {
                            input.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                                input.set_masked(true, window, cx);
                            });
                        }
                        if this.operation != PasswordOperation::Unlock && !this.dismissed {
                            window.close_dialog(cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        this.current.update(cx, |input, cx| {
                            input.set_value("", window, cx);
                            input.set_masked(true, window, cx);
                        });
                        this.focus(window, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        false
    }
}

impl Render for PasswordForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let input = |state: &Entity<InputState>, id: &'static str| {
            Input::new(state)
                .id(id)
                .mask_toggle()
                .content_type(InputContentType::Password)
                .disabled(self.working)
        };
        v_flex()
            .gap_3()
            .child(
                Form::new()
                    .when(self.operation != PasswordOperation::Enable, |form| {
                        form.child(
                            Field::new()
                                .label(t!("app_lock.current_password"))
                                .child(input(&self.current, "app-lock-password")),
                        )
                    })
                    .when(
                        matches!(
                            self.operation,
                            PasswordOperation::Enable | PasswordOperation::Change
                        ),
                        |form| {
                            form.child(
                                Field::new()
                                    .label(t!("app_lock.new_password"))
                                    .description(t!("app_lock.password_help"))
                                    .child(input(&self.new, "app-lock-new-password")),
                            )
                            .child(
                                Field::new()
                                    .label(t!("app_lock.repeat_password"))
                                    .child(input(&self.repeat, "app-lock-repeat-password")),
                            )
                        },
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .id("app-lock-error")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when(self.operation == PasswordOperation::Unlock, |view| {
                view.child(
                    Button::new("app-lock-unlock")
                        .primary()
                        .label(t!("app_lock.unlock"))
                        .loading(self.working)
                        .disabled(self.working)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.commit(window, cx);
                        })),
                )
            })
    }
}

pub fn open_password_dialog(
    service: Entity<AppLock>,
    operation: PasswordOperation,
    window: &mut Window,
    cx: &mut App,
) {
    if operation == PasswordOperation::Unlock || service.read(cx).is_locked() {
        return;
    }
    let title = match operation {
        PasswordOperation::Enable => t!("app_lock.enable_title"),
        PasswordOperation::Change => t!("app_lock.change_title"),
        PasswordOperation::Disable => t!("app_lock.disable_title"),
        PasswordOperation::Unlock => unreachable!(),
    };
    let commit_label = match operation {
        PasswordOperation::Enable => t!("app_lock.enable_commit"),
        PasswordOperation::Change => t!("app_lock.change_commit"),
        PasswordOperation::Disable => t!("app_lock.disable_commit"),
        PasswordOperation::Unlock => unreachable!(),
    };
    let form = cx.new(|cx| PasswordForm::new(service, operation, window, cx));
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, cx| {
            let working = form.read(cx).working;
            let commit = form.clone();
            let cancel = form.clone();
            let close = form.clone();
            dialog
                .title(title.clone())
                .overlay_closable(false)
                .keyboard(!working)
                .close_button(!working)
                .child(form.clone())
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(move |button| {
                            button.label(t!("common.cancel")).disabled(working)
                        }))
                        .child(
                            DialogAction::new().child(
                                Button::new("app-lock-commit")
                                    .primary()
                                    .label(commit_label.clone())
                                    .disabled(working)
                                    .loading(working),
                            ),
                        ),
                )
                .on_ok(move |_, window, cx| commit.update(cx, |form, cx| form.commit(window, cx)))
                .on_cancel(move |_, _, cx| !cancel.read(cx).working)
                .on_close(move |_, _, cx| close.update(cx, |form, _| form.dismissed = true))
        }
    });
    form.update(cx, |form, cx| form.focus(window, cx));
}

/// The sole visible content while locked. There is no dismiss path to the workspace.
pub struct UnlockScreen {
    service: Entity<AppLock>,
    form: Entity<PasswordForm>,
    _subscription: Subscription,
}

impl UnlockScreen {
    pub fn new(service: Entity<AppLock>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let form =
            cx.new(|cx| PasswordForm::new(service.clone(), PasswordOperation::Unlock, window, cx));
        let subscription = cx.observe(&service, |_, _, cx| cx.notify());
        Self {
            service,
            form,
            _subscription: subscription,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.form.update(cx, |form, cx| form.focus(window, cx));
    }
}

impl Render for UnlockScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.service.read(cx).is_loading();
        let error = self
            .service
            .read(cx)
            .load_error()
            .map(|error| t!("app_lock.load_failed", error = error));
        v_flex()
            .id("app-lock-screen")
            .test_support()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().child("ShellRS"))
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .child(
                        v_flex()
                            .w_80()
                            .max_w_full()
                            .gap_4()
                            .child(
                                Icon::new(crate::app::CatalogIcon::Lock)
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(t!("app_lock.locked")),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(t!("app_lock.unlock_description")),
                            )
                            .map(|view| match (loading, error) {
                                (true, _) => view.child(
                                    div()
                                        .id("app-lock-loading")
                                        .test_support()
                                        .role(Role::Status)
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(t!("app_lock.loading")),
                                ),
                                (false, Some(error)) => view
                                    .child(
                                        div()
                                            .id("app-lock-load-error")
                                            .test_support()
                                            .role(Role::Alert)
                                            .aria_label(error.clone())
                                            .text_sm()
                                            .text_color(cx.theme().danger)
                                            .child(error),
                                    )
                                    .child(
                                        Button::new("app-lock-retry")
                                            .label(t!("app_lock.retry"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.service
                                                    .update(cx, |service, cx| service.reload(cx))
                                            })),
                                    ),
                                (false, None) => view.child(self.form.clone()),
                            }),
                    ),
            )
    }
}
