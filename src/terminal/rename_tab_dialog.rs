use gpui_kit::component::{
    Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    form::{Field, Form},
    input::{Input, InputState},
};
use gpui_kit::*;

use super::TerminalPanel;

/// The body of the rename-tab dialog: one title field, pre-filled with the
/// tab's current label.
pub struct RenameTabForm {
    panel: Entity<TerminalPanel>,
    title: Entity<InputState>,
}

impl RenameTabForm {
    pub fn new(panel: Entity<TerminalPanel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (current, session_name) = {
            let panel = panel.read(cx);
            (panel.title_text(cx), panel.session_name(cx))
        };
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(session_name)
                .default_value(current)
        });
        Self { panel, title }
    }

    /// Apply the title. An empty field, or the session name itself, returns
    /// the tab to following the session name.
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        let title = self.title.read(cx).value().trim().to_string();
        self.panel.update(cx, |panel, cx| {
            let custom = (!title.is_empty() && title != panel.session_name(cx).as_ref())
                .then(|| title.into());
            panel.set_custom_title(custom, cx);
        });
    }
}

impl Render for RenameTabForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Form::new().child(
            Field::new()
                .label("标签名称")
                .description("留空则使用会话名称。")
                .child(Input::new(&self.title).id("tab-name").small()),
        )
    }
}

/// Open the dialog that renames one remote terminal tab.
pub fn open_rename_tab_dialog(panel: Entity<TerminalPanel>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| RenameTabForm::new(panel, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title("重命名标签")
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.label("取消")))
                    .child(
                        DialogAction::new().child(Button::new("commit").primary().label("保存")),
                    ),
            )
            .on_ok({
                let form = form.clone();
                move |_, _, cx| {
                    form.update(cx, |form, cx| form.commit(cx));
                    true
                }
            })
    });
}
