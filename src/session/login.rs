//! How a connection logs in to a host, with the credential the host uses, if
//! any, already looked up. The store builds one on the UI thread and hands it
//! to the terminal, SFTP, forward and CLI workers, which cannot read the
//! store themselves.

use std::path::PathBuf;

use crate::secrets::SecretRef;

use super::{AuthKind, Credential, CredentialKind, Session};

/// Which authentication methods a login tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginMethod {
    /// The SSH agent, then the default keys in `~/.ssh`, then a password.
    Auto,
    Password,
    /// One private key file.
    Key,
    /// Only the SSH agent's keys.
    Agent,
}

/// Everything a login needs. Two equal values log in the same way, which is
/// how the store tells whether an edit has to reconnect a host: it holds
/// nothing that only changes what the user sees, such as a name.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionLogin {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub method: LoginMethod,
    /// The private key of [`LoginMethod::Key`], and `None` for every other
    /// method.
    pub key_path: Option<PathBuf>,
    /// The keychain entry the password step reads: the endpoint's own, or a
    /// password credential's. Key and agent logins keep the endpoint's for a
    /// server that asks for a password after the key.
    pub password: SecretRef,
}

impl SessionLogin {
    /// A login typed into the host form rather than taken from a credential.
    pub fn manual(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        auth: AuthKind,
        key_path: Option<PathBuf>,
    ) -> Self {
        let host = host.into();
        let user = user.into();
        let (method, key_path) = match auth {
            AuthKind::Auto => (LoginMethod::Auto, None),
            AuthKind::Password => (LoginMethod::Password, None),
            AuthKind::Key => (LoginMethod::Key, key_path),
        };
        Self {
            password: SecretRef::password(&user, &host, port),
            host,
            port,
            user,
            method,
            key_path,
        }
    }

    /// A login through `credential` to `host:port`.
    pub fn with_credential(host: impl Into<String>, port: u16, credential: &Credential) -> Self {
        let host = host.into();
        let user = credential.user.to_string();
        let (method, key_path, password) = match credential.kind {
            CredentialKind::Password => (LoginMethod::Password, None, credential.password_secret()),
            CredentialKind::Key => (
                LoginMethod::Key,
                credential.key_path.as_deref().map(PathBuf::from),
                SecretRef::password(&user, &host, port),
            ),
            CredentialKind::Agent => (
                LoginMethod::Agent,
                None,
                SecretRef::password(&user, &host, port),
            ),
        };
        Self {
            host,
            port,
            user,
            method,
            key_path,
            password,
        }
    }

    /// How `session` logs in, given the credential it uses. `credential` is
    /// ignored unless it is the one the session names, so a session whose
    /// credential has gone logs in with what the form holds.
    pub fn of(session: &Session, credential: Option<&Credential>) -> Self {
        match credential.filter(|credential| session.credential == Some(credential.id)) {
            Some(credential) => {
                Self::with_credential(session.host.as_ref(), session.port, credential)
            }
            None => Self::manual(
                session.host.as_ref(),
                session.port,
                session.user.as_ref(),
                session.auth,
                session.key_path.as_deref().map(PathBuf::from),
            ),
        }
    }

    /// `user@host:port`, for messages and as the key of resumable transfers.
    pub fn endpoint(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CredentialDraft, CredentialId, SessionDraft, SessionId};
    use super::*;

    fn session(auth: AuthKind) -> Session {
        Session::new(
            SessionId(1),
            SessionDraft::new("web", "10.0.0.1", 22, "root", auth, None),
        )
    }

    fn credential(kind: CredentialKind) -> Credential {
        let draft = CredentialDraft::new("运维", kind, "deploy");
        let draft = match kind {
            CredentialKind::Key => draft.with_key_path("/tmp/id_deploy"),
            _ => draft,
        };
        Credential::new(CredentialId(7), draft)
    }

    fn using(kind: CredentialKind) -> (Session, Credential) {
        let credential = credential(kind);
        let mut session = session(AuthKind::Auto);
        session.credential = Some(credential.id);
        (session, credential)
    }

    #[test]
    fn a_manual_host_logs_in_with_its_endpoint_password() {
        let login = SessionLogin::of(&session(AuthKind::Password), None);
        assert_eq!(login.method, LoginMethod::Password);
        assert_eq!(login.user, "root");
        assert_eq!(login.password, SecretRef::password("root", "10.0.0.1", 22));
        assert_eq!(login.endpoint(), "root@10.0.0.1:22");
    }

    #[test]
    fn auto_ignores_a_leftover_key_path() {
        let mut host = session(AuthKind::Auto);
        host.key_path = Some("/tmp/id_old".into());
        assert_eq!(SessionLogin::of(&host, None).key_path, None);

        host.auth = AuthKind::Key;
        assert_eq!(
            SessionLogin::of(&host, None).key_path,
            Some(PathBuf::from("/tmp/id_old"))
        );
    }

    #[test]
    fn a_password_credential_logs_in_with_its_own_keychain_entry() {
        let (session, credential) = using(CredentialKind::Password);
        let login = SessionLogin::of(&session, Some(&credential));
        assert_eq!(login.method, LoginMethod::Password);
        assert_eq!(login.user, "deploy");
        assert_eq!(login.password, credential.password_secret());
    }

    #[test]
    fn key_and_agent_credentials_keep_the_endpoint_for_a_partial_success() {
        let (session, credential) = using(CredentialKind::Key);
        let key = SessionLogin::of(&session, Some(&credential));
        assert_eq!(key.method, LoginMethod::Key);
        assert_eq!(key.key_path, Some(PathBuf::from("/tmp/id_deploy")));
        assert_eq!(key.password, SecretRef::password("deploy", "10.0.0.1", 22));

        let (session, credential) = using(CredentialKind::Agent);
        let agent = SessionLogin::of(&session, Some(&credential));
        assert_eq!(agent.method, LoginMethod::Agent);
        assert_eq!(agent.key_path, None);
        assert_eq!(
            agent.password,
            SecretRef::password("deploy", "10.0.0.1", 22)
        );
    }

    #[test]
    fn a_credential_the_host_does_not_name_is_not_used() {
        let credential = credential(CredentialKind::Password);
        let login = SessionLogin::of(&session(AuthKind::Auto), Some(&credential));
        assert_eq!(login.method, LoginMethod::Auto);
        assert_eq!(login.user, "root");
    }

    #[test]
    fn renaming_a_credential_does_not_change_the_login() {
        let (session, mut credential) = using(CredentialKind::Password);
        let before = SessionLogin::of(&session, Some(&credential));
        credential.name = "别的名字".into();
        assert_eq!(SessionLogin::of(&session, Some(&credential)), before);
        credential.user = "admin".into();
        assert_ne!(SessionLogin::of(&session, Some(&credential)), before);
    }
}
