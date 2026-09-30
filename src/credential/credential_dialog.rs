use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, WindowExt as _,
    form::{Field, Form},
    input::{Input, InputState},
    radio::{Radio, RadioGroup},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::secrets::SecretRef;
use crate::session::{
    CredentialDraft, CredentialId, CredentialKind, DEFAULT_USER, SecretFields, SessionStore,
};
use crate::shared::{commit_footer, form_error};

/// Where the SSH agent is found, as the form explains it.
#[cfg(windows)]
const AGENT_NOTE: &str = "登录时使用 Windows 的 OpenSSH Authentication Agent 服务中的密钥。";
#[cfg(not(windows))]
const AGENT_NOTE: &str = "登录时使用 SSH Agent（SSH_AUTH_SOCK 所指）中的密钥。";

/// The body of the new/edit credential dialog.
pub struct CredentialForm {
    store: Entity<SessionStore>,
    editing: Option<CredentialId>,
    kind: CredentialKind,
    name: Entity<InputState>,
    user: Entity<InputState>,
    /// The password, key file and passphrase, shared with the host form.
    fields: Entity<SecretFields>,
    /// How many hosts use the credential being edited, and how many of them
    /// are connected right now.
    hosts: usize,
    connected: usize,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl CredentialForm {
    pub fn new(
        editing: Option<CredentialId>,
        store: Entity<SessionStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (credential, secrets, hosts, connected) = {
            let read = store.read(cx);
            let credential = editing.and_then(|id| read.credential(id)).cloned();
            let using: Vec<_> = editing
                .map(|id| read.sessions_using(id).collect())
                .unwrap_or_default();
            let connected = using
                .iter()
                .filter(|session| session.state.is_connected())
                .count();
            (credential, read.secrets(), using.len(), connected)
        };
        let draft = credential
            .as_ref()
            .map(|credential| credential.draft())
            .unwrap_or_else(|| CredentialDraft::new("", CredentialKind::default(), DEFAULT_USER));

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 生产环境 root")
                .default_value(draft.name.clone())
        });
        let user = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(DEFAULT_USER)
                .default_value(draft.user.clone())
        });
        let fields = cx.new(|cx| SecretFields::new(secrets, draft.key_path.clone(), window, cx));
        if let Some(credential) = &credential {
            fields.update(cx, |fields, cx| {
                fields.load_saved(
                    (credential.kind == CredentialKind::Password)
                        .then(|| credential.password_secret()),
                    credential.key_path.clone(),
                    cx,
                )
            });
        }
        let subscriptions = vec![cx.observe(&fields, |_, _, cx| cx.notify())];
        Self {
            store,
            editing,
            kind: draft.kind,
            name,
            user,
            fields,
            hosts,
            connected,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Put the keyboard where a new credential starts: its name.
    fn focus_name(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| input.focus(window, cx));
    }

    fn set_kind(&mut self, kind: CredentialKind, cx: &mut Context<Self>) {
        if self.kind != kind {
            self.kind = kind;
            self.error = None;
            cx.notify();
        }
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let fields = self.fields.read(cx);
        let draft = CredentialDraft::new(
            self.name.read(cx).value().to_string(),
            self.kind,
            self.user.read(cx).value().to_string(),
        )
        .with_key_path(fields.key_path(cx))
        .validated();
        let draft = match draft {
            Ok(draft) => draft,
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                return false;
            }
        };
        // Secrets never ride along in the draft, which derives `Debug`.
        let password_change = (draft.kind == CredentialKind::Password)
            .then(|| fields.password_change(cx))
            .flatten();
        let passphrase_change = draft.key_path.as_ref().and_then(|path| {
            Some((
                SecretRef::passphrase(path.as_ref()),
                fields.passphrase_change(cx)?,
            ))
        });

        let editing = self.editing;
        self.store.update(cx, |store, cx| {
            // The credential first: an edit that reconnects its hosts reads
            // the keychain later, on the connection's own thread.
            let id = match editing {
                Some(id) => store.update_credential(id, draft, cx).then_some(id),
                None => Some(store.insert_credential(draft, cx)),
            };
            let secret = id
                .and_then(|id| store.credential(id))
                .map(|credential| credential.password_secret());
            if let (Some(secret), Some(change)) = (secret, password_change) {
                store.save_secret(secret, change, cx);
            }
            if let Some((secret, change)) = passphrase_change {
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

    /// What editing the credential does to the hosts using it.
    fn usage_note(&self) -> Option<String> {
        if self.hosts == 0 {
            return None;
        }
        let mut note = format!("有 {} 台主机使用此凭据。", self.hosts);
        if self.connected > 0 {
            note.push_str("修改用户名、类型或私钥文件后，其中已连接的主机会重新连接。");
        }
        Some(note)
    }
}

impl Render for CredentialForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        let fields = self.fields.read(cx);
        let muted = cx.theme().muted_foreground;
        let secret_note = match kind {
            CredentialKind::Agent => None,
            _ if fields.keychain_available() => {
                Some("密码和口令保存在系统钥匙串，不会写入 ShellRS 的数据库。")
            }
            _ => Some("系统钥匙串不可用，这台机器上无法保存密码，每次连接都会询问。"),
        };
        let form = Form::new()
            .child(
                Field::new().label("类型").child(
                    RadioGroup::horizontal("credential-kind")
                        .selected_index(CredentialKind::ALL.iter().position(|each| *each == kind))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some(kind) = CredentialKind::ALL.get(*ix) {
                                this.set_kind(*kind, cx);
                            }
                        }))
                        .children(
                            CredentialKind::ALL
                                .map(|each| Radio::new(each.as_str()).label(each.label()).small()),
                        ),
                ),
            )
            .child(
                Field::new()
                    .label("名称")
                    .required(true)
                    .child(Input::new(&self.name).id("credential-name").small()),
            )
            .child(
                Field::new()
                    .label("用户名")
                    .child(Input::new(&self.user).id("credential-user").small()),
            );
        let form = match kind {
            CredentialKind::Password => form.child(
                Field::new()
                    .label("密码")
                    .child(fields.password_input("credential-password")),
            ),
            CredentialKind::Key => form
                .child(Field::new().label("私钥文件").required(true).child(
                    SecretFields::key_path_input(
                        &self.fields,
                        "credential-key-path",
                        "choose-credential-key",
                        cx,
                    ),
                ))
                .child(
                    Field::new()
                        .label("私钥口令")
                        .child(fields.passphrase_input("credential-passphrase")),
                ),
            CredentialKind::Agent => form,
        };
        v_flex()
            .gap_3()
            .w_full()
            .child(form)
            .when(kind == CredentialKind::Agent, |view| {
                view.child(
                    div()
                        .id("credential-agent-note")
                        .test_support()
                        .aria_label(AGENT_NOTE)
                        .text_sm()
                        .text_color(muted)
                        .child(AGENT_NOTE),
                )
            })
            .when_some(secret_note, |view, note| {
                view.child(div().text_sm().text_color(muted).child(note))
            })
            .when_some(self.usage_note(), |view, note| {
                view.child(
                    div()
                        .id("credential-usage")
                        .test_support()
                        .aria_label(note.clone())
                        .text_sm()
                        .text_color(muted)
                        .child(note),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(form_error(error.clone(), cx).aria_label(error))
            })
    }
}

/// Open the new-credential (`editing == None`) or edit-credential dialog.
pub fn open_credential_dialog(
    editing: Option<CredentialId>,
    store: Entity<SessionStore>,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| CredentialForm::new(editing, store, window, cx));
    let title: SharedString = if editing.is_some() {
        "编辑凭据"
    } else {
        "新建凭据"
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
    // Focused in the same update that opened the dialog, which is the one
    // that sticks: a later request loses to the dialog's own focus.
    if editing.is_none() {
        form.update(cx, |form, cx| form.focus_name(window, cx));
    }
}
