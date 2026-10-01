use gpui_kit::component::{
    IndexPath, Sizable as _, WindowExt as _,
    form::{Field, Form},
    input::{Input, InputState},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{DeleteHandler, Dependents, GroupDraft, GroupId, SessionStore, group_options};
use crate::shared::{commit_footer, confirm_delete, form_error};

/// The label of the row that stands for "no parent" / "no group".
pub const ROOT_LABEL: &str = "（顶层）";

/// The body of the new/rename group dialog. Owns the field states and
/// validates on commit; the store is only touched when validation passes.
pub struct GroupForm {
    store: Entity<SessionStore>,
    editing: Option<GroupId>,
    name: Entity<InputState>,
    parent: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the parent select's rows; `None` is the top level.
    parent_ids: Vec<Option<GroupId>>,
    error: Option<SharedString>,
}

impl GroupForm {
    pub fn new(
        editing: Option<GroupId>,
        default_parent: Option<GroupId>,
        store: Entity<SessionStore>,
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
        let mut labels: Vec<SharedString> = vec![ROOT_LABEL.into()];
        for (id, path) in options {
            parent_ids.push(Some(id));
            labels.push(path);
        }

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 生产")
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
            error: None,
        }
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, _: &mut Window, cx: &mut Context<Self>) -> bool {
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
            Some("请输入分组名称")
        } else if duplicate {
            Some("同一层级下已有同名分组")
        } else {
            None
        };
        if let Some(error) = error {
            self.error = Some(error.into());
            cx.notify();
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
            self.error = Some("无法把分组移动到它自己的下级".into());
            cx.notify();
            return false;
        }
        self.error = None;
        true
    }
}

impl Render for GroupForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .w_full()
            .child(
                Form::new()
                    .child(
                        Field::new()
                            .label("名称")
                            .required(true)
                            .child(Input::new(&self.name).id("group-name").small()),
                    )
                    .child(
                        Field::new()
                            .label("上级分组")
                            .child(Select::new(&self.parent).small()),
                    ),
            )
            .when_some(self.error.clone(), |form, error| {
                form.child(form_error(error.clone(), cx).aria_label(error))
            })
    }
}

/// Open the new-group (`editing == None`) or rename-group dialog.
pub fn open_group_dialog(
    editing: Option<GroupId>,
    default_parent: Option<GroupId>,
    store: Entity<SessionStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| GroupForm::new(editing, default_parent, store, window, cx));
    let title: SharedString = if editing.is_some() {
        "重命名分组"
    } else {
        "新建分组"
    }
    .into();
    let commit_label: SharedString = if editing.is_some() {
        "保存"
    } else {
        "创建"
    }
    .into();

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
        }
    });
}

/// Ask before deleting a group. Deleting one takes its subgroups and every
/// session inside them, and those sessions' port forwards, so the counts go
/// in the description, with the hosts elsewhere that jump through them.
#[allow(clippy::too_many_arguments)]
pub fn confirm_delete_group(
    name: &str,
    sessions: usize,
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
    let mut description = describe_contents(sessions, subgroups, closes_tabs);
    if uploads > 0 {
        description = Some(
            format!(
                "{}将停止 {uploads} 个传输批次并保留续传进度。",
                description.unwrap_or_default()
            )
            .into(),
        );
    }
    if forwards > 0 {
        description = Some(
            format!(
                "{}这些主机的 {forwards} 条端口转发会一并删除。",
                description.unwrap_or_default()
            )
            .into(),
        );
    }
    if jump_users > 0 {
        description = Some(
            format!(
                "{}分组外有 {jump_users} 台主机经由这些主机跳转，删除后要重新选择跳板主机才能连接。",
                description.unwrap_or_default()
            )
            .into(),
        );
    }
    confirm_delete(name, description, on_delete, window, cx);
}

/// What the delete dialog says about everything that goes with the group.
/// `None` for an empty group with nothing open.
fn describe_contents(sessions: usize, subgroups: usize, closes_tabs: bool) -> Option<SharedString> {
    let contents = match (sessions, subgroups) {
        (0, 0) => None,
        (0, subgroups) => Some(format!("将同时删除其中的 {subgroups} 个子分组。")),
        (sessions, 0) => Some(format!("将同时删除其中的 {sessions} 台主机。")),
        (sessions, subgroups) => Some(format!(
            "将同时删除其中的 {sessions} 台主机和 {subgroups} 个子分组。"
        )),
    };
    let tabs = closes_tabs.then_some("已打开的终端和 SFTP 标签会一并关闭。");
    match (contents, tabs) {
        (None, None) => None,
        (Some(contents), None) => Some(contents.into()),
        (None, Some(tabs)) => Some(tabs.into()),
        (Some(contents), Some(tabs)) => Some(format!("{contents}{tabs}").into()),
    }
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
}
