use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    combobox::{Combobox, ComboboxEvent, ComboboxState},
    dialog::{Cancel, Confirm, DialogButtonProps, DialogFooter},
    form::{Field, Form},
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    notification::Notification,
    searchable_list::{SearchableListItem, SearchableVec},
    select::{Select, SelectState},
    tag::Tag,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use zeroize::Zeroizing;

use crate::app::ConnectHost;
use crate::connection::{
    LoginTest, SharedConnectionTester, TelnetTest, TrustCallback, UnknownHostPrompt,
};
use crate::i18n::{t, tn};
use crate::secrets::SecretRef;

use super::secret_fields::SecretFields;
use super::{
    AuthKind, COMMON_BAUD_RATES, Credential, CredentialId, CredentialKind, DEFAULT_USER, DataBits,
    ErrorMessage as _, FlowControl, GroupId, Host, HostDraft, HostDraftError, HostId, HostLogin,
    HostLoginExt, HostStore, KindLabel as _, Parity, Protocol, ProtocolKind, ProxyKind,
    ProxySettings, Route, SSH_PORT, SerialLine, SerialSettings, StopBits, TELNET_PORT,
    TelnetSettings, credential_summary, group_options,
};
pub use crate::shared::DeleteHandler;
use crate::shared::{
    Segment, SegmentedControl, confirm_delete, dismiss_form_error, form_error_notification,
    parse_port,
};

/// Where the dialog's top sits and how tall it may grow, as fractions of
/// the window's height.
const DIALOG_TOP: f32 = 0.05;
const DIALOG_MAX_HEIGHT: f32 = 0.9;

/// Why an edit keeps the host an SSH host: the other protocols' segments
/// are disabled and say so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProtocolLock {
    /// These many hosts go through it as a jump host, which only an SSH
    /// host can be.
    JumpUsers(usize),
    /// These many port forwards go via it.
    Forwards(usize),
}

impl ProtocolLock {
    fn reason(self) -> SharedString {
        match self {
            ProtocolLock::JumpUsers(count) => tn!("host.dialog.protocol_jump_users", count),
            ProtocolLock::Forwards(count) => tn!("host.dialog.protocol_forwards", count),
        }
    }
}

/// Where a Telnet host's 自动登录 takes its user name and password, as the
/// form's 「认证方式」 offers it when 自动登录 is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TelnetSource {
    /// Typed into this form; either may be left empty.
    Manual,
    /// A saved password credential's.
    Credential,
}

impl TelnetSource {
    const ALL: [TelnetSource; 2] = [TelnetSource::Manual, TelnetSource::Credential];

    fn label(self) -> SharedString {
        match self {
            TelnetSource::Manual => t!("host.telnet_auth.manual"),
            TelnetSource::Credential => t!("host.auth.credential"),
        }
    }
}

