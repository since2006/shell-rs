use gpui_kit::{Rgba, SharedString, rgb};
use serde::Deserialize;

use crate::secrets::SecretRef;

/// Stable identity of a session. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
pub struct SessionId(pub u64);

/// Stable identity of a session group (a folder in the session tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
pub struct GroupId(pub u64);

/// How a session authenticates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AuthKind {
    #[default]
    Auto,
    Password,
    Key,
}

impl AuthKind {
    /// Every kind, in the order the session form lists them.
    pub const ALL: [AuthKind; 3] = [AuthKind::Auto, AuthKind::Password, AuthKind::Key];

    pub fn label(self) -> &'static str {
        match self {
            AuthKind::Auto => "自动",
            AuthKind::Password => "密码",
            AuthKind::Key => "私钥文件",
        }
    }

    /// The stored spelling. Kept separate from `label` so translating the UI
    /// cannot rewrite what is already in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::Auto => "auto",
            AuthKind::Password => "password",
            AuthKind::Key => "key",
        }
    }

    /// Parse a stored spelling, falling back to the default for anything a
    /// newer version might have written.
    pub fn from_stored(value: &str) -> Self {
        match value {
            "auto" => AuthKind::Auto,
            "password" => AuthKind::Password,
            "key" => AuthKind::Key,
            _ => AuthKind::Auto,
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
    pub name: SharedString,
    pub host: SharedString,
    pub port: u16,
    pub user: SharedString,
    pub auth: AuthKind,
    pub key_path: Option<SharedString>,
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
    pub fn new(id: SessionId, draft: SessionDraft) -> Self {
        Self {
            id,
            name: draft.name,
            host: draft.host,
            port: draft.port,
            user: draft.user,
            auth: draft.auth,
            key_path: draft.key_path,
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
            key_path: self.key_path.clone(),
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
    pub key_path: Option<SharedString>,
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
            key_path: None,
            group,
        }
    }

    /// The keychain entry this draft would log in with. Matches
    /// [`Session::password_secret`] once the draft is applied.
    pub fn password_secret(&self) -> SecretRef {
        SecretRef::password(self.user.as_ref(), self.host.as_ref(), self.port)
    }

    /// Set the private key used by [`AuthKind::Key`]. Keeping this as a
    /// builder preserves the existing six-argument constructor for callers.
    pub fn with_key_path(mut self, path: impl Into<SharedString>) -> Self {
        let path = path.into();
        self.key_path = (!path.trim().is_empty()).then_some(path);
        self
    }

    pub(crate) fn with_optional_key_path(mut self, path: Option<String>) -> Self {
        self.key_path = path
            .filter(|path| !path.trim().is_empty())
            .map(SharedString::from);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            SessionDraft::new("s", "h", 22, "root", AuthKind::Auto, None),
        );
        assert_eq!(session.os, None);
    }

    #[test]
    fn auto_is_the_default_and_key_path_is_opt_in() {
        assert_eq!(AuthKind::default(), AuthKind::Auto);
        let draft = SessionDraft::new("server", "host", 22, "me", AuthKind::Key, None)
            .with_key_path("/tmp/id_ed25519");
        assert_eq!(draft.key_path.as_deref(), Some("/tmp/id_ed25519"));
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
