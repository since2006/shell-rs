//! Names: a device's, a record kind's and a record's.

use std::cmp::Ordering;
use std::fmt;

use crate::{Env, SyncError};

/// A device in a vault: 16 random characters from `[a-z0-9]` (about 82
/// bits), drawn once when the device first syncs and kept with its state.
/// It names the device's file, so it is lower case: macOS, Windows and most
/// cloud drives do not tell `A.srs` from `a.srs`. Nothing about the machine
/// goes into it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId(String);

impl DeviceId {
    pub const LEN: usize = 16;
    const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";

    pub fn generate(env: &mut dyn Env) -> Result<Self, SyncError> {
        let mut id = String::with_capacity(Self::LEN);
        while id.len() < Self::LEN {
            let mut bytes = [0u8; 32];
            env.fill_random(&mut bytes)?;
            // 252 is the largest multiple of 36 a byte holds: taking only
            // bytes below it keeps every character equally likely.
            for byte in bytes {
                if byte < 252 && id.len() < Self::LEN {
                    id.push(char::from(Self::ALPHABET[usize::from(byte % 36)]));
                }
            }
        }
        Ok(Self(id))
    }

    pub fn parse(text: &str) -> Option<Self> {
        let shaped = text.len() == Self::LEN
            && text
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        shaped.then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a record is. A kind this version does not know is kept as `Other`
/// and passed on untouched. `secret` and `secret.*` are secrets: the known
/// ones are `Secret…`, and one this version does not know (`Secret`) is
/// kept sealed like them, never given to the app.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Group,
    Credential,
    Host,
    Bookmark,
    Forward,
    SnippetCategory,
    Snippet,
    /// An endpoint's sign-in password (SSH, Telnet).
    SecretPassword,
    /// A proxy's password.
    SecretProxy,
    /// A password credential's password; its id is the credential's.
    SecretCredential,
    /// A key credential's passphrase; its id is the credential's.
    SecretPassphrase,
    /// A key credential's private key; its id is the credential's.
    SecretPrivateKey,
    /// The secret of a host's one-time codes; its id is the host's.
    SecretTotp,
    Secret(String),
    Other(String),
}

impl Kind {
    /// Every kind this version knows, in the order the app takes them in:
    /// what a record points at comes before it.
    pub const KNOWN: [Kind; 13] = [
        Kind::Group,
        Kind::Credential,
        Kind::Host,
        Kind::Bookmark,
        Kind::Forward,
        Kind::SnippetCategory,
        Kind::Snippet,
        Kind::SecretPassword,
        Kind::SecretProxy,
        Kind::SecretCredential,
        Kind::SecretPassphrase,
        Kind::SecretPrivateKey,
        Kind::SecretTotp,
    ];

    /// The kind written as `name`, or `None` when it is not shaped like a
    /// kind: 1 to 32 of `[a-z0-9_.]`, starting with a letter.
    pub fn parse(name: &str) -> Option<Self> {
        let shaped = (1..=32).contains(&name.len())
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.');
        if !shaped {
            return None;
        }
        Some(match name {
            "group" => Self::Group,
            "credential" => Self::Credential,
            "host" => Self::Host,
            "bookmark" => Self::Bookmark,
            "forward" => Self::Forward,
            "snippet_category" => Self::SnippetCategory,
            "snippet" => Self::Snippet,
            "secret.password" => Self::SecretPassword,
            "secret.proxy" => Self::SecretProxy,
            "secret.credential" => Self::SecretCredential,
            "secret.passphrase" => Self::SecretPassphrase,
            "secret.private_key" => Self::SecretPrivateKey,
            "secret.totp" => Self::SecretTotp,
            secret if secret == "secret" || secret.starts_with("secret.") => {
                Self::Secret(secret.to_owned())
            }
            other => Self::Other(other.to_owned()),
        })
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Group => "group",
            Self::Credential => "credential",
            Self::Host => "host",
            Self::Bookmark => "bookmark",
            Self::Forward => "forward",
            Self::SnippetCategory => "snippet_category",
            Self::Snippet => "snippet",
            Self::SecretPassword => "secret.password",
            Self::SecretProxy => "secret.proxy",
            Self::SecretCredential => "secret.credential",
            Self::SecretPassphrase => "secret.passphrase",
            Self::SecretPrivateKey => "secret.private_key",
            Self::SecretTotp => "secret.totp",
            Self::Secret(name) | Self::Other(name) => name,
        }
    }