/// The port a new host of `protocol` starts with, which follows the protocol
/// until it is changed; a serial host has none.
fn default_port(protocol: ProtocolKind) -> Option<u16> {
    match protocol {
        ProtocolKind::Ssh => Some(SSH_PORT),
        ProtocolKind::Telnet => Some(TELNET_PORT),
        ProtocolKind::Serial => None,
    }
}

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

    fn label(self) -> SharedString {
        match self {
            AuthSource::Password => t!("host.auth.password"),
            AuthSource::Credential => t!("host.auth.credential"),
            AuthSource::NoPassword => t!("host.auth.no_password"),
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

    fn label(self) -> SharedString {
        match self {
            RouteChoice::Direct => t!("host.route.direct"),
            RouteChoice::Jump => t!("host.route.jump"),
            RouteChoice::Proxy => t!("host.route.proxy"),
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
        t!(
            "host.dialog.jump_host_title",
            name = self.name,
            address = self.address
        )
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
    /// A 临时连接's form: no group, route or notes, a name it may leave out,
    /// and a commit that saves nothing.
    temporary: bool,
    name: Entity<InputState>,
    protocol: ProtocolKind,
    /// Why an edit cannot change the protocol, if anything holds it.
    protocol_lock: Option<ProtocolLock>,
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
    /// A Telnet host's 自动登录: whether it is on, where it takes the
    /// user name and password from, and the prompts it waits for.
    telnet_auto_login: bool,
    telnet_source: TelnetSource,
    telnet_user: Entity<InputState>,
    telnet_fields: Entity<SecretFields>,
    telnet_credential: Entity<SelectState<Vec<SharedString>>>,
    /// The password credentials, parallel to the Telnet credential select's
    /// rows, and the host's own credential whatever it is.
    telnet_credentials: Vec<Credential>,
    telnet_user_prompt: Entity<InputState>,
    telnet_password_prompt: Entity<InputState>,
    /// A serial host's device and line.
    device: Entity<InputState>,
    baud: Entity<InputState>,
    data_bits: Entity<SelectState<Vec<SharedString>>>,
    parity: Entity<SelectState<Vec<SharedString>>>,
    stop_bits: Entity<SelectState<Vec<SharedString>>>,
    flow: Entity<SelectState<Vec<SharedString>>>,
    /// The serial ports of this computer, for the device field's menu:
    /// `None` until a look comes back, or while it is looking again.
    ports: Option<Vec<String>>,
    _looking_for_ports: Task<()>,
    testing_connection: bool,
    /// Logs in with the form's current values for 「测试连接」.
    tester: SharedConnectionTester,
    /// The host being edited has tabs open: the note says they stay as
    /// they are.
    has_tabs: bool,
    _subscriptions: Vec<Subscription>,
}

impl HostForm {
    /// `tabs_open` says whether the host being edited has tabs open, which
    /// an edit leaves as they are.
    pub fn new(
        editing: Option<HostId>,
        tabs_open: bool,
        preselect_group: Option<GroupId>,
        store: Entity<HostStore>,
        tester: SharedConnectionTester,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let secrets = store.read(cx).secrets();
        let (draft, options, credentials, key_dir, jump_hosts) = {
            let read = store.read(cx);
            (
                editing.and_then(|id| read.host(id)).map(Host::draft),
                group_options(read.groups(), &[]),
                read.credentials().to_vec(),
                read.key_dir().map(Path::to_path_buf),
                read.hosts()
                    .iter()
                    .filter(|host| Some(host.id) != editing && host.is_ssh())
                    .map(JumpHost::of)
                    .collect::<Vec<_>>(),
            )
        };
        let protocol_lock = editing.and_then(|id| {
            let read = store.read(cx);
            if !read.host(id)?.is_ssh() {
                return None;
            }
            match read.ssh_dependents(id) {
                (0, 0) => None,
                (0, forwards) => Some(ProtocolLock::Forwards(forwards)),
                (jumps, _) => Some(ProtocolLock::JumpUsers(jumps)),
            }
        });
        // A host with no group sits at the root of the tree, which is where
        // every host starts when the database is still empty.
        let mut group_ids: Vec<Option<GroupId>> = vec![None];
        let mut group_names: Vec<SharedString> = vec![t!("host.dialog.no_group")];
        for (id, path) in options {
            group_ids.push(Some(id));
            group_names.push(path);
        }
        let draft = draft.unwrap_or_else(|| {
            HostDraft::new(
                "",
                "",
                SSH_PORT,
                DEFAULT_USER,
                AuthKind::default(),
                preselect_group,
            )
        });
        let protocol = draft.protocol.kind();
        // A serial host's address is its device, which is not an address
        // the SSH or Telnet fields would show.
        let (address_value, device_value) = match protocol {
            ProtocolKind::Serial => (SharedString::default(), draft.address.clone()),
            _ => (draft.address.clone(), SharedString::default()),
        };
        let line = draft.protocol.serial().unwrap_or_default();
        let telnet = draft.protocol.telnet().cloned();
        // An SSH login starts as a new host's unless the host is SSH: a
        // serial host keeps no login, and a Telnet host's is its 自动登录.
        let ssh_login = protocol.is_ssh().then_some(&draft);

        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(match protocol {
                    ProtocolKind::Serial => t!("host.dialog.name_placeholder_serial"),
                    _ => t!("host.dialog.name_placeholder"),
                })
                .default_value(draft.name.clone())
        });
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.dialog.address_placeholder"))
                .default_value(address_value)
        });
        let device = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!(
                    "host.dialog.serial_device_placeholder",
                    example = example_device()
                ))
                .default_value(device_value)
        });
        let baud = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(SerialSettings::default().baud.to_string())
                .default_value(line.baud.to_string())
        });
        let choice = |labels: Vec<SharedString>,
                      ix: Option<usize>,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
            cx.new(|cx| SelectState::new(labels, Some(IndexPath::new(ix.unwrap_or(0))), window, cx))
        };
        let data_bits = choice(
            DataBits::ALL
                .map(|bits| bits.value().to_string().into())
                .to_vec(),
            DataBits::ALL
                .iter()
                .position(|bits| *bits == line.data_bits),
            window,
            cx,
        );
        let parity = choice(
            Parity::ALL.map(|parity| parity.label()).to_vec(),
            Parity::ALL.iter().position(|parity| *parity == line.parity),
            window,
            cx,
        );
        let stop_bits = choice(
            StopBits::ALL
                .map(|bits| bits.value().to_string().into())
                .to_vec(),
            StopBits::ALL
                .iter()
                .position(|bits| *bits == line.stop_bits),
            window,
            cx,
        );
        let flow = choice(
            FlowControl::ALL.map(|flow| flow.label()).to_vec(),
            FlowControl::ALL.iter().position(|flow| *flow == line.flow),
            window,
            cx,
        );
        let port = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(default_port(protocol).unwrap_or(SSH_PORT).to_string())
                .default_value(draft.port.to_string())
        });
        let user = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(DEFAULT_USER)
                .default_value(ssh_login.map_or(DEFAULT_USER.into(), |draft| draft.user.clone()))
        });
        let fields = cx.new(|cx| SecretFields::new(secrets.clone(), None, window, cx));
        let credential_ix = ssh_login.and_then(|draft| draft.credential).and_then(|id| {
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
        // Telnet: a password credential's user name and password, or
        // whatever the host already uses.
        let telnet_credentials = credentials
            .iter()
            .filter(|credential| {
                credential.kind == CredentialKind::Password
                    || (telnet.is_some() && draft.credential == Some(credential.id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let telnet_credential_ix = telnet.as_ref().and(draft.credential).and_then(|id| {
            telnet_credentials
                .iter()
                .position(|credential| credential.id == id)
        });
        let telnet_credential = cx.new(|cx| {
            SelectState::new(
                telnet_credentials
                    .iter()
                    .map(|credential| credential_option(credential, key_dir.as_deref()))
                    .collect::<Vec<_>>(),
                telnet_credential_ix.map(IndexPath::new),
                window,
                cx,
            )
            .searchable(true)
        });
        let telnet_user = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.dialog.optional"))
                .default_value(match (&telnet, draft.credential) {
                    (Some(_), None) => draft.user.clone(),
                    _ => SharedString::default(),
                })
        });
        let telnet_fields = cx.new(|cx| {
            let mut fields = SecretFields::new(secrets.clone(), None, window, cx);
            fields.set_password_placeholder(
                t!("host.dialog.telnet_password_placeholder"),
                window,
                cx,
            );
            fields
        });
        let prompt = |placeholder: &'static str,
                      value: SharedString,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let telnet_user_prompt = prompt(
            "login: / Username:",
            telnet
                .as_ref()
                .map(|telnet| telnet.user_prompt.clone())
                .unwrap_or_default(),
            window,
            cx,
        );
        let telnet_password_prompt = prompt(
            "Password:",
            telnet
                .as_ref()
                .map(|telnet| telnet.password_prompt.clone())
                .unwrap_or_default(),
            window,
            cx,
        );

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
                    .map(|kind| proxy_kind_label(*kind))
                    .collect::<Vec<_>>(),
                Some(IndexPath::new(proxy_kind_ix)),
                window,
                cx,
            )
        });
        let proxy_host = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.dialog.proxy_host_placeholder"))
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
                .placeholder(t!("host.dialog.optional"))
                .default_value(
                    proxy
                        .as_ref()
                        .and_then(|proxy| proxy.user.clone())
                        .unwrap_or_default(),
                )
        });
        let proxy_secret = cx.new(|cx| {
            let mut fields = SecretFields::new(secrets, None, window, cx);
            fields.set_password_placeholder(t!("host.dialog.optional"), window, cx);
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
            cx.observe(&telnet_credential, |_, _, cx| cx.notify()),
            cx.observe(&telnet_fields, |_, _, cx| cx.notify()),
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

        // The host's own password, even for a host that does not use one
        // now: it is what switching back would show.
        let own_password = match protocol {
            ProtocolKind::Ssh => Some(&fields),
            ProtocolKind::Telnet => Some(&telnet_fields),
            ProtocolKind::Serial => None,
        };
        if editing.is_some()
            && let Some(own_password) = own_password
        {
            own_password.update(cx, |fields, cx| {
                fields.load_saved(Some(draft.password_secret()), None, cx)
            });
        }
        let mut form = Self {
            store,
            editing,
            temporary: false,
            name,
            protocol,
            protocol_lock,
            address,
            port,
            source: match (
                credential_ix,
                ssh_login.map_or(AuthKind::default(), |draft| draft.auth),
            ) {
                (Some(_), _) => AuthSource::Credential,
                (None, AuthKind::Password) => AuthSource::Password,
                (None, AuthKind::NoPassword) => AuthSource::NoPassword,
            },
            user,
            fields,
            credential,
            credentials,
            key_dir,
            telnet_auto_login: telnet.is_some()
                && (draft.credential.is_some() || draft.auth == AuthKind::Password),
            telnet_source: if telnet_credential_ix.is_some() {
                TelnetSource::Credential
            } else {
                TelnetSource::Manual
            },
            telnet_user,
            telnet_fields,
            telnet_credential,
            telnet_credentials,
            telnet_user_prompt,
            telnet_password_prompt,
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
            device,
            baud,
            data_bits,
            parity,
            stop_bits,
            flow,
            ports: None,
            _looking_for_ports: Task::ready(()),
            testing_connection: false,
            tester,
            has_tabs: editing.is_some() && tabs_open,
            _subscriptions: subscriptions,
        };
        if protocol == ProtocolKind::Serial {
            form.look_for_ports(window, cx);
        }
        form
    }

    /// The form of a 临时连接: the login of a new host, without what only a
    /// saved host has (its group, its route, its notes).
    pub fn temporary(
        store: Entity<HostStore>,
        tester: SharedConnectionTester,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut form = Self::new(None, false, None, store, tester, window, cx);
        form.temporary = true;
        form.name.update(cx, |input, cx| {
            input.set_placeholder(t!("host.dialog.temporary_name_placeholder"), window, cx)
        });
        form
    }

    /// Make the form one for an SSH, a Telnet or a serial host. An edit's
    /// other protocols take what they share (the name, the address, the
    /// group, the route, the notes) and their own defaults for the rest.
    fn set_protocol(
        &mut self,
        protocol: ProtocolKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.protocol == protocol || self.protocol_lock.is_some() {
            return;
        }
        self.protocol = protocol;
        let placeholder = match (protocol, self.temporary) {
            (ProtocolKind::Serial, _) => t!("host.dialog.name_placeholder_serial"),
            (_, true) => t!("host.dialog.temporary_name_placeholder"),
            (_, false) => t!("host.dialog.name_placeholder"),
        };
        self.name.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        // The port follows the protocol while it is still one's default.
        if let Some(port) = default_port(protocol) {
            let typed = self.port.read(cx).value().trim().to_string();
            let untouched = [SSH_PORT, TELNET_PORT]
                .iter()
                .any(|default| default.to_string() == typed);
            self.port.update(cx, |input, cx| {
                input.set_placeholder(port.to_string(), window, cx);
                if untouched {
                    input.set_value(port.to_string(), window, cx);
                }
            });
        }
        if protocol == ProtocolKind::Serial && self.ports.is_none() {
            self.look_for_ports(window, cx);
        }
        cx.notify();
    }

    /// Find the serial ports of this computer again, off the UI thread.
    fn look_for_ports(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ports = None;
        self._looking_for_ports = cx.spawn_in(window, async move |this, cx| {
            let ports = cx
                .background_spawn(async { crate::serial::available_ports() })
                .await;
            this.update(cx, |this, cx| {
                this.ports = Some(ports);
                cx.notify();
            })
            .ok();
        });
        cx.notify();
    }

    fn set_device(&mut self, device: String, window: &mut Window, cx: &mut Context<Self>) {
        self.device
            .update(cx, |input, cx| input.set_value(device, window, cx));
    }

    fn set_baud(&mut self, baud: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.baud.update(cx, |input, cx| {
            input.set_value(baud.to_string(), window, cx)
        });
    }

    /// The device and line the form describes, or what is wrong with them.
    fn committed_serial(&self, cx: &App) -> Result<(String, SerialSettings), SharedString> {
        let device = self.device.read(cx).value().trim().to_string();
        if device.is_empty() {
            return Err(HostDraftError::Device.message());
        }
        let baud = self
            .baud
            .read(cx)
            .value()
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|baud| *baud > 0)
            .ok_or_else(|| HostDraftError::Baud.message())?;
        let row = |select: &Entity<SelectState<Vec<SharedString>>>| {
            select.read(cx).selected_index(cx).map(|ix| ix.row)
        };
        let default = SerialSettings::default();
        Ok((
            device,
            SerialSettings {
                baud,
                data_bits: row(&self.data_bits)
                    .and_then(|ix| DataBits::ALL.get(ix).copied())
                    .unwrap_or(default.data_bits),
                parity: row(&self.parity)
                    .and_then(|ix| Parity::ALL.get(ix).copied())
                    .unwrap_or(default.parity),
                stop_bits: row(&self.stop_bits)
                    .and_then(|ix| StopBits::ALL.get(ix).copied())
                    .unwrap_or(default.stop_bits),
                flow: row(&self.flow)
                    .and_then(|ix| FlowControl::ALL.get(ix).copied())
                    .unwrap_or(default.flow),
            },
        ))
    }

    /// The draft of a serial host, or what is wrong with the form.
    fn serial_draft(&self, group: Option<GroupId>, cx: &App) -> Result<HostDraft, SharedString> {
        let (device, settings) = self.committed_serial(cx)?;
        let name = self.name.read(cx).value().trim().to_string();
        let name = if name.is_empty() {
            device.clone()
        } else {
            name
        };
        let notes = self.notes.read(cx).value().trim().to_string();
        Ok(HostDraft::serial(name, device, settings, group).with_notes(notes))
    }

    /// The group the form has selected; `None` is the root of the tree.
    fn selected_group(&self, cx: &App) -> Option<GroupId> {
        self.group
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| self.group_ids.get(ix.row).copied())
            .unwrap_or(None)
    }

    fn set_source(&mut self, source: AuthSource, cx: &mut Context<Self>) {
        if self.source != source {
            self.source = source;
            cx.notify();
        }
    }

    fn set_route(&mut self, route: RouteChoice, cx: &mut Context<Self>) {
        if self.route != route {
            self.route = route;
            cx.notify();
        }
    }

    /// Put `id` at the end of the jump hosts, and take it off the hosts that
    /// can still be added.
    fn add_hop(&mut self, id: HostId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.hops.contains(&Some(id)) {
            self.hops.push(Some(id));
        }
        self.refresh_jump_picker(window, cx);
        cx.notify();
    }

    fn remove_hop(&mut self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        if position < self.hops.len() {
            self.hops.remove(position);
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
    fn committed_route(&self, cx: &App) -> Result<Route, SharedString> {
        match self.route {
            RouteChoice::Direct => Ok(Route::Direct),
            RouteChoice::Jump if self.hops.is_empty() => Err(HostDraftError::NoJumpHosts.message()),
            RouteChoice::Jump if self.hops.contains(&None) => {
                Err(HostDraftError::DeletedJumpHost.message())
            }
            RouteChoice::Jump => Ok(Route::Jump(self.hops.clone())),
            RouteChoice::Proxy => {
                let host = self.proxy_host.read(cx).value().trim().to_string();
                if host.is_empty() {
                    return Err(HostDraftError::ProxyAddress.message());
                }
                let port = parse_port(self.proxy_port.read(cx).value().trim())
                    .ok_or(HostDraftError::ProxyPort.message())?;
                let user = self.proxy_user.read(cx).value().trim().to_string();
                if user.is_empty() && !self.proxy_secret.read(cx).password(cx).is_empty() {
                    return Err(t!("host.dialog.proxy_password_needs_user"));
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
    fn endpoint(&self, cx: &App) -> Result<(String, u16), SharedString> {
        let address = self.address.read(cx).value().trim().to_string();
        let port = parse_port(self.port.read(cx).value().trim());
        match (address.is_empty(), port) {
            (true, _) => Err(HostDraftError::Address.message()),
            (_, None) => Err(HostDraftError::Port.message()),
            (_, Some(port)) => Ok((address, port)),
        }
    }

    /// The login the form's current values describe, saved or not, or why
    /// there is nothing to test yet.
    fn login_test(&self, cx: &App) -> Result<LoginTest, SharedString> {
        let (host, port) = self.endpoint(cx)?;
        let route = self.committed_route(cx)?;
        let route_login = self.store.read(cx).route_login(&route);
        let request = match self.source.auth() {
            None => {
                let credential = self
                    .selected_credential(cx)
                    .ok_or_else(|| t!("host.dialog.choose_credential"))?;
                LoginTest::saved(
                    HostLogin::with_credential(host, port, credential).with_route(route_login),
                )
            }
            Some(auth) => {
                let user = self.user.read(cx).value().trim().to_string();
                if user.is_empty() {
                    return Err(t!("host.dialog.enter_user"));
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

    /// The way to the Telnet host the form describes, or why there is
    /// nothing to test yet.
    fn telnet_test(&self, cx: &App) -> Result<TelnetTest, SharedString> {
        let (host, port) = self.endpoint(cx)?;
        let route = self.committed_route(cx)?;
        let request = TelnetTest::new(host, port, self.store.read(cx).route_login(&route));
        Ok(match route {
            Route::Proxy(_) => request.with_proxy_password(self.proxy_secret.read(cx).password(cx)),
            _ => request,
        })
    }

    /// The Telnet credential the form has selected.
    fn selected_telnet_credential(&self, cx: &App) -> Option<&Credential> {
        self.telnet_credential
            .read(cx)
            .selected_index(cx)
            .and_then(|ix| self.telnet_credentials.get(ix.row))
    }

    /// Whether the form types a Telnet password of its own into the device.
    fn telnet_password_typed(&self) -> bool {
        self.telnet_auto_login && self.telnet_source == TelnetSource::Manual
    }

    /// The draft of a Telnet host in `group`, without its route and notes,
    /// or what is wrong with the form. The password is not in it: it goes
    /// to the keychain apart.
    fn telnet_draft(&self, group: Option<GroupId>, cx: &App) -> Result<HostDraft, SharedString> {
        let (host, port) = self.endpoint(cx)?;
        let name = self.name.read(cx).value().trim().to_string();
        let name = if name.is_empty() { host.clone() } else { name };
        let mut draft = HostDraft::telnet(name, host, port, group);
        draft.protocol = Protocol::Telnet(TelnetSettings {
            user_prompt: self
                .telnet_user_prompt
                .read(cx)
                .value()
                .trim()
                .to_string()
                .into(),
            password_prompt: self
                .telnet_password_prompt
                .read(cx)
                .value()
                .trim()
                .to_string()
                .into(),
        });
        if self.telnet_auto_login {
            match self.telnet_source {
                TelnetSource::Manual => {
                    draft.auth = AuthKind::Password;
                    draft.user = self.telnet_user.read(cx).value().trim().to_string().into();
                }
                TelnetSource::Credential => {
                    let credential = self
                        .selected_telnet_credential(cx)
                        .ok_or_else(|| t!("host.dialog.choose_credential"))?;
                    draft.auth = AuthKind::default();
                    draft.user = credential.user.clone();
                    draft = draft.with_credential(credential.id);
                }
            }
        }
        Ok(draft)
    }

    /// The proxy password typed for `route`, to save under the proxy's
    /// entry: a change of it, or `None` to leave the entry as it is.
    fn proxy_change(&self, route: &Route, cx: &App) -> Option<(SecretRef, Option<String>)> {
        match route {
            Route::Proxy(proxy) => proxy.password_secret().and_then(|secret| {
                self.proxy_secret
                    .read(cx)
                    .password_change(cx)
                    .map(|change| (secret, change))
            }),
            _ => None,
        }
    }

    /// Log in with the form's current values without saving them, and report
    /// the outcome as a notification. The login runs on a thread of its own;
    /// a host key seen for the first time is put to the user in a dialog
    /// above this one.
    fn test_connection(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.testing_connection {
            return;
        }
        let tester = self.tester.clone();
        let (test, success): (TestJob, _) = match self.protocol {
            ProtocolKind::Serial => {
                self.test_serial(window, cx);
                return;
            }
            ProtocolKind::Ssh => match self.login_test(cx) {
                Ok(request) => (
                    Box::new(move |trust| tester.test(request, trust)),
                    t!("host.test.connected"),
                ),
                Err(reason) => {
                    window.push_notification(connection_test_notification(Err(reason.into())), cx);
                    return;
                }
            },
            ProtocolKind::Telnet => match self.telnet_test(cx) {
                Ok(request) => {
                    let reached = t!(
                        "host.test.telnet_reached",
                        address = request.host(),
                        port = request.port()
                    );
                    (
                        Box::new(move |trust| tester.test_telnet(request, trust)),
                        reached,
                    )
                }
                Err(reason) => {
                    window.push_notification(connection_test_notification(Err(reason.into())), cx);
                    return;
                }
            },
        };
        self.run_test(test, success, window, cx);
    }

    /// Run `test` on a thread of its own, put a host key seen for the first
    /// time to the user in a dialog above this one, and report the outcome
    /// as a notification, `success` when it went through.
    fn run_test(
        &mut self,
        test: TestJob,
        success: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (trust_tx, trust_rx) = std::sync::mpsc::channel::<TrustQuestion>();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("shellrs-connection-test".into())
            .spawn(move || {
                // Nobody left to answer (the form closed) reads as "no".
                let trust: TrustCallback = Box::new(move |prompt| {
                    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                    trust_tx.send((prompt, reply_tx)).is_ok() && reply_rx.recv().unwrap_or(false)
                });
                let _ = result_tx.send(test(trust));
            });
        if let Err(error) = spawned {
            window.push_notification(
                connection_test_notification(Err(t!(
                    "host.dialog.test_start_failed",
                    error = error
                )
                .into())),
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
                        Err(t!("host.dialog.test_aborted").to_string())
                    }
                };
                this.update_in(cx, |this, window, cx| {
                    this.testing_connection = false;
                    let notification = match result {
                        Ok(()) => Notification::success(success.clone()),
                        Err(reason) => connection_test_notification(Err(reason)),
                    };
                    window.push_notification(notification, cx);
                    cx.notify();
                })
                .ok();
                return;
            }
        })
        .detach();
    }

    /// 「测试连接」 for a serial host: open the port with the form's line and
    /// close it again, off the UI thread, and say whether it opened.
    fn test_serial(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = match self.committed_serial(cx) {
            Ok((device, settings)) => SerialLine { device, settings },
            Err(reason) => {
                window.push_notification(connection_test_notification(Err(reason.into())), cx);
                return;
            }
        };
        self.testing_connection = true;
        cx.notify();
        let tester = self.tester.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn({
                    let line = line.clone();
                    async move { tester.test_serial(&line) }
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.testing_connection = false;
                let notification = match result {
                    Ok(()) => {
                        Notification::success(t!("host.test.serial_opened", device = line.device))
                    }
                    Err(reason) => connection_test_notification(Err(reason)),
                };
                window.push_notification(notification, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// How the form says the host logs in, or what is missing.
    fn committed_login(&self, cx: &App) -> Result<CommittedLogin, SharedString> {
        match self.source.auth() {
            Some(auth) => Ok(CommittedLogin::Own(auth)),
            None => self
                .selected_credential(cx)
                .map(|credential| CommittedLogin::Saved {
                    credential: credential.id,
                    user: credential.user.clone(),
                })
                .ok_or_else(|| t!("host.dialog.choose_credential")),
        }
    }

    /// Validate and write to the store. Returns whether the dialog may close.
    pub fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.protocol == ProtocolKind::Telnet {
            return self.commit_telnet(window, cx);
        }
        if self.protocol == ProtocolKind::Serial {
            let draft = match self.serial_draft(self.selected_group(cx), cx) {
                Ok(draft) => draft,
                Err(error) => {
                    window.push_notification(form_error_notification(error), cx);
                    return false;
                }
            };
            let editing = self.editing;
            self.store.update(cx, |store, cx| match editing {
                Some(id) => {
                    store.update(id, draft, cx);
                }
                None => {
                    store.insert(draft, cx);
                }
            });
            self.clear_secrets(window, cx);
            return true;
        }
        let name = self.name.read(cx).value().trim().to_string();
        let checked = self.endpoint(cx).and_then(|endpoint| {
            Ok((
                endpoint,
                self.committed_login(cx)?,
                self.committed_route(cx)?,
            ))
        });
        let ((host, port), login, route) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                window.push_notification(form_error_notification(error), cx);
                return false;
            }
        };

        let name = if name.is_empty() { host.clone() } else { name };

        let group = self.selected_group(cx);
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
        let proxy_change = self.proxy_change(&route, cx);
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
        self.clear_secrets(window, cx);
        true
    }

    /// [`Self::commit`] for a Telnet host: as an SSH host's, with the
    /// password of its 自动登录, when typed here, going to the keychain.
    fn commit_telnet(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let checked = self
            .telnet_draft(self.selected_group(cx), cx)
            .and_then(|draft| Ok((draft, self.committed_route(cx)?)));
        let (draft, route) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                window.push_notification(form_error_notification(error), cx);
                return false;
            }
        };
        let proxy_change = self.proxy_change(&route, cx);
        let notes = self.notes.read(cx).value().trim().to_string();
        let draft = draft.with_route(route).with_notes(notes);
        let password_change = self
            .telnet_password_typed()
            .then(|| self.telnet_fields.read(cx).password_change(cx))
            .flatten()
            .map(|change| (draft.password_secret(), change));
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
            for (secret, change) in password_change.into_iter().chain(proxy_change) {
                store.save_secret(secret, change, cx);
            }
        });
        self.clear_secrets(window, cx);
        true
    }

    /// The dialog is about to close and take the form with it; drop the
    /// plaintext now rather than waiting for the entity.
    fn clear_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for fields in [&self.fields, &self.telnet_fields, &self.proxy_secret] {
            fields.update(cx, |fields, cx| fields.clear(window, cx));
        }
    }

    /// Validate and connect without saving: the host goes into the store's
    /// memory, its password into the store's memory too, never the
    /// keychain. The host to connect to, or `None` when the form said what
    /// is wrong.
    pub fn commit_temporary(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<HostId> {
        if self.protocol == ProtocolKind::Telnet {
            let draft = match self.telnet_draft(None, cx) {
                Ok(draft) => draft,
                Err(error) => {
                    window.push_notification(form_error_notification(error), cx);
                    return None;
                }
            };
            let password = self
                .telnet_password_typed()
                .then(|| Zeroizing::new(self.telnet_fields.read(cx).password(cx)));
            self.clear_secrets(window, cx);
            return Some(
                self.store
                    .update(cx, |store, cx| store.insert_temporary(draft, password, cx)),
            );
        }
        if self.protocol == ProtocolKind::Serial {
            let draft = match self.serial_draft(None, cx) {
                Ok(draft) => draft,
                Err(error) => {
                    window.push_notification(form_error_notification(error), cx);
                    return None;
                }
            };
            self.clear_secrets(window, cx);
            return Some(
                self.store
                    .update(cx, |store, cx| store.insert_temporary(draft, None, cx)),
            );
        }
        let checked = self
            .endpoint(cx)
            .and_then(|endpoint| Ok((endpoint, self.committed_login(cx)?)));
        let ((host, port), login) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                window.push_notification(form_error_notification(error), cx);
                return None;
            }
        };
        let name = self.name.read(cx).value().trim().to_string();
        let name = if name.is_empty() { host.clone() } else { name };
        let mut password = None;
        let draft = match login {
            CommittedLogin::Saved { credential, user } => {
                HostDraft::new(name, host, port, user, AuthKind::default(), None)
                    .with_credential(credential)
            }
            CommittedLogin::Own(auth) => {
                let user = self.user.read(cx).value().trim().to_string();
                let user = if user.is_empty() {
                    DEFAULT_USER.to_string()
                } else {
                    user
                };
                if auth == AuthKind::Password {
                    password = Some(Zeroizing::new(self.fields.read(cx).password(cx)));
                }
                HostDraft::new(name, host, port, user, auth, None)
            }
        };
        let id = self
            .store
            .update(cx, |store, cx| store.insert_temporary(draft, password, cx));
        self.fields
            .update(cx, |fields, cx| fields.clear(window, cx));
        Some(id)
    }

    /// The fields of a host that logs in on its own: its user, and its
    /// password when it has one.
    fn own_fields(&self, form: Form, auth: AuthKind, cx: &App) -> Form {
        form.child(
            Field::new()
                .label(t!("host.dialog.user"))
                .col_span(4)
                .child(Input::new(&self.user).id("host-user").small()),
        )
        .when(auth == AuthKind::Password, |form| {
            form.child(
                Field::new()
                    .label(t!("host.dialog.password"))
                    .col_span(4)
                    .child(self.fields.read(cx).password_input("host-password")),
            )
        })
    }

    /// The field that picks a saved credential, with what it logs in as.
    fn credential_field(&self, cx: &App) -> Field {
        self.credential_choice(
            CredentialChoice {
                state: &self.credential,
                selected: self.selected_credential(cx),
                none: self.credentials.is_empty(),
                id: "host-credential",
                summary_id: "host-credential-summary",
                empty: t!("host.dialog.no_credentials"),
                hint: t!("host.dialog.no_credentials_hint"),
            },
            cx,
        )
    }

    /// A Telnet host's credential, among the password credentials.
    fn telnet_credential_field(&self, cx: &App) -> Field {
        self.credential_choice(
            CredentialChoice {
                state: &self.telnet_credential,
                selected: self.selected_telnet_credential(cx),
                none: self.telnet_credentials.is_empty(),
                id: "host-telnet-credential",
                summary_id: "host-telnet-credential-summary",
                empty: t!("host.dialog.no_password_credentials"),
                hint: t!("host.dialog.no_password_credentials_hint"),
            },
            cx,
        )
    }

    fn credential_choice(&self, choice: CredentialChoice<'_>, _: &App) -> Field {
        let summary = match choice.selected {
            Some(credential) => Some(logs_in_with(credential, self.key_dir.as_deref())),
            None if choice.none => Some(choice.hint),
            None => None,
        };
        let empty = choice.empty;
        let summary_id = choice.summary_id;
        Field::new()
            .label(t!("host.dialog.credential"))
            .required(true)
            .col_span(4)
            .child(
                Select::new(choice.state)
                    .id(choice.id)
                    .placeholder(t!("host.dialog.choose_credential"))
                    .search_placeholder(t!("host.dialog.search_credentials"))
                    .empty(move |_, cx| {
                        div()
                            .py_4()
                            .text_sm()
                            .text_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty.clone())
                    })
                    .small(),
            )
            .when_some(summary, |field, summary: SharedString| {
                field.description_fn(move |_, _| {
                    div()
                        .id(summary_id)
                        .test_support()
                        .aria_label(summary.clone())
                        .child(summary.clone())
                })
            })
    }
}

