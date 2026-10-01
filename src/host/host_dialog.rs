use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    combobox::{Combobox, ComboboxEvent, ComboboxState},
    dialog::{Cancel, Confirm, DialogButtonProps, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    notification::Notification,
    searchable_list::{SearchableListItem, SearchableVec},
    select::{Select, SelectState},
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::connection::{LoginTest, SharedConnectionTester, TrustCallback, UnknownHostPrompt};

use super::secret_fields::SecretFields;
use super::{
    AuthKind, Credential, CredentialId, CredentialKind, DEFAULT_USER, GroupId, Host, HostDraft,
    HostId, HostLogin, HostStore, ProxyKind, ProxySettings, Route, group_options,
};
pub use crate::shared::DeleteHandler;
use crate::shared::{Segment, SegmentedControl, confirm_delete, form_error, parse_port};

/// The label of the row that puts a host at the root of the tree.
pub const NO_GROUP_LABEL: &str = "（无分组）";

/// Where the dialog's top sits and how tall it may grow, as fractions of
/// the window's height.
const DIALOG_TOP: f32 = 0.05;
const DIALOG_MAX_HEIGHT: f32 = 0.9;

/// How a host logs in, as the form's 「认证方式」 offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthSource {
    /// A password typed into this form, or asked for each time.
    Password,
    /// A saved credential: a password several hosts share, a key, the agent.
    Credential,
    /// Nothing typed: the server lets the user in, or the SSH agent or a
    /// default key does.
    NoPassword,
}

impl AuthSource {
    /// Every way, in the order the form lists them.
    const ALL: [AuthSource; 3] = [
        AuthSource::Password,
        AuthSource::Credential,
        AuthSource::NoPassword,
    ];

    fn label(self) -> &'static str {
        match self {
            AuthSource::Password => "密码",
            AuthSource::Credential => "使用凭据",
            AuthSource::NoPassword => "无密码",
        }
    }

    /// How a host of its own logs in, for the two ways that are not a
    /// credential.
    fn auth(self) -> Option<AuthKind> {
        match self {
            AuthSource::Password => Some(AuthKind::Password),
            AuthSource::NoPassword => Some(AuthKind::NoPassword),
            AuthSource::Credential => None,
        }
    }
}

/// What 「无密码」 tries, under the choice.
const NO_PASSWORD_NOTE: &str = "依次尝试服务器免认证、SSH Agent 和 ~/.ssh 中的默认私钥。";

/// How a host is reached, as the form's 「连接方式」 offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RouteChoice {
    Direct,
    Jump,
    Proxy,
}

impl RouteChoice {
    /// Every way, in the order the form lists them.
    const ALL: [RouteChoice; 3] = [RouteChoice::Direct, RouteChoice::Jump, RouteChoice::Proxy];

    fn label(self) -> &'static str {
        match self {
            RouteChoice::Direct => "直接连接",
            RouteChoice::Jump => "SSH 跳板",
            RouteChoice::Proxy => "代理连接",
        }
    }

    fn of(route: &Route) -> Self {
        match route {
            Route::Direct => RouteChoice::Direct,
            Route::Jump(_) => RouteChoice::Jump,
            Route::Proxy(_) => RouteChoice::Proxy,
        }
    }
}

/// What the jump-host box says above the chain.
const JUMP_NOTE: &str = "依次经过跳板主机连接到当前主机，可添加多台。";

/// What the proxy box says under its fields.
const PROXY_NOTE: &str = "目标地址由代理服务器解析；代理不需要认证时，用户名和密码留空。";

/// A host the form offers as a jump host, as it was when the form opened.
#[derive(Clone)]
struct JumpHost {
    id: HostId,
    name: SharedString,
    /// `host:port`.
    address: SharedString,
}

impl JumpHost {
    fn of(host: &Host) -> Self {
        Self {
            id: host.id,
            name: host.name.clone(),
            address: format!("{}:{}", host.address, host.port).into(),
        }
    }
}

impl SearchableListItem for JumpHost {
    type Value = HostId;

