//! The keys: `vault.json`, shared by every device of the vault.
//!
//! The sync password derives a key-encryption key (Argon2id), which only
//! wraps random data keys; snapshots are encrypted with a data key. A
//! password change rewraps the same data keys, so no snapshot has to be
//! written again. A rotation adds a data key (a new generation) and keeps
//! the old ones wrapped, so files not yet rewritten stay readable; each
//! device writes its own with the newest key on its next upload.
//!
//! The file is plain JSON: the storage learns the parameters and the
//! wrapped keys, nothing it can open without the password.

use std::collections::BTreeMap;
use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version as ArgonVersion};
use chacha20poly1305::aead::{Aead as _, KeyInit as _, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use unicode_normalization::UnicodeNormalization as _;
use zeroize::Zeroizing;

use crate::env::random_array;
use crate::record::Map;
use crate::store::{PutCondition, SyncStore};
use crate::{Env, StoreError, SyncError, hex, layout};

/// The vault format this version writes and reads.
const VAULT_FORMAT: u32 = 1;
const MAX_VAULT_BYTES: u64 = 1 << 20;

/// Argon2id's cost, kept in `vault.json` so it can be raised later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub memory_kib: u32,
    pub passes: u32,
    pub parallelism: u32,
}

impl KdfParams {
    /// 64 MiB and three passes: about a second on a phone.
    pub const DEFAULT: Self = Self {
        memory_kib: 64 * 1024,
        passes: 3,
        parallelism: 1,
    };
    /// The most a vault may ask for, so a tampered file cannot make a
    /// phone run out of memory or spin.
    pub const MAX: Self = Self {
        memory_kib: 256 * 1024,
        passes: 10,
        parallelism: 4,
    };

    pub(crate) fn argon2(self) -> Result<Argon2<'static>, SyncError> {
        let within = self.memory_kib <= Self::MAX.memory_kib
            && (1..=Self::MAX.passes).contains(&self.passes)
            && (1..=Self::MAX.parallelism).contains(&self.parallelism);
        if !within {
            return Err(SyncError::BadVault("key derivation out of bounds".into()));
        }
        let params = Params::new(self.memory_kib, self.passes, self.parallelism, Some(32))
            .map_err(|error| SyncError::BadVault(error.to_string()))?;
        Ok(Argon2::new(
            Algorithm::Argon2id,
            ArgonVersion::V0x13,
            params,
        ))
    }
}

/// The keys of a vault this device can use: the data keys of every
/// generation, and the key-encryption key, to read new generations
/// without asking for the password again. The app keeps it in the system
/// keychain only (`to_bytes`, the newest generations), never in its
/// database.
#[derive(Clone)]
pub struct Keyring {
    vault_id: [u8; 16],
    kek: Zeroizing<[u8; 32]>,
    keys: DataKeys,
    current: u32,
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keyring")
            .field("vault_id", &hex::encode(&self.vault_id))
            .field("generations", &self.keys.keys().collect::<Vec<_>>())
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

impl Keyring {
    const MAGIC: &[u8; 4] = b"SRK1";
    /// The generations `to_bytes` keeps, the newest: Windows' Credential
    /// Manager holds 2560 bytes, about 25 of them. An older key comes back
    /// from the vault, unwrapped with the key-encryption key, when a file
    /// or the state still needs it.
    const KEPT_GENERATIONS: usize = 8;

    pub fn current_generation(&self) -> u32 {
        self.current
    }

    /// The vault's id, which tells a vault from one made after a reset.
    pub fn vault_id(&self) -> String {
        hex::encode(&self.vault_id)
    }

    pub(crate) fn vault_id_bytes(&self) -> [u8; 16] {
        self.vault_id
    }

    pub(crate) fn key(&self, generation: u32) -> Option<&[u8; 32]> {
        self.keys.get(&generation).map(|key| &**key)
    }

