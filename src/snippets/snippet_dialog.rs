//! The dialogs of 命令片段: a snippet's, and a category's.

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    checkbox::Checkbox,
    form::{Field, Form},
    input::{Input, InputState, Textarea, TextareaState},
    select::{Select, SelectState},
};
use gpui_kit::*;

use super::model::category_options;
use crate::host::{
    ErrorMessage as _, HostStore, SnippetCategoryDraft, SnippetCategoryId, SnippetDraft, SnippetId,
    SnippetScope,
};
use crate::i18n::t;
use crate::shared::{commit_footer, dismiss_form_error, form_error_notification};

/// How wide the snippet dialog is: room for a command of a fair length on
/// one line.
const DIALOG_WIDTH: f32 = 36.;

/// Which snippet dialog to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetDialog {
    /// A new snippet, in this category or none.
    New(Option<SnippetCategoryId>),
    Edit(SnippetId),
}

/// The body of the new and edit snippet dialog. Validates on commit; the
/// store is only touched when that passes.
pub struct SnippetForm {
    store: Entity<HostStore>,
    editing: Option<SnippetId>,
    /// Kept as it is: nothing sets it yet.
    scope: SnippetScope,
    name: Entity<InputState>,
    category: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the category select's rows; `None` is none, at the top.
    category_ids: Vec<Option<SnippetCategoryId>>,
    command: Entity<TextareaState>,
    run_on_click: bool,
}

impl SnippetForm {
    fn new(
        dialog: SnippetDialog,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editing, draft, categories) = {
            let read = store.read(cx);
            let (editing, draft) = match dialog {
                SnippetDialog::Edit(id) => match read.snippet(id) {
                    Some(snippet) => (Some(id), snippet.draft()),
                    None => (None, SnippetDraft::new("", "", None)),
                },
                // A new snippet runs on a click unless told otherwise.
                SnippetDialog::New(category) => (
                    None,
                    SnippetDraft::new("", "", category).with_run_on_click(true),
                ),
            };
            let categories = category_options(read.snippet_categories(), None);
            (editing, draft, categories)
        };
        let mut category_ids: Vec<Option<SnippetCategoryId>> = vec![None];
        // The category list's row for a snippet of none.
        let mut labels: Vec<SharedString> = vec![t!("snippets.dialog.no_category")];
        for (id, name) in categories {
            category_ids.push(Some(id));
            labels.push(name);
        }
        let selected = category_ids
            .iter()
            .position(|id| *id == draft.category)
            .unwrap_or(0);

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("snippets.dialog.name_placeholder"))
                .default_value(draft.name.clone())
        });
        let category =
            cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(selected)), window, cx));
        let command = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 12)
                .placeholder(t!("snippets.dialog.command_placeholder"))
                .default_value(draft.command.clone())
        });
        Self {
            store,
            editing,
            scope: draft.scope,
            name,
            category,
            category_ids,
            command,
            run_on_click: draft.run_on_click,
        }
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let category = self
            .category
            .read(cx)
            .selected_index(cx)
            .and_then(|index| self.category_ids.get(index.row).copied())
            .unwrap_or(None);
        let draft = match SnippetDraft::validated(
            &self.name.read(cx).value(),
            &self.command.read(cx).value(),
            category,
        ) {
            Ok(draft) => SnippetDraft {
                scope: self.scope,
                ..draft.with_run_on_click(self.run_on_click)
            },
            Err(error) => {
                window.push_notification(form_error_notification(error.message()), cx);
                return false;
            }
        };
        let editing = self.editing;
        self.store.update(cx, |store, cx| match editing {
            Some(id) => {
                store.update_snippet(id, draft, cx);
            }
            None => {
                store.insert_snippet(draft, cx);
            }
        });
        true
    }
}

impl Render for SnippetForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Form::new()
            .child(
                Field::new()
                    .label(t!("snippets.dialog.name"))
                    .required(true)
                    .child(Input::new(&self.name).id("snippet-name").small()),
            )
            .child(
                Field::new().label(t!("snippets.dialog.category")).child(
                    div()
                        .id("snippet-category")
                        .test_support()
                        .w_full()
                        .child(Select::new(&self.category).small()),
                ),
            )
            .child(
                Field::new()
                    .label(t!("snippets.dialog.command"))
                    .required(true)
                    .description(t!("snippets.dialog.command_description"))
                    .child(
                        div().id("snippet-command").test_support().w_full().child(
                            Textarea::new(&self.command)
                                .text_sm()
                                .font_family(cx.theme().mono_font_family.clone()),
                        ),
                    ),
            )
            .child(
                Field::new().child(
                    Checkbox::new("snippet-run-on-click")
                        .label(t!("snippets.dialog.run_on_click"))
                        .checked(self.run_on_click)
                        .small()
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.run_on_click = *checked;
                            cx.notify();
                        }))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(t!("snippets.dialog.run_on_click_description")),
                        ),
                ),
            )
    }
}

