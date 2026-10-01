//! Pure functions that turn the host store into tree items.

use std::collections::HashSet;

use gpui_kit::SharedString;
use gpui_kit::component::tree::TreeItem;

use super::{GroupId, Host, HostGroup, HostId};

/// What a tree row stands for. Encoded into the row's `TreeItem` id so the
/// renderer and the context menu can recover the domain object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostNode {
    Group(GroupId),
    Host(HostId),
}

/// A drop location in the host tree. Groups and hosts each have their
/// own order within a parent; a drop into a group changes the parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeDrop {
    Before(HostNode),
    After(HostNode),
    Into(GroupId),
    Root,
}

impl HostNode {
    pub fn id(self) -> SharedString {
        match self {
            HostNode::Group(GroupId(id)) => format!("g:{id}").into(),
            HostNode::Host(HostId(id)) => format!("s:{id}").into(),
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        let (kind, number) = id.split_once(':')?;
        let number = number.parse().ok()?;
        match kind {
            "g" => Some(HostNode::Group(GroupId(number))),
            "s" => Some(HostNode::Host(HostId(number))),
            _ => None,
        }
    }

    pub fn host_id(self) -> Option<HostId> {
        match self {
            HostNode::Host(id) => Some(id),
            HostNode::Group(_) => None,
        }
    }

    pub fn group_id(self) -> Option<GroupId> {
        match self {
            HostNode::Group(id) => Some(id),
            HostNode::Host(_) => None,
        }
    }
}

/// Case-insensitive match on name, host or user.
pub fn matches_query(host: &Host, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    [&host.name, &host.address, &host.user]
        .iter()
        .any(|field| field.to_lowercase().contains(&query))
}

/// Build the nested tree. Each level lists its subgroups first, then the
/// hosts that belong to it; hosts with no group sit at the root beside
/// the top-level groups. A non-empty query keeps only matching hosts, drops
/// the groups left without any, and expands what remains; otherwise
/// `expanded` decides which groups are open.
pub fn host_tree_items(
    groups: &[HostGroup],
    hosts: &[Host],
    query: &str,
    expanded: &HashSet<GroupId>,
) -> Vec<TreeItem> {
    let filtering = !query.trim().is_empty();
    items_under(None, 0, groups, hosts, query, filtering, expanded)
}

/// The groups directly under `parent`, in display order. `None` asks for the
/// top-level groups.
fn child_groups(groups: &[HostGroup], parent: Option<GroupId>) -> Vec<&HostGroup> {
    let mut children: Vec<_> = groups
        .iter()
        .filter(|group| group.parent == parent)
        .collect();
    children.sort_by_key(|group| (group.sort_order, group.id));
    children
}

fn items_under(
    parent: Option<GroupId>,
    depth: usize,
    groups: &[HostGroup],
    hosts: &[Host],
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
    for group in child_groups(groups, parent) {
        let children = items_under(
            Some(group.id),
            depth + 1,
            groups,
            hosts,
            query,
            filtering,
            expanded,
        );
        if filtering && children.is_empty() {
            continue;
        }
        items.push(
            TreeItem::new(HostNode::Group(group.id).id(), group.name.clone())
                .expanded(filtering || expanded.contains(&group.id))
                .children(children),
        );
    }
    let mut child_hosts: Vec<_> = hosts
        .iter()
        .filter(|s| s.group == parent && matches_query(s, query))
        .collect();
    child_hosts.sort_by_key(|host| (host.sort_order, host.id));
    items.extend(
        child_hosts
            .into_iter()
            .map(|host| TreeItem::new(HostNode::Host(host.id).id(), host.name.clone())),
    );
    items
}

/// Groups flattened depth first, each labelled by its full path (e.g.
/// `生产 / 数据库`), for the group pickers in the forms. An excluded group is
/// skipped along with everything below it, which is how the rename dialog
/// keeps a group from being moved into its own subtree.
pub fn group_options(groups: &[HostGroup], excluded: &[GroupId]) -> Vec<(GroupId, SharedString)> {
    let mut options = Vec::new();
    collect_group_options(groups, None, "", 0, excluded, &mut options);
    options
}

fn collect_group_options(
    groups: &[HostGroup],
    parent: Option<GroupId>,
    prefix: &str,
    depth: usize,
    excluded: &[GroupId],
    options: &mut Vec<(GroupId, SharedString)>,
) {
    if depth > groups.len() {
        return;
    }
    for group in child_groups(groups, parent) {
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
    use crate::host::{AuthKind, GroupDraft, HostDraft, HostStore};

    fn all_groups(store: &HostStore) -> HashSet<GroupId> {
        store.groups().iter().map(|g| g.id).collect()
    }

    fn labels(items: &[TreeItem]) -> Vec<String> {
        items.iter().map(|item| item.label.to_string()).collect()
    }

    #[test]
    fn node_id_round_trips() {
        for node in [HostNode::Group(GroupId(7)), HostNode::Host(HostId(42))] {
            assert_eq!(HostNode::parse(&node.id()), Some(node));
        }
        assert_eq!(HostNode::parse("x:1"), None);
        assert_eq!(HostNode::parse("s:abc"), None);
    }

    #[test]
    fn empty_query_keeps_every_group_and_host() {
        let store = HostStore::seed();
        let items = host_tree_items(store.groups(), store.hosts(), "", &all_groups(&store));
        assert_eq!(items.len(), 3);
        let total: usize = items.iter().map(|g| g.children.len()).sum();
        assert_eq!(total, 6);
        assert!(items.iter().all(|g| g.is_expanded()));
    }

    #[test]
    fn collapsed_groups_follow_the_expanded_set() {
        let store = HostStore::seed();
        let items = host_tree_items(store.groups(), store.hosts(), "", &HashSet::new());
        assert!(items.iter().all(|g| !g.is_expanded()));
    }

    #[test]
    fn query_matches_host_and_user_case_insensitively() {
        let store = HostStore::seed();
        let by_host = host_tree_items(store.groups(), store.hosts(), "10.0.9", &HashSet::new());
        let names: Vec<_> = by_host
            .iter()
            .flat_map(|g| g.children.iter().map(|c| c.label.to_string()))
            .collect();
        assert_eq!(names, ["staging-api", "qa-runner"]);

        let by_user = host_tree_items(store.groups(), store.hosts(), "POSTGRES", &HashSet::new());
        assert_eq!(by_user.len(), 1);
        assert_eq!(by_user[0].children[0].label.as_ref(), "db-01");
    }

    #[test]
    fn query_drops_groups_without_matches_and_expands_the_rest() {
        let store = HostStore::seed();
        let items = host_tree_items(store.groups(), store.hosts(), "dev", &HashSet::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label.as_ref(), "开发");
        assert!(items[0].is_expanded());
    }

    /// 生产 / 数据库 with one host each, plus a host at the root.
    fn nested_store() -> HostStore {
        let mut store = HostStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        for (name, group) in [
            ("web-01", Some(production)),
            ("db-01", Some(databases)),
            ("jump", None),
        ] {
            store.insert_unnotified(HostDraft::new(
                name,
                "10.0.0.1",
                22,
                "root",
                AuthKind::NoPassword,
                group,
            ));
        }
        store
    }

    #[test]
    fn subgroups_nest_and_ungrouped_hosts_sit_at_the_root() {
        let store = nested_store();
        let items = host_tree_items(store.groups(), store.hosts(), "", &all_groups(&store));
        // A top-level group, then the host that belongs to no group.
        assert_eq!(labels(&items), ["生产", "jump"]);
        // Inside 生产: the subgroup first, then its own host.
        assert_eq!(labels(&items[0].children), ["数据库", "web-01"]);
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);
    }

    #[test]
    fn a_match_deep_in_the_tree_keeps_its_whole_ancestor_chain() {
        let store = nested_store();
        let items = host_tree_items(store.groups(), store.hosts(), "db-01", &HashSet::new());
        assert_eq!(labels(&items), ["生产"]);
        assert!(items[0].is_expanded());
        assert_eq!(labels(&items[0].children), ["数据库"]);
        assert!(items[0].children[0].is_expanded());
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);
    }

    #[test]
    fn group_options_are_depth_first_and_skip_an_excluded_subtree() {
        let mut store = HostStore::empty();
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
    fn a_query_matching_only_a_root_host_drops_every_group() {
        let store = nested_store();
        let items = host_tree_items(store.groups(), store.hosts(), "jump", &HashSet::new());
        assert_eq!(labels(&items), ["jump"]);
    }
}
