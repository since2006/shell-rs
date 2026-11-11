//! A setup code: where sync goes and the login to it, sealed for whoever
//! knows the sync password. A device in sync shows it as text and as a QR
//! code; another pastes or scans it, asks for the sync password, and joins
//! without the rest being typed again.
//!
//! ```text
//! "shellrs-sync:" then, as unpadded base64url:
//!   "SRSC" | version (1 byte, 1) | memory in KiB (4) | passes (4) | lanes (4) | salt (16) | nonce (24)
//!   XChaCha20-Poly1305 ciphertext and tag
//!   check (4): the first bytes of the SHA-256 of all before it
//! ```
//!
//! The key comes from the vault's key-encryption key, so the device that
//! makes a code asks for no password, and the code carries the cost and the
//! salt the password takes: whoever has both derives the same key. What a
//! code holds is the caller's (the app's sync location and its login, as
//! JSON). A code is never written to the store.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::env::random_array;
use crate::store::SyncStore;
use crate::vault::{self, KdfParams, Keyring};
use crate::{Env, SyncError};

const PREFIX: &str = "shellrs-sync:";
const MAGIC: &[u8; 4] = b"SRSC";
/// The prefix and as much of the base64 as the magic alone decides.
const START: &str = "shellrs-sync:U1JTQ";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 4 + 1 + 4 + 4 + 4 + 16 + 24;
const TAG_LEN: usize = 16;
const CHECK_LEN: usize = 4;
/// The longest code, in characters: more than a QR code holds.
const MAX_CHARS: usize = 4096;

/// Seal `plain` as a setup code of the vault in `store`, with the keys
/// this device holds: it opens with the sync password of now.
/// `NeedsPassword` or `VaultReplaced` when the keys are no longer the
/// vault's; `BadSetupCode` for more than a code holds.
pub fn seal_setup_code(
    store: &dyn SyncStore,
    keyring: &Keyring,
    plain: &[u8],
    env: &mut dyn Env,
) -> Result<String, SyncError> {
    let vault::CodeKey { params, salt, key } = vault::code_key_of(store, keyring)?;
    let nonce: [u8; 24] = random_array(env)?;
    let mut bytes = Vec::with_capacity(HEADER_LEN + plain.len() + TAG_LEN + CHECK_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(&params.memory_kib.to_be_bytes());
    bytes.extend_from_slice(&params.passes.to_be_bytes());
    bytes.extend_from_slice(&params.parallelism.to_be_bytes());
    bytes.extend_from_slice(&salt);
    bytes.extend_from_slice(&nonce);
    let sealed = XChaCha20Poly1305::new_from_slice(&*key)
        .expect("32-byte key")
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: plain,
                aad: &bytes,
            },
        )
        .map_err(|_| SyncError::Random)?;
    bytes.extend_from_slice(&sealed);
    let check = check(&bytes);
    bytes.extend_from_slice(&check);
    let code = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(&bytes));
    if code.len() > MAX_CHARS {
        return Err(SyncError::BadSetupCode);
    }
    Ok(code)
}

