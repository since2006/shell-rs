//! The words for what the host data says, in the user's language: the
//! labels of its kinds, why a draft cannot be saved, and the search and
//! summaries that go by those labels. The data itself has no words, so the
//! mobile apps word it in theirs.

use std::path::Path;

use gpui_kit::SharedString;

use crate::i18n::t;

use super::{
    Change, Credential, CredentialDraftError, CredentialKind, Failure, FlowControl,
    ForwardDraftError, ForwardKind, ForwardRule, HostDraftError, KeyFileError, OpenError, Parity,
    PastedKeyError, ProtocolKind, SecretError, SnippetDraftError, secrets::SecretRef,
};

/// What a kind is called in the forms and lists.
pub trait KindLabel {
    fn label(&self) -> SharedString;
}

impl KindLabel for CredentialKind {
    fn label(&self) -> SharedString {
        match self {
            CredentialKind::Password => t!("host.credential_kind.password"),
            CredentialKind::Key => t!("host.credential_kind.key"),
            CredentialKind::Agent => "SSH Agent".into(),
        }
    }
}

impl KindLabel for ForwardKind {
    fn label(&self) -> SharedString {
        match self {
            ForwardKind::Local => t!("host.forward_kind.local"),
            ForwardKind::Remote => t!("host.forward_kind.remote"),
            ForwardKind::Dynamic => t!("host.forward_kind.dynamic"),
        }
    }
}

impl KindLabel for ProtocolKind {
    fn label(&self) -> SharedString {
        match self {
            ProtocolKind::Ssh => t!("host.protocol.ssh"),
            ProtocolKind::Telnet => t!("host.protocol.telnet"),
            ProtocolKind::Serial => t!("host.protocol.serial"),
        }
    }
}

impl KindLabel for Parity {
    fn label(&self) -> SharedString {
        match self {
            Parity::None => t!("host.parity.none"),
            Parity::Odd => t!("host.parity.odd"),
            Parity::Even => t!("host.parity.even"),
        }
    }
}

impl KindLabel for FlowControl {
    fn label(&self) -> SharedString {
        match self {
            FlowControl::None => t!("host.flow.none"),
            FlowControl::RtsCts => "RTS/CTS".into(),
            FlowControl::XonXoff => "XON/XOFF".into(),
        }
    }
}

/// What a form says when its draft cannot be saved.
pub trait ErrorMessage {
    fn message(&self) -> SharedString;
}

impl ErrorMessage for HostDraftError {
    fn message(&self) -> SharedString {
        match self {
            HostDraftError::Address => t!("host.draft.address"),
            HostDraftError::Port => t!("host.draft.port"),
            HostDraftError::Device => t!("host.draft.device"),
            HostDraftError::Baud => t!("host.draft.baud"),
            HostDraftError::NoJumpHosts => t!("host.draft.no_jump_hosts"),
            HostDraftError::DeletedJumpHost => t!("host.draft.deleted_jump_host"),
            HostDraftError::ProxyAddress => t!("host.draft.proxy_address"),
            HostDraftError::ProxyPort => t!("host.draft.proxy_port"),
        }
    }
}

impl ErrorMessage for CredentialDraftError {
    fn message(&self) -> SharedString {
        match self {
            CredentialDraftError::Name => t!("host.credential_draft.name"),
            CredentialDraftError::KeyPath => t!("host.credential_draft.key_path"),
        }
    }
}

impl ErrorMessage for ForwardDraftError {
    fn message(&self) -> SharedString {
        match self {
            ForwardDraftError::BindHost => t!("host.forward_draft.bind_host"),
            ForwardDraftError::BindPort => t!("host.forward_draft.bind_port"),
            ForwardDraftError::TargetHost => t!("host.forward_draft.target_host"),
            ForwardDraftError::TargetPort => t!("host.forward_draft.target_port"),
        }
    }
}

impl ErrorMessage for SnippetDraftError {
    fn message(&self) -> SharedString {
        match self {
            SnippetDraftError::Name => t!("host.snippet_draft.name"),
            SnippetDraftError::Command => t!("host.snippet_draft.command"),
        }
    }
}

