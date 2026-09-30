use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    dialog::{Cancel, Confirm, DialogButtonProps, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputState},
    notification::Notification,
    radio::{Radio, RadioGroup},
    select::{Select, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::connection::{LoginTest, SharedConnectionTester, TrustCallback, UnknownHostPrompt};
use crate::secrets::SecretRef;

use super::secret_fields::SecretFields;
use super::{
    AuthKind, Credential, CredentialId, CredentialKind, DEFAULT_USER, GroupId, Session,
    SessionDraft, SessionId, SessionLogin, SessionStore, group_options,
};
pub use crate::shared::DeleteHandler;
use crate::shared::{confirm_delete, form_error, parse_port};

/// The label of the row that puts a session at the root of the tree.
pub const NO_GROUP_LABEL: &str = "（无分组）";

/// Where a host's login comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthSource {
    /// Typed into this form.
    Manual,
    /// A saved credential.
    Credential,
}

impl AuthSource {
    /// Both, in the order the form lists them.
    const ALL: [AuthSource; 2] = [AuthSource::Manual, AuthSource::Credential];

    fn label(self) -> &'static str {
        match self {
            AuthSource::Manual => "手动输入",
            AuthSource::Credential => "使用凭据",
        }
    }
}

/// The body of the new/edit session dialog. Owns the field states and
/// validates on commit; the store is only touched when validation passes.
pub struct SessionForm {
    store: Entity<SessionStore>,
    editing: Option<SessionId>,
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    source: AuthSource,
    user: Entity<InputState>,
    auth: Entity<SelectState<Vec<&'static str>>>,
    /// The password, key file and passphrase of a login typed here.
    fields: Entity<SecretFields>,
    credential: Entity<SelectState<Vec<SharedString>>>,
    /// The credentials as they were when the form opened, parallel to the
    /// credential select's rows. The dialog is modal, so nothing can change
    /// them while it is open.
    credentials: Vec<Credential>,
    group: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the group select's rows; `None` is the root of the tree.
    group_ids: Vec<Option<GroupId>>,
    error: Option<SharedString>,
    testing_connection: bool,
    /// Logs in with the form's current values for 「测试连接」.
    tester: SharedConnectionTester,
    editing_connected: bool,
    _subscriptions: Vec<Subscription>,
}

impl SessionForm {
    pub fn new(
        editing: Option<SessionId>,
        preselect_group: Option<GroupId>,
        store: Entity<SessionStore>,
        tester: SharedConnectionTester,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let secrets = store.read(cx).secrets();
        let (draft, options, credentials, editing_connected) = {
            let read = store.read(cx);
            (
                editing.and_then(|id| read.session(id)).map(Session::draft),
                group_options(read.groups(), &[]),
                read.credentials().to_vec(),
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
            SessionDraft::new("", "", 22, DEFAULT_USER, AuthKind::Auto, preselect_group)
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
                .placeholder(DEFAULT_USER)
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
        let fields = cx.new(|cx| SecretFields::new(secrets, draft.key_path.clone(), window, cx));
        let credential_ix = draft.credential.and_then(|id| {
            credentials
                .iter()
                .position(|credential| credential.id == id)
        });
        let credential = cx.new(|cx| {
            SelectState::new(
                credentials
                    .iter()
                    .map(credential_option)
                    .collect::<Vec<_>>(),
                credential_ix.map(IndexPath::new),
                window,
                cx,
            )
            .searchable(true)
        });
        let group_ix = group_ids
            .iter()
            .position(|group| *group == draft.group)
            .unwrap_or(0);
        let group =
            cx.new(|cx| SelectState::new(group_names, Some(IndexPath::new(group_ix)), window, cx));
        let subscriptions = vec![
            cx.observe(&auth, |_, _, cx| cx.notify()),
            cx.observe(&credential, |_, _, cx| cx.notify()),
            cx.observe(&fields, |_, _, cx| cx.notify()),
        ];

        if editing.is_some() {
            // The typed login's secrets, even for a host that uses a
            // credential now: they are what switching back would show.
            fields.update(cx, |fields, cx| {
                let key_path = (draft.auth == AuthKind::Key)
                    .then(|| draft.key_path.clone())
                    .flatten();
                fields.load_saved(Some(draft.password_secret()), key_path, cx)
            });
        }
        Self {
            store,
            editing,
            name,
            host,
            port,
            source: if credential_ix.is_some() {
                AuthSource::Credential
            } else {
                AuthSource::Manual
            },
            user,
            auth,
            fields,
            credential,
            credentials,
            group,
            group_ids,
            error: None,
            testing_connection: false,
            tester,
            editing_connected,
            _subscriptions: subscriptions,
        }
    }

    fn set_source(&mut self, source: AuthSource, cx: &mut Context<Self>) {
        if self.source != source {
            self.source = source;
            self.error = None;
            cx.notify();
        }
    }

    /// The authentication method the form has selected.
    fn auth(&self, cx: &App) -> AuthKind {
        self.auth
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| AuthKind::ALL.get(ix.row).copied())
            .unwrap_or_default()
    }

    /// The credential the form has selected.
    fn selected_credential(&self, cx: &App) -> Option<&Credential> {
        self.credential
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| self.credentials.get(ix.row))
    }

