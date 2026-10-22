//! 发送命令: a dialog of lines to send to one terminal, one line and one
//! character at a time while 发送延迟 asks for waits.

use gpui_kit::component::{
    ActiveTheme as _, WindowExt as _,
    form::{Field, Form},
    input::{Textarea, TextareaState},
};
use gpui_kit::*;

use crate::i18n::t;
use crate::shared::{commit_footer, dismiss_form_error, form_error_notification};

use super::TerminalView;

/// How wide the dialog is: room for a script of a fair length.
const DIALOG_WIDTH: f32 = 42.;

/// The body of the dialog: the lines, and the terminal they go to.
pub struct SendCommandsForm {
    terminal: Entity<TerminalView>,
    text: Entity<TextareaState>,
}

impl SendCommandsForm {
    fn new(terminal: Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 12)
                .placeholder(t!("terminal.send_commands.placeholder"))
        });
        Self { terminal, text }
    }

    /// Send the lines, or say why not. Returns whether the dialog may close.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.text.read(cx).value().to_string();
        if text.trim().is_empty() {
            window.push_notification(
                form_error_notification(t!("terminal.send_commands.empty")),
                cx,
            );
            return false;
        }
        self.terminal
            .update(cx, |terminal, cx| terminal.send_lines(&text, cx));
        true
    }
}

impl Render for SendCommandsForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Form::new().child(
            Field::new()
                .label(t!("terminal.send_commands.lines"))
                .required(true)
                .description(t!("terminal.send_commands.description"))
                .child(
                    div()
                        .id("send-commands-text")
                        .test_support()
                        .w_full()
                        .child(
                            Textarea::new(&self.text)
                                .text_sm()
                                .font_family(cx.theme().mono_font_family.clone()),
                        ),
                ),
        )
    }
}

/// Open the dialog for `terminal`, ready to type into.
pub fn open_send_commands_dialog(
    terminal: Entity<TerminalView>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| SendCommandsForm::new(terminal, window, cx));
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, window, _| {
            dialog
                .title(t!("terminal.send_commands.title"))
                // Dialog geometry is an API boundary that takes `Pixels`; the
                // width follows the interface zoom through the rem.
                .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", t!("terminal.send_commands.send")))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    let text = form.read(cx).text.clone();
    text.update(cx, |text, cx| text.focus(window, cx));
}
