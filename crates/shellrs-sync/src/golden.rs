//! Files of format 1 as this version wrote them, kept in `testdata/v1`:
//! every later version must still read them the same. Only reading is
//! checked; the bytes a version writes may differ (compression varies
//! with the backend, nonces are random).

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::record::{Body, RawRecord, RecordSet};
use crate::store::MemoryStore;
use crate::vault::tests::FAST;
use crate::{
    DeviceId, DeviceInfo, FixedEnv, Kind, RecordKey, Stamp, Version, create_vault, envelope,
    layout, open_setup_code, seal_setup_code, snapshot, unlock_vault, wire,
};

const PASSWORD: &str = "correct horse battery staple";
const DEVICE: &str = "k3jq8x0m2pa7d4vn";
/// What the setup code of `testdata/v1` holds, as the apps write it: a
/// place nothing answers at, so a test that joins with it stays off the
/// network.
const SETUP_CODE: &str = concat!(
    r#"{"location":{"kind":"webdav","url":"http://127.0.0.1:1/shellrs","username":"me"},"#,
    r#""login":"test login"}"#
);

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/v1")
}

fn records() -> RecordSet {
    let device = DeviceId::parse(DEVICE).unwrap();
    let mut set = RecordSet::new();
    for (i, (key, data)) in wire::tests::samples().into_iter().enumerate() {
        let record = RawRecord {
            version: Version {
                stamp: Stamp {
                    ms: 1_800_000_000_000 + i as u64,
                    counter: 0,
                },
                device: device.clone(),
            },
            body: Body::Live(wire::write_into(&data, None)),
            min_reader: None,
            rest: Default::default(),
            key,
        };
        set.insert(record.key.clone(), record);
    }
    // A secret of a kind only a later version knows.
    let Value::Object(data) = json!({"value": "test future secret"}) else {
        unreachable!()
    };
    let future = RawRecord {
        key: RecordKey::new(Kind::Secret("secret.future".into()), "x1"),
        version: Version {
            stamp: Stamp {
                ms: 1_800_000_000_050,
                counter: 0,
            },
            device: device.clone(),
        },
        body: Body::Live(data),
        min_reader: None,
        rest: Default::default(),
    };
    set.insert(future.key.clone(), future);
    for (kind, id) in [(Kind::Host, "gone"), (Kind::Other("tag".into()), "t1")] {
        let record = RawRecord {
            key: RecordKey::new(kind, id),
            version: Version {
                stamp: Stamp {
                    ms: 1_800_000_000_100,
                    counter: 2,
                },
                device: device.clone(),
            },
            body: Body::Deleted,
            min_reader: None,
            rest: Default::default(),
        };
        set.insert(record.key.clone(), record);
    }
    set
}

/// Write the files again: only when the format itself changes, which it
/// must not once released.
#[test]
#[ignore]
fn write_golden_files() {
    let store = MemoryStore::new();
    let mut env = FixedEnv::new(0, 2026);
    let keys = create_vault(&store, PASSWORD, FAST, &mut env).unwrap();
    let device = DeviceId::parse(DEVICE).unwrap();
    let info = DeviceInfo {
        name: "MacBook Pro".into(),
        platform: "macos".into(),
        app_version: "0.2.0".into(),
    };
    let plain = snapshot::encode(
        &device,
        &info,
        Stamp {
            ms: 1_800_000_000_200,
            counter: 0,
        },
        &records(),
    );
    let sealed = envelope::seal(&plain, &device, &keys, &mut env).unwrap();
    std::fs::create_dir_all(dir()).unwrap();
    std::fs::write(dir().join("vault.json"), store.raw(layout::VAULT).unwrap()).unwrap();
    std::fs::write(dir().join(format!("{DEVICE}.srs")), sealed).unwrap();
    let expected: Vec<Value> = records().values().map(RawRecord::to_json).collect();
    std::fs::write(
        dir().join("expected.json"),
        serde_json::to_vec_pretty(&expected).unwrap(),
    )
    .unwrap();
}

#[test]
fn format_1_files_still_read_the_same() {
    let store = MemoryStore::new();
    store.set_raw(
        layout::VAULT,
        std::fs::read(dir().join("vault.json")).unwrap(),
    );
    let keys = unlock_vault(&store, PASSWORD).unwrap();
    let device = DeviceId::parse(DEVICE).unwrap();
    let sealed = std::fs::read(dir().join(format!("{DEVICE}.srs"))).unwrap();
    let plain = envelope::open(&sealed, &device, &keys).unwrap();
    let read = snapshot::decode(&plain, &device).unwrap();
    assert_eq!(read.info.name, "MacBook Pro");
    let expected: Vec<Value> =
        serde_json::from_slice(&std::fs::read(dir().join("expected.json")).unwrap()).unwrap();
    let got: Vec<Value> = read.records.values().map(RawRecord::to_json).collect();
    assert_eq!(got, expected);
    // And what they hold still means the same to the app.
    for (key, data) in wire::tests::samples() {
        let fields = read.records[&key].live().unwrap();
        assert_eq!(wire::parse(&key, fields), Ok(data), "{key}");
    }
}

/// Write the setup code of the vault in `testdata/v1`, and nothing else:
/// the vault and the device file there are released and stay as they are.
#[test]
#[ignore]
fn write_golden_setup_code() {
    let store = MemoryStore::new();
    store.set_raw(
        layout::VAULT,
        std::fs::read(dir().join("vault.json")).unwrap(),
    );
    let keys = unlock_vault(&store, PASSWORD).unwrap();
    let code = seal_setup_code(
        &store,
        &keys,
        SETUP_CODE.as_bytes(),
        &mut FixedEnv::new(0, 95),
    )
    .unwrap();
    std::fs::write(dir().join("setup-code.txt"), format!("{code}\n")).unwrap();
}

#[test]
fn a_version_1_setup_code_still_opens() {
    let code = std::fs::read_to_string(dir().join("setup-code.txt")).unwrap();
    let plain = open_setup_code(&code, PASSWORD).unwrap();
    assert_eq!(std::str::from_utf8(&plain), Ok(SETUP_CODE));
}
