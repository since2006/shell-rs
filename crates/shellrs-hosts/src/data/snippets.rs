//! The command snippets' part of `HostData`: the snippets and their
//! categories, written through like everything else.

use std::collections::HashSet;

use super::{Change, HostData, edited_after, reordered};
use crate::{
    PublicId, Snippet, SnippetCategory, SnippetCategoryDraft, SnippetCategoryId, SnippetDraft,
    SnippetDrop, SnippetId, SnippetNode, SnippetScope, listed_order,
};

impl HostData {
    /// Every snippet, in the order they were made.
    pub fn snippets(&self) -> &[Snippet] {
        &self.snippets
    }

    pub fn snippet(&self, id: SnippetId) -> Option<&Snippet> {
        self.snippets.iter().find(|snippet| snippet.id == id)
    }

    /// Every snippet category, in the order they were made.
    pub fn snippet_categories(&self) -> &[SnippetCategory] {
        &self.snippet_categories
    }

    pub fn snippet_category(&self, id: SnippetCategoryId) -> Option<&SnippetCategory> {
        self.snippet_categories
            .iter()
            .find(|category| category.id == id)
    }

    /// The categories directly in `parent` (`None`: at the top), in the
    /// order they are listed.
    pub fn snippet_categories_in(
        &self,
        parent: Option<SnippetCategoryId>,
    ) -> Vec<&SnippetCategory> {
        let mut categories: Vec<_> = self
            .snippet_categories
            .iter()
            .filter(|category| category.parent == parent)
            .collect();
        categories.sort_by(|a, b| {
            listed_order(
                (a.sort_order, &a.name, a.id.0),
                (b.sort_order, &b.name, b.id.0),
            )
        });
        categories
    }

    /// The snippets directly in `category` (`None`: at the top), in the
    /// order they are listed.
    pub fn snippets_of(&self, category: Option<SnippetCategoryId>) -> Vec<&Snippet> {
        let mut snippets: Vec<_> = self
            .snippets
            .iter()
            .filter(|snippet| snippet.category == category)
            .collect();
        snippets.sort_by(|a, b| {
            listed_order(
                (a.sort_order, &a.name, a.id.0),
                (b.sort_order, &b.name, b.id.0),
            )
        });
        snippets
    }

    /// Every category inside `id`, at any depth.
    pub fn descendant_snippet_categories(&self, id: SnippetCategoryId) -> Vec<SnippetCategoryId> {
        let mut found = Vec::new();
        let mut seen = HashSet::from([id]);
        let mut frontier = vec![id];
        while let Some(current) = frontier.pop() {
            for child in &self.snippet_categories {
                // A hand-edited database may hold a cycle: each once.
                if child.parent == Some(current) && seen.insert(child.id) {
                    found.push(child.id);
                    frontier.push(child.id);
                }
            }
        }
        found
    }

    /// The categories above `id`, nearest first: what has to be unfolded
    /// for its row to show.
    pub fn ancestor_snippet_categories(&self, id: SnippetCategoryId) -> Vec<SnippetCategoryId> {
        let mut chain = Vec::new();
        let mut current = self
            .snippet_category(id)
            .and_then(|category| category.parent);
        while let Some(parent) = current {
            if chain.len() > self.snippet_categories.len() || parent == id {
                break;
            }
            chain.push(parent);
            current = self
                .snippet_category(parent)
                .and_then(|category| category.parent);
        }
        chain
    }

    /// How many snippets are in `category` and the categories inside it:
    /// what deleting it takes along, with its subcategories.
    pub fn snippets_in(&self, category: SnippetCategoryId) -> usize {
        let mut doomed = self.descendant_snippet_categories(category);
        doomed.push(category);
        self.snippets
            .iter()
            .filter(|snippet| snippet.category.is_some_and(|id| doomed.contains(&id)))
            .count()
    }

    pub fn insert_snippet(&mut self, draft: SnippetDraft) -> SnippetId {
        let id = self.insert_snippet_unnotified(draft);
        if let Some(snippet) = self.snippet(id) {
            self.persist(Change::NewSnippet, |db| db.insert_snippet(snippet));
        }
        self.changed();
        id
    }

