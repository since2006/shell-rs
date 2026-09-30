//! The SQLite file behind the session store.
//!
//! Everything here is synchronous and runs on the UI thread: the writes are
//! single rows triggered by a dialog the user just confirmed, not a stream.
//! The store keeps memory as the source of truth and mirrors each change
//! here, so a failed write costs the user persistence, never the edit.

use std::{collections::HashSet, path::Path};

use rusqlite::{Connection, params};

use super::{
    AuthKind, BookmarkSide, ForwardDraft, ForwardEndpoint, ForwardId, ForwardKind, ForwardRule,
    GroupDraft, GroupId, HostOs, PublicId, Session, SessionDraft, SessionGroup, SessionId,
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
/// The steps start at 7 rather than 1 because the databases of development
/// builds were already at 7 when the older steps were folded into `SCHEMA`.
const SCHEMA_VERSION: i64 = 8;

/// The port-forwarding rules, added in version 8. A macro rather than a
/// constant so that `SCHEMA` and the step from version 7 are built from the
/// same text and cannot drift apart.
///
/// A rule goes with its session. A dynamic forward has no target; the other
/// two kinds must have one.
macro_rules! forwards_table {
    () => {
        "\
CREATE TABLE forwards (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    name        TEXT NOT NULL DEFAULT '',
    kind        TEXT NOT NULL CHECK (kind IN ('local', 'remote', 'dynamic')),
    bind_host   TEXT NOT NULL,
    bind_port   INTEGER NOT NULL CHECK (bind_port BETWEEN 1 AND 65535),
    target_host TEXT,
    target_port INTEGER,
    auto_start  INTEGER NOT NULL DEFAULT 0,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    CHECK ((kind = 'dynamic' AND target_host IS NULL AND target_port IS NULL)
        OR (kind <> 'dynamic' AND target_host IS NOT NULL
            AND target_port BETWEEN 1 AND 65535))
);
CREATE INDEX forwards_session_id ON forwards(session_id);"
    };
}

/// The whole schema, as a new database gets it.
///
/// Groups nest through `parent_id`, and both references to a group cascade:
/// deleting one takes its subgroups and their sessions. Bookmarks are kept
/// per session and per pane, and go with their session, the way WinSCP drops
/// a site's bookmarks with the site. `public_id` may be missing, which a row
/// added by hand is until the next open (see `fill_missing_public_ids`); the
/// index keeps the ones that are there unique.
const SCHEMA: &str = concat!(
    "\
CREATE TABLE groups (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    parent_id  INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    expanded   INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE sessions (
    id                INTEGER PRIMARY KEY,
    name              TEXT NOT NULL,
    host              TEXT NOT NULL,
    port              INTEGER NOT NULL,
    username          TEXT NOT NULL,
    auth              TEXT NOT NULL,
    group_id          INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    last_connected_at INTEGER,
    key_path          TEXT,
    os                TEXT,
    sort_order        INTEGER NOT NULL DEFAULT 0,
    public_id         TEXT
);
CREATE INDEX sessions_group_id ON sessions(group_id);
CREATE UNIQUE INDEX sessions_public_id ON sessions(public_id);
CREATE TABLE bookmarks (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    side       TEXT NOT NULL CHECK (side IN ('local', 'remote')),
    path       TEXT NOT NULL,
    sort_order INTEGER NOT NULL,
    UNIQUE (session_id, side, path)
);
",
    forwards_table!()
);

/// Everything one launch reads back from disk.
pub struct StoredData {
    /// Groups in id order; `parent` and `sort_order` give the visible tree.
    pub groups: Vec<SessionGroup>,
    /// Sessions in id order, all of them disconnected.
    pub sessions: Vec<Session>,
    /// Sessions that have ever connected, most recently connected first.
    pub recent: Vec<SessionId>,
    /// SFTP bookmarks in the order they were added.
    pub bookmarks: Vec<(SessionId, BookmarkSide, String)>,
    /// Port-forwarding rules in the order the forward list shows them.
    pub forwards: Vec<ForwardRule>,
}

pub struct SessionDatabase {
    connection: Connection,
}

