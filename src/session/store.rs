use gpui_kit::{Context, EventEmitter, SharedString};

use super::database::now_seconds;
use super::{
    AuthKind, ConnectionState, GroupDraft, GroupId, Session, SessionDatabase, SessionDraft,
    SessionGroup, SessionId, StoredData,
};

/// The single source of truth for sessions and groups. Created once by the
/// workspace and shared with every panel and dialog; consumers observe it.
///
/// Mutators that take a `Context` notify observers and mirror the change into
/// the database; the `*_unnotified` variants are pure in-memory and exist for
/// tests, for loading, and for callers that batch several changes before one
/// notification.
///
/// Memory is authoritative. A failed write costs persistence, never the edit:
/// the change stands and a `SessionStoreEvent::PersistFailed` goes out so the
/// workspace can tell the user.
pub struct SessionStore {
    groups: Vec<SessionGroup>,
    sessions: Vec<Session>,
    next_session_id: u64,
    next_group_id: u64,
    active: Option<SessionId>,
    /// Sessions in the order they last connected, most recent first.
    recent: Vec<SessionId>,
    /// `None` for a memory-only store, as used by tests.
    database: Option<SessionDatabase>,
}

/// How many sessions the start page lists as recently connected.
const MAX_RECENT: usize = 10;

/// What the store tells the workspace about, beyond plain change notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStoreEvent {
    /// A change was applied in memory but could not be written to disk.
    PersistFailed(SharedString),
    /// A live terminal must reconnect because its SSH endpoint or
    /// authentication configuration changed.
    ConnectionSettingsChanged(SessionId),
}

impl EventEmitter<SessionStoreEvent> for SessionStore {}

impl SessionStore {
    /// An empty memory-only store.
    pub fn empty() -> Self {
        Self {
            groups: Vec::new(),
            sessions: Vec::new(),
            next_session_id: 1,
            next_group_id: 1,
            active: None,
            recent: Vec::new(),
            database: None,
        }
    }

    /// Read everything back from `database` and keep writing to it. Ids carry
    /// over from disk, so the allocators resume past the largest stored id.
    pub fn load(database: SessionDatabase) -> rusqlite::Result<Self> {
        let StoredData {
            groups,
            sessions,
            mut recent,
        } = database.load()?;
        recent.truncate(MAX_RECENT);
        Ok(Self {
            next_group_id: groups.iter().map(|group| group.id.0).max().unwrap_or(0) + 1,
            next_session_id: sessions.iter().map(|s| s.id.0).max().unwrap_or(0) + 1,
            groups,
            sessions,
            active: None,
            recent,
            database: Some(database),
        })
    }

    /// Sample data for tests. Production starts from an empty database, so
    /// nothing outside `tests/` builds a store this way; the UI tests depend
    /// on the exact shape (ids 1..=6 in the order listed here).
    pub fn seed() -> Self {
        let mut store = Self::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let staging = store.insert_group_unnotified(GroupDraft::new("测试", None));
        let development = store.insert_group_unnotified(GroupDraft::new("开发", None));
        let drafts = [
            SessionDraft::new(
                "web-01",
                "10.0.1.12",
                22,
                "root",
                AuthKind::Auto,
                Some(production),
            ),
            SessionDraft::new(
                "web-02",
                "10.0.1.13",
                22,
                "root",
                AuthKind::Auto,
                Some(production),
            ),
            SessionDraft::new(
                "db-01",
                "10.0.2.5",
                22,
                "postgres",
                AuthKind::Password,
                Some(production),
            ),
            SessionDraft::new(
                "staging-api",
                "10.0.9.20",
                2222,
                "deploy",
                AuthKind::Auto,
                Some(staging),
            ),
            SessionDraft::new(
                "qa-runner",
                "10.0.9.31",
                22,
                "ci",
                AuthKind::Password,
                Some(staging),
            ),
            SessionDraft::new(
                "dev-box",
                "192.168.1.20",
                22,
                "xuz",
                AuthKind::Auto,
                Some(development),
            ),
        ];
        for draft in drafts {
            store.insert_unnotified(draft);
        }
        for name in ["web-01", "staging-api"] {
            if let Some(id) = store.sessions.iter().find(|s| s.name == name).map(|s| s.id) {
                store.set_state_unnotified(id, ConnectionState::Connected);
            }
        }
        store
    }