    /// A new snippet goes last in its category.
    pub fn insert_snippet_unnotified(&mut self, draft: SnippetDraft) -> SnippetId {
        let id = SnippetId(self.next_snippet_id);
        self.next_snippet_id += 1;
        let draft = self.normalized_snippet(draft);
        let sort_order = self.last_snippet_order(draft.category, None);
        let mut snippet = Snippet::new(id, draft);
        snippet.sort_order = sort_order;
        snippet.sync_id = PublicId::generate_unused(|candidate| {
            self.snippets
                .iter()
                .any(|snippet| &snippet.sync_id == candidate)
        });
        snippet.edited_ms = edited_after(0);
        self.snippets.push(snippet);
        self.sync_revision += 1;
        id
    }

    pub fn update_snippet(&mut self, id: SnippetId, draft: SnippetDraft) -> bool {
        if !self.update_snippet_unnotified(id, draft) {
            return false;
        }
        if let Some(snippet) = self.snippet(id) {
            self.persist(Change::SaveSnippet, |db| db.update_snippet(snippet));
        }
        self.changed();
        true
    }

    /// The snippet keeps its place, or goes last in another category.
    pub fn update_snippet_unnotified(&mut self, id: SnippetId, draft: SnippetDraft) -> bool {
        let draft = self.normalized_snippet(draft);
        let last = self.last_snippet_order(draft.category, Some(id));
        let Some(snippet) = self.snippets.iter_mut().find(|snippet| snippet.id == id) else {
            return false;
        };
        let sync_id = snippet.sync_id.clone();
        let edited_ms = snippet.edited_ms;
        let sort_order = if snippet.category == draft.category {
            snippet.sort_order
        } else {
            last
        };
        *snippet = Snippet::new(id, draft);
        snippet.sort_order = sort_order;
        snippet.sync_id = sync_id;
        snippet.edited_ms = edited_after(edited_ms);
        self.sync_revision += 1;
        true
    }

    pub fn remove_snippet(&mut self, id: SnippetId) -> bool {
        if !self.remove_snippet_unnotified(id) {
            return false;
        }
        self.persist(Change::DeleteSnippet, |db| db.remove_snippet(id));
        self.changed();
        true
    }

    pub fn remove_snippet_unnotified(&mut self, id: SnippetId) -> bool {
        let before = self.snippets.len();
        self.snippets.retain(|snippet| snippet.id != id);
        if self.snippets.len() == before {
            return false;
        }
        self.sync_revision += 1;
        true
    }

    pub fn insert_snippet_category(&mut self, draft: SnippetCategoryDraft) -> SnippetCategoryId {
        let id = self.insert_snippet_category_unnotified(draft);
        if let Some(category) = self.snippet_category(id) {
            self.persist(Change::NewSnippetCategory, |db| {
                db.insert_snippet_category(category)
            });
        }
        self.changed();
        id
    }

    /// A new category goes last among those beside it; one in a category
    /// no longer there goes to the top.
    pub fn insert_snippet_category_unnotified(
        &mut self,
        mut draft: SnippetCategoryDraft,
    ) -> SnippetCategoryId {
        let id = SnippetCategoryId(self.next_snippet_category_id);
        self.next_snippet_category_id += 1;
        if draft
            .parent
            .is_some_and(|parent| self.snippet_category(parent).is_none())
        {
            draft.parent = None;
        }
        let sort_order = self.last_category_order(draft.parent, None);
        let mut category = SnippetCategory::new(id, draft);
        category.sort_order = sort_order;
        category.sync_id = PublicId::generate_unused(|candidate| {
            self.snippet_categories
                .iter()
                .any(|category| &category.sync_id == candidate)
        });
        category.edited_ms = edited_after(0);
        self.snippet_categories.push(category);
        self.sync_revision += 1;
        id
    }

    /// Rename a category, or move it into another: refused into itself or
    /// a category inside it, which would cut it off from the top.
    pub fn update_snippet_category(
        &mut self,
        id: SnippetCategoryId,
        draft: SnippetCategoryDraft,
    ) -> bool {
        if !self.update_snippet_category_unnotified(id, draft) {
            return false;
        }
        if let Some(category) = self.snippet_category(id) {
            self.persist(Change::SaveSnippetCategory, |db| {
                db.update_snippet_category(category)
            });
        }
        self.changed();
        true
    }

