use gpui_kit::{Rgba, SharedString, rgb};
use rand::{Rng as _, distr::Alphanumeric};

use crate::secrets::SecretRef;

use super::CredentialId;

/// Stable identity of a session. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// The identity a session shows outside the app, such as
/// `Jwg5rHvXCxw89paM`: what 复制 ID copies, so a script or another tool can
/// name the machine. Random letters and digits, not derived from
/// [`SessionId`]: that one is an allocator detail that can come back after a
/// restart, while this one is never reused and stays with its session
/// through renames and address changes. It names a session; it grants
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

/// Stable identity of a session group (a folder in the session tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(pub u64);

/// How a session logs in on its own, without a credential. A key, or a
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

    /// Parse a stored spelling. Anything else, including what versions
    /// before 10 wrote (`auto`, `key`), is a password: version 10's
    /// migration turned those rows into passwords and key credentials.
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
    Jump(Vec<Option<SessionId>>),
    /// Through an HTTP or SOCKS5 proxy.
    Proxy(ProxySettings),
}

impl Route {
    /// The jump hosts this route goes through, deleted ones left out.
    pub fn jump_hosts(&self) -> impl Iterator<Item = SessionId> + '_ {
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

/// The kind of proxy a host connects through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProxyKind {
    /// An HTTP proxy, which the connection asks to `CONNECT` to the host.
    #[default]
    Http,
    Socks5,
}

impl ProxyKind {
    /// Every kind, in the order the form lists them.
    pub const ALL: [ProxyKind; 2] = [ProxyKind::Http, ProxyKind::Socks5];

    pub fn label(self) -> &'static str {
        match self {
            ProxyKind::Http => "HTTP 代理",
            ProxyKind::Socks5 => "SOCKS5 代理",
        }
    }

    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyKind::Http => "http",
            ProxyKind::Socks5 => "socks5",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "http" => Some(ProxyKind::Http),
            "socks5" => Some(ProxyKind::Socks5),
            _ => None,
        }
    }
}

/// Which pane of a session's SFTP tab a bookmark belongs to.
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

