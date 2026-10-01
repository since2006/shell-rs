//! Private keys ShellRS is given or makes itself: a key pasted into the
//! credential form, a freshly generated one, the public half people put on
//! their servers, and the files ShellRS keeps such keys in.
//!
//! A key that came as text has no file of its own, so ShellRS writes one
//! into its data directory and the credential refers to that file like to
//! any other. Logging in never needs to know where a key came from.

use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use zeroize::Zeroizing;

use super::PublicId;

/// The kinds of key the credential form generates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyAlgorithm {
    /// Short, fast, and what OpenSSH itself suggests.
    #[default]
    Ed25519,
    /// For servers too old for Ed25519 (OpenSSH before 6.5).
    Rsa,
}

impl KeyAlgorithm {
    /// Every algorithm, in the order the form lists them.
    pub const ALL: [KeyAlgorithm; 2] = [KeyAlgorithm::Ed25519, KeyAlgorithm::Rsa];

    pub fn label(self) -> &'static str {
        match self {
            KeyAlgorithm::Ed25519 => "Ed25519",
            KeyAlgorithm::Rsa => "RSA 4096",
        }
    }

    fn algorithm(self) -> Algorithm {
        match self {
            KeyAlgorithm::Ed25519 => Algorithm::Ed25519,
            // 4096 bits, the size `ssh-key` makes.
            KeyAlgorithm::Rsa => Algorithm::Rsa { hash: None },
        }
    }
}

/// A key pair the credential form just made, not saved anywhere yet.
pub struct GeneratedKey {
    key: PrivateKey,
}

impl GeneratedKey {
    /// Blocks: finding the primes of an RSA key takes a while.
    pub fn generate(algorithm: KeyAlgorithm) -> anyhow::Result<Self> {
        let mut rng = russh::keys::key::safe_rng();
        Ok(Self {
            key: PrivateKey::random(&mut rng, algorithm.algorithm())?,
        })
    }

    /// The public half as one line of `authorized_keys`, ending in
    /// `comment` when there is one.
    pub fn public_key_line(&self, comment: &str) -> String {
        public_key_line(self.key.public_key(), comment)
    }

    /// The text of an OpenSSH private key file, encrypted with `passphrase`
    /// unless that is empty.
    ///
    /// Blocks while it derives the encryption key from the passphrase,
    /// which is slow on purpose.
    pub fn encode(&self, comment: &str, passphrase: &str) -> anyhow::Result<Zeroizing<String>> {
        let mut key = self.key.clone();
        key.set_comment(comment.trim());
        if !passphrase.is_empty() {
            key = key.encrypt(&mut russh::keys::key::safe_rng(), passphrase)?;
        }
        Ok(key.to_openssh(LineEnding::LF)?)
    }
}

impl fmt::Debug for GeneratedKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeneratedKey")
            .field("algorithm", &self.key.algorithm())
            .finish_non_exhaustive()
    }
}

fn public_key_line(key: &PublicKey, comment: &str) -> String {
    let mut key = key.clone();
    key.set_comment(comment.trim());
    key.to_openssh().unwrap_or_default()
}

/// A private key as pasted into the credential form, with what could be
/// read from it without its passphrase.
pub struct PastedKey {
    /// The text as it goes into the key file: blanks around it trimmed,
    /// Windows line endings undone, one newline at the end.
    text: Zeroizing<String>,
    encrypted: bool,
    /// Known when the format keeps it in the clear, or the key is not
    /// encrypted.
    public_key: Option<PublicKey>,
}

/// Why pasted text is not a private key. `Display` is what the form says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PastedKeyError {
    Empty,
    /// One line of `id_ed25519.pub`, the half that goes on the server.
    PublicKey,
    Unreadable,
}

impl fmt::Display for PastedKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            PastedKeyError::Empty => "请粘贴私钥",
            PastedKeyError::PublicKey => "这是公钥，请粘贴私钥（以 -----BEGIN 开头的那一段）",
            PastedKeyError::Unreadable => "无法识别这段私钥，支持 OpenSSH、PEM 和 PuTTY 格式",
        })
    }
}

impl std::error::Error for PastedKeyError {}

