//! The workspace's share of 命令片段: the commands of the snippet list,
//! which open the dialogs, ask before deleting and move rows.

use std::rc::Rc;

use gpui_kit::*;

use crate::app::{
    DeleteSnippet, DeleteSnippetCategory, EditSnippet, MoveSnippetNode, NewSnippet,
    NewSnippetCategory, NewSnippetIn, NewSnippetSubcategory, RenameSnippetCategory,
    ToggleSnippetCategory,
};
use crate::i18n::{join_list, t, tn};
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
        open_category_dialog(CategoryDialog::New(None), self.store.clone(), window, cx);
    }

    pub(super) fn on_new_snippet_subcategory(
        &mut self,
        action: &NewSnippetSubcategory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).snippet_category(action.0).is_some() {
            open_category_dialog(
                CategoryDialog::New(Some(action.0)),
                self.store.clone(),
                window,
                cx,
            );
        }
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

    /// Ask, saying how many categories and snippets go with it, then
    /// delete a category.
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
        let description = delete_category_description(
            store.snippets_in(id),
            store.descendant_snippet_categories(id).len(),
        );
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

    /// A row of 命令片段 dropped where it goes, shown there even in a
    /// folded category.
    pub(super) fn on_move_snippet_node(
        &mut self,
        action: &MoveSnippetNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let moved = self.store.update(cx, |store, cx| {
            store.move_snippet_node(action.source, action.destination, cx)
        });
        if moved {
            let node = action.source;
            self.tools
                .update(cx, |tools, cx| tools.reveal_snippet_node(node, cx));
        }
    }
}

/// What the delete dialog says goes with a category: the snippets in it and
/// the categories inside it, at any depth. `None` for an empty one.
fn delete_category_description(snippets: usize, subcategories: usize) -> Option<SharedString> {
    let mut items = Vec::new();
    if snippets > 0 {
        items.push(tn!("workspace.snippets.snippets", snippets));
    }
    if subcategories > 0 {
        items.push(tn!("workspace.snippets.subcategories", subcategories));
    }
    (!items.is_empty()).then(|| {
        t!(
            "workspace.snippets.delete_category",
            items = join_list(&items)
        )
    })
}

#[cfg(test)]
mod tests {
    // Imported one by one: `use gpui_kit::*` above would shadow `#[test]`.
    use super::delete_category_description;

    #[test]
    fn the_delete_dialog_says_what_goes_with_a_category() {
        assert_eq!(delete_category_description(0, 0), None);
        assert_eq!(
            delete_category_description(3, 0).as_deref(),
            Some("将同时删除其中的 3 个片段。")
        );
        assert_eq!(
            delete_category_description(2, 1).as_deref(),
            Some("将同时删除其中的 2 个片段和 1 个子分类。")
        );
    }

    #[test]
    fn the_delete_dialog_counts_in_english_too() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        assert_eq!(
            delete_category_description(1, 2).as_deref(),
            Some("This also deletes the category’s 1 snippet and 2 subcategories.")
        );
    }
}