    /// The category keeps its place, or goes last in another.
    pub fn update_snippet_category_unnotified(
        &mut self,
        id: SnippetCategoryId,
        draft: SnippetCategoryDraft,
    ) -> bool {
        if self.snippet_category(id).is_none() {
            return false;
        }
        if let Some(parent) = draft.parent
            && (parent == id
                || self.snippet_category(parent).is_none()
                || self.descendant_snippet_categories(id).contains(&parent))
        {
            return false;
        }
        let last = self.last_category_order(draft.parent, Some(id));
        let Some(category) = self
            .snippet_categories
            .iter_mut()
            .find(|category| category.id == id)
        else {
            return false;
        };
        if category.parent != draft.parent {
            category.sort_order = last;
        }
        category.name = draft.name;
        category.parent = draft.parent;
        category.edited_ms = edited_after(category.edited_ms);
        self.sync_revision += 1;
        true
    }

    /// Delete a category with the categories inside it and every snippet
    /// in them.
    pub fn remove_snippet_category(&mut self, id: SnippetCategoryId) -> bool {
        if !self.remove_snippet_category_unnotified(id) {
            return false;
        }
        // The database takes the rest through `ON DELETE CASCADE`.
        self.persist(Change::DeleteSnippetCategory, |db| {
            db.remove_snippet_category(id)
        });
        self.changed();
        true
    }

    pub fn remove_snippet_category_unnotified(&mut self, id: SnippetCategoryId) -> bool {
        if self.snippet_category(id).is_none() {
            return false;
        }
        let mut doomed = self.descendant_snippet_categories(id);
        doomed.push(id);
        self.snippet_categories
            .retain(|category| !doomed.contains(&category.id));
        self.snippets
            .retain(|snippet| !snippet.category.is_some_and(|c| doomed.contains(&c)));
        self.sync_revision += 1;
        true
    }

    /// A row of 命令片段 dropped where it goes: a category beside another
    /// or into one, a snippet beside another, into a category or to the
    /// top. Every row whose place changed is taken as edited.
    pub fn move_snippet_node(&mut self, source: SnippetNode, drop: SnippetDrop) -> bool {
        if !self.move_snippet_node_unnotified(source, drop) {
            return false;
        }
        self.persist(Change::MoveSnippetNode, |db| {
            db.save_snippet_order(&self.snippet_categories, &self.snippets)
        });
        self.changed();
        true
    }

    pub fn move_snippet_node_unnotified(&mut self, source: SnippetNode, drop: SnippetDrop) -> bool {
        let (parent, target, after) = match drop {
            SnippetDrop::Before(target) => (self.snippet_node_parent(target), Some(target), false),
            SnippetDrop::After(target) => (self.snippet_node_parent(target), Some(target), true),
            SnippetDrop::Into(category) if self.snippet_category(category).is_some() => {
                (Some(Some(category)), None, false)
            }
            SnippetDrop::Top => (Some(None), None, false),
            SnippetDrop::Into(_) => return false,
        };
        let Some(parent) = parent else {
            return false;
        };
        if target == Some(source) {
            return false;
        }
        match source {
            SnippetNode::Category(id) => {
                if self.snippet_category(id).is_none()
                    || parent == Some(id)
                    || parent.is_some_and(|parent| {
                        self.descendant_snippet_categories(id).contains(&parent)
                    })
                {
                    return false;
                }
                let target = match target {
                    Some(SnippetNode::Category(target)) => Some(target),
                    Some(SnippetNode::Snippet(_)) => return false,
                    None => None,
                };
                let siblings = ranked(self.snippet_categories_in(parent).iter().map(|c| c.id));
                let Some(ids) = reordered(siblings, id, target, after) else {
                    return false;
                };
                for category in &mut self.snippet_categories {
                    let before = (category.parent, category.sort_order);
                    if category.id == id {
                        category.parent = parent;
                    }
                    if let Some(order) = ids.iter().position(|sibling| *sibling == category.id) {
                        category.sort_order = order as i64;
                    }
                    if (category.parent, category.sort_order) != before {
                        category.edited_ms = edited_after(category.edited_ms);
                    }
                }
            }
            SnippetNode::Snippet(id) => {
                if self.snippet(id).is_none() {
                    return false;
                }
                let target = match target {
                    Some(SnippetNode::Snippet(target)) => Some(target),
                    Some(SnippetNode::Category(_)) => return false,
                    None => None,
                };
                let siblings = ranked(self.snippets_of(parent).iter().map(|s| s.id));
                let Some(ids) = reordered(siblings, id, target, after) else {
                    return false;
                };
                for snippet in &mut self.snippets {
                    let before = (snippet.category, snippet.sort_order);
                    if snippet.id == id {
                        snippet.category = parent;
                    }
                    if let Some(order) = ids.iter().position(|sibling| *sibling == snippet.id) {
                        snippet.sort_order = order as i64;
                    }
                    if (snippet.category, snippet.sort_order) != before {
                        snippet.edited_ms = edited_after(snippet.edited_ms);
                    }
                }
            }
        }
        self.sync_revision += 1;
        true
    }

