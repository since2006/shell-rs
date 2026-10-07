//! The 重命名标签 dialog of remote terminal and SFTP tabs: one title field,
//! pre-filled with the tab's current label. Titles live as long as the tab,
//! like the rest of the dock layout.

use gpui_kit::component::{
    Sizable as _, WindowExt as _,
    form::{Field, Form},
    input::{Input, InputState},
};
use gpui_kit::*;

use super::commit_footer;
use crate::i18n::t;

/// A tab whose label can be replaced by a title of its own.
pub trait RenamableTab: Sized + 'static {
    /// What the tab shows without a title of its own.
    fn default_title(&self, cx: &App) -> SharedString;
    /// What the tab shows now.
    fn tab_title(&self, cx: &App) -> SharedString;
    /// Give the tab its own title, or `None` to go back to the default.
    fn set_custom_title(&mut self, title: Option<SharedString>, cx: &mut Context<Self>);
}

struct RenameTabForm<T: RenamableTab> {
    panel: Entity<T>,
    title: Entity<InputState>,
    default: SharedString,
}

impl<T: RenamableTab> RenameTabForm<T> {
    /// Apply the title. An empty field, or the default itself, returns the
    /// tab to its default title.
    fn commit(&mut self, cx: &mut Context<Self>) {
        let title = self.title.read(cx).value().trim().to_string();
        let custom = (!title.is_empty() && title != self.default.as_ref()).then(|| title.into());
        self.panel
            .update(cx, |panel, cx| panel.set_custom_title(custom, cx));
    }
}

impl<T: RenamableTab> Render for RenameTabForm<T> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Form::new().child(
            Field::new()
                .label(t!("shared.rename_tab.name"))
                .description(t!("shared.rename_tab.default", title = self.default))
                .child(Input::new(&self.title).id("tab-name").small()),
        )
    }
}

/// Open the dialog that renames one tab.
pub fn open_rename_tab_dialog<T: RenamableTab>(
    panel: Entity<T>,
    window: &mut Window,
    cx: &mut App,
) {
    let (current, default) = {
        let panel = panel.read(cx);
        (panel.tab_title(cx), panel.default_title(cx))
    };
    let form = cx.new(|cx| RenameTabForm {
        panel,
        title: cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(default.clone())
                .default_value(current)
        }),
        default,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(t!("shared.rename_tab.title"))
            // Closed by its buttons or Escape, not by a click beside it.
            .overlay_closable(false)
            .child(form.clone())
            .footer(commit_footer("commit", t!("common.save")))
            .on_ok({
                let form = form.clone();
                move |_, _, cx| {
                    form.update(cx, |form, cx| form.commit(cx));
                    true
                }
            })
    });
}
