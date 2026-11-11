//! A round of sync, in two calls the app makes from a worker thread.
//!
//! 1. `pull` reads the other devices' files, merges them with the app's
//!    records and returns what the app is to change, with the state to
//!    store. It writes nothing anywhere.
//! 2. The app applies the changes and stores the state together, unless
//!    its records changed meanwhile, in which case it drops the result
//!    and pulls again.
//! 3. `push` uploads this device's file from that stored state; the app
//!    stores the state it returns.
//!
//! Uploading only what was stored keeps a crash from ever giving one
//! version two contents.
//!
//! The app does not keep versions. It hands over all its records, each with
//! the time the user last edited it, and the round finds what changed by
//! comparing them with what the app was last given:
//! - a record whose edit time changed since: the user's edit, stamped with
//!   a new version dated when it was made;
//! - one that differs though its edit time did not change: the app's copy
//!   is stale (or the app reshaped it), and it is given the record again;
//! - one gone though the app had it last round: deleted there.
//!
//! So the app changes a record's edit time when the user changes it, or
//! when it changes it on the user's behalf (hosts switched to their own
//! sign-in when their credential is deleted), and only then: records taken
//! from a pull keep the time they had, 0 when new. Comparing edit times
//! for equality rather than order keeps a wall clock set back from hiding
//! an edit.
//!
//! Secrets come and go the same way, with one more rule: what the app could
//! not read this round (its keychain locked, a key file gone) or could not
//! write when a round brought it, it says it could not, and that stays as
//! it was. Missing, it would count as deleted, and the deletion would reach
//! every device. A record it names is given again, so a key file gone comes
//! back and a secret not written is written; the secrets of a keychain that
//! did not answer are not, as it would not take them.
//!
//! Nor does the app have to read what it knows has not changed: a secret
//! whose edit time is the one the last round saw (`SyncState::edit_seen`)
//! it may name as unchanged instead, and the round takes the copy the state
//! keeps. So a keychain is read for what the user just changed only, not
//! for every password at every round.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::clock::Hlc;
use crate::envelope::{self, OpenError};
use crate::fixup;
use crate::merge::join;
use crate::record::{Body, Map, RawRecord, RecordSet};
use crate::secret::{self, Sealed, endpoint_secret_id};
use crate::snapshot::{self, DecodeError, DeviceInfo, Snapshot};
use crate::state::{FileMemo, OwnMemo, SyncState};
use crate::store::{PutCondition, RemoteFile, SyncStore, tag_base};
use crate::vault::{Keyring, refresh_keyring};
use crate::wire::{self, Capabilities, CredentialKind, RecordData, Route, SnippetScope};
use crate::{
    DeviceId, Env, FORMAT, Kind, RecordKey, StoreError, SyncError, Version, Warning, hex, layout,
};

/// A round refuses to delete at least this many records, and more than
/// half of what the app had, unless told to.
const MASS_DELETE: u32 = 10;
/// A device whose clock runs ahead by more than this is reported.
const CLOCK_AHEAD_MS: u64 = 60 * 60 * 1000;

/// A record as the app holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalRecord {
    pub id: String,
    pub data: RecordData,
    /// When the user last changed it, in this device's milliseconds since
    /// the epoch; kept as it was when a pull changes the record, 0 for a
    /// record a pull created.
    pub edited_ms: u64,
}

/// Every record the app holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalSnapshot {
    pub records: Vec<LocalRecord>,
    /// Whether `records` has the app's secrets. When it could not read them
    /// (the keychain locked), they stay as they were: none counts as
    /// deleted, none is given.
    pub secrets_read: bool,
    /// Records the app does not hold as it should, such as a key file gone
    /// from its folder or a secret it could not write: they stay as they
    /// were, and are given again (or their deletion is).
    pub unreadable: Vec<RecordKey>,
    /// Records the app did not read, as they have not changed since the
    /// last round saw them: the copy in the state stands in for each. One
    /// it cannot (another edit time, its secrets lost) is as if unreadable.
    pub unchanged: Vec<Unchanged>,
}

/// A record the app vouches for without reading it: still as it was when
/// the last round saw it with this edit time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unchanged {
    pub key: RecordKey,
    pub edited_ms: u64,
}

