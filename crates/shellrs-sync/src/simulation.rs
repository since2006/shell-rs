//! Many devices doing random things to one store: whatever happens on the
//! way, once every device has synced a few times they all hold the same
//! records, nothing points at a deletion, nothing anyone did not delete is
//! lost, and no state ever shows a secret.

use std::collections::{BTreeMap, BTreeSet};

use crate::fixup;
use crate::record::RecordSet;
use crate::secret;
use crate::store::{MemoryStore, Op};
use crate::testing::{Device, credential, group, host, key_credential, password};
use crate::vault::tests::vault;
use crate::wire::bookmark_id;
use crate::{
    Auth, BookmarkData, CredentialKind, DeviceId, FixedEnv, ForwardData, ForwardKind, Keyring,
    Kind, Pushed, RecordData, RecordKey, RoundOptions, Route, Secret, SnippetCategoryData,
    SnippetData, SnippetScope, StoreError, SyncError,
};

/// What every secret value in a history starts with: a state showing it
/// shows a secret.
const SECRET_MARK: &str = "s3cr3t";

const START: u64 = 1_800_000_000_000;
const HOUR: u64 = 3_600_000;

struct Sim {
    store: MemoryStore,
    keys: Keyring,
    devices: Vec<Device>,
    /// Each device's clock, as an offset from the true time.
    skews: Vec<i64>,
    now: u64,
    rng: FixedEnv,
    next_id: u64,
    created: BTreeSet<RecordKey>,
    deleted: BTreeSet<RecordKey>,
    /// Deleted by a device that had synced them: such a deletion reaches
    /// every device. (A device copied before it synced a record knows
    /// nothing of it; deleting it there is deleting a record never had.)
    deleted_synced: BTreeSet<RecordKey>,
    /// Edits a copied device took along before they were synced: both
    /// copies submit them, each dated by its own clock, so one may come
    /// after a deletion made meanwhile.
    copied_unsynced: BTreeSet<RecordKey>,
    /// What a record cannot live without (a forward's host).
    needs: BTreeMap<RecordKey, RecordKey>,
    /// How often each record was saved, by any device.
    saves: BTreeMap<RecordKey, u32>,
}

impl Sim {
    fn new(seed: u64) -> Self {
        let store = MemoryStore::new();
        let keys = vault(&store, &mut FixedEnv::new(0, seed));
        let mut sim = Self {
            store,
            keys,
            devices: Vec::new(),
            skews: Vec::new(),
            now: START,
            rng: FixedEnv::new(0, seed),
            next_id: 0,
            created: BTreeSet::new(),
            deleted: BTreeSet::new(),
            deleted_synced: BTreeSet::new(),
            copied_unsynced: BTreeSet::new(),
            needs: BTreeMap::new(),
            saves: BTreeMap::new(),
        };
        let count = 2 + sim.roll(3) as usize;
        for name in ['a', 'b', 'c', 'd'].into_iter().take(count) {
            let device = Device::new(name, &sim.keys, START);
            sim.devices.push(device);
            sim.skews.push(0);
        }
        sim
    }

    fn roll(&mut self, n: u64) -> u64 {
        self.rng.next_u64() % n
    }

    fn pick<T: Clone>(&mut self, items: &[T]) -> Option<T> {
        (!items.is_empty()).then(|| items[self.roll(items.len() as u64) as usize].clone())
    }

    fn fresh_id(&mut self) -> String {
        self.next_id += 1;
        format!("r{}", self.next_id)
    }

    fn secret_value(&mut self) -> Secret {
        self.next_id += 1;
        Secret::new(format!("{SECRET_MARK}{}", self.next_id))
    }

    /// A secret for an endpoint (a few, so devices save the same one) or
    /// for one of the credentials.
    fn create_secret(&mut self, d: usize, credentials: &[String]) {
        if self.roll(2) == 0 {
            let host = format!("srv{}", self.roll(4));
            let (id, mut data) = password("root", &host, "");
            if let RecordData::Password(endpoint) = &mut data {
                endpoint.value = self.secret_value();
            }
            self.create(d, id, data, None);
            return;
        }
        let Some(id) = self.pick(credentials) else {
            return;
        };
        let value = self.secret_value();
        let data = match self.devices[d].get(Kind::Credential, &id) {
            Some(RecordData::Credential(c)) if c.kind == CredentialKind::Key => {
                if self.roll(2) == 0 {
                    RecordData::Passphrase(value)
                } else {
                    RecordData::PrivateKey(value)
                }
            }
            _ => RecordData::CredentialPassword(value),
        };
        self.create(
            d,
            id.clone(),
            data,
            Some(RecordKey::new(Kind::Credential, id)),
        );
    }

