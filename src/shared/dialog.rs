//! The pieces every form dialog is put together from.

use gpui_kit::component::{
    ActiveTheme as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
};
use gpui_kit::*;

/// A form dialog's footer: 取消, and the button that commits the form.
pub fn commit_footer(id: &'static str, label: impl Into<SharedString>) -> DialogFooter {
    DialogFooter::new()
        .child(DialogClose::new().trigger(|button| button.label("取消")))
        .child(DialogAction::new().child(Button::new(id).primary().label(label)))
}

/// The line under a form that says why it was not accepted.
pub fn form_error(
    error: impl Into<SharedString>,
    cx: &App,
) -> gpui_kit::base::ObservedElement<Stateful<Div>> {
    div()
        .id("form-error")
        .test_support()
        .text_sm()
        .text_color(cx.theme().danger)
        .child(error.into())
}