/// What a device that has never synced does with its records.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JoinMode {
    /// They join the others'.
    #[default]
    Merge,
    /// They give way: the device ends up with the others' records only.
    UseRemote,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoundOptions {
    /// Delete however many records the app deleted (after the user
    /// confirmed a `MassDelete`).
    pub allow_mass_delete: bool,
    /// Only for a device's first round.
    pub join: JoinMode,
}

/// A change for the app, in an order it can apply them one by one: every
/// insert or update first, a record after those it points at; then every
/// deletion, a record before those it points at.
// A round gives a handful of these at a time; boxing the data would only
// make the app's side wordier.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Upsert { id: String, data: RecordData },
    Delete { kind: Kind, id: String },
}

impl Change {
    pub fn key(&self) -> RecordKey {
        match self {
            Self::Upsert { id, data } => RecordKey::new(data.kind(), id.clone()),
            Self::Delete { kind, id } => RecordKey::new(kind.clone(), id.clone()),
        }
    }
}

/// Another device of the vault, as its file describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteDevice {
    pub id: DeviceId,
    pub info: DeviceInfo,
    /// The device's clock when it last wrote its file.
    pub last_write_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoundReport {
    pub devices: Vec<RemoteDevice>,
    /// Files read this round.
    pub downloaded: u32,
    pub local_edits: u32,
    pub local_deletes: u32,
    /// Records repaired after the merge (see `fixup`).
    pub repairs: u32,
    /// Another device writes this one's file: give this one a new id
    /// (`SyncState::with_device`) before pushing.
    pub device_id_conflict: bool,
    /// This device's file is gone: it was removed from the vault elsewhere.
    /// Pushing writes it again.
    pub removed: bool,
    /// Some file needs this newer format; pushing is refused until updated.
    pub read_only: Option<u32>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug)]
pub struct Pulled {
    pub changes: Vec<Change>,
    pub state: SyncState,
    /// A keyring with keys learned this round, to keep in place of the old.
    pub keyring: Option<Keyring>,
    pub report: RoundReport,
}

// One per round, moved straight to the caller.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Pushed {
    /// The file already holds this state.
    Unchanged,
    Uploaded {
        state: SyncState,
    },
    /// The file changed since this device last read or wrote it: pull again.
    Conflict,
}

