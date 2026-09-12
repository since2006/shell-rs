use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter},
    form::{Field, Form},
    h_flex,
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
    key_path: Entity<InputState>,
    group: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the group select's rows; `None` is the root of the tree.
    group_ids: Vec<Option<GroupId>>,
    error: Option<SharedString>,
    editing_connected: bool,
    _auth_subscription: Subscription,
}

impl SessionForm {
    pub fn new(
        editing: Option<SessionId>,
        preselect_group: Option<GroupId>,
        store: Entity<SessionStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (draft, options, editing_connected) = {
            let read = store.read(cx);
            (
                editing.and_then(|id| read.session(id)).map(Session::draft),
                group_options(read.groups(), &[]),
                editing
                    .and_then(|id| read.session(id))
                    .is_some_and(|session| session.state != super::ConnectionState::Disconnected),
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
            SessionDraft::new("", "", 22, "root", AuthKind::Auto, preselect_group)
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
        let key_path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("选择 OpenSSH 私钥文件")
                .default_value(draft.key_path.clone().unwrap_or_default())
        });
        let auth_subscription = cx.observe(&auth, |_, _, cx| cx.notify());
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
            key_path,
            group,
            group_ids,
            error: None,
            editing_connected,
            _auth_subscription: auth_subscription,
        }
    }

    fn choose_key(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 SSH 私钥".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.key_path.update(cx, |input, cx| {
                    input.set_value(path.to_string_lossy().into_owned(), window, cx)
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, _: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        let host = self.host.read(cx).value().trim().to_string();
        let user = self.user.read(cx).value().trim().to_string();
        let port = self.port.read(cx).value().trim().parse::<u16>();
        let auth = self
            .auth
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| AuthKind::ALL.get(ix.row).copied())
            .unwrap_or_default();
        let key_path = self.key_path.read(cx).value().trim().to_string();

        let error = if name.is_empty() {
            Some("请输入名称")
        } else if host.is_empty() {
            Some("请输入主机")
        } else if !matches!(port, Ok(1..=u16::MAX)) {
            Some("端口必须是 1 到 65535 之间的数字")
        } else if auth == AuthKind::Key && key_path.is_empty() {
            Some("私钥认证需要选择私钥文件")
        } else {
            None
        };
        if let Some(error) = error {
            self.error = Some(error.into());
            cx.notify();
            return false;
        }

        let group = self
            .group
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| self.group_ids.get(ix.row).copied())
            .unwrap_or(None);
        let mut draft = SessionDraft::new(
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
        if auth == AuthKind::Key {
            draft = draft.with_key_path(key_path);
        }

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
        let auth = self
            .auth
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| AuthKind::ALL.get(ix.row).copied())
            .unwrap_or_default();
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
                            .child(Select::new(&self.auth).id("session-auth").small()),
                    )
                    .when(auth == AuthKind::Key, |form| {
                        form.child(
                            Field::new().label("私钥文件").required(true).child(
                                h_flex()
                                    .gap_2()
                                    .w_full()
                                    .child(
                                        Input::new(&self.key_path)
                                            .id("session-key-path")
                                            .small()
                                            .flex_1(),
                                    )
                                    .child(
                                        Button::new("choose-key")
                                            .label("选择…")
                                            .small()
                                            .on_click(cx.listener(Self::choose_key)),
                                    ),
                            ),
                        )
                    })
                    .child(
                        Field::new()
                            .label("分组")
                            .child(Select::new(&self.group).small()),
                    ),
            )
            .when(self.editing_connected, |form| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("保存后连接设置将立即生效并重连。"),
                )
            })
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