    /// The keyring for the system keychain, with the current generation
    /// and the newest before it.
    pub fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut kept: Vec<_> = self
            .keys
            .iter()
            .filter(|(generation, _)| **generation != self.current)
            .rev()
            .take(Self::KEPT_GENERATIONS - 1)
            .chain(self.keys.get_key_value(&self.current))
            .collect();
        kept.sort_by_key(|(generation, _)| **generation);
        let mut out = Zeroizing::new(Vec::with_capacity(60 + kept.len() * 36));
        out.extend_from_slice(Self::MAGIC);
        out.extend_from_slice(&self.vault_id);
        out.extend_from_slice(&*self.kek);
        out.extend_from_slice(&self.current.to_be_bytes());
        out.extend_from_slice(&(kept.len() as u32).to_be_bytes());
        for (generation, key) in kept {
            out.extend_from_slice(&generation.to_be_bytes());
            out.extend_from_slice(&**key);
        }
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SyncError> {
        let bad = || SyncError::StateUnreadable("not a keyring".into());
        let rest = bytes.strip_prefix(Self::MAGIC).ok_or_else(bad)?;
        let (vault_id, rest) = rest.split_first_chunk::<16>().ok_or_else(bad)?;
        let (kek, rest) = rest.split_first_chunk::<32>().ok_or_else(bad)?;
        let (current, rest) = rest.split_first_chunk::<4>().ok_or_else(bad)?;
        let (count, mut rest) = rest.split_first_chunk::<4>().ok_or_else(bad)?;
        let mut keys = BTreeMap::new();
        for _ in 0..u32::from_be_bytes(*count) {
            let (generation, after) = rest.split_first_chunk::<4>().ok_or_else(bad)?;
            let (key, after) = after.split_first_chunk::<32>().ok_or_else(bad)?;
            keys.insert(u32::from_be_bytes(*generation), Zeroizing::new(*key));
            rest = after;
        }
        let current = u32::from_be_bytes(*current);
        if !rest.is_empty() || !keys.contains_key(&current) {
            return Err(bad());
        }
        Ok(Self {
            vault_id: *vault_id,
            kek: Zeroizing::new(*kek),
            keys,
            current,
        })
    }
}

/// Data keys by generation.
type DataKeys = BTreeMap<u32, Zeroizing<[u8; 32]>>;

/// Whether a store has a vault, and what unlocking it will cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultStatus {
    Absent,
    Present { params: KdfParams },
}

/// The file as written. Fields this version does not know are kept in
/// `rest` and written back, so what a newer version added survives an
/// older one changing the password or rotating the key.
#[derive(Serialize, Deserialize)]
struct VaultFile {
    format: u32,
    vault_id: String,
    kdf: KdfFile,
    current: u32,
    keys: Vec<WrappedKey>,
    #[serde(flatten)]
    rest: Map,
}

/// Replaced whole, unknown fields too, when the password changes.
#[derive(Serialize, Deserialize)]
struct KdfFile {
    algorithm: String,
    version: u32,
    memory_kib: u32,
    passes: u32,
    parallelism: u32,
    salt: String,
    #[serde(flatten)]
    rest: Map,
}

/// Unknown fields describe the key, not its wrapping, so they stay when
/// the key is wrapped again.
#[derive(Serialize, Deserialize)]
struct WrappedKey {
    generation: u32,
    nonce: String,
    wrapped: String,
    #[serde(flatten)]
    rest: Map,
}

impl VaultFile {
    fn params(&self) -> Result<(KdfParams, [u8; 16]), SyncError> {
        let kdf = &self.kdf;
        if kdf.algorithm != "argon2id" || kdf.version != 0x13 {
            return Err(SyncError::BadVault("unknown key derivation".into()));
        }
        let salt = hex::decode_array(&kdf.salt).ok_or(SyncError::BadVault("bad salt".into()))?;
        let params = KdfParams {
            memory_kib: kdf.memory_kib,
            passes: kdf.passes,
            parallelism: kdf.parallelism,
        };
        params.argon2()?;
        Ok((params, salt))
    }

    fn vault_id(&self) -> Result<[u8; 16], SyncError> {
        hex::decode_array(&self.vault_id).ok_or(SyncError::BadVault("bad vault id".into()))
    }

