//! Rounds between devices sharing a store.

use std::io::Write as _;

use flate2::Compression;
use flate2::write::DeflateEncoder;
use serde_json::{Value, json};

use crate::record::RawRecord;
use crate::snapshot::{self, Snapshot};
use crate::store::{MemoryStore, Op};
use crate::testing::{Device, credential, group, host, password};
use crate::vault::tests::vault;
use crate::{
    Auth, Capabilities, Change, DeviceId, FixedEnv, HostData, JoinMode, Keyring, Kind, Protocol,
    Pushed, RecordData, RecordKey, RoundOptions, StoreError, SyncError, TelnetData, Warning,
    envelope, layout,
};

const START: u64 = 1_800_000_000_000;

pub(crate) fn setup() -> (MemoryStore, Keyring, Device, Device) {
    let store = MemoryStore::new();
    let keys = vault(&store, &mut FixedEnv::new(0, 99));
    let a = Device::new('a', &keys, START);
    let b = Device::new('b', &keys, START + 7);
    (store, keys, a, b)
}

fn key(kind: Kind, id: &str) -> RecordKey {
    RecordKey::new(kind, id)
}

fn name(device: &Device, id: &str) -> String {
    match device.get(Kind::Host, id) {
        Some(RecordData::Host(host)) => host.name.clone(),
        other => panic!("{other:?}"),
    }
}

/// A file for a device that is not in the test, holding `snapshot`, its
/// device described.
pub(crate) fn plant(
    store: &MemoryStore,
    keys: &Keyring,
    device: char,
    mut snapshot: Value,
) -> DeviceId {
    let id = DeviceId::parse(&device.to_string().repeat(16)).unwrap();
    for (key, value) in [("name", "planted"), ("platform", "test"), ("app", "0.0.0")] {
        snapshot["device"][key] = value.into();
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&serde_json::to_vec(&snapshot).unwrap())
        .unwrap();
    let plain = encoder.finish().unwrap();
    let sealed = envelope::seal(&plain, &id, keys, &mut FixedEnv::new(0, 5)).unwrap();
    store.set_raw(&layout::device_file(&id), sealed);
    id
}

pub(crate) fn read_file(store: &MemoryStore, keys: &Keyring, device: &DeviceId) -> Snapshot {
    let bytes = store.raw(&layout::device_file(device)).unwrap();
    let plain = envelope::open(&bytes, device, keys).unwrap();
    snapshot::decode(&plain, device).unwrap()
}

#[test]
fn a_second_device_gets_the_first_ones_records_in_order() {
    let (store, _, mut a, mut b) = setup();
    a.put("h1", host("web", Some("g1")));
    a.put("g1", group("Production", None));
    a.put("c1", credential("deploy"));
    let (report, pushed) = a.sync(&store);
    assert_eq!(report.local_edits, 3);
    assert!(matches!(pushed, Pushed::Uploaded { .. }));

    let pulled = b.pull(&store).unwrap();
    let order: Vec<_> = pulled.changes.iter().map(Change::key).collect();
    assert_eq!(
        order,
        [
            key(Kind::Group, "g1"),
            key(Kind::Credential, "c1"),
            key(Kind::Host, "h1")
        ]
    );
    b.apply(&pulled);
    assert_eq!(b.data(), a.data());
    assert_eq!(pulled.report.devices.len(), 1);
    assert_eq!(pulled.report.devices[0].info.name, "device a");
}

#[test]
fn edits_and_deletions_travel() {
    let (store, _, mut a, mut b) = setup();
    a.put("h1", host("web", None));
    a.put("h2", host("db", None));
    a.sync(&store);
    b.sync(&store);

    a.wait(1000);
    a.put("h1", host("web-01", None));
    a.sync(&store);
    b.sync(&store);
    assert_eq!(name(&b, "h1"), "web-01");

    b.wait(1000);
    b.remove(Kind::Host, "h2");
    let (report, _) = b.sync(&store);
    assert_eq!(report.local_deletes, 1);
    a.sync(&store);
    assert_eq!(a.get(Kind::Host, "h2"), None);
    assert_eq!(a.data(), b.data());
}

