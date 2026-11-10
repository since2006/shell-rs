//! Repairs after a merge. Two devices editing at once can leave a record
//! pointing at one the other deleted: a host in a deleted group, a group
//! moved under its own child. Each repair is an ordinary edit, so it syncs
//! like one; devices making the same repair make the same record, and once
//! repaired nothing points at a deletion, so repairs do not bounce.
//!
//! Only deletions are repaired, never records that are merely missing: one
//! may be missing because a device's file could not be read this round,
//! and repairing then would lose the host's group for good.

use std::collections::{BTreeMap, BTreeSet};

use crate::clock::{Hlc, after};
use crate::merge::join;
use crate::record::{Body, RawRecord, RecordSet};
use crate::wire::{self, Auth, RecordData, Route, SnippetScope};
use crate::{DeviceId, FORMAT, Kind, RecordKey, Version};

/// Repair `set` until nothing points at a deletion, each repair `device`'s,
/// dated just after the version it repairs (`clock::after`): a repair
/// never outranks what a user did after seeing the record, such as
/// deleting it. Returns how many records it changed.
pub(crate) fn repair(set: &mut RecordSet, clock: &mut Hlc, device: &DeviceId) -> u32 {
    let mut count = 0;
    // A repair can only remove what another repair would act on, so a few
    // passes settle; the bound is a guard, not a limit reached.
    for _ in 0..8 {
        let fixes = repairs(set);
        if fixes.is_empty() {
            break;
        }
        for (key, body) in fixes {
            let base = &set[&key];
            let stamp = after(base.version.stamp);
            clock.observe(stamp);
            let record = RawRecord {
                version: Version {
                    stamp,
                    device: device.clone(),
                },
                body,
                min_reader: base.min_reader,
                rest: base.rest.clone(),
                key,
            };
            join(set, record);
            count += 1;
        }
    }
    count
}

/// The repairs `set` needs now, as each record's new body.
pub(crate) fn repairs(set: &RecordSet) -> Vec<(RecordKey, Body)> {
    let deleted = |kind: Kind, id: &str| {
        set.get(&RecordKey::new(kind, id))
            .is_some_and(RawRecord::is_deleted)
    };
    let detached_groups = closing_cycles(set, Kind::Group);
    let detached_categories = closing_cycles(set, Kind::SnippetCategory);
    let mut out = Vec::new();
    for record in set.values().filter(|record| editable(record)) {
        let Some(fields) = record.live() else {
            continue;
        };
        let Ok(mut data) = wire::parse(&record.key, fields) else {
            continue;
        };
        let mut delete = false;
        let mut changed = false;
        match &mut data {
            RecordData::Group(group) => {
                let gone = group.parent.as_deref().is_some_and(|parent| {
                    deleted(Kind::Group, parent) || detached_groups.contains(&record.key.id)
                });
                if gone {
                    group.parent = None;
                    changed = true;
                }
            }
            RecordData::Host(host) => {
                if host
                    .group
                    .as_deref()
                    .is_some_and(|group| deleted(Kind::Group, group))
                {
                    host.group = None;
                    changed = true;
                }
                if host
                    .credential
                    .as_deref()
                    .is_some_and(|credential| deleted(Kind::Credential, credential))
                {
                    // The same on every device: the credential's user name
                    // went with it, and the user fills in their own.
                    host.auth = Auth::Password;
                    host.credential = None;
                    host.credential_username = None;
                    host.username = Some(String::new());
                    changed = true;
                }
                if let Route::Jump(hops) = &mut host.route {
                    for hop in hops.iter_mut() {
                        if hop.as_deref().is_some_and(|id| deleted(Kind::Host, id)) {
                            *hop = None;
                            changed = true;
                        }
                    }
                }
            }
            RecordData::Bookmark(bookmark) => delete = deleted(Kind::Host, &bookmark.host),
            RecordData::Forward(forward) => delete = deleted(Kind::Host, &forward.host),
            RecordData::Snippet(snippet) => {
                delete = match &snippet.scope {
                    SnippetScope::All => false,
                    SnippetScope::Group(group) => deleted(Kind::Group, group),
                    SnippetScope::Host(host) => deleted(Kind::Host, host),
                };
                if !delete
                    && snippet
                        .category
                        .as_deref()
                        .is_some_and(|category| deleted(Kind::SnippetCategory, category))
                {
                    snippet.category = None;
                    changed = true;
                }
            }
            // A credential's own secrets go with it.
            RecordData::CredentialPassword(_)
            | RecordData::Passphrase(_)
            | RecordData::PrivateKey(_) => delete = deleted(Kind::Credential, &record.key.id),
            RecordData::SnippetCategory(category) => {
                let gone = category.parent.as_deref().is_some_and(|parent| {
                    deleted(Kind::SnippetCategory, parent)
                        || detached_categories.contains(&record.key.id)
                });
                if gone {
                    category.parent = None;
                    changed = true;
                }
            }
            RecordData::Credential(_) | RecordData::Password(_) | RecordData::ProxyPassword(_) => {}
        }
        if delete {
            out.push((record.key.clone(), Body::Deleted));
        } else if changed {
            let body = Body::Live(wire::write_into(&data, Some(fields)));
            out.push((record.key.clone(), body));
        }
    }
    out
}