impl ErrorMessage for PastedKeyError {
    fn message(&self) -> SharedString {
        match self {
            PastedKeyError::Empty => t!("host.pasted_key.empty"),
            PastedKeyError::PublicKey => t!("host.pasted_key.public_key"),
            PastedKeyError::Unreadable => t!("host.pasted_key.unreadable"),
        }
    }
}

impl ErrorMessage for OpenError {
    fn message(&self) -> SharedString {
        match self {
            OpenError::Newer { version, supported } => t!(
                "host.database.newer",
                version = version,
                supported = supported
            ),
            OpenError::TooOld { version } => t!("host.database.too_old", version = version),
            OpenError::Sqlite(error) => error.to_string().into(),
        }
    }
}

impl ErrorMessage for Failure {
    fn message(&self) -> SharedString {
        match self {
            Failure::NotSaved { change, error } => not_saved(*change, error),
            Failure::KeyNotDeleted { error } => t!("host.store.key_not_deleted", error = error),
            Failure::Secret {
                secret,
                writing,
                error,
            } => secret_failure(secret, *writing)(&error.message()),
            Failure::SyncedSecret(error) => {
                t!("host.store.sync.secret_not_saved", error = error.message())
            }
        }
    }
}

impl ErrorMessage for SecretError {
    fn message(&self) -> SharedString {
        match self {
            SecretError::Unavailable => t!("secrets.unavailable"),
            SecretError::NoKeyDir => t!("host.store.no_key_dir"),
            SecretError::Keychain(error) | SecretError::KeyFile(error) => error.clone().into(),
        }
    }
}

impl ErrorMessage for KeyFileError {
    fn message(&self) -> SharedString {
        match self {
            KeyFileError::NoKeyDir => t!("host.store.no_key_dir"),
            KeyFileError::Unreadable => t!("host.store.key_unreadable"),
            KeyFileError::Io(error) => error.to_string().into(),
        }
    }
}

/// That `change` is not in the database, which said `error`.
fn not_saved(change: Change, error: &str) -> SharedString {
    match change {
        Change::NewHost => t!("host.store.not_saved.new_host", error = error),
        Change::SaveHost => t!("host.store.not_saved.save_host", error = error),
        Change::DeleteHost => t!("host.store.not_saved.delete_host", error = error),
        Change::DuplicateHost => t!("host.store.not_saved.duplicate_host", error = error),
        Change::HostOrder => t!("host.store.not_saved.host_order", error = error),
        Change::NewGroup => t!("host.store.not_saved.new_group", error = error),
        Change::SaveGroup => t!("host.store.not_saved.save_group", error = error),
        Change::AddBookmark => t!("host.store.not_saved.add_bookmark", error = error),
        Change::DeleteBookmark => t!("host.store.not_saved.delete_bookmark", error = error),
        Change::BookmarkOrder => t!("host.store.not_saved.bookmark_order", error = error),
        Change::NewForward => t!("host.store.not_saved.new_forward", error = error),
        Change::SaveForward => t!("host.store.not_saved.save_forward", error = error),
        Change::DeleteForward => t!("host.store.not_saved.delete_forward", error = error),
        Change::NewCredential => t!("host.store.not_saved.new_credential", error = error),
        Change::SaveCredential => t!("host.store.not_saved.save_credential", error = error),
        Change::DeleteCredential => t!("host.store.not_saved.delete_credential", error = error),
        Change::GroupExpanded => t!("host.store.not_saved.group_expanded", error = error),
        Change::AllGroupsExpanded => {
            t!("host.store.not_saved.all_groups_expanded", error = error)
        }
        Change::MoveHostNode => t!("host.store.not_saved.move_node", error = error),
        Change::DeleteGroup => t!("host.store.not_saved.delete_group", error = error),
        Change::Recent => t!("host.store.not_saved.recent", error = error),
        Change::HostOs => t!("host.store.not_saved.host_os", error = error),
        Change::SidebarTool => t!("host.store.not_saved.sidebar_tool", error = error),
        Change::NewSnippet => t!("host.store.not_saved.new_snippet", error = error),
        Change::SaveSnippet => t!("host.store.not_saved.save_snippet", error = error),
        Change::DeleteSnippet => t!("host.store.not_saved.delete_snippet", error = error),
        Change::NewSnippetCategory => {
            t!("host.store.not_saved.new_snippet_category", error = error)
        }
        Change::RenameSnippetCategory => t!(
            "host.store.not_saved.rename_snippet_category",
            error = error
        ),
        Change::DeleteSnippetCategory => t!(
            "host.store.not_saved.delete_snippet_category",
            error = error
        ),
        Change::NewTerminalGroup => {
            t!("host.store.not_saved.new_terminal_group", error = error)
        }
        Change::SaveTerminalGroup => {
            t!("host.store.not_saved.save_terminal_group", error = error)
        }
        Change::DeleteTerminalGroup => {
            t!("host.store.not_saved.delete_terminal_group", error = error)
        }
        Change::NewSavedTerminal => {
            t!("host.store.not_saved.new_saved_terminal", error = error)
        }
        Change::SaveSavedTerminal => {
            t!("host.store.not_saved.save_saved_terminal", error = error)
        }
        Change::DeleteSavedTerminal => {
            t!("host.store.not_saved.delete_saved_terminal", error = error)
        }
        Change::MoveTerminalNode => {
            t!("host.store.not_saved.move_terminal_node", error = error)
        }
        Change::SavedTerminalTabs => {
            t!("host.store.not_saved.saved_terminal_tabs", error = error)
        }
        Change::Batch => t!("host.store.not_saved.batch", error = error),
    }
}

