//! The command snippets' part of `HostStore`: the snippets and their
//! categories, written through like everything else.

use gpui_kit::{Context, SharedString};

use super::HostStore;
use crate::host::{
    Snippet, SnippetCategory, SnippetCategoryId, SnippetDraft, SnippetId, SnippetScope,
};

impl HostStore {
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

    /// How many snippets are in `category`: what deleting it takes along.
    pub fn snippets_in(&self, category: SnippetCategoryId) -> usize {
        self.snippets
            .iter()
            .filter(|snippet| snippet.category == Some(category))
            .count()
    }

    pub fn insert_snippet(&mut self, draft: SnippetDraft, cx: &mut Context<Self>) -> SnippetId {
        let id = self.insert_snippet_unnotified(draft);
        if let Some(snippet) = self.snippet(id) {
            self.persist("新建命令片段", cx, |db| db.insert_snippet(snippet));
        }
        cx.notify();
        id
    }

    pub fn insert_snippet_unnotified(&mut self, draft: SnippetDraft) -> SnippetId {
        let id = SnippetId(self.next_snippet_id);
        self.next_snippet_id += 1;
        let draft = self.normalized_snippet(draft);
        self.snippets.push(Snippet::new(id, draft));
        id
    }

    pub fn update_snippet(
        &mut self,
        id: SnippetId,
        draft: SnippetDraft,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.update_snippet_unnotified(id, draft) {
            return false;
        }
        if let Some(snippet) = self.snippet(id) {
            self.persist("保存命令片段", cx, |db| db.update_snippet(snippet));
        }
        cx.notify();
        true
    }

    pub fn update_snippet_unnotified(&mut self, id: SnippetId, draft: SnippetDraft) -> bool {
        let draft = self.normalized_snippet(draft);
        let Some(snippet) = self.snippets.iter_mut().find(|snippet| snippet.id == id) else {
            return false;
        };
        *snippet = Snippet::new(id, draft);
        true
    }

    pub fn remove_snippet(&mut self, id: SnippetId, cx: &mut Context<Self>) -> bool {
        if !self.remove_snippet_unnotified(id) {
            return false;
        }
        self.persist("删除命令片段", cx, |db| db.remove_snippet(id));
        cx.notify();
        true
    }

    pub fn remove_snippet_unnotified(&mut self, id: SnippetId) -> bool {
        let before = self.snippets.len();
        self.snippets.retain(|snippet| snippet.id != id);
        self.snippets.len() != before
    }

    pub fn insert_snippet_category(
        &mut self,
        name: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> SnippetCategoryId {
        let id = self.insert_snippet_category_unnotified(name);
        if let Some(category) = self.snippet_category(id) {
            self.persist("新建片段分类", cx, |db| {
                db.insert_snippet_category(category)
            });
        }
        cx.notify();
        id
    }

    pub fn insert_snippet_category_unnotified(
        &mut self,
        name: impl Into<SharedString>,
    ) -> SnippetCategoryId {
        let id = SnippetCategoryId(self.next_snippet_category_id);
        self.next_snippet_category_id += 1;
        self.snippet_categories.push(SnippetCategory {
            id,
            name: name.into(),
        });
        id
    }

    pub fn rename_snippet_category(
        &mut self,
        id: SnippetCategoryId,
        name: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.rename_snippet_category_unnotified(id, name) {
            return false;
        }
        if let Some(category) = self.snippet_category(id) {
            self.persist("重命名片段分类", cx, |db| {
                db.update_snippet_category(category)
            });
        }
        cx.notify();
        true
    }

    pub fn rename_snippet_category_unnotified(
        &mut self,
        id: SnippetCategoryId,
        name: impl Into<SharedString>,
    ) -> bool {
        let Some(category) = self
            .snippet_categories
            .iter_mut()
            .find(|category| category.id == id)
        else {
            return false;
        };
        category.name = name.into();
        true
    }

    /// Delete a category with the snippets in it.
    pub fn remove_snippet_category(
        &mut self,
        id: SnippetCategoryId,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.remove_snippet_category_unnotified(id) {
            return false;
        }
        // The database takes the snippets through `ON DELETE CASCADE`.
        self.persist("删除片段分类", cx, |db| {
            db.remove_snippet_category(id)
        });
        cx.notify();
        true
    }

    pub fn remove_snippet_category_unnotified(&mut self, id: SnippetCategoryId) -> bool {
        let before = self.snippet_categories.len();
        self.snippet_categories.retain(|category| category.id != id);
        if self.snippet_categories.len() == before {
            return false;
        }
        self.snippets.retain(|snippet| snippet.category != Some(id));
        true
    }

    /// A draft that names a category, a group or a host no longer there
    /// is saved without it: 未分类, for every host.
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
            SnippetScope::Host(host) => self.host(host).is_none(),
        };
        if gone {
            draft.scope = SnippetScope::All;
        }
        draft
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AuthKind, GroupDraft, Host, HostDatabase, HostDraft, HostId};

    fn draft(name: &str, category: Option<SnippetCategoryId>) -> SnippetDraft {
        SnippetDraft::new(name, format!("echo {name}"), category)
    }

    fn names(store: &HostStore) -> Vec<String> {
        store
            .snippets()
            .iter()
            .map(|snippet| snippet.name.to_string())
            .collect()
    }

    #[test]
    fn a_category_takes_its_snippets_with_it() {
        let mut store = HostStore::empty();
        let docker = store.insert_snippet_category_unnotified("Docker");
        let logs = store.insert_snippet_category_unnotified("日志");
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

    #[test]
    fn a_snippet_is_edited_in_place_and_removed() {
        let mut store = HostStore::empty();
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
        let mut store = HostStore::empty();
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
            .insert_snippet_category(&SnippetCategory {
                id: SnippetCategoryId(3),
                name: "Docker".into(),
            })
            .unwrap();
        database
            .insert_snippet(&Snippet::new(
                SnippetId(7),
                draft("ps", Some(SnippetCategoryId(3))),
            ))
            .unwrap();

        let mut store = HostStore::load(database).unwrap();
        assert_eq!(names(&store), ["ps"]);
        assert_eq!(
            store.insert_snippet_category_unnotified("日志"),
            SnippetCategoryId(4)
        );
        assert_eq!(
            store.insert_snippet_unnotified(draft("df", None)),
            SnippetId(8)
        );
    }
}
