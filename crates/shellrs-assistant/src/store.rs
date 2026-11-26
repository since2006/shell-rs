//! The conversations, in a database file of their own (`assistant.db`):
//! not the hosts' database, which is copied whole before each upgrade and
//! is read into memory whole, and never synced.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension as _, params};

use crate::error::StoreError;
use crate::message::{Message, Part, Raw, Role, RunIn, StopReason, Usage};
use crate::session::Approval;

/// The version this ShellRS writes. A file of a later one is not opened; an
/// earlier one is copied aside, then brought up to it.
const SCHEMA_VERSION: i64 = 1;

/// The whole schema, as a new file gets it.
const SCHEMA: &str = "
CREATE TABLE conversations (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE,
    host_public_id TEXT,
    host_label TEXT NOT NULL,
    title TEXT NOT NULL,
    title_by_user INTEGER NOT NULL,
    provider_public_id TEXT,
    model TEXT NOT NULL,
    approval TEXT,
    run_in TEXT,
    created_ms INTEGER NOT NULL,
    updated_ms INTEGER NOT NULL,
    elided_before_seq INTEGER
) STRICT;
CREATE INDEX conversations_by_host ON conversations (host_public_id, updated_ms);
CREATE TABLE messages (
    conversation_id INTEGER NOT NULL REFERENCES conversations (id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    parts TEXT NOT NULL,
    raw TEXT,
    raw_source TEXT,
    stop TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    created_ms INTEGER NOT NULL,
    PRIMARY KEY (conversation_id, seq)
) STRICT;
";

/// The steps from each earlier version to the next, in order. Written once:
/// a step is history and is never edited.
const STEPS: [(i64, &str); 0] = [];

/// A conversation, without its messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationSummary {
    pub public_id: String,
    /// The saved host it is about; `None` for a temporary or external
    /// connection.
    pub host_public_id: Option<String>,
    /// The host's name or address when the conversation started, shown
    /// even after the host is deleted.
    pub host_label: String,
    /// Empty until the first message names it.
    pub title: String,
    pub title_by_user: bool,
    pub provider_public_id: Option<String>,
    pub model: String,
    /// The approval the conversation was given, apart from the settings'.
    pub approval: Option<Approval>,
    /// Where its commands run, apart from the settings'.
    pub run_in: Option<RunIn>,
    pub created_ms: i64,
    pub updated_ms: i64,
    /// Tool output in messages before this one is no longer sent.
    pub elided_before: Option<i64>,
}

/// What a save of a turn changes of its conversation.
pub(crate) struct TurnSaved<'a> {
    pub updated_ms: i64,
    pub title: Option<&'a str>,
    pub provider_public_id: Option<&'a str>,
    pub model: &'a str,
    pub elided_before: Option<i64>,
}

