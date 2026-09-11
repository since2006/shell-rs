//! Pure functions that turn the session store into tree items.

use std::collections::HashSet;

use gpui_kit::SharedString;
use gpui_kit::component::tree::TreeItem;

use super::{GroupId, Session, SessionGroup, SessionId};

/// What a tree row stands for. Encoded into the row's `TreeItem` id so the
/// renderer and the context menu can recover the domain object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionNode {
    Group(GroupId),
    Session(SessionId),
}

impl SessionNode {
    pub fn id(self) -> SharedString {
        match self {
            SessionNode::Group(GroupId(id)) => format!("g:{id}").into(),
            SessionNode::Session(SessionId(id)) => format!("s:{id}").into(),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        let (kind, number) = id.split_once(':')?;
        let number = number.parse().ok()?;
        match kind {
            "g" => Some(SessionNode::Group(GroupId(number))),
            "s" => Some(SessionNode::Session(SessionId(number))),
            _ => None,
        }
    }

    pub fn session_id(self) -> Option<SessionId> {
        match self {
            SessionNode::Session(id) => Some(id),
            SessionNode::Group(_) => None,
        }
    }
}

/// Case-insensitive match on name, host or user.
pub fn matches_query(session: &Session, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    [&session.name, &session.host, &session.user]
        .iter()
        .any(|field| field.to_lowercase().contains(&query))
}

/// Build the grouped tree. A non-empty query keeps only matching sessions,
/// drops groups without matches and expands the remaining groups; otherwise
/// `expanded` decides which groups are open.
pub fn session_tree_items(
    groups: &[SessionGroup],
    sessions: &[Session],
    query: &str,
    expanded: &HashSet<GroupId>,
) -> Vec<TreeItem> {
    let filtering = !query.trim().is_empty();
    groups
        .iter()
        .filter_map(|group| {
            let children: Vec<TreeItem> = sessions
                .iter()
                .filter(|s| s.group == group.id && matches_query(s, query))
                .map(|s| TreeItem::new(SessionNode::Session(s.id).id(), s.name.clone()))
                .collect();
            if filtering && children.is_empty() {
                return None;
            }
            Some(
                TreeItem::new(SessionNode::Group(group.id).id(), group.name.clone())
                    .expanded(filtering || expanded.contains(&group.id))
                    .children(children),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionStore;

    fn all_groups(store: &SessionStore) -> HashSet<GroupId> {
        store.groups().iter().map(|g| g.id).collect()
    }

    #[test]
    fn node_id_round_trips() {
        for node in [
            SessionNode::Group(GroupId(7)),
            SessionNode::Session(SessionId(42)),
        ] {
            assert_eq!(SessionNode::parse(&node.id()), Some(node));
        }
        assert_eq!(SessionNode::parse("x:1"), None);
        assert_eq!(SessionNode::parse("s:abc"), None);
    }

    #[test]
    fn empty_query_keeps_every_group_and_session() {
        let store = SessionStore::seed();
        let items = session_tree_items(store.groups(), store.sessions(), "", &all_groups(&store));
        assert_eq!(items.len(), 3);
        let total: usize = items.iter().map(|g| g.children.len()).sum();
        assert_eq!(total, 6);
        assert!(items.iter().all(|g| g.is_expanded()));
    }

    #[test]
    fn collapsed_groups_follow_the_expanded_set() {
        let store = SessionStore::seed();
        let items = session_tree_items(store.groups(), store.sessions(), "", &HashSet::new());
        assert!(items.iter().all(|g| !g.is_expanded()));
    }

    #[test]
    fn query_matches_host_and_user_case_insensitively() {
        let store = SessionStore::seed();
        let by_host =
            session_tree_items(store.groups(), store.sessions(), "10.0.9", &HashSet::new());
        let names: Vec<_> = by_host
            .iter()
            .flat_map(|g| g.children.iter().map(|c| c.label.to_string()))
            .collect();
        assert_eq!(names, ["staging-api", "qa-runner"]);

        let by_user = session_tree_items(
            store.groups(),
            store.sessions(),
            "POSTGRES",
            &HashSet::new(),
        );
        assert_eq!(by_user.len(), 1);
        assert_eq!(by_user[0].children[0].label.as_ref(), "db-01");
    }

    #[test]
    fn query_drops_groups_without_matches_and_expands_the_rest() {
        let store = SessionStore::seed();
        let items = session_tree_items(store.groups(), store.sessions(), "dev", &HashSet::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label.as_ref(), "开发");
        assert!(items[0].is_expanded());
    }
}