    pub fn groups(&self) -> &[SessionGroup] {
        &self.groups
    }

    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    pub fn session(&self, id: SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn group(&self, id: GroupId) -> Option<&SessionGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// The groups directly under `parent`, in insertion order. `None` asks
    /// for the top-level groups.
    pub fn child_groups(&self, parent: Option<GroupId>) -> impl Iterator<Item = &SessionGroup> {
        self.groups.iter().filter(move |g| g.parent == parent)
    }

    /// Every group below `id`, at any depth. Shared by the cascading delete
    /// and by the check that keeps a group from being moved into itself.
    pub fn descendant_groups(&self, id: GroupId) -> Vec<GroupId> {
        let mut found = Vec::new();
        let mut frontier = vec![id];
        while let Some(current) = frontier.pop() {
            for child in self.groups.iter().filter(|g| g.parent == Some(current)) {
                found.push(child.id);
                frontier.push(child.id);
            }
        }
        found
    }

    /// Every session inside `id` or any group below it. What deleting the
    /// group takes with it.
    pub fn sessions_under(&self, id: GroupId) -> Vec<SessionId> {
        let mut doomed = self.descendant_groups(id);
        doomed.push(id);
        self.sessions
            .iter()
            .filter(|session| session.group.is_some_and(|group| doomed.contains(&group)))
            .map(|session| session.id)
            .collect()
    }

    /// The groups above `id`, nearest parent first. Used to open the folders
    /// over a row that has to be revealed.
    pub fn ancestor_groups(&self, id: GroupId) -> Vec<GroupId> {
        let mut chain = Vec::new();
        let mut current = self.group(id).and_then(|group| group.parent);
        while let Some(parent) = current {
            if chain.len() > self.groups.len() {
                break;
            }
            chain.push(parent);
            current = self.group(parent).and_then(|group| group.parent);
        }
        chain
    }

    /// The group's full path, e.g. `生产 / 数据库`, as the forms list it.
    pub fn group_path(&self, id: GroupId) -> String {
        let mut parts: Vec<&str> = Vec::new();
        let mut current = Some(id);
        while let Some(group) = current.and_then(|id| self.group(id)) {
            parts.push(group.name.as_ref());
            // A cycle is rejected on the way in; refuse to hang if one ever
            // reaches here anyway.
            if parts.len() > self.groups.len() {
                break;
            }
            current = group.parent;
        }
        parts.reverse();
        parts.join(" / ")
    }

    /// The session whose tab is currently displayed, if any.
    pub fn active(&self) -> Option<&Session> {
        self.active.and_then(|id| self.session(id))
    }

    /// Sessions in the order they last connected, most recent first. A
    /// session joins (or moves to the front) each time it becomes connected.
    pub fn recent_sessions(&self) -> impl Iterator<Item = &Session> {
        self.recent.iter().filter_map(|id| self.session(*id))
    }

    pub fn insert(&mut self, draft: SessionDraft, cx: &mut Context<Self>) -> SessionId {
        let id = self.insert_unnotified(draft);
        if let (Some(database), Some(session)) = (self.database.as_ref(), self.session(id)) {
            let result = database.insert_session(session);
            self.report(result, "新建会话", cx);
        }
        cx.notify();
        id
    }

    pub fn insert_unnotified(&mut self, draft: SessionDraft) -> SessionId {
        let id = SessionId(self.next_session_id);
        self.next_session_id += 1;
        self.sessions.push(Session::new(id, draft));
        id
    }

    /// Replace the editable fields of a session; connection state is kept.
    pub fn update(&mut self, id: SessionId, draft: SessionDraft, cx: &mut Context<Self>) -> bool {
        let connection_changed = self.session(id).is_some_and(|session| {
            session.host != draft.host
                || session.port != draft.port
                || session.user != draft.user
                || session.auth != draft.auth
                || session.key_path != draft.key_path
        });
        let updated = self.update_unnotified(id, draft);
        if updated {
            if let (Some(database), Some(session)) = (self.database.as_ref(), self.session(id)) {
                let result = database.update_session(session);
                self.report(result, "保存会话", cx);
            }
            if connection_changed {
                cx.emit(SessionStoreEvent::ConnectionSettingsChanged(id));
            }
            cx.notify();
        }
        updated
    }

    pub fn update_unnotified(&mut self, id: SessionId, draft: SessionDraft) -> bool {
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) else {
            return false;
        };
        let state = session.state;
        *session = Session::new(id, draft);
        session.state = state;
        true
    }

