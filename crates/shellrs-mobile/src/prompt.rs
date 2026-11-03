//! Questions a connection asks the user while it logs in.

use shellrs_ssh as ssh;

/// A question for the user. Answer it with [`Connection::answer`] and its
/// `id`; a connection waits for the answer, not counting the time towards
/// its timeout.
///
/// [`Connection::answer`]: crate::Connection::answer
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Prompt {
    pub id: u64,
    pub kind: PromptKind,
    /// The jump host the question is about, by its name; `None` for the
    /// host itself.
    pub jump_host: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PromptKind {
    /// The first connection to this host: trust its key? Trusting saves it
    /// in the data folder's `known_hosts`.
    UnknownHost {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
    },
    /// The host's key is not the one saved, which an attack would look
    /// like. Trusting replaces the saved one.
    HostKeyChanged {
        host: String,
        port: u16,
        algorithm: String,
        old_fingerprints: Vec<String>,
        fingerprint: String,
    },
    /// The login password; `saved_rejected` when the saved one was wrong.
    Password { saved_rejected: bool },
    /// The passphrase of the key at `key_path`.
    Passphrase {
        key_path: String,
        saved_rejected: bool,
    },
    /// The server's own questions, one answer per field.
    KeyboardInteractive {
        name: String,
        instructions: String,
        fields: Vec<PromptField>,
        saved_password_rejected: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PromptField {
    pub label: String,
    /// Whether the answer may be shown as it is typed.
    pub echo: bool,
}

/// The user's answer.
#[derive(Clone, PartialEq, Eq, uniffi::Enum)]
pub enum PromptReply {
    /// Trust the host's key and save it.
    TrustAndSave,
    /// The password, the passphrase, or one answer per field.
    Answers {
        answers: Vec<String>,
    },
    Cancel,
}

impl std::fmt::Debug for PromptReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TrustAndSave => f.write_str("TrustAndSave"),
            Self::Answers { answers } => write!(f, "Answers({} redacted)", answers.len()),
            Self::Cancel => f.write_str("Cancel"),
        }
    }
}

impl From<&ssh::ConnectionPrompt> for Prompt {
    fn from(prompt: &ssh::ConnectionPrompt) -> Self {
        let (kind, jump_host) = match prompt.kind() {
            ssh::ConnectionPromptKind::UnknownHost(question) => (
                PromptKind::UnknownHost {
                    host: question.host().into(),
                    port: question.port(),
                    algorithm: question.algorithm().into(),
                    fingerprint: question.fingerprint().into(),
                },
                question.jump_host(),
            ),
            ssh::ConnectionPromptKind::HostKeyChanged(question) => (
                PromptKind::HostKeyChanged {
                    host: question.host().into(),
                    port: question.port(),
                    algorithm: question.algorithm().into(),
                    old_fingerprints: question.old_fingerprints().to_vec(),
                    fingerprint: question.fingerprint().into(),
                },
                question.jump_host(),
            ),
            ssh::ConnectionPromptKind::Authentication(question) => (
                match question.request() {
                    ssh::AuthenticationRequest::Password { saved_rejected } => {
                        PromptKind::Password {
                            saved_rejected: *saved_rejected,
                        }
                    }
                    ssh::AuthenticationRequest::Passphrase {
                        key_path,
                        saved_rejected,
                    } => PromptKind::Passphrase {
                        key_path: key_path.display().to_string(),
                        saved_rejected: *saved_rejected,
                    },
                    ssh::AuthenticationRequest::KeyboardInteractive {
                        name,
                        instructions,
                        fields,
                        saved_password_rejected,
                    } => PromptKind::KeyboardInteractive {
                        name: name.clone(),
                        instructions: instructions.clone(),
                        fields: fields
                            .iter()
                            .map(|field| PromptField {
                                label: field.label().into(),
                                echo: field.echo(),
                            })
                            .collect(),
                        saved_password_rejected: *saved_password_rejected,
                    },
                },
                question.jump_host(),
            ),
        };
        Prompt {
            id: prompt.request_id(),
            kind,
            jump_host: jump_host.map(Into::into),
        }
    }
}

impl From<PromptReply> for ssh::ConnectionPromptReply {
    fn from(reply: PromptReply) -> Self {
        match reply {
            PromptReply::TrustAndSave => Self::TrustAndSave,
            PromptReply::Answers { answers } => Self::Answers(
                answers
                    .into_iter()
                    .map(ssh::ConnectionSecret::new)
                    .collect(),
            ),
            PromptReply::Cancel => Self::Cancel,
        }
    }
}