/// Concurrent edits: the one made later wins, whichever device syncs
/// first, since an edit is dated when it was made.
#[test]
fn the_later_edit_wins() {
    for a_first in [true, false] {
        let (store, _, mut a, mut b) = setup();
        a.put("h1", host("web", None));
        a.sync(&store);
        b.sync(&store);
        a.wait(100);
        a.put("h1", host("from a", None));
        b.wait(5000);
        b.put("h1", host("from b", None));
        // A syncs well after B's edit: still A's edit is the older.
        a.wait(20_000);
        if a_first {
            a.sync(&store);
            b.sync(&store);
        } else {
            b.sync(&store);
            a.sync(&store);
        }
        a.sync(&store);
        b.sync(&store);
        assert_eq!(name(&a, "h1"), "from b");
        assert_eq!(name(&b, "h1"), "from b");
    }
}

#[test]
fn a_deletion_and_an_edit_race_like_two_edits() {
    let (store, _, mut a, mut b) = setup();
    a.put("h1", host("web", None));
    a.sync(&store);
    b.sync(&store);
    // A deletes first, B edits later: the edit brings the host back.
    a.remove(Kind::Host, "h1");
    a.sync(&store);
    b.wait(60_000);
    b.put("h1", host("kept", None));
    b.sync(&store);
    a.sync(&store);
    assert_eq!(name(&a, "h1"), "kept");
    // B edits, A deletes later: the deletion stands.
    b.put("h1", host("edited", None));
    b.sync(&store);
    a.wait(120_000);
    a.remove(Kind::Host, "h1");
    a.sync(&store);
    b.sync(&store);
    assert_eq!(b.get(Kind::Host, "h1"), None);
}

#[test]
fn nothing_new_reads_and_writes_nothing() {
    let (store, _, mut a, mut b) = setup();
    a.put("h1", host("web", None));
    a.sync(&store);
    b.sync(&store);
    a.sync(&store);
    store.clear_operations();
    let (report, pushed) = a.sync(&store);
    assert!(matches!(pushed, Pushed::Unchanged));
    assert_eq!(report.downloaded, 0);
    assert_eq!(
        store.operations(),
        [Op::List(layout::DEVICES.into())],
        "one listing, nothing read or written"
    );
}

#[test]
fn a_damaged_file_is_left_out_and_the_rest_merged() {
    let (store, _, mut a, mut b) = setup();
    b.put("h1", host("from b", None));
    b.sync(&store);
    let c = DeviceId::parse("cccccccccccccccc").unwrap();
    store.set_raw(&layout::device_file(&c), b"garbage".to_vec());
    let (report, _) = a.sync(&store);
    assert_eq!(name(&a, "h1"), "from b");
    assert!(report.warnings.iter().any(|warning| matches!(
        warning,
        Warning::CorruptSnapshot { device, .. } if device == c.as_str()
    )));
    // It is read again next time, in case it was caught half written.
    store.clear_operations();
    a.sync(&store);
    assert!(
        store
            .operations()
            .contains(&Op::Get(layout::device_file(&c)))
    );
}

#[test]
fn a_file_needing_a_newer_reader_makes_the_device_read_only() {
    let (store, keys, mut a, _) = setup();
    plant(
        &store,
        &keys,
        'c',
        json!({"format": 2, "min_reader": 2, "device": {"id": "cccccccccccccccc"},
            "clock": [1, 0], "records": []}),
    );
    a.put("h1", host("web", None));
    let pulled = a.pull(&store).unwrap();
    assert_eq!(pulled.report.read_only, Some(2));
    a.apply(&pulled);
    assert_eq!(
        a.push(&store).unwrap_err(),
        SyncError::ReadOnly { min_reader: 2 }
    );
}

#[test]
fn a_rotated_key_is_learned_from_the_vault() {
    let (store, _, mut a, mut b) = setup();
    a.sync(&store);
    b.sync(&store);
    a.keys = crate::rotate_key(&store, &a.keys, &mut a.env).unwrap();
    a.wait(10);
    a.put("h1", host("web", None));
    a.sync(&store);
    let pulled = b.pull(&store).unwrap();
    assert_eq!(pulled.keyring.as_ref().unwrap().current_generation(), 2);
    b.apply(&pulled);
    assert_eq!(name(&b, "h1"), "web");
    // B's own file moves to the new key on its next upload, even with
    // nothing else new.
    assert!(matches!(b.push(&store).unwrap(), Pushed::Uploaded { .. }));
}

