//! How a connection logs in to a host, with the credential the host uses, if
//! any, already looked up. The store builds one on the UI thread and hands it
//! to the terminal, SFTP, forward and CLI workers, which cannot read the
//! store themselves.

use std::path::PathBuf;

use crate::secrets::SecretRef;
pub use shellrs_ssh::{
    DEFAULT_KEY_FILES, HostLogin, JumpLogin, LoginMethod, LoginRoute, ProxyLogin,
};

use super::{
    AuthKind, Credential, CredentialKind, Host, ProxySettings, SerialSettings, TelnetSettings,
};

/// How a host's terminal connects, looked up by the store when it connects.
/// Two equal values connect the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostConnection {
    Ssh(HostLogin),
    Telnet(TelnetLogin),
    Serial(SerialLine),
}

impl HostConnection {
    /// The endpoint and proxy password entries it reads, which the store
    /// keeps while any host still does.
    pub fn password_entries(&self) -> Vec<SecretRef> {
        let (password, route) = match self {
            HostConnection::Ssh(login) => (Some(login.password.clone()), &login.route),
            HostConnection::Telnet(login) => (
                login
                    .auto_login
                    .as_ref()
                    .and_then(|auto_login| auto_login.password.clone()),
                &login.route,
            ),
            HostConnection::Serial(_) => return Vec::new(),
        };
        [password, route.proxy_password()]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// What a Telnet host's terminal connects to, and how it logs in, if it
/// does.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TelnetLogin {
    pub host: String,
    pub port: u16,
    /// How the connection reaches the host, as an SSH host's does.
    pub route: LoginRoute,
    /// Telnet 自动登录, when it is on.
    pub auto_login: Option<TelnetAutoLogin>,
}

/// What Telnet 自动登录 types, and the prompts it waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TelnetAutoLogin {
    /// Empty for a device that asks only for a password.
    pub user: String,
    /// Where the password is kept: the endpoint's entry, or a password
    /// credential's. `None` when there is none to type: the user does.
    pub password: Option<SecretRef>,
    /// Empty for the usual prompts.
    pub user_prompt: String,
    pub password_prompt: String,
}

impl TelnetLogin {
    /// `host:port` reached directly, without 自动登录.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            route: LoginRoute::Direct,
            auto_login: None,
        }
    }

    /// How `host` connects as if it were reached directly: its route takes
    /// the store to look up. `credential` is ignored unless it is the one the
    /// host names; one that is not a password credential gives only its
    /// user name.
    pub fn of(host: &Host, settings: &TelnetSettings, credential: Option<&Credential>) -> Self {
        let address = host.address.to_string();
        let prompts = |user: String, password: Option<SecretRef>| TelnetAutoLogin {
            user,
            password,
            user_prompt: settings.user_prompt.to_string(),
            password_prompt: settings.password_prompt.to_string(),
        };
        let auto_login = match credential
            .filter(|credential| host.credential == Some(credential.id))
        {
            Some(credential) => Some(prompts(
                credential.user.to_string(),
                (credential.kind == CredentialKind::Password).then(|| credential.password_secret()),
            )),
            None if host.auth == AuthKind::Password => Some(prompts(
                host.user.to_string(),
                Some(SecretRef::password(host.user.as_ref(), &address, host.port)),
            )),
            None => None,
        };
        Self {
            auto_login,
            ..Self::new(address, host.port)
        }
    }

    /// The same connection, reaching the host by `route`.
    pub fn with_route(mut self, route: LoginRoute) -> Self {
        self.route = route;
        self
    }
}

/// What a serial host's terminal opens: the device, and how its line is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SerialLine {
    pub device: String,
    pub settings: SerialSettings,
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

/// How a saved host or credential logs in: [`HostLogin`]'s constructors
/// from what the app saved.
pub trait HostLoginExt {
    /// A login of the host's own rather than taken from a credential.
    fn manual(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        auth: AuthKind,
    ) -> HostLogin;

    /// A login through `credential` to `host:port`.
    fn with_credential(host: impl Into<String>, port: u16, credential: &Credential) -> HostLogin;

    /// How `host` logs in, given the credential it uses, as if it were
    /// reached directly: its route takes the store to look up. `credential`
    /// is ignored unless it is the one the host names, so a host whose
    /// credential has gone logs in with what the form holds.
    fn of(host: &Host, credential: Option<&Credential>) -> HostLogin;
}

impl HostLoginExt for HostLogin {
    fn manual(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        auth: AuthKind,
    ) -> HostLogin {
        let method = match auth {
            AuthKind::Password => LoginMethod::Password,
            AuthKind::NoPassword => LoginMethod::NoPassword,
        };
        HostLogin::new(host, port, user, method)
    }

    fn with_credential(host: impl Into<String>, port: u16, credential: &Credential) -> HostLogin {
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
        HostLogin {
            key_path,
            password,
            ..HostLogin::new(host, port, user, method)
        }
    }

