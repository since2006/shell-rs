//! The saved hosts, groups and credentials as the app sees and edits them:
//! plain values, read from the store ([`crate::HostStore`]) and handed
//! back to it.
//!
//! Ids are the store's: valid while the core runs, the same number maybe
//! another host's after the app starts again. What the app keeps across
//! launches names a host by its `public_id`.

use std::sync::Arc;

use shellrs_hosts::{self as hosts, GroupId, HostId};

use crate::login::ProxyKind;

/// A saved host.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Host {
    pub id: u64,
    /// Its identity outside the app, as the desktop's 复制 ID gives it:
    /// never reused, and the same on every device sync brings it to.
    pub public_id: String,
    pub name: String,
    pub protocol: HostProtocol,
    /// The host name or IP address it is reached at.
    pub address: String,
    pub port: u16,
    /// The user it logs in as: a credential's, for a host using one. A
    /// Telnet host's may be empty.
    pub user: String,
    pub auth: HostAuth,
    pub route: HostRoute,
    /// The group it is in; `None` at the top of the tree.
    pub group: Option<u64>,
    pub notes: String,
    /// The system it runs, as the desktop found it on connecting; `None`
    /// until then.
    pub os: Option<HostOs>,
    /// The colour of its mark when `os` has no brand colour, `0xRRGGBB`:
    /// picked by address from a fixed palette, the same as the desktop's.
    pub mark_color: u32,
}

/// How a host is reached. Serial hosts, which stay on the computer they
/// were made on, never come here.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostProtocol {
    Ssh,
    /// Its automatic login waits for these prompts; empty for the usual
    /// ones (`login:`, `Password:`).
    Telnet {
        user_prompt: String,
        password_prompt: String,
    },
}

/// How a host logs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostAuth {
    /// Its own password, saved or asked for at each connection. A Telnet
    /// host's automatic login.
    Password,
    /// Nothing typed: only a server that asks for nothing lets it in, a
    /// phone having no agent and no default keys. A Telnet host without
    /// automatic login.
    NoPassword,
    /// A saved credential, whose user it logs in as.
    Credential { id: u64 },
}

/// How a connection reaches a host.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostRoute {
    Direct,
    /// Through these SSH hosts, in order. `None` is one since deleted: the
    /// host fails to connect, saying so, until the route is edited.
    Jump {
        hosts: Vec<Option<u64>>,
    },
    Proxy {
        proxy: HostProxy,
    },
}

/// An HTTP or SOCKS5 proxy. One with a user has its password saved, as
/// the host's is.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostProxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
}

/// The system a host runs.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostOs {
    /// Never changes: `ubuntu`, `debian`, `macos`, `windows` and so on, as
    /// the desktop names its icons. One this interface does not know yet
    /// may come.
    pub key: String,
    /// Its own name, such as `Ubuntu`.
    pub name: String,
    /// The brand's colour behind the mark, `0xRRGGBB`; `None` for a mark
    /// drawn in the theme's colour.
    pub brand_color: Option<u32>,
    /// The mark itself, on `brand_color`.
    pub brand_foreground: Option<u32>,
}

/// A host as the form edits it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct HostDraft {
    /// Empty takes the address.
    pub name: String,
    pub protocol: HostProtocol,
    pub address: String,
    pub port: u16,
    /// Empty is `root`, except for a Telnet host. A host using a
    /// credential takes the credential's.
    pub user: String,
    pub auth: HostAuth,
    pub route: HostRoute,
    pub group: Option<u64>,
    pub notes: String,
}

/// A group of hosts. Groups nest.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Group {
    pub id: u64,
    pub name: String,
    pub parent: Option<u64>,
    /// Whether the tree shows what is in it.
    pub expanded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct GroupDraft {
    pub name: String,
    pub parent: Option<u64>,
}

/// A login several hosts can share: a password, a private key, or an SSH
/// agent's keys (which a phone has none of).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Credential {
    pub id: u64,
    pub name: String,
    pub kind: CredentialKind,
    pub user: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CredentialKind {
    Password,
    Key,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CredentialDraft {
    pub name: String,
    pub kind: CredentialKind,
    /// Empty is `root`.
    pub user: String,
}

/// A place in the host tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TreeNode {
    Group { id: u64 },
    Host { id: u64 },
}

/// Where a node is dropped: groups and hosts each keep their own order
/// within a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TreeDrop {
    Before {
        node: TreeNode,
    },
    After {
        node: TreeNode,
    },
    Into {
        group: u64,
    },
    /// At the top of the tree.
    Top,
}

/// What goes with a host or a group deleted, for the question before, as
/// the desktop asks it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct Dependents {
    /// The hosts in a group, however deep, serial ones the app does not
    /// list among them; 0 for a host.
    pub hosts: u32,
    /// The groups in a group, however deep.
    pub subgroups: u32,
    /// The desktop's port forwards via the hosts deleted, which go with
    /// them.
    pub forwards: u32,
    /// The hosts left that jump through one deleted: they connect again
    /// once their route names another.
    pub jump_users: u32,
}

