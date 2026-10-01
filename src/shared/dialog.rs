//! The pieces every form dialog is put together from.

use std::rc::Rc;

use gpui_kit::component::{
    WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter},
    notification::{Notification, NotificationType},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// A form dialog's footer: 取消, and the button that commits the form.
pub fn commit_footer(id: &'static str, label: impl Into<SharedString>) -> DialogFooter {
    DialogFooter::new()
        .child(DialogClose::new().trigger(|button| button.label("取消")))
        .child(DialogAction::new().child(Button::new(id).primary().label(label)))
}

/// Marks the notification a form's error is shown in, so a newer error
/// replaces the last one rather than stacking under it.
struct FormErrorNotification;

/// Why a form was not accepted, as an error notification over the dialog:
/// every add and edit dialog says it this way, not in a line under the form.
/// Its text has the `form-error` id, with the message as its label. A click
/// puts it away, and so does the dialog when it closes (each dialog calls
/// `dismiss_form_error` from its `on_close`).
pub fn form_error_notification(error: impl Into<SharedString>) -> Notification {
    let error = error.into();
    Notification::new()
        .with_type(NotificationType::Error)
        .id::<FormErrorNotification>()
        // Clicking a notification dismisses it once it has a click handler.
        .on_click(|_, _, _| {})
        .content(move |_, _, _| {
            div()
                .id("form-error")
                .test_support()
                .aria_label(error.clone())
                .text_sm()
                .child(error.clone())
                .into_any_element()
        })
}

/// Take a form's error notification away once its dialog has closed: the
/// error is about a form that is gone.
pub fn dismiss_form_error(window: &mut Window, cx: &mut App) {
    window.remove_notification::<FormErrorNotification>(cx);
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
