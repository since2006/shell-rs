//! The SQLite file behind the host store.
//!
//! Everything here is synchronous and runs on the UI thread: the writes are
//! single rows triggered by a dialog the user just confirmed, not a stream.
//! The store keeps memory as the source of truth and mirrors each change
//! here, so a failed write costs the user persistence, never the edit.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use rusqlite::{Connection, params};

use super::{
    AuthKind, BookmarkSide, Credential, CredentialDraft, CredentialId, CredentialKind,
    ForwardDraft, ForwardEndpoint, ForwardId, ForwardKind, ForwardRule, GroupDraft, GroupId, Host,
    HostDraft, HostGroup, HostId, HostOs, ProxyKind, ProxySettings, PublicId, Route, Snippet,
    SnippetCategory, SnippetCategoryId, SnippetDraft, SnippetId, SnippetScope,
};

/// SQLite's only integer type is `i64`, so the `u64` ids cross the boundary
/// as `i64`. They are counters that start at 1 and never come near the range
/// where the round trip would lose anything.
fn to_sql(id: u64) -> i64 {
    id as i64
}

fn from_sql(id: i64) -> u64 {
    id as u64
}

/// The schema's version, stored in `PRAGMA user_version`. A change to the
/// schema bumps it and gives `migrate` a step from the version before.
///
/// 13 is the schema as it was rebuilt before the first release. Versions up
/// to 12 were development builds, and their files are refused. 14 adds the
/// command snippets.
const SCHEMA_VERSION: i64 = 14;