/// The conversations' database. Cloned handles share one connection.
#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open the file at `path`, creating it, and bring it to this version.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        restrict(path);
        Self::start(connection, Some(path))
    }

    /// A database in memory, for tests.
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::start(Connection::open_in_memory()?, None)
    }

    fn start(connection: Connection, path: Option<&Path>) -> Result<Self, StoreError> {
        // A deleted conversation is overwritten, not left in free pages.
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA secure_delete = ON;")?;
        migrate(&connection, path)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The conversations about a host, or every one, the latest first.
    pub fn conversations(
        &self,
        host_public_id: Option<&str>,
    ) -> Result<Vec<ConversationSummary>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(&format!(
            "SELECT {SUMMARY} FROM conversations WHERE ?1 IS NULL OR host_public_id = ?1 ORDER BY updated_ms DESC, id DESC"
        ))?;
        let rows = statement.query_map(params![host_public_id], summary)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn conversation(&self, public_id: &str) -> Result<Option<ConversationSummary>, StoreError> {
        let connection = self.lock();
        Ok(connection
            .query_row(
                &format!("SELECT {SUMMARY} FROM conversations WHERE public_id = ?1"),
                params![public_id],
                summary,
            )
            .optional()?)
    }

    /// A conversation's messages, in order.
    pub fn messages(&self, public_id: &str) -> Result<Vec<Message>, StoreError> {
        let connection = self.lock();
        let mut statement = connection.prepare(
            "SELECT m.seq, m.role, m.parts, m.raw, m.raw_source, m.stop, m.input_tokens, m.output_tokens, m.cache_read_tokens, m.cache_write_tokens, m.created_ms
             FROM messages m JOIN conversations c ON c.id = m.conversation_id
             WHERE c.public_id = ?1 ORDER BY m.seq",
        )?;
        let rows = statement.query_map(params![public_id], |row| {
            let parts: String = row.get(2)?;
            let raw: Option<String> = row.get(3)?;
            let raw_source: Option<String> = row.get(4)?;
            let stop: Option<String> = row.get(5)?;
            let usage: [Option<i64>; 4] = [row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?];
            let role: String = row.get(1)?;
            Ok(Message {
                seq: row.get(0)?,
                role: Role::from_key(&role).unwrap_or(Role::User),
                parts: serde_json::from_str::<Vec<serde_json::Value>>(&parts)
                    .unwrap_or_default()
                    .into_iter()
                    .map(Part::from_stored)
                    .collect(),
                raw: match (raw, raw_source) {
                    (Some(raw), Some(source)) => serde_json::from_str(&raw)
                        .ok()
                        .map(|content| Raw { source, content }),
                    _ => None,
                },
                stop: stop.as_deref().map(StopReason::from_key),
                usage: usage[0].map(|input| Usage {
                    input: input as u64,
                    output: usage[1].unwrap_or(0) as u64,
                    cache_read: usage[2].unwrap_or(0) as u64,
                    cache_write: usage[3].unwrap_or(0) as u64,
                }),
                created_ms: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// What all the requests of a conversation took.
    pub fn usage(&self, public_id: &str) -> Result<Usage, StoreError> {
        let mut total = Usage::default();
        for message in self.messages(public_id)? {
            if let Some(usage) = message.usage {
                total.add(&usage);
            }
        }
        Ok(total)
    }

    /// Save a conversation's first turn: it is written only once it has one.
    pub(crate) fn create(&self, conversation: &ConversationSummary) -> Result<(), StoreError> {
        self.lock().execute(
            "INSERT INTO conversations (public_id, host_public_id, host_label, title, title_by_user, provider_public_id, model, approval, run_in, created_ms, updated_ms, elided_before_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                conversation.public_id,
                conversation.host_public_id,
                conversation.host_label,
                conversation.title,
                conversation.title_by_user,
                conversation.provider_public_id,
                conversation.model,
                conversation.approval.map(Approval::key),
                conversation.run_in.map(RunIn::key),
                conversation.created_ms,
                conversation.updated_ms,
                conversation.elided_before,
            ],
        )?;
        Ok(())
    }

    /// Add a turn's messages to a conversation, and what the turn changed of
    /// it, in one transaction.
    pub(crate) fn append(
        &self,
        public_id: &str,
        messages: &[Message],
        saved: TurnSaved<'_>,
    ) -> Result<(), StoreError> {
        let mut connection = self.lock();
        let transaction = connection.transaction()?;
        let id: i64 = transaction.query_row(
            "SELECT id FROM conversations WHERE public_id = ?1",
            params![public_id],
            |row| row.get(0),
        )?;
        {
            let mut insert = transaction.prepare(
                "INSERT OR REPLACE INTO messages (conversation_id, seq, role, parts, raw, raw_source, stop, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, created_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for message in messages {
                let parts: Vec<_> = message.parts.iter().map(Part::to_stored).collect();
                let usage = message.usage;
                insert.execute(params![
                    id,
                    message.seq,
                    message.role.key(),
                    serde_json::Value::Array(parts).to_string(),
                    message.raw.as_ref().map(|raw| raw.content.to_string()),
                    message.raw.as_ref().map(|raw| raw.source.as_str()),
                    message.stop.map(StopReason::key),
                    usage.map(|usage| usage.input as i64),
                    usage.map(|usage| usage.output as i64),
                    usage.map(|usage| usage.cache_read as i64),
                    usage.map(|usage| usage.cache_write as i64),
                    message.created_ms,
                ])?;
            }
        }
        transaction.execute(
            "UPDATE conversations SET updated_ms = ?2, provider_public_id = ?3, model = ?4, elided_before_seq = ?5,
                 title = CASE WHEN title_by_user = 0 AND ?6 IS NOT NULL THEN ?6 ELSE title END
             WHERE id = ?1",
            params![
                id,
                saved.updated_ms,
                saved.provider_public_id,
                saved.model,
                saved.elided_before,
                saved.title,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Give a conversation the user's own name, which no later turn changes.
    pub fn rename(&self, public_id: &str, title: &str) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE conversations SET title = ?2, title_by_user = 1 WHERE public_id = ?1",
            params![public_id, title.trim()],
        )?;
        Ok(())
    }

    pub fn set_approval(
        &self,
        public_id: &str,
        approval: Option<Approval>,
    ) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE conversations SET approval = ?2 WHERE public_id = ?1",
            params![public_id, approval.map(Approval::key)],
        )?;
        Ok(())
    }

    pub fn set_run_in(&self, public_id: &str, run_in: Option<RunIn>) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE conversations SET run_in = ?2 WHERE public_id = ?1",
            params![public_id, run_in.map(RunIn::key)],
        )?;
        Ok(())
    }

    pub fn delete(&self, public_id: &str) -> Result<(), StoreError> {
        self.lock().execute(
            "DELETE FROM conversations WHERE public_id = ?1",
            params![public_id],
        )?;
        Ok(())
    }

    /// Delete every conversation, and leave nothing of them in the file.
    pub fn delete_all(&self) -> Result<(), StoreError> {
        let connection = self.lock();
        connection.execute_batch("DELETE FROM messages; DELETE FROM conversations;")?;
        connection.execute_batch("VACUUM;")?;
        Ok(())
    }
}

/// The columns [`summary`] reads, in its order.
const SUMMARY: &str = "public_id, host_public_id, host_label, title, title_by_user, provider_public_id, model, approval, run_in, created_ms, updated_ms, elided_before_seq";

fn summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationSummary> {
    let approval: Option<String> = row.get(7)?;
    let run_in: Option<String> = row.get(8)?;
    Ok(ConversationSummary {
        public_id: row.get(0)?,
        host_public_id: row.get(1)?,
        host_label: row.get(2)?,
        title: row.get(3)?,
        title_by_user: row.get(4)?,
        provider_public_id: row.get(5)?,
        model: row.get(6)?,
        approval: approval.as_deref().and_then(Approval::from_key),
        run_in: run_in.as_deref().and_then(RunIn::from_key),
        created_ms: row.get(9)?,
        updated_ms: row.get(10)?,
        elided_before: row.get(11)?,
    })
}

fn migrate(connection: &Connection, path: Option<&Path>) -> Result<(), StoreError> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::Newer { version });
    }
    if version == 0 {
        connection.execute_batch(&format!(
            "BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"
        ))?;
        return Ok(());
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if let Some(path) = path {
        back_up(connection, path, version);
    }
    for (from, step) in STEPS {
        if from >= version {
            connection.execute_batch(step)?;
        }
    }
    Ok(())
}

