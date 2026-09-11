use gpui_kit::Context;

use super::{AuthKind, ConnectionState, GroupId, Session, SessionDraft, SessionGroup, SessionId};

/// The single source of truth for sessions and groups. Created once by the
/// workspace and shared with every panel and dialog; consumers observe it.
///
/// Mutators that take a `Context` notify observers; the `*_unnotified`
/// variants exist for pure tests and for callers that batch several changes
/// before one notification.
pub struct SessionStore {
    groups: Vec<SessionGroup>,
    sessions: Vec<Session>,
    next_id: u64,
    active: Option<SessionId>,
    /// Sessions in the order they last connected, most recent first.
    recent: Vec<SessionId>,
}

/// How many sessions the start page lists as recently connected.
const MAX_RECENT: usize = 10;

impl SessionStore {
    pub fn empty() -> Self {
        Self {
            groups: Vec::new(),
            sessions: Vec::new(),
            next_id: 1,
            active: None,
            recent: Vec::new(),
        }
    }

    /// The mock data every launch starts with.
    pub fn seed() -> Self {
        let production = GroupId(1);
        let staging = GroupId(2);
        let development = GroupId(3);
        let mut store = Self::empty();
        store.groups = vec![
            SessionGroup::new(production, "生产"),
            SessionGroup::new(staging, "测试"),
            SessionGroup::new(development, "开发"),
        ];
        let drafts = [
            SessionDraft::new("web-01", "10.0.1.12", 22, "root", AuthKind::Key, production),
            SessionDraft::new("web-02", "10.0.1.13", 22, "root", AuthKind::Key, production),
            SessionDraft::new(
                "db-01",
                "10.0.2.5",
                22,
                "postgres",
                AuthKind::Password,
                production,
            ),
            SessionDraft::new(
                "staging-api",
                "10.0.9.20",
                2222,
                "deploy",
                AuthKind::Key,
                staging,
            ),
            SessionDraft::new(
                "qa-runner",
                "10.0.9.31",
                22,
                "ci",
                AuthKind::Password,
                staging,
            ),
            SessionDraft::new(
                "dev-box",
                "192.168.1.20",
                22,
                "xuz",
                AuthKind::Key,
                development,
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
        cx.notify();
        id
    }

    pub fn insert_unnotified(&mut self, draft: SessionDraft) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id += 1;
        self.sessions.push(Session::new(id, draft));
        id
    }

    /// Replace the editable fields of a session; connection state is kept.
    pub fn update(&mut self, id: SessionId, draft: SessionDraft, cx: &mut Context<Self>) -> bool {
        let updated = self.update_unnotified(id, draft);
        if updated {
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
        let copy = self.duplicate_unnotified(id);
        if copy.is_some() {
            cx.notify();
        }
        copy
    }

    pub fn duplicate_unnotified(&mut self, id: SessionId) -> Option<SessionId> {
        let ix = self.sessions.iter().position(|s| s.id == id)?;
        let mut draft = self.sessions[ix].draft();
        draft.name = format!("{} 副本", draft.name).into();
        let copy_id = SessionId(self.next_id);
        self.next_id += 1;
        self.sessions.insert(ix + 1, Session::new(copy_id, draft));
        Some(copy_id)
    }

    pub fn set_state(&mut self, id: SessionId, state: ConnectionState, cx: &mut Context<Self>) {
        if self.set_state_unnotified(id, state) {
            cx.notify();
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn insert_assigns_increasing_ids_and_keeps_order() {
        let mut store = SessionStore::empty();
        let a = store.insert_unnotified(SessionDraft::new(
            "a",
            "h",
            22,
            "u",
            AuthKind::Key,
            GroupId(1),
        ));
        let b = store.insert_unnotified(SessionDraft::new(
            "b",
            "h",
            22,
            "u",
            AuthKind::Key,
            GroupId(1),
        ));
        assert!(a < b);
        assert_eq!(store.sessions()[1].id, b);
    }

    #[test]
    fn duplicate_places_a_copy_after_the_original() {
        let mut store = SessionStore::seed();
        let web01 = store.sessions()[0].id;
        let copy = store.duplicate_unnotified(web01).unwrap();
        assert_eq!(store.sessions()[1].id, copy);
        assert_eq!(store.sessions()[1].name.as_ref(), "web-01 副本");
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
            let draft =
                SessionDraft::new(format!("s{ix}"), "h", 22, "u", AuthKind::Key, GroupId(1));
            let id = store.insert_unnotified(draft);
            store.set_state_unnotified(id, ConnectionState::Connected);
        }
        assert_eq!(store.recent_sessions().count(), MAX_RECENT);
        let newest = store.recent_sessions().next().unwrap();
        assert_eq!(newest.name.as_ref(), "s11");
    }
}