    /// The address and port, or why they will not do.
    fn endpoint(&self, cx: &App) -> Result<(String, u16), &'static str> {
        let host = self.host.read(cx).value().trim().to_string();
        let port = parse_port(self.port.read(cx).value().trim());
        match (host.is_empty(), port) {
            (true, _) => Err("请输入地址"),
            (_, None) => Err("端口必须是 1 到 65535 之间的数字"),
            (_, Some(port)) => Ok((host, port)),
        }
    }

    /// The login the form's current values describe, saved or not, or why
    /// there is nothing to test yet.
    fn login_test(&self, cx: &App) -> Result<LoginTest, &'static str> {
        let (host, port) = self.endpoint(cx)?;
        if self.source == AuthSource::Credential {
            let credential = self.selected_credential(cx).ok_or("请选择凭据")?;
            return Ok(LoginTest::saved(SessionLogin::with_credential(
                host, port, credential,
            )));
        }
        let user = self.user.read(cx).value().trim().to_string();
        if user.is_empty() {
            return Err("请输入用户名");
        }
        let auth = self.auth(cx);
        let fields = self.fields.read(cx);
        let key_path = fields.key_path(cx);
        if auth == AuthKind::Key && key_path.is_empty() {
            return Err("私钥认证需要选择私钥文件");
        }
        let key_path = (auth == AuthKind::Key).then(|| key_path.into());
        let mut request = LoginTest::typed(SessionLogin::manual(host, port, user, auth, key_path));
        // Only what the chosen method uses, which is also what the form shows.
        if uses_password(auth) {
            let password = fields.password(cx);
            if !password.is_empty() {
                request = request.with_password(password);
            }
        }
        if auth == AuthKind::Key {
            let passphrase = fields.passphrase(cx);
            if !passphrase.is_empty() {
                request = request.with_passphrase(passphrase);
            }
        }
        Ok(request)
    }

    /// Log in with the form's current values without saving them, and report
    /// the outcome as a notification. The login runs on a thread of its own;
    /// a host key seen for the first time is put to the user in a dialog
    /// above this one.
    fn test_connection(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.testing_connection {
            return;
        }
        let request = match self.login_test(cx) {
            Ok(request) => request,
            Err(reason) => {
                window.push_notification(connection_test_notification(Err(reason.into())), cx);
                return;
            }
        };
        let (trust_tx, trust_rx) = std::sync::mpsc::channel::<TrustQuestion>();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let tester = self.tester.clone();
        let spawned = std::thread::Builder::new()
            .name("shellrs-connection-test".into())
            .spawn(move || {
                // Nobody left to answer (the form closed) reads as "no".
                let trust: TrustCallback = Box::new(move |prompt| {
                    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                    trust_tx.send((prompt, reply_tx)).is_ok() && reply_rx.recv().unwrap_or(false)
                });
                let _ = result_tx.send(tester.test(request, trust));
            });
        if let Err(error) = spawned {
            window.push_notification(
                connection_test_notification(Err(format!("无法启动连接测试：{error}"))),
                cx,
            );
            return;
        }
        self.testing_connection = true;
        cx.notify();
        // Polled rather than woken by the worker, as with every other worker
        // in the application.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                while let Ok((prompt, reply)) = trust_rx.try_recv() {
                    if this
                        .update_in(cx, |_, window, cx| ask_to_trust(prompt, reply, window, cx))
                        .is_err()
                    {
                        return;
                    }
                }
                let result = match result_rx.try_recv() {
                    Ok(result) => result,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        if this.update(cx, |_, _| ()).is_err() {
                            return;
                        }
                        continue;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        Err("连接测试意外中止".to_string())
                    }
                };
                this.update_in(cx, |this, window, cx| {
                    this.testing_connection = false;
                    window.push_notification(connection_test_notification(result), cx);
                    cx.notify();
                })
                .ok();
                return;
            }
        })
        .detach();
    }

    /// How the form says the host logs in, or what is missing.
    fn committed_login(&self, cx: &App) -> Result<CommittedLogin, &'static str> {
        match self.source {
            AuthSource::Manual => {
                let auth = self.auth(cx);
                let key_path = self.fields.read(cx).key_path(cx);
                if auth == AuthKind::Key && key_path.is_empty() {
                    return Err("私钥认证需要选择私钥文件");
                }
                Ok(CommittedLogin::Typed { auth, key_path })
            }
            AuthSource::Credential => self
                .selected_credential(cx)
                .map(|credential| CommittedLogin::Saved {
                    credential: credential.id,
                    user: credential.user.clone(),
                })
                .ok_or("请选择凭据"),
        }
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_string();
        let checked = if name.is_empty() {
            Err("请输入名称")
        } else {
            self.endpoint(cx)
                .and_then(|endpoint| Ok((endpoint, self.committed_login(cx)?)))
        };
        let ((host, port), login) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return false;
            }
        };

        let group = self
            .group
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| self.group_ids.get(ix.row).copied())
            .unwrap_or(None);
        // Secrets never ride along in the draft, which derives `Debug`. They
        // go to the keychain separately, under the endpoint the draft names;
        // a login through a credential leaves the host's own entries alone.
        let mut secret_changes: Vec<(SecretRef, Option<String>)> = Vec::new();
        let draft = match login {
            CommittedLogin::Saved { credential, user } => {
                SessionDraft::new(name, host, port, user, AuthKind::Auto, group)
                    .with_credential(credential)
            }
            CommittedLogin::Typed { auth, key_path } => {
                let user = self.user.read(cx).value().trim().to_string();
                let user = if user.is_empty() {
                    DEFAULT_USER.to_string()
                } else {
                    user
                };
                let mut draft = SessionDraft::new(name, host, port, user, auth, group);
                let fields = self.fields.read(cx);
                if uses_password(auth)
                    && let Some(change) = fields.password_change(cx)
                {
                    secret_changes.push((draft.password_secret(), change));
                }
                if auth == AuthKind::Key {
                    if let Some(change) = fields.passphrase_change(cx) {
                        secret_changes.push((SecretRef::passphrase(&key_path), change));
                    }
                    draft = draft.with_key_path(key_path);
                }
                draft
            }
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
            for (secret, change) in secret_changes {
                store.save_secret(secret, change, cx);
            }
        });
        // The dialog is about to close and take the form with it; drop the
        // plaintext now rather than waiting for the entity.
        self.fields
            .update(cx, |fields, cx| fields.clear(window, cx));
        self.error = None;
        true
    }

    /// The fields of a login typed into the form.
    fn manual_fields(&self, form: Form, cx: &mut Context<Self>) -> Form {
        let auth = self.auth(cx);
        let fields = self.fields.read(cx);
        form.child(
            Field::new()
                .label("用户名")
                .col_span(4)
                .child(Input::new(&self.user).id("session-user").small()),
        )
        .child(
            Field::new()
                .label("认证类型")
                .col_span(4)
                .child(Select::new(&self.auth).id("session-auth").small()),
        )
        .when(uses_password(auth), |form| {
            form.child(
                Field::new()
                    .label("密码")
                    .col_span(4)
                    .child(fields.password_input("session-password")),
            )
        })
        .when(auth == AuthKind::Key, |form| {
            form.child(
                Field::new()
                    .label("私钥文件")
                    .required(true)
                    .col_span(4)
                    .child(SecretFields::key_path_input(
                        &self.fields,
                        "session-key-path",
                        "choose-key",
                        cx,
                    )),
            )
            .child(
                Field::new()
                    .label("私钥口令")
                    .col_span(4)
                    .child(fields.passphrase_input("session-passphrase")),
            )
        })
    }

    /// The field that picks a saved credential, with what it logs in as.
    fn credential_field(&self, cx: &App) -> Field {
        let summary = match self.selected_credential(cx) {
            Some(credential) => Some(credential_summary(credential)),
            None if self.credentials.is_empty() => {
                Some("还没有凭据，可在侧栏的「凭据」中新建".into())
            }
            None => None,
        };
        Field::new()
            .label("凭据")
            .required(true)
            .col_span(4)
            .child(
                Select::new(&self.credential)
                    .id("session-credential")
                    .placeholder("请选择凭据")
                    .search_placeholder("搜索凭据")
                    .empty(|_, cx| {
                        div()
                            .py_4()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child("还没有凭据")
                    })
                    .small(),
            )
            .when_some(summary, |field, summary: SharedString| {
                field.description_fn(move |_, _| {
                    div()
                        .id("session-credential-summary")
                        .test_support()
                        .aria_label(summary.clone())
                        .child(summary.clone())
                })
            })
    }
}

