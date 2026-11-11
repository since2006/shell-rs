//! The dialogs of the setup code: this sync's, shown as a QR code and as
//! text to copy, and another device's, pasted with the sync password to
//! join the sync it is of.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogClose, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputContentType, InputState, Textarea, TextareaState},
    notification::Notification,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeroize::Zeroizing;

use super::qr::Qr;
use super::sync_dialog::finish;
use super::{JobError, SyncService, check_password};
use crate::host::ErrorMessage as _;
use crate::i18n::t;
use crate::shared::{commit_footer, dismiss_form_error, form_error_notification};

/// How wide the QR code is drawn: wide enough for a phone to read the
/// longest code off the screen at arm's length.
const QR_SIDE: Rems = rems(20.);

/// What 同步配置码 shows.
enum Shown {
    /// The code is being made: the place is read once for it.
    Making,
    Made {
        code: SharedString,
        /// `None` for a code longer than a QR code holds.
        qr: Option<Rc<Qr>>,
    },
    Failed(JobError),
}

/// The body of 同步配置码.
pub struct SetupCodeView {
    shown: Shown,
    _making: Task<()>,
}

impl SetupCodeView {
    fn new(service: &Entity<SyncService>, cx: &mut Context<Self>) -> Self {
        let made = service.update(cx, |service, cx| service.setup_code(cx));
        let making = cx.spawn(async move |this, cx| {
            let made = made.await;
            this.update(cx, |this, cx| {
                this.shown = match made {
                    Ok(code) => Shown::Made {
                        qr: Qr::of(&code).map(Rc::new),
                        code: code.into(),
                    },
                    Err(error) => Shown::Failed(error),
                };
                cx.notify();
            })
            .ok();
        });
        Self {
            shown: Shown::Making,
            _making: making,
        }
    }

    /// The code, once made.
    fn code(&self) -> Option<SharedString> {
        match &self.shown {
            Shown::Made { code, .. } => Some(code.clone()),
            _ => None,
        }
    }
}

impl Render for SetupCodeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        match &self.shown {
            Shown::Making => div()
                .id("sync-code-making")
                .test_support()
                .text_sm()
                .text_color(muted)
                .child(t!("sync.code.making"))
                .into_any_element(),
            Shown::Failed(error) => {
                let text = t!("sync.code.failed", reason = error.message());
                div()
                    .id("sync-code-failed")
                    .test_support()
                    .aria_label(text.clone())
                    .text_sm()
                    .text_color(theme.danger)
                    .child(text)
                    .into_any_element()
            }
            Shown::Made { code, qr } => v_flex()
                .gap_3()
                .w_full()
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(t!("sync.code.show_description")),
                )
                .when_some(qr.as_ref(), |view, qr| {
                    view.child(
                        h_flex()
                            .w_full()
                            .justify_center()
                            .child(qr.element(QR_SIDE)),
                    )
                })
                .child(
                    // The code as text: what the button copies, and what
                    // the tests and screen readers take the QR code for.
                    div()
                        .id("sync-setup-code")
                        .test_support()
                        .aria_label(code.clone())
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(muted)
                        .truncate()
                        .child(code.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(t!("sync.code.warning")),
                )
                .into_any_element(),
        }
    }
}

/// The footer of 同步配置码: 关闭, and 复制配置码 once there is one. Copying
/// leaves the dialog open, for the QR code to be scanned as well.
fn show_footer(view: &Entity<SetupCodeView>, cx: &App) -> DialogFooter {
    let code = view.read(cx).code();
    DialogFooter::new()
        .child(DialogClose::new().trigger(|button| button.label(t!("common.close"))))
        .child(
            Button::new("sync-copy-code")
                .primary()
                .label(t!("sync.code.copy"))
                .disabled(code.is_none())
                .on_click(move |_, window, cx| {
                    let Some(code) = &code else {
                        return;
                    };
                    cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                    window.push_notification(Notification::success(t!("sync.code.copied")), cx);
                }),
        )
}

/// Open 同步配置码: this sync's setup code, for another device to join
/// with.
pub fn open_setup_code_dialog(service: Entity<SyncService>, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| SetupCodeView::new(&service, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        dialog
            .title(t!("sync.code.show_title"))
            .overlay_closable(false)
            .child(view.clone())
            .footer(show_footer(&view, cx))
    });
}

/// The body of 用配置码加入同步: the code another device shows, and the sync
/// password.
pub struct JoinCodeForm {
    service: Entity<SyncService>,
    code: Entity<TextareaState>,
    password: Entity<InputState>,
    working: bool,
}

impl JoinCodeForm {
    fn new(service: Entity<SyncService>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let code = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 8)
                .placeholder("shellrs-sync:…")
        });
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        Self {
            service,
            code,
            password,
            working: false,
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        self.code.update(cx, |state, cx| state.focus(window, cx));
    }

    /// Check what was pasted and typed, and start joining. Returns whether
    /// the dialog may close: never at once, the job closes it when it went
    /// through.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.working {
            return false;
        }
        let code = self.code.read(cx).value().to_string();
        let password = Zeroizing::new(self.password.read(cx).value().to_string());
        // What is wrong with the code is said before the password is
        // looked at: a second of deriving a key would end the same.
        let checked = if code.trim().is_empty() {
            Err(t!("sync.code.missing"))
        } else {
            shellrs_sync::check_setup_code(&code)
                .map_err(|error| JobError::from(error).message())
                .and_then(|()| check_password(&password).map_err(|error| error.message()))
        };
        if let Err(error) = checked {
            window.push_notification(form_error_notification(error), cx);
            return false;
        }
        let job = self.service.update(cx, |service, cx| {
            service.join_with_setup_code(code, password, cx)
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
}

impl Render for JoinCodeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        v_flex()
            .gap_3()
            .w_full()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(t!("sync.code.join_description")),
            )
            .child(
                Form::new()
                    .child(
                        Field::new()
                            .label(t!("sync.code.code"))
                            .required(true)
                            .child(
                                div().id("sync-code").test_support().w_full().child(
                                    Textarea::new(&self.code)
                                        .font_family(theme.mono_font_family.clone())
                                        .text_xs()
                                        .disabled(self.working),
                                ),
                            ),
                    )
                    .child(
                        Field::new()
                            .label(t!("sync.dialog.password"))
                            .required(true)
                            .child(
                                Input::new(&self.password)
                                    .id("sync-code-password")
                                    .small()
                                    .mask_toggle()
                                    .content_type(InputContentType::Password)
                                    .disabled(self.working),
                            ),
                    ),
            )
            .when(self.working, |view| {
                view.child(
                    div()
                        .id("sync-working")
                        .test_support()
                        .text_sm()
                        .text_color(muted)
                        .child(t!("sync.dialog.joining")),
                )
            })
    }
}

/// Open 用配置码加入同步.
pub fn open_join_code_dialog(service: Entity<SyncService>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| JoinCodeForm::new(service, window, cx));
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, _| {
            dialog
                .title(t!("sync.code.join_title"))
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("sync-code-commit", t!("sync.dialog.join")))
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