#[test]
fn keys_the_keychain_let_go_come_back_from_the_vault() {
    let (store, _, mut a, mut b) = setup();
    let (id, secret) = password("root", "10.0.0.1", "hunter2");
    b.put(&id, secret);
    a.sync(&store);
    b.sync(&store);
    // B moves on nine generations; its keychain keeps the newest eight,
    // while its state's secrets and A's file are under the first.
    for _ in 0..9 {
        b.keys = crate::rotate_key(&store, &b.keys, &mut b.env).unwrap();
    }
    b.keys = Keyring::from_bytes(&b.keys.to_bytes()).unwrap();
    assert!(b.keys.key(1).is_none());
    a.wait(10);
    a.put("h1", host("web", None));
    a.sync(&store);

    let pulled = b.pull(&store).unwrap();
    assert!(
        !pulled
            .report
            .warnings
            .iter()
            .any(|warning| matches!(warning, Warning::SecretsUnreadable)),
        "{:?}",
        pulled.report.warnings
    );
    assert!(pulled.keyring.as_ref().unwrap().key(1).is_some());
    b.apply(&pulled);
    assert_eq!(name(&b, "h1"), "web");
    assert!(matches!(b.push(&store).unwrap(), Pushed::Uploaded { .. }));
}

#[test]
fn a_password_changed_elsewhere_is_asked_for() {
    let (store, _, mut a, mut b) = setup();
    a.sync(&store);
    b.sync(&store);
    a.keys = crate::change_password(
        &store,
        &a.keys,
        "new horse",
        crate::vault::tests::FAST,
        &mut a.env,
    )
    .unwrap();
    a.keys = crate::rotate_key(&store, &a.keys, &mut a.env).unwrap();
    a.put("h1", host("web", None));
    a.sync(&store);
    assert_eq!(b.pull(&store).unwrap_err(), SyncError::NeedsPassword);
}

#[test]
fn a_device_sharing_an_id_is_found_and_nothing_is_lost() {
    let (store, _, mut a, _) = setup();
    a.put("h1", host("web", None));
    a.sync(&store);
    // The portable data folder, copied to another computer.
    let mut twin = Device::new('a', &a.keys, a.now());
    twin.state = a.state.clone();
    twin.app = a.app.clone();
    twin.wait(1000);
    twin.put("h2", host("from the copy", None));
    twin.sync(&store);

    a.wait(2000);
    let pulled = a.pull(&store).unwrap();
    assert!(pulled.report.device_id_conflict);
    a.apply(&pulled);
    assert_eq!(name(&a, "h2"), "from the copy");
    // The app gives this device a new id; then it has its own file.
    let new_id = DeviceId::generate(&mut a.env).unwrap();
    a.state = a.state.clone().with_device(new_id.clone());
    assert!(matches!(a.push(&store).unwrap(), Pushed::Uploaded { .. }));
    assert!(store.raw(&layout::device_file(&new_id)).is_some());
    let (report, _) = a.sync(&store);
    assert!(!report.device_id_conflict);
}

#[test]
fn a_crash_after_uploading_is_not_taken_for_a_twin() {
    let (store, _, mut a, _) = setup();
    for _ in 0..2 {
        a.wait(1000);
        a.put("h1", host(&format!("web {}", a.now()), None));
        let pulled = a.pull(&store).unwrap();
        a.apply(&pulled);
        // Uploaded, but the state saying so was never stored.
        let stored = a.state.clone();
        a.push(&store).unwrap();
        a.state = stored;
        let (report, pushed) = a.sync(&store);
        assert!(!report.device_id_conflict);
        // Which key the lost upload used is unknown: written again.
        assert!(matches!(pushed, Pushed::Uploaded { .. }));
    }
}

#[test]
fn a_lost_conditional_write_is_a_conflict() {
    let (store, _, mut a, _) = setup();
    a.sync(&store);
    a.put("h1", host("web", None));
    let pulled = a.pull(&store).unwrap();
    a.apply(&pulled);
    store.fail_next(
        |op| matches!(op, Op::Put(_)),
        StoreError::PreconditionFailed,
    );
    assert!(matches!(a.push(&store).unwrap(), Pushed::Conflict));
}