/// Read the others' files and merge them with `local`; see the module.
pub fn pull(
    store: &dyn SyncStore,
    keys: &Keyring,
    state: &SyncState,
    local: &LocalSnapshot,
    caps: &Capabilities,
    opts: &RoundOptions,
    env: &mut dyn Env,
) -> Result<Pulled, SyncError> {
    let now = env.now_ms();
    let me = state.device.clone();
    let mut clock = Hlc::new(state.clock);
    let mut report = RoundReport::default();

    // Everything this device knew, its secrets unsealed. Secrets that do
    // not open come back from the other devices and the app.
    let mut known = state.records.clone();
    let mut app_had_before = state.app_had.clone();
    let mut edited_seen = state.edited_seen.clone();
    let mut known_secrets = None;
    let mut secrets_lost = false;
    let mut reader = Reader {
        store,
        keys,
        refreshed: None,
        checked_vault: false,
    };
    if let Some(sealed) = &state.secrets {
        match reader.open_secrets(sealed)? {
            Some(secrets) => {
                known.extend(secrets.clone());
                known_secrets = Some(secrets);
            }
            None => {
                secrets_lost = true;
                report.warnings.push(Warning::SecretsUnreadable);
                app_had_before.retain(|key| !key.kind.is_secret());
                edited_seen.retain(|key, _| !key.kind.is_secret());
            }
        }
    }
    let mut set = known.clone();

    // What the app could not read stays as it was: its secrets when it did
    // not read them, and whatever it lists as unreadable.
    let withheld = |key: &RecordKey| key.kind.is_secret() && !local.secrets_read;
    // What the app vouches for unread comes from the state, as it last was.
    let mut restored = Vec::new();
    let mut lost = BTreeSet::new();
    for unchanged in &local.unchanged {
        let kept = (edited_seen.get(&unchanged.key) == Some(&unchanged.edited_ms))
            .then(|| known.get(&unchanged.key))
            .flatten()
            .and_then(RawRecord::live)
            .and_then(|fields| wire::parse(&unchanged.key, fields).ok());
        match kept {
            Some(data) => restored.push(LocalRecord {
                id: unchanged.key.id.clone(),
                data,
                edited_ms: unchanged.edited_ms,
            }),
            None => {
                lost.insert(unchanged.key.clone());
            }
        }
    }
    let untouched =
        |key: &RecordKey| withheld(key) || local.unreadable.contains(key) || lost.contains(key);
    let unreadable: BTreeSet<RecordKey> = local.unreadable.iter().chain(&lost).cloned().collect();
    let local = index(
        local.records.iter().chain(&restored),
        &untouched,
        &mut report.warnings,
    );

    // The app's own changes come first, against what it was last given and
    // with the clock as it was then: an edit is dated when it was made.
    if !(state.is_fresh() && opts.join == JoinMode::UseRemote) {
        let (seen, _) = deliverable(&known, caps);
        for (key, record) in &local {
            if edited_seen.get(key) == Some(&record.edited_ms) {
                continue;
            }
            let base = known.get(key);
            let fields = match base.and_then(RawRecord::live) {
                None => wire::write_into(&record.data, None),
                Some(fields) => {
                    // A record this version cannot read: the app's copy is
                    // older than it, so its edit cannot be merged.
                    let Ok(was) = wire::parse(key, fields) else {
                        continue;
                    };
                    let data = restore_hidden(&record.data, &was, |kind, id| {
                        seen.contains_key(&RecordKey::new(kind, id))
                    });
                    if data == was {
                        edited_seen.insert(key.clone(), record.edited_ms);
                        continue;
                    }
                    wire::write_into(&data, Some(fields))
                }
            };
            let stamp = clock.tick(record.edited_ms.min(now));
            let live = base.filter(|base| !base.is_deleted());
            join(
                &mut set,
                RawRecord {
                    key: key.clone(),
                    version: Version {
                        stamp,
                        device: me.clone(),
                    },
                    body: Body::Live(fields),
                    min_reader: live.and_then(|base| base.min_reader),
                    rest: live.map(|base| base.rest.clone()).unwrap_or_default(),
                },
            );
            edited_seen.insert(key.clone(), record.edited_ms);
            report.local_edits += 1;
        }
        for key in &app_had_before {
            if untouched(key)
                || local.contains_key(key)
                || set.get(key).is_none_or(RawRecord::is_deleted)
            {
                continue;
            }
            let stamp = clock.tick(now);
            join(
                &mut set,
                RawRecord {
                    key: key.clone(),
                    version: Version {
                        stamp,
                        device: me.clone(),
                    },
                    body: Body::Deleted,
                    min_reader: None,
                    rest: Map::new(),
                },
            );
            edited_seen.remove(key);
            report.local_deletes += 1;
        }
        let had = app_had_before.len() as u32;
        if !opts.allow_mass_delete
            && report.local_deletes >= MASS_DELETE
            && report.local_deletes * 2 > had
        {
            return Err(SyncError::MassDelete {
                count: report.local_deletes,
                of: had,
            });
        }
    }

    // Then the other devices' files, those changed since last read.
    let listed = match store.list(layout::DEVICES.into()) {
        Err(StoreError::NotFound) => Vec::new(),
        result => result?,
    };
    let mut gathered = Gathered::default();
    let mut own = state.own.clone();
    let mut own_listed: Option<RemoteFile> = None;
    for file in listed {
        let Some(id) = layout::device_of(&file.name) else {
            continue;
        };
        if id == me {
            own_listed = Some(file);
            continue;
        }
        // Without its sealed secrets the state lost what those files gave.
        if !secrets_lost
            && let Some(memo) = state.files.get(&id).filter(|memo| memo.etag == file.etag)
        {
            gathered.files.insert(id, memo.clone());
            continue;
        }
        reader.fetch(id, now, &mut gathered, &mut report)?;
    }

    // This device's own file: changed only if another device writes it,
    // or if this one crashed between uploading and storing the state.
    let mut conflict = false;
    match (own_listed, &own) {
        (None, Some(_)) => {
            own = None;
            report.removed = true;
            // Removed, or sync was reset: a reset must not be written into.
            reader.refreshed = Some(refresh_keyring(store, reader.keys())?);
        }
        (None, None) => {}
        (Some(file), Some(memo)) if tag_base(&file.etag) == tag_base(&memo.etag) => {}
        (Some(_), _) => match reader.read(&me)? {
            // What this device uploaded before a crash kept it from storing
            // the state: the stored records are exactly the file's. Which
            // key it used is unknown, so the next push writes it again.
            Outcome::Read(etag, snapshot) if snapshot.records == known => {
                own = Some(OwnMemo {
                    etag,
                    digest: records_digest(&snapshot.info, &snapshot.records),
                    generation: 0,
                });
            }
            Outcome::Read(_, snapshot) => {
                conflict = true;
                gathered.snapshots.push(snapshot);
            }
            Outcome::Gone => own = None,
            Outcome::TooNew(_) | Outcome::Unreadable(_) => conflict = true,
        },
    }
    gathered.merge_into(&mut set, &mut clock);

    report.repairs = fixup::repair(&mut set, &mut clock, &me);

    // What the app is to change: whatever it should hold and does not,
    // what it could not read among it.
    let (visible, parked) = deliverable(&set, caps);
    report.warnings.extend(parked);
    let mut changes = Vec::new();
    for (key, view) in &visible {
        if !withheld(key) && local.get(key).is_none_or(|record| record.data != *view) {
            changes.push(Change::Upsert {
                id: key.id.clone(),
                data: view.clone(),
            });
        }
    }
    let gone = |key: &RecordKey| !visible.contains_key(key);
    for key in local.keys() {
        if gone(key) && set.get(key).is_none_or(RawRecord::is_deleted) {
            changes.push(Change::Delete {
                kind: key.kind.clone(),
                id: key.id.clone(),
            });
        }
    }
    // One it could not read that was never synced is not the round's to
    // delete.
    for key in &unreadable {
        if !withheld(key) && gone(key) && set.get(key).is_some_and(RawRecord::is_deleted) {
            changes.push(Change::Delete {
                kind: key.kind.clone(),
                id: key.id.clone(),
            });
        }
    }
    sort_changes(&mut changes, &local);

    let mut app_had: BTreeSet<RecordKey> = local.keys().cloned().collect();
    app_had.extend(app_had_before.iter().filter(|key| untouched(key)).cloned());
    for change in &changes {
        let key = change.key();
        match change {
            Change::Upsert { .. } => {
                let edited_ms = local.get(&key).map_or(0, |record| record.edited_ms);
                edited_seen.insert(key.clone(), edited_ms);
                app_had.insert(key);
            }
            Change::Delete { .. } => {
                edited_seen.remove(&key);
                app_had.remove(&key);
            }
        }
    }
    report.devices = gathered
        .files
        .iter()
        .map(|(id, memo)| RemoteDevice {
            id: id.clone(),
            info: memo.info.clone(),
            last_write_ms: memo.clock_ms,
        })
        .collect();
    report.device_id_conflict = conflict;
    report.read_only = gathered.read_only;

    // Secrets go into the state sealed; unchanged, as they were sealed.
    let (secrets, records): (RecordSet, RecordSet) =
        set.into_iter().partition(|(key, _)| key.kind.is_secret());
    let secrets = if secrets.is_empty() {
        None
    } else if known_secrets.as_ref() == Some(&secrets) {
        state.secrets.clone()
    } else {
        Some(secret::seal(&secrets, reader.keys(), env)?)
    };
    Ok(Pulled {
        changes,
        state: SyncState {
            device: me,
            clock: clock.last(),
            records,
            secrets,
            app_had,
            edited_seen,
            files: gathered.files,
            own,
            read_only: gathered.read_only,
            joined: true,
        },
        keyring: reader.refreshed,
        report,
    })
}