/// The whole schema, as a new database gets it.
///
/// Every table is `STRICT`: a value of the wrong type is refused rather than
/// stored. The constraints say what the store already keeps true, so a bug
/// or a hand edit that breaks it fails loudly instead of loading as nonsense.
///
/// - **groups** nest through `parent_id`; deleting one cascades to its
///   subgroups and, through `hosts.group_id`, to their hosts.
/// - **credentials** come before the hosts that refer to them. Only a key
///   credential names a key file. Secrets are never here: a password
///   credential's password is in the keychain under `keychain_id`.
/// - **hosts** carry how they log in and how they are reached:
///   - `auth` is `password` or `no-password` for a login of their own, with
///     `username`, or `credential`, with `credential_id` and no user name of
///     their own (the credential's is used). Deleting a credential that is
///     still used is refused: the store first gives its hosts a login of
///     their own.
///   - `route` is `direct`, `jump` (the hops are in `host_jumps`) or
///     `proxy`, whose four columns are set exactly for a proxy route.
///   - `public_id` may be missing, which a row added by hand is until the
///     next open (see `fill_missing_public_ids`); the ones there are unique.
///   - `os` and `last_connected_at` are written by connections, not by the
///     host form.
/// - **host_jumps** lists a host's jump hosts in order. A deleted jump
///   host leaves its row behind with no `jump_id`, so the host fails to
///   connect rather than skip the hop.
/// - **bookmarks** are kept per host and per pane, and go with their
///   host, the way WinSCP drops a site's bookmarks with the site.
/// - **forwards** go with the host they go through. A dynamic forward has
///   no target; the other two kinds must have one.
/// - **snippet_categories** are one level of folders for **snippets**, and
///   take their snippets with them. A snippet with neither scope column is
///   for every host, which every snippet is for now; one kept to a group or
///   a host will go with it. `run_on_click` runs it on a click rather than
///   only typing it.
///
/// A column added later goes after the table's last column here and is
/// added to existing files with `ALTER TABLE ADD COLUMN`, which puts it in
/// the same place (`a_column_added_later_matches_the_schema`).
const SCHEMA: &str = "\
CREATE TABLE groups (
    id         INTEGER PRIMARY KEY,
    parent_id  INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (name <> ''),
    sort_order INTEGER NOT NULL DEFAULT 0,
    expanded   INTEGER NOT NULL DEFAULT 1 CHECK (expanded IN (0, 1))
) STRICT;
CREATE INDEX groups_parent_id ON groups(parent_id);
CREATE TABLE credentials (
    id          INTEGER PRIMARY KEY,
    keychain_id TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL CHECK (name <> ''),
    kind        TEXT NOT NULL CHECK (kind IN ('password', 'key', 'agent')),
    username    TEXT NOT NULL,
    key_path    TEXT,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    CHECK ((kind = 'key') = (key_path IS NOT NULL))
) STRICT;
CREATE TABLE hosts (
    id                INTEGER PRIMARY KEY,
    public_id         TEXT UNIQUE,
    group_id          INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    sort_order        INTEGER NOT NULL DEFAULT 0,
    name              TEXT NOT NULL CHECK (name <> ''),
    address           TEXT NOT NULL CHECK (address <> ''),
    port              INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    auth              TEXT NOT NULL CHECK (auth IN ('password', 'no-password', 'credential')),
    username          TEXT,
    credential_id     INTEGER REFERENCES credentials(id),
    route             TEXT NOT NULL DEFAULT 'direct' CHECK (route IN ('direct', 'jump', 'proxy')),
    proxy_kind        TEXT CHECK (proxy_kind IN ('http', 'socks5')),
    proxy_host        TEXT,
    proxy_port        INTEGER CHECK (proxy_port BETWEEN 1 AND 65535),
    proxy_username    TEXT,
    notes             TEXT NOT NULL DEFAULT '',
    os                TEXT,
    last_connected_at INTEGER,
    CHECK ((auth = 'credential') = (credential_id IS NOT NULL)),
    CHECK ((auth = 'credential') = (username IS NULL)),
    CHECK ((route = 'proxy') = (proxy_kind IS NOT NULL)),
    CHECK ((proxy_kind IS NULL) = (proxy_host IS NULL)
        AND (proxy_kind IS NULL) = (proxy_port IS NULL)),
    CHECK (proxy_kind IS NOT NULL OR proxy_username IS NULL)
) STRICT;
CREATE INDEX hosts_group_id ON hosts(group_id);
CREATE INDEX hosts_credential_id ON hosts(credential_id);
CREATE TABLE host_jumps (
    host_id  INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    jump_id  INTEGER REFERENCES hosts(id) ON DELETE SET NULL,
    PRIMARY KEY (host_id, position),
    CHECK (jump_id <> host_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX host_jumps_jump_id ON host_jumps(jump_id);
CREATE TABLE bookmarks (
    host_id    INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    side       TEXT NOT NULL CHECK (side IN ('local', 'remote')),
    path       TEXT NOT NULL,
    sort_order INTEGER NOT NULL,
    PRIMARY KEY (host_id, side, path)
) STRICT, WITHOUT ROWID;
CREATE TABLE forwards (
    id          INTEGER PRIMARY KEY,
    host_id     INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL DEFAULT '',
    kind        TEXT NOT NULL CHECK (kind IN ('local', 'remote', 'dynamic')),
    bind_host   TEXT NOT NULL,
    bind_port   INTEGER NOT NULL CHECK (bind_port BETWEEN 1 AND 65535),
    target_host TEXT,
    target_port INTEGER,
    auto_start  INTEGER NOT NULL DEFAULT 0 CHECK (auto_start IN (0, 1)),
    sort_order  INTEGER NOT NULL DEFAULT 0,
    CHECK ((kind = 'dynamic' AND target_host IS NULL AND target_port IS NULL)
        OR (kind <> 'dynamic' AND target_host IS NOT NULL
            AND target_port BETWEEN 1 AND 65535))
) STRICT;
CREATE INDEX forwards_host_id ON forwards(host_id);
CREATE TABLE snippet_categories (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL CHECK (name <> '')
) STRICT;
CREATE TABLE snippets (
    id             INTEGER PRIMARY KEY,
    category_id    INTEGER REFERENCES snippet_categories(id) ON DELETE CASCADE,
    name           TEXT NOT NULL CHECK (name <> ''),
    command        TEXT NOT NULL CHECK (command <> ''),
    scope_group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    scope_host_id  INTEGER REFERENCES hosts(id) ON DELETE CASCADE,
    run_on_click   INTEGER NOT NULL DEFAULT 0 CHECK (run_on_click IN (0, 1)),
    CHECK (scope_group_id IS NULL OR scope_host_id IS NULL)
) STRICT;
CREATE INDEX snippets_category_id ON snippets(category_id);
CREATE INDEX snippets_scope_group_id ON snippets(scope_group_id);
CREATE INDEX snippets_scope_host_id ON snippets(scope_host_id);";

/// One step from a version to the next: one transaction that ends by
/// recording the version it reached.
// No step needs `Code` yet.
#[allow(dead_code)]
enum Step {
    /// Statements that make their own transaction.
    Sql(&'static str),
    /// A change SQL alone does not express well.
    Code(fn(&Connection) -> rusqlite::Result<()>),
}

/// The steps from each older version to the next, in order. A file older
/// than the first of them, or than `SCHEMA_VERSION` while there are none,
/// cannot be opened.
///
/// A step is history: it says what that version's change was, and stays as
/// written when `SCHEMA` changes again.
const STEPS: [(i64, Step); 1] = [(
    13,
    // The command snippets.
    Step::Sql(
        "BEGIN;
CREATE TABLE snippet_categories (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL CHECK (name <> '')
) STRICT;
CREATE TABLE snippets (
    id             INTEGER PRIMARY KEY,
    category_id    INTEGER REFERENCES snippet_categories(id) ON DELETE CASCADE,
    name           TEXT NOT NULL CHECK (name <> ''),
    command        TEXT NOT NULL CHECK (command <> ''),
    scope_group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    scope_host_id  INTEGER REFERENCES hosts(id) ON DELETE CASCADE,
    run_on_click   INTEGER NOT NULL DEFAULT 0 CHECK (run_on_click IN (0, 1)),
    CHECK (scope_group_id IS NULL OR scope_host_id IS NULL)
) STRICT;
CREATE INDEX snippets_category_id ON snippets(category_id);
CREATE INDEX snippets_scope_group_id ON snippets(scope_group_id);
CREATE INDEX snippets_scope_host_id ON snippets(scope_host_id);
PRAGMA user_version = 14;
COMMIT;",
    ),
)];

/// Everything one launch reads back from disk.
pub struct StoredData {
    /// Groups in id order; `parent` and `sort_order` give the visible tree.
    pub groups: Vec<HostGroup>,
    /// Hosts in id order, all of them disconnected. A host using a
    /// credential has no user name of its own here; the store copies the
    /// credential's in.
    pub hosts: Vec<Host>,
    /// Hosts that have ever connected, most recently connected first.
    pub recent: Vec<HostId>,
    /// SFTP bookmarks in the order they were added.
    pub bookmarks: Vec<(HostId, BookmarkSide, String)>,
    /// Port-forwarding rules in the order the forward list shows them.
    pub forwards: Vec<ForwardRule>,
    /// Credentials in the order the credential list shows them.
    pub credentials: Vec<Credential>,
    /// Snippet categories in id order.
    pub snippet_categories: Vec<SnippetCategory>,
    /// Snippets in id order.
    pub snippets: Vec<Snippet>,
}

pub struct HostDatabase {
    connection: Connection,
}

impl HostDatabase {
    /// Open the database at `path`, creating it if it is not there yet.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::prepare(Connection::open(path)?)
    }

    /// An anonymous database that lives only as long as this value.
    #[cfg(test)]
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut connection: Connection) -> rusqlite::Result<Self> {
        // Cascading deletes are how removing a group removes its subtree, and
        // SQLite only honours the foreign keys when asked, per connection.
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        migrate(&connection)?;
        fill_missing_public_ids(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn load(&self) -> rusqlite::Result<StoredData> {
        let groups = self
            .connection
            .prepare("SELECT id, name, parent_id, sort_order, expanded FROM groups ORDER BY id")?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let parent: Option<i64> = row.get(2)?;
                let mut group = HostGroup::new(
                    GroupId(from_sql(id)),
                    GroupDraft::new(name, parent.map(|id| GroupId(from_sql(id)))),
                );
                group.sort_order = row.get(3)?;
                group.expanded = row.get(4)?;
                Ok(group)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut hosts = self
            .connection
            .prepare(
                "SELECT id, public_id, group_id, sort_order, name, address, port, auth, username, \
                 credential_id, route, proxy_kind, proxy_host, proxy_port, proxy_username, \
                 notes, os FROM hosts ORDER BY id",
            )?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let public_id: Option<String> = row.get(1)?;
                let group: Option<i64> = row.get(2)?;
                let name: String = row.get(4)?;
                let address: String = row.get(5)?;
                let port: u16 = row.get(6)?;
                let auth: String = row.get(7)?;
                let user: Option<String> = row.get(8)?;
                let credential: Option<i64> = row.get(9)?;
                let route: String = row.get(10)?;
                let proxy_kind: Option<String> = row.get(11)?;
                let proxy_host: Option<String> = row.get(12)?;
                let proxy_port: Option<u16> = row.get(13)?;
                let proxy_user: Option<String> = row.get(14)?;
                let notes: String = row.get(15)?;
                let os: Option<String> = row.get(16)?;
                let mut host = Host::new(
                    HostId(from_sql(id)),
                    HostDraft::new(
                        name,
                        address,
                        port,
                        user.unwrap_or_default(),
                        AuthKind::from_stored(&auth),
                        group.map(|id| GroupId(from_sql(id))),
                    )
                    .with_notes(notes),
                );
                host.public_id = PublicId::from_stored(public_id.unwrap_or_default());
                host.sort_order = row.get(3)?;
                host.credential = credential.map(|id| CredentialId(from_sql(id)));
                // The table's CHECKs keep the proxy's columns whole.
                host.route = match (route.as_str(), proxy_kind, proxy_host, proxy_port) {
                    ("proxy", Some(kind), Some(proxy_host), Some(port)) => {
                        ProxyKind::from_stored(&kind)
                            .map(|kind| {
                                Route::Proxy(
                                    ProxySettings::new(kind, proxy_host, port)
                                        .with_user(proxy_user.unwrap_or_default()),
                                )
                            })
                            .unwrap_or_default()
                    }
                    ("jump", ..) => Route::Jump(Vec::new()),
                    _ => Route::Direct,
                };
                host.os = os.as_deref().and_then(HostOs::from_stored);
                Ok(host)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        self.load_jumps(&mut hosts)?;

        let recent = self
            .connection
            .prepare(
                "SELECT id FROM hosts WHERE last_connected_at IS NOT NULL \
                 ORDER BY last_connected_at DESC, id DESC",
            )?
            .query_map([], |row| {
                row.get::<_, i64>(0).map(|id| HostId(from_sql(id)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let bookmarks = self
            .connection
            .prepare(
                "SELECT host_id, side, path FROM bookmarks \
                 ORDER BY sort_order, host_id, side, path",
            )?
            .query_map([], |row| {
                let host: i64 = row.get(0)?;
                let side: String = row.get(1)?;
                Ok((
                    HostId(from_sql(host)),
                    BookmarkSide::from_stored(&side),
                    row.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|row| match row {
                Ok((host, Some(side), path)) => Some(Ok((host, side, path))),
                Ok((_, None, _)) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let forwards = self
            .connection
            .prepare(
                "SELECT id, host_id, name, kind, bind_host, bind_port, target_host, \
                 target_port, auto_start, sort_order FROM forwards ORDER BY sort_order, id",
            )?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let host: i64 = row.get(1)?;
                let name: String = row.get(2)?;
                let kind: String = row.get(3)?;
                let bind_host: String = row.get(4)?;
                let bind_port: u16 = row.get(5)?;
                let target_host: Option<String> = row.get(6)?;
                let target_port: Option<u16> = row.get(7)?;
                let auto_start: bool = row.get(8)?;
                let sort_order: i64 = row.get(9)?;
                // The table's CHECK admits only the kinds this build knows.
                Ok(ForwardKind::from_stored(&kind).map(|kind| {
                    let target = target_host
                        .zip(target_port)
                        .map(|(host, port)| ForwardEndpoint::new(host, port));
                    let draft = ForwardDraft::new(
                        kind,
                        HostId(from_sql(host)),
                        ForwardEndpoint::new(bind_host, bind_port),
                        target,
                    )
                    .with_name(name)
                    .with_auto_start(auto_start);
                    let mut rule = ForwardRule::new(ForwardId(from_sql(id)), draft);
                    rule.sort_order = sort_order;
                    rule
                }))
            })?
            .filter_map(Result::transpose)
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let credentials = self
            .connection
            .prepare(
                "SELECT id, keychain_id, name, kind, username, key_path, sort_order \
                 FROM credentials ORDER BY sort_order, id",
            )?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let keychain_id: String = row.get(1)?;
                let name: String = row.get(2)?;
                let kind: String = row.get(3)?;
                let user: String = row.get(4)?;
                let key_path: Option<String> = row.get(5)?;
                let sort_order: i64 = row.get(6)?;
                // The table's CHECK admits only the kinds this build knows.
                Ok(CredentialKind::from_stored(&kind).map(|kind| {
                    let mut draft = CredentialDraft::new(name, kind, user);
                    draft.key_path = key_path.map(Into::into);
                    let mut credential = Credential::new(CredentialId(from_sql(id)), draft);
                    credential.keychain_id = PublicId::from_stored(keychain_id);
                    credential.sort_order = sort_order;
                    credential
                }))
            })?
            .filter_map(Result::transpose)
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let snippet_categories = self
            .connection
            .prepare("SELECT id, name FROM snippet_categories ORDER BY id")?
            .query_map([], |row| {
                Ok(SnippetCategory {
                    id: SnippetCategoryId(from_sql(row.get(0)?)),
                    name: row.get::<_, String>(1)?.into(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let snippets = self
            .connection
            .prepare(
                "SELECT id, category_id, name, command, scope_group_id, scope_host_id, \
                 run_on_click FROM snippets ORDER BY id",
            )?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let category: Option<i64> = row.get(1)?;
                let name: String = row.get(2)?;
                let command: String = row.get(3)?;
                let group: Option<i64> = row.get(4)?;
                let host: Option<i64> = row.get(5)?;
                let run_on_click: bool = row.get(6)?;
                let mut draft = SnippetDraft::new(
                    name,
                    command,
                    category.map(|id| SnippetCategoryId(from_sql(id))),
                )
                .with_run_on_click(run_on_click);
                // The table's CHECK keeps at most one of the two.
                draft.scope = match (group, host) {
                    (Some(group), _) => SnippetScope::Group(GroupId(from_sql(group))),
                    (None, Some(host)) => SnippetScope::Host(HostId(from_sql(host))),
                    (None, None) => SnippetScope::All,
                };
                Ok(Snippet::new(SnippetId(from_sql(id)), draft))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(StoredData {
            groups,
            hosts,
            recent,
            bookmarks,
            forwards,
            credentials,
            snippet_categories,
            snippets,
        })
    }

    /// Give each jump route among `hosts` its hops, in order.
    fn load_jumps(&self, hosts: &mut [Host]) -> rusqlite::Result<()> {
        let index: HashMap<HostId, usize> = hosts
            .iter()
            .enumerate()
            .map(|(ix, host)| (host.id, ix))
            .collect();
        let hops = self
            .connection
            .prepare("SELECT host_id, jump_id FROM host_jumps ORDER BY host_id, position")?
            .query_map([], |row| {
                let host: i64 = row.get(0)?;
                let jump: Option<i64> = row.get(1)?;
                Ok((HostId(from_sql(host)), jump.map(|id| HostId(from_sql(id)))))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (host, hop) in hops {
            if let Some(Route::Jump(hops)) = index.get(&host).map(|ix| &mut hosts[*ix].route) {
                hops.push(hop);
            }
        }
        Ok(())
    }

    pub fn insert_group(&self, group: &HostGroup) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO groups (id, name, parent_id, sort_order, expanded) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.parent.map(|parent| to_sql(parent.0)),
                group.sort_order,
                group.expanded,
            ],
        )?;
        Ok(())
    }

    pub fn update_group(&self, group: &HostGroup) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE groups SET name = ?2, parent_id = ?3, sort_order = ?4, expanded = ?5 WHERE id = ?1",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.parent.map(|parent| to_sql(parent.0)),
                group.sort_order,
                group.expanded,
            ],
        )?;
        Ok(())
    }

    /// Save the host tree's expansion choice for one group.
    pub fn set_group_expanded(&self, id: GroupId, expanded: bool) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE groups SET expanded = ?2 WHERE id = ?1",
            params![to_sql(id.0), expanded],
        )?;
        Ok(())
    }

    /// Save an expand-all or collapse-all command in one database write.
    pub fn set_all_groups_expanded(&self, expanded: bool) -> rusqlite::Result<()> {
        self.connection
            .execute("UPDATE groups SET expanded = ?1", params![expanded])?;
        Ok(())
    }

    /// Deleting a group takes its subgroups and their hosts with it, via
    /// the `ON DELETE CASCADE` on both foreign keys.
    pub fn remove_group(&self, id: GroupId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM groups WHERE id = ?1", params![to_sql(id.0)])?;
        Ok(())
    }

    /// Insert a host together with its jump hosts.
    pub fn insert_host(&self, host: &Host) -> rusqlite::Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        let row = HostRow::of(host);
        transaction.execute(
            "INSERT INTO hosts (id, public_id, group_id, sort_order, name, address, port, auth, \
             username, credential_id, route, proxy_kind, proxy_host, proxy_port, \
             proxy_username, notes, os) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                to_sql(host.id.0),
                host.public_id.as_str(),
                host.group.map(|group| to_sql(group.0)),
                host.sort_order,
                host.name.as_ref(),
                host.address.as_ref(),
                host.port,
                row.auth,
                row.username,
                row.credential,
                row.route,
                row.proxy_kind,
                row.proxy_host,
                row.proxy_port,
                row.proxy_username,
                host.notes.as_ref(),
                host.os.map(HostOs::as_str),
            ],
        )?;
        write_jumps(&transaction, host)?;
        transaction.commit()
    }

    /// Rewrite the fields the host form edits, jump hosts included.
    /// `last_connected_at` and `os` are left alone: they are written by
    /// `touch_connected` and `set_host_os`, and neither is part of the
    /// host form. `public_id` never changes.
    pub fn update_host(&self, host: &Host) -> rusqlite::Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        let row = HostRow::of(host);
        transaction.execute(
            "UPDATE hosts SET group_id = ?2, sort_order = ?3, name = ?4, address = ?5, \
             port = ?6, auth = ?7, username = ?8, credential_id = ?9, route = ?10, \
             proxy_kind = ?11, proxy_host = ?12, proxy_port = ?13, proxy_username = ?14, \
             notes = ?15 WHERE id = ?1",
            params![
                to_sql(host.id.0),
                host.group.map(|group| to_sql(group.0)),
                host.sort_order,
                host.name.as_ref(),
                host.address.as_ref(),
                host.port,
                row.auth,
                row.username,
                row.credential,
                row.route,
                row.proxy_kind,
                row.proxy_host,
                row.proxy_port,
                row.proxy_username,
                host.notes.as_ref(),
            ],
        )?;
        write_jumps(&transaction, host)?;
        transaction.commit()
    }

    /// Deleting a host takes its jump hosts, bookmarks and forwards with
    /// it; the hosts that went through it keep its place, empty
    /// (`ON DELETE SET NULL`).
    pub fn remove_host(&self, id: HostId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM hosts WHERE id = ?1", params![to_sql(id.0)])?;
        Ok(())
    }

    /// Save one drag as a unit, including parent changes and sibling order.
    pub fn save_tree_order(&self, groups: &[HostGroup], hosts: &[Host]) -> rusqlite::Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        {
            let mut update_group = transaction
                .prepare("UPDATE groups SET parent_id = ?2, sort_order = ?3 WHERE id = ?1")?;
            for group in groups {
                update_group.execute(params![
                    to_sql(group.id.0),
                    group.parent.map(|id| to_sql(id.0)),
                    group.sort_order,
                ])?;
            }
            let mut update_host = transaction
                .prepare("UPDATE hosts SET group_id = ?2, sort_order = ?3 WHERE id = ?1")?;
            for host in hosts {
                update_host.execute(params![
                    to_sql(host.id.0),
                    host.group.map(|id| to_sql(id.0)),
                    host.sort_order,
                ])?;
            }
        }
        transaction.commit()
    }

    /// Append a bookmark; adding one that exists is a no-op.
    pub fn insert_bookmark(
        &self,
        host: HostId,
        side: BookmarkSide,
        path: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO bookmarks (host_id, side, path, sort_order) \
             VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM bookmarks))",
            params![to_sql(host.0), side.as_str(), path],
        )?;
        Ok(())
    }

    pub fn remove_bookmark(
        &self,
        host: HostId,
        side: BookmarkSide,
        path: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "DELETE FROM bookmarks WHERE host_id = ?1 AND side = ?2 AND path = ?3",
            params![to_sql(host.0), side.as_str(), path],
        )?;
        Ok(())
    }

    /// Put one pane's bookmarks in the order of `paths`. Loading groups
    /// bookmarks by pane, so only the order within this pane matters.
    pub fn set_bookmark_order(
        &self,
        host: HostId,
        side: BookmarkSide,
        paths: &[String],
    ) -> rusqlite::Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        {
            let mut update = transaction.prepare(
                "UPDATE bookmarks SET sort_order = ?4 \
                 WHERE host_id = ?1 AND side = ?2 AND path = ?3",
            )?;
            for (order, path) in (0_i64..).zip(paths) {
                update.execute(params![to_sql(host.0), side.as_str(), path, order])?;
            }
        }
        transaction.commit()
    }

    pub fn insert_forward(&self, rule: &ForwardRule) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO forwards (id, host_id, name, kind, bind_host, bind_port, \
             target_host, target_port, auto_start, sort_order) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                to_sql(rule.id.0),
                to_sql(rule.host.0),
                rule.name.as_ref(),
                rule.kind.as_str(),
                rule.bind.host.as_ref(),
                rule.bind.port,
                rule.target.as_ref().map(|target| target.host.as_ref()),
                rule.target.as_ref().map(|target| target.port),
                rule.auto_start,
                rule.sort_order,
            ],
        )?;
        Ok(())
    }

    pub fn update_forward(&self, rule: &ForwardRule) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE forwards SET host_id = ?2, name = ?3, kind = ?4, bind_host = ?5, \
             bind_port = ?6, target_host = ?7, target_port = ?8, auto_start = ?9, \
             sort_order = ?10 WHERE id = ?1",
            params![
                to_sql(rule.id.0),
                to_sql(rule.host.0),
                rule.name.as_ref(),
                rule.kind.as_str(),
                rule.bind.host.as_ref(),
                rule.bind.port,
                rule.target.as_ref().map(|target| target.host.as_ref()),
                rule.target.as_ref().map(|target| target.port),
                rule.auto_start,
                rule.sort_order,
            ],
        )?;
        Ok(())
    }

    pub fn remove_forward(&self, id: ForwardId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM forwards WHERE id = ?1", params![to_sql(id.0)])?;
        Ok(())
    }

    pub fn insert_credential(&self, credential: &Credential) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO credentials (id, keychain_id, name, kind, username, key_path, sort_order) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                to_sql(credential.id.0),
                credential.keychain_id.as_str(),
                credential.name.as_ref(),
                credential.kind.as_str(),
                credential.user.as_ref(),
                credential.key_path.as_deref(),
                credential.sort_order,
            ],
        )?;
        Ok(())
    }

    /// Rewrite a credential. The hosts using it have no user name of
    /// their own here, so they follow without being touched.
    pub fn update_credential(&self, credential: &Credential) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE credentials SET name = ?2, kind = ?3, username = ?4, key_path = ?5, \
             sort_order = ?6 WHERE id = ?1",
            params![
                to_sql(credential.id.0),
                credential.name.as_ref(),
                credential.kind.as_str(),
                credential.user.as_ref(),
                credential.key_path.as_deref(),
                credential.sort_order,
            ],
        )?;
        Ok(())
    }

    /// Delete a credential. The hosts using it go back to logging in on
    /// their own with `auth`, as the credential's user, first: a credential
    /// still in use cannot be deleted.
    pub fn remove_credential(&self, id: CredentialId, auth: AuthKind) -> rusqlite::Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "UPDATE hosts SET auth = ?2, credential_id = NULL, \
             username = (SELECT username FROM credentials WHERE id = ?1) \
             WHERE credential_id = ?1",
            params![to_sql(id.0), auth.as_str()],
        )?;
        transaction.execute(
            "DELETE FROM credentials WHERE id = ?1",
            params![to_sql(id.0)],
        )?;
        transaction.commit()
    }

    pub fn insert_snippet_category(&self, category: &SnippetCategory) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO snippet_categories (id, name) VALUES (?1, ?2)",
            params![to_sql(category.id.0), category.name.as_ref()],
        )?;
        Ok(())
    }

    pub fn update_snippet_category(&self, category: &SnippetCategory) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE snippet_categories SET name = ?2 WHERE id = ?1",
            params![to_sql(category.id.0), category.name.as_ref()],
        )?;
        Ok(())
    }

    /// Delete a category with the snippets in it: the foreign key cascades.
    pub fn remove_snippet_category(&self, id: SnippetCategoryId) -> rusqlite::Result<()> {
        self.connection.execute(
            "DELETE FROM snippet_categories WHERE id = ?1",
            params![to_sql(id.0)],
        )?;
        Ok(())
    }

    pub fn insert_snippet(&self, snippet: &Snippet) -> rusqlite::Result<()> {
        let (group, host) = scope_columns(snippet.scope);
        self.connection.execute(
            "INSERT INTO snippets (id, category_id, name, command, scope_group_id, scope_host_id, \
             run_on_click) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                to_sql(snippet.id.0),
                snippet.category.map(|id| to_sql(id.0)),
                snippet.name.as_ref(),
                snippet.command,
                group,
                host,
                snippet.run_on_click,
            ],
        )?;
        Ok(())
    }

    pub fn update_snippet(&self, snippet: &Snippet) -> rusqlite::Result<()> {
        let (group, host) = scope_columns(snippet.scope);
        self.connection.execute(
            "UPDATE snippets SET category_id = ?2, name = ?3, command = ?4, \
             scope_group_id = ?5, scope_host_id = ?6, run_on_click = ?7 WHERE id = ?1",
            params![
                to_sql(snippet.id.0),
                snippet.category.map(|id| to_sql(id.0)),
                snippet.name.as_ref(),
                snippet.command,
                group,
                host,
                snippet.run_on_click,
            ],
        )?;
        Ok(())
    }

    pub fn remove_snippet(&self, id: SnippetId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM snippets WHERE id = ?1", params![to_sql(id.0)])?;
        Ok(())
    }

    /// Record the operating system a probe found on the host. `None` clears
    /// it, which is what a failed probe on a rebuilt host leaves behind.
    pub fn set_host_os(&self, id: HostId, os: Option<HostOs>) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE hosts SET os = ?2 WHERE id = ?1",
            params![to_sql(id.0), os.map(HostOs::as_str)],
        )?;
        Ok(())
    }

    /// Record that a host just connected, which is what orders the start
    /// page's recent list across launches.
    pub fn touch_connected(&self, id: HostId, at: u64) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE hosts SET last_connected_at = ?2 WHERE id = ?1",
            params![to_sql(id.0), to_sql(at)],
        )?;
        Ok(())
    }
}