    /// The name and the address, both of which the search looks in.
    fn title(&self) -> SharedString {
        format!("{}（{}）", self.name, self.address).into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        h_flex()
            .min_w_0()
            .gap_2()
            .child(div().truncate().child(self.name.clone()))
            .child(
                div()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.address.clone()),
            )
    }

    fn value(&self) -> &HostId {
        &self.id
    }
}

type JumpPicker = ComboboxState<SearchableVec<JumpHost>>;

/// The body of the new/edit host dialog. Owns the field states and
/// validates on commit; the store is only touched when validation passes.
pub struct HostForm {
    store: Entity<HostStore>,
    editing: Option<HostId>,
    name: Entity<InputState>,
    address: Entity<InputState>,
    port: Entity<InputState>,
    source: AuthSource,
    user: Entity<InputState>,
    /// The password of a login typed here.
    fields: Entity<SecretFields>,
    credential: Entity<SelectState<Vec<SharedString>>>,
    /// The credentials as they were when the form opened, parallel to the
    /// credential select's rows. The dialog is modal, so nothing can change
    /// them while it is open.
    credentials: Vec<Credential>,
    /// Where ShellRS keeps pasted and generated keys, to name those in the
    /// credential's summary.
    key_dir: Option<PathBuf>,
    group: Entity<SelectState<Vec<SharedString>>>,
    /// Parallel to the group select's rows; `None` is the root of the tree.
    group_ids: Vec<Option<GroupId>>,
    route: RouteChoice,
    /// The jump hosts, in order; `None` is one that has been deleted.
    hops: Vec<Option<HostId>>,
    /// Every other host, as it was when the form opened: what can be a
    /// jump host, and what the hops are called.
    jump_hosts: Vec<JumpHost>,
    jump_picker: Entity<JumpPicker>,
    proxy_kind: Entity<SelectState<Vec<SharedString>>>,
    proxy_host: Entity<InputState>,
    proxy_port: Entity<InputState>,
    proxy_user: Entity<InputState>,
    /// The proxy's password.
    proxy_secret: Entity<SecretFields>,
    notes: Entity<TextareaState>,
    error: Option<SharedString>,
    testing_connection: bool,
    /// Logs in with the form's current values for 「测试连接」.
    tester: SharedConnectionTester,
    editing_connected: bool,
    _subscriptions: Vec<Subscription>,
}

