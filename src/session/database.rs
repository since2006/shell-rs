//! The SQLite file behind the session store.
//!
//! Everything here is synchronous and runs on the UI thread: the writes are
//! single rows triggered by a dialog the user just confirmed, not a stream.
//! The store keeps memory as the source of truth and mirrors each change
//! here, so a failed write costs the user persistence, never the edit.

use std::path::Path;

use rusqlite::{Connection, params};

use super::{
    AuthKind, BookmarkSide, GroupDraft, GroupId, HostOs, Session, SessionDraft, SessionGroup,
    SessionId,
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

/// Bumped whenever `migrate` gains a step. Stored in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 6;

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

const SCHEMA_V2: &str = "\
BEGIN;
ALTER TABLE sessions ADD COLUMN key_path TEXT;
UPDATE sessions SET auth = 'auto' WHERE auth = 'key';
PRAGMA user_version = 2;
COMMIT;";

const SCHEMA_V3: &str = "\
BEGIN;
ALTER TABLE sessions ADD COLUMN os TEXT;
PRAGMA user_version = 3;
COMMIT;";

const SCHEMA_V4: &str = "\
BEGIN;
ALTER TABLE groups ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
UPDATE groups SET sort_order = id;
UPDATE sessions SET sort_order = id;
PRAGMA user_version = 4;
COMMIT;";

const SCHEMA_V5: &str = "\
BEGIN;
ALTER TABLE groups ADD COLUMN expanded INTEGER NOT NULL DEFAULT 1;
PRAGMA user_version = 5;
COMMIT;";

/// Per-session SFTP bookmarks, one list per pane. The cascade removes them
/// with their session, the way WinSCP drops a site's bookmarks with the site.
const SCHEMA_V6: &str = "\
BEGIN;
CREATE TABLE bookmarks (
    id         INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    side       TEXT NOT NULL CHECK (side IN ('local', 'remote')),
    path       TEXT NOT NULL,
    sort_order INTEGER NOT NULL,
    UNIQUE (session_id, side, path)
);
PRAGMA user_version = 6;
COMMIT;";

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
                "SELECT id, name, host, port, username, auth, group_id, key_path, os, sort_order \
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

        Ok(StoredData {
            groups,
            sessions,
            recent,
            bookmarks,
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
            "INSERT INTO sessions (id, name, host, port, username, auth, group_id, key_path, os, sort_order) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            ],
        )?;
        Ok(())
    }

    /// Rewrites the editable fields. `last_connected_at` and `os` are left
    /// alone: they are written by `touch_connected` and `set_host_os`, and
    /// neither is part of the session form.
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

fn migrate(connection: &mut Connection) -> rusqlite::Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    if version < 1 {
        connection.execute_batch(SCHEMA_V1)?;
    }
    if version < 2 {
        connection.execute_batch(SCHEMA_V2)?;
    }
    if version < 3 {
        connection.execute_batch(SCHEMA_V3)?;
    }
    if version < 4 {
        connection.execute_batch(SCHEMA_V4)?;
    }
    if version < 5 {
        connection.execute_batch(SCHEMA_V5)?;
    }
    if version < 6 {
        connection.execute_batch(SCHEMA_V6)?;
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
    fn v4_groups_start_expanded_and_then_keep_the_saved_choice() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA_V1).unwrap();
        connection.execute_batch(SCHEMA_V2).unwrap();
        connection.execute_batch(SCHEMA_V3).unwrap();
        connection.execute_batch(SCHEMA_V4).unwrap();
        connection
            .execute("INSERT INTO groups (id, name) VALUES (1, '生产')", [])
            .unwrap();

        let database = SessionDatabase::prepare(connection).unwrap();
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
    fn migrates_v1_key_auth_to_auto_and_adds_key_path_and_os() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA_V1).unwrap();
        connection
            .execute(
                "INSERT INTO sessions (id, name, host, port, username, auth) \
             VALUES (1, '旧会话', 'example.test', 22, 'root', 'key')",
                [],
            )
            .unwrap();

        let db = SessionDatabase::prepare(connection).unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let data = db.load().unwrap();
        assert_eq!(data.sessions[0].auth, AuthKind::Auto);
        assert_eq!(data.sessions[0].key_path, None);
        assert_eq!(data.sessions[0].os, None, "老库里的会话还没探测过");
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
    fn v5_databases_gain_bookmarks_that_cascade_with_their_session() {
        let connection = Connection::open_in_memory().unwrap();
        for step in [SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5] {
            connection.execute_batch(step).unwrap();
        }
        connection
            .execute(
                "INSERT INTO sessions (id, name, host, port, username, auth) \
             VALUES (1, 'web', 'example.test', 22, 'root', 'auto'), \
                    (2, 'db', 'example.test', 22, 'root', 'auto')",
                [],
            )
            .unwrap();
        let db = SessionDatabase::prepare(connection).unwrap();
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
        let mut statement = db
            .connection
            .prepare("PRAGMA table_info(sessions)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.contains(&"key_path".to_string()));
        assert!(!columns.iter().any(|column| {
            column.contains("password") || column.contains("passphrase") || column.contains("otp")
        }));
    }
}