/// What `seal_setup_code` sealed in `code`, opened with the sync password.
/// White space anywhere in the text is passed over: a pasted code may come
/// wrapped. `SetupCodeWrongPassword` when the password does not open it,
/// which a code made before the password changed looks like too.
pub fn open_setup_code(code: &str, password: &str) -> Result<Zeroizing<Vec<u8>>, SyncError> {
    let code = parse(code)?;
    let key = vault::code_key_from(password, &code.salt, code.params)
        .map_err(|_| SyncError::BadSetupCode)?;
    XChaCha20Poly1305::new_from_slice(&*key)
        .expect("32-byte key")
        .decrypt(
            &XNonce::from(code.nonce),
            Payload {
                msg: &code.sealed,
                aad: &code.header,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| SyncError::SetupCodeWrongPassword)
}

/// What is wrong with `text` as a setup code, as far as can be told
/// without the password: to say so before asking for it.
pub fn check_setup_code(text: &str) -> Result<(), SyncError> {
    parse(text).map(drop)
}

/// Whether `text` starts as a setup code does, of any version, whole or
/// not: to tell one from whatever else a camera reads. `check_setup_code`
/// says what is wrong with it.
pub fn is_setup_code(text: &str) -> bool {
    let mut chars = text.chars().filter(|c| !c.is_whitespace());
    START.chars().all(|c| chars.next() == Some(c))
}

/// A code of this version, whole, not yet opened.
struct Parsed {
    header: Vec<u8>,
    params: KdfParams,
    salt: [u8; 16],
    nonce: [u8; 24],
    sealed: Vec<u8>,
}

fn parse(text: &str) -> Result<Parsed, SyncError> {
    let bytes = bytes_of(text).ok_or(SyncError::BadSetupCode)?;
    // The version comes first: a newer one may be laid out otherwise.
    let version = match bytes.strip_prefix(MAGIC).and_then(<[u8]>::first) {
        Some(version) => *version,
        None => return Err(SyncError::BadSetupCode),
    };
    if version > VERSION {
        return Err(SyncError::SetupCodeTooNew);
    }
    if version < VERSION || bytes.len() < HEADER_LEN + TAG_LEN + CHECK_LEN {
        return Err(SyncError::BadSetupCode);
    }
    // Then the check, so a code cut short is not taken for a wrong
    // password.
    let (body, checked) = bytes.split_at(bytes.len() - CHECK_LEN);
    if check(body) != checked {
        return Err(SyncError::BadSetupCode);
    }
    let (header, sealed) = body.split_at(HEADER_LEN);
    let word = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().expect("4 bytes"));
    let params = KdfParams {
        memory_kib: word(5),
        passes: word(9),
        parallelism: word(13),
    };
    // A cost a vault may not ask for is refused before it is paid.
    if params.argon2().is_err() {
        return Err(SyncError::BadSetupCode);
    }
    Ok(Parsed {
        header: header.to_vec(),
        params,
        salt: header[17..33].try_into().expect("16 bytes"),
        nonce: header[33..].try_into().expect("24 bytes"),
        sealed: sealed.to_vec(),
    })
}

/// The bytes `text` is the base64 of, after the prefix.
fn bytes_of(text: &str) -> Option<Vec<u8>> {
    let mut code = String::new();
    for c in text.chars().filter(|c| !c.is_whitespace()) {
        if code.len() >= MAX_CHARS {
            return None;
        }
        code.push(c);
    }
    URL_SAFE_NO_PAD.decode(code.strip_prefix(PREFIX)?).ok()
}

