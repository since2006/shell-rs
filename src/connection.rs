//! Shared connection questions, and the round trips measured on a live
//! connection. Secrets are never serialized or printed.
use std::time::Duration;

use zeroize::Zeroizing;

/// One field requested by an SSH keyboard-interactive challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionPromptField {
    label: String,
    echo: bool,
}

impl ConnectionPromptField {
    pub fn new(label: impl Into<String>, echo: bool) -> Self {
        Self {
            label: label.into(),
            echo,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn echo(&self) -> bool {
        self.echo
    }
}

/// A transport question that must be answered by the UI before SSH setup can
/// continue. The id is scoped to a single transport generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionPrompt {
    request_id: u64,
    kind: ConnectionPromptKind,
}

impl ConnectionPrompt {
    pub fn new(request_id: u64, kind: ConnectionPromptKind) -> Self {
        Self { request_id, kind }
    }

    pub fn request_id(&self) -> u64 {
        self.request_id
    }
    pub fn kind(&self) -> &ConnectionPromptKind {
        &self.kind
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionPromptKind {
    UnknownHost(UnknownHostPrompt),
    HostKeyChanged(HostKeyChangedPrompt),
    Authentication(AuthenticationPrompt),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownHostPrompt {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
    /// The jump host's name, when the key is a jump host's.
    jump_host: Option<String>,
}

impl UnknownHostPrompt {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// What the trust dialog tells the user about the key.
    pub fn description(&self) -> String {
        format!(
            "{}主机：{}:{}\n算法：{}\nSHA-256 指纹：{}\n\n请先确认该指纹来自可信渠道。",
            jump_host_line(self.jump_host.as_deref()),
            self.host,
            self.port,
            self.algorithm,
            self.fingerprint,
        )
    }
}

/// The line that opens a question about a jump host, naming it.
fn jump_host_line(jump_host: Option<&str>) -> String {
    jump_host
        .map(|name| format!("跳板主机：{name}\n"))
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyChangedPrompt {
    host: String,
    port: u16,
    algorithm: String,
    old_fingerprints: Vec<String>,
    fingerprint: String,
    known_hosts_path: std::path::PathBuf,
    jump_host: Option<String>,
}

impl HostKeyChangedPrompt {
    /// The jump host's name, when the key is a jump host's.
    pub fn jump_host(&self) -> Option<&str> {
        self.jump_host.as_deref()
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }
    pub fn old_fingerprints(&self) -> &[String] {
        &self.old_fingerprints
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    pub fn known_hosts_path(&self) -> &std::path::Path {
        &self.known_hosts_path
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticationPrompt {
    title: String,
    instructions: String,
    fields: Vec<ConnectionPromptField>,
}

impl AuthenticationPrompt {
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn instructions(&self) -> &str {
        &self.instructions
    }
    pub fn fields(&self) -> &[ConnectionPromptField] {
        &self.fields
    }
}

impl ConnectionPromptKind {
    pub fn unknown_host(
        host: impl Into<String>,
        port: u16,
        algorithm: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self::UnknownHost(UnknownHostPrompt {
            host: host.into(),
            port,
            algorithm: algorithm.into(),
            fingerprint: fingerprint.into(),
            jump_host: None,
        })
    }

    pub fn host_key_changed(
        host: impl Into<String>,
        port: u16,
        algorithm: impl Into<String>,
        old_fingerprints: Vec<String>,
        fingerprint: impl Into<String>,
        known_hosts_path: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::HostKeyChanged(HostKeyChangedPrompt {
            host: host.into(),
            port,
            algorithm: algorithm.into(),
            old_fingerprints,
            fingerprint: fingerprint.into(),
            known_hosts_path: known_hosts_path.into(),
            jump_host: None,
        })
    }

    pub fn authentication(
        title: impl Into<String>,
        instructions: impl Into<String>,
        fields: Vec<ConnectionPromptField>,
    ) -> Self {
        Self::Authentication(AuthenticationPrompt {
            title: title.into(),
            instructions: instructions.into(),
            fields,
        })
    }

    /// The same question, asked on the way through the jump host `name`:
    /// it says so, since the host the user opened is a different one.
    pub fn at_jump_host(mut self, name: Option<&str>) -> Self {
        let Some(name) = name else {
            return self;
        };
        match &mut self {
            Self::UnknownHost(prompt) => prompt.jump_host = Some(name.to_string()),
            Self::HostKeyChanged(prompt) => prompt.jump_host = Some(name.to_string()),
            Self::Authentication(prompt) => {
                prompt.instructions = if prompt.instructions.trim().is_empty() {
                    format!("跳板主机「{name}」")
                } else {
                    format!("跳板主机「{name}」：{}", prompt.instructions)
                };
            }
        }
        self
    }
}

/// A secret that is erased when dropped and never prints its contents.
pub struct ConnectionSecret(Zeroizing<String>);

impl ConnectionSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }
    pub(crate) fn expose(&self) -> &str {
        self.0.as_str()
    }
    pub(crate) fn into_inner(mut self) -> String {
        std::mem::take(&mut *self.0)
    }
}

impl std::fmt::Debug for ConnectionSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ConnectionSecret([已隐藏])")
    }
}

/// UI response to a transport prompt. Deliberately not `Clone`: answers may
/// contain passwords, private-key passphrases or one-time codes.
pub enum ConnectionPromptReply {
    TrustAndSave,
    Answers(Vec<ConnectionSecret>),
    Cancel,
}

impl std::fmt::Debug for ConnectionPromptReply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TrustAndSave => formatter.write_str("TrustAndSave"),
            Self::Answers(answers) => formatter
                .debug_tuple("Answers")
                .field(&format_args!("{} 个已隐藏答案", answers.len()))
                .finish(),
            Self::Cancel => formatter.write_str("Cancel"),
        }
    }
}

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

/// Answers whether to trust a host key seen for the first time. It blocks
/// until a person decides; `false` also covers "nobody is there to ask".
pub type TrustCallback = Box<dyn Fn(UnknownHostPrompt) -> bool + Send + Sync>;

/// Logs in once with a [`LoginTest`] and hangs up, so the host form can
/// check its values before saving them.
pub trait ConnectionTester: Send + Sync + 'static {
    /// Blocks: call it on a thread of its own. `Err` holds the reason, worded
    /// for the user.
    fn test(&self, request: LoginTest, trust: TrustCallback) -> Result<(), String>;
}