/// Upload this device's file from `state`, the state the app stored after
/// its last pull.
pub fn push(
    store: &dyn SyncStore,
    keys: &Keyring,
    device: &DeviceInfo,
    state: &SyncState,
    env: &mut dyn Env,
) -> Result<Pushed, SyncError> {
    if let Some(min_reader) = state.read_only {
        return Err(SyncError::ReadOnly { min_reader });
    }
    let mut records = state.records.clone();
    if let Some(sealed) = &state.secrets {
        let secrets = secret::open(sealed, keys)
            .ok_or_else(|| SyncError::StateUnreadable("its secrets do not open".into()))?;
        records.extend(secrets);
    }
    let digest = records_digest(device, &records);
    let generation = keys.current_generation();
    if state
        .own
        .as_ref()
        .is_some_and(|own| own.digest == digest && own.generation == generation)
    {
        return Ok(Pushed::Unchanged);
    }
    let plain = Zeroizing::new(snapshot::encode(
        &state.device,
        device,
        state.clock,
        &records,
    ));
    let sealed = envelope::seal(&plain, &state.device, keys, env)?;
    let cond = state.own.as_ref().map_or(PutCondition::Absent, |own| {
        PutCondition::Matches(own.etag.clone())
    });
    match store.put(layout::device_file(&state.device), sealed, cond) {
        Ok(etag) => Ok(Pushed::Uploaded {
            state: SyncState {
                own: Some(OwnMemo {
                    etag,
                    digest,
                    generation,
                }),
                ..state.clone()
            },
        }),
        Err(StoreError::PreconditionFailed) => Ok(Pushed::Conflict),
        Err(error) => Err(error.into()),
    }
}

