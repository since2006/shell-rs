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

use gpui_shared_string::SharedString;
use rusqlite::{Connection, params};

use super::{
    AuthKind, BookmarkSide, Credential, CredentialDraft, CredentialId, CredentialKind, DataBits,
    FlowControl, ForwardDraft, ForwardEndpoint, ForwardId, ForwardKind, ForwardRule, GroupDraft,
    GroupId, Host, HostDraft, HostGroup, HostId, HostOs, Parity, Protocol, ProxyKind,
    ProxySettings, PublicId, Route, SavedTerminal, SavedTerminalId, SavedTerminalTab,
    SerialSettings, Snippet, SnippetCategory, SnippetCategoryId, SnippetDraft, SnippetId,
    SnippetScope, StopBits, TelnetSettings, TerminalGroup, TerminalGroupId,
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
/// command snippets, 15 the serial hosts, 16 the Telnet hosts, 17 each
/// host's right sidebar, 18 the saved local terminals, 19 their titles, 20
/// what sync needs.
const SCHEMA_VERSION: i64 = 20;

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
///   - `os` and `last_connected_at` are written by connections, and
///     `sidebar_tool` (the right sidebar's tool as the host's terminals
///     last had it, the app's `ToolKind::key`; `NULL` while hidden) by the sidebar;
///     none of them by the host form.
///   - `protocol` is `ssh`, `telnet` or `serial`. A serial host's device is
///     its `address` and its line is in the five `serial_*` columns, set
///     exactly for a serial host; the login and route columns keep what a
///     new SSH host would have, unused. A Telnet host uses the login and
///     route columns as an SSH host does (`no-password` is no 自动登录), and
///     the prompts its 自动登录 waits for are in the two `telnet_*` columns,
///     `NULL` for the usual ones and always for other hosts.
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
/// - **terminal_groups** are 项目's groups, which do not nest, and take
///   their **saved_terminals** with them; a terminal of no group is at the
///   top. `expanded` is as a host group's. A terminal `renamed` by hand
///   keeps its name over the titles its shell gives it; `title` is the
///   last of those, `NULL` before one. A saved terminal's `directory` is
///   the folder of this computer its shell was last in, `NULL` before it
///   said.
/// - **saved_terminal_tabs** lists the saved terminals open when ShellRS
///   last had them, in tab order, rewritten whole: the next launch opens
///   them again. They go with their terminal; one at most is `front`.
/// - For sync, every record it carries has a global id and `edited_ms`, the
///   time of the user's last edit (0 for one sync created). A host's id is
///   its `public_id`, a credential's its `keychain_id`, a bookmark's is made
///   from its host and path; the other tables have a `sync_id`. A key
///   credential that came by sync has no `key_path` until its key does.
///   **secret_edits** keeps when each secret was last edited, by its sync
///   key, never the secret; **sync_state** is the sync's one blob of state,
///   whose secrets are sealed; **sync_pending** names the secrets a round
///   brought that are not written yet, by their sync key.
///
/// A column added later goes after the table's last column here and is
/// added to existing files with `ALTER TABLE ADD COLUMN`, which puts it in
/// the same place (`a_column_added_later_matches_the_schema`). A CHECK
/// cannot be changed that way: the table is made again (see `add_telnet`).
const SCHEMA: &str = "\
CREATE TABLE groups (
    id         INTEGER PRIMARY KEY,
    parent_id  INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (name <> ''),
    sort_order INTEGER NOT NULL DEFAULT 0,
    expanded   INTEGER NOT NULL DEFAULT 1 CHECK (expanded IN (0, 1)),
    sync_id    TEXT NOT NULL DEFAULT '',
    edited_ms  INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX groups_parent_id ON groups(parent_id);
CREATE UNIQUE INDEX groups_sync_id ON groups(sync_id);
CREATE TABLE credentials (
    id          INTEGER PRIMARY KEY,
    keychain_id TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL CHECK (name <> ''),
    kind        TEXT NOT NULL CHECK (kind IN ('password', 'key', 'agent')),
    username    TEXT NOT NULL,
    key_path    TEXT,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    edited_ms   INTEGER NOT NULL DEFAULT 0,
    CHECK (kind = 'key' OR key_path IS NULL)
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
    protocol          TEXT NOT NULL DEFAULT 'ssh' CHECK (protocol IN ('ssh', 'serial', 'telnet')),
    serial_baud       INTEGER CHECK (serial_baud > 0)
        CHECK ((protocol = 'serial') = (serial_baud IS NOT NULL)),
    serial_data_bits  INTEGER CHECK (serial_data_bits BETWEEN 5 AND 8)
        CHECK ((protocol = 'serial') = (serial_data_bits IS NOT NULL)),
    serial_parity     TEXT CHECK (serial_parity IN ('none', 'odd', 'even'))
        CHECK ((protocol = 'serial') = (serial_parity IS NOT NULL)),
    serial_stop_bits  INTEGER CHECK (serial_stop_bits IN (1, 2))
        CHECK ((protocol = 'serial') = (serial_stop_bits IS NOT NULL)),
    serial_flow       TEXT CHECK (serial_flow IN ('none', 'rts-cts', 'xon-xoff'))
        CHECK ((protocol = 'serial') = (serial_flow IS NOT NULL)),
    telnet_user_prompt TEXT CHECK (telnet_user_prompt <> '')
        CHECK (protocol = 'telnet' OR telnet_user_prompt IS NULL),
    telnet_pass_prompt TEXT CHECK (telnet_pass_prompt <> '')
        CHECK (protocol = 'telnet' OR telnet_pass_prompt IS NULL),
    sidebar_tool      TEXT,
    edited_ms         INTEGER NOT NULL DEFAULT 0,
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
    edited_ms  INTEGER NOT NULL DEFAULT 0,
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
    sync_id     TEXT NOT NULL DEFAULT '',
    edited_ms   INTEGER NOT NULL DEFAULT 0,
    CHECK ((kind = 'dynamic' AND target_host IS NULL AND target_port IS NULL)
        OR (kind <> 'dynamic' AND target_host IS NOT NULL
            AND target_port BETWEEN 1 AND 65535))
) STRICT;
CREATE INDEX forwards_host_id ON forwards(host_id);
CREATE UNIQUE INDEX forwards_sync_id ON forwards(sync_id);
CREATE TABLE snippet_categories (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL CHECK (name <> ''),
    sync_id   TEXT NOT NULL DEFAULT '',
    edited_ms INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE UNIQUE INDEX snippet_categories_sync_id ON snippet_categories(sync_id);
CREATE TABLE snippets (
    id             INTEGER PRIMARY KEY,
    category_id    INTEGER REFERENCES snippet_categories(id) ON DELETE CASCADE,
    name           TEXT NOT NULL CHECK (name <> ''),
    command        TEXT NOT NULL CHECK (command <> ''),
    scope_group_id INTEGER REFERENCES groups(id) ON DELETE CASCADE,
    scope_host_id  INTEGER REFERENCES hosts(id) ON DELETE CASCADE,
    run_on_click   INTEGER NOT NULL DEFAULT 0 CHECK (run_on_click IN (0, 1)),
    sync_id        TEXT NOT NULL DEFAULT '',
    edited_ms      INTEGER NOT NULL DEFAULT 0,
    CHECK (scope_group_id IS NULL OR scope_host_id IS NULL)
) STRICT;
CREATE INDEX snippets_category_id ON snippets(category_id);
CREATE INDEX snippets_scope_group_id ON snippets(scope_group_id);
CREATE INDEX snippets_scope_host_id ON snippets(scope_host_id);
CREATE UNIQUE INDEX snippets_sync_id ON snippets(sync_id);
CREATE TABLE terminal_groups (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL CHECK (name <> ''),
    sort_order INTEGER NOT NULL DEFAULT 0,
    expanded   INTEGER NOT NULL DEFAULT 1 CHECK (expanded IN (0, 1))
) STRICT;
CREATE TABLE saved_terminals (
    id         INTEGER PRIMARY KEY,
    group_id   INTEGER REFERENCES terminal_groups(id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (name <> ''),
    renamed    INTEGER NOT NULL DEFAULT 0 CHECK (renamed IN (0, 1)),
    directory  TEXT CHECK (directory <> ''),
    sort_order INTEGER NOT NULL DEFAULT 0,
    title      TEXT CHECK (title <> '')
) STRICT;
CREATE INDEX saved_terminals_group_id ON saved_terminals(group_id);
CREATE TABLE saved_terminal_tabs (
    position    INTEGER PRIMARY KEY CHECK (position >= 0),
    terminal_id INTEGER NOT NULL UNIQUE REFERENCES saved_terminals(id) ON DELETE CASCADE,
    front       INTEGER NOT NULL DEFAULT 0 CHECK (front IN (0, 1))
) STRICT;
CREATE UNIQUE INDEX saved_terminal_tabs_front ON saved_terminal_tabs(front) WHERE front = 1;
CREATE TABLE secret_edits (
    kind      TEXT NOT NULL,
    id        TEXT NOT NULL,
    edited_ms INTEGER NOT NULL,
    PRIMARY KEY (kind, id)
) STRICT, WITHOUT ROWID;
CREATE TABLE sync_state (
    one   INTEGER PRIMARY KEY CHECK (one = 1),
    state BLOB NOT NULL
) STRICT;
CREATE TABLE sync_pending (
    kind TEXT NOT NULL,
    id   TEXT NOT NULL,
    PRIMARY KEY (kind, id)
) STRICT, WITHOUT ROWID;";

/// One step from a version to the next: one transaction that ends by
/// recording the version it reached.
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
const STEPS: [(i64, Step); 7] = [
    (
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
    ),
    (
        14,
        // The serial hosts.
        Step::Sql(
            "BEGIN;
ALTER TABLE hosts ADD COLUMN protocol          TEXT NOT NULL DEFAULT 'ssh' CHECK (protocol IN ('ssh', 'serial'));
ALTER TABLE hosts ADD COLUMN serial_baud       INTEGER CHECK (serial_baud > 0)
        CHECK ((protocol = 'serial') = (serial_baud IS NOT NULL));
ALTER TABLE hosts ADD COLUMN serial_data_bits  INTEGER CHECK (serial_data_bits BETWEEN 5 AND 8)
        CHECK ((protocol = 'serial') = (serial_data_bits IS NOT NULL));
ALTER TABLE hosts ADD COLUMN serial_parity     TEXT CHECK (serial_parity IN ('none', 'odd', 'even'))
        CHECK ((protocol = 'serial') = (serial_parity IS NOT NULL));
ALTER TABLE hosts ADD COLUMN serial_stop_bits  INTEGER CHECK (serial_stop_bits IN (1, 2))
        CHECK ((protocol = 'serial') = (serial_stop_bits IS NOT NULL));
ALTER TABLE hosts ADD COLUMN serial_flow       TEXT CHECK (serial_flow IN ('none', 'rts-cts', 'xon-xoff'))
        CHECK ((protocol = 'serial') = (serial_flow IS NOT NULL));
PRAGMA user_version = 15;
COMMIT;",
        ),
    ),
    // The Telnet hosts.
    (15, Step::Code(add_telnet)),
    (
        16,
        // Each host's right sidebar.
        Step::Sql(
            "BEGIN;
ALTER TABLE hosts ADD COLUMN sidebar_tool      TEXT;
PRAGMA user_version = 17;
COMMIT;",
        ),
    ),
    (
        17,
        // The saved local terminals.
        Step::Sql(
            "BEGIN;
CREATE TABLE terminal_groups (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL CHECK (name <> ''),
    sort_order INTEGER NOT NULL DEFAULT 0,
    expanded   INTEGER NOT NULL DEFAULT 1 CHECK (expanded IN (0, 1))
) STRICT;
CREATE TABLE saved_terminals (
    id         INTEGER PRIMARY KEY,
    group_id   INTEGER REFERENCES terminal_groups(id) ON DELETE CASCADE,
    name       TEXT NOT NULL CHECK (name <> ''),
    renamed    INTEGER NOT NULL DEFAULT 0 CHECK (renamed IN (0, 1)),
    directory  TEXT CHECK (directory <> ''),
    sort_order INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX saved_terminals_group_id ON saved_terminals(group_id);
CREATE TABLE saved_terminal_tabs (
    position    INTEGER PRIMARY KEY CHECK (position >= 0),
    terminal_id INTEGER NOT NULL UNIQUE REFERENCES saved_terminals(id) ON DELETE CASCADE,
    front       INTEGER NOT NULL DEFAULT 0 CHECK (front IN (0, 1))
) STRICT;
CREATE UNIQUE INDEX saved_terminal_tabs_front ON saved_terminal_tabs(front) WHERE front = 1;
PRAGMA user_version = 18;
COMMIT;",
        ),
    ),
    (
        18,
        // A saved terminal's last title.
        Step::Sql(
            "BEGIN;
ALTER TABLE saved_terminals ADD COLUMN title      TEXT CHECK (title <> '');
PRAGMA user_version = 19;
COMMIT;",
        ),
    ),
    // What sync needs.
    (19, Step::Code(add_sync)),
];

/// Version 16, the Telnet hosts: `protocol` takes `telnet`, and the two
/// `telnet_*` columns come after the serial ones. SQLite cannot change the
/// CHECK on `protocol`, so `hosts` is made again and its rows copied in,
/// ids and all. Not with `ALTER TABLE … RENAME`: that would store the new
/// table's name quoted, unlike `SCHEMA`, and point the other tables'
/// foreign keys at the old one.
fn add_telnet(connection: &Connection) -> rusqlite::Result<()> {
    // With foreign keys on, dropping `hosts` would take every jump host,
    // bookmark, forward and host snippet with it. The pragma does nothing
    // inside a transaction, hence here.
    connection.pragma_update(None, "foreign_keys", false)?;
    let result = connection.execute_batch(TELNET_HOSTS);
    if result.is_err() {
        let _ = connection.execute_batch("ROLLBACK");
    }
    connection.pragma_update(None, "foreign_keys", true)?;
    result
}

const TELNET_HOSTS: &str = "BEGIN;
CREATE TEMP TABLE hosts_15 AS SELECT * FROM main.hosts;
DROP TABLE main.hosts;
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
    protocol          TEXT NOT NULL DEFAULT 'ssh' CHECK (protocol IN ('ssh', 'serial', 'telnet')),
    serial_baud       INTEGER CHECK (serial_baud > 0)
        CHECK ((protocol = 'serial') = (serial_baud IS NOT NULL)),
    serial_data_bits  INTEGER CHECK (serial_data_bits BETWEEN 5 AND 8)
        CHECK ((protocol = 'serial') = (serial_data_bits IS NOT NULL)),
    serial_parity     TEXT CHECK (serial_parity IN ('none', 'odd', 'even'))
        CHECK ((protocol = 'serial') = (serial_parity IS NOT NULL)),
    serial_stop_bits  INTEGER CHECK (serial_stop_bits IN (1, 2))
        CHECK ((protocol = 'serial') = (serial_stop_bits IS NOT NULL)),
    serial_flow       TEXT CHECK (serial_flow IN ('none', 'rts-cts', 'xon-xoff'))
        CHECK ((protocol = 'serial') = (serial_flow IS NOT NULL)),
    telnet_user_prompt TEXT CHECK (telnet_user_prompt <> '')
        CHECK (protocol = 'telnet' OR telnet_user_prompt IS NULL),
    telnet_pass_prompt TEXT CHECK (telnet_pass_prompt <> '')
        CHECK (protocol = 'telnet' OR telnet_pass_prompt IS NULL),
    CHECK ((auth = 'credential') = (credential_id IS NOT NULL)),
    CHECK ((auth = 'credential') = (username IS NULL)),
    CHECK ((route = 'proxy') = (proxy_kind IS NOT NULL)),
    CHECK ((proxy_kind IS NULL) = (proxy_host IS NULL)
        AND (proxy_kind IS NULL) = (proxy_port IS NULL)),
    CHECK (proxy_kind IS NOT NULL OR proxy_username IS NULL)
) STRICT;
INSERT INTO hosts (id, public_id, group_id, sort_order, name, address, port, auth, username,
    credential_id, route, proxy_kind, proxy_host, proxy_port, proxy_username, notes, os,
    last_connected_at, protocol, serial_baud, serial_data_bits, serial_parity, serial_stop_bits,
    serial_flow)
SELECT id, public_id, group_id, sort_order, name, address, port, auth, username,
    credential_id, route, proxy_kind, proxy_host, proxy_port, proxy_username, notes, os,
    last_connected_at, protocol, serial_baud, serial_data_bits, serial_parity, serial_stop_bits,
    serial_flow
FROM hosts_15;
DROP TABLE hosts_15;
CREATE INDEX hosts_group_id ON hosts(group_id);
CREATE INDEX hosts_credential_id ON hosts(credential_id);
PRAGMA user_version = 16;
COMMIT;";

/// Version 20, what sync needs: global ids and edit times, the tables of
/// secret edit times and sync state, and a key credential allowed no key
/// file, which takes making `credentials` again as `add_telnet` makes
/// `hosts`. Every row there was is taken as edited now.
fn add_sync(connection: &Connection) -> rusqlite::Result<()> {
    connection.pragma_update(None, "foreign_keys", false)?;
    let result = connection.execute_batch(SYNC_COLUMNS).and_then(|()| {
        for table in SYNC_ID_TABLES {
            fill_sync_ids(connection, table)?;
        }
        let now = to_sql(now_millis());
        for table in [
            "groups",
            "credentials",
            "hosts",
            "bookmarks",
            "forwards",
            "snippet_categories",
            "snippets",
        ] {
            connection.execute(&format!("UPDATE {table} SET edited_ms = ?1"), params![now])?;
        }
        connection.execute_batch(SYNC_INDEXES)
    });
    if result.is_err() {
        let _ = connection.execute_batch("ROLLBACK");
    }
    connection.pragma_update(None, "foreign_keys", true)?;
    result
}

/// The tables whose records have a `sync_id` of their own.
const SYNC_ID_TABLES: [&str; 4] = ["groups", "forwards", "snippet_categories", "snippets"];

const SYNC_COLUMNS: &str = "BEGIN;
ALTER TABLE groups ADD COLUMN sync_id    TEXT NOT NULL DEFAULT '';
ALTER TABLE groups ADD COLUMN edited_ms  INTEGER NOT NULL DEFAULT 0;
CREATE TEMP TABLE credentials_19 AS SELECT * FROM main.credentials;
DROP TABLE main.credentials;
CREATE TABLE credentials (
    id          INTEGER PRIMARY KEY,
    keychain_id TEXT NOT NULL UNIQUE,
    name        TEXT NOT NULL CHECK (name <> ''),
    kind        TEXT NOT NULL CHECK (kind IN ('password', 'key', 'agent')),
    username    TEXT NOT NULL,
    key_path    TEXT,
    sort_order  INTEGER NOT NULL DEFAULT 0,
    edited_ms   INTEGER NOT NULL DEFAULT 0,
    CHECK (kind = 'key' OR key_path IS NULL)
) STRICT;
INSERT INTO credentials (id, keychain_id, name, kind, username, key_path, sort_order)
SELECT id, keychain_id, name, kind, username, key_path, sort_order FROM credentials_19;
DROP TABLE credentials_19;
ALTER TABLE hosts ADD COLUMN edited_ms         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE bookmarks ADD COLUMN edited_ms  INTEGER NOT NULL DEFAULT 0;
ALTER TABLE forwards ADD COLUMN sync_id     TEXT NOT NULL DEFAULT '';
ALTER TABLE forwards ADD COLUMN edited_ms   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE snippet_categories ADD COLUMN sync_id   TEXT NOT NULL DEFAULT '';
ALTER TABLE snippet_categories ADD COLUMN edited_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE snippets ADD COLUMN sync_id        TEXT NOT NULL DEFAULT '';
ALTER TABLE snippets ADD COLUMN edited_ms      INTEGER NOT NULL DEFAULT 0;
CREATE TABLE secret_edits (
    kind      TEXT NOT NULL,
    id        TEXT NOT NULL,
    edited_ms INTEGER NOT NULL,
    PRIMARY KEY (kind, id)
) STRICT, WITHOUT ROWID;
CREATE TABLE sync_state (
    one   INTEGER PRIMARY KEY CHECK (one = 1),
    state BLOB NOT NULL
) STRICT;
CREATE TABLE sync_pending (
    kind TEXT NOT NULL,
    id   TEXT NOT NULL,
    PRIMARY KEY (kind, id)
) STRICT, WITHOUT ROWID;";

const SYNC_INDEXES: &str = "\
CREATE UNIQUE INDEX groups_sync_id ON groups(sync_id);
CREATE UNIQUE INDEX forwards_sync_id ON forwards(sync_id);
CREATE UNIQUE INDEX snippet_categories_sync_id ON snippet_categories(sync_id);
CREATE UNIQUE INDEX snippets_sync_id ON snippets(sync_id);
PRAGMA user_version = 20;
COMMIT;";

/// Give every row of `table` without a `sync_id` one: all of them as the
/// column is added, and later any row added by hand.
fn fill_sync_ids(connection: &Connection, table: &str) -> rusqlite::Result<()> {
    let mut taken = HashSet::new();
    let mut missing = Vec::new();
    {
        let mut rows = connection.prepare(&format!("SELECT id, sync_id FROM {table}"))?;
        let rows = rows.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, sync_id) = row?;
            if sync_id.is_empty() {
                missing.push(id);
            } else {
                taken.insert(PublicId::from_stored(sync_id));
            }
        }
    }
    let mut update =
        connection.prepare(&format!("UPDATE {table} SET sync_id = ?2 WHERE id = ?1"))?;
    for id in missing {
        let sync_id = PublicId::generate_unused(|candidate| taken.contains(candidate));
        update.execute(params![id, sync_id.as_str()])?;
        taken.insert(sync_id);
    }
    Ok(())
}

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
    /// SFTP bookmarks in their order, each with its `sort_order` and when
    /// it was last edited.
    pub bookmarks: Vec<(HostId, BookmarkSide, String, i64, u64)>,
    /// Port-forwarding rules in the order the forward list shows them.
    pub forwards: Vec<ForwardRule>,
    /// Credentials in the order the credential list shows them.
    pub credentials: Vec<Credential>,
    /// Snippet categories in id order.
    pub snippet_categories: Vec<SnippetCategory>,
    /// Snippets in id order.
    pub snippets: Vec<Snippet>,
    /// 项目's groups in id order.
    pub terminal_groups: Vec<TerminalGroup>,
    /// Saved local terminals in id order.
    pub saved_terminals: Vec<SavedTerminal>,
    /// The saved terminals open when ShellRS last had them, in tab order.
    pub saved_terminal_tabs: Vec<SavedTerminalTab>,
    /// When each secret was last edited, by its sync record's kind and id.
    pub secret_edits: HashMap<(String, String), u64>,
    /// The secrets a round brought and that are not written yet, likewise.
    pub sync_pending: Vec<(String, String)>,
}

