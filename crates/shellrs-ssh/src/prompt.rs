//! The questions a connection asks before it can go on, and their answers.
//! They carry what the question is about, never its wording: each app words
//! them in its own language. Secrets are never serialized or printed.
use std::path::PathBuf;

use zeroize::Zeroizing;

/// One field requested by an SSH keyboard-interactive challenge, in the
/// server's own words.
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
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// The jump host's name, when the key is a jump host's.
    pub fn jump_host(&self) -> Option<&str> {
        self.jump_host.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostKeyChangedPrompt {
    host: String,
    port: u16,
    algorithm: String,
    old_fingerprints: Vec<String>,
    fingerprint: String,
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
}

/// A credential the login asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticationPrompt {
    request: AuthenticationRequest,
    /// The jump host's name, when the login is a jump host's.
    jump_host: Option<String>,
}

/// What a login asks to be typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthenticationRequest {
    /// The login password; `saved_rejected` when the server refused the
    /// saved one.
    Password { saved_rejected: bool },
    /// The passphrase of the private key at `key_path`; `saved_rejected`
    /// when the saved one did not unlock it.
    Passphrase {
        key_path: PathBuf,
        saved_rejected: bool,
    },
    /// The server's own questions. `saved_password_rejected` when they ask
    /// for the password again after the saved one was refused.
    KeyboardInteractive {
        name: String,
        instructions: String,
        fields: Vec<ConnectionPromptField>,
        saved_password_rejected: bool,
    },
}

impl AuthenticationPrompt {
    pub fn request(&self) -> &AuthenticationRequest {
        &self.request
    }
    /// The jump host's name, when the login is a jump host's.
    pub fn jump_host(&self) -> Option<&str> {
        self.jump_host.as_deref()
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
    ) -> Self {
        Self::HostKeyChanged(HostKeyChangedPrompt {
            host: host.into(),
            port,
            algorithm: algorithm.into(),
            old_fingerprints,
            fingerprint: fingerprint.into(),
            jump_host: None,
        })
    }

    pub fn authentication(request: AuthenticationRequest) -> Self {
        Self::Authentication(AuthenticationPrompt {
            request,
            jump_host: None,
        })
    }

    /// The same question, asked on the way through the jump host `name`:
    /// it says so, since the host the user opened is a different one.
    pub fn at_jump_host(mut self, name: Option<&str>) -> Self {
        let Some(name) = name else {
            return self;
        };
        let jump_host = match &mut self {
            Self::UnknownHost(prompt) => &mut prompt.jump_host,
            Self::HostKeyChanged(prompt) => &mut prompt.jump_host,
            Self::Authentication(prompt) => &mut prompt.jump_host,
        };
        *jump_host = Some(name.to_string());
        self
    }
}

/// A secret that is erased when dropped and never prints its contents.
pub struct ConnectionSecret(Zeroizing<String>);

impl ConnectionSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
    pub fn into_inner(mut self) -> String {
        std::mem::take(&mut *self.0)
    }
}

impl std::fmt::Debug for ConnectionSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ConnectionSecret(<redacted>)")
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
                .field(&format_args!("{} redacted", answers.len()))
                .finish(),
            Self::Cancel => formatter.write_str("Cancel"),
        }
    }
}