impl HostForm {
    pub fn new(
        editing: Option<HostId>,
        preselect_group: Option<GroupId>,
        store: Entity<HostStore>,
        tester: SharedConnectionTester,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let secrets = store.read(cx).secrets();
        let (draft, options, credentials, key_dir, editing_connected, jump_hosts) = {
            let read = store.read(cx);
            (
                editing.and_then(|id| read.host(id)).map(Host::draft),
                group_options(read.groups(), &[]),
                read.credentials().to_vec(),
                read.key_dir().map(Path::to_path_buf),
                editing
                    .and_then(|id| read.host(id))
                    .is_some_and(|host| host.state != super::ConnectionState::Disconnected),
                read.hosts()
                    .iter()
                    .filter(|host| Some(host.id) != editing)
                    .map(JumpHost::of)
                    .collect::<Vec<_>>(),
            )
        };
        // A host with no group sits at the root of the tree, which is where
        // every host starts when the database is still empty.
        let mut group_ids: Vec<Option<GroupId>> = vec![None];
        let mut group_names: Vec<SharedString> = vec![NO_GROUP_LABEL.into()];
        for (id, path) in options {
            group_ids.push(Some(id));
            group_names.push(path);
        }
        let draft = draft.unwrap_or_else(|| {
            HostDraft::new(
                "",
                "",
                22,
                DEFAULT_USER,
                AuthKind::default(),
                preselect_group,
            )
        });

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 web-01")
                .default_value(draft.name.clone())
        });
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("主机名或 IP 地址")
                .default_value(draft.address.clone())
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
        let fields = cx.new(|cx| SecretFields::new(secrets.clone(), None, window, cx));
        let credential_ix = draft.credential.and_then(|id| {
            credentials
                .iter()
                .position(|credential| credential.id == id)
        });
        let credential = cx.new(|cx| {
            SelectState::new(
                credentials
                    .iter()
                    .map(|credential| credential_option(credential, key_dir.as_deref()))
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

        let hops = match &draft.route {
            Route::Jump(hops) => hops.clone(),
            _ => Vec::new(),
        };
        let jump_picker = cx.new(|cx| {
            ComboboxState::new(
                SearchableVec::new(available_jump_hosts(&jump_hosts, &hops)),
                Vec::new(),
                window,
                cx,
            )
            .searchable(true)
        });
        let proxy = match &draft.route {
            Route::Proxy(proxy) => Some(proxy.clone()),
            _ => None,
        };
        let proxy_kind_ix = ProxyKind::ALL
            .iter()
            .position(|kind| Some(*kind) == proxy.as_ref().map(|proxy| proxy.kind))
            .unwrap_or(0);
        let proxy_kind = cx.new(|cx| {
            SelectState::new(
                ProxyKind::ALL
                    .iter()
                    .map(|kind| SharedString::from(kind.label()))
                    .collect::<Vec<_>>(),
                Some(IndexPath::new(proxy_kind_ix)),
                window,
                cx,
            )
        });
        let proxy_host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例如 127.0.0.1")
                .default_value(
                    proxy
                        .as_ref()
                        .map(|proxy| proxy.host.clone())
                        .unwrap_or_default(),
                )
        });
        let proxy_port = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                proxy
                    .as_ref()
                    .map(|proxy| proxy.port.to_string())
                    .unwrap_or_default(),
            )
        });
        let proxy_user = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("可选")
                .default_value(
                    proxy
                        .as_ref()
                        .and_then(|proxy| proxy.user.clone())
                        .unwrap_or_default(),
                )
        });
        let proxy_secret = cx.new(|cx| {
            let mut fields = SecretFields::new(secrets, None, window, cx);
            fields.set_password_placeholder("可选", window, cx);
            fields
        });
        if let Some(secret) = proxy.as_ref().and_then(ProxySettings::password_secret) {
            proxy_secret.update(cx, |fields, cx| fields.load_saved(Some(secret), None, cx));
        }

        let notes = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .default_value(draft.notes.clone())
        });

        let subscriptions = vec![
            cx.observe(&credential, |_, _, cx| cx.notify()),
            cx.observe(&fields, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &jump_picker,
                window,
                |this, _, event: &ComboboxEvent<SearchableVec<JumpHost>>, window, cx| {
                    if let ComboboxEvent::Change(chosen) = event
                        && let Some(id) = chosen.first()
                    {
                        this.add_hop(*id, window, cx);
                    }
                },
            ),
        ];

        if editing.is_some() {
            // The host's own password, even for a host that does not use one
            // now: it is what switching back would show.
            fields.update(cx, |fields, cx| {
                fields.load_saved(Some(draft.password_secret()), None, cx)
            });
        }
        Self {
            store,
            editing,
            name,
            address,
            port,
            source: match (credential_ix, draft.auth) {
                (Some(_), _) => AuthSource::Credential,
                (None, AuthKind::Password) => AuthSource::Password,
                (None, AuthKind::NoPassword) => AuthSource::NoPassword,
            },
            user,
            fields,
            credential,
            credentials,
            key_dir,
            group,
            group_ids,
            route: RouteChoice::of(&draft.route),
            hops,
            jump_hosts,
            jump_picker,
            proxy_kind,
            proxy_host,
            proxy_port,
            proxy_user,
            proxy_secret,
            notes,
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

    fn set_route(&mut self, route: RouteChoice, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            self.error = None;
            cx.notify();
        }
    }

    /// Put `id` at the end of the jump hosts, and take it off the hosts that
    /// can still be added.
    fn add_hop(&mut self, id: HostId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.hops.contains(&Some(id)) {
            self.hops.push(Some(id));
            self.error = None;
        }
        self.refresh_jump_picker(window, cx);
        cx.notify();
    }

    fn remove_hop(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        if position < self.hops.len() {
            self.hops.remove(position);
            self.error = None;
            self.refresh_jump_picker(window, cx);
            cx.notify();
        }
    }

    /// Offer the hosts not in the chain yet, with nothing chosen and no
    /// search left over: choosing one adds it, and the picker is ready for
    /// the next.
    fn refresh_jump_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let available = available_jump_hosts(&self.jump_hosts, &self.hops);
        self.jump_picker.update(cx, |picker, cx| {
            picker.clear_selection(cx);
            picker.set_items(SearchableVec::new(available), window, cx);
            picker.set_query("", window, cx);
        });
    }

    /// The route the form describes, or what is missing from it.
    fn committed_route(&self, cx: &App) -> Result<Route, &'static str> {
        match self.route {
            RouteChoice::Direct => Ok(Route::Direct),
            RouteChoice::Jump if self.hops.is_empty() => Err("请添加跳板主机"),
            RouteChoice::Jump if self.hops.contains(&None) => Err("请移除已删除的跳板主机"),
            RouteChoice::Jump => Ok(Route::Jump(self.hops.clone())),
            RouteChoice::Proxy => {
                let host = self.proxy_host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return Err("请输入代理地址");
                }
                let port = parse_port(self.proxy_port.read(cx).value().trim())
                    .ok_or("代理端口必须是 1 到 65535 之间的数字")?;
                let user = self.proxy_user.read(cx).value().trim().to_string();
                if user.is_empty() && !self.proxy_secret.read(cx).password(cx).is_empty() {
                    return Err("填写代理密码时请同时填写用户名");
                }
                let kind = self
                    .proxy_kind
                    .read(cx)
                    .selected_index(cx)
                    .and_then(|ix| ProxyKind::ALL.get(ix.row).copied())
                    .unwrap_or_default();
                Ok(Route::Proxy(
                    ProxySettings::new(kind, host, port).with_user(user),
                ))
            }
        }
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
        let address = self.address.read(cx).value().trim().to_string();
        let port = parse_port(self.port.read(cx).value().trim());
        match (address.is_empty(), port) {
            (true, _) => Err("请输入地址"),
            (_, None) => Err("端口必须是 1 到 65535 之间的数字"),
            (_, Some(port)) => Ok((address, port)),
        }
    }

    /// The login the form's current values describe, saved or not, or why
    /// there is nothing to test yet.
    fn login_test(&self, cx: &App) -> Result<LoginTest, &'static str> {
        let (host, port) = self.endpoint(cx)?;
        let route = self.committed_route(cx)?;
        let route_login = self.store.read(cx).route_login(&route);
        let request = match self.source.auth() {
            None => {
                let credential = self.selected_credential(cx).ok_or("请选择凭据")?;
                LoginTest::saved(
                    HostLogin::with_credential(host, port, credential).with_route(route_login),
                )
            }
            Some(auth) => {
                let user = self.user.read(cx).value().trim().to_string();
                if user.is_empty() {
                    return Err("请输入用户名");
                }
                let mut request = LoginTest::typed(
                    HostLogin::manual(host, port, user, auth).with_route(route_login),
                );
                // Only what the chosen way uses, which is also what the form
                // shows.
                let password = self.fields.read(cx).password(cx);
                if auth == AuthKind::Password && !password.is_empty() {
                    request = request.with_password(password);
                }
                request
            }
        };
        Ok(match route {
            Route::Proxy(_) => request.with_proxy_password(self.proxy_secret.read(cx).password(cx)),
            _ => request,
        })
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
        match self.source.auth() {
            Some(auth) => Ok(CommittedLogin::Own(auth)),
            None => self
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
            self.endpoint(cx).and_then(|endpoint| {
                Ok((
                    endpoint,
                    self.committed_login(cx)?,
                    self.committed_route(cx)?,
                ))
            })
        };
        let ((host, port), login, route) = match checked {
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
        // The password never rides along in the draft, which derives
        // `Debug`. It goes to the keychain separately, under the endpoint the
        // draft names; any other way of logging in leaves that entry alone.
        let mut password_change = None;
        let draft = match login {
            CommittedLogin::Saved { credential, user } => {
                HostDraft::new(name, host, port, user, AuthKind::default(), group)
                    .with_credential(credential)
            }
            CommittedLogin::Own(auth) => {
                let user = self.user.read(cx).value().trim().to_string();
                let user = if user.is_empty() {
                    DEFAULT_USER.to_string()
                } else {
                    user
                };
                let draft = HostDraft::new(name, host, port, user, auth, group);
                if auth == AuthKind::Password
                    && let Some(change) = self.fields.read(cx).password_change(cx)
                {
                    password_change = Some((draft.password_secret(), change));
                }
                draft
            }
        };
        // The proxy's password goes the same way, under the proxy's entry.
        let proxy_change = match &route {
            Route::Proxy(proxy) => proxy.password_secret().and_then(|secret| {
                self.proxy_secret
                    .read(cx)
                    .password_change(cx)
                    .map(|change| (secret, change))
            }),
            _ => None,
        };
        let notes = self.notes.read(cx).value().trim().to_string();
        let draft = draft.with_route(route).with_notes(notes);

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
            // host just left; this writes the one it moved to.
            for (secret, change) in password_change.into_iter().chain(proxy_change) {
                store.save_secret(secret, change, cx);
            }
        });
        // The dialog is about to close and take the form with it; drop the
        // plaintext now rather than waiting for the entity.
        for fields in [&self.fields, &self.proxy_secret] {
            fields.update(cx, |fields, cx| fields.clear(window, cx));
        }
        self.error = None;
        true
    }

    /// The fields of a host that logs in on its own: its user, and its
    /// password when it has one.
    fn own_fields(&self, form: Form, auth: AuthKind, cx: &App) -> Form {
        form.child(
            Field::new()
                .label("用户名")
                .col_span(4)
                .child(Input::new(&self.user).id("host-user").small()),
        )
        .when(auth == AuthKind::Password, |form| {
            form.child(
                Field::new()
                    .label("密码")
                    .col_span(4)
                    .child(self.fields.read(cx).password_input("host-password")),
            )
        })
    }

    /// The field that picks a saved credential, with what it logs in as.
    fn credential_field(&self, cx: &App) -> Field {
        let summary = match self.selected_credential(cx) {
            Some(credential) => Some(credential_summary(credential, self.key_dir.as_deref())),
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
                    .id("host-credential")
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
                        .id("host-credential-summary")
                        .test_support()
                        .aria_label(summary.clone())
                        .child(summary.clone())
                })
            })
    }
}

