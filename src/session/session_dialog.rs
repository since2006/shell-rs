use std::rc::Rc;

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::{DialogAction, DialogButtonProps, DialogClose, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeroize::Zeroizing;

use crate::secrets::{SecretRef, SharedSecretStore};

use super::{AuthKind, GroupId, Session, SessionDraft, SessionId, SessionStore, group_options};

/// The label of the row that puts a session at the root of the tree.
pub const NO_GROUP_LABEL: &str = "（无分组）";

/// What runs when the user confirms deleting a session.
pub type DeleteHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// What the keychain had for the session being edited.
#[derive(Default)]
struct SavedSecrets {
    password: Option<Zeroizing<String>>,
    passphrase: Option<Zeroizing<String>>,
    /// Whether a key file was configured at all. Without one nothing was
    /// looked up, so an empty passphrase field means nothing either.
    looked_for_passphrase: bool,
}

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
    password: Entity<InputState>,
    passphrase: Entity<InputState>,
    group: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the group select's rows; `None` is the root of the tree.
    group_ids: Vec<Option<GroupId>>,
    error: Option<SharedString>,
    editing_connected: bool,
    /// Where saved secrets are read from. Writes go through the store, which
    /// owns the one error channel.
    secrets: SharedSecretStore,
    /// Whether the saved secret has been read back yet. Until it has, an empty
    /// field means "still loading", not "the user cleared it", so committing
    /// early never deletes anything.
    password_loaded: bool,
    passphrase_loaded: bool,
    _auth_subscription: Subscription,
    _key_path_subscription: Subscription,
}