pub struct HostDatabase {
    connection: Connection,
}

/// Why the database did not open. The first two leave the file as it is.
#[derive(Debug)]
pub enum OpenError {
    /// A newer ShellRS wrote it, at schema `version`; this one knows up to
    /// `supported`.
    Newer {
        version: i64,
        supported: i64,
    },
    /// An early development build wrote it, before the oldest version the
    /// migrations reach back to.
    TooOld {
        version: i64,
    },
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for OpenError {
    fn from(error: rusqlite::Error) -> Self {
        OpenError::Sqlite(error)
    }
}

/// For logs; the app words it for the user.
impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Newer { version, supported } => write!(
                f,
                "the database is from a newer ShellRS (schema {version}, this one knows up to {supported})"
            ),
            OpenError::TooOld { version } => write!(
                f,
                "the database is from an early development build (schema {version})"
            ),
            OpenError::Sqlite(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for OpenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OpenError::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl HostDatabase {
    /// Open the database at `path`, creating it if it is not there yet.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        Self::prepare(Connection::open(path)?)
    }

    /// An anonymous database that lives only as long as this value.
    #[cfg(any(test, feature = "testing"))]
    pub fn in_memory() -> Result<Self, OpenError> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut connection: Connection) -> Result<Self, OpenError> {
        // Cascading deletes are how removing a group removes its subtree, and
        // SQLite only honours the foreign keys when asked, per connection.
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.execute_batch("PRAGMA journal_mode = WAL;")?;
        migrate(&connection)?;
        fill_missing_public_ids(&mut connection)?;
        fill_missing_sync_ids(&mut connection)?;
        Ok(Self { connection })
    }