    pub fn remove(&mut self, id: SessionId, cx: &mut Context<Self>) -> bool {
        let removed = self.remove_unnotified(id);
        if removed {
            if let Some(database) = self.database.as_ref() {
                let result = database.remove_session(id);
                self.report(result, "删除会话", cx);
            }
            cx.notify();
        }
        removed
    }

    pub fn remove_unnotified(&mut self, id: SessionId) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        self.recent.retain(|recent| *recent != id);
        if self.active == Some(id) {
            self.active = None;
        }
        self.sessions.len() != before
    }

    /// Copy a session as `<name> 副本`, placed right after the original.
    pub fn duplicate(&mut self, id: SessionId, cx: &mut Context<Self>) -> Option<SessionId> {
        let copy = self.duplicate_unnotified(id)?;
        if let (Some(database), Some(session)) = (self.database.as_ref(), self.session(copy)) {
            let result = database.insert_session(session);
            self.report(result, "复制会话", cx);
        }
        cx.notify();
        Some(copy)
    }

    pub fn duplicate_unnotified(&mut self, id: SessionId) -> Option<SessionId> {
        let ix = self.sessions.iter().position(|s| s.id == id)?;
        let mut draft = self.sessions[ix].draft();
        draft.name = format!("{} 副本", draft.name).into();
        let copy_id = SessionId(self.next_session_id);
        self.next_session_id += 1;
        self.sessions.insert(ix + 1, Session::new(copy_id, draft));
        Some(copy_id)
    }

    pub fn insert_group(&mut self, draft: GroupDraft, cx: &mut Context<Self>) -> GroupId {
        let id = self.insert_group_unnotified(draft);
        if let (Some(database), Some(group)) = (self.database.as_ref(), self.group(id)) {
            let result = database.insert_group(group);
            self.report(result, "新建分组", cx);
        }
        cx.notify();
        id
    }

    pub fn insert_group_unnotified(&mut self, draft: GroupDraft) -> GroupId {
        let id = GroupId(self.next_group_id);
        self.next_group_id += 1;
        self.groups.push(SessionGroup::new(id, draft));
        id
    }

    /// Rename a group, and optionally move it under a different parent.
    pub fn update_group(&mut self, id: GroupId, draft: GroupDraft, cx: &mut Context<Self>) -> bool {
        let updated = self.update_group_unnotified(id, draft);
        if updated {
            if let (Some(database), Some(group)) = (self.database.as_ref(), self.group(id)) {
                let result = database.update_group(group);
                self.report(result, "保存分组", cx);
            }
            cx.notify();
        }
        updated
    }

    /// Returns whether the group changed. Moving a group into its own subtree
    /// would cut that subtree off from the root, so it is refused here as well
    /// as being left out of the form's parent list.
    pub fn update_group_unnotified(&mut self, id: GroupId, draft: GroupDraft) -> bool {
        if self.group(id).is_none() {
            return false;
        }
        if let Some(parent) = draft.parent
            && (parent == id || self.descendant_groups(id).contains(&parent))
        {
            return false;
        }
        let Some(group) = self.groups.iter_mut().find(|g| g.id == id) else {
            return false;
        };
        *group = SessionGroup::new(id, draft);
        true
    }

    /// Delete a group with its subgroups and every session inside them.
    /// Returns the sessions that went with it, so the workspace can close
    /// their tabs.
    pub fn remove_group(&mut self, id: GroupId, cx: &mut Context<Self>) -> Vec<SessionId> {
        if self.group(id).is_none() {
            return Vec::new();
        }
        let removed = self.remove_group_unnotified(id);
        // One delete mirrors the whole subtree: both foreign keys cascade.
        if let Some(database) = self.database.as_ref() {
            let result = database.remove_group(id);
            self.report(result, "删除分组", cx);
        }
        cx.notify();
        removed
    }

    pub fn remove_group_unnotified(&mut self, id: GroupId) -> Vec<SessionId> {
        if self.group(id).is_none() {
            return Vec::new();
        }
        let removed = self.sessions_under(id);
        let mut doomed = self.descendant_groups(id);
        doomed.push(id);
        self.groups.retain(|group| !doomed.contains(&group.id));
        for session in &removed {
            self.remove_unnotified(*session);
        }
        removed
    }

    pub fn set_state(&mut self, id: SessionId, state: ConnectionState, cx: &mut Context<Self>) {
        if !self.set_state_unnotified(id, state) {
            return;
        }
        if state.is_connected()
            && let Some(database) = self.database.as_ref()
        {
            let result = database.touch_connected(id, now_seconds());
            self.report(result, "记录最近连接", cx);
        }
        cx.notify();
    }

    /// Returns whether the state changed. Becoming connected also moves the
    /// session to the front of the recent list.
    pub fn set_state_unnotified(&mut self, id: SessionId, state: ConnectionState) -> bool {
        let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) else {
            return false;
        };
        if session.state == state {
            return false;
        }
        session.state = state;
        if state.is_connected() {
            self.recent.retain(|recent| *recent != id);
            self.recent.insert(0, id);
            self.recent.truncate(MAX_RECENT);
        }
        true
    }

    pub fn set_active(&mut self, id: Option<SessionId>, cx: &mut Context<Self>) {
        if self.active != id {
            self.active = id;
            cx.notify();
        }
    }

    /// Turn a failed write into an event. The in-memory change stands.
    fn report(&self, result: rusqlite::Result<()>, action: &str, cx: &mut Context<Self>) {
        if let Err(error) = result {
            cx.emit(SessionStoreEvent::PersistFailed(
                format!("{action}未能保存到本地数据库：{error}").into(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(name: &str, group: Option<GroupId>) -> SessionDraft {
        SessionDraft::new(name, "10.0.0.1", 22, "root", AuthKind::Auto, group)
    }

    #[test]
    fn seed_has_three_groups_and_two_connected_sessions() {
        let store = SessionStore::seed();
        assert_eq!(store.groups().len(), 3);
        assert_eq!(store.sessions().len(), 6);
        let connected: Vec<_> = store
            .sessions()
            .iter()
            .filter(|s| s.state.is_connected())
            .map(|s| s.name.as_ref())
            .collect();
        assert_eq!(connected, ["web-01", "staging-api"]);
    }

    #[test]
    fn groups_and_sessions_have_independent_id_sequences() {
        let mut store = SessionStore::seed();
        let group_ids: Vec<_> = store.groups().iter().map(|g| g.id).collect();
        assert_eq!(group_ids, [GroupId(1), GroupId(2), GroupId(3)]);
        assert_eq!(store.sessions()[0].id, SessionId(1));
        assert_eq!(store.sessions()[5].id, SessionId(6));
        assert_eq!(
            store.insert_group_unnotified(GroupDraft::new("预发", None)),
            GroupId(4)
        );
        assert_eq!(store.insert_unnotified(draft("new", None)), SessionId(7));
    }

    #[test]
    fn insert_assigns_increasing_ids_and_keeps_order() {
        let mut store = SessionStore::empty();
        let a = store.insert_unnotified(draft("a", Some(GroupId(1))));
        let b = store.insert_unnotified(draft("b", Some(GroupId(1))));
        assert!(a < b);
        assert_eq!(store.sessions()[1].id, b);
    }

    #[test]
    fn duplicate_places_a_copy_after_the_original() {
        let mut store = SessionStore::seed();
        let web01 = store.sessions()[0].id;
        let mut draft = store.session(web01).unwrap().draft();
        draft.auth = AuthKind::Key;
        draft.key_path = Some("/tmp/id_ed25519".into());
        assert!(store.update_unnotified(web01, draft));
        let copy = store.duplicate_unnotified(web01).unwrap();
        assert_eq!(store.sessions()[1].id, copy);
        assert_eq!(store.sessions()[1].name.as_ref(), "web-01 副本");
        assert_eq!(
            store.sessions()[1].key_path.as_deref(),
            Some("/tmp/id_ed25519")
        );
        assert_eq!(store.sessions()[1].state, ConnectionState::Disconnected);
    }

    #[test]
    fn update_keeps_connection_state() {
        let mut store = SessionStore::seed();
        let web01 = store.sessions()[0].id;
        let mut draft = store.session(web01).unwrap().draft();
        draft.port = 2200;
        assert!(store.update_unnotified(web01, draft));
        let session = store.session(web01).unwrap();
        assert_eq!(session.port, 2200);
        assert_eq!(session.state, ConnectionState::Connected);
        assert!(!store.update_unnotified(SessionId(999), session.draft()));
    }

    #[test]
    fn remove_clears_active_session() {
        let mut store = SessionStore::seed();
        let web01 = store.sessions()[0].id;
        store.active = Some(web01);
        assert!(store.remove_unnotified(web01));
        assert!(store.active().is_none());
        assert!(!store.remove_unnotified(web01));
    }

    #[test]
    fn seed_lists_connected_sessions_as_recent_newest_first() {
        let store = SessionStore::seed();
        let recent: Vec<_> = store.recent_sessions().map(|s| s.name.as_ref()).collect();
        assert_eq!(recent, ["staging-api", "web-01"]);
    }

    #[test]
    fn connecting_moves_a_session_to_the_front_of_recent() {
        let mut store = SessionStore::seed();
        let web01 = store.sessions()[0].id;
        let db01 = store.sessions()[2].id;
        assert!(store.set_state_unnotified(db01, ConnectionState::Connected));
        // Already connected: nothing changes, nothing moves.
        assert!(!store.set_state_unnotified(db01, ConnectionState::Connected));
        assert!(store.set_state_unnotified(web01, ConnectionState::Disconnected));
        assert!(store.set_state_unnotified(web01, ConnectionState::Connected));
        let recent: Vec<_> = store.recent_sessions().map(|s| s.name.as_ref()).collect();
        assert_eq!(recent, ["web-01", "db-01", "staging-api"]);
    }

    #[test]
    fn remove_drops_the_session_from_recent() {
        let mut store = SessionStore::seed();
        let web01 = store.sessions()[0].id;
        assert!(store.remove_unnotified(web01));
        let recent: Vec<_> = store.recent_sessions().map(|s| s.name.as_ref()).collect();
        assert_eq!(recent, ["staging-api"]);
    }

    #[test]
    fn recent_keeps_only_the_newest_entries() {
        let mut store = SessionStore::empty();
        for ix in 0..(MAX_RECENT + 2) {
            let id = store.insert_unnotified(draft(&format!("s{ix}"), None));
            store.set_state_unnotified(id, ConnectionState::Connected);
        }
        assert_eq!(store.recent_sessions().count(), MAX_RECENT);
        let newest = store.recent_sessions().next().unwrap();
        assert_eq!(newest.name.as_ref(), "s11");
    }

    #[test]
    fn nested_groups_report_children_descendants_and_path() {
        let mut store = SessionStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        let replicas = store.insert_group_unnotified(GroupDraft::new("只读副本", Some(databases)));
        let staging = store.insert_group_unnotified(GroupDraft::new("测试", None));

        let top: Vec<_> = store.child_groups(None).map(|g| g.id).collect();
        assert_eq!(top, [production, staging]);
        let under_production: Vec<_> = store.child_groups(Some(production)).map(|g| g.id).collect();
        assert_eq!(under_production, [databases]);

        let mut descendants = store.descendant_groups(production);
        descendants.sort();
        assert_eq!(descendants, [databases, replicas]);
        assert!(store.descendant_groups(replicas).is_empty());

        assert_eq!(store.group_path(replicas), "生产 / 数据库 / 只读副本");
        assert_eq!(store.group_path(staging), "测试");
    }

    #[test]
    fn removing_a_group_takes_its_subtree_and_its_sessions() {
        let mut store = SessionStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        let staging = store.insert_group_unnotified(GroupDraft::new("测试", None));
        let web = store.insert_unnotified(draft("web-01", Some(production)));
        let db = store.insert_unnotified(draft("db-01", Some(databases)));
        let qa = store.insert_unnotified(draft("qa", Some(staging)));
        let jump = store.insert_unnotified(draft("jump", None));
        store.set_state_unnotified(db, ConnectionState::Connected);
        store.active = Some(db);

        let mut removed = store.remove_group_unnotified(production);
        removed.sort();
        assert_eq!(removed, [web, db]);
        assert_eq!(
            store.groups().iter().map(|g| g.id).collect::<Vec<_>>(),
            [staging]
        );
        // The root-level session and the untouched group's session survive.
        assert_eq!(
            store.sessions().iter().map(|s| s.id).collect::<Vec<_>>(),
            [qa, jump]
        );
        assert_eq!(store.recent_sessions().count(), 0);
        assert!(store.active().is_none());
        // Removing it a second time finds nothing left to do.
        assert!(store.remove_group_unnotified(production).is_empty());
    }

    #[test]
    fn a_group_cannot_be_moved_into_its_own_subtree() {
        let mut store = SessionStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        let replicas = store.insert_group_unnotified(GroupDraft::new("只读副本", Some(databases)));

        assert!(
            !store.update_group_unnotified(production, GroupDraft::new("生产", Some(replicas)))
        );
        assert!(
            !store.update_group_unnotified(production, GroupDraft::new("生产", Some(production)))
        );
        assert!(store.group(production).unwrap().parent.is_none());

        // Moving in the other direction, and plain renaming, are fine.
        assert!(store.update_group_unnotified(replicas, GroupDraft::new("副本", Some(production))));
        assert_eq!(store.group_path(replicas), "生产 / 副本");
        assert!(!store.update_group_unnotified(GroupId(99), GroupDraft::new("x", None)));
    }

    #[test]
    fn load_resumes_the_id_sequences_and_the_recent_order() {
        let database = SessionDatabase::in_memory().unwrap();
        database
            .insert_group(&SessionGroup::new(
                GroupId(4),
                GroupDraft::new("生产", None),
            ))
            .unwrap();
        database
            .insert_session(&Session::new(
                SessionId(7),
                draft("web-01", Some(GroupId(4))),
            ))
            .unwrap();
        database
            .insert_session(&Session::new(SessionId(9), draft("db-01", None)))
            .unwrap();
        database.touch_connected(SessionId(9), 100).unwrap();

        let mut store = SessionStore::load(database).unwrap();
        assert_eq!(store.groups().len(), 1);
        assert_eq!(
            store.recent_sessions().map(|s| s.id).collect::<Vec<_>>(),
            [SessionId(9)]
        );
        // Nothing is connected on load, however recently it last connected.
        assert!(store.sessions().iter().all(|s| !s.state.is_connected()));
        assert_eq!(
            store.insert_group_unnotified(GroupDraft::new("测试", None)),
            GroupId(5)
        );
        assert_eq!(store.insert_unnotified(draft("new", None)), SessionId(10));
    }
}