/// The columns of a host's row that are spelled differently from the
/// model: its login and its route.
struct HostRow<'a> {
    auth: &'static str,
    username: Option<&'a str>,
    credential: Option<i64>,
    route: &'static str,
    proxy_kind: Option<&'static str>,
    proxy_host: Option<&'a str>,
    proxy_port: Option<u16>,
    proxy_username: Option<&'a str>,
}

impl<'a> HostRow<'a> {
    fn of(host: &'a Host) -> Self {
        let proxy = match &host.route {
            Route::Proxy(proxy) => Some(proxy),
            _ => None,
        };
        Self {
            auth: match host.credential {
                Some(_) => "credential",
                None => host.auth.as_str(),
            },
            // A host using a credential logs in as the credential's
            // user; the copy the store keeps in memory is not stored.
            username: match host.credential {
                Some(_) => None,
                None => Some(host.user.as_ref()),
            },
            credential: host.credential.map(|id| to_sql(id.0)),
            route: match host.route {
                Route::Direct => "direct",
                Route::Jump(_) => "jump",
                Route::Proxy(_) => "proxy",
            },
            proxy_kind: proxy.map(|proxy| proxy.kind.as_str()),
            proxy_host: proxy.map(|proxy| proxy.host.as_ref()),
            proxy_port: proxy.map(|proxy| proxy.port),
            proxy_username: proxy.and_then(|proxy| proxy.user.as_deref()),
        }
    }
}