impl Render for SessionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let source = self.source;
        let keychain = self.fields.read(cx).keychain_available();
        let secret_note = if keychain {
            "密码保存在系统钥匙串，不会写入 ShellRS 的数据库。"
        } else {
            "系统钥匙串不可用，这台机器上无法保存密码，每次连接都会询问。"
        };
        // Four columns so the address and its port share a row, as they are
        // written (`host:port`); every other field takes a row of its own.
        let form = Form::new()
            .columns(4)
            .child(
                Field::new()
                    .label("名称")
                    .required(true)
                    .col_span(4)
                    .child(Input::new(&self.name).id("session-name").small()),
            )
            .child(
                Field::new()
                    .label("地址")
                    .required(true)
                    .col_span(3)
                    .child(Input::new(&self.host).id("session-host").small()),
            )
            .child(
                Field::new()
                    .label("端口")
                    .child(Input::new(&self.port).id("session-port").small()),
            )
            .child(
                Field::new().label("认证方式").col_span(4).child(
                    RadioGroup::horizontal("session-auth-source")
                        .selected_index(AuthSource::ALL.iter().position(|each| *each == source))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some(source) = AuthSource::ALL.get(*ix) {
                                this.set_source(*source, cx);
                            }
                        }))
                        .children(
                            AuthSource::ALL
                                .map(|each| Radio::new(each.label()).label(each.label()).small()),
                        ),
                ),
            );
        let form = match source {
            AuthSource::Manual => self.manual_fields(form, cx),
            AuthSource::Credential => form.child(self.credential_field(cx)),
        };
        v_flex()
            .gap_3()
            .w_full()
            .child(
                form.child(
                    Field::new()
                        .label("分组")
                        .col_span(4)
                        .child(Select::new(&self.group).small()),
                ),
            )
            .when(source == AuthSource::Manual, |form| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(secret_note),
                )
            })
            .when(self.editing_connected, |form| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("保存后连接设置将立即生效并重连。"),
                )
            })
            .when_some(self.error.clone(), |form, error| {
                form.child(form_error(error.clone(), cx).aria_label(error))
            })
    }
}