impl SessionForm {
    pub fn new(
        editing: Option<SessionId>,
        preselect_group: Option<GroupId>,
        store: Entity<SessionStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let secrets = store.read(cx).secrets();
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
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("留空则每次连接都询问")
        });
        let passphrase = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("留空则每次连接都询问")
        });
        // A passphrase belongs to a key file, so picking another key makes
        // whatever is in the field meaningless. Clearing it is also the
        // visible cue that the new key needs its own passphrase.
        let key_path_subscription = cx.subscribe_in(
            &key_path,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.forget_loaded_passphrase(window, cx);
                }
            },
        );
        let group_ix = group_ids
            .iter()
            .position(|group| *group == draft.group)
            .unwrap_or(0);
        let group =
            cx.new(|cx| SelectState::new(group_names, Some(IndexPath::new(group_ix)), window, cx));

        let this = Self {
            store,
            editing,
            name,
            host,
            port,
            user,
            auth,
            key_path,
            password,
            passphrase,
            group,
            group_ids,
            error: None,
            editing_connected,
            secrets,
            password_loaded: false,
            passphrase_loaded: false,
            _auth_subscription: auth_subscription,
            _key_path_subscription: key_path_subscription,
        };
        if editing.is_some() {
            this.load_saved_secrets(&draft, cx);
        }
        this
    }

    /// Read what the keychain holds for the session being edited, off the UI
    /// thread: the call blocks and on macOS may raise a system authorization
    /// dialog. The form stays usable while it runs.
    fn load_saved_secrets(&self, draft: &SessionDraft, cx: &mut Context<Self>) {
        let secrets = self.secrets.clone();
        let endpoint = draft.password_secret();
        let key_path = draft.key_path.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    SavedSecrets {
                        password: secrets.get(&endpoint).ok().flatten(),
                        passphrase: key_path
                            .as_ref()
                            .map(|path| SecretRef::passphrase(path.as_ref()))
                            .and_then(|secret| secrets.get(&secret).ok().flatten()),
                        looked_for_passphrase: key_path.is_some(),
                    }
                })
                .await;
            this.update_in(cx, |this, window, cx| this.fill_saved(loaded, window, cx))
                .ok();
        })
        .detach();
    }

    /// Put the saved secrets into their fields. A field the user has already
    /// typed into wins: the read is asynchronous and may land late.
    fn fill_saved(&mut self, loaded: SavedSecrets, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(password) = loaded.password
            && self.password.read(cx).value().is_empty()
        {
            self.password
                .update(cx, |input, cx| input.set_value(&*password, window, cx));
        }
        self.password_loaded = true;
        if loaded.looked_for_passphrase {
            if let Some(passphrase) = loaded.passphrase
                && self.passphrase.read(cx).value().is_empty()
            {
                self.passphrase
                    .update(cx, |input, cx| input.set_value(&*passphrase, window, cx));
            }
            self.passphrase_loaded = true;
        }
        cx.notify();
    }

    /// Drop a passphrase that belonged to a key file the form no longer points
    /// at, so it cannot be written under the new key's name.
    fn forget_loaded_passphrase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.passphrase_loaded {
            return;
        }
        self.passphrase_loaded = false;
        self.passphrase
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
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
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
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

        // Secrets never ride along in the draft, which derives `Debug`. They
        // go to the keychain separately, under the endpoint the draft names.
        let password_secret = draft.password_secret();
        let passphrase_secret = draft
            .key_path
            .as_ref()
            .map(|path| SecretRef::passphrase(path.as_ref()));
        let password_change = if uses_password(auth) {
            secret_change(
                self.password.read(cx).value().to_string(),
                self.password_loaded,
            )
        } else {
            None
        };
        let passphrase_change = if auth == AuthKind::Key {
            secret_change(
                self.passphrase.read(cx).value().to_string(),
                self.passphrase_loaded,
            )
        } else {
            None
        };

        let editing = self.editing;
        self.store.update(cx, |store, cx| {
            match editing {
                Some(id) => {
                    store.update(id, draft, cx);
                }
                None => {
                    store.insert(draft, cx);
                }
            }
            // `update` above may have dropped the entry for the endpoint the
            // session just left; this writes the one it moved to.
            if let Some(change) = password_change {
                store.save_secret(password_secret, change, cx);
            }
            if let (Some(secret), Some(change)) = (passphrase_secret, passphrase_change) {
                store.save_secret(secret, change, cx);
            }
        });
        // The dialog is about to close and take the form with it; drop the
        // plaintext now rather than waiting for the entity.
        self.password
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.passphrase
            .update(cx, |input, cx| input.set_value("", window, cx));
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
        let keychain = self.secrets.is_available();
        let secret_note = if keychain {
            "密码保存在系统钥匙串，不会写入 shellr 的数据库。"
        } else {
            "系统钥匙串不可用，这台机器上无法保存密码，每次连接都会询问。"
        };
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
                    .when(uses_password(auth), |form| {
                        form.child(
                            Field::new().label("密码").child(
                                Input::new(&self.password)
                                    .id("session-password")
                                    .small()
                                    .mask_toggle()
                                    .content_type(InputContentType::Password)
                                    .disabled(!keychain),
                            ),
                        )
                    })
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
                        .child(
                            Field::new().label("私钥口令").child(
                                Input::new(&self.passphrase)
                                    .id("session-passphrase")
                                    .small()
                                    .mask_toggle()
                                    .content_type(InputContentType::Password)
                                    .disabled(!keychain),
                            ),
                        )
                    })
                    .child(
                        Field::new()
                            .label("分组")
                            .child(Select::new(&self.group).small()),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(secret_note),
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
/// Whether this authentication kind can end up asking for a password.
/// `Auto` walks agent, then keys, then password, so it can.
fn uses_password(auth: AuthKind) -> bool {
    matches!(auth, AuthKind::Auto | AuthKind::Password)
}

/// What to do with a secret field on commit: `None` leaves the keychain
/// alone, `Some(None)` deletes the entry, `Some(Some(value))` writes it.
///
/// An empty field only means "delete" once the saved value has been read
/// back. Before that it just means the read has not landed, so committing
/// straight away can never wipe a saved password.
fn secret_change(value: String, loaded: bool) -> Option<Option<String>> {
    if !value.is_empty() {
        Some(Some(value))
    } else if loaded {
        Some(None)
    } else {
        None
    }
}

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

#[cfg(test)]
mod tests {
    use super::{AuthKind, secret_change, uses_password};

    #[test]
    fn auto_and_password_can_reach_a_password_prompt() {
        assert!(uses_password(AuthKind::Auto));
        assert!(uses_password(AuthKind::Password));
        assert!(!uses_password(AuthKind::Key));
    }

    #[test]
    fn a_filled_field_is_written() {
        assert_eq!(
            secret_change("hunter2".into(), false),
            Some(Some("hunter2".into()))
        );
        assert_eq!(
            secret_change("hunter2".into(), true),
            Some(Some("hunter2".into()))
        );
    }

    #[test]
    fn an_emptied_field_deletes_only_once_the_saved_value_was_read() {
        assert_eq!(secret_change(String::new(), true), Some(None));
        assert_eq!(secret_change(String::new(), false), None);
    }

    #[test]
    fn surrounding_whitespace_is_part_of_the_secret() {
        assert_eq!(
            secret_change("  spaced  ".into(), true),
            Some(Some("  spaced  ".into()))
        );
    }
}
