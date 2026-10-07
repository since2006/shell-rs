//! The workspace's share of 命令片段: the commands of the snippet list,
//! which open the dialogs and ask before deleting.

use std::rc::Rc;

use gpui_kit::*;

use crate::app::{
    DeleteSnippet, DeleteSnippetCategory, EditSnippet, NewSnippet, NewSnippetCategory,
    NewSnippetIn, RenameSnippetCategory, ToggleSnippetCategory,
};
use crate::i18n::tn;
use crate::shared::confirm_delete;
use crate::snippets::{CategoryDialog, SnippetDialog, open_category_dialog, open_snippet_dialog};

use super::workspace_view::Workspace;

impl Workspace {
    pub(super) fn on_new_snippet(
        &mut self,
        _: &NewSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_snippet_dialog(SnippetDialog::New(None), self.store.clone(), window, cx);
    }

    pub(super) fn on_new_snippet_in(
        &mut self,
        action: &NewSnippetIn,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_snippet_dialog(
            SnippetDialog::New(Some(action.0)),
            self.store.clone(),
            window,
            cx,
        );
    }

    pub(super) fn on_edit_snippet(
        &mut self,
        action: &EditSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).snippet(action.0).is_some() {
            open_snippet_dialog(
                SnippetDialog::Edit(action.0),
                self.store.clone(),
                window,
                cx,
            );
        }
    }

    pub(super) fn on_delete_snippet(
        &mut self,
        action: &DeleteSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let Some(name) = self
            .store
            .read(cx)
            .snippet(id)
            .map(|snippet| snippet.name.clone())
        else {
            return;
        };
        let store = self.store.clone();
        confirm_delete(
            &name,
            None,
            Rc::new(move |_, cx| {
                store.update(cx, |store, cx| {
                    store.remove_snippet(id, cx);
                });
            }),
            window,
            cx,
        );
    }

    pub(super) fn on_new_snippet_category(
        &mut self,
        _: &NewSnippetCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_category_dialog(CategoryDialog::New, self.store.clone(), window, cx);
    }

    pub(super) fn on_rename_snippet_category(
        &mut self,
        action: &RenameSnippetCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).snippet_category(action.0).is_some() {
            open_category_dialog(
                CategoryDialog::Rename(action.0),
                self.store.clone(),
                window,
                cx,
            );
        }
    }

    /// Ask, saying how many snippets go with it, then delete a category.
    pub(super) fn on_delete_snippet_category(
        &mut self,
        action: &DeleteSnippetCategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let store = self.store.read(cx);
        let Some(name) = store
            .snippet_category(id)
            .map(|category| category.name.clone())
        else {
            return;
        };
        let snippets = store.snippets_in(id);
        let description =
            (snippets > 0).then(|| tn!("workspace.snippets.delete_category", snippets));
        let store = self.store.clone();
        confirm_delete(
            &name,
            description,
            Rc::new(move |_, cx| {
                store.update(cx, |store, cx| {
                    store.remove_snippet_category(id, cx);
                });
            }),
            window,
            cx,
        );
    }

    /// A category's heading: fold it away, or unfold it.
    pub(super) fn on_toggle_snippet_category(
        &mut self,
        action: &ToggleSnippetCategory,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.tools
            .update(cx, |tools, cx| tools.toggle_snippet_category(id, cx));
    }
}