/// How a host the form commits logs in.
enum CommittedLogin {
    Typed {
        auth: AuthKind,
        key_path: String,
    },
    Saved {
        credential: CredentialId,
        user: SharedString,
    },
}

/// A credential as the host form's select lists it: `运维（root · 密码）`.
fn credential_option(credential: &Credential) -> SharedString {
    format!("{}（{}）", credential.name, credential.summary()).into()
}

/// What a host using `credential` logs in as and with.
fn credential_summary(credential: &Credential) -> SharedString {
    let with = match credential.kind {
        CredentialKind::Password => "使用凭据保存的密码".to_string(),
        CredentialKind::Key => format!(
            "使用私钥 {}",
            credential.key_path.as_deref().unwrap_or_default()
        ),
        CredentialKind::Agent => "使用 SSH Agent 中的密钥".to_string(),
    };
    format!("以 {} 登录，{with}", credential.user).into()
}

/// A trust question from the test's worker, with where to send the answer.
type TrustQuestion = (UnknownHostPrompt, std::sync::mpsc::Sender<bool>);

/// Put a first-seen host key to the user, above the session dialog. Closing
/// the dialog any other way than trusting counts as declining.
fn ask_to_trust(
    prompt: UnknownHostPrompt,
    reply: std::sync::mpsc::Sender<bool>,
    window: &mut Window,
    cx: &mut App,
) {
    let description = prompt.description();
    window.open_alert_dialog(cx, move |alert, _, _| {
        let answer = |trusted: bool| {
            let reply = reply.clone();
            move || {
                let _ = reply.send(trusted);
            }
        };
        let (trust, decline, dismiss) = (answer(true), answer(false), answer(false));
        alert
            .title("首次连接此主机")
            .description(description.clone())
            .button_props(
                DialogButtonProps::default()
                    .ok_text("信任并继续")
                    .cancel_text("取消"),
            )
            .show_cancel(true)
            .on_ok(move |_, _, _| {
                trust();
                true
            })
            .on_cancel(move |_, _, _| {
                decline();
                true
            })
            .on_close(move |_, _, _| dismiss())
    });
}

/// A connection test has two outcomes: it connected, or it did not and the
/// message says why.
fn connection_test_notification(result: Result<(), String>) -> Notification {
    match result {
        Ok(()) => Notification::success("连接成功"),
        Err(reason) => Notification::error(reason).title("连接失败"),
    }
}