impl HostForm {
    /// 「协议」: an edit's too, unless something holds it.
    fn protocol_field(&self, cx: &mut Context<Self>) -> Field {
        let protocol = self.protocol;
        let lock = self.protocol_lock;
        Field::new()
            .label(t!("host.dialog.protocol"))
            .col_span(4)
            .child(
                SegmentedControl::new("host-protocol")
                    .selected_index(ProtocolKind::ALL.iter().position(|each| *each == protocol))
                    .on_change(cx.listener(|this, ix: &usize, window, cx| {
                        if let Some(protocol) = ProtocolKind::ALL.get(*ix) {
                            this.set_protocol(*protocol, window, cx);
                        }
                    }))
                    .segments(ProtocolKind::ALL.map(|each| {
                        Segment::new(each.label())
                            .when_some(lock.filter(|_| each != protocol), |segment, lock| {
                                segment.disabled(true).tooltip(lock.reason())
                            })
                    })),
            )
    }

    /// A serial host's fields: the device and its rate on one row, how
    /// each character is framed on the next, four across.
    fn serial_fields(&self, form: Form, cx: &mut Context<Self>) -> Form {
        let this = cx.entity().downgrade();
        let ports = self.ports.clone();
        let device = self.device.read(cx).value().trim().to_string();
        let ports_menu = Button::new("host-serial-ports")
            .ghost()
            .xsmall()
            .icon(IconName::ChevronDown)
            .tooltip(t!("host.dialog.serial_ports"))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                let menu = match &ports {
                    None => menu.item(
                        PopupMenuItem::new(t!("host.dialog.looking_for_ports")).disabled(true),
                    ),
                    Some(found) if found.is_empty() => {
                        menu.item(PopupMenuItem::new(t!("host.dialog.no_ports")).disabled(true))
                    }
                    Some(found) => found.iter().fold(menu, |menu, port| {
                        let (this, chosen) = (this.clone(), port.clone());
                        menu.item(
                            PopupMenuItem::new(port.clone())
                                .checked(*port == device)
                                .on_click(move |_, window, cx| {
                                    this.update(cx, |form, cx| {
                                        form.set_device(chosen.clone(), window, cx)
                                    })
                                    .ok();
                                }),
                        )
                    }),
                };
                let this = this.clone();
                menu.separator().item(
                    PopupMenuItem::new(t!("host.dialog.look_for_ports_again")).on_click(
                        move |_, window, cx| {
                            this.update(cx, |form, cx| form.look_for_ports(window, cx))
                                .ok();
                        },
                    ),
                )
            });
        let this = cx.entity().downgrade();
        let baud = self.baud.read(cx).value().trim().parse::<u32>().ok();
        let rates_menu = Button::new("host-serial-rates")
            .ghost()
            .xsmall()
            .icon(IconName::ChevronDown)
            .tooltip(t!("host.dialog.baud_rates"))
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                COMMON_BAUD_RATES.iter().fold(menu, |menu, rate| {
                    let (this, rate) = (this.clone(), *rate);
                    menu.item(
                        PopupMenuItem::new(rate.to_string())
                            .checked(baud == Some(rate))
                            .on_click(move |_, window, cx| {
                                this.update(cx, |form, cx| form.set_baud(rate, window, cx))
                                    .ok();
                            }),
                    )
                })
            });
        // The device and its rate in a row of three: a quarter of the form
        // leaves no room for 115200 beside the rate's menu.
        let line = Form::new()
            .columns(3)
            .child(
                Field::new()
                    .label(t!("host.dialog.serial_device"))
                    .required(true)
                    .col_span(2)
                    .child(
                        Input::new(&self.device)
                            .id("host-serial-device")
                            .small()
                            .suffix(ports_menu),
                    ),
            )
            .child(
                Field::new()
                    .label(t!("host.dialog.baud_rate"))
                    .required(true)
                    .child(
                        Input::new(&self.baud)
                            .id("host-serial-baud")
                            .small()
                            .suffix(rates_menu),
                    ),
            );
        form.child(Field::new().label_indent(false).col_span(4).child(line))
            .child(
                Field::new().label(t!("host.dialog.data_bits")).child(
                    Select::new(&self.data_bits)
                        .id("host-serial-data-bits")
                        .small(),
                ),
            )
            .child(
                Field::new()
                    .label(t!("host.dialog.parity"))
                    .child(Select::new(&self.parity).id("host-serial-parity").small()),
            )
            .child(
                Field::new().label(t!("host.dialog.stop_bits")).child(
                    Select::new(&self.stop_bits)
                        .id("host-serial-stop-bits")
                        .small(),
                ),
            )
            .child(
                Field::new()
                    .label(t!("host.dialog.flow_control"))
                    .child(Select::new(&self.flow).id("host-serial-flow").small()),
            )
    }

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
        Field::new()
            .label(t!("host.dialog.route"))
            .col_span(4)
            .child(
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
        let this_computer = t!("host.dialog.this_computer");
        let mut stops: Vec<(SharedString, Tag)> = vec![(
            this_computer.clone(),
            Tag::secondary()
                .outline()
                .rounded_full()
                .small()
                .child(this_computer),
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
                    t!("host.dialog.deleted_host"),
                    Tag::danger()
                        .rounded_full()
                        .small()
                        .child(t!("host.dialog.deleted_host")),
                ),
            });
        }
        let this_host = t!("host.dialog.this_host");
        stops.push((
            this_host.clone(),
            Tag::secondary()
                .outline()
                .rounded_full()
                .small()
                .child(this_host),
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
                        t!("host.dialog.deleted_host"),
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
                        .tooltip(t!("host.dialog.remove_hop"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove_hop(position, window, cx)
                        })),
                )
        });

        let all_added = available_jump_hosts(&self.jump_hosts, &self.hops).is_empty();
        let empty_note = if self.jump_hosts.is_empty() {
            t!("host.dialog.no_other_hosts")
        } else {
            t!("host.dialog.all_hosts_added")
        };
        let picker = div().id("jump-add").w_full().child(
            Combobox::new(&self.jump_picker)
                .small()
                .disabled(all_added)
                .search_placeholder(t!("host.dialog.search_hosts"))
                .empty(move |_, cx| {
                    div()
                        .py_4()
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("host.dialog.no_matching_hosts"))
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
                            empty_note.clone()
                        } else {
                            t!("host.dialog.add_jump_host")
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
                    .aria_label(t!("host.dialog.jump_note"))
                    .text_sm()
                    .text_color(muted)
                    .child(t!("host.dialog.jump_note")),
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
                            .label(t!("host.dialog.proxy_kind"))
                            .col_span(4)
                            .child(Select::new(&self.proxy_kind).id("host-proxy-kind").small()),
                    )
                    .child(
                        Field::new()
                            .label(t!("host.dialog.proxy_address"))
                            .required(true)
                            .col_span(3)
                            .child(Input::new(&self.proxy_host).id("host-proxy-host").small()),
                    )
                    .child(
                        Field::new()
                            .label(t!("host.dialog.port"))
                            .required(true)
                            .child(Input::new(&self.proxy_port).id("host-proxy-port").small()),
                    )
                    .child(
                        Field::new()
                            .label(t!("host.dialog.user"))
                            .col_span(2)
                            .child(Input::new(&self.proxy_user).id("host-proxy-user").small()),
                    )
                    .child(
                        Field::new()
                            .label(t!("host.dialog.password"))
                            .col_span(2)
                            .child(password.password_input("host-proxy-password")),
                    ),
            )
            .child(
                div()
                    .id("host-route-note")
                    .test_support()
                    .aria_label(t!("host.dialog.proxy_note"))
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(t!("host.dialog.proxy_note")),
            )
    }
}

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
        let serial = self.protocol == ProtocolKind::Serial;
        // Whether the form saves a password of the host's own.
        let typed_password = match self.protocol {
            ProtocolKind::Ssh => self.source == AuthSource::Password,
            ProtocolKind::Telnet => self.telnet_password_typed(),
            ProtocolKind::Serial => false,
        };
        let keychain = self.fields.read(cx).keychain_available();
        let secret_note = match (keychain, self.protocol) {
            (true, _) => t!("host.dialog.keychain_note"),
            // Telnet asks nobody: the password is typed in the terminal.
            (false, ProtocolKind::Telnet) => t!("host.dialog.telnet_no_keychain_note"),
            (false, _) => t!("host.dialog.no_keychain_note"),
        };
        // Four columns so the address and its port share a row, as they are
        // written (`host:port`), and a serial line's framing takes one row;
        // every other field takes a row of its own.
        let form = Form::new()
            .columns(4)
            .child(
                Field::new()
                    .label(t!("host.dialog.name"))
                    .col_span(4)
                    .child(Input::new(&self.name).id("host-name").small()),
            )
            .child(self.protocol_field(cx));
        let form = match self.protocol {
            ProtocolKind::Ssh => self.ssh_fields(form, cx),
            ProtocolKind::Telnet => self.telnet_fields(form, cx),
            ProtocolKind::Serial => self.serial_fields(form, cx),
        };
        // What only a saved host has.
        let form = if self.temporary {
            form
        } else {
            form.child(
                Field::new()
                    .label(t!("host.dialog.group"))
                    .col_span(4)
                    .child(Select::new(&self.group).small()),
            )
            .when(!serial, |form| form.child(self.route_field(cx)))
            .child(
                Field::new()
                    .label(t!("host.dialog.notes"))
                    .col_span(4)
                    .child(
                        div()
                            .id("host-notes")
                            .test_support()
                            .w_full()
                            .child(Textarea::new(&self.notes).text_sm()),
                    ),
            )
        };
        v_flex()
            .gap_3()
            .w_full()
            .child(form)
            .when(self.temporary, |form| {
                form.child(
                    div()
                        .id("temporary-note")
                        .test_support()
                        .aria_label(t!("host.dialog.temporary_note"))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("host.dialog.temporary_note")),
                )
            })
            .when(!self.temporary && typed_password, |form| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(secret_note),
                )
            })
            .when(self.has_tabs, |form| {
                form.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(t!("host.dialog.next_connection_note")),
                )
            })
    }
}

