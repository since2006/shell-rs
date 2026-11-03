//! The blocking body of each sync job, to run off the UI thread: the
//! storage, the keychain and the sync core, which may all take a while.

use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use shellrs_sync::{
    Capabilities, DeviceId, DeviceInfo, KdfParams, Keyring, Pulled, Pushed, PutCondition,
    RoundOptions, StoreError, SyncError, SyncState, SyncStore, SystemEnv, VaultStatus,
    change_password, create_vault, layout, pull, push, rotate_key, unlock_vault, vault_status,
};
use zeroize::Zeroizing;

use super::{JobError, SyncLocation, SyncServices};
use crate::secrets::{SecretRef, SecretStore};
use crate::{SecretWork, SecretsWritten, SyncExport};

/// The storage at `location`. A WebDAV server's or a bucket's opens with
/// `login` (the password, the secret key), or the one the keychain keeps.
pub fn open(
    services: &SyncServices,
    location: &SyncLocation,
    login: Option<&str>,
    secrets: &dyn SecretStore,
) -> Result<Arc<dyn SyncStore>, JobError> {
    let kept;
    let password = match (location, login) {
        (SyncLocation::Folder { .. }, _) => None,
        (_, Some(login)) => Some(login),
        (_, None) => {
            kept = secrets
                .get(&SecretRef::SyncLogin)
                .map_err(JobError::keychain)?
                .ok_or(JobError::NoLogin)?;
            Some(kept.as_str())
        }
    };
    (services.open)(location, password).map_err(|error| JobError::Sync(error.into()))
}

/// Keep the login of a WebDAV server or a bucket in the keychain, or with
/// `None` forget the one kept.
pub fn keep_login(secrets: &dyn SecretStore, login: Option<&str>) -> Result<(), JobError> {
    match login {
        Some(login) => secrets.set(&SecretRef::SyncLogin, login),
        None => secrets.delete(&SecretRef::SyncLogin),
    }
    .map_err(JobError::keychain)
}

/// 测试连接: list, write, read back and delete `name` beside the sync's
/// folder, which is all sync asks of a store.
pub fn probe(store: &dyn SyncStore, name: &str) -> Result<(), JobError> {
    let path = format!("shellrs-sync/{name}");
    let wrote = b"ShellRS".to_vec();
    store.list("shellrs-sync".into()).map_err(SyncError::from)?;
    store
        .put(path.clone(), wrote.clone(), PutCondition::Overwrite)
        .map_err(SyncError::from)?;
    let read = store.get(path.clone(), 64);
    let deleted = store.delete(path);
    let read = read.map_err(SyncError::from)?;
    deleted.map_err(SyncError::from)?;
    if read.bytes != wrote {
        return Err(SyncError::Store(StoreError::Io(
            "what was read is not what was written".into(),
        ))
        .into());
    }
    Ok(())
}

/// The vault's keys as this computer keeps them: in the keychain only.
pub fn load_keyring(secrets: &dyn SecretStore) -> Result<Keyring, JobError> {
    let text = secrets
        .get(&SecretRef::SyncKeyring)
        .map_err(JobError::keychain)?
        .ok_or(JobError::NoKeyring)?;
    let bytes = Zeroizing::new(
        STANDARD
            .decode(text.as_bytes())
            .map_err(|_| JobError::NoKeyring)?,
    );
    Keyring::from_bytes(&bytes).map_err(|_| JobError::NoKeyring)
}

pub fn save_keyring(secrets: &dyn SecretStore, keyring: &Keyring) -> Result<(), JobError> {
    let text = Zeroizing::new(STANDARD.encode(&*keyring.to_bytes()));
    secrets
        .set(&SecretRef::SyncKeyring, &text)
        .map_err(JobError::keychain)
}

pub fn forget_keyring(secrets: &dyn SecretStore) -> Result<(), JobError> {
    secrets
        .delete(&SecretRef::SyncKeyring)
        .map_err(JobError::keychain)
}

/// Whether sync is set up in `store` already.
pub fn inspect(store: &dyn SyncStore) -> Result<bool, JobError> {
    Ok(matches!(vault_status(store)?, VaultStatus::Present { .. }))
}

/// Set sync up in `store` and keep its keys.
pub fn create(
    store: &dyn SyncStore,
    secrets: &dyn SecretStore,
    password: &str,
    kdf: KdfParams,
) -> Result<(), JobError> {
    let keyring = create_vault(store, password, kdf, &mut SystemEnv)?;
    save_keyring(secrets, &keyring)
}