/// What a failed keychain write of `secret` says, given the keychain's
/// error: writing it when `writing`, deleting it otherwise.
fn secret_failure(secret: &SecretRef, writing: bool) -> fn(&str) -> SharedString {
    match (secret, writing) {
        (SecretRef::Password { .. }, true) => {
            |error| t!("host.store.secret.password_write", error = error)
        }
        (SecretRef::Password { .. }, false) => {
            |error| t!("host.store.secret.password_delete", error = error)
        }
        (SecretRef::Passphrase { .. }, true) => {
            |error| t!("host.store.secret.passphrase_write", error = error)
        }
        (SecretRef::Passphrase { .. }, false) => {
            |error| t!("host.store.secret.passphrase_delete", error = error)
        }
        (SecretRef::Credential { .. }, true) => {
            |error| t!("host.store.secret.credential_write", error = error)
        }
        (SecretRef::Credential { .. }, false) => {
            |error| t!("host.store.secret.credential_delete", error = error)
        }
        (SecretRef::Proxy { .. }, true) => {
            |error| t!("host.store.secret.proxy_write", error = error)
        }
        (SecretRef::Proxy { .. }, false) => {
            |error| t!("host.store.secret.proxy_delete", error = error)
        }
        // Kept in memory, which does not fail.
        (SecretRef::Temporary { .. }, _) => {
            |error| t!("host.store.secret.temporary", error = error)
        }
        (SecretRef::SyncKeyring | SecretRef::SyncLogin, _) => {
            |error| t!("host.store.secret.sync_keyring", error = error)
        }
    }
}

/// What a keychain read or write that failed says. The system keychain's
/// own failures come worded already; the data layer's are typed.
pub fn secret_error(error: &anyhow::Error) -> SharedString {
    SecretError::of(error).message()
}

/// What a credential logs in as and with: `root · 密码`, or
/// `deploy · id_ed25519` for a key file. A key ShellRS keeps in `key_dir`
/// has a random file name nobody knows it by, so it is just `deploy · 密钥`.
pub fn credential_summary(credential: &Credential, key_dir: Option<&Path>) -> String {
    let with = match (credential.kind, credential.key_path.as_deref()) {
        (CredentialKind::Key, Some(path)) if !credential.keeps_key_in(key_dir) => {
            file_name(path).to_string()
        }
        (kind, _) => kind.label().to_string(),
    };
    format!("{} · {with}", credential.user)
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
    let kind = credential.kind.label();
    [
        Some(credential.name.as_ref()),
        Some(credential.user.as_ref()),
        Some(kind.as_ref()),
        credential.key_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.to_lowercase().contains(&query))
}