/// Open the new or the edit snippet dialog.
pub fn open_snippet_dialog(
    dialog: SnippetDialog,
    store: Entity<HostStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| SnippetForm::new(dialog, store, window, cx));
    let editing = form.read(cx).editing.is_some();
    let (title, commit): (SharedString, SharedString) = if editing {
        (t!("snippets.dialog.edit_title"), t!("common.save"))
    } else {
        (
            t!("snippets.dialog.new_title"),
            t!("snippets.dialog.create"),
        )
    };
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, window, _| {
            dialog
                .title(title.clone())
                // Dialog geometry is an API boundary that takes `Pixels`; the
                // width follows the interface zoom through the rem.
                .w(rems(DIALOG_WIDTH).to_pixels(window.rem_size()))
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    // Straight to typing: in the same update as the opening, or the
    // dialog's own focus wins.
    let name = form.read(cx).name.clone();
    name.update(cx, |name, cx| name.focus(window, cx));
}

/// Which category dialog to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryDialog {
    /// A new category, in this one or at the top.
    New(Option<SnippetCategoryId>),
    /// Rename a category, or move it into another, as a group's dialog
    /// does.
    Rename(SnippetCategoryId),
}

/// The body of the new and rename category dialog.
pub struct CategoryForm {
    store: Entity<HostStore>,
    editing: Option<SnippetCategoryId>,
    name: Entity<InputState>,
    parent: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the parent select's rows; `None` is the top level.
    parent_ids: Vec<Option<SnippetCategoryId>>,
}

impl CategoryForm {
    fn new(
        dialog: CategoryDialog,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editing, draft, options) = {
            let read = store.read(cx);
            let (editing, draft) = match dialog {
                CategoryDialog::New(parent) => (None, SnippetCategoryDraft::new("", parent)),
                CategoryDialog::Rename(id) => match read.snippet_category(id) {
                    Some(category) => (Some(id), category.draft()),
                    None => (None, SnippetCategoryDraft::new("", None)),
                },
            };
            // Not into itself, nor into what is inside it.
            let options = category_options(read.snippet_categories(), editing);
            (editing, draft, options)
        };
        let mut parent_ids: Vec<Option<SnippetCategoryId>> = vec![None];
        let mut labels: Vec<SharedString> = vec![t!("snippets.category.top_level")];
        for (id, path) in options {
            parent_ids.push(Some(id));
            labels.push(path);
        }
        let selected = parent_ids
            .iter()
            .position(|id| *id == draft.parent)
            .unwrap_or(0);
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("snippets.category.name_placeholder"))
                .default_value(draft.name.clone())
        });
        let parent =
            cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(selected)), window, cx));
        Self {
            store,
            editing,
            name,
            parent,
            parent_ids,
        }
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        let parent = self
            .parent
            .read(cx)
            .selected_index(cx)
            .and_then(|index| self.parent_ids.get(index.row).copied())
            .unwrap_or(None);
        let editing = self.editing;
        // Two of a name side by side could not be told apart.
        let taken = self
            .store
            .read(cx)
            .snippet_categories()
            .iter()
            .any(|category| {
                category.parent == parent
                    && category.name == name.as_str()
                    && Some(category.id) != editing
            });
        let error = if name.is_empty() {
            Some(t!("snippets.category.name_missing"))
        } else if taken {
            Some(t!("snippets.category.name_taken"))
        } else {
            None
        };
        if let Some(error) = error {
            window.push_notification(form_error_notification(error), cx);
            return false;
        }
        let draft = SnippetCategoryDraft::new(name, parent);
        let committed = self.store.update(cx, |store, cx| match editing {
            Some(id) => store.update_snippet_category(id, draft, cx),
            None => {
                store.insert_snippet_category(draft, cx);
                true
            }
        });
        if !committed {
            window.push_notification(
                form_error_notification(t!("snippets.category.into_itself")),
                cx,
            );
        }
        committed
    }
}

impl Render for CategoryForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Form::new()
            .child(
                Field::new()
                    .label(t!("snippets.category.name"))
                    .required(true)
                    .child(Input::new(&self.name).id("snippet-category-name").small()),
            )
            .child(
                Field::new().label(t!("snippets.category.parent")).child(
                    div()
                        .id("snippet-category-parent")
                        .test_support()
                        .w_full()
                        .child(Select::new(&self.parent).small()),
                ),
            )
    }
}

/// Open the new or the rename category dialog.
pub fn open_category_dialog(
    dialog: CategoryDialog,
    store: Entity<HostStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| CategoryForm::new(dialog, store, window, cx));
    let renaming = form.read(cx).editing.is_some();
    let (title, commit): (SharedString, SharedString) = if renaming {
        (t!("snippets.category.rename_title"), t!("common.save"))
    } else {
        (
            t!("snippets.category.new_title"),
            t!("snippets.dialog.create"),
        )
    };
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, _| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
    let name = form.read(cx).name.clone();
    name.update(cx, |name, cx| name.focus(window, cx));
}
