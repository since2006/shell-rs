//! Command snippets: the commands 命令片段 keeps, in categories that nest
//! as host groups do. They live with the hosts because they are stored in
//! the same database, and a snippet may one day be kept to a group or a
//! host and go with it; the `snippets` module that shows them depends on
//! this one.

use std::cmp::Ordering;

use gpui_shared_string::SharedString;

use super::{GroupId, HostId, PublicId};

/// Stable identity of a snippet. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnippetId(pub u64);

/// Stable identity of a snippet category. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnippetCategoryId(pub u64);

/// Where a snippet is offered. Every snippet is for every host for now;
/// the database has room for one kept to a group (with its subgroups) or
/// to a host, which goes when they do, and nothing sets that yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SnippetScope {
    #[default]
    All,
    Group(GroupId),
    Host(HostId),
}

/// Why a snippet draft cannot be saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetDraftError {
    Name,
    Command,
}

/// What the snippet form edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetDraft {
    pub name: SharedString,
    /// What it types into the terminal, one line or several.
    pub command: String,
    /// `None` is 未分类.
    pub category: Option<SnippetCategoryId>,
    pub scope: SnippetScope,
    /// Whether a click on it runs it, rather than only putting it on the
    /// terminal's line.
    pub run_on_click: bool,
}

impl SnippetDraft {
    pub fn new(
        name: impl Into<SharedString>,
        command: impl Into<String>,
        category: Option<SnippetCategoryId>,
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            category,
            scope: SnippetScope::All,
            run_on_click: false,
        }
    }

    pub fn with_run_on_click(mut self, run_on_click: bool) -> Self {
        self.run_on_click = run_on_click;
        self
    }

    /// The draft as the form's fields give it, tidied, or why it cannot be
    /// saved. Blank lines and spaces around the command go; inside it,
    /// every line stays as written.
    pub fn validated(
        name: &str,
        command: &str,
        category: Option<SnippetCategoryId>,
    ) -> Result<Self, SnippetDraftError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(SnippetDraftError::Name);
        }
        let command = command.trim_end().trim_start_matches(['\n', '\r']);
        if command.trim().is_empty() {
            return Err(SnippetDraftError::Command);
        }
        Ok(Self::new(name.to_owned(), command.to_owned(), category))
    }
}

/// A saved command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub id: SnippetId,
    pub name: SharedString,
    pub command: String,
    pub category: Option<SnippetCategoryId>,
    pub scope: SnippetScope,
    pub run_on_click: bool,
    /// Its place among its category's snippets; see [`listed_order`].
    pub sort_order: i64,
    /// The snippet's id on every device it syncs to.
    pub sync_id: PublicId,
    /// When the user last changed it; see [`super::Host::edited_ms`].
    pub edited_ms: u64,
}

impl Snippet {
    pub fn new(id: SnippetId, draft: SnippetDraft) -> Self {
        Self {
            id,
            name: draft.name,
            command: draft.command,
            category: draft.category,
            scope: draft.scope,
            run_on_click: draft.run_on_click,
            sort_order: 0,
            sync_id: PublicId::generate(),
            edited_ms: 0,
        }
    }

    pub fn draft(&self) -> SnippetDraft {
        SnippetDraft {
            name: self.name.clone(),
            command: self.command.clone(),
            category: self.category,
            scope: self.scope,
            run_on_click: self.run_on_click,
        }
    }

    /// Whether the snippet holds every word of `query` in its name or its
    /// command, regardless of case.
    pub fn matches(&self, query: &str) -> bool {
        let name = self.name.to_lowercase();
        let command = self.command.to_lowercase();
        query.split_whitespace().all(|word| {
            let word = word.to_lowercase();
            name.contains(&word) || command.contains(&word)
        })
    }
}

/// What the category form commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetCategoryDraft {
    pub name: SharedString,
    /// `None` is the top level.
    pub parent: Option<SnippetCategoryId>,
}

impl SnippetCategoryDraft {
    pub fn new(name: impl Into<SharedString>, parent: Option<SnippetCategoryId>) -> Self {
        Self {
            name: name.into(),
            parent,
        }
    }
}

