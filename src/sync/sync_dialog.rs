//! The dialogs of 同步: the ones that take a sync password (setting sync
//! up, joining it, changing the password) and turning it off.
//!
//! Each runs its job on the sync service and stays open with the user's
//! input until it is done: closed when it went through, the reason shown
//! over it when not.

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::{DialogAction, DialogClose, DialogFooter},
    form::{Field, Form},
    input::{Input, InputContentType, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeroize::Zeroizing;

use super::{
    JobError, SyncLocation, SyncService, check_new_password, check_password, random_password,
};
use crate::host::ErrorMessage as _;
use crate::i18n::t;
use crate::shared::{commit_footer, dismiss_form_error, form_error_notification};

/// What a password dialog does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordDialog {
    /// 创建同步, where there is none.
    Create { location: SyncLocation },
    /// 加入同步, where there is one.
    Join {
        location: SyncLocation,
        reason: JoinReason,
    },
    /// 更换同步密码, and the data key with it.
    Change,
}

/// Why the sync password is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinReason {
    /// This computer joins the sync in the folder it picked.
    First,
    /// The password was changed on another device.
    NewPassword,
    /// Sync was reset on another device.
    Reset,
}

impl PasswordDialog {
    /// Whether the dialog sets a new password, typed twice.
    fn is_new(&self) -> bool {
        !matches!(self, Self::Join { .. })
    }

    fn title(&self) -> SharedString {
        match self {
            Self::Create { .. } => t!("sync.dialog.create_title"),
            Self::Join {
                reason: JoinReason::First,
                ..
            } => t!("sync.dialog.join_title"),
            Self::Join {
                reason: JoinReason::NewPassword,
                ..
            } => t!("sync.dialog.new_password_title"),
            Self::Join {
                reason: JoinReason::Reset,
                ..
            } => t!("sync.dialog.reset_title"),
            Self::Change => t!("sync.dialog.change_title"),
        }
    }

    fn description(&self) -> SharedString {
        match self {
            Self::Create { .. } => t!("sync.dialog.create_description"),
            Self::Join {
                reason: JoinReason::First,
                ..
            } => t!("sync.dialog.join_description"),
            Self::Join {
                reason: JoinReason::NewPassword,
                ..
            } => t!("sync.dialog.new_password_description"),
            Self::Join {
                reason: JoinReason::Reset,
                ..
            } => t!("sync.dialog.reset_description"),
            Self::Change => t!("sync.dialog.change_description"),
        }
    }

    fn commit_label(&self) -> SharedString {
        match self {
            Self::Create { .. } => t!("sync.dialog.create"),
            Self::Join {
                reason: JoinReason::NewPassword,
                ..
            } => t!("sync.dialog.continue"),
            Self::Join { .. } => t!("sync.dialog.join"),
            Self::Change => t!("sync.dialog.change"),
        }
    }

    /// What the dialog says while its job runs.
    fn working(&self) -> SharedString {
        match self {
            Self::Create { .. } => t!("sync.dialog.creating"),
            Self::Join { .. } => t!("sync.dialog.joining"),
            Self::Change { .. } => t!("sync.dialog.changing"),
        }
    }
}

/// The body of a password dialog.
pub struct PasswordForm {
    service: Entity<SyncService>,
    dialog: PasswordDialog,
    /// The WebDAV server's password, from its dialog, for the keychain to
    /// keep once sync is set up.
    login: Option<Zeroizing<String>>,
    password: Entity<InputState>,
    /// The new password again.
    repeat: Option<Entity<InputState>>,
    working: bool,
}

impl PasswordForm {
    fn new(
        service: Entity<SyncService>,
        dialog: PasswordDialog,
        login: Option<Zeroizing<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).masked(true))
        };
        let password = field(window, cx);
        let repeat = dialog.is_new().then(|| field(window, cx));
        Self {
            service,
            dialog,
            login,
            password,
            repeat,
            working: false,
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        self.password
            .update(cx, |state, cx| state.focus(window, cx));
    }

    /// Check the password and start the job. Returns whether the dialog may
    /// close: never at once, the job closes it when it went through.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.working {
            return false;
        }
        let password = Zeroizing::new(self.password.read(cx).value().to_string());
        let checked = match &self.repeat {
            Some(repeat) => check_new_password(&password, &repeat.read(cx).value()),
            None => check_password(&password),
        };
        if let Err(error) = checked {
            window.push_notification(form_error_notification(error.message()), cx);
            return false;
        }
        let (dialog, login) = (self.dialog.clone(), self.login.clone());
        let job = self.service.update(cx, |service, cx| match dialog {
            PasswordDialog::Create { location } => service.create(location, login, password, cx),
            PasswordDialog::Join { location, reason } => service.join(
                location,
                login,
                password,
                reason != JoinReason::NewPassword,
                cx,
            ),
            PasswordDialog::Change => service.change_password(password, cx),
        });
        self.working = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.working = false;
                cx.notify();
                finish(result, window, cx);
            })
            .ok();
        })
        .detach();
        false
    }

    /// Fill both fields with a new random password, shown, for the user to
    /// keep somewhere.
    fn generate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let password = random_password();
        for field in std::iter::once(&self.password).chain(&self.repeat) {
            field.update(cx, |state, cx| {
                state.set_value(password.as_str().to_owned(), window, cx);
                state.set_masked(false, window, cx);
            });
        }
    }
}

