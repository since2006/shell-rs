//! Shared connection questions. Secrets are never serialized or printed.
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
}

impl UnknownHostPrompt {
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// What the trust dialog tells the user about the key.
    pub fn description(&self) -> String {
        format!(
            "主机：{}:{}\n算法：{}\nSHA-256 指纹：{}\n\n请先确认该指纹来自可信渠道。",
            self.host, self.port, self.algorithm, self.fingerprint,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyChangedPrompt {
    host: String,
    port: u16,
    algorithm: String,
    old_fingerprints: Vec<String>,
    fingerprint: String,
    known_hosts_path: std::path::PathBuf,
}

impl HostKeyChangedPrompt {
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

/// What a connection test logs in with: the session form's current values,
/// saved or not. Secrets stay zeroized and never print.
pub struct LoginTest {
    login: crate::session::SessionLogin,
    /// The secrets typed into the form, for a login typed there. `None` for
    /// a login through a credential, which uses what the credential saved.
    typed: Option<TypedSecrets>,
}

#[derive(Default)]
struct TypedSecrets {
    password: Option<Zeroizing<String>>,
    passphrase: Option<Zeroizing<String>>,
}

impl LoginTest {
    /// A login typed into the form. The form decides its password and its
    /// key's passphrase: an empty field means none, whatever is saved.
    pub fn typed(login: crate::session::SessionLogin) -> Self {
        Self {
            login,
            typed: Some(TypedSecrets::default()),
        }
    }

    /// A login through a saved credential, with the secrets saved for it.
    pub fn saved(login: crate::session::SessionLogin) -> Self {
        Self { login, typed: None }
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

    pub fn login(&self) -> &crate::session::SessionLogin {
        &self.login
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
}

impl std::fmt::Debug for LoginTest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginTest")
            .field("login", &self.login)
            .field("typed", &self.is_typed())
            .field("password", &self.password().map(|_| "<redacted>"))
            .field("passphrase", &self.passphrase().map(|_| "<redacted>"))
            .finish()
    }
}

/// Answers whether to trust a host key seen for the first time. It blocks
/// until a person decides; `false` also covers "nobody is there to ask".
pub type TrustCallback = Box<dyn Fn(UnknownHostPrompt) -> bool + Send + Sync>;

/// Logs in once with a [`LoginTest`] and hangs up, so the session form can
/// check its values before saving them.
pub trait ConnectionTester: Send + Sync + 'static {
    /// Blocks: call it on a thread of its own. `Err` holds the reason, worded
    /// for the user.
    fn test(&self, request: LoginTest, trust: TrustCallback) -> Result<(), String>;
}

pub type SharedConnectionTester = std::sync::Arc<dyn ConnectionTester>;