/// Copy the file aside before it is upgraded, as `assistant.db.v<n>.bak`.
/// A copy that fails does not stop the upgrade.
fn back_up(connection: &Connection, path: &Path, version: i64) {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".v{version}.bak"));
    let backup: PathBuf = path.with_file_name(name);
    let _ = std::fs::remove_file(&backup);
    let _ = connection.execute("VACUUM INTO ?1", params![backup.to_string_lossy()]);
    restrict(&backup);
}

/// Only this user may read the file: it holds what the terminals showed.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ResultStatus;

    fn conversation(public_id: &str, host: Option<&str>, at: i64) -> ConversationSummary {
        ConversationSummary {
            public_id: public_id.into(),
            host_public_id: host.map(Into::into),
            host_label: "web-01".into(),
            title: String::new(),
            title_by_user: false,
            provider_public_id: Some("p1".into()),
            model: "model-a".into(),
            approval: None,
            run_in: None,
            created_ms: at,
            updated_ms: at,
            elided_before: None,
        }
    }

    fn messages() -> Vec<Message> {
        vec![
            Message {
                seq: 0,
                role: Role::User,
                parts: vec![Part::Text {
                    text: "disk full?".into(),
                }],
                raw: None,
                stop: None,
                usage: None,
                created_ms: 10,
            },
            Message {
                seq: 1,
                role: Role::Assistant,
                parts: vec![Part::ToolCall {
                    id: "c1_0".into(),
                    provider_id: Some("toolu_9".into()),
                    name: "run_command".into(),
                    input: serde_json::json!({"command": "df -h"}),
                }],
                raw: Some(Raw {
                    source: "anthropic|x|m".into(),
                    content: serde_json::json!([{"type": "thinking", "thinking": "", "signature": "sig"}]),
                }),
                stop: Some(StopReason::ToolUse),
                usage: Some(Usage {
                    input: 100,
                    output: 20,
                    cache_read: 5,
                    cache_write: 1,
                }),
                created_ms: 11,
            },
            Message {
                seq: 2,
                role: Role::ToolResults,
                parts: vec![Part::ToolResult {
                    call_id: "c1_0".into(),
                    status: ResultStatus::Ok,
                    output: "98%".into(),
                    exit: Some(0),
                    total_bytes: Some(3),
                    mode: Some(RunIn::Background),
                }],
                raw: None,
                stop: None,
                usage: None,
                created_ms: 12,
            },
        ]
    }

    fn saved(at: i64, title: Option<&str>) -> TurnSaved<'_> {
        TurnSaved {
            updated_ms: at,
            title,
            provider_public_id: Some("p1"),
            model: "model-a",
            elided_before: None,
        }
    }

    #[test]
    fn a_turn_reads_back_as_it_was_saved() {
        let store = Store::in_memory().unwrap();
        store.create(&conversation("aaaa", Some("h1"), 1)).unwrap();
        store
            .append("aaaa", &messages(), saved(20, Some("disk full?")))
            .unwrap();

        assert_eq!(store.messages("aaaa").unwrap(), messages());
        let summary = store.conversation("aaaa").unwrap().unwrap();
        assert_eq!(summary.title, "disk full?");
        assert_eq!(summary.updated_ms, 20);
        assert_eq!(store.usage("aaaa").unwrap().total(), 126);
    }

    #[test]
    fn a_title_the_user_gave_stays() {
        let store = Store::in_memory().unwrap();
        store.create(&conversation("aaaa", None, 1)).unwrap();
        store.rename("aaaa", " Disks ").unwrap();
        store
            .append("aaaa", &messages(), saved(2, Some("disk full?")))
            .unwrap();
        assert_eq!(store.conversation("aaaa").unwrap().unwrap().title, "Disks");
    }

    #[test]
    fn conversations_are_listed_by_host_latest_first() {
        let store = Store::in_memory().unwrap();
        store.create(&conversation("old", Some("h1"), 1)).unwrap();
        store.create(&conversation("new", Some("h1"), 5)).unwrap();
        store.create(&conversation("other", Some("h2"), 9)).unwrap();
        let ids = |host| {
            store
                .conversations(host)
                .unwrap()
                .into_iter()
                .map(|summary| summary.public_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(Some("h1")), ["new", "old"]);
        assert_eq!(ids(None), ["other", "new", "old"]);
    }

    #[test]
    fn deleting_takes_the_messages_with_it() {
        let store = Store::in_memory().unwrap();
        store.create(&conversation("aaaa", None, 1)).unwrap();
        store.append("aaaa", &messages(), saved(2, None)).unwrap();
        store.create(&conversation("bbbb", None, 1)).unwrap();
        store.delete("aaaa").unwrap();
        assert!(store.messages("aaaa").unwrap().is_empty());
        assert!(store.conversation("bbbb").unwrap().is_some());
        store.delete_all().unwrap();
        assert!(store.conversations(None).unwrap().is_empty());
    }

    #[test]
    fn a_file_of_a_newer_shellrs_is_not_opened() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("assistant.db");
        Connection::open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        assert!(matches!(
            Store::open(&path),
            Err(StoreError::Newer { version: 99 })
        ));
    }

    #[test]
    fn a_file_opens_again_and_only_its_user_reads_it() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("assistant.db");
        {
            let store = Store::open(&path).unwrap();
            store.create(&conversation("aaaa", None, 1)).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert!(store.conversation("aaaa").unwrap().is_some());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// Secrets live in the keychain: no column here is named for one.
    #[test]
    fn no_column_is_for_a_secret() {
        let store = Store::in_memory().unwrap();
        let connection = store.lock();
        for table in ["conversations", "messages"] {
            let mut statement = connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let names: Vec<String> = statement
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for name in names {
                for word in ["password", "passphrase", "secret", "api_key", "token_value"] {
                    assert!(!name.contains(word), "{table}.{name}");
                }
            }
        }
    }
}