impl HostForm {
    /// Where an SSH or Telnet host is: its address, and its port beside it.
    fn address_fields(&self, form: Form) -> Form {
        form.child(
            Field::new()
                .label(t!("host.dialog.address"))
                .required(true)
                .col_span(3)
                .child(Input::new(&self.address).id("host-address").small()),
        )
        .child(
            Field::new()
                .label(t!("host.dialog.port"))
                .child(Input::new(&self.port).id("host-port").small()),
        )
    }

    /// A Telnet host's fields: where it is, and its 自动登录 when it has one:
    /// the user name and password, typed here or a credential's, and the
    /// prompts to answer.
    fn telnet_fields(&self, form: Form, cx: &mut Context<Self>) -> Form {
        let auto_login = self.telnet_auto_login;
        let source = self.telnet_source;
        let form = self.address_fields(form).child(
            Field::new().col_span(4).child(
                Checkbox::new("host-telnet-auto-login")
                    .label(t!("host.dialog.telnet_auto_login"))
                    .checked(auto_login)
                    .small()
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.telnet_auto_login = *checked;
                        cx.notify();
                    })),
            ),
        );
        if !auto_login {
            return form;
        }
        let form = form.child(
            Field::new()
                .label(t!("host.dialog.auth"))
                .col_span(4)
                .child(
                    SegmentedControl::new("host-telnet-auth")
                        .selected_index(TelnetSource::ALL.iter().position(|each| *each == source))
                        .on_change(cx.listener(|this, ix: &usize, _, cx| {
                            if let Some(source) = TelnetSource::ALL.get(*ix) {
                                this.telnet_source = *source;
                                cx.notify();
                            }
                        }))
                        .segments(TelnetSource::ALL.map(|each| Segment::new(each.label()))),
                ),
        );
        let form = match source {
            TelnetSource::Manual => form
                .child(
                    Field::new()
                        .label(t!("host.dialog.user"))
                        .col_span(2)
                        .child(Input::new(&self.telnet_user).id("host-telnet-user").small()),
                )
                .child(
                    Field::new()
                        .label(t!("host.dialog.password"))
                        .col_span(2)
                        .child(
                            self.telnet_fields
                                .read(cx)
                                .password_input("host-telnet-password"),
                        ),
                ),
            TelnetSource::Credential => form.child(self.telnet_credential_field(cx)),
        };
        let prompts = Form::new()
            .columns(2)
            .child(
                Field::new()
                    .label(t!("host.dialog.telnet_user_prompt"))
                    .child(
                        Input::new(&self.telnet_user_prompt)
                            .id("host-telnet-user-prompt")
                            .small(),
                    ),
            )
            .child(
                Field::new()
                    .label(t!("host.dialog.telnet_password_prompt"))
                    .child(
                        Input::new(&self.telnet_password_prompt)
                            .id("host-telnet-password-prompt")
                            .small(),
                    ),
            );
        form.child(
            Field::new()
                .label_indent(false)
                .col_span(4)
                .child(prompts)
                .description(t!("host.dialog.telnet_prompt_note")),
        )
    }

    /// An SSH host's fields: where it is, and how it logs in.
    fn ssh_fields(&self, form: Form, cx: &mut Context<Self>) -> Form {
        let source = self.source;
        let form = self.address_fields(form).child(
            Field::new()
                .label(t!("host.dialog.auth"))
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
                            .aria_label(t!("host.dialog.no_password_note"))
                            .child(t!("host.dialog.no_password_note"))
                    })
                }),
        );
        match source.auth() {
            Some(auth) => self.own_fields(form, auth, cx),
            None => form.child(self.credential_field(cx)),
        }
    }
}

