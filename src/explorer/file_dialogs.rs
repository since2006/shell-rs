//! The delete confirmation and the name form shared by 重命名 and 新建.

use super::{ExplorerPanel, NewEntryKind, PaneOperation};
use crate::app::ExplorerCommand;
use crate::i18n::{t, tn};
use crate::shared::{commit_footer, dismiss_form_error, form_error_notification};
use gpui_kit::component::{
    Sizable as _, WindowExt as _,
    button::ButtonVariant,
    dialog::DialogButtonProps,
    form::{Field, Form},
    input::{Input, InputState},
    v_flex,
};
use gpui_kit::*;

/// Check a name typed for a new or renamed item. `existing` is the current
/// listing; the server or file system still has the final word.
pub fn validate_entry_name(
    name: &str,
    original: Option<&str>,
    existing: &[String],
) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(t!("explorer.name.empty").into());
    }
    if matches!(name, "." | "..") {
        return Err(t!("explorer.name.dots").into());
    }
    if name.contains(['/', '\0']) {
        return Err(t!("explorer.name.slash").into());
    }
    if original == Some(name) {
        return Err(t!("explorer.name.unchanged").into());
    }
    if existing.iter().any(|existing| existing == name) {
        return Err(t!("explorer.name.exists", name = name).into());
    }
    Ok(())
}

/// What the name form commits.
#[derive(Clone)]
enum NameIntent {
    Rename(String),
    New(NewEntryKind),
}

struct NameForm {
    input: Entity<InputState>,
    label: SharedString,
}
impl Render for NameForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_2().child(
            Form::new().child(
                Field::new()
                    .label(self.label.clone())
                    .required(true)
                    .child(Input::new(&self.input).id("entry-name").small()),
            ),
        )
    }
}

impl ExplorerPanel {
    /// 删除: confirm with the scope and consequence, then delete. Local
    /// items go to the Trash; remote ones are gone for good.
    pub(super) fn confirm_delete(
        &mut self,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let names = self.pane(remote).read(cx).selected_names(cx);
        if names.is_empty() || !self.can_modify(remote, cx) || window.has_active_dialog(cx) {
            return;
        }
        let title = if let [name] = names.as_slice() {
            t!("explorer.delete.title_one", name = name)
        } else {
            tn!("explorer.delete.title_several", names.len())
        };
        let description = if remote {
            t!("explorer.delete.remote")
        } else if cfg!(windows) {
            t!("explorer.delete.recycle_bin")
        } else {
            t!("explorer.delete.trash")
        };
        let sender = self.sender();
        let focus = window.focused(cx);
        window.open_alert_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(t!("common.delete"))
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(t!("common.cancel")),
                )
                .show_cancel(true)
                .on_cancel({
                    let focus = focus.clone();
                    move |_, window, cx| {
                        if let Some(focus) = &focus {
                            window.focus(focus, cx);
                        }
                        true
                    }
                })
                .on_ok({
                    let (sender, focus, names) = (sender.clone(), focus.clone(), names.clone());
                    move |_, window, cx| {
                        let operation = PaneOperation::Delete(names.clone());
                        sender.send(ExplorerCommand::Operate { remote, operation }, window, cx);
                        if let Some(focus) = &focus {
                            window.focus(focus, cx);
                        }
                        true
                    }
                })
        });
    }

    pub(super) fn open_rename(
        &mut self,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let [name] = self.pane(remote).read(cx).selected_names(cx).as_slice() {
            let name = name.clone();
            self.open_name_dialog(remote, NameIntent::Rename(name), window, cx);
        }
    }

    pub(super) fn open_new(
        &mut self,
        remote: bool,
        kind: NewEntryKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_name_dialog(remote, NameIntent::New(kind), window, cx);
    }

    fn open_name_dialog(
        &mut self,
        remote: bool,
        intent: NameIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_modify(remote, cx) || window.has_active_dialog(cx) {
            return;
        }
        let existing: Vec<String> = self
            .pane(remote)
            .read(cx)
            .listed_entries(cx)
            .iter()
            .filter(|entry| !entry.is_parent())
            .map(|entry| entry.name.to_string())
            .collect();
        let (title, initial, commit, label) = match &intent {
            NameIntent::Rename(name) => (
                t!("explorer.rename.title", name = name),
                name.clone(),
                t!("explorer.rename.commit"),
                t!("explorer.rename.label"),
            ),
            NameIntent::New(kind) => (
                kind.title(),
                kind.default_name().to_string(),
                t!("explorer.new.commit"),
                t!("explorer.new.label"),
            ),
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let form = cx.new(|_| NameForm { input, label });
        let sender = self.sender();
        let focus = window.focused(cx);
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit.clone()))
                .on_ok({
                    let (form, sender, intent, existing) = (
                        form.clone(),
                        sender.clone(),
                        intent.clone(),
                        existing.clone(),
                    );
                    move |_, window, cx| {
                        let name = form.read(cx).input.read(cx).value().to_string();
                        let original = match &intent {
                            NameIntent::Rename(from) => Some(from.as_str()),
                            NameIntent::New(_) => None,
                        };
                        if let Err(error) = validate_entry_name(&name, original, &existing) {
                            window.push_notification(form_error_notification(error), cx);
                            return false;
                        }
                        let operation = match &intent {
                            NameIntent::Rename(from) => PaneOperation::Rename {
                                from: from.clone(),
                                to: name,
                            },
                            NameIntent::New(kind) => PaneOperation::Create { kind: *kind, name },
                        };
                        sender.send(ExplorerCommand::Operate { remote, operation }, window, cx);
                        true
                    }
                })
                .on_close({
                    let focus = focus.clone();
                    move |_, window, cx| {
                        dismiss_form_error(window, cx);
                        if let Some(focus) = &focus {
                            window.focus(focus, cx);
                        }
                    }
                })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::validate_entry_name;

    #[test]
    fn names_are_checked_against_the_listing() {
        let existing = vec!["a.txt".to_string(), "目录".to_string()];
        assert!(validate_entry_name("b.txt", None, &existing).is_ok());
        assert_eq!(
            validate_entry_name("  ", None, &existing).unwrap_err(),
            "名称不能为空"
        );
        assert!(validate_entry_name("..", None, &existing).is_err());
        assert!(validate_entry_name("a/b", None, &existing).is_err());
        assert_eq!(
            validate_entry_name("a.txt", Some("a.txt"), &existing).unwrap_err(),
            "名称未改变"
        );
        assert_eq!(
            validate_entry_name("目录", Some("a.txt"), &existing).unwrap_err(),
            "已有名为「目录」的项目"
        );
        assert!(validate_entry_name("A.txt", Some("a.txt"), &existing).is_ok());
    }
}