    /// The category a row is in: `Some(None)` at the top, `None` for a row
    /// not there.
    fn snippet_node_parent(&self, node: SnippetNode) -> Option<Option<SnippetCategoryId>> {
        match node {
            SnippetNode::Category(id) => self.snippet_category(id).map(|c| c.parent),
            SnippetNode::Snippet(id) => self.snippet(id).map(|s| s.category),
        }
    }

    /// The `sort_order` after every category in `parent` but `except`.
    fn last_category_order(
        &self,
        parent: Option<SnippetCategoryId>,
        except: Option<SnippetCategoryId>,
    ) -> i64 {
        self.snippet_categories
            .iter()
            .filter(|c| Some(c.id) != except && c.parent == parent)
            .map(|c| c.sort_order)
            .max()
            .map_or(0, |last| last + 1)
    }

    /// The `sort_order` after every snippet in `category` but `except`.
    fn last_snippet_order(
        &self,
        category: Option<SnippetCategoryId>,
        except: Option<SnippetId>,
    ) -> i64 {
        self.snippets
            .iter()
            .filter(|s| Some(s.id) != except && s.category == category)
            .map(|s| s.sort_order)
            .max()
            .map_or(0, |last| last + 1)
    }

    /// A draft that names a category, a group or a host no longer there
    /// is saved without it: at the top, for every host.
    fn normalized_snippet(&self, mut draft: SnippetDraft) -> SnippetDraft {
        if draft
            .category
            .is_some_and(|category| self.snippet_category(category).is_none())
        {
            draft.category = None;
        }
        let gone = match draft.scope {
            SnippetScope::All => false,
            SnippetScope::Group(group) => self.group(group).is_none(),
            SnippetScope::Host(host) => self.saved_host(host).is_none(),
        };
        if gone {
            draft.scope = SnippetScope::All;
        }
        draft
    }
}