/// Whether this authentication kind can end up asking for a password.
/// `Auto` walks agent, then keys, then password, so it can.
fn uses_password(auth: AuthKind) -> bool {
    matches!(auth, AuthKind::Auto | AuthKind::Password)
}

/// Open the new-session (`editing == None`) or edit-session dialog.
/// `preselect_group` fills in the group field of a new session, so creating
/// one from a group's context menu lands it in that group. `tester` backs the
/// dialog's 「测试连接」 button.
pub fn open_session_dialog(
    editing: Option<SessionId>,
    preselect_group: Option<GroupId>,
    store: Entity<SessionStore>,
    tester: SharedConnectionTester,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| SessionForm::new(editing, preselect_group, store, tester, window, cx));
    let title: SharedString = if editing.is_some() {
        "编辑主机"
    } else {
        "新建主机"
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
        move |dialog, _, cx| {
            dialog
                .title(title.clone())
                // Closed by its buttons or Escape, not by a click beside it.
                .overlay_closable(false)
                .child(form.clone())
                .footer(
                    DialogFooter::new()
                        .w_full()
                        .justify_between()
                        .child(
                            Button::new("test-connection")
                                .label("测试连接")
                                .icon(crate::app::CatalogIcon::Plug)
                                .small()
                                .loading(form.read(cx).testing_connection)
                                .on_click({
                                    let form = form.clone();
                                    move |event, window, cx| {
                                        form.update(cx, |form, cx| {
                                            form.test_connection(event, window, cx)
                                        });
                                    }
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .child(Button::new("cancel").label("取消").small().on_click(
                                    |_, window, cx| {
                                        window.dispatch_action(Box::new(Cancel), cx);
                                    },
                                ))
                                .child(
                                    Button::new("commit")
                                        .primary()
                                        .label(commit_label.clone())
                                        .small()
                                        .on_click(|_, window, cx| {
                                            window.dispatch_action(
                                                Box::new(Confirm { secondary: false }),
                                                cx,
                                            );
                                        }),
                                ),
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
/// `affected` is whether tabs of the session are open and how many of them
/// are transferring; `forwards` is how many port forwards go through it.
pub fn confirm_delete_session(
    session: &Session,
    affected: (bool, usize),
    forwards: usize,
    on_delete: DeleteHandler,
    window: &mut Window,
    cx: &mut App,
) {
    let (closes_tabs, uploads) = affected;
    confirm_delete(
        &session.name,
        describe_session_delete(closes_tabs, uploads, forwards),
        on_delete,
        window,
        cx,
    );
}

/// What the delete dialog says goes with the session. `None` for a session
/// with nothing open and no port forwards.
fn describe_session_delete(
    closes_tabs: bool,
    uploads: usize,
    forwards: usize,
) -> Option<SharedString> {
    let mut description = String::new();
    if closes_tabs {
        description.push_str("会一并关闭该主机已打开的终端和 SFTP 标签。");
        if uploads > 0 {
            description.push_str(&format!("将停止 {uploads} 个传输批次并保留续传进度。"));
        }
    }
    if forwards > 0 {
        description.push_str(&format!("将同时删除经由该主机的 {forwards} 条端口转发。"));
    }
    (!description.is_empty()).then(|| description.into())
}

#[cfg(test)]
mod tests {
    use super::{AuthKind, describe_session_delete, uses_password};

    #[test]
    fn deleting_a_session_says_what_goes_with_it() {
        assert_eq!(describe_session_delete(false, 0, 0), None);
        assert_eq!(
            describe_session_delete(true, 0, 0).as_deref(),
            Some("会一并关闭该主机已打开的终端和 SFTP 标签。")
        );
        assert_eq!(
            describe_session_delete(true, 2, 0).as_deref(),
            Some("会一并关闭该主机已打开的终端和 SFTP 标签。将停止 2 个传输批次并保留续传进度。")
        );
        assert_eq!(
            describe_session_delete(false, 0, 3).as_deref(),
            Some("将同时删除经由该主机的 3 条端口转发。")
        );
        assert_eq!(
            describe_session_delete(true, 0, 1).as_deref(),
            Some("会一并关闭该主机已打开的终端和 SFTP 标签。将同时删除经由该主机的 1 条端口转发。")
        );
    }

    #[test]
    fn auto_and_password_can_reach_a_password_prompt() {
        assert!(uses_password(AuthKind::Auto));
        assert!(uses_password(AuthKind::Password));
        assert!(!uses_password(AuthKind::Key));
    }
}