#[test]
fn a_failed_call_stops_the_round() {
    let (store, _, mut a, mut b) = setup();
    b.put("h1", host("web", None));
    b.sync(&store);
    store.fail_next(|op| matches!(op, Op::List(_)), StoreError::Timeout);
    assert_eq!(
        a.pull(&store).unwrap_err(),
        SyncError::Store(StoreError::Timeout)
    );
    store.fail_next(
        |op| matches!(op, Op::Get(_)),
        StoreError::Unavailable("down".into()),
    );
    assert!(matches!(
        a.pull(&store),
        Err(SyncError::Store(StoreError::Unavailable(_)))
    ));
    a.sync(&store);
    assert_eq!(name(&a, "h1"), "web");
}

#[test]
fn deleting_most_records_at_once_asks_first() {
    let (store, _, mut a, _) = setup();
    for i in 0..12 {
        a.put(&format!("h{i}"), host("web", None));
    }
    a.sync(&store);
    for i in 0..11 {
        a.remove(Kind::Host, &format!("h{i}"));
    }
    assert_eq!(
        a.pull(&store).unwrap_err(),
        SyncError::MassDelete { count: 11, of: 12 }
    );
    let pulled = a
        .pull_with(
            &store,
            RoundOptions {
                allow_mass_delete: true,
                ..RoundOptions::default()
            },
        )
        .unwrap();
    assert_eq!(pulled.report.local_deletes, 11);
}

#[test]
fn a_pull_thrown_away_is_harmless() {
    let (store, _, mut a, mut b) = setup();
    a.put("h1", host("web", None));
    a.sync(&store);
    let first = b.pull(&store).unwrap();
    let second = b.pull(&store).unwrap();
    assert_eq!(first.changes, second.changes);
    b.apply(&second);
    assert_eq!(b.data(), a.data());
}

/// The app storing a record its own way (here, trimming the notes) is not
/// an edit: it is given the record again, and nothing is uploaded.
#[test]
fn the_app_reshaping_a_record_is_not_an_edit() {
    let (store, _, mut a, mut b) = setup();
    let mut web = host("web", None);
    if let RecordData::Host(host) = &mut web {
        host.notes = "behind the bastion ".into();
    }
    a.put("h1", web);
    a.sync(&store);
    b.sync(&store);
    let k = key(Kind::Host, "h1");
    if let Some((RecordData::Host(host), _)) = b.app.get_mut(&k) {
        host.notes = "behind the bastion".into();
    }
    let (report, pushed) = b.sync(&store);
    assert_eq!(report.local_edits, 0);
    assert!(matches!(pushed, Pushed::Unchanged));
    assert_eq!(b.data(), a.data());
}

fn category(name: &str, parent: Option<&str>, sort_order: i64) -> RecordData {
    RecordData::SnippetCategory(crate::SnippetCategoryData {
        name: name.into(),
        parent: parent.map(Into::into),
        sort_order,
    })
}

#[test]
fn a_category_comes_after_the_one_it_is_in() {
    let (store, _, mut a, mut b) = setup();
    a.put("s1", category("logs", Some("s3"), 0));
    a.put("s2", category("nginx", Some("s1"), 1));
    a.put("s3", category("ops", None, 0));
    a.sync(&store);
    let pulled = b.pull(&store).unwrap();
    let order: Vec<_> = pulled
        .changes
        .iter()
        .map(|change| change.key().id)
        .collect();
    assert_eq!(order, ["s3", "s1", "s2"]);
    b.apply(&pulled);
    assert_eq!(b.data(), a.data());
}