/// What a device file holds: the device's description and its records.
/// It is kept in the app's database, so a secret counts by its version
/// alone: a hash of its content would let a weak password be guessed
/// offline. One version has one content, so nothing is missed.
pub(crate) fn records_digest(info: &DeviceInfo, records: &RecordSet) -> String {
    let mut hash = Sha256::new();
    for text in [&info.name, &info.platform, &info.app_version] {
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text.as_bytes());
    }
    for record in records.values() {
        if record.key.kind.is_secret() {
            let mut mark = Sha256::new();
            for text in [record.key.kind.as_str(), &record.key.id] {
                mark.update(text.as_bytes());
                mark.update([0]);
            }
            mark.update(record.version.stamp.ms.to_be_bytes());
            mark.update(record.version.stamp.counter.to_be_bytes());
            mark.update(record.version.device.as_str().as_bytes());
            mark.update([u8::from(record.is_deleted())]);
            hash.update(mark.finalize());
        } else {
            hash.update(record.digest());
        }
    }
    hex::encode(&hash.finalize())
}

/// The app's records by key, those `untouched` this round left out; one
/// that could not be stored as it is is parked.
fn index<'a>(
    records: impl Iterator<Item = &'a LocalRecord>,
    untouched: &dyn Fn(&RecordKey) -> bool,
    warnings: &mut Vec<Warning>,
) -> BTreeMap<RecordKey, &'a LocalRecord> {
    let mut out = BTreeMap::new();
    for record in records {
        let key = RecordKey::new(record.data.kind(), record.id.clone());
        if untouched(&key) {
            continue;
        }
        let reason = match &record.data {
            _ if !RecordKey::is_valid_id(&record.id) => Some("not a valid id"),
            RecordData::Password(endpoint) | RecordData::ProxyPassword(endpoint)
                if record.id
                    != endpoint_secret_id(&endpoint.user, &endpoint.host, endpoint.port) =>
            {
                Some("the id is not its endpoint's")
            }
            _ => None,
        };
        match reason {
            None => {
                out.insert(key, record);
            }
            Some(reason) => warnings.push(Warning::Parked {
                kind: key.kind.to_string(),
                id: key.id,
                reason: reason.into(),
            }),
        }
    }
    out
}

enum Outcome {
    Read(String, Snapshot),
    Gone,
    TooNew(u32),
    Unreadable(Warning),
}

/// What reading the other devices' files gathers.
#[derive(Default)]
struct Gathered {
    files: BTreeMap<DeviceId, FileMemo>,
    snapshots: Vec<Snapshot>,
    read_only: Option<u32>,
}