/// Close the dialog when its job went through; say why not when it did
/// not.
fn finish(result: Result<(), JobError>, window: &mut Window, cx: &mut App) {
    match result {
        Ok(()) => {
            // Closing it from here skips the dialog's `on_close`.
            dismiss_form_error(window, cx);
            window.close_dialog(cx);
        }
        Err(error) => window.push_notification(form_error_notification(error.message()), cx),
    }
}

impl Render for PasswordForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let new = self.dialog.is_new();
        let password_label = if new {
            t!("sync.dialog.new_password")
        } else {
            t!("sync.dialog.password")
        };
        let input = |state: &Entity<InputState>, id: &'static str| {
            Input::new(state)
                .id(id)
                .small()
                .mask_toggle()
                .content_type(InputContentType::Password)
                .disabled(self.working)
        };
        let description = self.dialog.description();
        v_flex()
            .gap_3()
            .w_full()
            .child(
                div()
                    .id("sync-dialog-description")
                    .test_support()
                    .aria_label(description.clone())
                    .text_sm()
                    .text_color(muted)
                    .child(description),
            )
            .child(
                Form::new()
                    .child(
                        Field::new()
                            .label(password_label)
                            .required(true)
                            .child(input(&self.password, "sync-password")),
                    )
                    .when_some(self.repeat.as_ref(), |form, repeat| {
                        form.child(
                            Field::new()
                                .label(t!("sync.dialog.repeat"))
                                .required(true)
                                .child(input(repeat, "sync-password-repeat")),
                        )
                    }),
            )
            .when(new, |view| {
                view.child(
                    Button::new("sync-generate-password")
                        .outline()
                        .small()
                        .label(t!("sync.dialog.generate"))
                        .disabled(self.working)
                        .on_click(cx.listener(|this, _, window, cx| this.generate(window, cx))),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child(t!("sync.dialog.cannot_recover")),
                )
            })
            .when(self.working, |view| {
                view.child(
                    div()
                        .id("sync-working")
                        .test_support()
                        .text_sm()
                        .text_color(muted)
                        .child(self.dialog.working()),
                )
            })
    }
}

/// Open a password dialog. `login` is the WebDAV server's password when
/// its dialog came first.
pub fn open_password_dialog(
    service: Entity<SyncService>,
    dialog: PasswordDialog,
    login: Option<Zeroizing<String>>,
    window: &mut Window,
    cx: &mut App,
) {
    let (title, commit_label) = (dialog.title(), dialog.commit_label());
    let form = cx.new(|cx| PasswordForm::new(service, dialog, login, window, cx));
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, _| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("sync-commit", commit_label.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    // Focused in the same update that opened the dialog, which is the one
    // that sticks: a later request loses to the dialog's own focus.
    form.update(cx, |form, cx| form.focus(window, cx));
}

/// The body of 关闭同步: what stays, and whether this computer's file in
/// the folder goes too.
pub struct TurnOffForm {
    service: Entity<SyncService>,
    delete_file: bool,
    working: bool,
}

impl TurnOffForm {
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.working {
            return false;
        }
        let delete_file = self.delete_file;
        let job = self
            .service
            .update(cx, |service, cx| service.turn_off(delete_file, cx));
        self.working = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            this.update_in(cx, |this, window, cx| {
                this.working = false;
                cx.notify();
                finish(result, window, cx);
            })
            .ok();
        })
        .detach();
        false
    }
}

impl Render for TurnOffForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .w_full()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("sync.turn_off.description")),
            )
            .child(
                Checkbox::new("sync-delete-own-file")
                    .label(t!("sync.turn_off.delete_file"))
                    .checked(self.delete_file)
                    .small()
                    .disabled(self.working)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.delete_file = *checked;
                        cx.notify();
                    })),
            )
    }
}

/// Open 关闭同步.
pub fn open_turn_off_dialog(service: Entity<SyncService>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|_| TurnOffForm {
        service,
        delete_file: false,
        working: false,
    });
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, _| {
            dialog
                .title(t!("sync.turn_off.title"))
                .overlay_closable(false)
                .child(form.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            DialogClose::new().trigger(|button| button.label(t!("common.cancel"))),
                        )
                        .child(
                            DialogAction::new().child(
                                Button::new("sync-turn-off")
                                    .danger()
                                    .label(t!("sync.turn_off.commit")),
                            ),
                        ),
                )
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
}