/// A device the device field gives as an example, the way this system
/// names its serial ports.
fn example_device() -> &'static str {
    if cfg!(windows) {
        "COM3"
    } else if cfg!(target_os = "macos") {
        "/dev/cu.usbserial-1410"
    } else {
        "/dev/ttyUSB0"
    }
}

/// A connection test, run on a thread of its own with the way to answer a
/// trust question.
type TestJob = Box<dyn FnOnce(TrustCallback) -> Result<(), String> + Send>;

/// A credential select and what goes around it.
struct CredentialChoice<'a> {
    state: &'a Entity<SelectState<Vec<SharedString>>>,
    selected: Option<&'a Credential>,
    /// Whether there are no credentials to pick at all.
    none: bool,
    id: &'static str,
    summary_id: &'static str,
    /// What the open select says with nothing to list.
    empty: SharedString,
    /// What the field says with nothing to list.
    hint: SharedString,
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
pub(super) fn credential_option(credential: &Credential, key_dir: Option<&Path>) -> SharedString {
    t!(
        "host.dialog.credential_option",
        name = credential.name,
        summary = credential_summary(credential, key_dir)
    )
}

/// What a host using `credential` logs in as and with.
fn logs_in_with(credential: &Credential, key_dir: Option<&Path>) -> SharedString {
    let user = &credential.user;
    match credential.kind {
        CredentialKind::Password => t!("host.dialog.logs_in_with.password", user = user),
        CredentialKind::Key if credential.keeps_key_in(key_dir) => {
            t!("host.dialog.logs_in_with.kept_key", user = user)
        }
        CredentialKind::Key => t!(
            "host.dialog.logs_in_with.key",
            user = user,
            path = credential.key_path.as_deref().unwrap_or_default()
        ),
        CredentialKind::Agent => t!("host.dialog.logs_in_with.agent", user = user),
    }
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
    let description = crate::ssh::unknown_host_description(&prompt);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let answer = |trusted: bool| {
            let reply = reply.clone();
            move || {
                let _ = reply.send(trusted);
            }
        };
        let (trust, decline, dismiss) = (answer(true), answer(false), answer(false));
        alert
            .title(t!("host.trust.title"))
            .description(description.clone())
            .button_props(
                DialogButtonProps::default()
                    .ok_text(t!("host.trust.trust"))
                    .cancel_text(t!("common.cancel")),
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
        Ok(()) => Notification::success(t!("host.test.connected")),
        Err(reason) => Notification::error(reason).title(t!("host.test.failed")),
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
    tabs_open: bool,
    preselect_group: Option<GroupId>,
    store: Entity<HostStore>,
    tester: SharedConnectionTester,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| {
        HostForm::new(
            editing,
            tabs_open,
            preselect_group,
            store,
            tester,
            window,
            cx,
        )
    });
    let title = if editing.is_some() {
        t!("host.dialog.title_edit")
    } else {
        t!("host.dialog.title_new")
    };
    let commit_label = if editing.is_some() {
        t!("common.save")
    } else {
        t!("host.dialog.create")
    };

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
                .footer(form_footer(&form, commit_label.clone(), cx))
                .on_ok({
                    let form = form.clone();
                    move |_, window, cx| form.update(cx, |form, cx| form.commit(window, cx))
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
        }
    });
}

