//! Saved logins that hosts share: a user name with a password, a private key
//! file or the SSH agent. They live with the sessions because hosts refer to
//! them and they are stored in the same database; the `credential` module
//! that lists and edits them depends on this one, never the reverse.

use std::fmt;
use std::path::Path;

use gpui_kit::SharedString;

use crate::secrets::SecretRef;

use super::PublicId;

/// Stable identity of a credential. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CredentialId(pub u64);

/// What a credential logs in with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum CredentialKind {
    /// A password kept in the system keychain under the credential's own
    /// entry.
    #[default]
    Password,
    /// A private key file on this machine; its passphrase, if any, is kept
    /// under the file's path like every other key's.
    Key,
    /// Whatever keys the SSH agent holds.
    Agent,
}

impl CredentialKind {
    /// Every kind, in the order the form lists them.
    pub const ALL: [CredentialKind; 3] = [
        CredentialKind::Password,
        CredentialKind::Key,
        CredentialKind::Agent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CredentialKind::Password => "密码",
            CredentialKind::Key => "密钥",
            CredentialKind::Agent => "SSH Agent",
        }
    }

    /// The stored spelling. Kept separate from `label` so translating the UI
    /// cannot rewrite what is already in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            CredentialKind::Password => "password",
            CredentialKind::Key => "key",
            CredentialKind::Agent => "agent",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "password" => Some(CredentialKind::Password),
            "key" => Some(CredentialKind::Key),
            "agent" => Some(CredentialKind::Agent),
            _ => None,
        }
    }
}

/// Why a draft cannot be saved. `Display` is the text the form shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialDraftError {
    Name,
    KeyPath,
}

impl fmt::Display for CredentialDraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            CredentialDraftError::Name => "请输入名称",
            CredentialDraftError::KeyPath => "密钥凭据需要选择私钥文件",
        })
    }
}

impl std::error::Error for CredentialDraftError {}

/// The user name a login falls back to when none is given, as in the host
/// form.
pub const DEFAULT_USER: &str = "root";

/// The values the credential form commits. Never holds a secret: those go
/// straight to the keychain.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CredentialDraft {
    pub name: SharedString,
    pub kind: CredentialKind,
    pub user: SharedString,
    /// The private key file. Set exactly for [`CredentialKind::Key`].
    pub key_path: Option<SharedString>,
}

impl CredentialDraft {
    pub fn new(
        name: impl Into<SharedString>,
        kind: CredentialKind,
        user: impl Into<SharedString>,
    ) -> Self {
        Self {
            name: name.into(),
            kind,
            user: user.into(),
            key_path: None,
        }
    }

    pub fn with_key_path(mut self, path: impl Into<SharedString>) -> Self {
        let path = path.into();
        self.key_path = (!path.trim().is_empty()).then_some(path);
        self
    }

    /// The draft with surrounding blanks removed, an empty user name taken
    /// as [`DEFAULT_USER`] and a key file kept only by a key credential, or
    /// the first thing wrong with it.
    pub fn validated(mut self) -> Result<Self, CredentialDraftError> {
        self.name = self.name.trim().to_string().into();
        if self.name.is_empty() {
            return Err(CredentialDraftError::Name);
        }
        let user = self.user.trim();
        self.user = if user.is_empty() {
            DEFAULT_USER.into()
        } else {
            user.to_string().into()
        };
        self.key_path = match self.kind {
            CredentialKind::Key => Some(
                self.key_path
                    .map(|path| path.trim().to_string())
                    .filter(|path| !path.is_empty())
                    .ok_or(CredentialDraftError::KeyPath)?
                    .into(),
            ),
            CredentialKind::Password | CredentialKind::Agent => None,
        };
        Ok(self)
    }
}

/// A saved credential.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Credential {
    pub id: CredentialId,
    /// Names the credential's password in the keychain. Random rather than
    /// derived from `id`, which can come back after a restart: a new
    /// credential must never find the password an old one left behind.
    pub keychain_id: PublicId,
    pub name: SharedString,
    pub kind: CredentialKind,
    pub user: SharedString,
    pub key_path: Option<SharedString>,
    /// Position in the credential list.
    pub sort_order: i64,
}

impl Credential {
    /// A credential with a fresh keychain id. The store makes sure it is not
    /// one another credential already has.
    pub fn new(id: CredentialId, draft: CredentialDraft) -> Self {
        Self {
            id,
            keychain_id: PublicId::generate(),
            name: draft.name,
            kind: draft.kind,
            user: draft.user,
            key_path: draft.key_path,
            sort_order: 0,
        }
    }