/// Record `host`'s jump hosts, in place of whatever was recorded.
fn write_jumps(connection: &Connection, host: &Host) -> rusqlite::Result<()> {
    let id = to_sql(host.id.0);
    connection.execute("DELETE FROM host_jumps WHERE host_id = ?1", params![id])?;
    if let Route::Jump(hops) = &host.route {
        let mut insert = connection
            .prepare("INSERT INTO host_jumps (host_id, position, jump_id) VALUES (?1, ?2, ?3)")?;
        for (position, hop) in (0_i64..).zip(hops) {
            insert.execute(params![id, position, hop.map(|hop| to_sql(hop.0))])?;
        }
    }
    Ok(())
}

/// Bring the database to `SCHEMA_VERSION`. A new file has version 0 and gets
/// the whole schema; an older one is backed up, then taken forward one
/// version at a time. Each step is one transaction, so a file is never left
/// between versions.
///
/// A file from a newer ShellRS is refused and left as it is: this version
/// does not know what the newer tables mean, and writing to them could lose
/// what the newer version saved. That happens after going back to an older
/// version by hand, the one way an update can be undone. A file older than
/// any step reaches back to is refused the same way.
fn migrate(connection: &Connection) -> rusqlite::Result<()> {
    let mut version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 0 {
        return connection.execute_batch(&format!(
            "BEGIN;\n{SCHEMA}\nPRAGMA user_version = {SCHEMA_VERSION};\nCOMMIT;"
        ));
    }
    if version > SCHEMA_VERSION {
        return Err(refusal(format!(
            "数据库来自更新版本的 ShellRS（结构版本 {version}，本版本支持到 \
             {SCHEMA_VERSION}），请安装最新版本"
        )));
    }
    let oldest = STEPS.first().map_or(SCHEMA_VERSION, |(from, _)| *from);
    if version < oldest {
        return Err(refusal(format!(
            "数据库来自 ShellRS 的早期开发版本（结构版本 {version}），无法升级"
        )));
    }
    if version < SCHEMA_VERSION {
        back_up(connection, version);
    }
    for (from, step) in STEPS {
        if version == from {
            match step {
                Step::Sql(sql) => connection.execute_batch(sql)?,
                Step::Code(change) => change(connection)?,
            }
            version = from + 1;
        }
    }
    Ok(())
}