impl Gathered {
    /// Merge the snapshots read so far into `set`.
    fn merge_into(&mut self, set: &mut RecordSet, clock: &mut Hlc) {
        for snapshot in self.snapshots.drain(..) {
            clock.observe(snapshot.clock);
            for record in snapshot.records.into_values() {
                clock.observe(record.version.stamp);
                join(set, record);
            }
        }
    }
}

/// Reads device files, learning new keys from the vault once a round if a
/// file needs them.
struct Reader<'a> {
    store: &'a dyn SyncStore,
    keys: &'a Keyring,
    refreshed: Option<Keyring>,
    checked_vault: bool,
}

impl Reader<'_> {
    fn keys(&self) -> &Keyring {
        self.refreshed.as_ref().unwrap_or(self.keys)
    }

    /// The state's secrets, with a key the keychain may have let go (it
    /// keeps the newest generations only) taken from the vault again.
    fn open_secrets(&mut self, sealed: &Sealed) -> Result<Option<RecordSet>, SyncError> {
        if self.keys().key(sealed.generation).is_none() && !self.checked_vault {
            self.checked_vault = true;
            self.refreshed = Some(refresh_keyring(self.store, self.keys)?);
        }
        Ok(secret::open(sealed, self.keys()))
    }

    /// Read another device's file into `into`. One not read is not noted,
    /// so the next round tries again.
    fn fetch(
        &mut self,
        id: DeviceId,
        now: u64,
        into: &mut Gathered,
        report: &mut RoundReport,
    ) -> Result<(), SyncError> {
        match self.read(&id)? {
            Outcome::Read(etag, snapshot) => {
                report.downloaded += 1;
                if snapshot.clock.ms > now.saturating_add(CLOCK_AHEAD_MS) {
                    report.warnings.push(Warning::ClockAhead {
                        device: id.to_string(),
                        ahead_ms: snapshot.clock.ms - now,
                    });
                }
                into.files.insert(
                    id,
                    FileMemo {
                        etag,
                        info: snapshot.info.clone(),
                        clock_ms: snapshot.clock.ms,
                    },
                );
                into.snapshots.push(snapshot);
            }
            Outcome::Gone => {}
            Outcome::TooNew(min_reader) => {
                into.read_only = into.read_only.max(Some(min_reader));
                report.warnings.push(Warning::SnapshotTooNew {
                    device: id.to_string(),
                    min_reader,
                });
            }
            Outcome::Unreadable(warning) => report.warnings.push(warning),
        }
        Ok(())
    }

    fn read(&mut self, device: &DeviceId) -> Result<Outcome, SyncError> {
        let fetched = match self
            .store
            .get(layout::device_file(device), snapshot::MAX_FILE)
        {
            Ok(fetched) => fetched,
            Err(StoreError::NotFound) => return Ok(Outcome::Gone),
            Err(StoreError::TooLarge) => {
                return Ok(Outcome::Unreadable(Warning::CorruptSnapshot {
                    device: device.to_string(),
                    reason: "too large".into(),
                }));
            }
            Err(error) => return Err(error.into()),
        };
        let mut opened = envelope::open(&fetched.bytes, device, self.keys());
        if matches!(
            opened,
            Err(OpenError::OtherVault | OpenError::UnknownGeneration(_))
        ) && !self.checked_vault
        {
            // The vault may have a key this device has not seen, or may
            // have been reset; either way it says.
            self.checked_vault = true;
            self.refreshed = Some(refresh_keyring(self.store, self.keys)?);
            opened = envelope::open(&fetched.bytes, device, self.keys());
        }
        let plain = match opened {
            Ok(plain) => Zeroizing::new(plain),
            Err(OpenError::TooNew(_)) => return Ok(Outcome::TooNew(FORMAT + 1)),
            Err(OpenError::OtherVault) => {
                return Ok(Outcome::Unreadable(Warning::OtherVault {
                    device: device.to_string(),
                }));
            }
            Err(OpenError::UnknownGeneration(generation)) => {
                return Ok(Outcome::Unreadable(Warning::UnknownKeyGeneration {
                    device: device.to_string(),
                    generation,
                }));
            }
            Err(OpenError::NotOurs | OpenError::Damaged) => {
                return Ok(Outcome::Unreadable(Warning::CorruptSnapshot {
                    device: device.to_string(),
                    reason: "does not decrypt".into(),
                }));
            }
        };
        Ok(match snapshot::decode(&plain, device) {
            Ok(snapshot) => Outcome::Read(fetched.etag, snapshot),
            Err(DecodeError::TooNew(min_reader)) => Outcome::TooNew(min_reader),
            Err(DecodeError::Corrupt(reason)) => Outcome::Unreadable(Warning::CorruptSnapshot {
                device: device.to_string(),
                reason,
            }),
        })
    }
}