/// A category or snippet an older version wrote has no `parent` and no
/// `sort_order`: it reads as at the top, first, and is not taken for an
/// edit; an edit then writes both.
#[test]
fn a_copy_from_before_snippets_were_ordered_is_used_as_it_is() {
    let (store, keys, mut a, mut b) = setup();
    let c = "cccccccccccccccc";
    let planted = json!({"format": 1, "min_reader": 1, "device": {"id": c},
    "clock": [START, 0], "records": [
        {"kind": "snippet_category", "id": "s1", "v": [START, 0, c], "data": {"name": "logs"}},
    ]});
    plant(&store, &keys, 'c', planted);
    b.sync(&store);
    assert_eq!(
        b.get(Kind::SnippetCategory, "s1"),
        Some(&category("logs", None, 0))
    );
    let (report, pushed) = b.sync(&store);
    assert_eq!(report.local_edits, 0);
    assert!(matches!(pushed, Pushed::Unchanged));

    b.wait(1000);
    b.put("s1", category("logs", None, 2));
    b.sync(&store);
    let file = read_file(&store, &keys, b.state.device());
    let s1 = file.records[&key(Kind::SnippetCategory, "s1")]
        .live()
        .unwrap();
    assert_eq!(s1["parent"], Value::Null);
    assert_eq!(s1["sort_order"], json!(2));
    a.sync(&store);
    assert_eq!(a.data(), b.data());
}

/// Records this version cannot use — an unknown kind, a field it does not
/// know, a value it cannot store — are kept and written back as they came.
#[test]
fn records_this_version_cannot_use_pass_through() {
    let (store, keys, mut a, mut b) = setup();
    let c = "cccccccccccccccc";
    let mut written = host("web", None);
    if let RecordData::Host(host) = &mut written {
        host.notes = "x".into();
    }
    let mut fields = crate::wire::write_into(&written, None);
    fields.insert("color".into(), json!("red"));
    let mut mosh = fields.clone();
    mosh.insert("protocol".into(), json!("mosh"));
    let planted = json!({"format": 1, "min_reader": 1, "device": {"id": c},
    "clock": [START, 0], "records": [
        {"kind": "host", "id": "h1", "v": [START, 0, c], "data": fields},
        {"kind": "host", "id": "h2", "v": [START, 0, c], "data": mosh},
        {"kind": "tag", "id": "t1", "v": [START, 0, c], "data": {"label": "prod"}, "signed": true},
        {"kind": "host", "id": "h3", "v": [START, 0, c], "data": fields, "min_reader": 7}
    ]});
    plant(&store, &keys, 'c', planted);
    let (report, _) = b.sync(&store);
    assert_eq!(b.data().len(), 1, "only the host it can use");
    let parked: Vec<_> = report
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            Warning::Parked { id, .. } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(parked, ["h2", "h3", "t1"]);

    // B edits the host it can use; the field it does not know stays.
    b.wait(1000);
    let mut edited = written.clone();
    if let RecordData::Host(host) = &mut edited {
        host.name = "web-01".into();
    }
    b.put("h1", edited);
    b.sync(&store);
    let file = read_file(&store, &keys, b.state.device());
    let h1 = &file.records[&key(Kind::Host, "h1")];
    assert_eq!(h1.live().unwrap()["color"], json!("red"));
    assert_eq!(h1.live().unwrap()["name"], json!("web-01"));
    let c = DeviceId::parse(c).unwrap();
    let theirs = read_file(&store, &keys, &c);
    for id in ["h2", "h3"] {
        assert_eq!(
            file.records[&key(Kind::Host, id)],
            theirs.records[&key(Kind::Host, id)]
        );
    }
    let tag = key(Kind::Other("tag".into()), "t1");
    assert_eq!(file.records[&tag], theirs.records[&tag]);
    assert_eq!(
        RawRecord::to_json(&file.records[&tag])["signed"],
        json!(true)
    );

    // A, which never had them either, passes them on too.
    a.sync(&store);
    assert_eq!(a.data(), b.data());
}

#[test]
fn a_device_joining_can_take_the_others_records_only() {
    let (store, _, mut a, mut b) = setup();
    a.put("h1", host("web", None));
    a.sync(&store);
    b.put("h9", host("only on b", None));
    let pulled = b
        .pull_with(
            &store,
            RoundOptions {
                join: JoinMode::UseRemote,
                ..RoundOptions::default()
            },
        )
        .unwrap();
    b.apply(&pulled);
    assert_eq!(b.data(), a.data());
    b.push(&store).unwrap();
    a.sync(&store);
    assert_eq!(a.get(Kind::Host, "h9"), None);
}