fn check(bytes: &[u8]) -> [u8; CHECK_LEN] {
    let hash = Sha256::digest(bytes);
    hash[..CHECK_LEN].try_into().expect("4 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use crate::vault::tests::{FAST, vault};
    use crate::{FixedEnv, change_password, create_vault, layout, unlock_vault};

    const PLAIN: &[u8] = br#"{"location":{"kind":"webdav"},"login":"hunter2"}"#;

    /// A vault whose password is "correct horse", and a code of it.
    fn code() -> (MemoryStore, FixedEnv, Keyring, String) {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 95);
        let keys = vault(&store, &mut env);
        let code = seal_setup_code(&store, &keys, PLAIN, &mut env).unwrap();
        (store, env, keys, code)
    }

    fn bytes(code: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD
            .decode(code.strip_prefix(PREFIX).unwrap())
            .unwrap()
    }

    fn text(bytes: &[u8]) -> String {
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
    }

    /// `bytes` with their check made right again.
    fn checked(mut bytes: Vec<u8>) -> String {
        let body = bytes.len() - CHECK_LEN;
        let check = check(&bytes[..body]);
        bytes[body..].copy_from_slice(&check);
        text(&bytes)
    }

    #[test]
    fn a_code_opens_with_the_sync_password_only() {
        let (_, _, _, code) = code();
        assert_eq!(&**open_setup_code(&code, "correct horse").unwrap(), PLAIN);
        assert_eq!(
            open_setup_code(&code, "wrong horse").unwrap_err(),
            SyncError::SetupCodeWrongPassword
        );
        assert_eq!(check_setup_code(&code), Ok(()));
        assert!(is_setup_code(&code));
        // Typed composed or decomposed, as the vault's.
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        let keys = create_vault(&store, "caf\u{e9}", FAST, &mut env).unwrap();
        let code = seal_setup_code(&store, &keys, PLAIN, &mut env).unwrap();
        assert!(open_setup_code(&code, "cafe\u{301}").is_ok());
    }

    /// The layout is the format: what `FORMAT.md` says, byte for byte.
    #[test]
    fn a_code_is_laid_out_as_the_format_says() {
        let (store, _, _, code) = code();
        assert!(code.starts_with("shellrs-sync:"));
        assert!(
            code["shellrs-sync:".len()..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let bytes = bytes(&code);
        assert_eq!(bytes.len(), 57 + PLAIN.len() + 16 + 4);
        assert_eq!(&bytes[..4], b"SRSC");
        assert_eq!(bytes[4], 1);
        assert_eq!(bytes[5..9], FAST.memory_kib.to_be_bytes());
        assert_eq!(bytes[9..13], FAST.passes.to_be_bytes());
        assert_eq!(bytes[13..17], FAST.parallelism.to_be_bytes());
        let vault: serde_json::Value =
            serde_json::from_slice(&store.raw(layout::VAULT).unwrap()).unwrap();
        assert_eq!(
            crate::hex::encode(&bytes[17..33]),
            vault["kdf"]["salt"].as_str().unwrap()
        );
        let body = bytes.len() - 4;
        assert_eq!(bytes[body..], Sha256::digest(&bytes[..body])[..4]);
        assert!(!bytes.windows(7).any(|w| w == b"hunter2"));
    }

    #[test]
    fn any_changed_byte_is_noticed() {
        let (_, _, _, code) = code();
        let whole = bytes(&code);
        for i in 0..whole.len() {
            let mut bad = whole.clone();
            bad[i] ^= 1;
            assert!(
                open_setup_code(&text(&bad), "correct horse").is_err(),
                "byte {i}"
            );
        }
        // And by more than the check: the salt, the nonce and what is
        // sealed do not open once changed, however the check is made up.
        for i in 17..whole.len() - CHECK_LEN {
            let mut bad = whole.clone();
            bad[i] ^= 1;
            assert_eq!(
                open_setup_code(&checked(bad), "correct horse").unwrap_err(),
                SyncError::SetupCodeWrongPassword,
                "byte {i}"
            );
        }
    }

    /// A code cut short while copying is told as that, not as a wrong
    /// password.
    #[test]
    fn a_code_cut_short_or_not_one_is_refused() {
        let (_, _, _, code) = code();
        for cut in 0..code.len() {
            assert_eq!(
                open_setup_code(&code[..cut], "correct horse").unwrap_err(),
                SyncError::BadSetupCode,
                "cut at {cut}"
            );
        }
        for other in [
            "",
            "hello",
            "https://example.com/",
            "shellrs-sync:",
            "shellrs-sync:not base64!",
            "shellrs-sync:aGVsbG8",
            "SHELLRS-SYNC:U1JTQwE",
        ] {
            assert_eq!(
                check_setup_code(other),
                Err(SyncError::BadSetupCode),
                "{other}"
            );
            assert!(!is_setup_code(other), "{other}");
        }
        // Padded, or a version before the first.
        assert_eq!(
            check_setup_code(&format!("{code}=")),
            Err(SyncError::BadSetupCode)
        );
        let mut old = bytes(&code);
        old[4] = 0;
        assert_eq!(
            check_setup_code(&checked(old)),
            Err(SyncError::BadSetupCode)
        );
        // One cut short is a code still, to whoever only looks.
        assert_eq!(&text(MAGIC)[..START.len()], START);
        assert!(is_setup_code(&code[..40]));
        assert!(is_setup_code(START));
        assert!(!is_setup_code(&START[..START.len() - 1]));
    }

    /// A newer version may be laid out otherwise: nothing after its
    /// version is read.
    #[test]
    fn a_newer_version_is_told_before_anything_else() {
        let (_, _, _, code) = code();
        let mut newer = bytes(&code);
        newer[4] = 2;
        for code in [text(&newer), text(b"SRSC\x02"), text(b"SRSC\xffwhatever")] {
            assert_eq!(check_setup_code(&code), Err(SyncError::SetupCodeTooNew));
            assert_eq!(
                open_setup_code(&code, "correct horse").unwrap_err(),
                SyncError::SetupCodeTooNew
            );
            assert!(is_setup_code(&code));
        }
    }

    /// Were the cost paid first, these would end as a wrong password.
    #[test]
    fn costs_out_of_bounds_are_refused_before_they_are_paid() {
        let (_, _, _, code) = code();
        let whole = bytes(&code);
        for (at, value) in [(5, 512u32 * 1024), (9, 11), (9, 0), (13, 5), (13, 0)] {
            let mut bad = whole.clone();
            bad[at..at + 4].copy_from_slice(&value.to_be_bytes());
            let bad = checked(bad);
            assert_eq!(check_setup_code(&bad), Err(SyncError::BadSetupCode));
            assert_eq!(
                open_setup_code(&bad, "correct horse").unwrap_err(),
                SyncError::BadSetupCode,
                "{value} at {at}"
            );
        }
    }

    #[test]
    fn white_space_in_a_pasted_code_is_passed_over() {
        let (_, _, _, code) = code();
        let mut wrapped = String::from("  \n");
        for (i, c) in code.chars().enumerate() {
            wrapped.push(c);
            if i % 20 == 19 {
                wrapped.push_str("\r\n\t ");
            }
        }
        wrapped.push_str("\u{a0}\n");
        assert_eq!(
            &**open_setup_code(&wrapped, "correct horse").unwrap(),
            PLAIN
        );
        assert!(is_setup_code(&wrapped));
    }

    #[test]
    fn no_code_is_made_longer_than_one_is_read() {
        let (store, mut env, keys, _) = code();
        let fits = vec![b'x'; 2900];
        let made = seal_setup_code(&store, &keys, &fits, &mut env).unwrap();
        assert_eq!(&**open_setup_code(&made, "correct horse").unwrap(), fits);
        assert_eq!(
            seal_setup_code(&store, &keys, &[b'x'; 3100], &mut env).unwrap_err(),
            SyncError::BadSetupCode
        );
        let long = format!("{PREFIX}{}", "A".repeat(MAX_CHARS));
        assert_eq!(check_setup_code(&long), Err(SyncError::BadSetupCode));
        assert!(!is_setup_code(&long));
    }

    /// A code dies with the password it was made under, and none is made
    /// with keys the vault has left behind.
    #[test]
    fn a_code_made_before_a_password_change_opens_with_the_old_one_only() {
        let (store, mut env, keys, before) = code();
        let changed = change_password(&store, &keys, "new horse", FAST, &mut env).unwrap();
        assert_eq!(
            open_setup_code(&before, "new horse").unwrap_err(),
            SyncError::SetupCodeWrongPassword
        );
        assert!(open_setup_code(&before, "correct horse").is_ok());
        // The device that has not heard of the change makes none.
        assert_eq!(
            seal_setup_code(&store, &keys, PLAIN, &mut env).unwrap_err(),
            SyncError::NeedsPassword
        );
        let after = seal_setup_code(&store, &changed, PLAIN, &mut env).unwrap();
        assert_eq!(&**open_setup_code(&after, "new horse").unwrap(), PLAIN);
        assert_eq!(
            open_setup_code(&after, "correct horse").unwrap_err(),
            SyncError::SetupCodeWrongPassword
        );
        // What opens a code opens the vault it is of.
        assert!(unlock_vault(&store, "new horse").is_ok());
    }

    #[test]
    fn no_code_is_made_of_a_vault_that_is_gone_or_another() {
        let (store, mut env, keys, _) = code();
        store.remove_raw(layout::VAULT);
        assert_eq!(
            seal_setup_code(&store, &keys, PLAIN, &mut env).unwrap_err(),
            SyncError::VaultMissing
        );
        create_vault(&store, "correct horse", FAST, &mut env).unwrap();
        assert_eq!(
            seal_setup_code(&store, &keys, PLAIN, &mut env).unwrap_err(),
            SyncError::VaultReplaced
        );
    }

    /// Two codes of one vault differ, and both open.
    #[test]
    fn each_code_has_its_own_nonce() {
        let (store, mut env, keys, one) = code();
        let two = seal_setup_code(&store, &keys, PLAIN, &mut env).unwrap();
        assert_ne!(one, two);
        assert_eq!(&**open_setup_code(&two, "correct horse").unwrap(), PLAIN);
    }
}
