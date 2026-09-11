use gpui_kit::SharedString;
use serde::Deserialize;

/// Stable identity of a session. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
pub struct SessionId(pub u64);

/// Stable identity of a session group (a folder in the session tree).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
pub struct GroupId(pub u64);

/// How a session authenticates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AuthKind {
    Password,
    #[default]
    Key,
}

impl AuthKind {
    /// Every kind, in the order the session form lists them.
    pub const ALL: [AuthKind; 2] = [AuthKind::Password, AuthKind::Key];

    pub fn label(self) -> &'static str {
        match self {
            AuthKind::Password => "密码",
            AuthKind::Key => "密钥",
        }
    }

    /// The stored spelling. Kept separate from `label` so translating the UI
    /// cannot rewrite what is already in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::Password => "password",
            AuthKind::Key => "key",
        }
    }

    /// Parse a stored spelling, falling back to the default for anything a
    /// newer version might have written.
    pub fn from_stored(value: &str) -> Self {
        match value {
            "password" => AuthKind::Password,
            _ => AuthKind::Key,
        }
    }
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
    pub group: Option<GroupId>,
    pub state: ConnectionState,
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
            group: draft.group,
            state: ConnectionState::Disconnected,
        }
    }

    /// `user@host:port`, as shown in the status bar.
    pub fn address(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }

    /// The editable fields, for pre-filling the session form.
    pub fn draft(&self) -> SessionDraft {
        SessionDraft {
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth: self.auth,
            group: self.group,
        }
    }
}

/// A folder in the session tree. Groups nest: `parent` is `None` for a
/// top-level folder.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SessionGroup {
    pub id: GroupId,
    pub name: SharedString,
    pub parent: Option<GroupId>,
}

impl SessionGroup {
    pub fn new(id: GroupId, draft: GroupDraft) -> Self {
        Self {
            id,
            name: draft.name,
            parent: draft.parent,
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
            group,
        }
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