/// A snippet's scope as its two columns: the group's, the host's.
fn scope_columns(scope: SnippetScope) -> (Option<i64>, Option<i64>) {
    match scope {
        SnippetScope::All => (None, None),
        SnippetScope::Group(group) => (Some(to_sql(group.0)), None),
        SnippetScope::Host(host) => (None, Some(to_sql(host.0))),
    }
}

/// An error that refuses a file without touching it.
fn refusal(message: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(message),
    )
}

/// Copy the database to `<file>.v<version>.bak` before migrating it, so the
/// version that wrote it can still open its data. `VACUUM INTO` writes a
/// consistent copy; copying the file would miss what is still in the WAL.
/// A failed backup does not stop the migration: the data is still there,
/// only the way back is not.
fn back_up(connection: &Connection, version: i64) {
    let Some(path) = connection.path().filter(|path| !path.is_empty()) else {
        return;
    };
    let backup = format!("{path}.v{version}.bak");
    let _ = std::fs::remove_file(&backup);
    if let Err(error) = connection.execute("VACUUM INTO ?1", [&backup]) {
        eprintln!("shellrs: 迁移前无法备份数据库：{error}");
    }
}

/// Give every host without a [`PublicId`] one, which is any row added by
/// hand with `sqlite3`. Runs at every open; once every host has an id it
/// is one query that finds nothing.
fn fill_missing_public_ids(connection: &mut Connection) -> rusqlite::Result<()> {
    let missing = connection
        .prepare("SELECT id FROM hosts WHERE public_id IS NULL")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if missing.is_empty() {
        return Ok(());
    }
    let mut taken = connection
        .prepare("SELECT public_id FROM hosts WHERE public_id IS NOT NULL")?
        .query_map([], |row| row.get::<_, String>(0).map(PublicId::from_stored))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    let transaction = connection.transaction()?;
    {
        let mut update = transaction.prepare("UPDATE hosts SET public_id = ?2 WHERE id = ?1")?;
        for id in missing {
            let public_id = PublicId::generate_unused(|candidate| taken.contains(candidate));
            update.execute(params![id, public_id.as_str()])?;
            taken.insert(public_id);
        }
    }
    transaction.commit()
}