    fn of(host: &Host, credential: Option<&Credential>) -> HostLogin {
        match credential.filter(|credential| host.credential == Some(credential.id)) {
            Some(credential) => Self::with_credential(host.address.as_ref(), host.port, credential),
            None => Self::manual(
                host.address.as_ref(),
                host.port,
                host.user.as_ref(),
                host.auth,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CredentialDraft, CredentialId, HostDraft, HostId, ProxyKind};
    use super::*;

    fn host(auth: AuthKind) -> Host {
        Host::new(
            HostId(1),
            HostDraft::new("web", "10.0.0.1", 22, "root", auth, None),
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

    fn using(kind: CredentialKind) -> (Host, Credential) {
        let credential = credential(kind);
        let mut host = host(AuthKind::Password);
        host.credential = Some(credential.id);
        (host, credential)
    }

    #[test]
    fn a_manual_host_logs_in_with_its_endpoint_password() {
        let login = HostLogin::of(&host(AuthKind::Password), None);
        assert_eq!(login.method, LoginMethod::Password);
        assert_eq!(login.user, "root");
        assert_eq!(login.password, SecretRef::password("root", "10.0.0.1", 22));
        assert_eq!(login.endpoint(), "root@10.0.0.1:22");
    }

    #[test]
    fn a_host_without_a_password_keeps_its_endpoint_for_a_partial_success() {
        let login = HostLogin::of(&host(AuthKind::NoPassword), None);
        assert_eq!(login.method, LoginMethod::NoPassword);
        assert_eq!(login.key_path, None);
        assert_eq!(login.password, SecretRef::password("root", "10.0.0.1", 22));
    }

    #[test]
    fn a_password_credential_logs_in_with_its_own_keychain_entry() {
        let (host, credential) = using(CredentialKind::Password);
        let login = HostLogin::of(&host, Some(&credential));
        assert_eq!(login.method, LoginMethod::Password);
        assert_eq!(login.user, "deploy");
        assert_eq!(login.password, credential.password_secret());
    }

    #[test]
    fn key_and_agent_credentials_keep_the_endpoint_for_a_partial_success() {
        let (host, credential) = using(CredentialKind::Key);
        let key = HostLogin::of(&host, Some(&credential));
        assert_eq!(key.method, LoginMethod::Key);
        assert_eq!(key.key_path, Some(PathBuf::from("/tmp/id_deploy")));
        assert_eq!(key.password, SecretRef::password("deploy", "10.0.0.1", 22));

        let (host, credential) = using(CredentialKind::Agent);
        let agent = HostLogin::of(&host, Some(&credential));
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
        let login = HostLogin::of(&host(AuthKind::NoPassword), Some(&credential));
        assert_eq!(login.method, LoginMethod::NoPassword);
        assert_eq!(login.user, "root");
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
    fn telnet_logs_in_only_when_told_and_with_a_password_credential_s_password() {
        let telnet = |auth: AuthKind, user: &str| {
            let mut draft = HostDraft::telnet("switch", "10.0.0.5", 23, None);
            draft.auth = auth;
            draft.user = user.into();
            Host::new(HostId(1), draft)
        };
        let prompts = TelnetSettings {
            user_prompt: "账号：".into(),
            password_prompt: "".into(),
        };
        let off = TelnetLogin::of(&telnet(AuthKind::NoPassword, "admin"), &prompts, None);
        assert_eq!(off.auto_login, None);
        assert_eq!((off.host.as_str(), off.port), ("10.0.0.5", 23));
        assert!(HostConnection::Telnet(off).password_entries().is_empty());

        let typed = TelnetLogin::of(&telnet(AuthKind::Password, ""), &prompts, None);
        let auto_login = typed.auto_login.clone().unwrap();
        assert_eq!(auto_login.user, "");
        assert_eq!(auto_login.user_prompt, "账号：");
        assert_eq!(
            auto_login.password,
            Some(SecretRef::password("", "10.0.0.5", 23))
        );
        assert_eq!(
            HostConnection::Telnet(typed).password_entries(),
            [SecretRef::password("", "10.0.0.5", 23)]
        );

        let password = credential(CredentialKind::Password);
        let mut host = telnet(AuthKind::Password, "");
        host.credential = Some(password.id);
        let saved = TelnetLogin::of(&host, &prompts, Some(&password))
            .auto_login
            .unwrap();
        assert_eq!(saved.user, "deploy");
        assert_eq!(saved.password, Some(password.password_secret()));
        // A key is no use to Telnet: only the user name is typed.
        let key = credential(CredentialKind::Key);
        host.credential = Some(key.id);
        let keyed = TelnetLogin::of(&host, &prompts, Some(&key))
            .auto_login
            .unwrap();
        assert_eq!((keyed.user.as_str(), keyed.password), ("deploy", None));
    }

    #[test]
    fn renaming_a_credential_does_not_change_the_login() {
        let (host, mut credential) = using(CredentialKind::Password);
        let before = HostLogin::of(&host, Some(&credential));
        credential.name = "别的名字".into();
        assert_eq!(HostLogin::of(&host, Some(&credential)), before);
        credential.user = "admin".into();
        assert_ne!(HostLogin::of(&host, Some(&credential)), before);
    }
}