    /// Every data key, unwrapped with `kek`; `None` when `kek` opens none
    /// of them (the wrong password).
    fn unwrap_all(
        &self,
        vault_id: &[u8; 16],
        kek: &[u8; 32],
    ) -> Result<Option<DataKeys>, SyncError> {
        let mut keys = BTreeMap::new();
        for wrapped in &self.keys {
            let nonce = hex::decode_array::<24>(&wrapped.nonce)
                .ok_or(SyncError::BadVault("bad nonce".into()))?;
            let sealed =
                hex::decode(&wrapped.wrapped).ok_or(SyncError::BadVault("bad key".into()))?;
            let aad = key_aad(vault_id, wrapped.generation);
            let cipher = XChaCha20Poly1305::new_from_slice(kek).expect("32-byte key");
            let opened = cipher.decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &sealed,
                    aad: &aad,
                },
            );
            match opened.ok().map(Zeroizing::new) {
                Some(key) => {
                    let key: [u8; 32] = key
                        .as_slice()
                        .try_into()
                        .map_err(|_| SyncError::BadVault("bad key length".into()))?;
                    keys.insert(wrapped.generation, Zeroizing::new(key));
                }
                None if keys.is_empty() && wrapped.generation == self.keys[0].generation => {
                    return Ok(None);
                }
                None => return Err(SyncError::BadVault("a key does not open".into())),
            }
        }
        if !keys.contains_key(&self.current) {
            return Err(SyncError::BadVault("no current key".into()));
        }
        Ok(Some(keys))
    }
}

/// What a wrapped key is bound to: its vault and generation, so a key
/// cannot be moved to another place in the file unnoticed.
fn key_aad(vault_id: &[u8; 16], generation: u32) -> Vec<u8> {
    let mut aad = b"shellrs-sync key".to_vec();
    aad.extend_from_slice(vault_id);
    aad.extend_from_slice(&generation.to_be_bytes());
    aad
}

fn derive(
    password: &str,
    salt: &[u8; 16],
    params: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, SyncError> {
    // The same password typed on a Mac and on Windows can differ in how
    // accented letters are composed; NFC makes them the same bytes.
    let password = Zeroizing::new(password.nfc().collect::<String>());
    let mut kek = Zeroizing::new([0u8; 32]);
    params
        .argon2()?
        .hash_password_into(password.as_bytes(), salt, &mut *kek)
        .map_err(|error| SyncError::BadVault(error.to_string()))?;
    Ok(kek)
}

/// The key of a setup code (`setup_code`), from the key-encryption key:
/// whoever knows the sync password can derive it, and a password change
/// leaves the codes made before it closed to the new one.
fn code_key(kek: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut mac = Hmac::<Sha256>::new_from_slice(kek).expect("a key of any length");
    mac.update(b"shellrs-sync setup code");
    Zeroizing::new(mac.finalize().into_bytes().into())
}

/// What a device of the vault seals a setup code with: the cost and the
/// salt the vault's password takes now, which the code carries, and the
/// code's key.
pub(crate) struct CodeKey {
    pub params: KdfParams,
    pub salt: [u8; 16],
    pub key: Zeroizing<[u8; 32]>,
}

/// The key of the setup codes of the vault in `store`, without the
/// password. `NeedsPassword` or `VaultReplaced` when the keyring is no
/// longer the vault's, so a code is never made that the password of today
/// does not open.
pub(crate) fn code_key_of(store: &dyn SyncStore, keyring: &Keyring) -> Result<CodeKey, SyncError> {
    let (file, _) = read(store)?;
    let (params, salt) = file.params()?;
    let current = current_keys(&file, keyring)?;
    Ok(CodeKey {
        params,
        salt,
        key: code_key(&current.kek),
    })
}

/// The key of a setup code that carries `salt` and `params`, from the
/// password typed.
pub(crate) fn code_key_from(
    password: &str,
    salt: &[u8; 16],
    params: KdfParams,
) -> Result<Zeroizing<[u8; 32]>, SyncError> {
    Ok(code_key(&*derive(password, salt, params)?))
}

fn wrap(
    kek: &[u8; 32],
    vault_id: &[u8; 16],
    generation: u32,
    key: &[u8; 32],
    env: &mut dyn Env,
) -> Result<WrappedKey, SyncError> {
    let nonce: [u8; 24] = random_array(env)?;
    let cipher = XChaCha20Poly1305::new_from_slice(kek).expect("32-byte key");
    let sealed = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: key,
                aad: &key_aad(vault_id, generation),
            },
        )
        .map_err(|_| SyncError::BadVault("cannot wrap a key".into()))?;
    Ok(WrappedKey {
        generation,
        nonce: hex::encode(&nonce),
        wrapped: hex::encode(&sealed),
        rest: Map::new(),
    })
}

