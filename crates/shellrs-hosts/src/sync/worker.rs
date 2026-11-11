//! The blocking body of each sync job, to run off the UI thread: the
//! storage, the keychain and the sync core, which may all take a while.

use std::path::Path;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use shellrs_sync::{
    Capabilities, DeviceId, DeviceInfo, KdfParams, Keyring, Pulled, Pushed, PutCondition,
    RoundOptions, StoreError, SyncError, SyncState, SyncStore, SystemEnv, VaultStatus,
    change_password, create_vault, layout, pull, push, rotate_key, unlock_vault, vault_status,
};
use zeroize::Zeroizing;

use super::{JobError, S3Form, SyncLocation, SyncServices, WebDavForm};
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

/// What a setup code holds, as written. No `Debug`: it carries the login.
#[derive(Serialize)]
struct CodeOut<'a> {
    location: &'a SyncLocation,
    login: &'a str,
}

/// What a setup code holds, as read: the place still as JSON, so one of a
/// kind a newer ShellRS added is told from a damaged one.
#[derive(Deserialize)]
struct CodeIn {
    location: serde_json::Value,
    login: String,
}

/// The setup code of the sync in `store`, which `location` is: the place
/// and the login the keychain keeps, for another device to join with the
/// sync password alone. A folder has none, its path being this computer's.
pub fn setup_code(
    store: &dyn SyncStore,
    secrets: &dyn SecretStore,
    location: &SyncLocation,
) -> Result<String, JobError> {
    if !location.signs_in() {
        return Err(JobError::NoLogin);
    }
    let login = secrets
        .get(&SecretRef::SyncLogin)
        .map_err(JobError::keychain)?
        .ok_or(JobError::NoLogin)?;
    let keyring = load_keyring(secrets)?;
    let held = CodeOut {
        location,
        login: &login,
    };
    let plain = Zeroizing::new(serde_json::to_vec(&held).expect("a location serializes"));
    Ok(shellrs_sync::seal_setup_code(
        store,
        &keyring,
        &plain,
        &mut SystemEnv,
    )?)
}