/// The operating system running on a host, as reported by the session's own
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

            /// The brand's published colour, for the badge behind the mark.
            ///
            /// `None` means the mark is monochrome and should follow the
            /// theme instead. That is how Apple's is meant to be drawn, and
            /// the honest answer where a project publishes no single colour.
            pub fn brand_color(self) -> Option<Rgba> {
                let hex: Option<u32> = match self { $(Self::$variant => $brand,)* };
                hex.map(rgb)
            }

            /// What to draw the mark itself in, on top of [`Self::brand_color`].
            ///
            /// White, the way nearly every brand draws its own mark, except on
            /// the light ones where the brand uses a dark mark instead.
            pub fn brand_foreground(self) -> Option<Rgba> {
                self.brand_color().map(|color| {
                    if is_light(color) {
                        rgb(BRAND_DARK_MARK)
                    } else {
                        rgb(BRAND_LIGHT_MARK)
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

/// Drawn on top of a light brand colour.
const BRAND_DARK_MARK: u32 = 0x00_0000;
/// Drawn on top of every other brand colour.
const BRAND_LIGHT_MARK: u32 = 0xFF_FFFF;

/// Whether a brand colour is light enough that the brand puts a dark mark on
/// it. The threshold sits well above the point where white text would fail,
/// because a logo silhouette is not body text and nearly every brand draws its
/// own mark in white; only the yellows go the other way.
fn is_light(color: Rgba) -> bool {
    fn linear(channel: f32) -> f32 {
        if channel <= 0.03928 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
    let luminance = 0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b);
    luminance > 0.5
}

/// Connection state of a session. Runtime only: it is never persisted, so a
/// freshly loaded session always starts disconnected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

impl ConnectionState {
    pub fn label(self) -> &'static str {
        match self {
            ConnectionState::Disconnected => "未连接",
            ConnectionState::Connecting => "连接中",
            ConnectionState::Connected => "已连接",
        }
    }

    pub fn is_connected(self) -> bool {
        matches!(self, ConnectionState::Connected)
    }
}

/// A saved SSH session. `group` is `None` for a session that sits at the root
/// of the tree rather than inside a folder.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Session {
    pub id: SessionId,
    /// Set once when the session is created; editing never changes it.
    pub public_id: PublicId,
    pub name: SharedString,
    pub host: SharedString,
    pub port: u16,
    /// The user it logs in as. For a session using a credential this is the
    /// credential's user, which the store keeps in step.
    pub user: SharedString,
    /// How a session logs in on its own. A session using a credential keeps
    /// the default here.
    pub auth: AuthKind,
    /// The credential it logs in with instead of `auth`, if any.
    pub credential: Option<CredentialId>,
    /// How a connection reaches it.
    pub route: Route,
    pub group: Option<GroupId>,
    /// Order among sessions in the same group.
    pub sort_order: i64,
    pub state: ConnectionState,
    /// Detected on every successful connection and persisted, so the tree can
    /// show the right mark before anyone connects. `None` until a probe
    /// succeeds, and the session tree falls back to the name's first letter.
    pub os: Option<HostOs>,
}

impl Session {
    /// A session with a fresh [`PublicId`]. The store makes sure it is not
    /// one another session already has.
    pub fn new(id: SessionId, draft: SessionDraft) -> Self {
        Self {
            id,
            public_id: PublicId::generate(),
            name: draft.name,
            host: draft.host,
            port: draft.port,
            user: draft.user,
            auth: draft.auth,
            credential: draft.credential,
            route: draft.route,
            group: draft.group,
            sort_order: 0,
            state: ConnectionState::Disconnected,
            os: None,
        }
    }

    /// `user@host:port`, as shown in the status bar.
    pub fn address(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }

    /// Whether the host field is a literal IP address rather than a name, so
    /// a command can say which of the two it copies.
    pub fn host_is_ip(&self) -> bool {
        is_ip_address(&self.host)
    }

    /// Where this session's login password lives in the system keychain.
    /// Keyed by the endpoint, so renaming or copying a session keeps the
    /// password and two sessions on the same account share one entry.
    pub fn password_secret(&self) -> SecretRef {
        SecretRef::password(self.user.as_ref(), self.host.as_ref(), self.port)
    }

    /// The editable fields, for pre-filling the session form.
    pub fn draft(&self) -> SessionDraft {
        SessionDraft {
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth: self.auth,
            credential: self.credential,
            route: self.route.clone(),
            group: self.group,
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

/// A folder in the session tree. Groups nest: `parent` is `None` for a
/// top-level folder.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SessionGroup {
    pub id: GroupId,
    pub name: SharedString,
    pub parent: Option<GroupId>,
    /// Order among groups with the same parent.
    pub sort_order: i64,
    /// Whether the group is expanded in the session tree across launches.
    pub expanded: bool,
}

impl SessionGroup {
    pub fn new(id: GroupId, draft: GroupDraft) -> Self {
        Self {
            id,
            name: draft.name,
            parent: draft.parent,
            sort_order: 0,
            expanded: true,
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

/// The values the session form commits.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionDraft {
    pub name: SharedString,
    pub host: SharedString,
    pub port: u16,
    pub user: SharedString,
    pub auth: AuthKind,
    pub credential: Option<CredentialId>,
    pub route: Route,
    pub group: Option<GroupId>,
}

impl SessionDraft {
    pub fn new(
        name: impl Into<SharedString>,
        host: impl Into<SharedString>,
        port: u16,
        user: impl Into<SharedString>,
        auth: AuthKind,
        group: Option<GroupId>,
    ) -> Self {
        Self {
            name: name.into(),
            host: host.into(),
            port,
            user: user.into(),
            auth,
            credential: None,
            route: Route::Direct,
            group,
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

    /// The keychain entry this draft would log in with. Matches
    /// [`Session::password_secret`] once the draft is applied.
    pub fn password_secret(&self) -> SecretRef {
        SecretRef::password(self.user.as_ref(), self.host.as_ref(), self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(HostOs::Linux.brand_foreground(), Some(rgb(0x00_0000)));
        assert_eq!(HostOs::OpenBsd.brand_foreground(), Some(rgb(0x00_0000)));
        // Everything else follows the usual white-on-brand treatment.
        assert_eq!(HostOs::Ubuntu.brand_foreground(), Some(rgb(0xFF_FFFF)));
        assert_eq!(HostOs::Debian.brand_foreground(), Some(rgb(0xFF_FFFF)));
        assert_eq!(HostOs::Windows.brand_foreground(), Some(rgb(0xFF_FFFF)));
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
    fn a_new_session_has_not_been_probed_yet() {
        let session = Session::new(
            SessionId(1),
            SessionDraft::new("s", "h", 22, "root", AuthKind::Password, None),
        );
        assert_eq!(session.os, None);
    }

    #[test]
    fn a_password_is_the_default_and_old_spellings_read_as_one() {
        assert_eq!(AuthKind::default(), AuthKind::Password);
        for auth in [AuthKind::Password, AuthKind::NoPassword] {
            assert_eq!(AuthKind::from_stored(auth.as_str()), auth);
        }
        // What versions before 10 wrote.
        assert_eq!(AuthKind::from_stored("auto"), AuthKind::Password);
        assert_eq!(AuthKind::from_stored("key"), AuthKind::Password);
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