    /// The editable fields, for pre-filling the credential form.
    pub fn draft(&self) -> CredentialDraft {
        CredentialDraft {
            name: self.name.clone(),
            kind: self.kind,
            user: self.user.clone(),
            key_path: self.key_path.clone(),
        }
    }

    /// Where a password credential keeps its password.
    pub fn password_secret(&self) -> SecretRef {
        SecretRef::credential(self.keychain_id.as_str())
    }

    /// What the credential logs in as and with: `root · 密码`, or
    /// `deploy · id_ed25519` for a key.
    pub fn summary(&self) -> String {
        let with = match (self.kind, self.key_path.as_deref()) {
            (CredentialKind::Key, Some(path)) => file_name(path).to_string(),
            (kind, _) => kind.label().to_string(),
        };
        format!("{} · {with}", self.user)
    }
}

/// The last component of a key file's path, which is how people know their
/// keys apart.
fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

/// Whether a credential matches what was typed into the credential list's
/// search box: its name, user, kind or key file.
pub fn matches_credential_query(credential: &Credential, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    [
        Some(credential.name.as_ref()),
        Some(credential.user.as_ref()),
        Some(credential.kind.label()),
        credential.key_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(&query))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_the_database_spelling() {
        for kind in CredentialKind::ALL {
            assert_eq!(CredentialKind::from_stored(kind.as_str()), Some(kind));
        }
        assert_eq!(CredentialKind::from_stored("otp"), None);
    }

    #[test]
    fn a_draft_needs_a_name_and_a_key_file_for_a_key() {
        let saved = CredentialDraft::new("  运维  ", CredentialKind::Password, "  ")
            .validated()
            .unwrap();
        assert_eq!(saved.name.as_ref(), "运维");
        assert_eq!(saved.user.as_ref(), DEFAULT_USER);

        assert_eq!(
            CredentialDraft::new(" ", CredentialKind::Password, "root").validated(),
            Err(CredentialDraftError::Name)
        );
        assert_eq!(
            CredentialDraft::new("部署", CredentialKind::Key, "deploy").validated(),
            Err(CredentialDraftError::KeyPath)
        );
        let key = CredentialDraft::new("部署", CredentialKind::Key, "deploy")
            .with_key_path(" /tmp/id_ed25519 ")
            .validated()
            .unwrap();
        assert_eq!(key.key_path.as_deref(), Some("/tmp/id_ed25519"));
    }

    #[test]
    fn only_a_key_credential_keeps_a_key_file() {
        for kind in [CredentialKind::Password, CredentialKind::Agent] {
            let draft = CredentialDraft::new("x", kind, "root")
                .with_key_path("/tmp/id_ed25519")
                .validated()
                .unwrap();
            assert_eq!(draft.key_path, None);
        }
    }

    #[test]
    fn the_summary_names_the_user_and_what_it_logs_in_with() {
        let password = Credential::new(
            CredentialId(1),
            CredentialDraft::new("运维", CredentialKind::Password, "root"),
        );
        assert_eq!(password.summary(), "root · 密码");
        let key = Credential::new(
            CredentialId(2),
            CredentialDraft::new("部署", CredentialKind::Key, "deploy")
                .with_key_path("/Users/me/.ssh/id_ed25519"),
        );
        assert_eq!(key.summary(), "deploy · id_ed25519");
        let agent = Credential::new(
            CredentialId(3),
            CredentialDraft::new("agent", CredentialKind::Agent, "me"),
        );
        assert_eq!(agent.summary(), "me · SSH Agent");
    }

    #[test]
    fn each_credential_has_its_own_keychain_entry() {
        let draft = CredentialDraft::new("运维", CredentialKind::Password, "root");
        let one = Credential::new(CredentialId(1), draft.clone());
        let other = Credential::new(CredentialId(1), draft);
        assert_ne!(one.password_secret(), other.password_secret());
        assert_eq!(
            one.password_secret().account(),
            format!("credential:{}", one.keychain_id)
        );
    }

    #[test]
    fn the_search_looks_at_the_name_the_user_the_kind_and_the_key_file() {
        let credential = Credential::new(
            CredentialId(1),
            CredentialDraft::new("生产部署", CredentialKind::Key, "deploy")
                .with_key_path("/Users/me/.ssh/id_prod"),
        );
        assert!(matches_credential_query(&credential, ""));
        assert!(matches_credential_query(&credential, "生产"));
        assert!(matches_credential_query(&credential, "DEPLOY"));
        assert!(matches_credential_query(&credential, "密钥"));
        assert!(matches_credential_query(&credential, "id_prod"));
        assert!(!matches_credential_query(&credential, "root"));
    }
}