#[test]
fn a_device_removed_elsewhere_is_told() {
    let (store, _, mut a, _) = setup();
    a.sync(&store);
    store.remove_raw(&layout::device_file(a.state.device()));
    let (report, pushed) = a.sync(&store);
    assert!(report.removed);
    assert!(matches!(pushed, Pushed::Uploaded { .. }));
}

#[test]
fn a_reset_vault_stops_the_round() {
    let (store, _, mut a, _) = setup();
    a.sync(&store);
    for path in store.paths() {
        store.remove_raw(&path);
    }
    let keys = vault(&store, &mut FixedEnv::new(0, 1234));
    let mut c = Device::new('c', &keys, START);
    c.sync(&store);
    assert_eq!(a.pull(&store).unwrap_err(), SyncError::VaultReplaced);
}

/// A device without Telnet never gets the Telnet host, keeps it for the
/// others, and still syncs the rest.
#[test]
fn a_device_that_cannot_store_a_record_keeps_it_for_the_others() {
    let (store, _, mut a, mut b) = setup();
    let telnet = RecordData::Host(HostData {
        port: 23,
        protocol: Protocol::Telnet(TelnetData::default()),
        ..crate::wire::tests::ssh_host("switch")
    });
    a.put("t1", telnet.clone());
    a.put("h1", host("web", None));
    a.sync(&store);
    b.caps = Capabilities { telnet: false };
    b.sync(&store);
    assert_eq!(b.get(Kind::Host, "t1"), None);
    assert_eq!(name(&b, "h1"), "web");
    b.wait(1000);
    b.put("h1", host("web-01", None));
    b.sync(&store);
    a.sync(&store);
    assert_eq!(a.get(Kind::Host, "t1"), Some(&telnet));
    assert_eq!(name(&a, "h1"), "web-01");
}

/// A group B cannot read shows as no group; B editing the host keeps the
/// host in it.
#[test]
fn a_reference_shown_empty_is_kept() {
    let (store, keys, mut a, mut b) = setup();
    a.put("g1", group("Production", None));
    a.put("h1", host("web", Some("g1")));
    a.sync(&store);
    let c = "cccccccccccccccc";
    plant(
        &store,
        &keys,
        'c',
        json!({"format": 1, "min_reader": 1, "device": {"id": c}, "clock": [START + 10_000, 0],
            "records": [{"kind": "group", "id": "g1", "v": [START + 10_000, 0, c],
                "data": {"name": "Production"}, "min_reader": 5}]}),
    );
    b.sync(&store);
    let Some(RecordData::Host(shown)) = b.get(Kind::Host, "h1").cloned() else {
        panic!()
    };
    assert_eq!(shown.group, None);
    b.wait(1000);
    b.put(
        "h1",
        RecordData::Host(HostData {
            name: "web-01".into(),
            ..shown
        }),
    );
    b.sync(&store);
    let record = &b.state.records[&key(Kind::Host, "h1")];
    assert_eq!(record.live().unwrap()["group"], json!("g1"));
    assert_eq!(record.live().unwrap()["name"], json!("web-01"));
}

/// Deleting a credential on one device while another assigns it: the
/// host falls back to a password, the same on both.
#[test]
fn a_host_whose_credential_is_deleted_signs_in_by_password() {
    let (store, _, mut a, mut b) = setup();
    a.put("c1", credential("deploy"));
    a.put("h1", host("web", None));
    a.sync(&store);
    b.sync(&store);
    a.wait(1000);
    a.remove(Kind::Credential, "c1");
    a.sync(&store);
    b.wait(500);
    b.put(
        "h1",
        RecordData::Host(HostData {
            auth: Auth::Credential,
            username: None,
            credential: Some("c1".into()),
            ..crate::wire::tests::ssh_host("web")
        }),
    );
    b.sync(&store);
    a.sync(&store);
    b.sync(&store);
    let Some(RecordData::Host(on_a)) = a.get(Kind::Host, "h1") else {
        panic!()
    };
    assert_eq!(on_a.auth, Auth::Password);
    assert_eq!(on_a.username.as_deref(), Some(""));
    assert_eq!(a.data(), b.data());
}
