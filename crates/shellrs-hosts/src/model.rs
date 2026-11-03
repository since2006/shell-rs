use gpui_shared_string::SharedString;
use rand::{Rng as _, distr::Alphanumeric};

use crate::secrets::SecretRef;

use super::{CredentialId, Protocol, ProtocolKind, SerialSettings, TelnetSettings};

/// Stable identity of a host. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostId(pub u64);

/// The identity a host shows outside the app, such as
/// `Jwg5rHvXCxw89paM`: what 复制 ID copies, so a script or another tool can
/// name the machine. Random letters and digits, not derived from
/// [`HostId`]: that one is an allocator detail that can come back after a
/// restart, while this one is never reused and stays with its host
/// through renames and address changes. It names a host; it grants
/// nothing.
///
/// A credential uses one the same way to name its password in the keychain.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PublicId(SharedString);

impl PublicId {
    const LEN: usize = 16;

    /// A new id: 16 characters from 62, about 95 random bits.
    pub fn generate() -> Self {
        let id: String = rand::rng()
            .sample_iter(&Alphanumeric)
            .take(Self::LEN)
            .map(char::from)
            .collect();
        Self(id.into())
    }

    /// A new id that `in_use` does not already claim. Sixteen random
    /// characters practically never collide, but ids must be unique.
    pub fn generate_unused(in_use: impl Fn(&PublicId) -> bool) -> Self {
        loop {
            let id = Self::generate();
            if !in_use(&id) {
                return id;
            }
        }
    }

    /// Whether `text` looks like an id: sixteen ASCII letters and digits.
    pub fn has_shape(text: &str) -> bool {
        text.len() == Self::LEN && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }

    /// An id read back from the database.
    pub(crate) fn from_stored(id: String) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PublicId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable identity of a host group (a folder in the host tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(pub u64);

/// How a host logs in on its own, without a credential. A key, or a
/// password shared by several hosts, is a credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AuthKind {
    /// A password typed into the host form, or asked for at each connection
    /// when none was saved.
    #[default]
    Password,
    /// Nothing typed: the server lets the user in as they are, or the SSH
    /// agent or a default key in `~/.ssh` does. A server that wants a
    /// password is refused rather than asked.
    NoPassword,
}

impl AuthKind {
    /// The stored spelling. Kept separate from any label so translating the
    /// UI cannot rewrite what is already in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::Password => "password",
            AuthKind::NoPassword => "no-password",
        }
    }

    /// Parse a stored spelling. Anything else is the default: `credential`,
    /// which a host using a credential stores, keeps the default here.
    pub fn from_stored(value: &str) -> Self {
        match value {
            "no-password" => AuthKind::NoPassword,
            _ => AuthKind::Password,
        }
    }
}

/// How a connection reaches a host, as the host form's 「连接方式」 offers it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Route {
    /// A TCP connection from this machine.
    #[default]
    Direct,
    /// Through other saved hosts, one after another: this machine logs in to
    /// the first, which opens a channel to the second, and so on to the
    /// host. Only this list counts; a jump host's own route does not.
    ///
    /// `None` is a jump host that has been deleted. It stays in its place
    /// so that the host fails to connect, saying why, rather than quietly
    /// skipping a hop or going direct.
    Jump(Vec<Option<HostId>>),
    /// Through an HTTP or SOCKS5 proxy.
    Proxy(ProxySettings),
}

impl Route {
    /// The jump hosts this route goes through, deleted ones left out.
    pub fn jump_hosts(&self) -> impl Iterator<Item = HostId> + '_ {
        let hops = match self {
            Route::Jump(hops) => hops.as_slice(),
            _ => &[],
        };
        hops.iter().flatten().copied()
    }
}

/// The proxy of a host that connects through one.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProxySettings {
    pub kind: ProxyKind,
    pub host: SharedString,
    pub port: u16,
    /// For a proxy that wants a user name and password; its password is in
    /// the keychain.
    pub user: Option<SharedString>,
}

impl ProxySettings {
    pub fn new(kind: ProxyKind, host: impl Into<SharedString>, port: u16) -> Self {
        Self {
            kind,
            host: host.into(),
            port,
            user: None,
        }
    }

    /// Log in to the proxy as `user`. An empty name is no name.
    pub fn with_user(mut self, user: impl Into<SharedString>) -> Self {
        let user = user.into();
        self.user = (!user.is_empty()).then_some(user);
        self
    }

