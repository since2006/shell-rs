use gpui_kit::component::{
    IndexPath, Sizable as _, WindowExt as _,
    form::{Field, Form},
    input::{Input, InputState},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::*;

use super::host_dialog::join_sentences;
use super::{DeleteHandler, Dependents, GroupDraft, GroupId, HostStore, group_options};
use crate::i18n::{join_list, t, tn};
use crate::shared::{commit_footer, confirm_delete, dismiss_form_error, form_error_notification};

/// The body of the new/rename group dialog. Owns the field states and
/// validates on commit; the store is only touched when validation passes.
pub struct GroupForm {
    store: Entity<HostStore>,
    editing: Option<GroupId>,
    name: Entity<InputState>,
    parent: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the parent select's rows; `None` is the top level.
    parent_ids: Vec<Option<GroupId>>,
}

impl GroupForm {
    pub fn new(
        editing: Option<GroupId>,
        default_parent: Option<GroupId>,
        store: Entity<HostStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (draft, options) = {
            let read = store.read(cx);
            let draft = editing
                .and_then(|id| read.group(id))
                .map(|group| group.draft())
                .unwrap_or_else(|| GroupDraft::new("", default_parent));
            // A group cannot become its own ancestor, so its subtree is not
            // offered as a destination.
            let mut excluded = editing
                .map(|id| read.descendant_groups(id))
                .unwrap_or_default();
            excluded.extend(editing);
            (draft, group_options(read.groups(), &excluded))
        };
        let mut parent_ids: Vec<Option<GroupId>> = vec![None];
        let mut labels: Vec<SharedString> = vec![t!("host.group_dialog.top_level")];
        for (id, path) in options {
            parent_ids.push(Some(id));
            labels.push(path);
        }

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.group_dialog.name_placeholder"))
                .default_value(draft.name.clone())
        });
        let parent_ix = parent_ids
            .iter()
            .position(|id| *id == draft.parent)
            .unwrap_or(0);
        let parent =
            cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(parent_ix)), window, cx));

        Self {
            store,
            editing,
            name,
            parent,
            parent_ids,
        }
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        let parent = self
            .parent
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| self.parent_ids.get(ix.row).copied())
            .unwrap_or(None);

        let editing = self.editing;
        let duplicate = self.store.read(cx).groups().iter().any(|group| {
            group.parent == parent && group.name == name.as_str() && Some(group.id) != editing
        });
        let error = if name.is_empty() {
            Some(t!("host.group_dialog.enter_name"))
        } else if duplicate {
            Some(t!("host.group_dialog.duplicate"))
        } else {
            None
        };
        if let Some(error) = error {
            window.push_notification(form_error_notification(error), cx);
            return false;
        }

        let draft = GroupDraft::new(name, parent);
        let committed = self.store.update(cx, |store, cx| match editing {
            Some(id) => store.update_group(id, draft, cx),
            None => {
                store.insert_group(draft, cx);
                true
            }
        });
        if !committed {
            window.push_notification(
                form_error_notification(t!("host.group_dialog.into_itself")),
                cx,
            );
            return false;
        }
        true
    }
}

impl Render for GroupForm {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        v_flex().gap_3().w_full().child(
            Form::new()
                .child(
                    Field::new()
                        .label(t!("host.group_dialog.name"))
                        .required(true)
                        .child(Input::new(&self.name).id("group-name").small()),
                )
                .child(
                    Field::new()
                        .label(t!("host.group_dialog.parent"))
                        .child(Select::new(&self.parent).small()),
                ),
        )
    }
}

/// Open the new-group (`editing == None`) or rename-group dialog.
pub fn open_group_dialog(
    editing: Option<GroupId>,
    default_parent: Option<GroupId>,
    store: Entity<HostStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| GroupForm::new(editing, default_parent, store, window, cx));
    let title = if editing.is_some() {
        t!("host.group_dialog.title_rename")
    } else {
        t!("host.group_dialog.title_new")
    };
    let commit_label = if editing.is_some() {
        t!("common.save")
    } else {
        t!("host.group_dialog.create")
    };

    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, _, _| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(commit_footer("commit", commit_label.clone()))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
}

/// Ask before deleting a group. Deleting one takes its subgroups and every
/// host inside them, and those hosts' port forwards, so the counts go
/// in the description, with the hosts elsewhere that jump through them.
#[allow(clippy::too_many_arguments)]
pub fn confirm_delete_group(
    name: &str,
    hosts: usize,
    subgroups: usize,
    affected: (bool, usize),
    dependents: Dependents,
    on_delete: DeleteHandler,
    window: &mut Window,
    cx: &mut App,
) {
    let Dependents {
        forwards,
        jump_users,
    } = dependents;
    let (closes_tabs, uploads) = affected;
    let mut description = contents_sentences(hosts, subgroups, closes_tabs);
    if uploads > 0 {
        description.push(tn!("host.delete.transfers", uploads));
    }
    if forwards > 0 {
        description.push(tn!("host.delete_group.forwards", forwards));
    }
    if jump_users > 0 {
        description.push(tn!("host.delete_group.jump_users", jump_users));
    }
    confirm_delete(name, join_sentences(description), on_delete, window, cx);
}

/// What the delete dialog says about everything that goes with the group.
/// `None` for an empty group with nothing open.
#[cfg(test)]
fn describe_contents(hosts: usize, subgroups: usize, closes_tabs: bool) -> Option<SharedString> {
    join_sentences(contents_sentences(hosts, subgroups, closes_tabs))
}

/// What the delete dialog says about everything that goes with the group,
/// a sentence each.
fn contents_sentences(hosts: usize, subgroups: usize, closes_tabs: bool) -> Vec<SharedString> {
    let mut contents = Vec::new();
    if hosts > 0 {
        contents.push(tn!("host.count.hosts", hosts));
    }
    if subgroups > 0 {
        contents.push(tn!("host.count.subgroups", subgroups));
    }
    let mut sentences = Vec::new();
    if !contents.is_empty() {
        sentences.push(t!(
            "host.delete_group.contents",
            items = join_list(&contents)
        ));
    }
    if closes_tabs {
        sentences.push(t!("host.delete_group.closes_tabs"));
    }
    sentences
}

#[cfg(test)]
mod tests {
    // Imported one by one: `use gpui_kit::*` at the top of this file would
    // shadow `#[test]` with gpui-kit's own test macro.
    use super::describe_contents;

    #[test]
    fn the_description_only_mentions_what_is_there() {
        assert_eq!(describe_contents(0, 0, false), None);
        assert_eq!(
            describe_contents(3, 0, false).as_deref(),
            Some("将同时删除其中的 3 台主机。")
        );
        assert_eq!(
            describe_contents(0, 2, false).as_deref(),
            Some("将同时删除其中的 2 个子分组。")
        );
        assert_eq!(
            describe_contents(3, 2, true).as_deref(),
            Some("将同时删除其中的 3 台主机和 2 个子分组。已打开的终端和 SFTP 标签会一并关闭。")
        );
        assert_eq!(
            describe_contents(0, 0, true).as_deref(),
            Some("已打开的终端和 SFTP 标签会一并关闭。")
        );
    }

    #[test]
    fn the_description_counts_in_english_too() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        assert_eq!(
            describe_contents(1, 2, true).as_deref(),
            Some(
                "This also deletes the group’s 1 host and 2 subgroups. \
                 Open terminal and SFTP tabs will close."
            )
        );
        assert_eq!(
            describe_contents(3, 0, false).as_deref(),
            Some("This also deletes the group’s 3 hosts.")
        );
    }
}
