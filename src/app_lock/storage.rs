//! System keychain storage for the application lock state and password verifier.
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::password::Verifier;

const ACCOUNT: &str = "app-lock";

/// Blocking storage for the complete application lock record. Call only on a
/// worker: the system keychain can display its own authorization dialog.
pub trait LockRecordStore: Send + Sync + 'static {
    fn read(&self) -> Result<Option<Zeroizing<String>>>;
    fn write(&self, record: &str) -> Result<()>;
}

pub struct KeychainLockStore;

impl LockRecordStore for KeychainLockStore {
    fn read(&self) -> Result<Option<Zeroizing<String>>> {
        crate::secrets::KeychainSecretStore::read_account(ACCOUNT)
    }

    fn write(&self, record: &str) -> Result<()> {
        crate::secrets::KeychainSecretStore::write_account(ACCOUNT, record)
    }
}

/// An isolated keychain replacement for tests; never touches the OS store.
#[derive(Default)]
pub struct InMemoryLockStore {
    record: Mutex<Option<String>>,
}

impl LockRecordStore for InMemoryLockStore {
    fn read(&self) -> Result<Option<Zeroizing<String>>> {
        Ok(self.record.lock().unwrap().clone().map(Zeroizing::new))
    }

    fn write(&self, record: &str) -> Result<()> {
        *self.record.lock().unwrap() = Some(record.to_owned());
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    state: LockState,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum LockState {
    Disabled,
    Enabled { verifier: Verifier },
}

pub(super) fn save(store: &dyn LockRecordStore, verifier: Option<&Verifier>) -> Result<()> {
    let state = match verifier {
        Some(verifier) => LockState::Enabled {
            verifier: verifier.clone(),
        },
        None => LockState::Disabled,
    };
    let record = Zeroizing::new(serde_json::to_string(&Record { version: 1, state })?);
    store.write(&record)
}

/// A failed read or malformed record keeps the application locked.
pub(super) fn load(store: &dyn LockRecordStore) -> Result<Option<Verifier>> {
    let Some(record) = store
        .read()
        .context("reading application lock from keychain")?
    else {
        return Ok(None);
    };
    let record: Record = serde_json::from_str(&record)?;
    anyhow::ensure!(record.version == 1, "unsupported app lock record version");
    match record.state {
        LockState::Disabled => Ok(None),
        LockState::Enabled { verifier } => {
            verifier.validate()?;
            Ok(Some(verifier))
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::{InMemoryLockStore, LockRecordStore, load, save};
    use crate::app_lock::password::Verifier;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    pub(crate) struct FailingStore {
        pub(crate) memory: InMemoryLockStore,
        pub(crate) fail_read: AtomicBool,
        pub(crate) fail_write: AtomicBool,
    }

    impl LockRecordStore for FailingStore {
        fn read(&self) -> anyhow::Result<Option<zeroize::Zeroizing<String>>> {
            anyhow::ensure!(
                !self.fail_read.load(Ordering::SeqCst),
                "keychain read denied"
            );
            self.memory.read()
        }
        fn write(&self, record: &str) -> anyhow::Result<()> {
            anyhow::ensure!(
                !self.fail_write.load(Ordering::SeqCst),
                "keychain write denied"
            );
            self.memory.write(record)
        }
    }

    #[test]
    fn a_keychain_record_round_trips_without_plaintext() {
        let store = InMemoryLockStore::default();
        assert!(load(&store).unwrap().is_none());
        let verifier = Verifier::new("keychain password").unwrap();
        save(&store, Some(&verifier)).unwrap();
        assert!(!store.read().unwrap().unwrap().contains("keychain password"));
        assert!(
            load(&store)
                .unwrap()
                .unwrap()
                .verify("keychain password")
                .unwrap()
        );
    }

    #[test]
    fn unreadable_or_invalid_keychain_records_never_fall_back_to_unlocked() {
        let store = FailingStore::default();
        store.fail_read.store(true, Ordering::SeqCst);
        assert!(load(&store).is_err());
        store.fail_read.store(false, Ordering::SeqCst);
        for record in [
            "broken",
            "{}",
            r#"{"version":1}"#,
            r#"{"version":2,"state":"disabled"}"#,
        ] {
            store.memory.write(record).unwrap();
            assert!(load(&store).is_err());
        }
    }

    #[test]
    fn disabling_writes_an_explicit_keychain_state() {
        let store = InMemoryLockStore::default();
        let verifier = Verifier::new("previous password").unwrap();
        save(&store, Some(&verifier)).unwrap();
        save(&store, None).unwrap();
        assert!(store.read().unwrap().unwrap().contains("disabled"));
        assert!(load(&store).unwrap().is_none());
    }
}