/// Open the vault in `store` with its password and keep its keys.
pub fn join(
    store: &dyn SyncStore,
    secrets: &dyn SecretStore,
    password: &str,
) -> Result<(), JobError> {
    let keyring = unlock_vault(store, password)?;
    save_keyring(secrets, &keyring)
}

/// A new sync password, and a new data key after it: a device that kept the
/// old keys (a removed one) can't read anything newer.
pub fn new_password(
    store: &dyn SyncStore,
    secrets: &dyn SecretStore,
    password: &str,
    kdf: KdfParams,
) -> Result<(), JobError> {
    let keyring = load_keyring(secrets)?;
    let keyring = change_password(store, &keyring, password, kdf, &mut SystemEnv)?;
    // The password changed already: keep the keys that open with it before
    // anything else can fail.
    save_keyring(secrets, &keyring)?;
    let keyring = rotate_key(store, &keyring, &mut SystemEnv)?;
    save_keyring(secrets, &keyring)
}

/// Delete a device's file: it leaves the vault until it writes it again.
pub fn remove_device(store: &dyn SyncStore, device: &DeviceId) -> Result<(), JobError> {
    match store.delete(layout::device_file(device)) {
        Ok(()) | Err(shellrs_sync::StoreError::NotFound) => Ok(()),
        Err(error) => Err(shellrs_sync::SyncError::from(error).into()),
    }
}

/// The first half of a round: the records read, the others' files merged.
/// `state` is the stored one, `None` before the first round.
pub fn pull_round(
    store: &dyn SyncStore,
    secrets: &dyn SecretStore,
    export: &SyncExport,
    state: Option<&[u8]>,
    opts: RoundOptions,
) -> Result<(Pulled, Keyring), JobError> {
    let mut env = SystemEnv;
    let mut keyring = load_keyring(secrets)?;
    let state = match state {
        Some(bytes) => SyncState::from_bytes(bytes)?,
        None => SyncState::new(DeviceId::generate(&mut env)?),
    };
    let local = export.local(secrets, &state);
    let mut pulled = pull(
        store,
        &keyring,
        &state,
        &local,
        &Capabilities::ALL,
        &opts,
        &mut env,
    )?;
    if let Some(newer) = pulled.keyring.take() {
        // Kept for the rounds after; should the keychain refuse it, the
        // next round learns the keys again.
        let _ = save_keyring(secrets, &newer);
        keyring = newer;
    }
    if pulled.report.device_id_conflict {
        pulled.state = pulled.state.with_device(DeviceId::generate(&mut env)?);
    }
    Ok((pulled, keyring))
}

/// The second half: the round's secrets written where they go, then this
/// device's file uploaded from the state stored with the changes.
pub fn push_round(
    store: &dyn SyncStore,
    secrets: &dyn SecretStore,
    key_dir: Option<&Path>,
    work: SecretWork,
    keyring: &Keyring,
    device: &DeviceInfo,
    state: &SyncState,
) -> (SecretsWritten, Result<Pushed, JobError>) {
    let written = work.run(secrets, key_dir);
    let pushed = push(store, keyring, device, state, &mut SystemEnv).map_err(JobError::from);
    (written, pushed)
}

#[cfg(test)]
mod tests {
    use shellrs_sync::MemoryStore;

    use super::*;
    use crate::secrets::InMemorySecretStore;

    const FAST: KdfParams = KdfParams {
        memory_kib: 8,
        passes: 1,
        parallelism: 1,
    };

    #[test]
    fn the_keys_live_in_the_keychain_and_open_with_the_password_only() {
        let store = MemoryStore::new();
        let (one, two) = (
            InMemorySecretStore::default(),
            InMemorySecretStore::default(),
        );
        assert!(!inspect(&store).unwrap());
        create(&store, &one, "correct horse battery", FAST).unwrap();
        assert!(inspect(&store).unwrap());
        assert_eq!(
            join(&store, &two, "wrong horse battery"),
            Err(JobError::Sync(shellrs_sync::SyncError::WrongPassword))
        );
        assert_eq!(load_keyring(&two).unwrap_err(), JobError::NoKeyring);
        join(&store, &two, "correct horse battery").unwrap();
        assert_eq!(
            load_keyring(&one).unwrap().vault_id(),
            load_keyring(&two).unwrap().vault_id()
        );

        new_password(&store, &one, "staple battery horse", FAST).unwrap();
        assert_eq!(load_keyring(&one).unwrap().current_generation(), 2);
        assert!(join(&store, &two, "correct horse battery").is_err());
        join(&store, &two, "staple battery horse").unwrap();
        forget_keyring(&two).unwrap();
        assert_eq!(load_keyring(&two).unwrap_err(), JobError::NoKeyring);
    }
}