/// What saving does to a saved password or passphrase. It carries one, so
/// it never prints.
#[derive(uniffi::Enum)]
pub enum SecretChange {
    /// Leave it as it is. A host's password moves with the host when its
    /// address, port or user changes.
    Keep,
    /// Delete it: the next connection asks.
    Clear,
    Set {
        value: String,
    },
}

/// What saving a key credential does to its private key. It carries one,
/// so it never prints.
#[derive(uniffi::Enum)]
pub enum KeyChange {
    /// The key it has, its saved passphrase as `passphrase` says.
    Keep { passphrase: SecretChange },
    /// A private key's text, pasted or read from a file the user picked
    /// (see [`check_private_key`]). The core keeps a copy among its own
    /// keys, which is what syncs. Its passphrase is saved when it is
    /// encrypted.
    Text {
        text: String,
        passphrase: Option<String>,
    },
    /// A key [`crate::HostStore::generate_key`] made, encrypted with
    /// `passphrase` when there is one.
    Generated {
        key: Arc<NewKey>,
        passphrase: Option<String>,
    },
}

/// The kinds of key [`crate::HostStore::generate_key`] makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum KeyAlgorithm {
    Ed25519,
    /// RSA, 4096 bits, for servers too old for Ed25519.
    Rsa,
}

/// A key just generated, not saved until a credential is with it.
#[derive(uniffi::Object)]
pub struct NewKey {
    pub(crate) key: hosts::GeneratedKey,
}

#[uniffi::export]
impl NewKey {
    /// Its public key as a line of `authorized_keys`, ending in `comment`
    /// (the credential's name, as the desktop puts it).
    pub fn public_key(&self, comment: String) -> String {
        self.key.public_key_line(&comment)
    }
}

/// What a pasted private key is.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PrivateKeyInfo {
    /// It needs its passphrase to be used.
    pub encrypted: bool,
    /// Its public key, when it can be told without the passphrase.
    pub public_key: Option<String>,
}

/// Whether `text` is a private key ShellRS can log in with, and what kind.
#[uniffi::export]
pub fn check_private_key(text: String) -> Result<PrivateKeyInfo, KeyProblem> {
    let key = hosts::PastedKey::parse(&text).map_err(KeyProblem::from)?;
    Ok(PrivateKeyInfo {
        encrypted: key.is_encrypted(),
        public_key: key.public_key_line(),
    })
}

/// What is wrong with a host as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum HostProblem {
    /// It has no address.
    Address,
    /// Its port is 0.
    Port,
    /// It goes through jump hosts, and lists none.
    NoJumpHosts,
    /// One of its jump hosts has been deleted.
    DeletedJumpHost,
    /// Its proxy has no address.
    ProxyAddress,
    /// Its proxy's port is 0.
    ProxyPort,
}

/// What is wrong with a credential as typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CredentialProblem {
    /// It has no name.
    Name,
    /// A key credential, with no key.
    NoKey,
}

/// What is wrong with a private key's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Error)]
pub enum KeyProblem {
    /// There is nothing.
    Empty,
    /// It is a public key, not a private one.
    PublicKey,
    /// It is no private key ShellRS reads.
    Unreadable,
}

impl std::fmt::Display for KeyProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "no key",
            Self::PublicKey => "a public key",
            Self::Unreadable => "not a private key",
        })
    }
}

impl std::error::Error for KeyProblem {}

impl From<hosts::PastedKeyError> for KeyProblem {
    fn from(error: hosts::PastedKeyError) -> Self {
        match error {
            hosts::PastedKeyError::Empty => Self::Empty,
            hosts::PastedKeyError::PublicKey => Self::PublicKey,
            hosts::PastedKeyError::Unreadable => Self::Unreadable,
        }
    }
}

impl From<hosts::HostDraftError> for HostProblem {
    fn from(error: hosts::HostDraftError) -> Self {
        match error {
            // A serial host's, which never comes here: what it lacks is
            // its address and its port all the same.
            hosts::HostDraftError::Address | hosts::HostDraftError::Device => Self::Address,
            hosts::HostDraftError::Port | hosts::HostDraftError::Baud => Self::Port,
            hosts::HostDraftError::NoJumpHosts => Self::NoJumpHosts,
            hosts::HostDraftError::DeletedJumpHost => Self::DeletedJumpHost,
            hosts::HostDraftError::ProxyAddress => Self::ProxyAddress,
            hosts::HostDraftError::ProxyPort => Self::ProxyPort,
        }
    }
}

impl From<hosts::CredentialDraftError> for CredentialProblem {
    fn from(error: hosts::CredentialDraftError) -> Self {
        match error {
            hosts::CredentialDraftError::Name => Self::Name,
            hosts::CredentialDraftError::KeyPath => Self::NoKey,
        }
    }
}