    /// Set device `d`'s clock from the true time and its skew.
    fn clock(&mut self, d: usize) {
        let at = self.now.saturating_add_signed(self.skews[d]);
        self.devices[d].env.set_now(at);
    }

    fn ids(&self, d: usize, kind: Kind) -> Vec<String> {
        self.devices[d]
            .app
            .keys()
            .filter(|key| key.kind == kind)
            .map(|key| key.id.clone())
            .collect()
    }

    fn create(&mut self, d: usize, id: String, data: RecordData, needs: Option<RecordKey>) {
        let key = RecordKey::new(data.kind(), id.clone());
        self.created.insert(key.clone());
        if let Some(parent) = needs {
            self.needs.insert(key, parent);
        }
        self.save(d, &id, data);
    }

    /// Device `d`'s user saves the record.
    fn save(&mut self, d: usize, id: &str, data: RecordData) {
        *self
            .saves
            .entry(RecordKey::new(data.kind(), id))
            .or_default() += 1;
        self.devices[d].put(id, data);
    }

    fn step(&mut self, d: usize) {
        self.clock(d);
        let groups = self.ids(d, Kind::Group);
        let hosts = self.ids(d, Kind::Host);
        let credentials = self.ids(d, Kind::Credential);
        let categories = self.ids(d, Kind::SnippetCategory);
        match self.roll(19) {
            17 | 18 => self.create_secret(d, &credentials),
            0 => {
                let parent = self.maybe(&groups);
                let id = self.fresh_id();
                self.create(d, id.clone(), group(&id, parent.as_deref()), None);
            }
            1 | 2 => {
                let id = self.fresh_id();
                let mut data = host(&id, self.maybe(&groups).as_deref());
                if let RecordData::Host(h) = &mut data {
                    if let Some(credential) = self.maybe(&credentials) {
                        h.auth = Auth::Credential;
                        h.credential = Some(credential);
                        h.username = None;
                    }
                    if let Some(hop) = self.maybe(&hosts) {
                        h.route = Route::Jump(vec![Some(hop)]);
                    }
                }
                self.create(d, id, data, None);
            }
            3 => {
                let id = self.fresh_id();
                let data = if self.roll(2) == 0 {
                    credential(&id)
                } else {
                    key_credential(&id)
                };
                self.create(d, id, data, None);
            }
            4 => {
                let Some(on) = self.pick(&hosts) else { return };
                let id = self.fresh_id();
                let data = RecordData::Forward(ForwardData {
                    host: on.clone(),
                    name: id.clone(),
                    kind: ForwardKind::Dynamic,
                    bind_host: "127.0.0.1".into(),
                    bind_port: 1080,
                    target: None,
                    sort_order: 0,
                });
                self.create(d, id, data, Some(RecordKey::new(Kind::Host, on)));
            }
            5 => {
                let Some(on) = self.pick(&hosts) else { return };
                let path = format!("/srv/{}", self.roll(3));
                let data = RecordData::Bookmark(BookmarkData {
                    host: on.clone(),
                    path: path.clone(),
                    sort_order: 0,
                });
                self.create(
                    d,
                    bookmark_id(&on, &path),
                    data,
                    Some(RecordKey::new(Kind::Host, on)),
                );
            }
            6 => {
                let id = self.fresh_id();
                let data = RecordData::SnippetCategory(SnippetCategoryData {
                    name: id.clone(),
                    parent: self.maybe(&categories),
                    sort_order: 0,
                });
                self.create(d, id, data, None);
            }
            7 => {
                let id = self.fresh_id();
                let (scope, needs) = match self.roll(3) {
                    0 => match self.pick(&hosts) {
                        Some(h) => (
                            SnippetScope::Host(h.clone()),
                            Some(RecordKey::new(Kind::Host, h)),
                        ),
                        None => (SnippetScope::All, None),
                    },
                    1 => match self.pick(&groups) {
                        Some(g) => (
                            SnippetScope::Group(g.clone()),
                            Some(RecordKey::new(Kind::Group, g)),
                        ),
                        None => (SnippetScope::All, None),
                    },
                    _ => (SnippetScope::All, None),
                };
                let data = RecordData::Snippet(SnippetData {
                    category: self.maybe(&categories),
                    name: id.clone(),
                    command: "uptime".into(),
                    scope,
                    run_on_click: false,
                    sort_order: 0,
                });
                self.create(d, id, data, needs);
            }
            8 => self.rename(d),
            9 => self.move_host(d, &hosts, &groups),
            10 => self.move_group(d, &groups),
            11 | 12 => self.delete(d),
            13 | 14 => {
                // Now and then the keychain does not open.
                self.devices[d].keychain_ok = self.roll(8) != 0;
                self.sync(d);
                self.devices[d].keychain_ok = true;
            }
            15 => self.mishap(d),
            _ => {
                self.now += self.roll(10 * 60_000);
                if self.roll(6) == 0 {
                    // A clock set wrong, or set right again: it may go back.
                    self.skews[d] = self.roll(4 * HOUR) as i64 - 2 * HOUR as i64;
                }
            }
        }
        self.now += 1 + self.roll(5_000);
    }