/// The records the app should hold, as it should hold them, and the
/// records it cannot have (`Parked` warnings).
///
/// A record that needs another (a forward its host, a host signing in with
/// a credential that credential, a credential's secret that credential) is
/// held back while that one is missing. A reference the app can do without
/// (a host's group, a hop) shows as `None` meanwhile, and comes back once
/// the record does.
fn deliverable(
    set: &RecordSet,
    caps: &Capabilities,
) -> (BTreeMap<RecordKey, RecordData>, Vec<Warning>) {
    let mut parsed = BTreeMap::new();
    let mut warnings = Vec::new();
    for record in set.values() {
        let Some(fields) = record.live() else {
            continue;
        };
        // A secret of a kind this version does not know is only passed on.
        if matches!(record.key.kind, Kind::Secret(_)) {
            continue;
        }
        let result = match record.min_reader {
            Some(min_reader) if min_reader > FORMAT => Err(format!("needs format {min_reader}")),
            _ => wire::parse(&record.key, fields).and_then(|data| data.fits(caps).map(|()| data)),
        };
        match result {
            Ok(data) => {
                parsed.insert(record.key.clone(), data);
            }
            Err(reason) => warnings.push(Warning::Parked {
                kind: record.key.kind.to_string(),
                id: record.key.id.clone(),
                reason,
            }),
        }
    }
    let has = |kind: Kind, id: &str| parsed.contains_key(&RecordKey::new(kind, id));
    let hosts: BTreeSet<&str> = parsed
        .iter()
        .filter_map(|(key, data)| match data {
            RecordData::Host(host) => host
                .credential
                .as_deref()
                .is_none_or(|credential| has(Kind::Credential, credential))
                .then_some(key.id.as_str()),
            _ => None,
        })
        .collect();
    // The hosts that fill in one-time codes, of those that stay.
    let totp_hosts: BTreeSet<&str> = parsed
        .iter()
        .filter_map(|(key, data)| match data {
            RecordData::Host(host) if host.totp.is_some() => Some(key.id.as_str()),
            _ => None,
        })
        .filter(|id| hosts.contains(id))
        .collect();
    let credential_kind = |id: &str| match parsed.get(&RecordKey::new(Kind::Credential, id)) {
        Some(RecordData::Credential(credential)) => Some(credential.kind),
        _ => None,
    };
    let mut out = BTreeMap::new();
    for (key, data) in &parsed {
        let mut data = data.clone();
        let keep = match &mut data {
            RecordData::Group(group) => {
                if group
                    .parent
                    .as_deref()
                    .is_some_and(|p| !has(Kind::Group, p))
                {
                    group.parent = None;
                }
                true
            }
            RecordData::Host(host) => {
                if host.group.as_deref().is_some_and(|g| !has(Kind::Group, g)) {
                    host.group = None;
                }
                if let Route::Jump(hops) = &mut host.route {
                    for hop in hops.iter_mut() {
                        if hop.as_deref().is_some_and(|id| !hosts.contains(id)) {
                            *hop = None;
                        }
                    }
                }
                hosts.contains(key.id.as_str())
            }
            RecordData::Bookmark(bookmark) => hosts.contains(bookmark.host.as_str()),
            RecordData::Forward(forward) => hosts.contains(forward.host.as_str()),
            RecordData::Snippet(snippet) => {
                if snippet
                    .category
                    .as_deref()
                    .is_some_and(|c| !has(Kind::SnippetCategory, c))
                {
                    snippet.category = None;
                }
                match &snippet.scope {
                    SnippetScope::All => true,
                    SnippetScope::Group(group) => has(Kind::Group, group),
                    SnippetScope::Host(host) => hosts.contains(host.as_str()),
                }
            }
            // A credential's secret goes to it only while it is the kind
            // that has one.
            RecordData::CredentialPassword(_) => {
                credential_kind(&key.id) == Some(CredentialKind::Password)
            }
            RecordData::Passphrase(_) | RecordData::PrivateKey(_) => {
                credential_kind(&key.id) == Some(CredentialKind::Key)
            }
            // A host's, only while it fills in codes.
            RecordData::Totp(_) => totp_hosts.contains(key.id.as_str()),
            RecordData::Credential(_)
            | RecordData::SnippetCategory(_)
            | RecordData::Password(_)
            | RecordData::ProxyPassword(_) => true,
        };
        if keep {
            out.insert(key.clone(), data);
        }
    }
    (out, warnings)
}