/// Whether this version may rewrite `record`: not one a newer format says
/// it cannot read.
fn editable(record: &RawRecord) -> bool {
    record
        .min_reader
        .is_none_or(|min_reader| min_reader <= FORMAT)
}

/// Records of `kind` (groups, snippet categories) that close a cycle of
/// parents, one per cycle: the one whose version is least, so every device
/// picks the same.
fn closing_cycles(set: &RecordSet, kind: Kind) -> BTreeSet<String> {
    let mut parent = BTreeMap::new();
    let mut versions = BTreeMap::new();
    for record in set.values().filter(|record| editable(record)) {
        let Some(fields) = record.live().filter(|_| record.key.kind == kind) else {
            continue;
        };
        let up = match wire::parse(&record.key, fields) {
            Ok(RecordData::Group(group)) => group.parent,
            Ok(RecordData::SnippetCategory(category)) => category.parent,
            _ => continue,
        };
        versions.insert(record.key.id.clone(), &record.version);
        if let Some(up) = up {
            parent.insert(record.key.id.clone(), up);
        }
    }
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut out = BTreeSet::new();
    for start in versions.keys() {
        let mut path: Vec<String> = Vec::new();
        let mut at = start.clone();
        loop {
            if done.contains(&at) || !versions.contains_key(&at) {
                break;
            }
            if let Some(i) = path.iter().position(|seen| *seen == at) {
                let cycle = &path[i..];
                let least = cycle
                    .iter()
                    .min_by_key(|id| versions[id.as_str()])
                    .expect("a cycle has members");
                out.insert(least.clone());
                break;
            }
            path.push(at.clone());
            match parent.get(&at) {
                Some(up) => at = up.clone(),
                None => break,
            }
        }
        done.extend(path);
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::record::tests::{deleted, device, live, version};
    use crate::secret::{EndpointSecret, Secret, endpoint_secret_id};
    use crate::wire::tests::ssh_host;
    use crate::wire::{ForwardData, ForwardKind, GroupData, SnippetCategoryData, SnippetData};
    use crate::{FixedEnv, Stamp};

    fn record(kind: Kind, id: &str, at: u64, data: RecordData) -> RawRecord {
        let fields = Value::Object(wire::write_into(&data, None));
        live(kind, id, version(at, 'a'), fields)
    }

    fn group(id: &str, parent: Option<&str>, at: u64) -> RawRecord {
        let data = RecordData::Group(GroupData {
            name: id.into(),
            parent: parent.map(Into::into),
            sort_order: 0,
        });
        record(Kind::Group, id, at, data)
    }

    fn category(id: &str, parent: Option<&str>, at: u64) -> RawRecord {
        let data = RecordData::SnippetCategory(SnippetCategoryData {
            name: id.into(),
            parent: parent.map(Into::into),
            sort_order: 0,
        });
        record(Kind::SnippetCategory, id, at, data)
    }

    fn set(records: impl IntoIterator<Item = RawRecord>) -> RecordSet {
        records.into_iter().map(|r| (r.key.clone(), r)).collect()
    }

    fn parsed(set: &RecordSet, kind: Kind, id: &str) -> Option<RecordData> {
        let record = &set[&RecordKey::new(kind, id)];
        record
            .live()
            .map(|fields| wire::parse(&record.key, fields).unwrap())
    }

    fn repaired(mut records: RecordSet) -> (RecordSet, u32) {
        let mut clock = Hlc::new(Stamp {
            ms: 100,
            counter: 0,
        });
        let count = repair(&mut records, &mut clock, &device('z'));
        (records, count)
    }

    #[test]
    fn a_host_in_a_deleted_group_moves_to_the_top() {
        let mut host = ssh_host("web");
        host.group = Some("g".into());
        let records = set([
            deleted(Kind::Group, "g", version(5, 'b')),
            record(Kind::Host, "h", 1, RecordData::Host(host)),
        ]);
        let (records, count) = repaired(records);
        assert_eq!(count, 1);
        let Some(RecordData::Host(host)) = parsed(&records, Kind::Host, "h") else {
            panic!()
        };
        assert_eq!(host.group, None);
        assert_eq!(
            records[&RecordKey::new(Kind::Host, "h")].version.device,
            device('z')
        );
    }

    #[test]
    fn a_missing_group_is_not_a_deleted_one() {
        let mut host = ssh_host("web");
        host.group = Some("g".into());
        let records = set([record(Kind::Host, "h", 1, RecordData::Host(host))]);
        assert_eq!(repaired(records).1, 0);
    }

    #[test]
    fn a_deleted_credential_leaves_sign_in_by_password() {
        let mut host = ssh_host("web");
        host.auth = Auth::Credential;
        host.credential = Some("c".into());
        host.username = None;
        let records = set([
            deleted(Kind::Credential, "c", version(5, 'b')),
            record(Kind::Host, "h", 1, RecordData::Host(host)),
        ]);
        let (records, _) = repaired(records);
        let Some(RecordData::Host(host)) = parsed(&records, Kind::Host, "h") else {
            panic!()
        };
        assert_eq!(host.auth, Auth::Password);
        assert_eq!(host.credential, None);
        assert_eq!(host.username.as_deref(), Some(""));
    }

    #[test]
    fn a_deleted_credential_takes_its_secrets() {
        let secret = |kind| record(kind, "c", 1, RecordData::Passphrase(Secret::new("x")));
        let endpoint = RecordData::Password(EndpointSecret {
            user: "root".into(),
            host: "10.0.0.1".into(),
            port: 22,
            value: Secret::new("x"),
        });
        let endpoint_id = endpoint_secret_id("root", "10.0.0.1", 22);
        let records = set([
            deleted(Kind::Credential, "c", version(5, 'b')),
            secret(Kind::SecretCredential),
            secret(Kind::SecretPassphrase),
            secret(Kind::SecretPrivateKey),
            record(Kind::SecretPassword, &endpoint_id, 1, endpoint),
        ]);
        let (records, count) = repaired(records);
        assert_eq!(count, 3);
        assert!(!records[&RecordKey::new(Kind::SecretPassword, endpoint_id)].is_deleted());
        // Kept while the credential is only missing.
        let only_missing = set([secret(Kind::SecretPassphrase)]);
        assert_eq!(repaired(only_missing).1, 0);
    }

    #[test]
    fn a_deleted_jump_host_leaves_its_hop_empty() {
        let mut host = ssh_host("inner");
        host.route = Route::Jump(vec![Some("j1".into()), Some("j2".into())]);
        let records = set([
            deleted(Kind::Host, "j1", version(5, 'b')),
            record(Kind::Host, "h", 1, RecordData::Host(host)),
        ]);
        let (records, _) = repaired(records);
        let Some(RecordData::Host(host)) = parsed(&records, Kind::Host, "h") else {
            panic!()
        };
        assert_eq!(host.route, Route::Jump(vec![None, Some("j2".into())]));
    }

    #[test]
    fn what_goes_with_a_deleted_host_goes() {
        let forward = RecordData::Forward(ForwardData {
            host: "h".into(),
            name: String::new(),
            kind: ForwardKind::Dynamic,
            bind_host: "127.0.0.1".into(),
            bind_port: 1080,
            target: None,
            sort_order: 0,
        });
        let snippet = RecordData::Snippet(SnippetData {
            category: None,
            name: "df".into(),
            command: "df".into(),
            scope: SnippetScope::Host("h".into()),
            run_on_click: false,
            sort_order: 0,
        });
        let bookmark = RecordData::Bookmark(wire::BookmarkData {
            host: "h".into(),
            path: "/".into(),
            sort_order: 0,
        });
        let records = set([
            deleted(Kind::Host, "h", version(5, 'b')),
            record(Kind::Forward, "f", 1, forward),
            record(Kind::Snippet, "s", 1, snippet),
            record(Kind::Bookmark, "b", 1, bookmark),
        ]);
        let (records, count) = repaired(records);
        assert_eq!(count, 3);
        assert!(records.values().all(RawRecord::is_deleted));
    }

    #[test]
    fn a_snippet_of_a_deleted_category_moves_to_the_top() {
        let snippet = RecordData::Snippet(SnippetData {
            category: Some("c".into()),
            name: "df".into(),
            command: "df".into(),
            scope: SnippetScope::All,
            run_on_click: false,
            sort_order: 0,
        });
        let records = set([
            deleted(Kind::SnippetCategory, "c", version(5, 'b')),
            record(Kind::Snippet, "s", 1, snippet),
        ]);
        let (records, _) = repaired(records);
        let Some(RecordData::Snippet(snippet)) = parsed(&records, Kind::Snippet, "s") else {
            panic!()
        };
        assert_eq!(snippet.category, None);
    }

    #[test]
    fn a_category_in_a_deleted_one_moves_to_the_top() {
        let records = set([
            deleted(Kind::SnippetCategory, "c", version(5, 'b')),
            category("logs", Some("c"), 1),
        ]);
        let (records, count) = repaired(records);
        assert_eq!(count, 1);
        let Some(RecordData::SnippetCategory(logs)) =
            parsed(&records, Kind::SnippetCategory, "logs")
        else {
            panic!()
        };
        assert_eq!(logs.parent, None);
        // Only missing, it stays where it was.
        assert_eq!(repaired(set([category("logs", Some("c"), 1)])).1, 0);
    }

    #[test]
    fn a_cycle_of_categories_is_broken_at_its_oldest() {
        // Each kind has its cycles of its own: a group named like a category
        // is not its parent.
        let records = set([
            category("a", Some("b"), 2),
            category("b", Some("a"), 1),
            group("a", Some("b"), 1),
            group("b", None, 1),
        ]);
        let (records, count) = repaired(records);
        assert_eq!(count, 1);
        assert!(closing_cycles(&records, Kind::SnippetCategory).is_empty());
        let Some(RecordData::SnippetCategory(b)) = parsed(&records, Kind::SnippetCategory, "b")
        else {
            panic!()
        };
        assert_eq!(b.parent, None);
        let Some(RecordData::Group(a)) = parsed(&records, Kind::Group, "a") else {
            panic!()
        };
        assert_eq!(a.parent.as_deref(), Some("b"));
    }

    #[test]
    fn a_cycle_of_groups_is_broken_at_its_oldest() {
        for groups in [
            vec![group("a", Some("a"), 1)],
            vec![group("a", Some("b"), 2), group("b", Some("a"), 1)],
            vec![
                group("a", Some("b"), 3),
                group("b", Some("c"), 1),
                group("c", Some("a"), 2),
                group("d", Some("a"), 0),
            ],
        ] {
            let (records, count) = repaired(set(groups));
            assert_eq!(count, 1);
            assert_cycle_free(&records);
        }
        let (records, _) = repaired(set([group("a", Some("b"), 2), group("b", Some("a"), 1)]));
        let Some(RecordData::Group(b)) = parsed(&records, Kind::Group, "b") else {
            panic!()
        };
        assert_eq!(b.parent, None);
    }

    #[test]
    fn repairing_twice_changes_nothing() {
        let mut host = ssh_host("web");
        host.group = Some("g".into());
        let records = set([
            deleted(Kind::Group, "g", version(5, 'b')),
            record(Kind::Host, "h", 1, RecordData::Host(host)),
            group("x", Some("y"), 1),
            group("y", Some("x"), 2),
        ]);
        let (records, _) = repaired(records);
        assert!(repairs(&records).is_empty());
        let (again, count) = repaired(records.clone());
        assert_eq!((again, count), (records, 0));
    }

    /// Two devices making the same repair make the same record, whatever
    /// their clocks: only the versions differ.
    #[test]
    fn devices_repair_alike() {
        let mut host = ssh_host("web");
        host.group = Some("g".into());
        let records = set([
            deleted(Kind::Group, "g", version(5, 'b')),
            record(Kind::Host, "h", 1, RecordData::Host(host)),
        ]);
        let mut one = records.clone();
        let mut two = records;
        repair(&mut one, &mut Hlc::default(), &device('x'));
        repair(&mut two, &mut Hlc::default(), &device('y'));
        let key = RecordKey::new(Kind::Host, "h");
        assert_eq!(one[&key].body, two[&key].body);
    }

    fn assert_cycle_free(records: &RecordSet) {
        assert!(closing_cycles(records, Kind::Group).is_empty());
    }

    /// Random groups, hosts and deletions: after repair, nothing live
    /// points at a deletion and no group is its own ancestor.
    #[test]
    fn repairs_leave_no_dangling_reference_and_no_cycle() {
        let mut env = FixedEnv::new(0, 5);
        let ids = ["a", "b", "c", "d", "e"];
        for _ in 0..300 {
            let mut records = RecordSet::new();
            for (i, id) in ids.iter().enumerate() {
                let roll = env.next_u64();
                let at = version(roll % 50, 'a');
                if roll.is_multiple_of(5) {
                    join(&mut records, deleted(Kind::Group, id, at.clone()));
                } else {
                    let parent =
                        (!(roll / 5).is_multiple_of(3)).then(|| ids[(roll / 15 % 5) as usize]);
                    let mut g = group(id, parent, 0);
                    g.version = at.clone();
                    join(&mut records, g);
                }
                let mut host = ssh_host(id);
                host.group = Some(ids[(roll / 75 % 5) as usize].into());
                host.route = Route::Jump(vec![Some(format!("h{}", (i + 1) % 5))]);
                let host_id = format!("h{i}");
                if (roll / 375).is_multiple_of(4) {
                    join(&mut records, deleted(Kind::Host, &host_id, at));
                } else {
                    join(
                        &mut records,
                        record(Kind::Host, &host_id, roll % 50, RecordData::Host(host)),
                    );
                }
            }
            let (records, _) = repaired(records);
            assert_cycle_free(&records);
            assert!(repairs(&records).is_empty());
            let gone = |kind: Kind, id: &str| {
                records
                    .get(&RecordKey::new(kind, id))
                    .is_some_and(RawRecord::is_deleted)
            };
            for record in records.values() {
                match record.live().map(|f| wire::parse(&record.key, f).unwrap()) {
                    Some(RecordData::Group(g)) => {
                        assert!(!g.parent.is_some_and(|p| gone(Kind::Group, &p)))
                    }
                    Some(RecordData::Host(h)) => {
                        assert!(!h.group.is_some_and(|g| gone(Kind::Group, &g)));
                        if let Route::Jump(hops) = h.route {
                            assert!(hops.iter().flatten().all(|hop| !gone(Kind::Host, hop)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
