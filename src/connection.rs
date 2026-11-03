//! What the host form's 「测试连接」 tries, and the connection questions and
//! round trips the SSH layer defines, under the names the app has always
//! used for them.
use zeroize::Zeroizing;

pub use crate::ssh::{
    AuthenticationPrompt, AuthenticationRequest, ConnectionPrompt, ConnectionPromptField,
    ConnectionPromptKind, ConnectionPromptReply, ConnectionSecret, HostKeyChangedPrompt, Latency,
    LatencyLevel, UnknownHostPrompt,
};

/// What a connection test logs in with: the host form's current values,
/// saved or not. Secrets stay zeroized and never print.
pub struct LoginTest {
    login: crate::host::HostLogin,
    /// The secrets typed into the form, for a login typed there. `None` for
    /// a login through a credential, which uses what the credential saved.
    typed: Option<TypedSecrets>,
    /// The proxy's password as the form has it, whichever way the host logs
    /// in: the proxy is the form's either way.
    proxy_password: Option<Zeroizing<String>>,
}

#[derive(Default)]
struct TypedSecrets {
    password: Option<Zeroizing<String>>,
    passphrase: Option<Zeroizing<String>>,
}

impl LoginTest {
    /// A login typed into the form. The form decides its password and its
    /// key's passphrase: an empty field means none, whatever is saved.
    pub fn typed(login: crate::host::HostLogin) -> Self {
        Self {
            login,
            typed: Some(TypedSecrets::default()),
            proxy_password: None,
        }
    }

    /// A login through a saved credential, with the secrets saved for it.
    pub fn saved(login: crate::host::HostLogin) -> Self {
        Self {
            login,
            typed: None,
            proxy_password: None,
        }
    }

    /// The proxy's password typed into the form. An empty one is none.
    pub fn with_proxy_password(mut self, password: impl Into<String>) -> Self {
        let password = password.into();
        self.proxy_password = (!password.is_empty()).then(|| Zeroizing::new(password));
        self
    }

    /// The password typed into the form. Only a typed login has one.
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        if let Some(typed) = &mut self.typed {
            typed.password = Some(Zeroizing::new(password.into()));
        }
        self
    }

    /// The passphrase typed into the form. Only a typed login has one.
    pub fn with_passphrase(mut self, passphrase: impl Into<String>) -> Self {
        if let Some(typed) = &mut self.typed {
            typed.passphrase = Some(Zeroizing::new(passphrase.into()));
        }
        self
    }

    pub fn login(&self) -> &crate::host::HostLogin {
        &self.login
    }

    /// A login by password typed into the form with the password left
    /// empty. 测试连接 then goes without one, as 「无密码」 would, rather than
    /// stop at the missing password (the user's call).
    pub fn password_left_empty(&self) -> bool {
        self.login.method == crate::host::LoginMethod::Password
            && self
                .typed
                .as_ref()
                .is_some_and(|typed| typed.password.is_none())
    }
    pub fn host(&self) -> &str {
        &self.login.host
    }
    pub fn port(&self) -> u16 {
        self.login.port
    }
    pub fn user(&self) -> &str {
        &self.login.user
    }
    /// Whether the form, not the keychain, decides the secrets.
    pub fn is_typed(&self) -> bool {
        self.typed.is_some()
    }
    pub fn password(&self) -> Option<&str> {
        self.typed
            .as_ref()
            .and_then(|typed| typed.password.as_deref())
            .map(String::as_str)
    }
    pub fn passphrase(&self) -> Option<&str> {
        self.typed
            .as_ref()
            .and_then(|typed| typed.passphrase.as_deref())
            .map(String::as_str)
    }
    pub fn proxy_password(&self) -> Option<&str> {
        self.proxy_password.as_deref().map(String::as_str)
    }
}

impl std::fmt::Debug for LoginTest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginTest")
            .field("login", &self.login)
            .field("typed", &self.is_typed())
            .field("password", &self.password().map(|_| "<redacted>"))
            .field("passphrase", &self.passphrase().map(|_| "<redacted>"))
            .field(
                "proxy_password",
                &self.proxy_password().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// What 测试连接 tries for a Telnet host: the way to its address and port,
/// with the form's proxy password. It does not log in: Telnet's login is
/// the device's prompts, which 自动登录 answers in the terminal.
pub struct TelnetTest {
    host: String,
    port: u16,
    route: crate::host::LoginRoute,
    proxy_password: Option<Zeroizing<String>>,
}

impl TelnetTest {
    pub fn new(host: impl Into<String>, port: u16, route: crate::host::LoginRoute) -> Self {
        Self {
            host: host.into(),
            port,
            route,
            proxy_password: None,
        }
    }

    /// The proxy's password typed into the form. An empty one is none.
    pub fn with_proxy_password(mut self, password: impl Into<String>) -> Self {
        let password = password.into();
        self.proxy_password = (!password.is_empty()).then(|| Zeroizing::new(password));
        self
    }

    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn route(&self) -> &crate::host::LoginRoute {
        &self.route
    }
    pub fn proxy_password(&self) -> Option<&str> {
        self.proxy_password.as_deref().map(String::as_str)
    }
}

impl std::fmt::Debug for TelnetTest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelnetTest")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("route", &self.route)
            .field(
                "proxy_password",
                &self.proxy_password().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Answers whether to trust a host key seen for the first time. It blocks
/// until a person decides; `false` also covers "nobody is there to ask".
pub type TrustCallback = Box<dyn Fn(UnknownHostPrompt) -> bool + Send + Sync>;

/// Logs in once with a [`LoginTest`] and hangs up, reaches a Telnet host's
/// port, or opens a serial port and closes it again, so the host form can
/// check its values before saving them.
pub trait ConnectionTester: Send + Sync + 'static {
    /// Blocks: call it on a thread of its own. `Err` holds the reason, worded
    /// for the user.
    fn test(&self, request: LoginTest, trust: TrustCallback) -> Result<(), String>;

    /// Reach `request`'s address and port the way its terminal would, jump
    /// hosts and proxy included, and hang up. Blocks like [`Self::test`].
    fn test_telnet(&self, request: TelnetTest, trust: TrustCallback) -> Result<(), String>;

    /// Open `line` as a serial host's terminal would, and close it. Blocks
    /// too, briefly; `Err` is why it would not open.
    fn test_serial(&self, line: &crate::host::SerialLine) -> Result<(), String>;
}

pub type SharedConnectionTester = std::sync::Arc<dyn ConnectionTester>;
