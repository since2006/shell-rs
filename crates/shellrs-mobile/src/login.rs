//! How a connection logs in, as the app describes it.

use shellrs_ssh as ssh;

use crate::secrets::SecretRef;

/// Everything a connection needs to log in to a host.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Login {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub method: LoginMethod,
    /// The private key file of [`LoginMethod::Key`], in the app's own
    /// files.
    pub key_path: Option<String>,
    /// Where the password is saved: the endpoint's own
    /// ([`SecretRef::Password`] of this user, host and port) or a password
    /// credential's. Logins by key read it too, for a server that asks for
    /// a password after the key.
    pub password: SecretRef,
    pub route: Route,
}

/// What a login tries. Each starts by asking the server to let the user in
/// with nothing, which a server without authentication grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LoginMethod {
    /// A password, saved or asked for.
    Password,
    /// Nothing typed. A phone has no agent and no default keys, so this
    /// only gets in where the server asks for nothing.
    NoPassword,
    /// The private key file `key_path`.
    Key,
    /// An SSH agent's keys: phones have none, so this fails.
    Agent,
}

/// How a connection reaches its host.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Route {
    Direct,
    /// Through these hosts, in order, each reached through the one before.
    Jump {
        hosts: Vec<JumpHost>,
    },
    Proxy {
        proxy: Proxy,
    },
}

/// A jump host on the way.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct JumpHost {
    /// What the user calls it: questions and failures at it carry it.
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub method: LoginMethod,
    pub key_path: Option<String>,
    pub password: SecretRef,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    /// With a user, the proxy's password is read as
    /// [`SecretRef::Proxy`] of the user and the proxy's address.
    pub user: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ProxyKind {
    Http,
    Socks5,
}

impl From<ProxyKind> for ssh::ProxyKind {
    fn from(kind: ProxyKind) -> Self {
        match kind {
            ProxyKind::Http => Self::Http,
            ProxyKind::Socks5 => Self::Socks5,
        }
    }
}

impl From<ssh::ProxyKind> for ProxyKind {
    fn from(kind: ssh::ProxyKind) -> Self {
        match kind {
            ssh::ProxyKind::Http => Self::Http,
            ssh::ProxyKind::Socks5 => Self::Socks5,
        }
    }
}

impl From<LoginMethod> for ssh::LoginMethod {
    fn from(method: LoginMethod) -> Self {
        match method {
            LoginMethod::Password => Self::Password,
            LoginMethod::NoPassword => Self::NoPassword,
            LoginMethod::Key => Self::Key,
            LoginMethod::Agent => Self::Agent,
        }
    }
}

impl From<Login> for ssh::HostLogin {
    fn from(login: Login) -> Self {
        let route = match login.route {
            Route::Direct => ssh::LoginRoute::Direct,
            Route::Jump { hosts } => ssh::LoginRoute::Jump(
                hosts
                    .into_iter()
                    .map(|hop| ssh::JumpLogin::Host {
                        name: hop.name,
                        login: Box::new(host_login(
                            hop.host,
                            hop.port,
                            hop.user,
                            hop.method,
                            hop.key_path,
                            hop.password,
                        )),
                    })
                    .collect(),
            ),
            Route::Proxy { proxy } => ssh::LoginRoute::Proxy(ssh::ProxyLogin {
                kind: proxy.kind.into(),
                host: proxy.host,
                port: proxy.port,
                user: proxy.user,
            }),
        };
        host_login(
            login.host,
            login.port,
            login.user,
            login.method,
            login.key_path,
            login.password,
        )
        .with_route(route)
    }
}

fn host_login(
    host: String,
    port: u16,
    user: String,
    method: LoginMethod,
    key_path: Option<String>,
    password: SecretRef,
) -> ssh::HostLogin {
    let mut login = ssh::HostLogin::new(host, port, user, method.into());
    login.key_path = key_path
        .filter(|_| method == LoginMethod::Key)
        .map(Into::into);
    login.password = password.into();
    login
}