    fn maybe(&mut self, items: &[String]) -> Option<String> {
        if self.roll(3) == 0 {
            None
        } else {
            self.pick(items)
        }
    }

    fn rename(&mut self, d: usize) {
        let keys: Vec<RecordKey> = self.devices[d].app.keys().cloned().collect();
        let Some(key) = self.pick(&keys) else { return };
        let mut data = self.devices[d].app[&key].0.clone();
        let name = format!("renamed {}", self.roll(1000));
        let value = self.secret_value();
        match &mut data {
            RecordData::Group(g) => g.name = name,
            RecordData::Host(h) => h.name = name,
            RecordData::Credential(c) => c.name = name,
            RecordData::Forward(f) => f.name = name,
            RecordData::SnippetCategory(c) => c.name = name,
            RecordData::Snippet(s) => s.name = name,
            RecordData::Bookmark(b) => b.sort_order += 1,
            RecordData::Password(e) | RecordData::ProxyPassword(e) => e.value = value,
            RecordData::CredentialPassword(s)
            | RecordData::Passphrase(s)
            | RecordData::PrivateKey(s)
            | RecordData::Totp(s) => *s = value,
        }
        self.save(d, &key.id, data);
    }

    fn move_host(&mut self, d: usize, hosts: &[String], groups: &[String]) {
        let Some(id) = self.pick(hosts) else { return };
        let to = self.maybe(groups);
        let Some(RecordData::Host(mut h)) = self.devices[d].get(Kind::Host, &id).cloned() else {
            return;
        };
        h.group = to;
        self.save(d, &id, RecordData::Host(h));
    }

    /// Move a group under another, as the app allows: never under itself
    /// or its own descendants. Cycles come only from two devices at once.
    fn move_group(&mut self, d: usize, groups: &[String]) {
        let (Some(id), to) = (self.pick(groups), self.maybe(groups)) else {
            return;
        };
        let parent_of = |g: &str| match self.devices[d].get(Kind::Group, g) {
            Some(RecordData::Group(data)) => data.parent.clone(),
            _ => None,
        };
        let mut at = to.clone();
        while let Some(g) = at {
            if g == id {
                return;
            }
            at = parent_of(&g);
        }
        let Some(RecordData::Group(mut data)) = self.devices[d].get(Kind::Group, &id).cloned()
        else {
            return;
        };
        data.parent = to;
        self.save(d, &id, RecordData::Group(data));
    }

