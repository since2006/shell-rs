//! The dialogs of 命令片段: a snippet's, and a category's.

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    checkbox::Checkbox,
    form::{Field, Form},
    input::{Input, InputState, Textarea, TextareaState},
    select::{Select, SelectState},
};
use gpui_kit::*;

use crate::host::{HostStore, SnippetCategoryId, SnippetDraft, SnippetId, SnippetScope, by_name};
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
    /// Parallel to the category select's rows; `None` is 未分类.
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
        let (editing, draft, mut categories) = {
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
            let categories: Vec<(SnippetCategoryId, SharedString)> = read
                .snippet_categories()
                .iter()
                .map(|category| (category.id, category.name.clone()))
                .collect();
            (editing, draft, categories)
        };
        categories.sort_by(|a, b| by_name((&a.1, a.0.0), (&b.1, b.0.0)));
        let mut category_ids: Vec<Option<SnippetCategoryId>> = vec![None];
        // The category list's row for a snippet of none.
        let mut labels: Vec<SharedString> = vec![t!("snippets.uncategorized")];
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
                window.push_notification(form_error_notification(error), cx);
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
    New,
    Rename(SnippetCategoryId),
}

/// The body of the new and rename category dialog.
pub struct CategoryForm {
    store: Entity<HostStore>,
    editing: Option<SnippetCategoryId>,
    name: Entity<InputState>,
}

impl CategoryForm {
    fn new(
        dialog: CategoryDialog,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editing, current) = match dialog {
            CategoryDialog::New => (None, SharedString::default()),
            CategoryDialog::Rename(id) => match store.read(cx).snippet_category(id) {
                Some(category) => (Some(id), category.name.clone()),
                None => (None, SharedString::default()),
            },
        };
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("snippets.category.name_placeholder"))
                .default_value(current)
        });
        Self {
            store,
            editing,
            name,
        }
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        let editing = self.editing;
        let taken = self
            .store
            .read(cx)
            .snippet_categories()
            .iter()
            .any(|category| category.name == name.as_str() && Some(category.id) != editing);
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
        self.store.update(cx, |store, cx| match editing {
            Some(id) => {
                store.rename_snippet_category(id, name, cx);
            }
            None => {
                store.insert_snippet_category(name, cx);
            }
        });
        true
    }
}

impl Render for CategoryForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Form::new().child(
            Field::new()
                .label(t!("snippets.category.name"))
                .required(true)
                .child(Input::new(&self.name).id("snippet-category-name").small()),
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
