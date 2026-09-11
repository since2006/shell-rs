//! The SQLite file behind the session store.
//!
//! Everything here is synchronous and runs on the UI thread: the writes are
//! single rows triggered by a dialog the user just confirmed, not a stream.
//! The store keeps memory as the source of truth and mirrors each change
//! here, so a failed write costs the user persistence, never the edit.

use std::path::Path;

use rusqlite::{Connection, params};

use super::{AuthKind, GroupDraft, GroupId, Session, SessionDraft, SessionGroup, SessionId};

/// SQLite's only integer type is `i64`, so the `u64` ids cross the boundary
/// as `i64`. They are counters that start at 1 and never come near the range
/// where the round trip would lose anything.
fn to_sql(id: u64) -> i64 {
    id as i64
}

fn from_sql(id: i64) -> u64 {
    id as u64
}

/// Bumped whenever `migrate` gains a step. Stored in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = "\
BEGIN;
CREATE TABLE groups (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL,
    parent_id INTEGER REFERENCES groups(id) ON DELETE CASCADE
);
CREATE TABLE sessions (
    id                INTEGER PRIMARY KEY,
    name              TEXT NOT NULL,
    host              TEXT NOT NULL,
    port              INTEGER NOT NULL,
    username          TEXT NOT NULL,
    auth              TEXT NOT NULL,
    group_id          INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    last_connected_at INTEGER
);
CREATE INDEX sessions_group_id ON sessions(group_id);
PRAGMA user_version = 1;
COMMIT;";

/// Everything one launch reads back from disk.
pub struct StoredData {
    /// Groups in insertion order; `parent` gives the nesting.
    pub groups: Vec<SessionGroup>,
    /// Sessions in insertion order, all of them disconnected.
    pub sessions: Vec<Session>,
    /// Sessions that have ever connected, most recently connected first.
    pub recent: Vec<SessionId>,
}

pub struct SessionDatabase {
    connection: Connection,
}

impl SessionDatabase {
    /// Open (creating it if needed) the database at `path` and migrate it.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::prepare(Connection::open(path)?)
    }

    /// An anonymous database that lives only as long as this value. Used by
    /// tests, and as the fallback when the real file cannot be opened.
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut connection: Connection) -> rusqlite::Result<Self> {
        // Cascading deletes are how removing a group removes its subtree, and
        // SQLite only honours the foreign keys when asked, per connection.
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        migrate(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn load(&self) -> rusqlite::Result<StoredData> {
        let groups = self
            .connection
            .prepare("SELECT id, name, parent_id FROM groups ORDER BY id")?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let parent: Option<i64> = row.get(2)?;
                Ok(SessionGroup::new(
                    GroupId(from_sql(id)),
                    GroupDraft::new(name, parent.map(|id| GroupId(from_sql(id)))),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let sessions = self
            .connection
            .prepare(
                "SELECT id, name, host, port, username, auth, group_id \
                 FROM sessions ORDER BY id",
            )?
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let host: String = row.get(2)?;
                let port: u16 = row.get(3)?;
                let user: String = row.get(4)?;
                let auth: String = row.get(5)?;
                let group: Option<i64> = row.get(6)?;
                Ok(Session::new(
                    SessionId(from_sql(id)),
                    SessionDraft::new(
                        name,
                        host,
                        port,
                        user,
                        AuthKind::from_stored(&auth),
                        group.map(|id| GroupId(from_sql(id))),
                    ),
                ))
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

        Ok(StoredData {
            groups,
            sessions,
            recent,
        })
    }

    pub fn insert_group(&self, group: &SessionGroup) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO groups (id, name, parent_id) VALUES (?1, ?2, ?3)",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.parent.map(|parent| to_sql(parent.0)),
            ],
        )?;
        Ok(())
    }

    pub fn update_group(&self, group: &SessionGroup) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE groups SET name = ?2, parent_id = ?3 WHERE id = ?1",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.parent.map(|parent| to_sql(parent.0)),
            ],
        )?;
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
            "INSERT INTO sessions (id, name, host, port, username, auth, group_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                to_sql(session.id.0),
                session.name.as_ref(),
                session.host.as_ref(),
                session.port,
                session.user.as_ref(),
                session.auth.as_str(),
                session.group.map(|group| to_sql(group.0)),
            ],
        )?;
        Ok(())
    }

    /// Rewrites the editable fields. `last_connected_at` is left alone: it is
    /// written by `touch_connected` and is not part of the session form.
    pub fn update_session(&self, session: &Session) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE sessions SET name = ?2, host = ?3, port = ?4, username = ?5, \
             auth = ?6, group_id = ?7 WHERE id = ?1",
            params![
                to_sql(session.id.0),
                session.name.as_ref(),
                session.host.as_ref(),
                session.port,
                session.user.as_ref(),
                session.auth.as_str(),
                session.group.map(|group| to_sql(group.0)),
            ],
        )?;
        Ok(())
    }

    pub fn remove_session(&self, id: SessionId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM sessions WHERE id = ?1", params![to_sql(id.0)])?;
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

fn migrate(connection: &mut Connection) -> rusqlite::Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    if version < 1 {
        connection.execute_batch(SCHEMA_V1)?;
    }
    Ok(())
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
    fn a_fresh_database_is_empty_and_migrated() {
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
        db.update_session(&moved).unwrap();
        db.update_group(&group(2, "预发", None)).unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.sessions[0].port, 2222);
        assert_eq!(data.sessions[0].auth, AuthKind::Key);
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
        let path = dir.path().join("shellr.db");
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
}
