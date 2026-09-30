//! The pieces every form dialog is put together from.

use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter},
};
use gpui_kit::prelude::FluentBuilder as _;
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

/// A port field's value, when it is one: a number from 1 to 65535.
pub fn parse_port(port: &str) -> Option<u16> {
    port.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

/// What runs when the user confirms a deletion.
pub type DeleteHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The confirmation every deletion of a saved thing shares: 删除“名称”？ with
/// a danger button. `description` says what goes with it, when anything does.
pub fn confirm_delete(
    name: &str,
    description: Option<SharedString>,
    on_delete: DeleteHandler,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = format!("删除“{name}”？").into();
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(title.clone())
            .when_some(description.clone(), |alert, description| {
                alert.description(description)
            })
            .button_props(
                DialogButtonProps::default()
                    .ok_text("删除")
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text("取消"),
            )
            .show_cancel(true)
            .on_ok({
                let on_delete = on_delete.clone();
                move |_, window, cx| {
                    on_delete(window, cx);
                    true
                }
            })
    });
}

#[cfg(test)]
mod tests {
    use super::parse_port;

    #[test]
    fn a_port_field_is_a_number_from_1_to_65535() {
        assert_eq!(parse_port("22"), Some(22));
        assert_eq!(parse_port(" 8080 "), Some(8080));
        assert_eq!(parse_port("65535"), Some(65535));
        for port in ["0", "65536", "ssh", ""] {
            assert_eq!(parse_port(port), None, "{port:?}");
        }
    }
}