/// The place and the login a setup code holds, opened with the sync
/// password: what [`join`] and [`keep_login`] take. They are checked as
/// the ones typed into the forms are, so a code brings nothing a form
/// would refuse.
pub fn open_setup_code(
    code: &str,
    password: &str,
) -> Result<(SyncLocation, Zeroizing<String>), JobError> {
    let plain = shellrs_sync::open_setup_code(code, password)?;
    let held: CodeIn = serde_json::from_slice(&plain).map_err(|_| SyncError::BadSetupCode)?;
    let login = Zeroizing::new(held.login);
    match held.location.get("kind").and_then(|kind| kind.as_str()) {
        Some("webdav" | "s3") => {}
        Some("folder") | None => return Err(SyncError::BadSetupCode.into()),
        Some(_) => return Err(SyncError::SetupCodeTooNew.into()),
    }
    let checked = match serde_json::from_value(held.location) {
        Ok(SyncLocation::WebDav { url, username }) => WebDavForm {
            url: &url,
            username: &username,
            password: &login,
        }
        .login(),
        Ok(SyncLocation::S3 {
            endpoint,
            region,
            bucket,
            prefix,
            path_style,
            access_key,
        }) => S3Form {
            endpoint: &endpoint,
            region: &region,
            bucket: &bucket,
            prefix: &prefix,
            path_style,
            access_key: &access_key,
            secret_key: &login,
        }
        .login(),
        Ok(SyncLocation::Folder { .. }) | Err(_) => return Err(SyncError::BadSetupCode.into()),
    };
    checked.map_err(|_| SyncError::BadSetupCode.into())
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

    const PASSWORD: &str = "correct horse battery";

    fn webdav() -> SyncLocation {
        SyncLocation::WebDav {
            url: "https://dav.example.com/shellrs".into(),
            username: "me".into(),
        }
    }

    fn bucket() -> SyncLocation {
        SyncLocation::S3 {
            endpoint: "https://s3.example.com".into(),
            region: "us-east-1".into(),
            bucket: "shellrs".into(),
            prefix: "sync".into(),
            path_style: true,
            access_key: "AKIAEXAMPLE".into(),
        }
    }

    /// A device in the sync of `store`, signed in with `login`.
    fn device_in_sync(store: &MemoryStore, login: &str) -> InMemorySecretStore {
        let secrets = InMemorySecretStore::default();
        create(store, &secrets, PASSWORD, FAST).unwrap();
        keep_login(&secrets, Some(login)).unwrap();
        secrets
    }

    /// A setup code of `store`'s sync holding `plain`, whatever it is.
    fn code_holding(store: &MemoryStore, secrets: &InMemorySecretStore, plain: &str) -> String {
        let keyring = load_keyring(secrets).unwrap();
        shellrs_sync::seal_setup_code(store, &keyring, plain.as_bytes(), &mut SystemEnv).unwrap()
    }

    #[test]
    fn a_setup_code_takes_another_device_into_the_sync() {
        for (location, login) in [(webdav(), " dav password "), (bucket(), "secret/key+")] {
            let store = MemoryStore::new();
            let one = device_in_sync(&store, login);
            let code = setup_code(&store, &one, &location).unwrap();
            assert!(shellrs_sync::is_setup_code(&code));
            assert!(!code.contains("password") && !code.contains("example"));

            assert_eq!(
                open_setup_code(&code, "wrong horse battery").map(|_| ()),
                Err(JobError::Sync(SyncError::SetupCodeWrongPassword))
            );
            let (place, opened) = open_setup_code(&code, PASSWORD).unwrap();
            assert_eq!(place, location);
            assert_eq!(opened.as_str(), login);

            // What it gave is all the other device needs.
            let two = InMemorySecretStore::default();
            join(&store, &two, PASSWORD).unwrap();
            keep_login(&two, Some(&opened)).unwrap();
            assert_eq!(
                load_keyring(&one).unwrap().vault_id(),
                load_keyring(&two).unwrap().vault_id()
            );
            assert_eq!(
                two.get(&SecretRef::SyncLogin).unwrap().as_deref(),
                Some(&login.to_owned())
            );
        }
    }

    /// What a code holds is the format (`FORMAT.md` of the sync core): the
    /// one kept there opens as this in every version.
    #[test]
    fn what_a_setup_code_holds_is_pinned() {
        let held = CodeOut {
            location: &bucket(),
            login: "secret",
        };
        assert_eq!(
            serde_json::to_string(&held).unwrap(),
            concat!(
                r#"{"location":{"kind":"s3","endpoint":"https://s3.example.com","#,
                r#""region":"us-east-1","bucket":"shellrs","prefix":"sync","path_style":true,"#,
                r#""access_key":"AKIAEXAMPLE"},"login":"secret"}"#
            )
        );
        let golden = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../shellrs-sync/testdata/v1/setup-code.txt"
        ))
        .unwrap();
        let (place, login) = open_setup_code(&golden, "correct horse battery staple").unwrap();
        assert_eq!(
            place,
            SyncLocation::WebDav {
                url: "http://127.0.0.1:1/shellrs".into(),
                username: "me".into(),
            }
        );
        assert_eq!(login.as_str(), "test login");
    }

    #[test]
    fn no_setup_code_without_a_login_or_the_keys() {
        let store = MemoryStore::new();
        let one = device_in_sync(&store, "dav password");
        let folder = SyncLocation::Folder {
            path: "/sync".into(),
        };
        assert_eq!(setup_code(&store, &one, &folder), Err(JobError::NoLogin));
        keep_login(&one, None).unwrap();
        assert_eq!(setup_code(&store, &one, &webdav()), Err(JobError::NoLogin));
        keep_login(&one, Some("dav password")).unwrap();
        forget_keyring(&one).unwrap();
        assert_eq!(
            setup_code(&store, &one, &webdav()),
            Err(JobError::NoKeyring)
        );
    }

    /// A code brings nothing a form would refuse, and says when it takes
    /// a newer ShellRS to read.
    #[test]
    fn a_setup_code_holding_what_no_form_takes_is_refused() {
        let store = MemoryStore::new();
        let one = device_in_sync(&store, "dav password");
        let open = |plain: &str| {
            open_setup_code(&code_holding(&store, &one, plain), PASSWORD).map(|(place, _)| place)
        };
        let bad = Err(JobError::Sync(SyncError::BadSetupCode));
        for plain in [
            "not json",
            "{}",
            r#"{"location":{"kind":"webdav","url":"https://dav.example.com","username":"me"}}"#,
            r#"{"location":{"kind":"folder","path":"/sync"},"login":"x"}"#,
            r#"{"location":{"url":"https://dav.example.com","username":"me"},"login":"x"}"#,
            r#"{"location":{"kind":"webdav","url":"ftp://dav.example.com","username":"me"},"login":"x"}"#,
            r#"{"location":{"kind":"webdav","url":"file:///etc","username":"me"},"login":"x"}"#,
            r#"{"location":{"kind":"webdav","url":"https://dav.example.com","username":""},"login":"x"}"#,
            r#"{"location":{"kind":"webdav","url":"https://dav.example.com","username":"me"},"login":""}"#,
            r#"{"location":{"kind":"s3","endpoint":"https://s3.example.com"},"login":"x"}"#,
            r#"{"location":"webdav","login":"x"}"#,
        ] {
            assert_eq!(open(plain), bad, "{plain}");
        }
        assert_eq!(
            open(r#"{"location":{"kind":"gdrive","folder":"abc"},"login":"x"}"#),
            Err(JobError::Sync(SyncError::SetupCodeTooNew))
        );
        // What a newer version added to a kind this one knows is passed
        // over.
        assert_eq!(
            open(concat!(
                r#"{"location":{"kind":"webdav","url":"https://dav.example.com/shellrs","#,
                r#""username":"me","depth":1},"login":"x","name":"NAS"}"#
            )),
            Ok(webdav())
        );
    }
}
