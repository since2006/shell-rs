//! How a connection logs in to a host, with every secret named by where it
//! is kept rather than held. The app builds one from what it saved and
//! hands it to a worker, which never reads the app's own records.

use std::path::PathBuf;

use super::secret::SecretRef;

/// The private keys in `~/.ssh` a [`LoginMethod::NoPassword`] login tries,
/// in order: OpenSSH's defaults, less the ones it has dropped.
pub const DEFAULT_KEY_FILES: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

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
/// how the app tells whether an edit has to reconnect a host: it holds
/// nothing that only changes what the user sees, such as a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostLogin {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub method: LoginMethod,
    /// The private key of [`LoginMethod::Key`], and `None` for every other
    /// method.
    pub key_path: Option<PathBuf>,
    /// The saved password the password step reads: the endpoint's own, or a
    /// password credential's. Every other login keeps the endpoint's for a
    /// server that asks for a password after the key.
    pub password: SecretRef,
    /// How the connection reaches the host.
    pub route: LoginRoute,
    /// The connection carries the terminal and nothing beside it: no
    /// channel of its own for the system probe or the commands run beside
    /// the shell. A bastion host that opened ShellRS with a link serves one
    /// session per connection and ends it when another channel comes and
    /// goes, as Xshell and OpenSSH never make it do.
    pub shell_only: bool,
}

impl HostLogin {
    /// `user@host:port` reached directly by `method`, with the endpoint's
    /// own password.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        method: LoginMethod,
    ) -> Self {
        let host = host.into();
        let user = user.into();
        Self {
            password: SecretRef::password(&user, &host, port),
            host,
            port,
            user,
            method,
            key_path: None,
            route: LoginRoute::Direct,
            shell_only: false,
        }
    }

    /// The same login, reaching the host by `route`.
    pub fn with_route(mut self, route: LoginRoute) -> Self {
        self.route = route;
        self
    }

    /// `user@host:port`, for messages and as the key of resumable transfers.
    pub fn endpoint(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
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
        login: Box<HostLogin>,
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

/// The kind of proxy a host connects through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProxyKind {
    /// An HTTP proxy, which the connection asks to `CONNECT` to the host.
    #[default]
    Http,
    Socks5,
}

impl ProxyKind {
    /// Every kind, in the order a form lists them.
    pub const ALL: [ProxyKind; 2] = [ProxyKind::Http, ProxyKind::Socks5];

    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyKind::Http => "http",
            ProxyKind::Socks5 => "socks5",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "http" => Some(ProxyKind::Http),
            "socks5" => Some(ProxyKind::Socks5),
            _ => None,
        }
    }
}

/// The proxy a connection goes through.
#[derive(Clone, Debug, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_reads_its_endpoints_password() {
        let login = HostLogin::new("10.0.0.1", 22, "root", LoginMethod::Password);
        assert_eq!(login.password, SecretRef::password("root", "10.0.0.1", 22));
        assert_eq!(login.endpoint(), "root@10.0.0.1:22");
        assert_eq!(login.route, LoginRoute::Direct);
        assert!(!login.shell_only);
    }

    #[test]
    fn a_jump_hosts_name_is_not_part_of_the_login() {
        let hop = |name: &str, host: &str| JumpLogin::Host {
            name: name.into(),
            login: Box::new(HostLogin::new(host, 22, "root", LoginMethod::Password)),
        };
        assert_eq!(hop("one", "10.0.0.1"), hop("other", "10.0.0.1"));
        assert_ne!(hop("one", "10.0.0.1"), hop("one", "10.0.0.2"));
        assert_ne!(hop("one", "10.0.0.1"), JumpLogin::Deleted);
        assert_eq!(JumpLogin::Deleted, JumpLogin::Deleted);
    }

    #[test]
    fn only_a_proxy_with_a_user_has_a_password() {
        let proxy = |user: Option<&str>| ProxyLogin {
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".into(),
            port: 7890,
            user: user.map(Into::into),
        };
        assert_eq!(proxy(None).password(), None);
        assert_eq!(
            LoginRoute::Proxy(proxy(Some("me"))).proxy_password(),
            Some(SecretRef::proxy("me", "127.0.0.1", 7890))
        );
        assert_eq!(LoginRoute::Direct.proxy_password(), None);
    }

    #[test]
    fn proxy_kinds_keep_their_stored_spelling() {
        for kind in ProxyKind::ALL {
            assert_eq!(ProxyKind::from_stored(kind.as_str()), Some(kind));
        }
        assert_eq!(ProxyKind::Socks5.as_str(), "socks5");
        assert_eq!(ProxyKind::from_stored("ftp"), None);
    }
}