    /// Start a batch: every write until [`Self::end_batch`] lands in one
    /// transaction, so a burst of them (an import) pays for one commit
    /// rather than one each.
    pub fn begin_batch(&self) -> rusqlite::Result<()> {
        self.connection.execute_batch("BEGIN")
    }

    /// Commit the batch. A commit that fails rolls it back, so the
    /// connection is back to committing each write on its own.
    pub fn end_batch(&self) -> rusqlite::Result<()> {
        self.connection.execute_batch("COMMIT").inspect_err(|_| {
            let _ = self.connection.execute_batch("ROLLBACK");
        })
    }

    /// One write that lands whole or not at all: in a transaction of its
    /// own, or, inside a batch, in a savepoint, which costs no commit.
    fn atomically<T>(
        &self,
        write: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<T> {
        if self.connection.is_autocommit() {
            let transaction = self.connection.unchecked_transaction()?;
            let value = write(&transaction)?;
            transaction.commit()?;
            return Ok(value);
        }
        self.connection.execute_batch("SAVEPOINT write")?;
        match write(&self.connection) {
            Ok(value) => {
                self.connection.execute_batch("RELEASE write")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self
                    .connection
                    .execute_batch("ROLLBACK TO write; RELEASE write");
                Err(error)
            }
        }
    }

    pub fn load(&self) -> rusqlite::Result<StoredData> {
        let groups = self
            .connection
            .prepare(
                "SELECT id, name, parent_id, sort_order, expanded, sync_id, edited_ms \
                 FROM groups ORDER BY id",
            )?
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
                group.sync_id = PublicId::from_stored(row.get(5)?);
                group.edited_ms = from_sql(row.get(6)?);
                Ok(group)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut hosts = self
            .connection
            .prepare(
                "SELECT id, public_id, group_id, sort_order, name, address, port, auth, username, \
                 credential_id, route, proxy_kind, proxy_host, proxy_port, proxy_username, \
                 notes, os, protocol, serial_baud, serial_data_bits, serial_parity, \
                 serial_stop_bits, serial_flow, telnet_user_prompt, telnet_pass_prompt, \
                 sidebar_tool, edited_ms \
                 FROM hosts ORDER BY id",
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
                let protocol: String = row.get(17)?;
                let serial = SerialColumns {
                    baud: row.get(18)?,
                    data_bits: row.get(19)?,
                    parity: row.get(20)?,
                    stop_bits: row.get(21)?,
                    flow: row.get(22)?,
                };
                let telnet = TelnetSettings {
                    user_prompt: row.get::<_, Option<String>>(23)?.unwrap_or_default().into(),
                    password_prompt: row.get::<_, Option<String>>(24)?.unwrap_or_default().into(),
                };
                let sidebar_tool: Option<String> = row.get(25)?;
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
                host.sidebar_tool = sidebar_tool.map(SharedString::from);
                host.edited_ms = from_sql(row.get(26)?);
                match protocol.as_str() {
                    "serial" => host.protocol = Protocol::Serial(serial.settings()),
                    "telnet" => host.protocol = Protocol::Telnet(telnet),
                    _ => {}
                }
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
                "SELECT host_id, side, path, sort_order, edited_ms FROM bookmarks \
                 ORDER BY sort_order, host_id, side, path",
            )?
            .query_map([], |row| {
                let host: i64 = row.get(0)?;
                let side: String = row.get(1)?;
                Ok((
                    HostId(from_sql(host)),
                    BookmarkSide::from_stored(&side),
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    from_sql(row.get(4)?),
                ))
            })?
            .filter_map(|row| match row {
                Ok((host, Some(side), path, order, edited)) => {
                    Some(Ok((host, side, path, order, edited)))
                }
                Ok((_, None, ..)) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let forwards = self
            .connection
            .prepare(
                "SELECT id, host_id, name, kind, bind_host, bind_port, target_host, \
                 target_port, auto_start, sort_order, sync_id, edited_ms FROM forwards \
                 ORDER BY sort_order, id",
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
                let sync_id: String = row.get(10)?;
                let edited_ms: i64 = row.get(11)?;
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
                    rule.sync_id = PublicId::from_stored(sync_id);
                    rule.edited_ms = from_sql(edited_ms);
                    rule
                }))
            })?
            .filter_map(Result::transpose)
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let credentials = self
            .connection
            .prepare(
                "SELECT id, keychain_id, name, kind, username, key_path, sort_order, edited_ms \
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
                let edited_ms: i64 = row.get(7)?;
                // The table's CHECK admits only the kinds this build knows.
                Ok(CredentialKind::from_stored(&kind).map(|kind| {
                    let mut draft = CredentialDraft::new(name, kind, user);
                    draft.key_path = key_path.map(Into::into);
                    let mut credential = Credential::new(CredentialId(from_sql(id)), draft);
                    credential.keychain_id = PublicId::from_stored(keychain_id);
                    credential.sort_order = sort_order;
                    credential.edited_ms = from_sql(edited_ms);
                    credential
                }))
            })?
            .filter_map(Result::transpose)
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let snippet_categories = self
            .connection
            .prepare("SELECT id, name, sync_id, edited_ms FROM snippet_categories ORDER BY id")?
            .query_map([], |row| {
                Ok(SnippetCategory {
                    id: SnippetCategoryId(from_sql(row.get(0)?)),
                    name: row.get::<_, String>(1)?.into(),
                    sync_id: PublicId::from_stored(row.get(2)?),
                    edited_ms: from_sql(row.get(3)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let snippets = self
            .connection
            .prepare(
                "SELECT id, category_id, name, command, scope_group_id, scope_host_id, \
                 run_on_click, sync_id, edited_ms FROM snippets ORDER BY id",
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
                let mut snippet = Snippet::new(SnippetId(from_sql(id)), draft);
                snippet.sync_id = PublicId::from_stored(row.get(7)?);
                snippet.edited_ms = from_sql(row.get(8)?);
                Ok(snippet)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let terminal_groups = self
            .connection
            .prepare("SELECT id, name, sort_order, expanded FROM terminal_groups ORDER BY id")?
            .query_map([], |row| {
                let name: String = row.get(1)?;
                Ok(TerminalGroup {
                    id: TerminalGroupId(from_sql(row.get(0)?)),
                    name: name.into(),
                    sort_order: row.get(2)?,
                    expanded: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let saved_terminals = self
            .connection
            .prepare(
                "SELECT id, group_id, name, renamed, title, directory, sort_order \
                 FROM saved_terminals ORDER BY id",
            )?
            .query_map([], |row| {
                let name: String = row.get(2)?;
                Ok(SavedTerminal {
                    id: SavedTerminalId(from_sql(row.get(0)?)),
                    group: row
                        .get::<_, Option<i64>>(1)?
                        .map(|id| TerminalGroupId(from_sql(id))),
                    name: name.into(),
                    renamed: row.get(3)?,
                    title: row.get::<_, Option<String>>(4)?.map(Into::into),
                    directory: row.get(5)?,
                    sort_order: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let saved_terminal_tabs = self
            .connection
            .prepare("SELECT terminal_id, front FROM saved_terminal_tabs ORDER BY position")?
            .query_map([], |row| {
                Ok(SavedTerminalTab {
                    terminal: SavedTerminalId(from_sql(row.get(0)?)),
                    front: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let secret_edits = self
            .connection
            .prepare("SELECT kind, id, edited_ms FROM secret_edits")?
            .query_map([], |row| {
                Ok(((row.get(0)?, row.get(1)?), from_sql(row.get(2)?)))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        let sync_pending = self
            .connection
            .prepare("SELECT kind, id FROM sync_pending ORDER BY kind, id")?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
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
            terminal_groups,
            saved_terminals,
            saved_terminal_tabs,
            secret_edits,
            sync_pending,
        })
    }

    /// Run `write` in one transaction that lands whole or not at all: the
    /// first write that fails rolls back every one before it.
    pub fn all_or_nothing(
        &self,
        write: impl FnOnce(&Self) -> rusqlite::Result<()>,
    ) -> rusqlite::Result<()> {
        self.connection.execute_batch("BEGIN")?;
        match write(self).and_then(|()| self.connection.execute_batch("COMMIT")) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Inside [`Self::all_or_nothing`], check the foreign keys only at the
    /// commit: a round brings a host before the one it jumps through.
    pub fn defer_foreign_keys(&self) -> rusqlite::Result<()> {
        self.connection
            .pragma_update(None, "defer_foreign_keys", true)
    }

    /// Record when the secret of sync record `kind`/`id` was last edited.
    pub fn set_secret_edit(&self, kind: &str, id: &str, edited_ms: u64) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO secret_edits (kind, id, edited_ms) VALUES (?1, ?2, ?3) \
             ON CONFLICT (kind, id) DO UPDATE SET edited_ms = excluded.edited_ms",
            params![kind, id, to_sql(edited_ms)],
        )?;
        Ok(())
    }

    /// The sync state as last saved, `None` before sync was set up.
    pub fn sync_state(&self) -> rusqlite::Result<Option<Vec<u8>>> {
        let mut statement = self
            .connection
            .prepare("SELECT state FROM sync_state WHERE one = 1")?;
        let mut rows = statement.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    pub fn set_sync_state(&self, state: &[u8]) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO sync_state (one, state) VALUES (1, ?1) \
             ON CONFLICT (one) DO UPDATE SET state = excluded.state",
            params![state],
        )?;
        Ok(())
    }

    /// Forget the sync state, as turning sync off does, and what was still
    /// to be written.
    pub fn clear_sync_state(&self) -> rusqlite::Result<()> {
        self.connection
            .execute_batch("DELETE FROM sync_state; DELETE FROM sync_pending;")
    }

    /// Note that the secret of sync record `kind`/`id` is still to be
    /// written, or that it is written.
    pub fn set_sync_pending(&self, kind: &str, id: &str, pending: bool) -> rusqlite::Result<()> {
        let sql = if pending {
            "INSERT OR IGNORE INTO sync_pending (kind, id) VALUES (?1, ?2)"
        } else {
            "DELETE FROM sync_pending WHERE kind = ?1 AND id = ?2"
        };
        self.connection.execute(sql, params![kind, id])?;
        Ok(())
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
            "INSERT INTO groups (id, name, parent_id, sort_order, expanded, sync_id, edited_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.parent.map(|parent| to_sql(parent.0)),
                group.sort_order,
                group.expanded,
                group.sync_id.as_str(),
                to_sql(group.edited_ms),
            ],
        )?;
        Ok(())
    }

    pub fn update_group(&self, group: &HostGroup) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE groups SET name = ?2, parent_id = ?3, sort_order = ?4, expanded = ?5, \
             edited_ms = ?6 WHERE id = ?1",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.parent.map(|parent| to_sql(parent.0)),
                group.sort_order,
                group.expanded,
                to_sql(group.edited_ms),
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
        self.atomically(|connection| {
            let row = HostRow::of(host);
            connection.execute(
            "INSERT INTO hosts (id, public_id, group_id, sort_order, name, address, port, auth, \
             username, credential_id, route, proxy_kind, proxy_host, proxy_port, \
             proxy_username, notes, os, protocol, serial_baud, serial_data_bits, \
             serial_parity, serial_stop_bits, serial_flow, telnet_user_prompt, \
             telnet_pass_prompt, sidebar_tool, edited_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
             ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
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
                host.protocol.as_str(),
                row.serial.baud,
                row.serial.data_bits,
                row.serial.parity,
                row.serial.stop_bits,
                row.serial.flow,
                row.telnet_user_prompt,
                row.telnet_password_prompt,
                host.sidebar_tool.as_deref(),
                to_sql(host.edited_ms),
            ],
        )?;
            write_jumps(connection, host)
        })
    }

    /// Rewrite the fields the host form edits, jump hosts included.
    /// `last_connected_at`, `os` and `sidebar_tool` are left alone: they are
    /// written by `touch_connected`, `set_host_os` and
    /// `set_host_sidebar_tool`, and none is part of the host form.
    /// `public_id` never changes.
    pub fn update_host(&self, host: &Host) -> rusqlite::Result<()> {
        self.atomically(|connection| {
            let row = HostRow::of(host);
            connection.execute(
                "UPDATE hosts SET group_id = ?2, sort_order = ?3, name = ?4, address = ?5, \
             port = ?6, auth = ?7, username = ?8, credential_id = ?9, route = ?10, \
             proxy_kind = ?11, proxy_host = ?12, proxy_port = ?13, proxy_username = ?14, \
             notes = ?15, serial_baud = ?16, serial_data_bits = ?17, serial_parity = ?18, \
             serial_stop_bits = ?19, serial_flow = ?20, telnet_user_prompt = ?21, \
             telnet_pass_prompt = ?22, protocol = ?23, edited_ms = ?24 WHERE id = ?1",
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
                    row.serial.baud,
                    row.serial.data_bits,
                    row.serial.parity,
                    row.serial.stop_bits,
                    row.serial.flow,
                    row.telnet_user_prompt,
                    row.telnet_password_prompt,
                    host.protocol.as_str(),
                    to_sql(host.edited_ms),
                ],
            )?;
            write_jumps(connection, host)
        })
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
        self.atomically(|connection| {
            let mut update_group = connection.prepare(
                "UPDATE groups SET parent_id = ?2, sort_order = ?3, edited_ms = ?4 WHERE id = ?1",
            )?;
            for group in groups {
                update_group.execute(params![
                    to_sql(group.id.0),
                    group.parent.map(|id| to_sql(id.0)),
                    group.sort_order,
                    to_sql(group.edited_ms),
                ])?;
            }
            let mut update_host = connection.prepare(
                "UPDATE hosts SET group_id = ?2, sort_order = ?3, edited_ms = ?4 WHERE id = ?1",
            )?;
            for host in hosts {
                update_host.execute(params![
                    to_sql(host.id.0),
                    host.group.map(|id| to_sql(id.0)),
                    host.sort_order,
                    to_sql(host.edited_ms),
                ])?;
            }
            Ok(())
        })
    }

    /// Add a bookmark, or give the one there its order and edit time.
    pub fn insert_bookmark(
        &self,
        host: HostId,
        side: BookmarkSide,
        path: &str,
        sort_order: i64,
        edited_ms: u64,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO bookmarks (host_id, side, path, sort_order, edited_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (host_id, side, path) \
             DO UPDATE SET sort_order = excluded.sort_order, edited_ms = excluded.edited_ms",
            params![
                to_sql(host.0),
                side.as_str(),
                path,
                sort_order,
                to_sql(edited_ms)
            ],
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

    /// Give one pane's bookmarks their order and edit times, as
    /// `(path, sort_order, edited_ms)`. Loading groups bookmarks by pane, so
    /// only the order within this pane matters.
    pub fn set_bookmark_order(
        &self,
        host: HostId,
        side: BookmarkSide,
        bookmarks: &[(String, i64, u64)],
    ) -> rusqlite::Result<()> {
        self.atomically(|connection| {
            let mut update = connection.prepare(
                "UPDATE bookmarks SET sort_order = ?4, edited_ms = ?5 \
                 WHERE host_id = ?1 AND side = ?2 AND path = ?3",
            )?;
            for (path, order, edited_ms) in bookmarks {
                update.execute(params![
                    to_sql(host.0),
                    side.as_str(),
                    path,
                    order,
                    to_sql(*edited_ms)
                ])?;
            }
            Ok(())
        })
    }

    pub fn insert_forward(&self, rule: &ForwardRule) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO forwards (id, host_id, name, kind, bind_host, bind_port, \
             target_host, target_port, auto_start, sort_order, sync_id, edited_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
                rule.sync_id.as_str(),
                to_sql(rule.edited_ms),
            ],
        )?;
        Ok(())
    }

    pub fn update_forward(&self, rule: &ForwardRule) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE forwards SET host_id = ?2, name = ?3, kind = ?4, bind_host = ?5, \
             bind_port = ?6, target_host = ?7, target_port = ?8, auto_start = ?9, \
             sort_order = ?10, edited_ms = ?11 WHERE id = ?1",
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
                to_sql(rule.edited_ms),
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
            "INSERT INTO credentials (id, keychain_id, name, kind, username, key_path, sort_order, \
             edited_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                to_sql(credential.id.0),
                credential.keychain_id.as_str(),
                credential.name.as_ref(),
                credential.kind.as_str(),
                credential.user.as_ref(),
                credential.key_path.as_deref(),
                credential.sort_order,
                to_sql(credential.edited_ms),
            ],
        )?;
        Ok(())
    }

    /// Rewrite a credential. The hosts using it have no user name of
    /// their own here, so they follow without being touched.
    pub fn update_credential(&self, credential: &Credential) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE credentials SET name = ?2, kind = ?3, username = ?4, key_path = ?5, \
             sort_order = ?6, edited_ms = ?7 WHERE id = ?1",
            params![
                to_sql(credential.id.0),
                credential.name.as_ref(),
                credential.kind.as_str(),
                credential.user.as_ref(),
                credential.key_path.as_deref(),
                credential.sort_order,
                to_sql(credential.edited_ms),
            ],
        )?;
        Ok(())
    }

    /// Delete a credential. The hosts using it go back to logging in on
    /// their own with `auth`, as the credential's user, first, edited at
    /// `edited_ms`: a credential still in use cannot be deleted.
    pub fn remove_credential(
        &self,
        id: CredentialId,
        auth: AuthKind,
        edited_ms: u64,
    ) -> rusqlite::Result<()> {
        self.atomically(|connection| {
            connection.execute(
                "UPDATE hosts SET auth = ?2, credential_id = NULL, \
                 username = (SELECT username FROM credentials WHERE id = ?1), edited_ms = ?3 \
                 WHERE credential_id = ?1",
                params![to_sql(id.0), auth.as_str(), to_sql(edited_ms)],
            )?;
            connection.execute(
                "DELETE FROM credentials WHERE id = ?1",
                params![to_sql(id.0)],
            )?;
            Ok(())
        })
    }

    pub fn insert_snippet_category(&self, category: &SnippetCategory) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO snippet_categories (id, name, sync_id, edited_ms) VALUES (?1, ?2, ?3, ?4)",
            params![
                to_sql(category.id.0),
                category.name.as_ref(),
                category.sync_id.as_str(),
                to_sql(category.edited_ms),
            ],
        )?;
        Ok(())
    }

    pub fn update_snippet_category(&self, category: &SnippetCategory) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE snippet_categories SET name = ?2, edited_ms = ?3 WHERE id = ?1",
            params![
                to_sql(category.id.0),
                category.name.as_ref(),
                to_sql(category.edited_ms),
            ],
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
             run_on_click, sync_id, edited_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                to_sql(snippet.id.0),
                snippet.category.map(|id| to_sql(id.0)),
                snippet.name.as_ref(),
                snippet.command,
                group,
                host,
                snippet.run_on_click,
                snippet.sync_id.as_str(),
                to_sql(snippet.edited_ms),
            ],
        )?;
        Ok(())
    }

    pub fn update_snippet(&self, snippet: &Snippet) -> rusqlite::Result<()> {
        let (group, host) = scope_columns(snippet.scope);
        self.connection.execute(
            "UPDATE snippets SET category_id = ?2, name = ?3, command = ?4, \
             scope_group_id = ?5, scope_host_id = ?6, run_on_click = ?7, edited_ms = ?8 \
             WHERE id = ?1",
            params![
                to_sql(snippet.id.0),
                snippet.category.map(|id| to_sql(id.0)),
                snippet.name.as_ref(),
                snippet.command,
                group,
                host,
                snippet.run_on_click,
                to_sql(snippet.edited_ms),
            ],
        )?;
        Ok(())
    }

    pub fn remove_snippet(&self, id: SnippetId) -> rusqlite::Result<()> {
        self.connection
            .execute("DELETE FROM snippets WHERE id = ?1", params![to_sql(id.0)])?;
        Ok(())
    }

    pub fn insert_terminal_group(&self, group: &TerminalGroup) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO terminal_groups (id, name, sort_order, expanded) VALUES (?1, ?2, ?3, ?4)",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.sort_order,
                group.expanded
            ],
        )?;
        Ok(())
    }

    pub fn update_terminal_group(&self, group: &TerminalGroup) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE terminal_groups SET name = ?2, sort_order = ?3, expanded = ?4 WHERE id = ?1",
            params![
                to_sql(group.id.0),
                group.name.as_ref(),
                group.sort_order,
                group.expanded
            ],
        )?;
        Ok(())
    }

    /// Delete a group with its terminals and their tabs: the foreign keys
    /// cascade.
    pub fn remove_terminal_group(&self, id: TerminalGroupId) -> rusqlite::Result<()> {
        self.connection.execute(
            "DELETE FROM terminal_groups WHERE id = ?1",
            params![to_sql(id.0)],
        )?;
        Ok(())
    }

    pub fn insert_saved_terminal(&self, terminal: &SavedTerminal) -> rusqlite::Result<()> {
        self.connection.execute(
            "INSERT INTO saved_terminals \
             (id, group_id, name, renamed, title, directory, sort_order) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                to_sql(terminal.id.0),
                terminal.group.map(|group| to_sql(group.0)),
                terminal.name.as_ref(),
                terminal.renamed,
                terminal.title.as_deref(),
                terminal.directory,
                terminal.sort_order,
            ],
        )?;
        Ok(())
    }

    pub fn update_saved_terminal(&self, terminal: &SavedTerminal) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE saved_terminals \
             SET group_id = ?2, name = ?3, renamed = ?4, title = ?5, directory = ?6, \
                 sort_order = ?7 \
             WHERE id = ?1",
            params![
                to_sql(terminal.id.0),
                terminal.group.map(|group| to_sql(group.0)),
                terminal.name.as_ref(),
                terminal.renamed,
                terminal.title.as_deref(),
                terminal.directory,
                terminal.sort_order,
            ],
        )?;
        Ok(())
    }

    /// Save where every group and saved terminal of 项目 is, after a drag.
    pub fn save_terminal_order(
        &self,
        groups: &[TerminalGroup],
        terminals: &[SavedTerminal],
    ) -> rusqlite::Result<()> {
        self.atomically(|connection| {
            let mut update_group =
                connection.prepare("UPDATE terminal_groups SET sort_order = ?2 WHERE id = ?1")?;
            for group in groups {
                update_group.execute(params![to_sql(group.id.0), group.sort_order])?;
            }
            let mut update_terminal = connection.prepare(
                "UPDATE saved_terminals SET group_id = ?2, sort_order = ?3 WHERE id = ?1",
            )?;
            for terminal in terminals {
                update_terminal.execute(params![
                    to_sql(terminal.id.0),
                    terminal.group.map(|group| to_sql(group.0)),
                    terminal.sort_order,
                ])?;
            }
            Ok(())
        })
    }

    /// Delete a saved terminal with its tab: the foreign key cascades.
    pub fn remove_saved_terminal(&self, id: SavedTerminalId) -> rusqlite::Result<()> {
        self.connection.execute(
            "DELETE FROM saved_terminals WHERE id = ?1",
            params![to_sql(id.0)],
        )?;
        Ok(())
    }

    /// Replace the saved terminals' tabs with `tabs`, in their order.
    pub fn replace_saved_terminal_tabs(&self, tabs: &[SavedTerminalTab]) -> rusqlite::Result<()> {
        self.atomically(|connection| {
            connection.execute("DELETE FROM saved_terminal_tabs", [])?;
            let mut insert = connection.prepare(
                "INSERT INTO saved_terminal_tabs (position, terminal_id, front) \
                 VALUES (?1, ?2, ?3)",
            )?;
            for (position, tab) in tabs.iter().enumerate() {
                insert.execute(params![position as i64, to_sql(tab.terminal.0), tab.front])?;
            }
            Ok(())
        })
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

    /// Record the right sidebar's tool as the host's terminals have it now,
    /// `None` for hidden.
    pub fn set_host_sidebar_tool(&self, id: HostId, tool: Option<&str>) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE hosts SET sidebar_tool = ?2 WHERE id = ?1",
            params![to_sql(id.0), tool],
        )?;
        Ok(())
    }

    /// Record when each of `hosts` was last edited.
    pub fn set_hosts_edited(&self, hosts: &[(HostId, u64)]) -> rusqlite::Result<()> {
        self.atomically(|connection| {
            let mut update = connection.prepare("UPDATE hosts SET edited_ms = ?2 WHERE id = ?1")?;
            for (id, edited_ms) in hosts {
                update.execute(params![to_sql(id.0), to_sql(*edited_ms)])?;
            }
            Ok(())
        })
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
/// model: its login, its route, its serial line and its Telnet prompts.
struct HostRow<'a> {
    serial: SerialColumns,
    /// `NULL` for the usual prompt, and for a host that is not Telnet.
    telnet_user_prompt: Option<&'a str>,
    telnet_password_prompt: Option<&'a str>,
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
        let telnet = host.protocol.telnet();
        let prompt =
            |prompt: &'a SharedString| Some(prompt.as_ref()).filter(|prompt| !prompt.is_empty());
        Self {
            serial: SerialColumns::of(&host.protocol),
            telnet_user_prompt: telnet.and_then(|telnet| prompt(&telnet.user_prompt)),
            telnet_password_prompt: telnet.and_then(|telnet| prompt(&telnet.password_prompt)),
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

/// The five `serial_*` columns, all set for a serial host and none for an
/// SSH one.
struct SerialColumns {
    baud: Option<u32>,
    data_bits: Option<i64>,
    parity: Option<String>,
    stop_bits: Option<i64>,
    flow: Option<String>,
}

impl SerialColumns {
    fn of(protocol: &Protocol) -> Self {
        let settings = protocol.serial();
        Self {
            baud: settings.map(|line| line.baud),
            data_bits: settings.map(|line| line.data_bits.value().into()),
            parity: settings.map(|line| line.parity.as_str().to_owned()),
            stop_bits: settings.map(|line| line.stop_bits.value().into()),
            flow: settings.map(|line| line.flow.as_str().to_owned()),
        }
    }

    /// The line these columns describe. The table's CHECKs keep them whole
    /// and valid; anything a hand edit got past them reads as the default.
    fn settings(&self) -> SerialSettings {
        let default = SerialSettings::default();
        SerialSettings {
            baud: self.baud.unwrap_or(default.baud),
            data_bits: self
                .data_bits
                .and_then(DataBits::from_value)
                .unwrap_or(default.data_bits),
            parity: self
                .parity
                .as_deref()
                .and_then(Parity::from_stored)
                .unwrap_or(default.parity),
            stop_bits: self
                .stop_bits
                .and_then(StopBits::from_value)
                .unwrap_or(default.stop_bits),
            flow: self
                .flow
                .as_deref()
                .and_then(FlowControl::from_stored)
                .unwrap_or(default.flow),
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
fn migrate(connection: &Connection) -> Result<(), OpenError> {
    let mut version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 0 {
        connection.execute_batch(&format!(
            "BEGIN;\n{SCHEMA}\nPRAGMA user_version = {SCHEMA_VERSION};\nCOMMIT;"
        ))?;
        return Ok(());
    }
    if version > SCHEMA_VERSION {
        return Err(OpenError::Newer {
            version,
            supported: SCHEMA_VERSION,
        });
    }
    let oldest = STEPS.first().map_or(SCHEMA_VERSION, |(from, _)| *from);
    if version < oldest {
        return Err(OpenError::TooOld { version });
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
        eprintln!("shellrs: cannot back up the database before migrating it: {error}");
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

/// The same for the tables with a `sync_id`.
fn fill_missing_sync_ids(connection: &mut Connection) -> rusqlite::Result<()> {
    for table in SYNC_ID_TABLES {
        let missing: i64 = connection.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE sync_id = ''"),
            [],
            |row| row.get(0),
        )?;
        if missing > 0 {
            let transaction = connection.transaction()?;
            fill_sync_ids(&transaction, table)?;
            transaction.commit()?;
        }
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

/// Milliseconds since the Unix epoch, for an edit time.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
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
        assert!(
            matches!(error, OpenError::Newer { version, supported: SCHEMA_VERSION } if version == newer),
            "{error}"
        );
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

    /// The schema of version 14, as files of that version have it.
    const SCHEMA_14: &str = "\
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

    /// The schema of version 15, as files of that version have it.
    const SCHEMA_15: &str = "\
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
    protocol          TEXT NOT NULL DEFAULT 'ssh' CHECK (protocol IN ('ssh', 'serial')),
    serial_baud       INTEGER CHECK (serial_baud > 0)
        CHECK ((protocol = 'serial') = (serial_baud IS NOT NULL)),
    serial_data_bits  INTEGER CHECK (serial_data_bits BETWEEN 5 AND 8)
        CHECK ((protocol = 'serial') = (serial_data_bits IS NOT NULL)),
    serial_parity     TEXT CHECK (serial_parity IN ('none', 'odd', 'even'))
        CHECK ((protocol = 'serial') = (serial_parity IS NOT NULL)),
    serial_stop_bits  INTEGER CHECK (serial_stop_bits IN (1, 2))
        CHECK ((protocol = 'serial') = (serial_stop_bits IS NOT NULL)),
    serial_flow       TEXT CHECK (serial_flow IN ('none', 'rts-cts', 'xon-xoff'))
        CHECK ((protocol = 'serial') = (serial_flow IS NOT NULL)),
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

    #[test]
    fn a_version_14_file_gets_the_serial_columns_and_keeps_its_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(&format!(
                "BEGIN;\n{SCHEMA_14}\nPRAGMA user_version = 14;\nCOMMIT;"
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
        assert!(dir.path().join("shellrs.db.v14.bak").exists());
        let fresh = HostDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));
        let data = db.load().unwrap();
        assert_eq!(data.hosts.len(), 1);
        assert_eq!(data.hosts[0].protocol, Protocol::Ssh);
    }

    #[test]
    fn a_version_15_file_takes_telnet_hosts_and_keeps_every_row_on_its_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let old = Connection::open(&path).unwrap();
            old.pragma_update(None, "foreign_keys", true).unwrap();
            old.execute_batch(&format!(
                "BEGIN;\n{SCHEMA_15}\nPRAGMA user_version = 15;\nCOMMIT;"
            ))
            .unwrap();
            old.execute_batch(
                "INSERT INTO groups (id, name) VALUES (1, '生产');
INSERT INTO credentials (id, keychain_id, name, kind, username)
    VALUES (1, 'k1', '运维', 'password', 'deploy');
INSERT INTO hosts (id, public_id, group_id, sort_order, name, address, port, auth, username,
    last_connected_at)
    VALUES (1, 'pub-web', 1, 7, 'web', '10.0.0.1', 22, 'password', 'root', 200);
INSERT INTO hosts (id, name, address, port, auth, credential_id, route, last_connected_at)
    VALUES (2, 'db', '10.0.0.2', 22, 'credential', 1, 'jump', 300);
INSERT INTO hosts (id, name, address, port, auth, username, protocol, serial_baud,
    serial_data_bits, serial_parity, serial_stop_bits, serial_flow)
    VALUES (3, 'console', '/dev/ttyUSB0', 22, 'no-password', '', 'serial', 9600, 8, 'none', 1,
    'none');
INSERT INTO host_jumps (host_id, position, jump_id) VALUES (2, 0, 1), (2, 1, NULL);
INSERT INTO bookmarks (host_id, side, path, sort_order) VALUES (1, 'remote', '/var/log', 0);
INSERT INTO forwards (id, host_id, kind, bind_host, bind_port, target_host, target_port)
    VALUES (1, 1, 'local', '127.0.0.1', 8080, 'localhost', 80);
INSERT INTO snippet_categories (id, name) VALUES (1, '常用');
INSERT INTO snippets (id, category_id, name, command, scope_host_id)
    VALUES (1, 1, 'top', 'top', 1);",
            )
            .unwrap();
        }

        let db = HostDatabase::open(&path).unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(dir.path().join("shellrs.db.v15.bak").exists());
        let fresh = HostDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));
        let foreign_keys: i64 = db
            .connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1, "back on after the rebuild");
        let violations = db
            .connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query_map([], |_| Ok(()))
            .unwrap()
            .count();
        assert_eq!(violations, 0);

        let data = db.load().unwrap();
        assert_eq!(data.hosts.len(), 3);
        let web = &data.hosts[0];
        assert_eq!(web.public_id.as_str(), "pub-web");
        assert_eq!((web.sort_order, web.group), (7, Some(GroupId(1))));
        assert_eq!(
            data.hosts[1].route,
            Route::Jump(vec![Some(HostId(1)), None])
        );
        assert_eq!(data.hosts[1].credential, Some(CredentialId(1)));
        assert!(data.hosts[2].is_serial());
        assert_eq!(data.recent, [HostId(2), HostId(1)]);
        assert_eq!(data.bookmarks.len(), 1);
        assert_eq!(data.forwards.len(), 1);
        assert_eq!(data.snippets.len(), 1);

        // The foreign keys hold as before: deleting web takes its rows, and
        // leaves its place in db's jump hosts empty.
        db.remove_host(HostId(1)).unwrap();
        let data = db.load().unwrap();
        assert!(data.bookmarks.is_empty());
        assert!(data.forwards.is_empty());
        assert!(data.snippets.is_empty());
        assert_eq!(data.hosts[0].route, Route::Jump(vec![None, None]));
    }

    #[test]
    fn a_version_16_file_gets_the_sidebar_column_and_keeps_its_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            // Version 16 as files of it have it: 15's schema, taken on by
            // its step.
            let old = Connection::open(&path).unwrap();
            old.execute_batch(&format!(
                "BEGIN;\n{SCHEMA_15}\nPRAGMA user_version = 15;\nCOMMIT;"
            ))
            .unwrap();
            add_telnet(&old).unwrap();
            old.execute(
                "INSERT INTO hosts (id, name, address, port, auth, username, os, protocol) \
                 VALUES (1, 'switch', '10.0.0.5', 23, 'no-password', '', 'debian', 'telnet')",
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
        assert!(dir.path().join("shellrs.db.v16.bak").exists());
        let fresh = HostDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));
        let data = db.load().unwrap();
        assert_eq!(data.hosts.len(), 1);
        assert_eq!(data.hosts[0].os, Some(HostOs::Debian));
        assert!(data.hosts[0].protocol.telnet().is_some());
        assert_eq!(data.hosts[0].sidebar_tool, None);
    }

    #[test]
    fn a_version_17_file_gets_the_saved_terminals_and_keeps_its_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            // Version 17 as files of it have it: 15's schema, taken on by
            // its steps.
            let old = Connection::open(&path).unwrap();
            old.execute_batch(&format!(
                "BEGIN;\n{SCHEMA_15}\nPRAGMA user_version = 15;\nCOMMIT;"
            ))
            .unwrap();
            add_telnet(&old).unwrap();
            let Step::Sql(sidebar) = STEPS[3].1 else {
                panic!("the sidebar column is a SQL step");
            };
            old.execute_batch(sidebar).unwrap();
            old.execute(
                "INSERT INTO hosts (id, name, address, port, auth, username, sidebar_tool) \
                 VALUES (1, 'web', '10.0.0.1', 22, 'password', 'root', 'files')",
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
        assert!(dir.path().join("shellrs.db.v17.bak").exists());
        let fresh = HostDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));
        let data = db.load().unwrap();
        assert_eq!(data.hosts.len(), 1);
        assert_eq!(data.hosts[0].sidebar_tool.as_deref(), Some("files"));
        assert!(data.saved_terminals.is_empty());
        assert!(data.saved_terminal_tabs.is_empty());
    }

    #[test]
    fn a_version_18_file_gets_the_title_column_and_keeps_its_terminals() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            // Version 18 as files of it have it: 15's schema, taken on by
            // its steps.
            let old = Connection::open(&path).unwrap();
            old.execute_batch(&format!(
                "BEGIN;\n{SCHEMA_15}\nPRAGMA user_version = 15;\nCOMMIT;"
            ))
            .unwrap();
            add_telnet(&old).unwrap();
            for (_, step) in &STEPS[3..5] {
                let Step::Sql(sql) = step else {
                    panic!("versions 17 and 18 are SQL steps");
                };
                old.execute_batch(sql).unwrap();
            }
            old.execute_batch(
                "INSERT INTO terminal_groups (id, name) VALUES (1, 'shellrs');
INSERT INTO saved_terminals (id, group_id, name, directory) \
 VALUES (1, 1, 'shellrs 1', '/code/shellrs');",
            )
            .unwrap();
        }

        let db = HostDatabase::open(&path).unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(dir.path().join("shellrs.db.v18.bak").exists());
        let fresh = HostDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));
        let mut first = terminal(1, 1, "shellrs 1");
        first.directory = Some("/code/shellrs".into());
        assert_eq!(db.load().unwrap().saved_terminals, [first]);
    }

    #[test]
    fn a_version_19_file_gets_what_sync_needs_and_keeps_its_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            // Version 19 as files of it have it: 15's schema, taken on by
            // its steps.
            let old = Connection::open(&path).unwrap();
            old.execute_batch(&format!(
                "BEGIN;\n{SCHEMA_15}\nPRAGMA user_version = 15;\nCOMMIT;"
            ))
            .unwrap();
            add_telnet(&old).unwrap();
            for (_, step) in &STEPS[3..6] {
                let Step::Sql(sql) = step else {
                    panic!("versions 17 to 19 are SQL steps");
                };
                old.execute_batch(sql).unwrap();
            }
            old.execute_batch(
                "INSERT INTO groups (id, name) VALUES (1, 'production'), (2, 'staging');
INSERT INTO credentials (id, keychain_id, name, kind, username, key_path) \
 VALUES (1, 'k1aaaaaaaaaaaaaa', 'deploy', 'key', 'deploy', '/home/me/.ssh/id');
INSERT INTO hosts (id, public_id, group_id, name, address, port, auth, credential_id) \
 VALUES (1, 'h1aaaaaaaaaaaaaa', 1, 'web', '10.0.0.1', 22, 'credential', 1);
INSERT INTO bookmarks (host_id, side, path, sort_order) VALUES (1, 'remote', '/var/log', 1);
INSERT INTO forwards (id, host_id, kind, bind_host, bind_port) \
 VALUES (1, 1, 'dynamic', '127.0.0.1', 1080);
INSERT INTO snippet_categories (id, name) VALUES (1, 'Docker');
INSERT INTO snippets (id, category_id, name, command) VALUES (1, 1, 'ps', 'docker ps');",
            )
            .unwrap();
        }

        let db = HostDatabase::open(&path).unwrap();
        let version: i64 = db
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert!(dir.path().join("shellrs.db.v19.bak").exists());
        let fresh = HostDatabase::in_memory().unwrap();
        assert_eq!(schema_of(&db.connection), schema_of(&fresh.connection));
        let foreign_key_problems: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(foreign_key_problems, 0);

        // Every record is taken as edited when the file was upgraded, and
        // has a sync id of its own where it needs one.
        let data = db.load().unwrap();
        let group_ids: HashSet<_> = data.groups.iter().map(|g| g.sync_id.clone()).collect();
        assert_eq!(group_ids.len(), 2);
        assert!(group_ids.iter().all(|id| PublicId::has_shape(id.as_str())));
        assert!(PublicId::has_shape(data.forwards[0].sync_id.as_str()));
        assert!(PublicId::has_shape(data.snippets[0].sync_id.as_str()));
        assert!(PublicId::has_shape(
            data.snippet_categories[0].sync_id.as_str()
        ));
        let edits = [
            data.groups[0].edited_ms,
            data.hosts[0].edited_ms,
            data.credentials[0].edited_ms,
            data.bookmarks[0].4,
            data.forwards[0].edited_ms,
            data.snippet_categories[0].edited_ms,
            data.snippets[0].edited_ms,
        ];
        assert!(edits.iter().all(|edited| *edited > 0), "{edits:?}");
        assert_eq!(
            data.credentials[0].key_path.as_deref(),
            Some("/home/me/.ssh/id")
        );
        assert_eq!(data.hosts[0].credential, Some(CredentialId(1)));

        // A key credential sync brings may come before its key.
        let mut synced = credential(2, "synced", CredentialKind::Key);
        synced.key_path = None;
        assert!(db.insert_credential(&synced).is_ok());
    }

    #[test]
    fn a_row_added_by_hand_gets_a_sync_id_at_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        drop(HostDatabase::open(&path).unwrap());
        Connection::open(&path)
            .unwrap()
            .execute("INSERT INTO groups (id, name) VALUES (1, 'by hand')", [])
            .unwrap();
        let db = HostDatabase::open(&path).unwrap();
        assert!(PublicId::has_shape(
            db.load().unwrap().groups[0].sync_id.as_str()
        ));
    }

    fn terminal(id: u64, group: u64, name: &str) -> SavedTerminal {
        SavedTerminal {
            id: SavedTerminalId(id),
            group: Some(TerminalGroupId(group)),
            name: name.into(),
            renamed: false,
            title: None,
            directory: None,
            sort_order: 0,
        }
    }

    fn terminal_group(id: u64, name: &str) -> TerminalGroup {
        TerminalGroup {
            id: TerminalGroupId(id),
            name: name.into(),
            sort_order: 0,
            expanded: true,
        }
    }

    fn saved_tab(terminal: u64, front: bool) -> SavedTerminalTab {
        SavedTerminalTab {
            terminal: SavedTerminalId(terminal),
            front,
        }
    }

    #[test]
    fn saved_terminals_round_trip_and_go_with_their_group() {
        let db = HostDatabase::in_memory().unwrap();
        for (id, name) in [(1, "shellrs"), (2, "blog")] {
            db.insert_terminal_group(&terminal_group(id, name)).unwrap();
        }
        let mut claude = terminal(1, 1, "claude");
        claude.directory = Some("/code/shellrs".into());
        claude.sort_order = 4;
        db.insert_saved_terminal(&claude).unwrap();
        db.insert_saved_terminal(&terminal(2, 1, "shellrs 2"))
            .unwrap();
        db.insert_saved_terminal(&terminal(3, 2, "blog 1")).unwrap();
        db.replace_saved_terminal_tabs(&[saved_tab(1, false), saved_tab(3, true)])
            .unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.terminal_groups.len(), 2);
        assert_eq!(data.saved_terminals[0], claude);
        assert_eq!(
            data.saved_terminal_tabs,
            [saved_tab(1, false), saved_tab(3, true)]
        );

        claude.name = "cargo run".into();
        claude.renamed = true;
        claude.title = Some("✳ Claude Code".into());
        claude.directory = Some("/code/shellrs/src".into());
        db.update_saved_terminal(&claude).unwrap();
        let mut site = terminal_group(2, "site");
        site.expanded = false;
        db.update_terminal_group(&site).unwrap();
        // A drag's order.
        let mut groups = [terminal_group(1, "shellrs"), terminal_group(2, "site")];
        groups[1].sort_order = -1;
        let mut moved = terminal(2, 1, "shellrs 2");
        moved.sort_order = 7;
        db.save_terminal_order(&groups, std::slice::from_ref(&moved))
            .unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.terminal_groups[1].sort_order, -1);
        // A drag's order leaves it folded.
        assert!(data.terminal_groups[0].expanded);
        assert!(!data.terminal_groups[1].expanded);
        assert_eq!(data.saved_terminals[1], moved);
        // A group takes its terminals and their tabs.
        db.remove_terminal_group(TerminalGroupId(2)).unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.saved_terminals, [claude, moved]);
        assert_eq!(data.saved_terminal_tabs, [saved_tab(1, false)]);
        assert!(
            data.terminal_groups
                .iter()
                .all(|group| group.name != "site")
        );
        db.remove_saved_terminal(SavedTerminalId(1)).unwrap();
        assert!(db.load().unwrap().saved_terminal_tabs.is_empty());
    }

    #[test]
    fn a_saved_terminal_has_one_tab_and_one_is_in_front() {
        let db = HostDatabase::in_memory().unwrap();
        db.insert_terminal_group(&terminal_group(1, "shellrs"))
            .unwrap();
        db.insert_saved_terminal(&terminal(1, 1, "a")).unwrap();
        db.insert_saved_terminal(&terminal(2, 1, "b")).unwrap();
        assert!(
            db.replace_saved_terminal_tabs(&[saved_tab(1, true), saved_tab(2, true)])
                .is_err()
        );
        assert!(
            db.replace_saved_terminal_tabs(&[saved_tab(1, false), saved_tab(1, false)])
                .is_err()
        );
        // A failed write leaves the list as it was.
        db.replace_saved_terminal_tabs(&[saved_tab(1, true)])
            .unwrap();
        assert!(
            db.replace_saved_terminal_tabs(&[saved_tab(9, false)])
                .is_err()
        );
        assert_eq!(db.load().unwrap().saved_terminal_tabs, [saved_tab(1, true)]);
        // A terminal is in a group that is there, or at the top.
        assert!(db.insert_saved_terminal(&terminal(3, 7, "c")).is_err());
        let top = SavedTerminal {
            group: None,
            ..terminal(4, 0, "终端 1")
        };
        db.insert_saved_terminal(&top).unwrap();
        assert_eq!(db.load().unwrap().saved_terminals.last(), Some(&top));
    }

    #[test]
    fn a_host_keeps_its_sidebar_tool_apart_from_the_form() {
        let db = HostDatabase::in_memory().unwrap();
        let mut host = Host::new(
            HostId(1),
            HostDraft::new("web", "10.0.0.1", 22, "root", AuthKind::Password, None),
        );
        host.sidebar_tool = Some("files".into());
        db.insert_host(&host).unwrap();
        let tool = || db.load().unwrap().hosts[0].sidebar_tool.clone();
        assert_eq!(tool().as_deref(), Some("files"));

        // Saving the form leaves it alone; the sidebar changes it.
        host.name = "web-01".into();
        host.sidebar_tool = None;
        db.update_host(&host).unwrap();
        assert_eq!(tool().as_deref(), Some("files"));
        db.set_host_sidebar_tool(HostId(1), Some("monitor"))
            .unwrap();
        assert_eq!(tool().as_deref(), Some("monitor"));
        db.set_host_sidebar_tool(HostId(1), None).unwrap();
        assert_eq!(tool(), None);

        // A tool a newer version has is kept for it; the app shows none.
        db.connection
            .execute("UPDATE hosts SET sidebar_tool = 'radar' WHERE id = 1", [])
            .unwrap();
        assert_eq!(tool().as_deref(), Some("radar"));
    }

    #[test]
    fn a_telnet_host_keeps_its_prompts_and_other_hosts_have_none() {
        let db = HostDatabase::in_memory().unwrap();
        let mut telnet = Host::new(HostId(1), HostDraft::telnet("switch", "10.0.0.5", 23, None));
        db.insert_host(&telnet).unwrap();
        let ssh = Host::new(
            HostId(2),
            HostDraft::new("web", "10.0.0.1", 22, "root", AuthKind::Password, None),
        );
        db.insert_host(&ssh).unwrap();

        let loaded = &db.load().unwrap().hosts[0];
        assert_eq!(loaded.protocol, Protocol::Telnet(TelnetSettings::default()));
        assert_eq!((loaded.port, loaded.auth), (23, AuthKind::NoPassword));
        assert_eq!(loaded.user.as_ref(), "");

        let prompts = TelnetSettings {
            user_prompt: "账号：".into(),
            password_prompt: "".into(),
        };
        telnet.protocol = Protocol::Telnet(prompts.clone());
        telnet.auth = AuthKind::Password;
        telnet.user = "admin".into();
        db.update_host(&telnet).unwrap();
        let data = db.load().unwrap();
        assert_eq!(data.hosts[0].protocol, Protocol::Telnet(prompts));
        assert_eq!(data.hosts[0].user.as_ref(), "admin");
        assert_eq!(data.hosts[1].protocol, Protocol::Ssh);

        // A prompt on another host, an empty one, or a protocol nobody knows
        // is refused.
        for change in [
            "UPDATE hosts SET telnet_user_prompt = 'login:' WHERE id = 2",
            "UPDATE hosts SET telnet_pass_prompt = '' WHERE id = 1",
            "UPDATE hosts SET protocol = 'rlogin' WHERE id = 1",
        ] {
            assert!(db.connection.execute(change, []).is_err(), "{change}");
        }
    }

    #[test]
    fn a_host_changes_protocol_and_its_columns_follow() {
        let db = HostDatabase::in_memory().unwrap();
        let line = SerialSettings::default();
        let mut host = Host::new(
            HostId(1),
            HostDraft::serial("console", "/dev/ttyUSB0", line, None),
        );
        db.insert_host(&host).unwrap();
        let prompts = TelnetSettings {
            user_prompt: "账号：".into(),
            password_prompt: "".into(),
        };
        for draft in [
            HostDraft::telnet("console", "10.0.0.5", 23, None),
            HostDraft::new("console", "10.0.0.5", 22, "root", AuthKind::Password, None),
            HostDraft::serial("console", "COM3", line, None),
        ] {
            let mut next = Host::new(HostId(1), draft);
            if next.protocol.telnet().is_some() {
                next.protocol = Protocol::Telnet(prompts.clone());
            }
            next.public_id = host.public_id.clone();
            db.update_host(&next).unwrap();
            let loaded = &db.load().unwrap().hosts[0];
            assert_eq!(loaded.protocol, next.protocol);
            assert_eq!(
                (loaded.address.as_ref(), loaded.port),
                (next.address.as_ref(), next.port)
            );
            host = next;
        }
    }

    #[test]
    fn a_serial_host_keeps_its_line_and_an_ssh_host_has_none() {
        let db = HostDatabase::in_memory().unwrap();
        let settings = SerialSettings {
            baud: 9600,
            data_bits: DataBits::Seven,
            parity: Parity::Even,
            stop_bits: StopBits::Two,
            flow: FlowControl::RtsCts,
        };
        let mut serial = Host::new(
            HostId(1),
            HostDraft::serial("console", "/dev/ttyUSB0", settings, None),
        );
        db.insert_host(&serial).unwrap();
        let ssh = Host::new(
            HostId(2),
            HostDraft::new("web", "10.0.0.1", 22, "root", AuthKind::Password, None),
        );
        db.insert_host(&ssh).unwrap();

        let data = db.load().unwrap();
        assert_eq!(data.hosts[0].protocol, Protocol::Serial(settings));
        assert_eq!(data.hosts[0].address.as_ref(), "/dev/ttyUSB0");
        assert_eq!(data.hosts[1].protocol, Protocol::Ssh);

        let faster = SerialSettings {
            baud: 115200,
            flow: FlowControl::None,
            ..settings
        };
        serial.protocol = Protocol::Serial(faster);
        db.update_host(&serial).unwrap();
        assert_eq!(
            db.load().unwrap().hosts[0].protocol,
            Protocol::Serial(faster)
        );

        // A line on an SSH host, or a serial host without one, is refused.
        let error = db
            .connection
            .execute("UPDATE hosts SET serial_baud = 9600 WHERE id = 2", []);
        assert!(error.is_err());
        let error = db
            .connection
            .execute("UPDATE hosts SET serial_flow = NULL WHERE id = 1", []);
        assert!(error.is_err());
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
        let docker = SnippetCategory::new(SnippetCategoryId(1), "Docker");
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
        assert!(
            matches!(error, OpenError::TooOld { version: 12 }),
            "{error}"
        );
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

        db.remove_credential(key.id, AuthKind::NoPassword, 5)
            .unwrap();
        let data = db.load().unwrap();
        assert!(data.credentials.is_empty());
        assert_eq!(data.hosts[0].credential, None);
        assert_eq!(data.hosts[0].auth, AuthKind::NoPassword);
        // Edited on the user's behalf; the other host not at all.
        assert_eq!(data.hosts[0].edited_ms, 5);
        assert_eq!(data.hosts[1].edited_ms, 0);
        assert_eq!(data.hosts[0].user.as_ref(), "deploy");
        // A host that never used it is untouched.
        assert_eq!(data.hosts[1].auth, AuthKind::Password);
    }

    #[test]
    fn the_database_refuses_a_credential_whose_key_file_does_not_fit_its_kind() {
        let db = HostDatabase::in_memory().unwrap();
        // A key credential sync brought has no file until its key comes.
        let mut key_without_file = credential(1, "部署", CredentialKind::Key);
        key_without_file.key_path = None;
        assert!(db.insert_credential(&key_without_file).is_ok());

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
    fn a_batch_lands_in_one_commit_and_a_failed_write_takes_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shellrs.db");
        {
            let db = HostDatabase::open(&path).unwrap();
            db.begin_batch().unwrap();
            db.insert_group(&group(1, "生产", None)).unwrap();
            db.insert_host(&host(1, "web-01", Some(1))).unwrap();
            // A host that jumps through itself fails whole: neither its row
            // nor its jump is left behind, and the batch goes on.
            let looped = routed(host(2, "loop", None), Route::Jump(vec![Some(HostId(2))]));
            assert!(db.insert_host(&looped).is_err());
            let jumped = routed(host(3, "app", None), Route::Jump(vec![Some(HostId(1))]));
            db.insert_host(&jumped).unwrap();
            db.end_batch().unwrap();
            assert!(db.connection.is_autocommit());
            // Back to one transaction per write.
            db.insert_host(&host(4, "db-01", None)).unwrap();
        }
        let db = HostDatabase::open(&path).unwrap();
        let data = db.load().unwrap();
        let names: Vec<&str> = data.hosts.iter().map(|host| host.name.as_ref()).collect();
        assert_eq!(names, ["web-01", "app", "db-01"]);
        assert_eq!(data.groups.len(), 1);
        let jumps: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM host_jumps", [], |row| row.get(0))
            .unwrap();
        assert_eq!(jumps, 1);
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
        let paths = |db: &HostDatabase| -> Vec<_> {
            db.load()
                .unwrap()
                .bookmarks
                .into_iter()
                .map(|(host, side, path, ..)| (host, side, path))
                .collect()
        };
        db.insert_host(&host(1, "web", None)).unwrap();
        db.insert_host(&host(2, "db", None)).unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/var/log", 1, 7)
            .unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Local, "/Users/me", 2, 7)
            .unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/etc", 3, 7)
            .unwrap();
        // The same path twice is one bookmark.
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/var/log", 1, 7)
            .unwrap();
        db.insert_bookmark(HostId(2), BookmarkSide::Remote, "/srv", 5, 7)
            .unwrap();
        assert_eq!(
            paths(&db),
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
            &[("/etc".into(), 0, 8), ("/var/log".into(), 1, 8)],
        )
        .unwrap();
        db.insert_bookmark(HostId(1), BookmarkSide::Remote, "/opt", 6, 7)
            .unwrap();
        let remote: Vec<_> = db
            .load()
            .unwrap()
            .bookmarks
            .into_iter()
            .filter(|(host, side, ..)| *host == HostId(1) && *side == BookmarkSide::Remote)
            .map(|(_, _, path, ..)| path)
            .collect();
        assert_eq!(remote, ["/etc", "/var/log", "/opt"]);
        let edited: Vec<u64> = db
            .load()
            .unwrap()
            .bookmarks
            .iter()
            .map(|row| row.4)
            .collect();
        assert!(edited.contains(&8) && edited.contains(&7), "{edited:?}");
        db.remove_bookmark(HostId(1), BookmarkSide::Remote, "/opt")
            .unwrap();
        db.remove_bookmark(HostId(1), BookmarkSide::Remote, "/etc")
            .unwrap();
        db.remove_host(HostId(2)).unwrap();
        assert_eq!(
            paths(&db),
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
            "snippet_categories",
            "snippets",
            "terminal_groups",
            "saved_terminals",
            "saved_terminal_tabs",
            "secret_edits",
            "sync_state",
            "sync_pending",
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