impl HostForm {
    /// 「连接方式」: the choice, and below it what the choice needs.
    fn route_field(&self, cx: &mut Context<Self>) -> Field {
        let route = self.route;
        let choice = SegmentedControl::new("host-route")
            .selected_index(RouteChoice::ALL.iter().position(|each| *each == route))
            .on_change(cx.listener(|this, ix: &usize, _, cx| {
                if let Some(route) = RouteChoice::ALL.get(*ix) {
                    this.set_route(*route, cx);
                }
            }))
            .segments(RouteChoice::ALL.map(|each| Segment::new(each.label())));
        let details = match route {
            RouteChoice::Direct => None,
            RouteChoice::Jump => Some(self.jump_box(cx).into_any_element()),
            RouteChoice::Proxy => Some(self.proxy_box(cx).into_any_element()),
        };
        Field::new().label("连接方式").col_span(4).child(
            v_flex()
                .w_full()
                .gap_2()
                .child(choice)
                .when_some(details, |field, details| {
                    field.child(
                        div()
                            .w_full()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(details),
                    )
                }),
        )
    }

    /// The jump hosts: the way through them, one row each, and a picker to
    /// add another.
    fn jump_box(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let name_of = |hop: &Option<HostId>| {
            hop.and_then(|id| self.jump_hosts.iter().find(|host| host.id == id))
        };

        // 本机 → 阿里云99 → 禅道 → 当前主机
        let mut stops: Vec<(SharedString, Tag)> = vec![(
            "本机".into(),
            Tag::secondary()
                .outline()
                .rounded_full()
                .small()
                .child("本机"),
        )];
        for hop in &self.hops {
            stops.push(match name_of(hop) {
                Some(host) => (
                    host.name.clone(),
                    Tag::primary()
                        .rounded_full()
                        .small()
                        .child(host.name.clone()),
                ),
                None => (
                    DELETED_HOST.into(),
                    Tag::danger().rounded_full().small().child(DELETED_HOST),
                ),
            });
        }
        stops.push((
            "当前主机".into(),
            Tag::secondary()
                .outline()
                .rounded_full()
                .small()
                .child("当前主机"),
        ));
        let chain_label = stops
            .iter()
            .map(|(name, _)| name.as_ref())
            .collect::<Vec<_>>()
            .join(" → ");
        let stop_count = stops.len();
        let chain = h_flex()
            .id("host-route-chain")
            .test_support()
            .aria_label(chain_label)
            .flex_wrap()
            .gap_1()
            .children(stops.into_iter().enumerate().flat_map(|(ix, (_, tag))| {
                let arrow = (ix + 1 < stop_count).then(|| {
                    Icon::new(IconName::ArrowRight)
                        .xsmall()
                        .text_color(muted)
                        .into_any_element()
                });
                std::iter::once(tag.into_any_element()).chain(arrow)
            }));

        let rows = self.hops.iter().enumerate().map(|(position, hop)| {
            let (id, name, address): (ElementId, SharedString, Option<SharedString>) =
                match name_of(hop) {
                    Some(host) => (
                        ("jump-hop", host.id.0).into(),
                        host.name.clone(),
                        Some(host.address.clone()),
                    ),
                    None => (
                        ("jump-hop-deleted", position).into(),
                        DELETED_HOST.into(),
                        None,
                    ),
                };
            h_flex()
                .id(id)
                .test_support()
                .aria_label(name.clone())
                .w_full()
                .gap_3()
                .pl_3()
                .pr_1()
                .py_1()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .text_sm()
                .child(
                    div()
                        .w_4()
                        .flex_none()
                        .text_color(muted)
                        .child((position + 1).to_string()),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .truncate()
                                .when(address.is_none(), |name| name.text_color(danger))
                                .child(name),
                        )
                        .when_some(address, |row, address| {
                            row.child(div().truncate().text_color(muted).child(address))
                        }),
                )
                .child(
                    Button::new(("remove-jump-hop", position))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("移除")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove_hop(position, window, cx)
                        })),
                )
        });

        let all_added = available_jump_hosts(&self.jump_hosts, &self.hops).is_empty();
        let empty_note = if self.jump_hosts.is_empty() {
            "还没有其他主机可做跳板"
        } else {
            "其他主机都已添加"
        };
        let picker = div().id("jump-add").w_full().child(
            Combobox::new(&self.jump_picker)
                .small()
                .disabled(all_added)
                .search_placeholder("搜索主机")
                .empty(move |_, cx| {
                    div()
                        .py_4()
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child("没有匹配的主机")
                })
                .render_trigger(move |_, _, _| {
                    h_flex()
                        .id("jump-add-trigger")
                        .test_support()
                        .w_full()
                        .justify_center()
                        .gap_1()
                        .child(Icon::new(IconName::Plus).xsmall())
                        .child(if all_added {
                            empty_note
                        } else {
                            "添加跳板主机"
                        })
                }),
        );

        v_flex()
            .w_full()
            .gap_2()
            .child(
                div()
                    .id("host-route-note")
                    .test_support()
                    .aria_label(JUMP_NOTE)
                    .text_sm()
                    .text_color(muted)
                    .child(JUMP_NOTE),
            )
            .child(chain)
            .children(rows)
            .child(picker)
    }

    /// The proxy's kind, address and login.
    fn proxy_box(&self, cx: &App) -> impl IntoElement {
        let password = self.proxy_secret.read(cx);
        v_flex()
            .w_full()
            .gap_2()
            .child(
                Form::new()
                    .columns(4)
                    .child(
                        Field::new()
                            .label("代理类型")
                            .col_span(4)
                            .child(Select::new(&self.proxy_kind).id("host-proxy-kind").small()),
                    )
                    .child(
                        Field::new()
                            .label("代理地址")
                            .required(true)
                            .col_span(3)
                            .child(Input::new(&self.proxy_host).id("host-proxy-host").small()),
                    )
                    .child(
                        Field::new()
                            .label("端口")
                            .required(true)
                            .child(Input::new(&self.proxy_port).id("host-proxy-port").small()),
                    )
                    .child(
                        Field::new()
                            .label("用户名")
                            .col_span(2)
                            .child(Input::new(&self.proxy_user).id("host-proxy-user").small()),
                    )
                    .child(
                        Field::new()
                            .label("密码")
                            .col_span(2)
                            .child(password.password_input("host-proxy-password")),
                    ),
            )
            .child(
                div()
                    .id("host-route-note")
                    .test_support()
                    .aria_label(PROXY_NOTE)
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(PROXY_NOTE),
            )
    }
}