/// Rows in the order they are listed, as `reordered` takes them: their
/// place there stands for `sort_order`, whose ties the names break.
fn ranked<Id>(ids: impl Iterator<Item = Id>) -> Vec<(i64, Id)> {
    ids.enumerate()
        .map(|(rank, id)| (rank as i64, id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthKind, GroupDraft, Host, HostDatabase, HostDraft, HostId};

    fn draft(name: &str, category: Option<SnippetCategoryId>) -> SnippetDraft {
        SnippetDraft::new(name, format!("echo {name}"), category)
    }

    fn top(name: &str) -> SnippetCategoryDraft {
        SnippetCategoryDraft::new(name, None)
    }

    fn names(store: &HostData) -> Vec<String> {
        store
            .snippets()
            .iter()
            .map(|snippet| snippet.name.to_string())
            .collect()
    }

    #[test]
    fn a_category_takes_its_snippets_with_it() {
        let mut store = HostData::empty();
        let docker = store.insert_snippet_category_unnotified(top("Docker"));
        let logs = store.insert_snippet_category_unnotified(top("日志"));
        store.insert_snippet_unnotified(draft("ps", Some(docker)));
        store.insert_snippet_unnotified(draft("tail", Some(logs)));
        store.insert_snippet_unnotified(draft("df", None));
        assert_eq!(store.snippets_in(docker), 1);

        assert!(store.remove_snippet_category_unnotified(docker));
        assert_eq!(names(&store), ["tail", "df"]);
        assert!(!store.remove_snippet_category_unnotified(docker));
        // A draft naming it now goes to 未分类.
        let moved = store.insert_snippet_unnotified(draft("images", Some(docker)));
        assert_eq!(store.snippet(moved).unwrap().category, None);
    }

    fn listed(store: &HostData, category: Option<SnippetCategoryId>) -> Vec<String> {
        store
            .snippets_of(category)
            .iter()
            .map(|snippet| snippet.name.to_string())
            .collect()
    }

    fn categories_in(store: &HostData, parent: Option<SnippetCategoryId>) -> Vec<String> {
        store
            .snippet_categories_in(parent)
            .iter()
            .map(|category| category.name.to_string())
            .collect()
    }

    #[test]
    fn a_category_takes_the_ones_inside_it_and_their_snippets() {
        let mut store = HostData::empty();
        let ops = store.insert_snippet_category_unnotified(top("运维"));
        let logs =
            store.insert_snippet_category_unnotified(SnippetCategoryDraft::new("日志", Some(ops)));
        let nginx = store
            .insert_snippet_category_unnotified(SnippetCategoryDraft::new("nginx", Some(logs)));
        let other = store.insert_snippet_category_unnotified(top("其他"));
        store.insert_snippet_unnotified(draft("access", Some(nginx)));
        store.insert_snippet_unnotified(draft("journal", Some(logs)));
        store.insert_snippet_unnotified(draft("df", Some(other)));
        assert_eq!(store.descendant_snippet_categories(ops).len(), 2);
        assert_eq!(store.ancestor_snippet_categories(nginx), [logs, ops]);
        assert_eq!(store.snippets_in(ops), 2);

        assert!(store.remove_snippet_category_unnotified(ops));
        assert_eq!(categories_in(&store, None), ["其他"]);
        assert_eq!(store.snippet_categories().len(), 1);
        assert_eq!(names(&store), ["df"]);
    }

    #[test]
    fn rows_list_by_name_until_dragged_and_new_ones_go_last() {
        let mut store = HostData::empty();
        // As they were before they could be dragged: all in the first place.
        for name in ["nginx", "apt", "Docker"] {
            let id = store.insert_snippet_unnotified(draft(name, None));
            store
                .snippets
                .iter_mut()
                .find(|s| s.id == id)
                .unwrap()
                .sort_order = 0;
        }
        assert_eq!(listed(&store, None), ["apt", "Docker", "nginx"]);
        store.insert_snippet_unnotified(draft("df", None));
        assert_eq!(listed(&store, None), ["apt", "Docker", "nginx", "df"]);
    }

    #[test]
    fn a_dragged_snippet_goes_beside_another_or_into_a_category() {
        let mut store = HostData::empty();
        let docker = store.insert_snippet_category_unnotified(top("Docker"));
        let ps = store.insert_snippet_unnotified(draft("ps", Some(docker)));
        let images = store.insert_snippet_unnotified(draft("images", Some(docker)));
        let df = store.insert_snippet_unnotified(draft("df", None));
        let uptime = store.insert_snippet_unnotified(draft("uptime", None));
        let edited = store.snippet(ps).unwrap().edited_ms;

        // Beside another: into its category, before it.
        assert!(store.move_snippet_node_unnotified(
            SnippetNode::Snippet(uptime),
            SnippetDrop::Before(SnippetNode::Snippet(ps))
        ));
        assert_eq!(listed(&store, Some(docker)), ["uptime", "ps", "images"]);
        assert_eq!(listed(&store, None), ["df"]);
        assert!(store.snippet(ps).unwrap().edited_ms > edited);
        // Into a category, last; to the top, last.
        assert!(
            store.move_snippet_node_unnotified(SnippetNode::Snippet(df), SnippetDrop::Into(docker))
        );
        assert_eq!(
            listed(&store, Some(docker)),
            ["uptime", "ps", "images", "df"]
        );
        assert!(store.move_snippet_node_unnotified(SnippetNode::Snippet(ps), SnippetDrop::Top));
        assert_eq!(listed(&store, None), ["ps"]);
        // Where it already is, nothing moves; beside a category is no place.
        assert!(!store.move_snippet_node_unnotified(
            SnippetNode::Snippet(images),
            SnippetDrop::After(SnippetNode::Snippet(uptime))
        ));
        assert!(!store.move_snippet_node_unnotified(
            SnippetNode::Snippet(images),
            SnippetDrop::Before(SnippetNode::Category(docker))
        ));
        // Edited, it keeps its place; moved to another category, it goes last.
        assert!(store.update_snippet_unnotified(uptime, draft("uptime -p", Some(docker))));
        assert_eq!(listed(&store, Some(docker)), ["uptime -p", "images", "df"]);
        assert!(store.update_snippet_unnotified(uptime, draft("uptime", None)));
        assert_eq!(listed(&store, None), ["ps", "uptime"]);
    }

    #[test]
    fn a_dragged_category_nests_but_never_inside_itself() {
        let mut store = HostData::empty();
        let ops = store.insert_snippet_category_unnotified(top("运维"));
        let logs = store.insert_snippet_category_unnotified(top("日志"));
        let docker = store.insert_snippet_category_unnotified(top("Docker"));
        assert_eq!(categories_in(&store, None), ["运维", "日志", "Docker"]);

        assert!(store.move_snippet_node_unnotified(
            SnippetNode::Category(docker),
            SnippetDrop::Before(SnippetNode::Category(ops))
        ));
        assert_eq!(categories_in(&store, None), ["Docker", "运维", "日志"]);
        assert!(
            store.move_snippet_node_unnotified(SnippetNode::Category(logs), SnippetDrop::Into(ops))
        );
        assert_eq!(categories_in(&store, Some(ops)), ["日志"]);
        // Not into itself, nor into what is inside it, by drag or by form.
        assert!(
            !store
                .move_snippet_node_unnotified(SnippetNode::Category(ops), SnippetDrop::Into(logs))
        );
        assert!(
            !store.move_snippet_node_unnotified(SnippetNode::Category(ops), SnippetDrop::Into(ops))
        );
        assert!(!store.update_snippet_category_unnotified(
            ops,
            SnippetCategoryDraft::new("运维", Some(logs))
        ));
        // Out again, to the top, last.
        assert!(store.move_snippet_node_unnotified(SnippetNode::Category(logs), SnippetDrop::Top));
        assert_eq!(categories_in(&store, None), ["Docker", "运维", "日志"]);
        // Moved by the form, it goes last where it lands.
        assert!(store.update_snippet_category_unnotified(
            docker,
            SnippetCategoryDraft::new("容器", Some(ops))
        ));
        assert_eq!(categories_in(&store, Some(ops)), ["容器"]);
        assert_eq!(categories_in(&store, None), ["运维", "日志"]);
    }

    #[test]
    fn a_snippet_is_edited_in_place_and_removed() {
        let mut store = HostData::empty();
        let ps = store.insert_snippet_unnotified(draft("ps", None));
        let df = store.insert_snippet_unnotified(draft("df", None));
        assert!(store.update_snippet_unnotified(ps, SnippetDraft::new("容器", "docker ps", None)));
        assert_eq!(store.snippet(ps).unwrap().command, "docker ps");
        assert_eq!(names(&store), ["容器", "df"]);
        assert!(store.remove_snippet_unnotified(df));
        assert!(!store.update_snippet_unnotified(df, draft("df", None)));
        assert_eq!(names(&store), ["容器"]);
    }

    #[test]
    fn a_snippet_kept_to_a_group_or_a_host_goes_with_it() {
        let mut store = HostData::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let web = store.insert_group_unnotified(GroupDraft::new("web", Some(production)));
        let host = store.insert_unnotified(HostDraft::new(
            "db-01",
            "10.0.0.2",
            22,
            "root",
            AuthKind::Password,
            None,
        ));
        let scoped = |name: &str, scope| SnippetDraft {
            scope,
            ..draft(name, None)
        };
        store.insert_snippet_unnotified(scoped("all", SnippetScope::All));
        store.insert_snippet_unnotified(scoped("web", SnippetScope::Group(web)));
        store.insert_snippet_unnotified(scoped("db", SnippetScope::Host(host)));

        store.remove_unnotified(host);
        assert_eq!(names(&store), ["all", "web"]);
        // A subgroup goes with its group, and its snippets with it.
        store.remove_group_unnotified(production);
        assert_eq!(names(&store), ["all"]);
        // Kept to what is not there, a snippet is for every host.
        let stray = store.insert_snippet_unnotified(scoped("stray", SnippetScope::Host(host)));
        assert_eq!(store.snippet(stray).unwrap().scope, SnippetScope::All);
    }

    #[test]
    fn load_resumes_the_snippet_id_sequences() {
        let database = HostDatabase::in_memory().unwrap();
        database
            .insert_host(&Host::new(
                HostId(1),
                HostDraft::new("web", "10.0.0.1", 22, "root", AuthKind::Password, None),
            ))
            .unwrap();
        database
            .insert_snippet_category(&SnippetCategory::new(SnippetCategoryId(3), top("Docker")))
            .unwrap();
        database
            .insert_snippet(&Snippet::new(
                SnippetId(7),
                draft("ps", Some(SnippetCategoryId(3))),
            ))
            .unwrap();

        let mut store = HostData::load(database).unwrap();
        assert_eq!(names(&store), ["ps"]);
        assert_eq!(
            store.insert_snippet_category_unnotified(top("日志")),
            SnippetCategoryId(4)
        );
        assert_eq!(
            store.insert_snippet_unnotified(draft("df", None)),
            SnippetId(8)
        );
    }
}