/// Whether a rule matches what was typed into the forward list's search box:
/// its name, its addresses, or the name of the host it goes through.
pub fn matches_forward_query(rule: &ForwardRule, host_name: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    [
        rule.name.to_string(),
        rule.summary(),
        rule.kind.label().to_string(),
        host_name.to_string(),
    ]
    .iter()
    .any(|text| text.to_lowercase().contains(&query))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{
        CredentialDraft, CredentialId, DEFAULT_BIND_HOST, ForwardDraft, ForwardEndpoint, ForwardId,
        HostId,
    };

    #[test]
    fn a_refused_database_says_which_shellrs_wrote_it() {
        let newer = OpenError::Newer {
            version: 21,
            supported: 20,
        };
        assert_eq!(
            newer.message(),
            "数据库来自更新版本的 ShellRS（结构版本 21，本版本支持到 20），请安装最新版本"
        );
        let old = OpenError::TooOld { version: 12 };
        assert_eq!(
            old.message(),
            "数据库来自 ShellRS 的早期开发版本（结构版本 12），无法升级"
        );
    }

    #[test]
    fn a_keychain_that_is_not_there_says_so() {
        use crate::host::secrets::{NoSecretStore, SecretRef, SecretStore as _};

        let error = NoSecretStore
            .set(&SecretRef::password("root", "10.0.0.1", 22), "hunter2")
            .unwrap_err();
        assert_eq!(secret_error(&error), "系统钥匙串不可用，密码无法保存");
        let other = anyhow::anyhow!("钥匙串被锁定");
        assert_eq!(secret_error(&other), "钥匙串被锁定");
    }

    #[test]
    fn a_draft_error_says_what_to_fix_in_either_language() {
        crate::i18n::isolate_thread();
        assert_eq!(SnippetDraftError::Name.message(), "请输入名称");
        assert_eq!(SnippetDraftError::Command.message(), "请输入命令");
        assert_eq!(ForwardKind::Local.label(), "本地转发");
        crate::i18n::set_locale("en");
        assert_eq!(SnippetDraftError::Name.message(), "Enter a name");
        assert_eq!(ForwardKind::Local.label(), "Local");
        assert_eq!(
            OpenError::TooOld { version: 12 }.message(),
            "The database is from an early development version of ShellRS (schema version 12) and can’t be upgraded."
        );
        assert_eq!(FlowControl::RtsCts.label(), "RTS/CTS");
    }

    #[test]
    fn the_summary_names_the_user_and_what_it_logs_in_with() {
        let password = Credential::new(
            CredentialId(1),
            CredentialDraft::new("运维", CredentialKind::Password, "root"),
        );
        assert_eq!(credential_summary(&password, None), "root · 密码");
        let key = Credential::new(
            CredentialId(2),
            CredentialDraft::new("部署", CredentialKind::Key, "deploy")
                .with_key_path("/Users/me/.ssh/id_ed25519"),
        );
        assert_eq!(credential_summary(&key, None), "deploy · id_ed25519");
        assert_eq!(
            credential_summary(&key, Some(Path::new("/Users/me/.ssh"))),
            "deploy · 密钥",
            "a key ShellRS keeps is not known by its file name"
        );
        assert_eq!(
            credential_summary(&key, Some(Path::new("/Users/me/Library/shellrs/keys"))),
            "deploy · id_ed25519"
        );
        let agent = Credential::new(
            CredentialId(3),
            CredentialDraft::new("agent", CredentialKind::Agent, "me"),
        );
        assert_eq!(credential_summary(&agent, None), "me · SSH Agent");
    }

    #[test]
    fn the_credential_search_looks_at_the_name_the_user_the_kind_and_the_key_file() {
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

    #[test]
    fn the_forward_search_looks_at_the_name_the_addresses_and_the_host() {
        let draft = ForwardDraft::new(
            ForwardKind::Local,
            HostId(1),
            ForwardEndpoint::new(DEFAULT_BIND_HOST, 8080),
            Some(ForwardEndpoint::new("db.internal", 3306)),
        );
        let rule = ForwardRule::new(ForwardId(1), draft.with_name("数据库"));
        assert!(matches_forward_query(&rule, "web-01", ""));
        assert!(matches_forward_query(&rule, "web-01", "数据"));
        assert!(matches_forward_query(&rule, "web-01", "3306"));
        assert!(matches_forward_query(&rule, "web-01", "WEB"));
        assert!(matches_forward_query(&rule, "web-01", "本地"));
        assert!(!matches_forward_query(&rule, "web-01", "redis"));
    }
}