/// What a jump host that has been deleted is called in the form.
const DELETED_HOST: &str = "已删除的主机";

/// The hosts that can still be added as a jump host: those not in `hops`.
fn available_jump_hosts(hosts: &[JumpHost], hops: &[Option<HostId>]) -> Vec<JumpHost> {
    hosts
        .iter()
        .filter(|host| !hops.contains(&Some(host.id)))
        .cloned()
        .collect()
}

impl Render for HostForm {
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
                    .child(Input::new(&self.name).id("host-name").small()),
            )
            .child(
                Field::new()
                    .label("地址")
                    .required(true)
                    .col_span(3)
                    .child(Input::new(&self.address).id("host-address").small()),
            )
            .child(
                Field::new()
                    .label("端口")
                    .child(Input::new(&self.port).id("host-port").small()),
            )
            .child(
                Field::new()
                    .label("认证方式")
                    .col_span(4)
                    .child(
                        SegmentedControl::new("host-auth-source")
                            .selected_index(AuthSource::ALL.iter().position(|each| *each == source))
                            .on_change(cx.listener(|this, ix: &usize, _, cx| {
                                if let Some(source) = AuthSource::ALL.get(*ix) {
                                    this.set_source(*source, cx);
                                }
                            }))
                            .segments(AuthSource::ALL.map(|each| Segment::new(each.label()))),
                    )
                    .when(source == AuthSource::NoPassword, |field| {
                        field.description_fn(|_, _| {
                            div()
                                .id("host-no-password-note")
                                .test_support()
                                .aria_label(NO_PASSWORD_NOTE)
                                .child(NO_PASSWORD_NOTE)
                        })
                    }),
            );
        let form = match source.auth() {
            Some(auth) => self.own_fields(form, auth, cx),
            None => form.child(self.credential_field(cx)),
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
                )
                .child(self.route_field(cx))
                .child(
                    Field::new().label("备注").col_span(4).child(
                        div()
                            .id("host-notes")
                            .test_support()
                            .w_full()
                            .child(Textarea::new(&self.notes).text_sm()),
                    ),
                ),
            )
            .when(source == AuthSource::Password, |form| {
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
    /// On its own.
    Own(AuthKind),
    Saved {
        credential: CredentialId,
        user: SharedString,
    },
}

/// A credential as the host form's select lists it: `运维（root · 密码）`.
fn credential_option(credential: &Credential, key_dir: Option<&Path>) -> SharedString {
    format!("{}（{}）", credential.name, credential.summary(key_dir)).into()
}

/// What a host using `credential` logs in as and with.
fn credential_summary(credential: &Credential, key_dir: Option<&Path>) -> SharedString {
    let with = match credential.kind {
        CredentialKind::Password => "使用凭据保存的密码".to_string(),
        CredentialKind::Key if credential.keeps_key_in(key_dir) => {
            "使用 ShellRS 保存的私钥".to_string()
        }
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

/// Put a first-seen host key to the user, above the host dialog. Closing
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
/// Open the new-host (`editing == None`) or edit-host dialog.
/// `preselect_group` fills in the group field of a new host, so creating
/// one from a group's context menu lands it in that group. `tester` backs the
/// dialog's 「测试连接」 button.
pub fn open_host_dialog(
    editing: Option<HostId>,
    preselect_group: Option<GroupId>,
    store: Entity<HostStore>,
    tester: SharedConnectionTester,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| HostForm::new(editing, preselect_group, store, tester, window, cx));
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
        move |dialog, window, cx| {
            dialog
                .title(title.clone())
                // Taller than most with a route's details open: it starts
                // near the top and its body scrolls rather than run off the
                // window. Dialog geometry is an API boundary that takes
                // `Pixels`.
                .margin_top(window.viewport_size().height * DIALOG_TOP)
                .max_h(window.viewport_size().height * DIALOG_MAX_HEIGHT)
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

/// What depends on hosts about to be deleted, beyond their own tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Dependents {
    /// Port forwards through them, which go with them.
    pub forwards: usize,
    /// Other hosts that use one of them as a jump host, and are left
    /// without a way through.
    pub jump_users: usize,
}

/// Ask before deleting a host. `on_delete` runs when the user confirms.
/// `affected` is whether tabs of the host are open and how many of them
/// are transferring.
pub fn confirm_delete_host(
    host: &Host,
    affected: (bool, usize),
    dependents: Dependents,
    on_delete: DeleteHandler,
    window: &mut Window,
    cx: &mut App,
) {
    let (closes_tabs, uploads) = affected;
    confirm_delete(
        &host.name,
        describe_host_delete(closes_tabs, uploads, dependents),
        on_delete,
        window,
        cx,
    );
}

/// What the delete dialog says goes with the host. `None` for a host
/// with nothing open and nothing depending on it.
fn describe_host_delete(
    closes_tabs: bool,
    uploads: usize,
    dependents: Dependents,
) -> Option<SharedString> {
    let mut description = String::new();
    if closes_tabs {
        description.push_str("会一并关闭该主机已打开的终端和 SFTP 标签。");
        if uploads > 0 {
            description.push_str(&format!("将停止 {uploads} 个传输批次并保留续传进度。"));
        }
    }
    let Dependents {
        forwards,
        jump_users,
    } = dependents;
    if forwards > 0 {
        description.push_str(&format!("将同时删除经由该主机的 {forwards} 条端口转发。"));
    }
    if jump_users > 0 {
        description.push_str(&format!(
            "有 {jump_users} 台主机把它用作跳板主机，删除后要重新选择跳板主机才能连接。"
        ));
    }
    (!description.is_empty()).then(|| description.into())
}

#[cfg(test)]
mod tests {
    use super::{Dependents, describe_host_delete};

    fn forwards(forwards: usize) -> Dependents {
        Dependents {
            forwards,
            ..Dependents::default()
        }
    }

    #[test]
    fn deleting_a_host_says_what_goes_with_it() {
        assert_eq!(describe_host_delete(false, 0, forwards(0)), None);
        assert_eq!(
            describe_host_delete(true, 0, forwards(0)).as_deref(),
            Some("会一并关闭该主机已打开的终端和 SFTP 标签。")
        );
        assert_eq!(
            describe_host_delete(true, 2, forwards(0)).as_deref(),
            Some("会一并关闭该主机已打开的终端和 SFTP 标签。将停止 2 个传输批次并保留续传进度。")
        );
        assert_eq!(
            describe_host_delete(false, 0, forwards(3)).as_deref(),
            Some("将同时删除经由该主机的 3 条端口转发。")
        );
        assert_eq!(
            describe_host_delete(true, 0, forwards(1)).as_deref(),
            Some("会一并关闭该主机已打开的终端和 SFTP 标签。将同时删除经由该主机的 1 条端口转发。")
        );
    }

    #[test]
    fn deleting_a_jump_host_says_who_loses_their_way_through() {
        let dependents = Dependents {
            forwards: 1,
            jump_users: 2,
        };
        assert_eq!(
            describe_host_delete(false, 0, dependents).as_deref(),
            Some(
                "将同时删除经由该主机的 1 条端口转发。有 2 台主机把它用作跳板主机，删除后要重新选择跳板主机才能连接。"
            )
        );
    }
}