impl SessionDatabase {
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
                let mut group = SessionGroup::new(
                    GroupId(from_sql(id)),
                    GroupDraft::new(name, parent.map(|id| GroupId(from_sql(id)))),
                );
                group.sort_order = row.get(3)?;
                group.expanded = row.get(4)?;
                Ok(group)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let sessions = self
            .connection
            .prepare(
                "SELECT id, name, host, port, username, auth, group_id, key_path, os, sort_order, \
                 public_id FROM sessions ORDER BY id",
            )?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let host: String = row.get(2)?;
                let port: u16 = row.get(3)?;
                let user: String = row.get(4)?;
                let auth: String = row.get(5)?;
                let group: Option<i64> = row.get(6)?;
                let key_path: Option<String> = row.get(7)?;
                let os: Option<String> = row.get(8)?;
                let mut session = Session::new(
                    SessionId(from_sql(id)),
                    SessionDraft::new(
                        name,
                        host,
                        port,
                        user,
                        AuthKind::from_stored(&auth),
                        group.map(|id| GroupId(from_sql(id))),
                    )
                    .with_optional_key_path(key_path),
                );
                session.os = os.as_deref().and_then(HostOs::from_stored);
                session.sort_order = row.get(9)?;
                session.public_id = PublicId::from_stored(row.get(10)?);
                Ok(session)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let recent = self
            .connection
            .prepare(
                "SELECT id FROM sessions WHERE last_connected_at IS NOT NULL \
                 ORDER BY last_connected_at DESC, id DESC",
            )?
            .query_map([], |row| {
                row.get::<_, i64>(0).map(|id| SessionId(from_sql(id)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let bookmarks = self
            .connection
            .prepare("SELECT session_id, side, path FROM bookmarks ORDER BY sort_order, id")?
            .query_map([], |row| {
                let session: i64 = row.get(0)?;
                let side: String = row.get(1)?;
                Ok((
                    SessionId(from_sql(session)),
                    BookmarkSide::from_stored(&side),
                    row.get::<_, String>(2)?,
                ))
            })?
            .filter_map(|row| match row {
                Ok((session, Some(side), path)) => Some(Ok((session, side, path))),
                Ok((_, None, _)) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let forwards = self
            .connection
            .prepare(
                "SELECT id, session_id, name, kind, bind_host, bind_port, target_host, \
                 target_port, auto_start, sort_order FROM forwards ORDER BY sort_order, id",
            )?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let session: i64 = row.get(1)?;
                let name: String = row.get(2)?;
                let kind: String = row.get(3)?;
                let bind_host: String = row.get(4)?;
                let bind_port: u16 = row.get(5)?;
                let target_host: Option<String> = row.get(6)?;
                let target_port: Option<u16> = row.get(7)?;
                let auto_start: bool = row.get(8)?;
                let sort_order: i64 = row.get(9)?;
                // A kind this build does not know was written by a newer one;
                // the rule is left out rather than run as something else.
                Ok(ForwardKind::from_stored(&kind).map(|kind| {
                    let target = target_host
                        .zip(target_port)
                        .map(|(host, port)| ForwardEndpoint::new(host, port));
                    let draft = ForwardDraft::new(
                        kind,
                        SessionId(from_sql(session)),
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

        Ok(StoredData {
            groups,
            sessions,
            recent,
            bookmarks,
            forwards,
        })
    }

    pub fn insert_group(&self, group: &SessionGroup) -> rusqlite::Result<()> {
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

    pub fn update_group(&self, group: &SessionGroup) -> rusqlite::Result<()> {
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

    /// Save the session tree's expansion choice for one group.
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

    /// Deleting a group takes its subgroups and their sessions with it, via
    /// the `ON DELETE CASCADE` on both foreign keys.
    pub fn remove_group(&self, id: GroupId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM groups WHERE id = ?1", params![to_sql(id.0)])?;
        Ok(())
    }

    pub fn insert_session(&self, session: &Session) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO sessions (id, name, host, port, username, auth, group_id, key_path, os, sort_order, public_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                to_sql(session.id.0),
                session.name.as_ref(),
                session.host.as_ref(),
                session.port,
                session.user.as_ref(),
                session.auth.as_str(),
                session.group.map(|group| to_sql(group.0)),
                session.key_path.as_deref(),
                session.os.map(HostOs::as_str),
                session.sort_order,
                session.public_id.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Rewrites the editable fields. `last_connected_at` and `os` are left
    /// alone: they are written by `touch_connected` and `set_host_os`, and
    /// neither is part of the session form. `public_id` never changes.
    pub fn update_session(&self, session: &Session) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE sessions SET name = ?2, host = ?3, port = ?4, username = ?5, \
             auth = ?6, group_id = ?7, key_path = ?8, sort_order = ?9 WHERE id = ?1",
            params![
                to_sql(session.id.0),
                session.name.as_ref(),
                session.host.as_ref(),
                session.port,
                session.user.as_ref(),
                session.auth.as_str(),
                session.group.map(|group| to_sql(group.0)),
                session.key_path.as_deref(),
                session.sort_order,
            ],
        )?;
        Ok(())
    }

    pub fn remove_session(&self, id: SessionId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM sessions WHERE id = ?1", params![to_sql(id.0)])?;
        Ok(())
    }

    /// Save one drag as a unit, including parent changes and sibling order.
    pub fn save_tree_order(
        &self,
        groups: &[SessionGroup],
        sessions: &[Session],
    ) -> rusqlite::Result<()> {
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
            let mut update_session = transaction
                .prepare("UPDATE sessions SET group_id = ?2, sort_order = ?3 WHERE id = ?1")?;
            for session in sessions {
                update_session.execute(params![
                    to_sql(session.id.0),
                    session.group.map(|id| to_sql(id.0)),
                    session.sort_order,
                ])?;
            }
        }
        transaction.commit()
    }

    /// Append a bookmark; adding one that exists is a no-op.
    pub fn insert_bookmark(
        &self,
        session: SessionId,
        side: BookmarkSide,
        path: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO bookmarks (session_id, side, path, sort_order) \
             VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM bookmarks))",
            params![to_sql(session.0), side.as_str(), path],
        )?;
        Ok(())
    }

    pub fn remove_bookmark(
        &self,
        session: SessionId,
        side: BookmarkSide,
        path: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "DELETE FROM bookmarks WHERE session_id = ?1 AND side = ?2 AND path = ?3",
            params![to_sql(session.0), side.as_str(), path],
        )?;
        Ok(())
    }

    /// Put one pane's bookmarks in the order of `paths`. Loading groups
    /// bookmarks by pane, so only the order within this pane matters.
    pub fn set_bookmark_order(
        &self,
        session: SessionId,
        side: BookmarkSide,
        paths: &[String],
    ) -> rusqlite::Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        {
            let mut update = transaction.prepare(
                "UPDATE bookmarks SET sort_order = ?4 \
                 WHERE session_id = ?1 AND side = ?2 AND path = ?3",
            )?;
            for (order, path) in (0_i64..).zip(paths) {
                update.execute(params![to_sql(session.0), side.as_str(), path, order])?;
            }
        }
        transaction.commit()
    }

    pub fn insert_forward(&self, rule: &ForwardRule) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO forwards (id, session_id, name, kind, bind_host, bind_port, \
             target_host, target_port, auto_start, sort_order) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                to_sql(rule.id.0),
                to_sql(rule.session.0),
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
            "UPDATE forwards SET session_id = ?2, name = ?3, kind = ?4, bind_host = ?5, \
             bind_port = ?6, target_host = ?7, target_port = ?8, auto_start = ?9, \
             sort_order = ?10 WHERE id = ?1",
            params![
                to_sql(rule.id.0),
                to_sql(rule.session.0),
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

    /// Record the operating system a probe found on the host. `None` clears
    /// it, which is what a failed probe on a rebuilt host leaves behind.
    pub fn set_host_os(&self, id: SessionId, os: Option<HostOs>) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE sessions SET os = ?2 WHERE id = ?1",
            params![to_sql(id.0), os.map(HostOs::as_str)],
        )?;
        Ok(())
    }

    /// Record that a session just connected, which is what orders the start
    /// page's recent list across launches.
    pub fn touch_connected(&self, id: SessionId, at: u64) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE sessions SET last_connected_at = ?2 WHERE id = ?1",
            params![to_sql(id.0), to_sql(at)],
        )?;
        Ok(())
    }
}

/// Bring the database to `SCHEMA_VERSION`. A new file has version 0 and gets
/// the whole schema; an older one is taken forward one version at a time.
/// Each step is one transaction, so a file is never left between versions.
fn migrate(connection: &Connection) -> rusqlite::Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 0 {
        return connection.execute_batch(&format!(
            "BEGIN;\n{SCHEMA}\nPRAGMA user_version = {SCHEMA_VERSION};\nCOMMIT;"
        ));
    }
    if version == 7 {
        connection.execute_batch(concat!(
            "BEGIN;\n",
            forwards_table!(),
            "\nPRAGMA user_version = 8;\nCOMMIT;"
        ))?;
    }
    Ok(())
}