/// A folder of snippets, at the top or in another, as host groups nest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetCategory {
    pub id: SnippetCategoryId,
    pub name: SharedString,
    /// The category it is in; `None` at the top.
    pub parent: Option<SnippetCategoryId>,
    /// Its place among the categories beside it; see [`listed_order`].
    pub sort_order: i64,
    /// The category's id on every device it syncs to.
    pub sync_id: PublicId,
    /// When the user last changed it; see [`super::Host::edited_ms`].
    pub edited_ms: u64,
}

impl SnippetCategory {
    pub fn new(id: SnippetCategoryId, draft: SnippetCategoryDraft) -> Self {
        Self {
            id,
            name: draft.name,
            parent: draft.parent,
            sort_order: 0,
            sync_id: PublicId::generate(),
            edited_ms: 0,
        }
    }

    pub fn draft(&self) -> SnippetCategoryDraft {
        SnippetCategoryDraft::new(self.name.clone(), self.parent)
    }
}

/// A row of 命令片段's list: a category or a snippet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnippetNode {
    Category(SnippetCategoryId),
    Snippet(SnippetId),
}

/// Where a dragged row of 命令片段 lands, as in the host tree: beside a
/// row of its own kind, into a category (last in it), or at the top level
/// (last there).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetDrop {
    Before(SnippetNode),
    After(SnippetNode),
    Into(SnippetCategoryId),
    Top,
}

/// The order snippets and categories are listed in: by name, regardless of
/// case, then by when they were made.
pub fn by_name(a: (&str, u64), b: (&str, u64)) -> Ordering {
    a.0.to_lowercase()
        .cmp(&b.0.to_lowercase())
        .then(a.0.cmp(b.0))
        .then(a.1.cmp(&b.1))
}

/// The order of the categories beside each other, and of a category's
/// snippets: by `sort_order`, which a drag sets, then [`by_name`]. Those
/// never dragged are all 0, as everything made before they could be, so
/// they keep listing by name.
pub fn listed_order(a: (i64, &str, u64), b: (i64, &str, u64)) -> Ordering {
    a.0.cmp(&b.0).then_with(|| by_name((a.1, a.2), (b.1, b.2)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snippet_needs_a_name_and_a_command() {
        assert_eq!(
            SnippetDraft::validated("  ", "ls", None),
            Err(SnippetDraftError::Name)
        );
        assert_eq!(
            SnippetDraft::validated("列表", " \n \n", None),
            Err(SnippetDraftError::Command)
        );
        let draft = SnippetDraft::validated(
            " 清理日志 ",
            "\n\n  find /var/log -mtime +7\n  -delete \n\n",
            None,
        )
        .unwrap();
        assert_eq!(draft.name, "清理日志");
        assert_eq!(draft.command, "  find /var/log -mtime +7\n  -delete");
        assert_eq!(draft.scope, SnippetScope::All);
    }

    #[test]
    fn the_search_looks_in_the_name_and_the_command() {
        let snippet = Snippet::new(
            SnippetId(1),
            SnippetDraft::new("查看容器", "docker ps -a\ndocker images", None),
        );
        assert!(snippet.matches(""));
        assert!(snippet.matches("容器"));
        assert!(snippet.matches("DOCKER images"));
        assert!(snippet.matches("容器 ps"));
        assert!(!snippet.matches("kubectl"));
    }

    #[test]
    fn a_drag_s_order_goes_first_then_the_name() {
        let mut rows = vec![(0, "nginx", 1), (1, "apt", 2), (0, "Docker", 3)];
        rows.sort_by(|a, b| listed_order(*a, *b));
        assert_eq!(rows, [(0, "Docker", 3), (0, "nginx", 1), (1, "apt", 2)]);
    }

    #[test]
    fn names_sort_regardless_of_case() {
        let mut names = vec![("nginx", 1), ("Docker", 2), ("apt", 3), ("docker", 4)];
        names.sort_by(|a, b| by_name(*a, *b));
        assert_eq!(
            names,
            [("apt", 3), ("Docker", 2), ("docker", 4), ("nginx", 1)]
        );
    }
}