/// Seconds since the Unix epoch, for `touch_connected`.
pub fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: u64, name: &str, parent: Option<u64>) -> HostGroup {
        HostGroup::new(GroupId(id), GroupDraft::new(name, parent.map(GroupId)))
    }

    fn host(id: u64, name: &str, group: Option<u64>) -> Host {
        Host::new(
            HostId(id),
            HostDraft::new(
                name,
                "10.0.0.1",
                22,
                "root",
                AuthKind::Password,
                group.map(GroupId),
            ),
        )
    }

    #[test]
    fn a_fresh_database_is_empty_and_at_the_current_version() {
        let db = HostDatabase::in_memory().unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let data = db.load().unwrap();
        assert!(data.groups.is_empty());
        assert!(data.hosts.is_empty());
        assert!(data.recent.is_empty());
        assert!(data.forwards.is_empty());
    }

    #[test]
    fn a_database_from_a_newer_shellrs_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        drop(HostDatabase::open(&path).unwrap());
        let newer = SCHEMA_VERSION + 1;
        Connection::open(&path)
            .unwrap()
            .pragma_update(None, "user_version", newer)
            .unwrap();

        let Err(error) = HostDatabase::open(&path) else {
            panic!("a newer database opened");
        };
        assert!(error.to_string().contains("更新版本的 ShellRS"), "{error}");
        let version: i64 = Connection::open(&path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, newer);
        assert!(!dir.path().join(format!("shellrs.db.v{newer}.bak")).exists());
    }

    #[test]
    fn a_new_or_current_database_needs_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        drop(HostDatabase::open(&path).unwrap());
        drop(HostDatabase::open(&path).unwrap());
        let files: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".bak"))
            .collect();
        assert!(files.is_empty(), "{files:?}");
    }

    /// Every table and index as SQLite recorded its definition, with the
    /// layout of the text taken out: a column added with `ALTER TABLE ADD
    /// COLUMN` is spelled `x, y …` where the schema writes `x,\n    y …`.
    fn schema_of(connection: &Connection) -> Vec<(String, String)> {
        connection
            .prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, normalized_sql(&row.get::<_, String>(1)?)))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn normalized_sql(sql: &str) -> String {
        sql.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace(" ,", ",")
            .replace("( ", "(")
            .replace(" )", ")")
    }

    #[test]
    fn a_column_added_later_matches_the_schema() {
        // What a later version does to a file it finds: the new column goes
        // where the schema puts it, after the last column, before the
        // table's own constraints.
        let old = Connection::open_in_memory().unwrap();
        old.execute_batch(
            "CREATE TABLE t (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    CHECK (name <> '')
) STRICT;
ALTER TABLE t ADD COLUMN notes TEXT NOT NULL DEFAULT '';",
        )
        .unwrap();
        let new = Connection::open_in_memory().unwrap();
        new.execute_batch(
            "CREATE TABLE t (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL,
    notes TEXT NOT NULL DEFAULT '',
    CHECK (name <> '')
) STRICT;",
        )
        .unwrap();
        assert_eq!(schema_of(&old), schema_of(&new));
    }

    /// The schema of version 13, as files of that version have it.
    const SCHEMA_13: &str = "\
CREATE TABLE groups (
    id         INTEGER PRIMARY KEY,
    parent_id  INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (name <> ''),
    sort_order INTEGER NOT NULL DEFAULT 0,
    expanded   INTEGER NOT NULL DEFAULT 1 CHECK (expanded IN (0, 1))
) STRICT;
CREATE INDEX groups_parent_id ON groups(parent_id);
CREATE TABLE credentials (
    id          INTEGER PRIMARY KEY,
    keychain_id TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL CHECK (name <> ''),
    kind        TEXT NOT NULL CHECK (kind IN ('password', 'key', 'agent')),
    username    TEXT NOT NULL,
    key_path    TEXT,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    CHECK ((kind = 'key') = (key_path IS NOT NULL))
) STRICT;
CREATE TABLE hosts (
    id                INTEGER PRIMARY KEY,
    public_id         TEXT UNIQUE,
    group_id          INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    sort_order        INTEGER NOT NULL DEFAULT 0,
    name              TEXT NOT NULL CHECK (name <> ''),
    address           TEXT NOT NULL CHECK (address <> ''),
    port              INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    auth              TEXT NOT NULL CHECK (auth IN ('password', 'no-password', 'credential')),
    username          TEXT,
    credential_id     INTEGER REFERENCES credentials(id),
    route             TEXT NOT NULL DEFAULT 'direct' CHECK (route IN ('direct', 'jump', 'proxy')),
    proxy_kind        TEXT CHECK (proxy_kind IN ('http', 'socks5')),
    proxy_host        TEXT,
    proxy_port        INTEGER CHECK (proxy_port BETWEEN 1 AND 65535),
    proxy_username    TEXT,
    notes             TEXT NOT NULL DEFAULT '',
    os                TEXT,
    last_connected_at INTEGER,
    CHECK ((auth = 'credential') = (credential_id IS NOT NULL)),
    CHECK ((auth = 'credential') = (username IS NULL)),
    CHECK ((route = 'proxy') = (proxy_kind IS NOT NULL)),
    CHECK ((proxy_kind IS NULL) = (proxy_host IS NULL)
        AND (proxy_kind IS NULL) = (proxy_port IS NULL)),
    CHECK (proxy_kind IS NOT NULL OR proxy_username IS NULL)
) STRICT;
CREATE INDEX hosts_group_id ON hosts(group_id);
CREATE INDEX hosts_credential_id ON hosts(credential_id);
CREATE TABLE host_jumps (
    host_id  INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    jump_id  INTEGER REFERENCES hosts(id) ON DELETE SET NULL,
    PRIMARY KEY (host_id, position),
    CHECK (jump_id <> host_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX host_jumps_jump_id ON host_jumps(jump_id);
CREATE TABLE bookmarks (
    host_id    INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    side       TEXT NOT NULL CHECK (side IN ('local', 'remote')),
    path       TEXT NOT NULL,
    sort_order INTEGER NOT NULL,
    PRIMARY KEY (host_id, side, path)
) STRICT, WITHOUT ROWID;
CREATE TABLE forwards (
    id          INTEGER PRIMARY KEY,
    host_id     INTEGER NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL DEFAULT '',
    kind        TEXT NOT NULL CHECK (kind IN ('local', 'remote', 'dynamic')),
    bind_host   TEXT NOT NULL,
    bind_port   INTEGER NOT NULL CHECK (bind_port BETWEEN 1 AND 65535),
    target_host TEXT,
    target_port INTEGER,
    auto_start  INTEGER NOT NULL DEFAULT 0 CHECK (auto_start IN (0, 1)),
    sort_order  INTEGER NOT NULL DEFAULT 0,
    CHECK ((kind = 'dynamic' AND target_host IS NULL AND target_port IS NULL)
        OR (kind <> 'dynamic' AND target_host IS NOT NULL
            AND target_port BETWEEN 1 AND 65535))
) STRICT;
CREATE INDEX forwards_host_id ON forwards(host_id);";

    #[test]
    fn a_version_13_file_gets_the_snippets_and_keeps_its_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(&format!(
                "BEGIN;\n{SCHEMA_13}\nPRAGMA user_version = 13;\nCOMMIT;"
            ))
            .unwrap();
            old.execute(
                "INSERT INTO hosts (id, name, address, port, auth, username) \
                 VALUES (1, 'web', '10.0.0.1', 22, 'password', 'root')",
                [],
            )
            .unwrap();
        }

        let db = HostDatabase::open(&path).unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(dir.path().join("shellrs.db.v13.bak").exists());
        let fresh = HostDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));
        let data = db.load().unwrap();
        assert_eq!(data.hosts.len(), 1);
        assert!(data.snippets.is_empty());
    }

    fn snippet(id: u64, name: &str, category: Option<u64>, scope: SnippetScope) -> Snippet {
        let mut draft = SnippetDraft::new(
            name,
            format!("echo {name}"),
            category.map(SnippetCategoryId),
        );
        draft.scope = scope;
        Snippet::new(SnippetId(id), draft)
    }

    #[test]
    fn snippets_round_trip_and_go_with_their_category() {
        let db = HostDatabase::in_memory().unwrap();
        let docker = SnippetCategory {
            id: SnippetCategoryId(1),
            name: "Docker".into(),
        };
        db.insert_snippet_category(&docker).unwrap();
        let mut ps = snippet(1, "列出容器", Some(1), SnippetScope::All);
        ps.command = "docker ps -a\ndocker images".into();
        db.insert_snippet(&ps).unwrap();
        db.insert_snippet(&snippet(2, "磁盘", None, SnippetScope::All))
            .unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.snippet_categories, std::slice::from_ref(&docker));
        assert_eq!(data.snippets[0], ps);
        assert_eq!(data.snippets[1].category, None);

        db.update_snippet_category(&SnippetCategory {
            name: "容器".into(),
            ..docker
        })
        .unwrap();
        ps.name = "所有容器".into();
        ps.category = None;
        ps.run_on_click = true;
        db.update_snippet(&ps).unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.snippet_categories[0].name, "容器");
        assert_eq!(data.snippets[0], ps);

        db.insert_snippet(&snippet(3, "日志", Some(1), SnippetScope::All))
            .unwrap();
        db.remove_snippet_category(SnippetCategoryId(1)).unwrap();
        db.remove_snippet(SnippetId(2)).unwrap();
        let names: Vec<_> = db
            .load()
            .unwrap()
            .snippets
            .into_iter()
            .map(|snippet| snippet.name)
            .collect();
        assert_eq!(names, ["所有容器"]);
    }

    #[test]
    fn a_snippet_kept_to_a_group_or_a_host_goes_with_it() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_group(&group(2, "web", Some(1))).unwrap();
        db.insert_host(&host(1, "web-01", Some(2))).unwrap();
        db.insert_host(&host(2, "db-01", None)).unwrap();
        let scoped = [
            snippet(1, "全部", None, SnippetScope::All),
            snippet(2, "分组", None, SnippetScope::Group(GroupId(2))),
            snippet(3, "主机", None, SnippetScope::Host(HostId(2))),
            snippet(4, "分组里的主机", None, SnippetScope::Host(HostId(1))),
        ];
        for snippet in &scoped {
            db.insert_snippet(snippet).unwrap();
        }
        assert_eq!(db.load().unwrap().snippets, scoped);

        db.remove_host(HostId(2)).unwrap();
        // The group's subgroup and the host in it go, and so do theirs.
        db.remove_group(GroupId(1)).unwrap();
        let left: Vec<_> = db
            .load()
            .unwrap()
            .snippets
            .into_iter()
            .map(|snippet| snippet.id)
            .collect();
        assert_eq!(left, [SnippetId(1)]);
    }

    #[test]
    fn a_database_from_an_early_development_build_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE sessions (id INTEGER PRIMARY KEY); PRAGMA user_version = 12;",
            )
            .unwrap();
        let Err(error) = HostDatabase::open(&path) else {
            panic!("a version 12 database opened");
        };
        assert!(error.to_string().contains("早期开发版本"), "{error}");
        let version: i64 = Connection::open(&path)
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 12);
        assert!(!dir.path().join("shellrs.db.v12.bak").exists());
    }

    #[test]
    fn notes_round_trip_and_go_with_their_host() {
        let db = HostDatabase::in_memory().unwrap();
        let mut noted = host(1, "web", None);
        noted.notes = "机房 A，到期 2027-03\n负责人：张三".into();
        db.insert_host(&noted).unwrap();
        db.insert_host(&host(2, "db", None)).unwrap();
        let notes = |db: &HostDatabase| -> Vec<String> {
            db.load()
                .unwrap()
                .hosts
                .into_iter()
                .map(|host| host.notes.to_string())
                .collect()
        };
        assert_eq!(
            notes(&db),
            [
                "机房 A，到期 2027-03\n负责人：张三".to_string(),
                String::new()
            ]
        );

        db.update_host(&host(1, "web", None)).unwrap();
        assert_eq!(notes(&db), [String::new(), String::new()]);
    }

    fn routed(mut host: Host, route: Route) -> Host {
        host.route = route;
        host
    }

    #[test]
    fn routes_round_trip_and_a_deleted_jump_host_leaves_its_place() {
        let db = HostDatabase::in_memory().unwrap();
        let proxy =
            Route::Proxy(ProxySettings::new(ProxyKind::Socks5, "127.0.0.1", 7890).with_user("me"));
        let hosts = [
            host(1, "阿里云99", None),
            host(2, "禅道", None),
            routed(
                host(3, "db", None),
                Route::Jump(vec![Some(HostId(1)), Some(HostId(2))]),
            ),
            routed(host(4, "abroad", None), proxy.clone()),
            routed(
                host(5, "http", None),
                Route::Proxy(ProxySettings::new(ProxyKind::Http, "proxy.test", 8080)),
            ),
        ];
        for host in &hosts {
            db.insert_host(host).unwrap();
        }
        let routes = |db: &HostDatabase| -> Vec<Route> {
            db.load()
                .unwrap()
                .hosts
                .into_iter()
                .map(|host| host.route)
                .collect()
        };
        assert_eq!(
            routes(&db),
            hosts.iter().map(|s| s.route.clone()).collect::<Vec<_>>()
        );

        // Updating rewrites the route, and the order of the hops with it.
        db.update_host(&routed(
            host(3, "db", None),
            Route::Jump(vec![Some(HostId(2)), Some(HostId(1))]),
        ))
        .unwrap();
        db.update_host(&host(4, "abroad", None)).unwrap();
        assert_eq!(
            routes(&db)[2..4],
            [
                Route::Jump(vec![Some(HostId(2)), Some(HostId(1))]),
                Route::Direct
            ]
        );

        // The jump host goes; its place stays, empty. Its own route goes
        // with a host that is deleted.
        db.remove_host(HostId(1)).unwrap();
        db.remove_host(HostId(5)).unwrap();
        assert_eq!(
            routes(&db),
            [
                Route::Direct,
                Route::Jump(vec![Some(HostId(2)), None]),
                Route::Direct
            ]
        );
        // A route that is no longer a proxy keeps none of the proxy's columns.
        let proxies: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM hosts WHERE proxy_host IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(proxies, 0);
    }

    #[test]
    fn the_database_refuses_a_host_that_jumps_through_itself() {
        let db = HostDatabase::in_memory().unwrap();
        let looped = routed(host(1, "web", None), Route::Jump(vec![Some(HostId(1))]));
        assert!(db.insert_host(&looped).is_err());
        // The transaction took the host with it.
        assert!(db.load().unwrap().hosts.is_empty());
    }

    fn credential(id: u64, name: &str, kind: CredentialKind) -> Credential {
        let draft = CredentialDraft::new(name, kind, "deploy");
        let draft = match kind {
            CredentialKind::Key => draft.with_key_path("/tmp/id_deploy"),
            _ => draft,
        };
        Credential::new(CredentialId(id), draft)
    }

    fn using(mut host: Host, credential: &Credential) -> Host {
        host.credential = Some(credential.id);
        host.user = credential.user.clone();
        host.auth = AuthKind::Password;
        host
    }

    #[test]
    fn credentials_round_trip_and_hosts_keep_their_link() {
        let db = HostDatabase::in_memory().unwrap();
        let mut agent = credential(1, "agent", CredentialKind::Agent);
        agent.sort_order = 1;
        db.insert_credential(&agent).unwrap();
        let key = credential(2, "部署", CredentialKind::Key);
        db.insert_credential(&key).unwrap();
        let web = using(host(1, "web", None), &key);
        db.insert_host(&web).unwrap();
        db.insert_host(&host(2, "db", None)).unwrap();

        let data = db.load().unwrap();
        // `sort_order` first, then id.
        assert_eq!(data.credentials, [key.clone(), agent.clone()]);
        assert_eq!(data.hosts[0].credential, Some(key.id));
        assert_eq!(data.hosts[1].credential, None);

        // Moving a host to another credential replaces its link; leaving it
        // takes the link away.
        db.update_host(&using(web.clone(), &agent)).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].credential, Some(agent.id));
        let mut manual = web.clone();
        manual.credential = None;
        db.update_host(&manual).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].credential, None);

        // A host using a credential keeps no user name of its own: the
        // credential's is the only copy on disk.
        db.update_host(&web).unwrap();
        let mut renamed = key.clone();
        renamed.name = "生产部署".into();
        renamed.user = "admin".into();
        db.update_credential(&renamed).unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.credentials[0], renamed);
        assert_eq!(data.hosts[0].user.as_ref(), "");
        assert_eq!(data.hosts[1].user.as_ref(), "root");

        // A credential still in use cannot simply go.
        assert!(
            db.connection
                .execute("DELETE FROM credentials WHERE id = ?1", [to_sql(key.id.0)])
                .is_err()
        );
    }

    #[test]
    fn removing_a_credential_leaves_its_hosts_on_their_own_and_drops_the_link() {
        let db = HostDatabase::in_memory().unwrap();
        let key = credential(1, "部署", CredentialKind::Key);
        db.insert_credential(&key).unwrap();
        db.insert_host(&using(host(1, "web", None), &key)).unwrap();
        db.insert_host(&host(2, "db", None)).unwrap();

        db.remove_credential(key.id, AuthKind::NoPassword).unwrap();
        let data = db.load().unwrap();
        assert!(data.credentials.is_empty());
        assert_eq!(data.hosts[0].credential, None);
        assert_eq!(data.hosts[0].auth, AuthKind::NoPassword);
        assert_eq!(data.hosts[0].user.as_ref(), "deploy");
        // A host that never used it is untouched.
        assert_eq!(data.hosts[1].auth, AuthKind::Password);
    }

    #[test]
    fn the_database_refuses_a_credential_whose_key_file_does_not_fit_its_kind() {
        let db = HostDatabase::in_memory().unwrap();
        let mut key_without_file = credential(1, "部署", CredentialKind::Key);
        key_without_file.key_path = None;
        assert!(db.insert_credential(&key_without_file).is_err());

        let mut password_with_file = credential(2, "运维", CredentialKind::Password);
        password_with_file.key_path = Some("/tmp/id".into());
        assert!(db.insert_credential(&password_with_file).is_err());

        // A host cannot use a credential that is not there.
        let mut orphan = host(1, "web", None);
        orphan.credential = Some(CredentialId(99));
        assert!(db.insert_host(&orphan).is_err());
        // And the failed insert left no half of the host behind.
        assert!(db.load().unwrap().hosts.is_empty());
    }

    fn forward(id: u64, host: u64, kind: ForwardKind, port: u16) -> ForwardRule {
        let target = kind
            .has_target()
            .then(|| ForwardEndpoint::new("db.internal", 3306));
        ForwardRule::new(
            ForwardId(id),
            ForwardDraft::new(
                kind,
                HostId(host),
                ForwardEndpoint::new("127.0.0.1", port),
                target,
            ),
        )
    }

    #[test]
    fn forwards_round_trip_in_list_order_and_go_with_their_host() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_host(&host(1, "web", Some(1))).unwrap();
        db.insert_host(&host(2, "db", None)).unwrap();

        let mut local = forward(1, 1, ForwardKind::Local, 8080);
        local.name = "数据库".into();
        local.auto_start = true;
        local.sort_order = 1;
        db.insert_forward(&local).unwrap();
        let socks = forward(2, 2, ForwardKind::Dynamic, 1080);
        db.insert_forward(&socks).unwrap();
        let remote = forward(3, 1, ForwardKind::Remote, 9000);
        db.insert_forward(&remote).unwrap();

        // `sort_order` first, then id: the dynamic and remote rules are at 0.
        assert_eq!(
            db.load().unwrap().forwards,
            [socks.clone(), remote.clone(), local.clone()]
        );

        let mut edited = local.clone();
        edited.kind = ForwardKind::Dynamic;
        edited.target = None;
        edited.host = HostId(2);
        edited.bind = ForwardEndpoint::new("0.0.0.0", 1081);
        edited.auto_start = false;
        db.update_forward(&edited).unwrap();
        assert_eq!(db.load().unwrap().forwards[2], edited);

        db.remove_forward(ForwardId(2)).unwrap();
        assert_eq!(db.load().unwrap().forwards, [remote, edited.clone()]);

        // Deleting a group takes its hosts, and they take their forwards.
        db.remove_group(GroupId(1)).unwrap();
        assert_eq!(db.load().unwrap().forwards, [edited]);
        db.remove_host(HostId(2)).unwrap();
        assert!(db.load().unwrap().forwards.is_empty());
    }

    #[test]
    fn the_database_refuses_a_forward_whose_target_does_not_fit_its_kind() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_host(&host(1, "web", None)).unwrap();

        let mut dynamic_with_target = forward(1, 1, ForwardKind::Dynamic, 1080);
        dynamic_with_target.target = Some(ForwardEndpoint::new("db", 3306));
        assert!(db.insert_forward(&dynamic_with_target).is_err());

        let mut local_without_target = forward(2, 1, ForwardKind::Local, 8080);
        local_without_target.target = None;
        assert!(db.insert_forward(&local_without_target).is_err());

        // A rule must go through a host that exists.
        assert!(
            db.insert_forward(&forward(3, 99, ForwardKind::Local, 8080))
                .is_err()
        );
    }

    #[test]
    fn round_trips_nested_groups_and_hosts() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_group(&group(2, "数据库", Some(1))).unwrap();
        db.insert_host(&host(1, "web-01", Some(1))).unwrap();
        db.insert_host(&host(2, "db-01", Some(2))).unwrap();
        // A host at the root of the tree, outside every group.
        db.insert_host(&host(3, "jump", None)).unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.groups.len(), 2);
        assert_eq!(data.groups[1].parent, Some(GroupId(1)));
        let names: Vec<_> = data.hosts.iter().map(|s| s.name.as_ref()).collect();
        assert_eq!(names, ["web-01", "db-01", "jump"]);
        assert_eq!(data.hosts[2].group, None);
        assert_eq!(data.hosts[0].auth, AuthKind::Password);
        // Connection state is runtime only: everything loads disconnected.
        assert!(data.hosts.iter().all(|s| !s.state.is_connected()));
    }

    #[test]
    fn updating_rewrites_the_editable_fields() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_group(&group(2, "测试", None)).unwrap();
        db.insert_host(&host(1, "web-01", Some(1))).unwrap();

        let mut moved = host(1, "web-01", Some(2));
        moved.port = 2222;
        moved.auth = AuthKind::NoPassword;
        db.update_host(&moved).unwrap();
        db.update_group(&group(2, "预发", None)).unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.hosts[0].port, 2222);
        assert_eq!(data.hosts[0].auth, AuthKind::NoPassword);
        assert_eq!(data.hosts[0].group, Some(GroupId(2)));
        assert_eq!(data.groups[1].name.as_ref(), "预发");
    }

    #[test]
    fn removing_a_group_cascades_to_subgroups_and_their_hosts() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_group(&group(2, "数据库", Some(1))).unwrap();
        db.insert_group(&group(3, "测试", None)).unwrap();
        db.insert_host(&host(1, "web-01", Some(1))).unwrap();
        db.insert_host(&host(2, "db-01", Some(2))).unwrap();
        db.insert_host(&host(3, "qa", Some(3))).unwrap();
        db.insert_host(&host(4, "jump", None)).unwrap();

        db.remove_group(GroupId(1)).unwrap();

        let data = db.load().unwrap();
        let groups: Vec<_> = data.groups.iter().map(|g| g.name.as_ref()).collect();
        assert_eq!(groups, ["测试"]);
        let hosts: Vec<_> = data.hosts.iter().map(|s| s.name.as_ref()).collect();
        assert_eq!(hosts, ["qa", "jump"]);
    }

    #[test]
    fn recent_lists_the_most_recently_connected_first() {
        let db = HostDatabase::in_memory().unwrap();
        for id in 1..=3 {
            db.insert_host(&host(id, &format!("s{id}"), None)).unwrap();
        }
        db.touch_connected(HostId(1), 100).unwrap();
        db.touch_connected(HostId(3), 300).unwrap();
        // Host 2 never connected and stays out of the list.
        assert_eq!(
            db.load().unwrap().recent,
            [HostId(3), HostId(1)],
            "newest connection first"
        );

        db.touch_connected(HostId(1), 400).unwrap();
        assert_eq!(db.load().unwrap().recent, [HostId(1), HostId(3)]);
    }

    #[test]
    fn reopening_the_same_file_keeps_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let db = HostDatabase::open(&path).unwrap();
            db.insert_group(&group(1, "生产", None)).unwrap();
            db.insert_host(&host(1, "web-01", Some(1))).unwrap();
            db.touch_connected(HostId(1), 42).unwrap();
        }
        let db = HostDatabase::open(&path).unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.groups.len(), 1);
        assert_eq!(data.hosts[0].name.as_ref(), "web-01");
        assert_eq!(data.recent, [HostId(1)]);
    }

    #[test]
    fn tree_order_and_parent_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let db = HostDatabase::open(&path).unwrap();
            db.insert_group(&group(1, "生产", None)).unwrap();
            db.insert_group(&group(2, "测试", None)).unwrap();
            db.insert_host(&host(1, "web", Some(1))).unwrap();
            db.insert_host(&host(2, "db", Some(1))).unwrap();
            let mut data = db.load().unwrap();
            data.groups[1].parent = Some(GroupId(1));
            data.groups[1].sort_order = 0;
            data.hosts[0].group = Some(GroupId(2));
            data.hosts[1].sort_order = 0;
            db.save_tree_order(&data.groups, &data.hosts).unwrap();
        }
        let data = HostDatabase::open(&path).unwrap().load().unwrap();
        assert_eq!(data.groups[1].parent, Some(GroupId(1)));
        assert_eq!(data.hosts[0].group, Some(GroupId(2)));
        assert_eq!(data.hosts[1].sort_order, 0);
    }

    #[test]
    fn groups_start_expanded_and_then_keep_the_saved_choice() {
        let database = HostDatabase::in_memory().unwrap();
        database
            .connection
            .execute("INSERT INTO groups (id, name) VALUES (1, '生产')", [])
            .unwrap();

        let mut group = database.load().unwrap().groups.remove(0);
        assert!(group.expanded);
        database.set_group_expanded(group.id, false).unwrap();
        group.name = "生产环境".into();
        group.expanded = false;
        database.update_group(&group).unwrap();
        let saved = database.load().unwrap().groups.remove(0);
        assert_eq!(saved.name.as_ref(), "生产环境");
        assert!(!saved.expanded);
    }

    #[test]
    fn a_public_id_is_saved_and_kept_through_updates() {
        let db = HostDatabase::in_memory().unwrap();
        let host = host(1, "web-01", None);
        db.insert_host(&host).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].public_id, host.public_id);

        let mut renamed = host.clone();
        renamed.name = "web".into();
        renamed.public_id = PublicId::generate();
        db.update_host(&renamed).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].public_id, host.public_id);

        // Two hosts can never share one.
        let mut twin = Host::new(HostId(2), host.draft());
        twin.public_id = host.public_id.clone();
        assert!(db.insert_host(&twin).is_err());
    }

    #[test]
    fn hosts_added_by_hand_get_distinct_public_ids_that_stay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        HostDatabase::open(&path)
            .unwrap()
            .connection
            .execute(
                "INSERT INTO hosts (id, name, address, port, username, auth) \
             VALUES (1, 'web', 'example.test', 22, 'root', 'password'), \
                    (2, 'db', 'example.test', 22, 'root', 'password')",
                [],
            )
            .unwrap();

        let ids = || -> Vec<PublicId> {
            let data = HostDatabase::open(&path).unwrap().load().unwrap();
            data.hosts.into_iter().map(|s| s.public_id).collect()
        };
        let first = ids();
        assert_eq!(first.len(), 2);
        assert_ne!(first[0], first[1]);
        assert!(first.iter().all(|id| id.as_str().len() == 16));
        // Assigned once, not again at every open.
        assert_eq!(ids(), first);
    }

    #[test]
    fn the_detected_operating_system_is_read_back() {
        let db = HostDatabase::in_memory().unwrap();
        let host = Host::new(
            HostId(1),
            HostDraft::new("web", "10.0.0.1", 22, "root", AuthKind::Password, None),
        );
        db.insert_host(&host).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].os, None);

        db.set_host_os(host.id, Some(HostOs::Ubuntu)).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].os, Some(HostOs::Ubuntu));

        // Renaming must not disturb it: the form does not own this column.
        let mut renamed = host.clone();
        renamed.name = "web-01".into();
        db.update_host(&renamed).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].os, Some(HostOs::Ubuntu));

        db.set_host_os(host.id, None).unwrap();
        assert_eq!(db.load().unwrap().hosts[0].os, None);
    }

    #[test]
    fn bookmarks_keep_their_order_and_go_with_their_host() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_host(&host(1, "web", None)).unwrap();
        db.insert_host(&host(2, "db", None)).unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/var/log")
            .unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Local, "/Users/me")
            .unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/etc")
            .unwrap();
        // Adding the same path twice keeps one bookmark in its first place.
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/var/log")
            .unwrap();
        db.insert_bookmark(HostId(2), BookmarkSide::Remote, "/srv")
            .unwrap();
        assert_eq!(
            db.load().unwrap().bookmarks,
            [
                (HostId(1), BookmarkSide::Remote, "/var/log".to_string()),
                (HostId(1), BookmarkSide::Local, "/Users/me".to_string()),
                (HostId(1), BookmarkSide::Remote, "/etc".to_string()),
                (HostId(2), BookmarkSide::Remote, "/srv".to_string()),
            ]
        );
        // Reordering one pane leaves the other panes' order alone, and a
        // bookmark added afterwards still goes last.
        db.set_bookmark_order(
            HostId(1),
            BookmarkSide::Remote,
            &["/etc".into(), "/var/log".into()],
        )
        .unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/opt")
            .unwrap();
        let remote: Vec<_> = db
            .load()
            .unwrap()
            .bookmarks
            .into_iter()
            .filter(|(host, side, _)| *host == HostId(1) && *side == BookmarkSide::Remote)
            .map(|(_, _, path)| path)
            .collect();
        assert_eq!(remote, ["/etc", "/var/log", "/opt"]);
        db.remove_bookmark(HostId(1), BookmarkSide::Remote, "/opt")
            .unwrap();
        db.remove_bookmark(HostId(1), BookmarkSide::Remote, "/etc")
            .unwrap();
        db.remove_host(HostId(2)).unwrap();
        assert_eq!(
            db.load().unwrap().bookmarks,
            [
                (HostId(1), BookmarkSide::Remote, "/var/log".to_string()),
                (HostId(1), BookmarkSide::Local, "/Users/me".to_string()),
            ]
        );
    }

    #[test]
    fn schema_never_contains_secret_columns() {
        let db = HostDatabase::in_memory().unwrap();
        let tables = db
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for table in [
            "groups",
            "hosts",
            "host_jumps",
            "bookmarks",
            "forwards",
            "credentials",
        ] {
            assert!(tables.contains(&table.to_string()), "{table}");
        }
        let mut columns = Vec::new();
        for table in tables {
            let mut statement = db
                .connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            columns.extend(
                statement
                    .query_map([], |row| row.get::<_, String>(1))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap(),
            );
        }
        assert!(columns.contains(&"key_path".to_string()));
        assert!(!columns.iter().any(|column| {
            column.contains("password") || column.contains("passphrase") || column.contains("otp")
        }));
    }
}
