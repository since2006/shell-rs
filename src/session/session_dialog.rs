use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter},
    form::{Field, Form},
    input::{Input, InputState},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{AuthKind, GroupId, Session, SessionDraft, SessionId, SessionStore, group_options};

/// The label of the row that puts a session at the root of the tree.
pub const NO_GROUP_LABEL: &str = "（无分组）";

/// What runs when the user confirms deleting a session.
pub type DeleteHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The body of the new/edit session dialog. Owns the field states and
/// validates on commit; the store is only touched when validation passes.
pub struct SessionForm {
    store: Entity<SessionStore>,
    editing: Option<SessionId>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    user: Entity<InputState>,
    auth: Entity<SelectState<Vec<&'static str>>>,
    group: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the group select's rows; `None` is the root of the tree.
    group_ids: Vec<Option<GroupId>>,
    error: Option<SharedString>,
}

impl SessionForm {
    pub fn new(
        editing: Option<SessionId>,
        preselect_group: Option<GroupId>,
        store: Entity<SessionStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (draft, options) = {
            let read = store.read(cx);
            (
                editing.and_then(|id| read.session(id)).map(Session::draft),
                group_options(read.groups(), &[]),
            )
        };
        // A session with no group sits at the root of the tree, which is where
        // every session starts when the database is still empty.
        let mut group_ids: Vec<Option<GroupId>> = vec![None];
        let mut group_names: Vec<SharedString> = vec![NO_GROUP_LABEL.into()];
        for (id, path) in options {
            group_ids.push(Some(id));
            group_names.push(path);
        }
        let draft = draft.unwrap_or_else(|| {
            SessionDraft::new("", "", 22, "root", AuthKind::Key, preselect_group)
        });

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 web-01")
                .default_value(draft.name.clone())
        });
        let host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("主机名或 IP 地址")
                .default_value(draft.host.clone())
        });
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("22")
                .default_value(draft.port.to_string())
        });
        let user = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("root")
                .default_value(draft.user.clone())
        });
        let auth_ix = AuthKind::ALL
            .iter()
            .position(|kind| *kind == draft.auth)
            .unwrap_or(0);
        let auth = cx.new(|cx| {
            SelectState::new(
                AuthKind::ALL
                    .iter()
                    .map(|kind| kind.label())
                    .collect::<Vec<_>>(),
                Some(IndexPath::new(auth_ix)),
                window,
                cx,
            )
        });
        let group_ix = group_ids
            .iter()
            .position(|group| *group == draft.group)
            .unwrap_or(0);
        let group =
            cx.new(|cx| SelectState::new(group_names, Some(IndexPath::new(group_ix)), window, cx));

        Self {
            store,
            editing,
            name,
            host,
            port,
            user,
            auth,
            group,
            group_ids,
            error: None,
        }
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, _: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        let host = self.host.read(cx).value().trim().to_string();
        let user = self.user.read(cx).value().trim().to_string();
        let port = self.port.read(cx).value().trim().parse::<u16>();

        let error = if name.is_empty() {
            Some("请输入名称")
        } else if host.is_empty() {
            Some("请输入主机")
        } else if !matches!(port, Ok(1..=u16::MAX)) {
            Some("端口必须是 1 到 65535 之间的数字")
        } else {
            None
        };
        if let Some(error) = error {
            self.error = Some(error.into());
            cx.notify();
            return false;
        }

        let auth = self
            .auth
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| AuthKind::ALL.get(ix.row).copied())
            .unwrap_or_default();
        let group = self
            .group
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| self.group_ids.get(ix.row).copied())
            .unwrap_or(None);
        let draft = SessionDraft::new(
            name,
            host,
            port.unwrap_or(22),
            if user.is_empty() {
                "root".to_string()
            } else {
                user
            },
            auth,
            group,
        );

        let editing = self.editing;
        self.store.update(cx, |store, cx| match editing {
            Some(id) => {
                store.update(id, draft, cx);
            }
            None => {
                store.insert(draft, cx);
            }
        });
        self.error = None;
        true
    }
}

impl Render for SessionForm {
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
                            .child(Input::new(&self.name).id("session-name").small()),
                    )
                    .child(
                        Field::new()
                            .label("主机")
                            .required(true)
                            .child(Input::new(&self.host).id("session-host").small()),
                    )
                    .child(
                        Field::new()
                            .label("端口")
                            .child(Input::new(&self.port).id("session-port").small()),
                    )
                    .child(
                        Field::new()
                            .label("用户名")
                            .child(Input::new(&self.user).id("session-user").small()),
                    )
                    .child(
                        Field::new()
                            .label("认证方式")
                            .child(Select::new(&self.auth).small()),
                    )
                    .child(
                        Field::new()
                            .label("分组")
                            .child(Select::new(&self.group).small()),
                    ),
            )
            .when_some(self.error.clone(), |form, error| {
                form.child(
                    div()
                        .id("form-error")
                        .test_support()
                        .aria_label(error.clone())
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}

/// Open the new-session (`editing == None`) or edit-session dialog.
/// `preselect_group` fills in the group field of a new session, so creating
/// one from a group's context menu lands it in that group.
pub fn open_session_dialog(
    editing: Option<SessionId>,
    preselect_group: Option<GroupId>,
    store: Entity<SessionStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| SessionForm::new(editing, preselect_group, store, window, cx));
    let title: SharedString = if editing.is_some() {
        "编辑会话"
    } else {
        "新建会话"
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
                .child(form.clone())
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.label("取消")))
                        .child(
                            DialogAction::new()
                                .child(Button::new("commit").primary().label(commit_label.clone())),
                        ),
                )
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
        }
    });
}

/// Ask before deleting a session. `on_delete` runs when the user confirms.
pub fn confirm_delete_session(
    session: &Session,
    closes_tabs: bool,
    on_delete: DeleteHandler,
    window: &mut Window,
    cx: &mut App,
) {
    let title: SharedString = format!("删除“{}”？", session.name).into();
    window.open_alert_dialog(cx, move |alert, _, _| {
        alert
            .title(title.clone())
            .when(closes_tabs, |alert| {
                alert.description("会一并关闭该会话已打开的终端和 SFTP 标签。")
            })
            .button_props(
                DialogButtonProps::default()
                    .ok_text("删除")
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text("取消"),
            )
            .show_cancel(true)
            .on_ok({
                let on_delete = on_delete.clone();
                move |_, window, cx| {
                    on_delete(window, cx);
                    true
                }
            })
    });
}