    /// Delete a record as the app does, with what goes with it.
    fn delete(&mut self, d: usize) {
        let keys: Vec<RecordKey> = self.devices[d].app.keys().cloned().collect();
        let Some(key) = self.pick(&keys) else { return };
        let mut doomed = BTreeSet::from([key]);
        // What goes with a deleted group, host or category, until nothing
        // more does.
        loop {
            let before = doomed.len();
            for (other, (data, _)) in &self.devices[d].app {
                let gone = |kind: Kind, id: &str| doomed.contains(&RecordKey::new(kind, id));
                let goes = match data {
                    RecordData::Group(g) => {
                        g.parent.as_deref().is_some_and(|p| gone(Kind::Group, p))
                    }
                    RecordData::Host(h) => h.group.as_deref().is_some_and(|g| gone(Kind::Group, g)),
                    RecordData::SnippetCategory(c) => c
                        .parent
                        .as_deref()
                        .is_some_and(|p| gone(Kind::SnippetCategory, p)),
                    RecordData::Forward(f) => gone(Kind::Host, &f.host),
                    RecordData::Bookmark(b) => gone(Kind::Host, &b.host),
                    // The app forgets a credential's secrets with it.
                    RecordData::CredentialPassword(_)
                    | RecordData::Passphrase(_)
                    | RecordData::PrivateKey(_) => gone(Kind::Credential, &other.id),
                    RecordData::Snippet(s) => {
                        s.category
                            .as_deref()
                            .is_some_and(|c| gone(Kind::SnippetCategory, c))
                            || match &s.scope {
                                SnippetScope::All => false,
                                SnippetScope::Group(g) => gone(Kind::Group, g),
                                SnippetScope::Host(h) => gone(Kind::Host, h),
                            }
                    }
                    _ => false,
                };
                if goes {
                    doomed.insert(other.clone());
                }
            }
            if doomed.len() == before {
                break;
            }
        }
        for key in &doomed {
            if self.devices[d].state.app_had.contains(key) {
                self.deleted_synced.insert(key.clone());
            }
            self.devices[d].remove(key.kind.clone(), &key.id);
            self.deleted.insert(key.clone());
        }
        // What the app changes on the user's behalf: hosts lose a deleted
        // hop or credential.
        let credentials: BTreeMap<String, String> = doomed
            .iter()
            .filter(|key| key.kind == Kind::Credential)
            .map(|key| (key.id.clone(), "deploy".to_owned()))
            .collect();
        let hosts: Vec<(String, RecordData)> = self.devices[d]
            .app
            .iter()
            .filter(|(key, _)| key.kind == Kind::Host)
            .map(|(key, (data, _))| (key.id.clone(), data.clone()))
            .collect();
        for (id, data) in hosts {
            let RecordData::Host(mut h) = data else {
                continue;
            };
            let mut changed = false;
            if let Some(username) = h.credential.as_ref().and_then(|c| credentials.get(c)) {
                h.auth = Auth::Password;
                h.username = Some(username.clone());
                h.credential = None;
                changed = true;
            }
            if let Route::Jump(hops) = &mut h.route {
                for hop in hops.iter_mut() {
                    if hop.as_ref().is_some_and(|hop| {
                        doomed.contains(&RecordKey::new(Kind::Host, hop.clone()))
                    }) {
                        *hop = None;
                        changed = true;
                    }
                }
            }
            if changed {
                self.save(d, &id, RecordData::Host(h));
            }
        }
    }

    /// A round, as the app runs it, riding out what goes wrong.
    fn sync(&mut self, d: usize) -> bool {
        self.clock(d);
        let device = &mut self.devices[d];
        let pulled = match device.pull(&self.store) {
            Err(SyncError::MassDelete { .. }) => device.pull_with(
                &self.store,
                RoundOptions {
                    allow_mass_delete: true,
                    ..RoundOptions::default()
                },
            ),
            result => result,
        };
        let Ok(pulled) = pulled else { return false };
        device.apply(&pulled);
        if pulled.report.device_id_conflict {
            let id = DeviceId::generate(&mut device.env).unwrap();
            device.state = device.state.clone().with_device(id);
        }
        matches!(
            device.push(&self.store),
            Ok(Pushed::Uploaded { .. } | Pushed::Unchanged)
        )
    }