impl PastedKey {
    pub fn parse(text: &str) -> Result<Self, PastedKeyError> {
        let unix = Zeroizing::new(text.replace("\r\n", "\n"));
        let trimmed = unix.trim();
        if trimmed.is_empty() {
            return Err(PastedKeyError::Empty);
        }
        let file = Zeroizing::new(format!("{trimmed}\n"));
        let first_line = trimmed.lines().next().unwrap_or_default();
        if PublicKey::from_openssh(first_line).is_ok() {
            return Err(PastedKeyError::PublicKey);
        }
        // OpenSSH's own format keeps the public half outside the encrypted
        // part, so it is known even before the passphrase is.
        if trimmed.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
            let key = PrivateKey::from_openssh(trimmed).map_err(|_| PastedKeyError::Unreadable)?;
            return Ok(Self {
                text: file,
                encrypted: key.is_encrypted(),
                public_key: Some(key.public_key().clone()),
            });
        }
        match russh::keys::decode_secret_key(trimmed, None) {
            Ok(key) => Ok(Self {
                text: file,
                encrypted: false,
                public_key: Some(key.public_key().clone()),
            }),
            Err(russh::keys::Error::KeyIsEncrypted) => Ok(Self::encrypted(file)),
            Err(_) if looks_encrypted(trimmed) => Ok(Self::encrypted(file)),
            Err(_) => Err(PastedKeyError::Unreadable),
        }
    }

    fn encrypted(text: Zeroizing<String>) -> Self {
        Self {
            text,
            encrypted: true,
            public_key: None,
        }
    }

    /// What the key file holds.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the key needs a passphrase to be used.
    pub fn is_encrypted(&self) -> bool {
        self.encrypted
    }

    /// The public half as one line of `authorized_keys`, with the key's own
    /// comment, when the text gives it away without the passphrase.
    pub fn public_key_line(&self) -> Option<String> {
        self.public_key
            .as_ref()
            .and_then(|key| key.to_openssh().ok())
    }
}

impl fmt::Debug for PastedKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PastedKey")
            .field("text", &"<redacted>")
            .field("encrypted", &self.encrypted)
            .finish()
    }
}

/// Whether a key that would not open without a passphrase says it is
/// encrypted, in the formats that do not hand that back as an error:
/// PKCS#8's `ENCRYPTED PRIVATE KEY`, the older PEM `Proc-Type: 4,ENCRYPTED`
/// and PuTTY's `Encryption:` header.
fn looks_encrypted(text: &str) -> bool {
    text.lines().any(|line| {
        let line = line.trim();
        line == "-----BEGIN ENCRYPTED PRIVATE KEY-----"
            || line == "Proc-Type: 4,ENCRYPTED"
            || line
                .strip_prefix("Encryption:")
                .is_some_and(|cipher| cipher.trim() != "none")
    })
}

/// The public half of the key in the file at `path`, as one line of
/// `authorized_keys`: read from the private key when its format keeps the
/// public half in the clear, else from `<path>.pub` beside it.
///
/// Blocks on the file system.
pub fn read_public_key(path: &Path) -> Option<String> {
    let from_key = read_small(path)
        .and_then(|text| PastedKey::parse(&text).ok())
        .and_then(|key| key.public_key_line());
    from_key.or_else(|| {
        let mut public_path = path.as_os_str().to_owned();
        public_path.push(".pub");
        let text = read_small(Path::new(&public_path))?;
        let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
        PublicKey::from_openssh(line).ok()?.to_openssh().ok()
    })
}

/// A file's text, if it is small enough to be a key. Anything else that was
/// picked by mistake is not read whole.
fn read_small(path: &Path) -> Option<Zeroizing<String>> {
    const LARGEST_KEY: u64 = 64 * 1024;
    let mut file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > LARGEST_KEY {
        return None;
    }
    let mut text = Zeroizing::new(String::new());
    file.read_to_string(&mut text).ok()?;
    Some(text)
}