/// The app's copy of a record, with the references it was shown as `None`
/// because their record was missing put back: the app could not have meant
/// to clear what it never saw.
fn restore_hidden(
    app: &RecordData,
    was: &RecordData,
    seen: impl Fn(Kind, &str) -> bool,
) -> RecordData {
    let hidden = |now: &Option<String>, before: &Option<String>, kind: Kind| {
        now.is_none() && before.as_deref().is_some_and(|id| !seen(kind, id))
    };
    let mut data = app.clone();
    match (&mut data, was) {
        (RecordData::Group(now), RecordData::Group(before)) => {
            if hidden(&now.parent, &before.parent, Kind::Group) {
                now.parent.clone_from(&before.parent);
            }
        }
        (RecordData::Host(now), RecordData::Host(before)) => {
            if hidden(&now.group, &before.group, Kind::Group) {
                now.group.clone_from(&before.group);
            }
            if let (Route::Jump(hops), Route::Jump(were)) = (&mut now.route, &before.route)
                && hops.len() == were.len()
            {
                for (hop, was) in hops.iter_mut().zip(were) {
                    if hidden(hop, was, Kind::Host) {
                        hop.clone_from(was);
                    }
                }
            }
        }
        (RecordData::SnippetCategory(now), RecordData::SnippetCategory(before))
            if hidden(&now.parent, &before.parent, Kind::SnippetCategory) =>
        {
            now.parent.clone_from(&before.parent);
        }
        (RecordData::Snippet(now), RecordData::Snippet(before))
            if hidden(&now.category, &before.category, Kind::SnippetCategory) =>
        {
            now.category.clone_from(&before.category);
        }
        _ => {}
    }
    data
}

/// Inserts and updates first, by kind (what is pointed at first) and a
/// parent group or category before its children; then deletions the other
/// way round.
fn sort_changes(changes: &mut [Change], local: &BTreeMap<RecordKey, &LocalRecord>) {
    // A parent is of its child's kind.
    let parent_of = |key: &RecordKey, data: &RecordData| {
        let up = match data {
            RecordData::Group(group) => &group.parent,
            RecordData::SnippetCategory(category) => &category.parent,
            _ => return None,
        };
        Some(up.clone().map(|up| RecordKey::new(key.kind.clone(), up)))
    };
    let mut parents: BTreeMap<RecordKey, Option<RecordKey>> = BTreeMap::new();
    for change in changes.iter() {
        if let Change::Upsert { data, .. } = change
            && let Some(up) = parent_of(&change.key(), data)
        {
            parents.insert(change.key(), up);
        }
    }
    for (key, record) in local {
        if let Some(up) = parent_of(key, &record.data) {
            parents.entry(key.clone()).or_insert(up);
        }
    }
    let depth = |key: &RecordKey| {
        let mut depth = 0;
        let mut at = key;
        while let Some(Some(up)) = parents.get(at) {
            depth += 1;
            at = up;
            if depth > parents.len() {
                break;
            }
        }
        depth
    };
    changes.sort_by_cached_key(|change| {
        let key = change.key();
        let depth = depth(&key);
        match change {
            Change::Upsert { .. } => (0, key.kind.rank(), depth, key.id),
            Change::Delete { .. } => (1, usize::MAX - key.kind.rank(), usize::MAX - depth, key.id),
        }
    });
}
