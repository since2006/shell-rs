//! ShellRS for the mobile apps: the SSH core (`shellrs-ssh`) and the hosts
//! core (`shellrs-hosts`) behind an interface Swift and Kotlin bindings are
//! generated from (UniFFI). This is the one interface the apps rely on,
//! kept stable; the cores under it change as the desktop app needs.
//!
//! A [`Core`] per data folder holds the runtime everything runs on, the
//! saved hosts ([`HostStore`]) and sync ([`SyncService`]). A [`Connection`]
//! logs in once and carries any number of terminals ([`Terminal`]) and
//! SFTP sessions ([`SftpSession`]). Each object says what happens through
//! `next_event()`, which the app awaits in a loop: nothing calls back into
//! the app but its [`Keychain`]. Nothing here is in words: failures are
//! typed, and the app words them in its own language.

#[cfg(target_os = "android")]
mod android;
mod assistant;
mod connection;
mod core;
mod events;
mod failure;
mod hosts;
mod login;
mod prompt;
mod secrets;
mod sftp;
mod store;
mod sync;
mod task;
mod terminal;
mod update;

pub use assistant::{
    AiPreset, AiPresetService, AiProtocol, AiProvider, AiProviderDraft, AiProviderProblem,
    Approval, AssistantCall, AssistantContext, AssistantError, AssistantEvent, AssistantFailure,
    AssistantMessage, AssistantSession, AssistantStart, AssistantToolCall, AssistantToolResult,
    AssistantUsage, CallStatus, ConversationInfo, MessageRole, ProviderErrorKind, TurnEnd,
    ai_common_models, ai_endpoint, ai_preset_of, ai_preset_service, ai_presets,
    assistant_command_kinds,
};
pub use connection::{Connection, ConnectionEvent, ExecOutput};
pub use core::{Core, CoreConfig, CoreError};
pub use failure::*;
pub use hosts::{
    Credential, CredentialDraft, CredentialKind, CredentialProblem, Dependents, Group, GroupDraft,
    Host, HostAuth, HostDraft, HostOs, HostProblem, HostProtocol, HostProxy, HostRoute,
    KeyAlgorithm, KeyChange, KeyProblem, NewKey, PrivateKeyInfo, SecretChange, TreeDrop, TreeNode,
    check_private_key,
};
pub use login::{JumpHost, Login, LoginMethod, Proxy, ProxyKind, Route};
pub use prompt::{Prompt, PromptField, PromptKind, PromptReply};
pub use secrets::{Keychain, KeychainError, SecretRef, account_for};
pub use sftp::{
    DirectoryEntry, DirectoryListing, EntryKind, SftpDisconnect, SftpEvent, SftpSession,
    TransferAnswer, TransferChoice, TransferDetail, TransferDirection, TransferOutcome,
    TransferPhase, TransferProgress, TransferQuestion, TransferQuestionKind,
};
pub use store::{
    ChangeKind, HostStore, SavedSecret, SecretProblem, StoreError, StoreEvent, StoreFailure,
};
pub use sync::{
    Attention, CheckedLogin, LoginProblem, PasswordProblem, S3Login, StorageProblem, SyncDevice,
    SyncFailure, SyncLocation, SyncOutcome, SyncService, SyncStatus, check_new_sync_password,
    check_s3_login, check_sync_password, check_webdav_login, min_sync_password_length,
    random_sync_password,
};
pub use terminal::{Terminal, TerminalEvent, TerminalOptions};
pub use update::{
    UpdateChannel, UpdateFailure, UpdateOffer, UpdatePackage, UpdateRelease,
    android_update_manifest_url, check_android_update,
};

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    /// What every Kotlin exception has already, from `Throwable`: a field
    /// of an error by one of these names does not compile there.
    const THROWABLE_MEMBERS: &[&str] = &[
        "message",
        "cause",
        "localized_message",
        "stack_trace",
        "suppressed",
    ];

    /// The errors' fields (a `uniffi::Error` becomes a Kotlin exception)
    /// keep clear of `Throwable`'s members. Swift does not mind, so only
    /// the Kotlin check would find one, on a computer with `kotlinc`.
    #[test]
    fn no_error_field_is_named_like_a_member_of_throwable() {
        let mut clashes = Vec::new();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in fs::read_dir(&source).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            let mut in_error = false;
            for (number, line) in text.lines().enumerate() {
                if line.starts_with("#[derive(") && line.contains("uniffi::Error") {
                    in_error = true;
                } else if in_error && line == "}" {
                    in_error = false;
                } else if in_error {
                    let field = line.trim_start().split(':').next().unwrap_or_default();
                    if line.contains(':') && THROWABLE_MEMBERS.contains(&field) {
                        clashes.push(format!("{}:{}: {line}", path.display(), number + 1));
                    }
                }
            }
        }
        assert!(clashes.is_empty(), "{clashes:#?}");
    }
}