    /// Where the proxy's password lives in the keychain. Only a proxy with
    /// a user name has one; hosts behind the same proxy as the same user
    /// share it.
    pub fn password_secret(&self) -> Option<SecretRef> {
        self.user
            .as_ref()
            .map(|user| SecretRef::proxy(user.as_ref(), self.host.as_ref(), self.port))
    }
}

pub use shellrs_ssh::ProxyKind;

/// Which pane of a host's SFTP tab a bookmark belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BookmarkSide {
    Local,
    Remote,
}

impl BookmarkSide {
    pub fn from_remote(remote: bool) -> Self {
        if remote { Self::Remote } else { Self::Local }
    }

    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            BookmarkSide::Local => "local",
            BookmarkSide::Remote => "remote",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "local" => Some(BookmarkSide::Local),
            "remote" => Some(BookmarkSide::Remote),
            _ => None,
        }
    }
}

/// The operating system running on a host, as reported by the host's own
/// shell after it connects.
///
/// Each variant owns four things that must stay in step: the spelling kept in
/// the database, the name shown to a person, the icon embedded in
/// `app/assets.rs`, and the brand's own colour. Adding a distribution means
/// one line here, one icon file, and one arm in `ssh/probe.rs`.
///
/// The colours are the published brand values, which is why they are literals
/// rather than theme tokens: here the colour *is* the data, the one case the
/// design guides allow. Keeping them in this table is what stops them from
/// leaking into render code.
macro_rules! host_os {
    ($($variant:ident => $stored:literal, $label:literal, $icon:literal, $brand:expr;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum HostOs {
            $($variant,)*
        }

        impl HostOs {
            /// Every variant, for tests that must cover the whole table.
            pub const ALL: &'static [HostOs] = &[$(HostOs::$variant,)*];

            /// How the database spells it. Kept separate from `label` so
            /// translating the UI cannot rewrite what is already stored.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $stored,)* }
            }

            /// What a person sees, in a tooltip or the status bar.
            pub fn label(self) -> &'static str {
                match self { $(Self::$variant => $label,)* }
            }

            /// Path into the application asset bundle.
            pub fn icon_path(self) -> &'static str {
                match self { $(Self::$variant => concat!("icons/os/", $icon, ".svg"),)* }
            }

            /// The brand's published colour as `0xRRGGBB`, for the badge
            /// behind the mark.
            ///
            /// `None` means the mark is monochrome and should follow the
            /// theme instead. That is how Apple's is meant to be drawn, and
            /// the honest answer where a project publishes no single colour.
            pub fn brand_color(self) -> Option<u32> {
                match self { $(Self::$variant => $brand,)* }
            }

            /// What to draw the mark itself in, on top of [`Self::brand_color`].
            ///
            /// White, the way nearly every brand draws its own mark, except on
            /// the light ones where the brand uses a dark mark instead.
            pub fn brand_foreground(self) -> Option<u32> {
                self.brand_color().map(|color| {
                    if is_light(color) {
                        BRAND_DARK_MARK
                    } else {
                        BRAND_LIGHT_MARK
                    }
                })
            }

            /// Read back a stored spelling. An unknown one means the row was
            /// written by a newer build, so it is treated as undetected.
            pub fn from_stored(value: &str) -> Option<Self> {
                match value {
                    $($stored => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

host_os! {
    Ubuntu   => "ubuntu",    "Ubuntu",           "ubuntu",      Some(0xE9_5420);
    Debian   => "debian",    "Debian",           "debian",      Some(0xA8_1D33);
    Fedora   => "fedora",    "Fedora",           "fedora",      Some(0x51_A2DA);
    RedHat   => "rhel",      "Red Hat",          "redhat",      Some(0xEE_0000);
    CentOs   => "centos",    "CentOS",           "centos",      Some(0x26_2577);
    Rocky    => "rocky",     "Rocky Linux",      "rockylinux",  Some(0x10_B981);
    // AlmaLinux publishes no single flat colour, so its mark follows the theme.
    Alma     => "almalinux", "AlmaLinux",        "almalinux",   None;
    Arch     => "arch",      "Arch Linux",       "archlinux",   Some(0x17_93D1);
    Alpine   => "alpine",    "Alpine Linux",     "alpinelinux", Some(0x0D_597F);
    Suse     => "suse",      "openSUSE",         "opensuse",    Some(0x73_BA25);
    Gentoo   => "gentoo",    "Gentoo",           "gentoo",      Some(0x54_487A);
    Kali     => "kali",      "Kali Linux",       "kalilinux",   Some(0x55_7C94);
    Manjaro  => "manjaro",   "Manjaro",          "manjaro",     Some(0x35_BFA4);
    Raspbian => "raspbian",  "Raspberry Pi OS",  "raspberrypi", Some(0xA2_2846);
    Linux    => "linux",     "Linux",            "linux",       Some(0xFC_C624);
    MacOs    => "macos",     "macOS",            "apple",       None;
    Windows  => "windows",   "Windows",          "windows",     Some(0x00_78D4);
    FreeBsd  => "freebsd",   "FreeBSD",          "freebsd",     Some(0xAB_2B28);
    OpenBsd  => "openbsd",   "OpenBSD",          "openbsd",     Some(0xF2_CA30);
    NetBsd   => "netbsd",    "NetBSD",           "netbsd",      Some(0xFF_6600);
}

impl HostOs {
    /// Whether the host runs Linux, any distribution of it: what the system
    /// monitor can read.
    pub fn is_linux(self) -> bool {
        !matches!(
            self,
            HostOs::MacOs | HostOs::Windows | HostOs::FreeBsd | HostOs::OpenBsd | HostOs::NetBsd
        )
    }
}

/// Drawn on top of a light brand colour.
const BRAND_DARK_MARK: u32 = 0x00_0000;
/// Drawn on top of every other brand colour.
const BRAND_LIGHT_MARK: u32 = 0xFF_FFFF;

/// Whether a brand colour is light enough that the brand puts a dark mark on
/// it. The threshold sits well above the point where white text would fail,
/// because a logo silhouette is not body text and nearly every brand draws its
/// own mark in white; only the yellows go the other way.
fn is_light(color: u32) -> bool {
    super::mark::luminance(color) > 0.5
}

/// A saved host: an SSH or Telnet server, or a serial port of this computer. `group`
/// is `None` for a host that sits at the root of the tree rather than
/// inside a folder.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Host {
    pub id: HostId,
    /// Set once when the host is created; editing never changes it.
    pub public_id: PublicId,
    pub name: SharedString,
    /// SSH, Telnet or serial, for good: see [`Protocol`].
    pub protocol: Protocol,
    /// The host name or IP address it is reached at: 「地址」 in the form.
    /// A serial host's device, such as `/dev/cu.usbserial-1410` or `COM3`.
    pub address: SharedString,
    pub port: u16,
    /// The user it logs in as. For a host using a credential this is the
    /// credential's user, which the store keeps in step. A Telnet host's
    /// may be empty: its device asks only for a password.
    pub user: SharedString,
    /// How a host logs in on its own. A host using a credential keeps
    /// the default here. For a Telnet host, [`AuthKind::Password`] is 自动登录
    /// with its own user and password, and [`AuthKind::NoPassword`] no
    /// 自动登录 at all.
    pub auth: AuthKind,
    /// The credential it logs in with instead of `auth`, if any.
    pub credential: Option<CredentialId>,
    /// How a connection reaches it.
    pub route: Route,
    pub group: Option<GroupId>,
    /// Whatever the user wants to remember about it; empty for none.
    pub notes: SharedString,
    /// Order among hosts in the same group.
    pub sort_order: i64,
    /// Detected on every successful connection and persisted, so the tree can
    /// show the right mark before anyone connects. `None` until a probe
    /// succeeds, and the host tree falls back to the name's first letter.
    pub os: Option<HostOs>,
    /// The right sidebar's tool as the host's terminals last had it, `None`
    /// while hidden: the next terminal of the host in front brings it back.
    /// Written by the sidebar, never by the host form. The tool's key
    /// (`ToolKind::key` in the app), kept as it is even when this version
    /// has no such tool.
    pub sidebar_tool: Option<SharedString>,
    /// When the user last changed what syncs of it, in milliseconds since
    /// the epoch; 0 for a host sync brought. `os` and the like are not
    /// edits.
    pub edited_ms: u64,
}

impl Host {
    /// A host with a fresh [`PublicId`]. The store makes sure it is not
    /// one another host already has.
    pub fn new(id: HostId, draft: HostDraft) -> Self {
        Self {
            id,
            public_id: PublicId::generate(),
            name: draft.name,
            protocol: draft.protocol,
            address: draft.address,
            port: draft.port,
            user: draft.user,
            auth: draft.auth,
            credential: draft.credential,
            route: draft.route,
            group: draft.group,
            notes: draft.notes,
            sort_order: 0,
            os: None,
            sidebar_tool: None,
            edited_ms: 0,
        }
    }

    /// `user@address:port`, as shown in the status bar; a Telnet host's
    /// `address:port`, its user being only what 自动登录 types; a serial
    /// host's device and line, `/dev/cu.usbserial-1410 · 115200 8N1`.
    pub fn endpoint(&self) -> String {
        match &self.protocol {
            Protocol::Ssh => format!("{}@{}:{}", self.user, self.address, self.port),
            Protocol::Telnet(_) => format!("{}:{}", self.address, self.port),
            Protocol::Serial(settings) => format!("{} · {}", self.address, settings.summary()),
        }
    }

    pub fn kind(&self) -> ProtocolKind {
        self.protocol.kind()
    }

    pub fn is_ssh(&self) -> bool {
        self.protocol.is_ssh()
    }

    pub fn is_serial(&self) -> bool {
        self.protocol.is_serial()
    }

    /// Whether the address is a literal IP address rather than a name, so a
    /// command can say which of the two it copies.
    pub fn address_is_ip(&self) -> bool {
        is_ip_address(&self.address)
    }

    /// Where this host's login password lives in the system keychain.
    /// Keyed by the endpoint, so renaming or copying a host keeps the
    /// password and two hosts on the same account share one entry.
    pub fn password_secret(&self) -> SecretRef {
        SecretRef::password(self.user.as_ref(), self.address.as_ref(), self.port)
    }

    /// The editable fields, for pre-filling the host form.
    pub fn draft(&self) -> HostDraft {
        HostDraft {
            name: self.name.clone(),
            protocol: self.protocol.clone(),
            address: self.address.clone(),
            port: self.port,
            user: self.user.clone(),
            auth: self.auth,
            credential: self.credential,
            route: self.route.clone(),
            group: self.group,
            notes: self.notes.clone(),
        }
    }
}

/// IPv6 literals are accepted with or without the brackets a URL puts around
/// them.
fn is_ip_address(host: &str) -> bool {
    let host = host.trim();
    let bare = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>().is_ok()
}

/// A folder in the host tree. Groups nest: `parent` is `None` for a
/// top-level folder.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct HostGroup {
    pub id: GroupId,
    pub name: SharedString,
    pub parent: Option<GroupId>,
    /// Order among groups with the same parent.
    pub sort_order: i64,
    /// Whether the group is expanded in the host tree across launches.
    pub expanded: bool,
    /// The group's id on every device it syncs to. The store makes sure no
    /// other group has it.
    pub sync_id: PublicId,
    /// When the user last changed it; see [`Host::edited_ms`].
    pub edited_ms: u64,
}

impl HostGroup {
    pub fn new(id: GroupId, draft: GroupDraft) -> Self {
        Self {
            id,
            name: draft.name,
            parent: draft.parent,
            sort_order: 0,
            expanded: true,
            sync_id: PublicId::generate(),
            edited_ms: 0,
        }
    }

    /// The editable fields, for pre-filling the group form.
    pub fn draft(&self) -> GroupDraft {
        GroupDraft {
            name: self.name.clone(),
            parent: self.parent,
        }
    }
}

/// The values the host form commits.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostDraft {
    pub name: SharedString,
    pub protocol: Protocol,
    pub address: SharedString,
    pub port: u16,
    pub user: SharedString,
    pub auth: AuthKind,
    pub credential: Option<CredentialId>,
    pub route: Route,
    pub group: Option<GroupId>,
    pub notes: SharedString,
}

impl HostDraft {
    pub fn new(
        name: impl Into<SharedString>,
        address: impl Into<SharedString>,
        port: u16,
        user: impl Into<SharedString>,
        auth: AuthKind,
        group: Option<GroupId>,
    ) -> Self {
        Self {
            name: name.into(),
            protocol: Protocol::Ssh,
            address: address.into(),
            port,
            user: user.into(),
            auth,
            credential: None,
            route: Route::Direct,
            group,
            notes: SharedString::default(),
        }
    }

    /// A serial host on `device`. What only SSH uses keeps its default.
    pub fn serial(
        name: impl Into<SharedString>,
        device: impl Into<SharedString>,
        settings: SerialSettings,
        group: Option<GroupId>,
    ) -> Self {
        Self {
            protocol: Protocol::Serial(settings),
            ..Self::new(name, device, SSH_PORT, "", AuthKind::NoPassword, group)
        }
    }

    /// A Telnet host at `address:port`, without 自动登录. What only SSH uses
    /// keeps its default.
    pub fn telnet(
        name: impl Into<SharedString>,
        address: impl Into<SharedString>,
        port: u16,
        group: Option<GroupId>,
    ) -> Self {
        Self {
            protocol: Protocol::Telnet(TelnetSettings::default()),
            ..Self::new(name, address, port, "", AuthKind::NoPassword, group)
        }
    }

    /// Log in with a saved credential instead of `auth`. The store takes the
    /// user name from the credential when the draft is saved.
    pub fn with_credential(mut self, credential: CredentialId) -> Self {
        self.credential = Some(credential);
        self
    }

    /// Reach the host some other way than directly.
    pub fn with_route(mut self, route: Route) -> Self {
        self.route = route;
        self
    }

    pub fn with_notes(mut self, notes: impl Into<SharedString>) -> Self {
        self.notes = notes.into();
        self
    }

    /// The keychain entry this draft would log in with. Matches
    /// [`Host::password_secret`] once the draft is applied.
    pub fn password_secret(&self) -> SecretRef {
        SecretRef::password(self.user.as_ref(), self.address.as_ref(), self.port)
    }

    /// The draft with surrounding blanks removed, an empty name taken as
    /// the address and an empty user name as [`super::DEFAULT_USER`] (a
    /// Telnet host may have none), or the first thing wrong with it,
    /// in the order the host form checks. What the form cannot leave out
    /// of a draft (a credential to pick, a proxy password without a user)
    /// is the caller's to check.
    pub fn validated(mut self) -> Result<Self, HostDraftError> {
        self.name = self.name.trim().to_string().into();
        self.address = self.address.trim().to_string().into();
        if let Protocol::Serial(settings) = self.protocol {
            return self.validated_serial(settings);
        }
        if let Protocol::Telnet(settings) = &mut self.protocol {
            settings.user_prompt = settings.user_prompt.trim().to_string().into();
            settings.password_prompt = settings.password_prompt.trim().to_string().into();
        }
        if self.address.is_empty() {
            return Err(HostDraftError::Address);
        }
        if self.name.is_empty() {
            self.name = self.address.clone();
        }
        if self.port == 0 {
            return Err(HostDraftError::Port);
        }
        let user = self.user.trim();
        self.user = if user.is_empty() && !self.protocol.is_telnet() {
            super::DEFAULT_USER.into()
        } else {
            user.to_string().into()
        };
        match &mut self.route {
            Route::Direct => {}
            Route::Jump(hops) if hops.is_empty() => return Err(HostDraftError::NoJumpHosts),
            Route::Jump(hops) if hops.contains(&None) => {
                return Err(HostDraftError::DeletedJumpHost);
            }
            Route::Jump(_) => {}
            Route::Proxy(proxy) => {
                proxy.host = proxy.host.trim().to_string().into();
                if proxy.host.is_empty() {
                    return Err(HostDraftError::ProxyAddress);
                }
                if proxy.port == 0 {
                    return Err(HostDraftError::ProxyPort);
                }
                let user = proxy.user.as_deref().map(str::trim).unwrap_or_default();
                proxy.user = (!user.is_empty()).then(|| user.to_string().into());
            }
        }
        self.notes = self.notes.trim().to_string().into();
        Ok(self)
    }

    /// [`Self::validated`] for a serial host, which needs a device and a
    /// rate and keeps none of what only SSH uses.
    fn validated_serial(self, settings: SerialSettings) -> Result<Self, HostDraftError> {
        if self.address.is_empty() {
            return Err(HostDraftError::Device);
        }
        if settings.baud == 0 {
            return Err(HostDraftError::Baud);
        }
        let name = if self.name.is_empty() {
            self.address.clone()
        } else {
            self.name.clone()
        };
        let notes = self.notes.trim().to_string();
        Ok(Self::serial(name, self.address, settings, self.group).with_notes(notes))
    }
}

/// The port of an SSH host that names none, also what a serial host keeps
/// in the column it has no use for.
pub const SSH_PORT: u16 = 22;

/// The port of a Telnet host that names none.
pub const TELNET_PORT: u16 = 23;

/// Why a host draft cannot be saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostDraftError {
    Address,
    Port,
    Device,
    Baud,
    NoJumpHosts,
    DeletedJumpHost,
    ProxyAddress,
    ProxyPort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_host_name_uses_the_trimmed_address() {
        for (name, expected) in [
            ("", "example.com"),
            (" \t ", "example.com"),
            (" web-01 ", "web-01"),
        ] {
            let draft = HostDraft::new(name, " example.com ", 2222, "", AuthKind::Password, None)
                .validated()
                .unwrap();
            assert_eq!(draft.name.as_ref(), expected);
            assert_eq!(draft.address.as_ref(), "example.com");
            assert_eq!(draft.user.as_ref(), super::super::DEFAULT_USER);
            assert_eq!(draft.port, 2222);
        }
    }

    #[test]
    fn a_host_still_requires_an_address_with_or_without_a_name() {
        for name in ["", " ", "web-01"] {
            for address in ["", " \t "] {
                let error = HostDraft::new(name, address, 22, "root", AuthKind::Password, None)
                    .validated()
                    .unwrap_err();
                assert_eq!(error, HostDraftError::Address);
            }
        }
        let error = HostDraft::new("", "example.com", 0, "root", AuthKind::Password, None)
            .validated()
            .unwrap_err();
        assert_eq!(error, HostDraftError::Port);
    }

    #[test]
    fn a_serial_host_needs_a_device_and_a_rate_and_keeps_nothing_of_ssh() {
        let settings = SerialSettings::default();
        let draft = HostDraft::serial(" ", " /dev/ttyUSB0 ", settings, Some(GroupId(2)))
            .with_notes(" 机柜 3 ")
            .validated()
            .unwrap();
        assert_eq!(draft.name.as_ref(), "/dev/ttyUSB0");
        assert_eq!(draft.address.as_ref(), "/dev/ttyUSB0");
        assert_eq!(draft.protocol, Protocol::Serial(settings));
        assert_eq!(draft.user.as_ref(), "");
        assert_eq!(draft.route, Route::Direct);
        assert_eq!(draft.group, Some(GroupId(2)));
        assert_eq!(draft.notes.as_ref(), "机柜 3");

        let error = HostDraft::serial("console", " ", settings, None)
            .validated()
            .unwrap_err();
        assert_eq!(error, HostDraftError::Device);
        let silent = SerialSettings {
            baud: 0,
            ..settings
        };
        let error = HostDraft::serial("console", "COM3", silent, None)
            .validated()
            .unwrap_err();
        assert_eq!(error, HostDraftError::Baud);
    }

    #[test]
    fn a_telnet_host_may_have_no_user_and_keeps_its_prompts_trimmed() {
        let mut draft = HostDraft::telnet("", " 10.0.0.5 ", 23, None);
        draft.protocol = Protocol::Telnet(TelnetSettings {
            user_prompt: " Username: ".into(),
            password_prompt: "".into(),
        });
        let draft = draft.validated().unwrap();
        assert_eq!(
            draft.user.as_ref(),
            "",
            "not root: the device may ask only for a password"
        );
        assert_eq!(draft.name.as_ref(), "10.0.0.5");
        assert_eq!(
            draft.protocol.telnet().unwrap().user_prompt.as_ref(),
            "Username:"
        );
        assert_eq!(
            HostDraft::telnet("x", "", 23, None).validated(),
            Err(HostDraftError::Address)
        );
        let host = Host::new(HostId(1), draft);
        assert_eq!(host.endpoint(), "10.0.0.5:23");
        assert_eq!(host.kind(), ProtocolKind::Telnet);
    }

    #[test]
    fn a_serial_host_shows_its_device_and_line_where_ssh_shows_its_endpoint() {
        let ssh = Host::new(
            HostId(1),
            HostDraft::new("web", "10.0.0.1", 22, "root", AuthKind::Password, None),
        );
        assert_eq!(ssh.endpoint(), "root@10.0.0.1:22");
        let serial = Host::new(
            HostId(2),
            HostDraft::serial("console", "COM3", SerialSettings::default(), None),
        );
        assert_eq!(serial.endpoint(), "COM3 · 115200 8N1");
        assert!(serial.is_serial());
        assert!(!ssh.is_serial());
    }

    #[test]
    fn the_distributions_are_linux_and_the_rest_are_not() {
        let not_linux: Vec<_> = HostOs::ALL
            .iter()
            .filter(|os| !os.is_linux())
            .map(|os| os.label())
            .collect();
        assert_eq!(
            not_linux,
            ["macOS", "Windows", "FreeBSD", "OpenBSD", "NetBSD"]
        );
    }

    #[test]
    fn public_ids_are_sixteen_letters_and_digits() {
        let id = PublicId::generate();
        assert_eq!(id.as_str().len(), 16);
        assert!(id.as_str().chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(PublicId::generate(), id);

        let taken = [id.clone()];
        assert_ne!(PublicId::generate_unused(|id| taken.contains(id)), id);
    }

    #[test]
    fn ip_literals_are_told_apart_from_host_names() {
        assert!(is_ip_address("10.0.1.21"));
        assert!(is_ip_address("::1"));
        assert!(is_ip_address("[fe80::1]"));
        assert!(!is_ip_address("web-01.example.com"));
        assert!(!is_ip_address("localhost"));
        assert!(!is_ip_address(""));
    }

    #[test]
    fn every_operating_system_round_trips_through_the_database_spelling() {
        for os in HostOs::ALL {
            assert_eq!(HostOs::from_stored(os.as_str()), Some(*os));
            assert!(!os.label().is_empty());
            assert!(os.icon_path().starts_with("icons/os/"));
        }
    }

    #[test]
    fn the_light_brands_get_a_dark_mark_and_the_rest_get_white() {
        // Tux is black on yellow, and OpenBSD's Puffy likewise.
        assert_eq!(HostOs::Linux.brand_foreground(), Some(0x00_0000));
        assert_eq!(HostOs::OpenBsd.brand_foreground(), Some(0x00_0000));
        // Everything else follows the usual white-on-brand treatment.
        assert_eq!(HostOs::Ubuntu.brand_foreground(), Some(0xFF_FFFF));
        assert_eq!(HostOs::Debian.brand_foreground(), Some(0xFF_FFFF));
        assert_eq!(HostOs::Windows.brand_foreground(), Some(0xFF_FFFF));
    }

    #[test]
    fn a_monochrome_mark_leaves_the_colour_to_the_theme() {
        assert_eq!(HostOs::MacOs.brand_color(), None);
        assert_eq!(HostOs::MacOs.brand_foreground(), None);
    }

    #[test]
    fn stored_spellings_and_icons_are_unique() {
        let mut spellings: Vec<_> = HostOs::ALL.iter().map(|os| os.as_str()).collect();
        spellings.sort_unstable();
        let count = spellings.len();
        spellings.dedup();
        assert_eq!(spellings.len(), count, "两个变体用了同一个存储拼写");
    }

    #[test]
    fn an_unknown_spelling_reads_as_undetected() {
        assert_eq!(HostOs::from_stored("plan9"), None);
        assert_eq!(HostOs::from_stored(""), None);
    }

    #[test]
    fn a_new_host_has_not_been_probed_yet() {
        let host = Host::new(
            HostId(1),
            HostDraft::new("s", "h", 22, "root", AuthKind::Password, None),
        );
        assert_eq!(host.os, None);
    }

    #[test]
    fn a_password_is_the_default_and_spellings_round_trip() {
        assert_eq!(AuthKind::default(), AuthKind::Password);
        for auth in [AuthKind::Password, AuthKind::NoPassword] {
            assert_eq!(AuthKind::from_stored(auth.as_str()), auth);
        }
        assert_eq!(AuthKind::from_stored("credential"), AuthKind::Password);
    }
}

/// The values the group form commits.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GroupDraft {
    pub name: SharedString,
    pub parent: Option<GroupId>,
}

impl GroupDraft {
    pub fn new(name: impl Into<SharedString>, parent: Option<GroupId>) -> Self {
        Self {
            name: name.into(),
            parent,
        }
    }
}

/// Where a group path such as `生产/数据库` puts a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupPlan {
    Existing(Option<GroupId>),
    /// Into groups made for it: `names` one inside the other, the first
    /// under `parent`.
    Create {
        parent: Option<GroupId>,
        names: Vec<String>,
    },
}

/// Why a group path names no one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupPathError {
    /// A level between two slashes is empty, as in `生产//数据库`.
    EmptyLevel { path: String },
    /// Several groups have the same path, so it picks none of them.
    Ambiguous { count: usize, path: String },
}