fn read(store: &dyn SyncStore) -> Result<(VaultFile, String), SyncError> {
    let fetched = match store.get(layout::VAULT.into(), MAX_VAULT_BYTES) {
        Err(StoreError::NotFound) => return Err(SyncError::VaultMissing),
        result => result?,
    };
    let file: serde_json::Value = serde_json::from_slice(&fetched.bytes)
        .map_err(|error| SyncError::BadVault(error.to_string()))?;
    let format = file
        .get("format")
        .and_then(serde_json::Value::as_u64)
        .ok_or(SyncError::BadVault("no format".into()))?;
    if format > u64::from(VAULT_FORMAT) {
        return Err(SyncError::VaultTooNew(
            u32::try_from(format).unwrap_or(u32::MAX),
        ));
    }
    let file: VaultFile =
        serde_json::from_value(file).map_err(|error| SyncError::BadVault(error.to_string()))?;
    if file.keys.is_empty() {
        return Err(SyncError::BadVault("no keys".into()));
    }
    Ok((file, fetched.etag))
}

fn write(store: &dyn SyncStore, file: &VaultFile, cond: PutCondition) -> Result<(), SyncError> {
    let bytes = serde_json::to_vec_pretty(file).expect("a vault serializes");
    match store.put(layout::VAULT.into(), bytes, cond) {
        Ok(_) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn vault_status(store: &dyn SyncStore) -> Result<VaultStatus, SyncError> {
    match read(store) {
        Ok((file, _)) => Ok(VaultStatus::Present {
            params: file.params()?.0,
        }),
        Err(SyncError::VaultMissing) => Ok(VaultStatus::Absent),
        Err(error) => Err(error),
    }
}

/// Set sync up in `store`: a new vault with one data key. Fails with
/// `VaultExists` if one is there, even if it appeared just now.
pub fn create_vault(
    store: &dyn SyncStore,
    password: &str,
    params: KdfParams,
    env: &mut dyn Env,
) -> Result<Keyring, SyncError> {
    let vault_id: [u8; 16] = random_array(env)?;
    let salt: [u8; 16] = random_array(env)?;
    let kek = derive(password, &salt, params)?;
    let key = Zeroizing::new(random_array::<32>(env)?);
    let file = VaultFile {
        format: VAULT_FORMAT,
        vault_id: hex::encode(&vault_id),
        kdf: kdf_file(params, &salt),
        current: 1,
        keys: vec![wrap(&kek, &vault_id, 1, &key, env)?],
        rest: Map::new(),
    };
    match write(store, &file, PutCondition::Absent) {
        Err(SyncError::Store(StoreError::PreconditionFailed)) => {
            return Err(SyncError::VaultExists);
        }
        result => result?,
    }
    Ok(Keyring {
        vault_id,
        kek,
        keys: BTreeMap::from([(1, key)]),
        current: 1,
    })
}

fn kdf_file(params: KdfParams, salt: &[u8; 16]) -> KdfFile {
    KdfFile {
        algorithm: "argon2id".into(),
        version: 0x13,
        memory_kib: params.memory_kib,
        passes: params.passes,
        parallelism: params.parallelism,
        salt: hex::encode(salt),
        rest: Map::new(),
    }
}

/// Join the vault in `store` with its password.
pub fn unlock_vault(store: &dyn SyncStore, password: &str) -> Result<Keyring, SyncError> {
    let (file, _) = read(store)?;
    let (params, salt) = file.params()?;
    let vault_id = file.vault_id()?;
    let kek = derive(password, &salt, params)?;
    let keys = file
        .unwrap_all(&vault_id, &kek)?
        .ok_or(SyncError::WrongPassword)?;
    Ok(Keyring {
        vault_id,
        kek,
        keys,
        current: file.current,
    })
}

/// The vault as it is now, read with the keys `keyring` already holds: new
/// generations come in without the password. `NeedsPassword` when the
/// password was changed since; `VaultReplaced` when sync was reset.
pub fn refresh_keyring(store: &dyn SyncStore, keyring: &Keyring) -> Result<Keyring, SyncError> {
    let (file, _) = read(store)?;
    current_keys(&file, keyring)
}

fn current_keys(file: &VaultFile, keyring: &Keyring) -> Result<Keyring, SyncError> {
    let vault_id = file.vault_id()?;
    if vault_id != keyring.vault_id {
        return Err(SyncError::VaultReplaced);
    }
    let keys = file
        .unwrap_all(&vault_id, &keyring.kek)?
        .ok_or(SyncError::NeedsPassword)?;
    Ok(Keyring {
        vault_id,
        kek: keyring.kek.clone(),
        keys,
        current: file.current,
    })
}

/// Change the sync password. The data keys stay, so no snapshot changes;
/// `params` are the caller's, never ones read back from the file, which
/// someone could have weakened.
pub fn change_password(
    store: &dyn SyncStore,
    keyring: &Keyring,
    new_password: &str,
    params: KdfParams,
    env: &mut dyn Env,
) -> Result<Keyring, SyncError> {
    let (mut file, etag) = read(store)?;
    let current = current_keys(&file, keyring)?;
    let salt: [u8; 16] = random_array(env)?;
    let kek = derive(new_password, &salt, params)?;
    for entry in &mut file.keys {
        let key = current
            .key(entry.generation)
            .ok_or(SyncError::BadVault("a key does not open".into()))?;
        let rest = std::mem::take(&mut entry.rest);
        *entry = WrappedKey {
            rest,
            ..wrap(&kek, &current.vault_id, entry.generation, key, env)?
        };
    }
    file.kdf = kdf_file(params, &salt);
    write(store, &file, PutCondition::Matches(etag))?;
    Ok(Keyring { kek, ..current })
}

/// Start a new generation of data key. Should another device rotate at the
/// same time, its new key is taken instead of adding a second one.
pub fn rotate_key(
    store: &dyn SyncStore,
    keyring: &Keyring,
    env: &mut dyn Env,
) -> Result<Keyring, SyncError> {
    let mut started_at = None;
    for _ in 0..3 {
        let (mut file, etag) = read(store)?;
        let current = current_keys(&file, keyring)?;
        match started_at {
            Some(generation) if current.current > generation => return Ok(current),
            None => started_at = Some(current.current),
            Some(_) => {}
        }
        let generation = current.keys.keys().max().copied().unwrap_or(0) + 1;
        let key = Zeroizing::new(random_array::<32>(env)?);
        file.keys.push(wrap(
            &current.kek,
            &current.vault_id,
            generation,
            &key,
            env,
        )?);
        file.current = generation;
        match write(store, &file, PutCondition::Matches(etag)) {
            Ok(()) => {
                let mut keys = current.keys;
                keys.insert(generation, key);
                return Ok(Keyring {
                    keys,
                    current: generation,
                    ..current
                });
            }
            Err(SyncError::Store(StoreError::PreconditionFailed)) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(SyncError::Store(StoreError::PreconditionFailed))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use crate::{FixedEnv, hex};

    /// Argon2's least, so tests do not wait on it.
    pub(crate) const FAST: KdfParams = KdfParams {
        memory_kib: 8,
        passes: 1,
        parallelism: 1,
    };

    pub(crate) fn vault(store: &MemoryStore, env: &mut FixedEnv) -> Keyring {
        create_vault(store, "correct horse", FAST, env).unwrap()
    }

    #[test]
    fn a_vault_opens_with_its_password_only() {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        assert_eq!(vault_status(&store), Ok(VaultStatus::Absent));
        let made = vault(&store, &mut env);
        assert_eq!(
            vault_status(&store),
            Ok(VaultStatus::Present { params: FAST })
        );
        let opened = unlock_vault(&store, "correct horse").unwrap();
        assert_eq!(opened.to_bytes(), made.to_bytes());
        assert_eq!(
            unlock_vault(&store, "wrong horse").unwrap_err(),
            SyncError::WrongPassword
        );
        assert_eq!(
            create_vault(&store, "other", FAST, &mut env).unwrap_err(),
            SyncError::VaultExists
        );
        assert_eq!(
            unlock_vault(&MemoryStore::new(), "x").unwrap_err(),
            SyncError::VaultMissing
        );
    }

    #[test]
    fn a_password_typed_composed_or_decomposed_is_the_same() {
        let store = MemoryStore::new();
        create_vault(&store, "caf\u{e9}", FAST, &mut FixedEnv::new(0, 1)).unwrap();
        assert!(unlock_vault(&store, "cafe\u{301}").is_ok());
    }

    /// The derivation is the format: the same password, salt and cost give
    /// the same key in every version. The value is the reference C
    /// implementation's (argon2-cffi), not only this crate's.
    #[test]
    fn the_key_derivation_is_pinned() {
        let kek = derive("correct horse", &[7; 16], FAST).unwrap();
        assert_eq!(
            hex::encode(&*kek),
            "77c7165d162cfa442dd959d139711790f84c091401080df1b8cbdb1855ccf11e"
        );
    }

    /// And so is a setup code's key from it: HMAC-SHA256 of the words, as
    /// Python's `hmac` gives it.
    #[test]
    fn the_setup_code_key_is_pinned() {
        let key = code_key_from("correct horse", &[7; 16], FAST).unwrap();
        assert_eq!(
            hex::encode(&*key),
            "31da6ee0ff19b0caf535b9be89b74e309f56b8dace4cc6d568eb5161ad317abe"
        );
    }

    #[test]
    fn costs_out_of_bounds_are_refused_before_anything_runs() {
        let store = MemoryStore::new();
        vault(&store, &mut FixedEnv::new(0, 1));
        let mut file: serde_json::Value =
            serde_json::from_slice(&store.raw(layout::VAULT).unwrap()).unwrap();
        file["kdf"]["memory_kib"] = (4u64 << 30).into();
        store.set_raw(layout::VAULT, serde_json::to_vec(&file).unwrap());
        assert!(matches!(
            unlock_vault(&store, "correct horse"),
            Err(SyncError::BadVault(_))
        ));
        file["kdf"]["memory_kib"] = 8.into();
        file["kdf"]["passes"] = 1000.into();
        store.set_raw(layout::VAULT, serde_json::to_vec(&file).unwrap());
        assert!(matches!(
            unlock_vault(&store, "correct horse"),
            Err(SyncError::BadVault(_))
        ));
        file["format"] = 2.into();
        store.set_raw(layout::VAULT, serde_json::to_vec(&file).unwrap());
        assert_eq!(
            unlock_vault(&store, "correct horse").unwrap_err(),
            SyncError::VaultTooNew(2)
        );
    }

    #[test]
    fn a_new_password_keeps_the_keys() {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        let old = vault(&store, &mut env);
        let new = change_password(&store, &old, "battery staple", FAST, &mut env).unwrap();
        assert_eq!(new.key(1), old.key(1));
        assert_eq!(
            unlock_vault(&store, "correct horse").unwrap_err(),
            SyncError::WrongPassword
        );
        assert_eq!(
            unlock_vault(&store, "battery staple").unwrap().key(1),
            old.key(1)
        );
        // A device that still holds the old password's keys must ask.
        assert_eq!(
            refresh_keyring(&store, &old).unwrap_err(),
            SyncError::NeedsPassword
        );
    }

    #[test]
    fn a_rotation_adds_a_generation_and_keeps_the_old() {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        let first = vault(&store, &mut env);
        let second = rotate_key(&store, &first, &mut env).unwrap();
        assert_eq!(second.current_generation(), 2);
        assert_eq!(second.key(1), first.key(1));
        assert_ne!(second.key(2), first.key(1));
        // Another device learns the new key without the password.
        let refreshed = refresh_keyring(&store, &first).unwrap();
        assert_eq!(refreshed.key(2), second.key(2));
        assert_eq!(refreshed.current_generation(), 2);
    }

    /// What a newer version adds must survive this one rotating the key or
    /// changing the password. Only `kdf` goes whole with the password.
    #[test]
    fn fields_a_newer_version_added_survive_a_rewrite() {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        let keyring = vault(&store, &mut env);
        let written = || -> serde_json::Value {
            serde_json::from_slice(&store.raw(layout::VAULT).unwrap()).unwrap()
        };
        let mut file = written();
        file["later"] = serde_json::json!({"kept": true});
        file["keys"][0]["made_ms"] = 5.into();
        file["kdf"]["note"] = "about this salt".into();
        store.set_raw(layout::VAULT, serde_json::to_vec(&file).unwrap());

        let keyring = rotate_key(&store, &keyring, &mut env).unwrap();
        let rotated = written();
        assert_eq!(rotated["later"], file["later"]);
        assert_eq!(rotated["keys"][0]["made_ms"], 5);
        assert_eq!(rotated["kdf"]["note"], "about this salt");

        change_password(&store, &keyring, "battery staple", FAST, &mut env).unwrap();
        let changed = written();
        assert_eq!(changed["later"], file["later"]);
        assert_eq!(changed["keys"][0]["made_ms"], 5);
        assert_eq!(changed["keys"][1].get("made_ms"), None);
        assert_eq!(changed["kdf"].get("note"), None);
        let opened = unlock_vault(&store, "battery staple").unwrap();
        assert_eq!(opened.key(1), keyring.key(1));
        assert_eq!(opened.key(2), keyring.key(2));
    }

    /// Two devices rotating at once end with one new key: the one that
    /// loses the race takes the winner's.
    #[test]
    fn rotations_at_once_settle_on_one_key() {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        let keyring = vault(&store, &mut env);
        let before = store.raw(layout::VAULT).unwrap();
        let theirs = rotate_key(&store, &keyring, &mut env).unwrap();
        let rotated = store.raw(layout::VAULT).unwrap();
        // This device reads the vault as it was, and the other's rotation
        // lands just before this one's write.
        store.set_raw(layout::VAULT, before);
        store.race_next(
            |op| matches!(op, crate::store::Op::Put(_)),
            layout::VAULT,
            rotated,
        );
        let mine = rotate_key(&store, &keyring, &mut env).unwrap();
        assert_eq!(mine.current_generation(), 2);
        assert_eq!(mine.key(2), theirs.key(2));
    }

    #[test]
    fn a_reset_vault_is_told_apart() {
        let store = MemoryStore::new();
        let old = vault(&store, &mut FixedEnv::new(0, 1));
        store.remove_raw(layout::VAULT);
        vault(&store, &mut FixedEnv::new(0, 2));
        assert_eq!(
            refresh_keyring(&store, &old).unwrap_err(),
            SyncError::VaultReplaced
        );
    }

    #[test]
    fn a_keyring_survives_the_keychain_and_does_not_show_its_keys() {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        let keyring = rotate_key(&store, &vault(&store, &mut env), &mut env).unwrap();
        let back = Keyring::from_bytes(&keyring.to_bytes()).unwrap();
        assert_eq!(back.to_bytes(), keyring.to_bytes());
        assert!(Keyring::from_bytes(&keyring.to_bytes()[..40]).is_err());
        let shown = format!("{keyring:?}");
        assert!(!shown.contains(&hex::encode(keyring.key(1).unwrap())));
        assert!(shown.contains("generations: [1, 2]"));
    }

    #[test]
    fn the_keychain_keeps_the_newest_keys_and_the_vault_the_rest() {
        let store = MemoryStore::new();
        let mut env = FixedEnv::new(0, 1);
        let mut keyring = vault(&store, &mut env);
        for _ in 0..29 {
            keyring = rotate_key(&store, &keyring, &mut env).unwrap();
        }
        let bytes = keyring.to_bytes();
        // What the keychain gets, base64 in UTF-16 on Windows.
        assert!(bytes.len().div_ceil(3) * 4 * 2 <= 2560);
        let kept = Keyring::from_bytes(&bytes).unwrap();
        assert_eq!(kept.current_generation(), 30);
        assert_eq!(
            kept.keys.keys().copied().collect::<Vec<_>>(),
            (23..=30).collect::<Vec<_>>()
        );
        let refreshed = refresh_keyring(&store, &kept).unwrap();
        assert_eq!(refreshed.keys.len(), 30);
        assert_eq!(refreshed.key(1), keyring.key(1));
    }
}