pub type SharedConnectionTester = std::sync::Arc<dyn ConnectionTester>;

/// The last round trip measured on a remote connection: how long the server
/// took to answer, or that it did not answer in time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Latency {
    Measured(Duration),
    TimedOut,
}

/// How a latency reads to someone typing: echo is instant below 100 ms,
/// noticeable up to 200 ms, and sluggish beyond.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LatencyLevel {
    Good,
    Fair,
    Poor,
}

impl Latency {
    pub fn level(self) -> LatencyLevel {
        match self {
            Latency::Measured(rtt) if rtt < Duration::from_millis(100) => LatencyLevel::Good,
            Latency::Measured(rtt) if rtt <= Duration::from_millis(200) => LatencyLevel::Fair,
            Latency::Measured(_) | Latency::TimedOut => LatencyLevel::Poor,
        }
    }

    pub fn label(self) -> String {
        match self {
            Latency::Measured(rtt) => format!("{} ms", rtt.as_millis()),
            Latency::TimedOut => "超时".into(),
        }
    }
}

#[cfg(test)]
mod latency_tests {
    use std::time::Duration;

    use super::{Latency, LatencyLevel};

    fn ms(millis: u64) -> Latency {
        Latency::Measured(Duration::from_millis(millis))
    }

    #[test]
    fn latency_levels_split_at_100_and_200_ms() {
        assert_eq!(ms(99).level(), LatencyLevel::Good);
        assert_eq!(ms(100).level(), LatencyLevel::Fair);
        assert_eq!(ms(200).level(), LatencyLevel::Fair);
        assert_eq!(ms(201).level(), LatencyLevel::Poor);
        assert_eq!(Latency::TimedOut.level(), LatencyLevel::Poor);
    }

    #[test]
    fn latency_labels_show_whole_milliseconds() {
        assert_eq!(
            Latency::Measured(Duration::from_micros(32_900)).label(),
            "32 ms"
        );
        assert_eq!(Latency::TimedOut.label(), "超时");
    }
}