impl Host {
    pub(crate) fn of(host: &hosts::Host) -> Self {
        Self {
            id: host.id.0,
            public_id: host.public_id.to_string(),
            name: host.name.to_string(),
            protocol: match &host.protocol {
                hosts::Protocol::Telnet(settings) => HostProtocol::Telnet {
                    user_prompt: settings.user_prompt.to_string(),
                    password_prompt: settings.password_prompt.to_string(),
                },
                hosts::Protocol::Ssh | hosts::Protocol::Serial(_) => HostProtocol::Ssh,
            },
            address: host.address.to_string(),
            port: host.port,
            user: host.user.to_string(),
            auth: match (host.credential, host.auth) {
                (Some(id), _) => HostAuth::Credential { id: id.0 },
                (None, hosts::AuthKind::Password) => HostAuth::Password,
                (None, hosts::AuthKind::NoPassword) => HostAuth::NoPassword,
            },
            route: match &host.route {
                hosts::Route::Direct => HostRoute::Direct,
                hosts::Route::Jump(hops) => HostRoute::Jump {
                    hosts: hops.iter().map(|hop| hop.map(|id| id.0)).collect(),
                },
                hosts::Route::Proxy(proxy) => HostRoute::Proxy {
                    proxy: HostProxy {
                        kind: proxy.kind.into(),
                        host: proxy.host.to_string(),
                        port: proxy.port,
                        user: proxy.user.as_ref().map(ToString::to_string),
                    },
                },
            },
            group: host.group.map(|id| id.0),
            notes: host.notes.to_string(),
            os: host.os.map(|os| HostOs {
                key: os.as_str().into(),
                name: os.label().into(),
                brand_color: os.brand_color(),
                brand_foreground: os.brand_foreground(),
            }),
            mark_color: hosts::mark_color(&host.address),
        }
    }
}

impl HostDraft {
    /// The core's draft, not yet checked.
    pub(crate) fn core(self) -> hosts::HostDraft {
        let auth = match self.auth {
            HostAuth::NoPassword => hosts::AuthKind::NoPassword,
            HostAuth::Password | HostAuth::Credential { .. } => hosts::AuthKind::Password,
        };
        let group = self.group.map(GroupId);
        let mut draft =
            hosts::HostDraft::new(self.name, self.address, self.port, self.user, auth, group);
        if let HostProtocol::Telnet {
            user_prompt,
            password_prompt,
        } = self.protocol
        {
            draft.protocol = hosts::Protocol::Telnet(hosts::TelnetSettings {
                user_prompt: user_prompt.into(),
                password_prompt: password_prompt.into(),
            });
        }
        if let HostAuth::Credential { id } = self.auth {
            draft = draft.with_credential(hosts::CredentialId(id));
        }
        let route = match self.route {
            HostRoute::Direct => hosts::Route::Direct,
            HostRoute::Jump { hosts: hops } => {
                hosts::Route::Jump(hops.into_iter().map(|hop| hop.map(HostId)).collect())
            }
            HostRoute::Proxy { proxy } => hosts::Route::Proxy(
                hosts::ProxySettings::new(proxy.kind.into(), proxy.host, proxy.port)
                    .with_user(proxy.user.unwrap_or_default()),
            ),
        };
        draft.with_route(route).with_notes(self.notes)
    }
}

impl Group {
    pub(crate) fn of(group: &hosts::HostGroup) -> Self {
        Self {
            id: group.id.0,
            name: group.name.to_string(),
            parent: group.parent.map(|id| id.0),
            expanded: group.expanded,
        }
    }
}

impl Credential {
    pub(crate) fn of(credential: &hosts::Credential) -> Self {
        Self {
            id: credential.id.0,
            name: credential.name.to_string(),
            kind: credential.kind.into(),
            user: credential.user.to_string(),
        }
    }
}

impl From<hosts::CredentialKind> for CredentialKind {
    fn from(kind: hosts::CredentialKind) -> Self {
        match kind {
            hosts::CredentialKind::Password => Self::Password,
            hosts::CredentialKind::Key => Self::Key,
            hosts::CredentialKind::Agent => Self::Agent,
        }
    }
}

impl From<CredentialKind> for hosts::CredentialKind {
    fn from(kind: CredentialKind) -> Self {
        match kind {
            CredentialKind::Password => Self::Password,
            CredentialKind::Key => Self::Key,
            CredentialKind::Agent => Self::Agent,
        }
    }
}

impl From<KeyAlgorithm> for hosts::KeyAlgorithm {
    fn from(algorithm: KeyAlgorithm) -> Self {
        match algorithm {
            KeyAlgorithm::Ed25519 => Self::Ed25519,
            KeyAlgorithm::Rsa => Self::Rsa,
        }
    }
}

impl From<TreeNode> for hosts::HostNode {
    fn from(node: TreeNode) -> Self {
        match node {
            TreeNode::Group { id } => Self::Group(GroupId(id)),
            TreeNode::Host { id } => Self::Host(HostId(id)),
        }
    }
}

impl From<TreeDrop> for hosts::NodeDrop {
    fn from(drop: TreeDrop) -> Self {
        match drop {
            TreeDrop::Before { node } => Self::Before(node.into()),
            TreeDrop::After { node } => Self::After(node.into()),
            TreeDrop::Into { group } => Self::Into(GroupId(group)),
            TreeDrop::Top => Self::Root,
        }
    }
}
