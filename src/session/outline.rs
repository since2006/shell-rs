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

    pub fn group_id(self) -> Option<GroupId> {
        match self {
            SessionNode::Group(id) => Some(id),
            SessionNode::Session(_) => None,
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

/// Build the nested tree. Each level lists its subgroups first, then the
/// sessions that belong to it; sessions with no group sit at the root beside
/// the top-level groups. A non-empty query keeps only matching sessions, drops
/// the groups left without any, and expands what remains; otherwise
/// `expanded` decides which groups are open.
pub fn session_tree_items(
    groups: &[SessionGroup],
    sessions: &[Session],
    query: &str,
    expanded: &HashSet<GroupId>,
) -> Vec<TreeItem> {
    let filtering = !query.trim().is_empty();
    items_under(None, 0, groups, sessions, query, filtering, expanded)
}

fn items_under(
    parent: Option<GroupId>,
    depth: usize,
    groups: &[SessionGroup],
    sessions: &[Session],
    query: &str,
    filtering: bool,
    expanded: &HashSet<GroupId>,
) -> Vec<TreeItem> {
    // Nesting cannot run deeper than the number of groups unless the data has
    // a cycle, which the store refuses to create. Stop rather than recurse
    // forever if a hand-edited database ever produces one.
    if depth > groups.len() {
        return Vec::new();
    }
    let mut items: Vec<TreeItem> = Vec::new();
    for group in groups.iter().filter(|group| group.parent == parent) {
        let children = items_under(
            Some(group.id),
            depth + 1,
            groups,
            sessions,
            query,
            filtering,
            expanded,
        );
        if filtering && children.is_empty() {
            continue;
        }
        items.push(
            TreeItem::new(SessionNode::Group(group.id).id(), group.name.clone())
                .expanded(filtering || expanded.contains(&group.id))
                .children(children),
        );
    }
    items.extend(
        sessions
            .iter()
            .filter(|s| s.group == parent && matches_query(s, query))
            .map(|s| TreeItem::new(SessionNode::Session(s.id).id(), s.name.clone())),
    );
    items
}

/// Groups flattened depth first, each labelled by its full path (e.g.
/// `生产 / 数据库`), for the group pickers in the forms. An excluded group is
/// skipped along with everything below it, which is how the rename dialog
/// keeps a group from being moved into its own subtree.
pub fn group_options(
    groups: &[SessionGroup],
    excluded: &[GroupId],
) -> Vec<(GroupId, SharedString)> {
    let mut options = Vec::new();
    collect_group_options(groups, None, "", 0, excluded, &mut options);
    options
}

fn collect_group_options(
    groups: &[SessionGroup],
    parent: Option<GroupId>,
    prefix: &str,
    depth: usize,
    excluded: &[GroupId],
    options: &mut Vec<(GroupId, SharedString)>,
) {
    if depth > groups.len() {
        return;
    }
    for group in groups.iter().filter(|group| group.parent == parent) {
        if excluded.contains(&group.id) {
            continue;
        }
        let path = if prefix.is_empty() {
            group.name.to_string()
        } else {
            format!("{prefix} / {}", group.name)
        };
        options.push((group.id, path.as_str().into()));
        collect_group_options(groups, Some(group.id), &path, depth + 1, excluded, options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AuthKind, GroupDraft, SessionDraft, SessionStore};

    fn all_groups(store: &SessionStore) -> HashSet<GroupId> {
        store.groups().iter().map(|g| g.id).collect()
    }

    fn labels(items: &[TreeItem]) -> Vec<String> {
        items.iter().map(|item| item.label.to_string()).collect()
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

    /// 生产 / 数据库 with one session each, plus a session at the root.
    fn nested_store() -> SessionStore {
        let mut store = SessionStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        for (name, group) in [
            ("web-01", Some(production)),
            ("db-01", Some(databases)),
            ("jump", None),
        ] {
            store.insert_unnotified(SessionDraft::new(
                name,
                "10.0.0.1",
                22,
                "root",
                AuthKind::Key,
                group,
            ));
        }
        store
    }

    #[test]
    fn subgroups_nest_and_ungrouped_sessions_sit_at_the_root() {
        let store = nested_store();
        let items = session_tree_items(store.groups(), store.sessions(), "", &all_groups(&store));
        // A top-level group, then the session that belongs to no group.
        assert_eq!(labels(&items), ["生产", "jump"]);
        // Inside 生产: the subgroup first, then its own session.
        assert_eq!(labels(&items[0].children), ["数据库", "web-01"]);
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);
    }

    #[test]
    fn a_match_deep_in_the_tree_keeps_its_whole_ancestor_chain() {
        let store = nested_store();
        let items = session_tree_items(store.groups(), store.sessions(), "db-01", &HashSet::new());
        assert_eq!(labels(&items), ["生产"]);
        assert!(items[0].is_expanded());
        assert_eq!(labels(&items[0].children), ["数据库"]);
        assert!(items[0].children[0].is_expanded());
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);
    }

    #[test]
    fn group_options_are_depth_first_and_skip_an_excluded_subtree() {
        let mut store = SessionStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        store.insert_group_unnotified(GroupDraft::new("只读副本", Some(databases)));
        store.insert_group_unnotified(GroupDraft::new("测试", None));

        let paths: Vec<_> = group_options(store.groups(), &[])
            .into_iter()
            .map(|(_, path)| path.to_string())
            .collect();
        assert_eq!(
            paths,
            ["生产", "生产 / 数据库", "生产 / 数据库 / 只读副本", "测试"]
        );

        // Renaming 数据库: neither it nor its children may become its parent.
        let mut excluded = store.descendant_groups(databases);
        excluded.push(databases);
        let paths: Vec<_> = group_options(store.groups(), &excluded)
            .into_iter()
            .map(|(_, path)| path.to_string())
            .collect();
        assert_eq!(paths, ["生产", "测试"]);
    }

    #[test]
    fn a_query_matching_only_a_root_session_drops_every_group() {
        let store = nested_store();
        let items = session_tree_items(store.groups(), store.sessions(), "jump", &HashSet::new());
        assert_eq!(labels(&items), ["jump"]);
    }
}