/// Write a private key file into `dir`, where ShellRS keeps the keys it was
/// given or made: over `replace` when given, else under a new random name.
/// Only the user may read it, as `ssh` asks of every key. Returns where it
/// went.
///
/// The text goes to a temporary file first, so a login reading the key at
/// that moment sees the old key or the new one, never half of either.
pub fn write_key_file(dir: &Path, replace: Option<&Path>, text: &str) -> io::Result<PathBuf> {
    create_private_dir(dir)?;
    let path = match replace {
        Some(path) => path.to_path_buf(),
        None => unused_name(dir),
    };
    let temporary = dir.join(format!(".{}.tmp", PublicId::generate()));
    let written = create_private_file(&temporary).and_then(|mut file| {
        file.write_all(text.as_bytes())?;
        file.sync_all()
    });
    let result = written.and_then(|()| std::fs::rename(&temporary, &path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map(|()| path)
}

/// Whether `path` is one of the key files ShellRS keeps in `dir`.
pub fn is_kept_in(dir: &Path, path: &Path) -> bool {
    path.parent() == Some(dir)
}

fn unused_name(dir: &Path) -> PathBuf {
    loop {
        let path = dir.join(PublicId::generate().as_str());
        if !path.exists() {
            return path;
        }
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// On Windows the data directory is the user's own, and a new file takes
/// its permissions.
#[cfg(not(unix))]
fn create_private_file(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed25519() -> GeneratedKey {
        GeneratedKey::generate(KeyAlgorithm::Ed25519).unwrap()
    }

    fn public_half(text: &str) -> PublicKey {
        PrivateKey::from_openssh(text).unwrap().public_key().clone()
    }

    #[test]
    fn a_generated_key_reads_back_from_its_file_text() {
        let key = ed25519();
        let text = key.encode(" 部署 ", "").unwrap();
        let pasted = PastedKey::parse(&text).unwrap();
        assert!(!pasted.is_encrypted());
        let line = key.public_key_line(" 部署 ");
        assert!(line.starts_with("ssh-ed25519 "), "{line}");
        assert!(line.ends_with(" 部署"), "{line}");
        assert_eq!(pasted.public_key_line(), Some(line));
        // The comment written into the file is the one the line shows.
        let decoded = russh::keys::decode_secret_key(&text, None).unwrap();
        assert_eq!(decoded.comment().as_str_lossy(), "部署");
    }

    #[test]
    fn a_passphrase_encrypts_the_key_and_opens_it_again() {
        let key = ed25519();
        let plain = key.encode("部署", "").unwrap();
        let text = key.encode("部署", "correct horse").unwrap();
        let pasted = PastedKey::parse(&text).unwrap();
        assert!(pasted.is_encrypted());
        // OpenSSH keeps the public half outside the encryption, without
        // the comment, which is encrypted along with the key.
        assert_eq!(pasted.public_key_line(), Some(key.public_key_line("")));
        assert!(matches!(
            russh::keys::decode_secret_key(&text, None),
            Err(russh::keys::Error::KeyIsEncrypted)
        ));
        let opened = russh::keys::decode_secret_key(&text, Some("correct horse")).unwrap();
        assert_eq!(opened.public_key(), &public_half(&plain));
    }

    #[test]
    fn an_rsa_key_is_generated_too() {
        let key = GeneratedKey::generate(KeyAlgorithm::Rsa).unwrap();
        assert!(key.public_key_line("").starts_with("ssh-rsa "));
    }

    #[test]
    fn pasted_text_is_tidied_into_a_file() {
        let text = ed25519().encode("", "").unwrap();
        let windows = format!("\r\n  {}  \r\n\r\n", text.trim().replace('\n', "\r\n"));
        let pasted = PastedKey::parse(&windows).unwrap();
        assert_eq!(pasted.text(), text.as_str());
        assert!(!pasted.text().contains('\r'));
    }

    #[test]
    fn pasting_something_else_says_what_it_is() {
        assert_eq!(PastedKey::parse(" \n ").unwrap_err(), PastedKeyError::Empty);
        let public = ed25519().public_key_line("me@laptop");
        assert_eq!(
            PastedKey::parse(&public).unwrap_err(),
            PastedKeyError::PublicKey
        );
        assert_eq!(
            PastedKey::parse("hunter2").unwrap_err(),
            PastedKeyError::Unreadable
        );
        assert_eq!(
            PastedKey::parse(
                "-----BEGIN OPENSSH PRIVATE KEY-----\nnot base64\n-----END OPENSSH PRIVATE KEY-----"
            )
            .unwrap_err(),
            PastedKeyError::Unreadable
        );
    }

    #[test]
    fn older_encrypted_formats_are_taken_without_their_public_half() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00112233445566778899AABBCCDDEEFF\n\nAAAA\n-----END RSA PRIVATE KEY-----";
        let pasted = PastedKey::parse(pem).unwrap();
        assert!(pasted.is_encrypted());
        assert_eq!(pasted.public_key_line(), None);
        let pkcs8 =
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----";
        assert!(PastedKey::parse(pkcs8).unwrap().is_encrypted());
    }

    #[test]
    fn the_debug_output_never_shows_the_key() {
        let key = ed25519();
        assert!(!format!("{key:?}").contains("OPENSSH"));
        let text = key.encode("", "").unwrap();
        let pasted = PastedKey::parse(&text).unwrap();
        let debug = format!("{pasted:?}");
        assert!(!debug.contains("OPENSSH"), "{debug}");
    }

    #[test]
    fn the_public_key_comes_from_the_key_file_or_the_pub_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let key = ed25519();

        let plain = dir.path().join("id_plain");
        std::fs::write(&plain, key.encode("plain", "").unwrap()).unwrap();
        assert_eq!(read_public_key(&plain), Some(key.public_key_line("plain")));

        // An older format that hides the public half: the .pub has it.
        let hidden = dir.path().join("id_hidden");
        std::fs::write(
            &hidden,
            "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n",
        )
        .unwrap();
        assert_eq!(read_public_key(&hidden), None);
        let line = key.public_key_line("me@laptop");
        std::fs::write(dir.path().join("id_hidden.pub"), format!("{line}\n")).unwrap();
        assert_eq!(read_public_key(&hidden), Some(line));

        assert_eq!(read_public_key(&dir.path().join("missing")), None);
    }

    #[test]
    fn a_key_file_is_new_and_private_unless_it_replaces_one() {
        let data = tempfile::tempdir().unwrap();
        let dir = data.path().join("keys");
        let first = write_key_file(&dir, None, "one").unwrap();
        let second = write_key_file(&dir, None, "two").unwrap();
        assert_ne!(first, second);
        assert!(is_kept_in(&dir, &first));
        assert!(!is_kept_in(&dir, data.path()));
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");

        let replaced = write_key_file(&dir, Some(&first), "three").unwrap();
        assert_eq!(replaced, first);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "three");
        // Nothing is left behind from writing through a temporary file.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&first), 0o600);
            assert_eq!(mode(&second), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
    }
}