    fn mishap(&mut self, d: usize) {
        match self.roll(5) {
            // A pull whose result the app throws away.
            0 => {
                self.clock(d);
                let _ = self.devices[d].pull(&self.store);
            }
            // A crash between uploading and storing the state.
            1 => {
                self.clock(d);
                let device = &mut self.devices[d];
                if let Ok(pulled) = device.pull(&self.store) {
                    device.apply(&pulled);
                    let stored = device.state.clone();
                    let _ = device.push(&self.store);
                    device.state = stored;
                }
            }
            // The store fails a call.
            2 => {
                let error = match self.roll(3) {
                    0 => StoreError::Timeout,
                    1 => StoreError::Unavailable("down".into()),
                    _ => StoreError::PreconditionFailed,
                };
                let which: fn(&Op) -> bool = match self.roll(3) {
                    0 => |op| matches!(op, Op::List(_)),
                    1 => |op| matches!(op, Op::Get(_)),
                    _ => |op| matches!(op, Op::Put(_)),
                };
                self.store.fail_next(which, error);
            }
            // The data folder copied to another computer.
            3 if self.devices.len() < 5 => {
                let original = &self.devices[d];
                for (key, (_, edited)) in &original.app {
                    if original.state.edited_seen.get(key) != Some(edited) {
                        self.copied_unsynced.insert(key.clone());
                    }
                }
                let mut twin = Device::new('t', &self.keys, original.now());
                twin.state = original.state.clone();
                twin.app = original.app.clone();
                twin.keys = original.keys.clone();
                twin.env = FixedEnv::new(original.now(), self.rng.next_u64());
                self.devices.push(twin);
                self.skews.push(self.skews[d]);
            }
            _ => {
                self.now += HOUR;
            }
        }
    }
}

fn run(seed: u64, steps: usize) {
    let mut sim = Sim::new(seed);
    for _ in 0..steps {
        let d = sim.roll(sim.devices.len() as u64) as usize;
        sim.step(d);
    }
    sim.store.clear_failures();
    // Everyone syncs, a few times over.
    for _ in 0..3 {
        for d in 0..sim.devices.len() {
            sim.now += 1000;
            for _ in 0..3 {
                if sim.sync(d) {
                    break;
                }
            }
        }
    }
    // Then a round changes nothing anywhere.
    for d in 0..sim.devices.len() {
        sim.clock(d);
        let device = &mut sim.devices[d];
        let pulled = device.pull(&sim.store).unwrap();
        assert_eq!(pulled.changes, [], "seed {seed}: device {d} still changes");
        device.apply(&pulled);
        assert!(
            matches!(device.push(&sim.store).unwrap(), Pushed::Unchanged),
            "seed {seed}: device {d} still uploads"
        );
    }
    let first = &sim.devices[0];
    let everything = |device: &Device| {
        let mut set: RecordSet = device.state.records.clone();
        if let Some(sealed) = &device.state.secrets {
            set.extend(secret::open(sealed, &device.keys).expect("its secrets open"));
        }
        set
    };
    for (d, device) in sim.devices.iter().enumerate() {
        assert_eq!(
            device.data(),
            first.data(),
            "seed {seed}: device {d} differs"
        );
        assert_eq!(
            everything(device),
            everything(first),
            "seed {seed}: device {d}"
        );
        let state = device.state.to_bytes();
        assert!(
            !state
                .windows(SECRET_MARK.len())
                .any(|w| w == SECRET_MARK.as_bytes()),
            "seed {seed}: device {d}'s state shows a secret"
        );
    }
    assert!(
        fixup::repairs(&everything(first)).is_empty(),
        "seed {seed}: something points at a deletion"
    );
    let held = first.data();
    for key in &sim.created {
        let lost_with = sim
            .needs
            .get(key)
            .is_some_and(|parent| sim.deleted.contains(parent));
        if !sim.deleted.contains(key) && !lost_with {
            assert!(held.contains_key(key), "seed {seed}: {key} was lost");
        }
        // Deleted and never edited by anyone: nothing can bring it back.
        if sim.deleted_synced.contains(key)
            && sim.saves.get(key) == Some(&1)
            && !sim.copied_unsynced.contains(key)
        {
            assert!(!held.contains_key(key), "seed {seed}: {key} came back");
        }
    }
}

#[test]
fn devices_converge_whatever_happens() {
    for seed in 0..24 {
        run(seed, 250);
    }
}

/// Many more histories, for when the merge or the round changes:
/// `cargo test -p shellrs-sync --release many_histories -- --ignored`.
#[test]
#[ignore]
fn devices_converge_over_many_histories() {
    for seed in 24..1000 {
        run(seed, 400);
    }
}