/// Give every session without a [`PublicId`] one, which is any row added by
/// hand with `sqlite3`. Runs at every open; once every session has an id it
/// is one query that finds nothing.
fn fill_missing_public_ids(connection: &mut Connection) -> rusqlite::Result<()> {
    let missing = connection
        .prepare("SELECT id FROM sessions WHERE public_id IS NULL")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if missing.is_empty() {
        return Ok(());
    }
    let mut taken = connection
        .prepare("SELECT public_id FROM sessions WHERE public_id IS NOT NULL")?
        .query_map([], |row| row.get::<_, String>(0).map(PublicId::from_stored))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    let transaction = connection.transaction()?;
    {
        let mut update = transaction.prepare("UPDATE sessions SET public_id = ?2 WHERE id = ?1")?;
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

    fn group(id: u64, name: &str, parent: Option<u64>) -> SessionGroup {
        SessionGroup::new(GroupId(id), GroupDraft::new(name, parent.map(GroupId)))
    }

    fn session(id: u64, name: &str, group: Option<u64>) -> Session {
        Session::new(
            SessionId(id),
            SessionDraft::new(
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
        let db = SessionDatabase::in_memory().unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let data = db.load().unwrap();
        assert!(data.groups.is_empty());
        assert!(data.sessions.is_empty());
        assert!(data.recent.is_empty());
        assert!(data.forwards.is_empty());
    }

    /// The schema as version 7 had it, frozen here so the step from 7 keeps
    /// being tested against what is really on people's disks.
    const SCHEMA_V7: &str = "\
CREATE TABLE groups (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    parent_id  INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    expanded   INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE sessions (
    id                INTEGER PRIMARY KEY,
    name              TEXT NOT NULL,
    host              TEXT NOT NULL,
    port              INTEGER NOT NULL,
    username          TEXT NOT NULL,
    auth              TEXT NOT NULL,
    group_id          INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    last_connected_at INTEGER,
    key_path          TEXT,
    os                TEXT,
    sort_order        INTEGER NOT NULL DEFAULT 0,
    public_id         TEXT
);
CREATE INDEX sessions_group_id ON sessions(group_id);
CREATE UNIQUE INDEX sessions_public_id ON sessions(public_id);
CREATE TABLE bookmarks (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    side       TEXT NOT NULL CHECK (side IN ('local', 'remote')),
    path       TEXT NOT NULL,
    sort_order INTEGER NOT NULL,
    UNIQUE (session_id, side, path)
);
PRAGMA user_version = 7;";

    /// Every table and index, as SQLite recorded its definition.
    fn schema_of(connection: &Connection) -> Vec<(String, String)> {
        connection
            .prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    #[test]
    fn a_version_7_database_gains_the_forwards_table_and_keeps_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(SCHEMA_V7).unwrap();
            old.execute(
                "INSERT INTO sessions (id, name, host, port, username, auth, public_id) \
                 VALUES (1, 'web', 'example.test', 22, 'root', 'auto', 'abcdefgh12345678')",
                [],
            )
            .unwrap();
        }

        let db = SessionDatabase::open(&path).unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let data = db.load().unwrap();
        assert_eq!(data.sessions[0].name.as_ref(), "web");
        assert!(data.forwards.is_empty());
        // Upgrading ends with exactly the schema a new file gets.
        let fresh = SessionDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));

        // Opening it again finds nothing left to do.
        drop(db);
        assert!(SessionDatabase::open(&path).is_ok());
    }

    fn forward(id: u64, session: u64, kind: ForwardKind, port: u16) -> ForwardRule {
        let target = kind
            .has_target()
            .then(|| ForwardEndpoint::new("db.internal", 3306));
        ForwardRule::new(
            ForwardId(id),
            ForwardDraft::new(
                kind,
                SessionId(session),
                ForwardEndpoint::new("127.0.0.1", port),
                target,
            ),
        )
    }

    #[test]
    fn forwards_round_trip_in_list_order_and_go_with_their_session() {
        let db = SessionDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_session(&session(1, "web", Some(1))).unwrap();
        db.insert_session(&session(2, "db", None)).unwrap();

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
        edited.session = SessionId(2);
        edited.bind = ForwardEndpoint::new("0.0.0.0", 1081);
        edited.auto_start = false;
        db.update_forward(&edited).unwrap();
        assert_eq!(db.load().unwrap().forwards[2], edited);

        db.remove_forward(ForwardId(2)).unwrap();
        assert_eq!(db.load().unwrap().forwards, [remote, edited.clone()]);

        // Deleting a group takes its sessions, and they take their forwards.
        db.remove_group(GroupId(1)).unwrap();
        assert_eq!(db.load().unwrap().forwards, [edited]);
        db.remove_session(SessionId(2)).unwrap();
        assert!(db.load().unwrap().forwards.is_empty());
    }

    #[test]
    fn the_database_refuses_a_forward_whose_target_does_not_fit_its_kind() {
        let db = SessionDatabase::in_memory().unwrap();
        db.insert_session(&session(1, "web", None)).unwrap();

        let mut dynamic_with_target = forward(1, 1, ForwardKind::Dynamic, 1080);
        dynamic_with_target.target = Some(ForwardEndpoint::new("db", 3306));
        assert!(db.insert_forward(&dynamic_with_target).is_err());

        let mut local_without_target = forward(2, 1, ForwardKind::Local, 8080);
        local_without_target.target = None;
        assert!(db.insert_forward(&local_without_target).is_err());

        // A rule must go through a session that exists.
        assert!(
            db.insert_forward(&forward(3, 99, ForwardKind::Local, 8080))
                .is_err()
        );
    }

    #[test]
    fn round_trips_nested_groups_and_sessions() {
        let db = SessionDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_group(&group(2, "数据库", Some(1))).unwrap();
        db.insert_session(&session(1, "web-01", Some(1))).unwrap();
        db.insert_session(&session(2, "db-01", Some(2))).unwrap();
        // A session at the root of the tree, outside every group.
        db.insert_session(&session(3, "jump", None)).unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.groups.len(), 2);
        assert_eq!(data.groups[1].parent, Some(GroupId(1)));
        let names: Vec<_> = data.sessions.iter().map(|s| s.name.as_ref()).collect();
        assert_eq!(names, ["web-01", "db-01", "jump"]);
        assert_eq!(data.sessions[2].group, None);
        assert_eq!(data.sessions[0].auth, AuthKind::Password);
        // Connection state is runtime only: everything loads disconnected.
        assert!(data.sessions.iter().all(|s| !s.state.is_connected()));
    }

    #[test]
    fn updating_rewrites_the_editable_fields() {
        let db = SessionDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_group(&group(2, "测试", None)).unwrap();
        db.insert_session(&session(1, "web-01", Some(1))).unwrap();

        let mut moved = session(1, "web-01", Some(2));
        moved.port = 2222;
        moved.auth = AuthKind::Key;
        moved.key_path = Some("/tmp/test-key".into());
        db.update_session(&moved).unwrap();
        db.update_group(&group(2, "预发", None)).unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.sessions[0].port, 2222);
        assert_eq!(data.sessions[0].auth, AuthKind::Key);
        assert_eq!(data.sessions[0].key_path.as_deref(), Some("/tmp/test-key"));
        assert_eq!(data.sessions[0].group, Some(GroupId(2)));
        assert_eq!(data.groups[1].name.as_ref(), "预发");
    }

    #[test]
    fn removing_a_group_cascades_to_subgroups_and_their_sessions() {
        let db = SessionDatabase::in_memory().unwrap();
        db.insert_group(&group(1, "生产", None)).unwrap();
        db.insert_group(&group(2, "数据库", Some(1))).unwrap();
        db.insert_group(&group(3, "测试", None)).unwrap();
        db.insert_session(&session(1, "web-01", Some(1))).unwrap();
        db.insert_session(&session(2, "db-01", Some(2))).unwrap();
        db.insert_session(&session(3, "qa", Some(3))).unwrap();
        db.insert_session(&session(4, "jump", None)).unwrap();

        db.remove_group(GroupId(1)).unwrap();

        let data = db.load().unwrap();
        let groups: Vec<_> = data.groups.iter().map(|g| g.name.as_ref()).collect();
        assert_eq!(groups, ["测试"]);
        let sessions: Vec<_> = data.sessions.iter().map(|s| s.name.as_ref()).collect();
        assert_eq!(sessions, ["qa", "jump"]);
    }

    #[test]
    fn recent_lists_the_most_recently_connected_first() {
        let db = SessionDatabase::in_memory().unwrap();
        for id in 1..=3 {
            db.insert_session(&session(id, &format!("s{id}"), None))
                .unwrap();
        }
        db.touch_connected(SessionId(1), 100).unwrap();
        db.touch_connected(SessionId(3), 300).unwrap();
        // Session 2 never connected and stays out of the list.
        assert_eq!(
            db.load().unwrap().recent,
            [SessionId(3), SessionId(1)],
            "newest connection first"
        );

        db.touch_connected(SessionId(1), 400).unwrap();
        assert_eq!(db.load().unwrap().recent, [SessionId(1), SessionId(3)]);
    }

    #[test]
    fn reopening_the_same_file_keeps_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let db = SessionDatabase::open(&path).unwrap();
            db.insert_group(&group(1, "生产", None)).unwrap();
            db.insert_session(&session(1, "web-01", Some(1))).unwrap();
            db.touch_connected(SessionId(1), 42).unwrap();
        }
        let db = SessionDatabase::open(&path).unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.groups.len(), 1);
        assert_eq!(data.sessions[0].name.as_ref(), "web-01");
        assert_eq!(data.recent, [SessionId(1)]);
    }

    #[test]
    fn tree_order_and_parent_survive_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let db = SessionDatabase::open(&path).unwrap();
            db.insert_group(&group(1, "生产", None)).unwrap();
            db.insert_group(&group(2, "测试", None)).unwrap();
            db.insert_session(&session(1, "web", Some(1))).unwrap();
            db.insert_session(&session(2, "db", Some(1))).unwrap();
            let mut data = db.load().unwrap();
            data.groups[1].parent = Some(GroupId(1));
            data.groups[1].sort_order = 0;
            data.sessions[0].group = Some(GroupId(2));
            data.sessions[1].sort_order = 0;
            db.save_tree_order(&data.groups, &data.sessions).unwrap();
        }
        let data = SessionDatabase::open(&path).unwrap().load().unwrap();
        assert_eq!(data.groups[1].parent, Some(GroupId(1)));
        assert_eq!(data.sessions[0].group, Some(GroupId(2)));
        assert_eq!(data.sessions[1].sort_order, 0);
    }

    #[test]
    fn groups_start_expanded_and_then_keep_the_saved_choice() {
        let database = SessionDatabase::in_memory().unwrap();
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
        let db = SessionDatabase::in_memory().unwrap();
        let session = session(1, "web-01", None);
        db.insert_session(&session).unwrap();
        assert_eq!(db.load().unwrap().sessions[0].public_id, session.public_id);

        let mut renamed = session.clone();
        renamed.name = "web".into();
        renamed.public_id = PublicId::generate();
        db.update_session(&renamed).unwrap();
        assert_eq!(db.load().unwrap().sessions[0].public_id, session.public_id);

        // Two sessions can never share one.
        let mut twin = Session::new(SessionId(2), session.draft());
        twin.public_id = session.public_id.clone();
        assert!(db.insert_session(&twin).is_err());
    }

    #[test]
    fn sessions_added_by_hand_get_distinct_public_ids_that_stay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        SessionDatabase::open(&path)
            .unwrap()
            .connection
            .execute(
                "INSERT INTO sessions (id, name, host, port, username, auth) \
             VALUES (1, 'web', 'example.test', 22, 'root', 'auto'), \
                    (2, 'db', 'example.test', 22, 'root', 'auto')",
                [],
            )
            .unwrap();

        let ids = || -> Vec<PublicId> {
            let data = SessionDatabase::open(&path).unwrap().load().unwrap();
            data.sessions.into_iter().map(|s| s.public_id).collect()
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
        let db = SessionDatabase::in_memory().unwrap();
        let session = Session::new(
            SessionId(1),
            SessionDraft::new("web", "10.0.0.1", 22, "root", AuthKind::Auto, None),
        );
        db.insert_session(&session).unwrap();
        assert_eq!(db.load().unwrap().sessions[0].os, None);

        db.set_host_os(session.id, Some(HostOs::Ubuntu)).unwrap();
        assert_eq!(db.load().unwrap().sessions[0].os, Some(HostOs::Ubuntu));

        // Renaming must not disturb it: the form does not own this column.
        let mut renamed = session.clone();
        renamed.name = "web-01".into();
        db.update_session(&renamed).unwrap();
        assert_eq!(db.load().unwrap().sessions[0].os, Some(HostOs::Ubuntu));

        db.set_host_os(session.id, None).unwrap();
        assert_eq!(db.load().unwrap().sessions[0].os, None);
    }

    #[test]
    fn bookmarks_keep_their_order_and_go_with_their_session() {
        let db = SessionDatabase::in_memory().unwrap();
        db.insert_session(&session(1, "web", None)).unwrap();
        db.insert_session(&session(2, "db", None)).unwrap();
        db.insert_bookmark(SessionId(1), BookmarkSide::Remote, "/var/log")
            .unwrap();
        db.insert_bookmark(SessionId(1), BookmarkSide::Local, "/Users/me")
            .unwrap();
        db.insert_bookmark(SessionId(1), BookmarkSide::Remote, "/etc")
            .unwrap();
        // Adding the same path twice keeps one bookmark in its first place.
        db.insert_bookmark(SessionId(1), BookmarkSide::Remote, "/var/log")
            .unwrap();
        db.insert_bookmark(SessionId(2), BookmarkSide::Remote, "/srv")
            .unwrap();
        assert_eq!(
            db.load().unwrap().bookmarks,
            [
                (SessionId(1), BookmarkSide::Remote, "/var/log".to_string()),
                (SessionId(1), BookmarkSide::Local, "/Users/me".to_string()),
                (SessionId(1), BookmarkSide::Remote, "/etc".to_string()),
                (SessionId(2), BookmarkSide::Remote, "/srv".to_string()),
            ]
        );
        // Reordering one pane leaves the other panes' order alone, and a
        // bookmark added afterwards still goes last.
        db.set_bookmark_order(
            SessionId(1),
            BookmarkSide::Remote,
            &["/etc".into(), "/var/log".into()],
        )
        .unwrap();
        db.insert_bookmark(SessionId(1), BookmarkSide::Remote, "/opt")
            .unwrap();
        let remote: Vec<_> = db
            .load()
            .unwrap()
            .bookmarks
            .into_iter()
            .filter(|(session, side, _)| *session == SessionId(1) && *side == BookmarkSide::Remote)
            .map(|(_, _, path)| path)
            .collect();
        assert_eq!(remote, ["/etc", "/var/log", "/opt"]);
        db.remove_bookmark(SessionId(1), BookmarkSide::Remote, "/opt")
            .unwrap();
        db.remove_bookmark(SessionId(1), BookmarkSide::Remote, "/etc")
            .unwrap();
        db.remove_session(SessionId(2)).unwrap();
        assert_eq!(
            db.load().unwrap().bookmarks,
            [
                (SessionId(1), BookmarkSide::Remote, "/var/log".to_string()),
                (SessionId(1), BookmarkSide::Local, "/Users/me".to_string()),
            ]
        );
    }

    #[test]
    fn schema_never_contains_secret_columns() {
        let db = SessionDatabase::in_memory().unwrap();
        let tables = db
            .connection
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"forwards".to_string()));
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
