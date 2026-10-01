//! How a connection logs in to a host, with the credential the host uses, if
//! any, already looked up. The store builds one on the UI thread and hands it
//! to the terminal, SFTP, forward and CLI workers, which cannot read the
//! store themselves.

use std::path::PathBuf;

use crate::secrets::SecretRef;

use super::{AuthKind, Credential, CredentialKind, ProxyKind, ProxySettings, Session};

/// Which authentication methods a login tries. Each one starts by asking
/// the server to let the user in with nothing at all, which a server without
/// authentication grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginMethod {
    /// A password, saved or asked for.
    Password,
    /// Whatever needs nothing typed: the SSH agent, then the default keys in
    /// `~/.ssh`. A server that wants a password is refused, not asked.
    NoPassword,
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
    /// password credential's. Every other login keeps the endpoint's for a
    /// server that asks for a password after the key.
    pub password: SecretRef,
    /// How the connection reaches the host.
    pub route: LoginRoute,
}

/// How a connection reaches a host, with every jump host's login looked up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LoginRoute {
    #[default]
    Direct,
    /// Through these hosts, in order. Each of them is reached through the
    /// one before it, whatever its own route says.
    Jump(Vec<JumpLogin>),
    Proxy(ProxyLogin),
}

impl LoginRoute {
    /// The proxy's password entry, for a route through a proxy that has one.
    pub fn proxy_password(&self) -> Option<SecretRef> {
        match self {
            LoginRoute::Proxy(proxy) => proxy.password(),
            _ => None,
        }
    }
}

/// One jump host on the way to a host.
#[derive(Clone, Debug)]
pub enum JumpLogin {
    Host {
        /// What the user calls it, for the questions and errors of its hop.
        name: String,
        /// How it logs in, directly from the hop before it.
        login: Box<SessionLogin>,
    },
    /// A jump host that has been deleted: the route cannot be taken.
    Deleted,
}

/// Equal when they log in the same way: renaming a jump host does not make
/// the hosts behind it reconnect.
impl PartialEq for JumpLogin {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (JumpLogin::Host { login: mine, .. }, JumpLogin::Host { login: theirs, .. }) => {
                mine == theirs
            }
            (JumpLogin::Deleted, JumpLogin::Deleted) => true,
            _ => false,
        }
    }
}

impl Eq for JumpLogin {}

/// The proxy a connection goes through.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProxyLogin {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
}

impl ProxyLogin {
    /// Where the proxy's password is kept: only a proxy with a user name
    /// has one.
    pub fn password(&self) -> Option<SecretRef> {
        self.user
            .as_ref()
            .map(|user| SecretRef::proxy(user, &self.host, self.port))
    }
}

impl From<&ProxySettings> for ProxyLogin {
    fn from(proxy: &ProxySettings) -> Self {
        Self {
            kind: proxy.kind,
            host: proxy.host.to_string(),
            port: proxy.port,
            user: proxy.user.as_ref().map(ToString::to_string),
        }
    }
}

impl SessionLogin {
    /// A login of the host's own rather than taken from a credential.
    pub fn manual(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        auth: AuthKind,
    ) -> Self {
        let host = host.into();
        let user = user.into();
        let method = match auth {
            AuthKind::Password => LoginMethod::Password,
            AuthKind::NoPassword => LoginMethod::NoPassword,
        };
        Self {
            password: SecretRef::password(&user, &host, port),
            host,
            port,
            user,
            method,
            key_path: None,
            route: LoginRoute::Direct,
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
            route: LoginRoute::Direct,
        }
    }

    /// The same login, reaching the host by `route`.
    pub fn with_route(mut self, route: LoginRoute) -> Self {
        self.route = route;
        self
    }

    /// How `session` logs in, given the credential it uses, as if it were
    /// reached directly: its route takes the store to look up. `credential`
    /// is ignored unless it is the one the session names, so a session whose
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
        let mut session = session(AuthKind::Password);
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
    fn a_host_without_a_password_keeps_its_endpoint_for_a_partial_success() {
        let login = SessionLogin::of(&session(AuthKind::NoPassword), None);
        assert_eq!(login.method, LoginMethod::NoPassword);
        assert_eq!(login.key_path, None);
        assert_eq!(login.password, SecretRef::password("root", "10.0.0.1", 22));
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
        let login = SessionLogin::of(&session(AuthKind::NoPassword), Some(&credential));
        assert_eq!(login.method, LoginMethod::NoPassword);
        assert_eq!(login.user, "root");
    }

    #[test]
    fn a_jump_hosts_name_is_not_part_of_the_login() {
        let hop = |name: &str, host: &str| JumpLogin::Host {
            name: name.into(),
            login: Box::new(SessionLogin::manual(host, 22, "root", AuthKind::Password)),
        };
        assert_eq!(hop("阿里云99", "10.0.0.1"), hop("跳板", "10.0.0.1"));
        assert_ne!(hop("阿里云99", "10.0.0.1"), hop("阿里云99", "10.0.0.2"));
        assert_ne!(hop("阿里云99", "10.0.0.1"), JumpLogin::Deleted);
        assert_eq!(JumpLogin::Deleted, JumpLogin::Deleted);
    }

    #[test]
    fn only_a_proxy_with_a_user_has_a_password() {
        let proxy = ProxySettings::new(ProxyKind::Socks5, "127.0.0.1", 7890);
        assert_eq!(ProxyLogin::from(&proxy).password(), None);
        assert_eq!(
            ProxyLogin::from(&proxy.clone().with_user("")).password(),
            None
        );
        let login = ProxyLogin::from(&proxy.with_user("me"));
        assert_eq!(
            login.password(),
            Some(SecretRef::proxy("me", "127.0.0.1", 7890))
        );
        assert_eq!(
            LoginRoute::Proxy(login).proxy_password(),
            Some(SecretRef::proxy("me", "127.0.0.1", 7890))
        );
        assert_eq!(LoginRoute::Direct.proxy_password(), None);
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