    pub fn is_known(&self) -> bool {
        !matches!(self, Self::Secret(_) | Self::Other(_))
    }

    /// Whether records of this kind hold a secret: kept sealed in the
    /// state, never shown in the clear.
    pub fn is_secret(&self) -> bool {
        matches!(
            self,
            Self::SecretPassword
                | Self::SecretProxy
                | Self::SecretCredential
                | Self::SecretPassphrase
                | Self::SecretPrivateKey
                | Self::SecretTotp
                | Self::Secret(_)
        )
    }

    /// Where the kind comes in `KNOWN`; unknown kinds come last.
    pub(crate) fn rank(&self) -> usize {
        Self::KNOWN
            .iter()
            .position(|kind| kind == self)
            .unwrap_or(Self::KNOWN.len())
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl PartialOrd for Kind {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Kind {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

/// One record: its kind and its id within the kind. The app's own ids are
/// used where it has them (a host's public id); ids are 1 to 64 of
/// `[A-Za-z0-9_-]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordKey {
    pub kind: Kind,
    pub id: String,
}

impl RecordKey {
    pub fn new(kind: Kind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub fn is_valid_id(id: &str) -> bool {
        (1..=64).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    }
}

impl fmt::Display for RecordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixedEnv;

    #[test]
    fn device_ids_are_lower_case_and_parse_back() {
        let mut env = FixedEnv::new(0, 7);
        let id = DeviceId::generate(&mut env).unwrap();
        assert_eq!(id.as_str().len(), 16);
        assert_eq!(DeviceId::parse(id.as_str()), Some(id.clone()));
        assert_ne!(DeviceId::generate(&mut env).unwrap(), id);
        assert_eq!(DeviceId::parse("ABCDEFGHIJKLMNOP"), None);
        assert_eq!(DeviceId::parse("abc"), None);
        assert_eq!(DeviceId::parse("abcdefghijklmno-"), None);
    }

    #[test]
    fn kinds_read_back_and_secrets_are_set_apart() {
        for kind in Kind::KNOWN {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind.clone()));
            assert!(kind.is_known());
            assert_eq!(kind.is_secret(), kind.as_str().starts_with("secret."));
        }
        assert_eq!(Kind::parse("secret.password"), Some(Kind::SecretPassword));
        assert_eq!(Kind::parse("secret"), Some(Kind::Secret("secret".into())));
        let future = Kind::parse("secret.future").unwrap();
        assert_eq!(future, Kind::Secret("secret.future".into()));
        assert!(future.is_secret() && !future.is_known());
        assert_eq!(Kind::parse("secrets"), Some(Kind::Other("secrets".into())));
        assert_eq!(Kind::parse("tag"), Some(Kind::Other("tag".into())));
        assert_eq!(Kind::parse("Host"), None);
        assert_eq!(Kind::parse(""), None);
        assert_eq!(Kind::parse("9lives"), None);
    }

    #[test]
    fn record_ids_have_a_shape() {
        assert!(RecordKey::is_valid_id("Jwg5rHvXCxw89paM"));
        assert!(RecordKey::is_valid_id("a_b-c"));
        assert!(!RecordKey::is_valid_id(""));
        assert!(!RecordKey::is_valid_id("a/b"));
        assert!(!RecordKey::is_valid_id(&"a".repeat(65)));
    }
}