/// The host form's footer: 「测试连接」 on the left, 「取消」 and the
/// commit on the right. The new-host, edit-host and 临时连接 dialogs share it.
fn form_footer(form: &Entity<HostForm>, commit_label: SharedString, cx: &App) -> DialogFooter {
    DialogFooter::new()
        .w_full()
        .justify_between()
        .child(
            Button::new("test-connection")
                .label(t!("host.dialog.test_connection"))
                .icon(crate::app::CatalogIcon::Plug)
                .small()
                .loading(form.read(cx).testing_connection)
                .on_click({
                    let form = form.clone();
                    move |event, window, cx| {
                        form.update(cx, |form, cx| form.test_connection(event, window, cx));
                    }
                }),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new("cancel")
                        .label(t!("common.cancel"))
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(Cancel), cx);
                        }),
                )
                .child(
                    Button::new("commit")
                        .primary()
                        .label(commit_label)
                        .small()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(Confirm { secondary: false }), cx);
                        }),
                ),
        )
}

/// Open the 临时连接 dialog: a host's login, connected to in a terminal tab
/// without being saved. `dispatch` is the workspace's focus handle, which the
/// connection is asked of once the dialog has closed and left the focus
/// nowhere.
pub fn open_temporary_connection_dialog(
    store: Entity<HostStore>,
    tester: SharedConnectionTester,
    dispatch: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let form = cx.new(|cx| HostForm::temporary(store, tester, window, cx));
    window.open_dialog(cx, {
        let form = form.clone();
        move |dialog, window, cx| {
            dialog
                .title(t!("host.dialog.title_temporary"))
                .margin_top(window.viewport_size().height * DIALOG_TOP)
                .max_h(window.viewport_size().height * DIALOG_MAX_HEIGHT)
                .overlay_closable(false)
                .child(form.clone())
                .footer(form_footer(&form, t!("host.dialog.connect"), cx))
                .on_ok({
                    let form = form.clone();
                    let dispatch = dispatch.clone();
                    move |_, window, cx| {
                        let Some(host) =
                            form.update(cx, |form, cx| form.commit_temporary(window, cx))
                        else {
                            return false;
                        };
                        // After the dialog is gone, so the new tab keeps the
                        // focus it takes.
                        let dispatch = dispatch.clone();
                        window.defer(cx, move |window, cx| {
                            dispatch.dispatch_action(&ConnectHost(host), window, cx)
                        });
                        true
                    }
                })
                .on_close(|_, window, cx| dismiss_form_error(window, cx))
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
/// Its open tabs are not its business: they stay.
pub fn confirm_delete_host(
    host: &Host,
    dependents: Dependents,
    on_delete: DeleteHandler,
    window: &mut Window,
    cx: &mut App,
) {
    confirm_delete(
        &host.name,
        describe_host_delete(dependents),
        on_delete,
        window,
        cx,
    );
}

/// What the delete dialog says goes with the host. `None` for a host
/// nothing depends on.
fn describe_host_delete(dependents: Dependents) -> Option<SharedString> {
    let mut description = Vec::new();
    let Dependents {
        forwards,
        jump_users,
    } = dependents;
    if forwards > 0 {
        description.push(tn!("host.delete.forwards", forwards));
    }
    if jump_users > 0 {
        description.push(tn!("host.delete.jump_users", jump_users));
    }
    join_sentences(description)
}

/// Sentences one after another, as a description says them: Chinese ones
/// end in 。 and follow straight on, English ones take a space between.
/// `None` for no sentences.
pub fn join_sentences(sentences: Vec<SharedString>) -> Option<SharedString> {
    let mut text = String::new();
    for sentence in sentences {
        if text.ends_with(|c: char| c.is_ascii()) {
            text.push(' ');
        }
        text.push_str(&sentence);
    }
    (!text.is_empty()).then(|| text.into())
}

fn proxy_kind_label(kind: ProxyKind) -> SharedString {
    match kind {
        ProxyKind::Http => t!("host.proxy_kind.http"),
        ProxyKind::Socks5 => t!("host.proxy_kind.socks5"),
    }
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
        assert_eq!(describe_host_delete(forwards(0)), None);
        assert_eq!(
            describe_host_delete(forwards(3)).as_deref(),
            Some("将同时删除经由该主机的 3 条端口转发。")
        );
    }

    #[test]
    fn the_sentences_of_an_english_description_are_spaced() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        let dependents = Dependents {
            forwards: 2,
            jump_users: 1,
        };
        assert_eq!(
            describe_host_delete(dependents).as_deref(),
            Some(
                "2 port forwards through this host will also be deleted. \
                 1 host uses it as a jump host and will need another one to connect."
            )
        );
    }

    #[test]
    fn deleting_a_jump_host_says_who_loses_their_way_through() {
        let dependents = Dependents {
            forwards: 1,
            jump_users: 2,
        };
        assert_eq!(
            describe_host_delete(dependents).as_deref(),
            Some(
                "将同时删除经由该主机的 1 条端口转发。有 2 台主机把它用作跳板主机，删除后要重新选择跳板主机才能连接。"
            )
        );
    }
}
