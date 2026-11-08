//! The lines of the snippet list, from the store's snippets and categories.

use std::collections::HashSet;

use gpui_kit::SharedString;

use crate::host::{Snippet, SnippetCategory, SnippetCategoryId, SnippetId, listed_order};

/// A line of the snippet list. `depth` is how many categories it is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetRow {
    /// A category's heading, what is in it after it unless `folded`.
    /// `count` is the snippets in it and in the categories inside it: while
    /// searching, those that match.
    Category {
        id: SnippetCategoryId,
        depth: usize,
        count: usize,
        folded: bool,
    },
    Snippet {
        id: SnippetId,
        depth: usize,
    },
}

/// The list, as the host tree lists hosts: at each level its categories,
/// then its snippets, each in their order (`listed_order`); the snippets of
/// no category last, at the top. A search lists what matches, folded or
/// not, with the categories it is in, and leaves out the categories with
/// nothing that does.
pub fn rows(
    categories: &[SnippetCategory],
    snippets: &[Snippet],
    query: &str,
    folded: &HashSet<SnippetCategoryId>,
) -> Vec<SnippetRow> {
    let level = Level {
        categories,
        snippets,
        query,
        searching: !query.trim().is_empty(),
        folded,
    };
    let mut rows = Vec::new();
    level.list(None, 0, &mut rows);
    rows
}

/// What every level of the list is made from.
struct Level<'a> {
    categories: &'a [SnippetCategory],
    snippets: &'a [Snippet],
    query: &'a str,
    searching: bool,
    folded: &'a HashSet<SnippetCategoryId>,
}

impl Level<'_> {
    /// The lines of what is in `parent` (`None`: the top) onto `rows`, and
    /// how many snippets they hold.
    fn list(
        &self,
        parent: Option<SnippetCategoryId>,
        depth: usize,
        rows: &mut Vec<SnippetRow>,
    ) -> usize {
        // Deeper than there are categories only through a cycle, which the
        // store refuses to make: stop rather than go on for ever.
        if depth > self.categories.len() {
            return 0;
        }
        let mut count = 0;
        let mut categories: Vec<&SnippetCategory> = self
            .categories
            .iter()
            .filter(|category| category.parent == parent)
            .collect();
        categories.sort_by(|a, b| {
            listed_order(
                (a.sort_order, &a.name, a.id.0),
                (b.sort_order, &b.name, b.id.0),
            )
        });
        for category in categories {
            let folded = !self.searching && self.folded.contains(&category.id);
            // What is inside, listed to count it even when folded away.
            let mut inside = Vec::new();
            let held = self.list(Some(category.id), depth + 1, &mut inside);
            // A new category shows, empty, until something goes in it.
            if self.searching && held == 0 {
                continue;
            }
            rows.push(SnippetRow::Category {
                id: category.id,
                depth,
                count: held,
                folded,
            });
            if !folded {
                rows.extend(inside);
            }
            count += held;
        }
        let mut snippets: Vec<&Snippet> = self
            .snippets
            .iter()
            .filter(|snippet| snippet.category == parent && snippet.matches(self.query))
            .collect();
        snippets.sort_by(|a, b| {
            listed_order(
                (a.sort_order, &a.name, a.id.0),
                (b.sort_order, &b.name, b.id.0),
            )
        });
        count += snippets.len();
        rows.extend(snippets.into_iter().map(|snippet| SnippetRow::Snippet {
            id: snippet.id,
            depth,
        }));
        count
    }
}

/// Every category, depth first in the list's order, each labelled with its
/// whole path (`Docker / compose`), for the forms' pickers. An excluded one
/// is left out with everything inside it: a category cannot move into
/// itself.
pub fn category_options(
    categories: &[SnippetCategory],
    excluded: Option<SnippetCategoryId>,
) -> Vec<(SnippetCategoryId, SharedString)> {
    fn collect(
        categories: &[SnippetCategory],
        parent: Option<SnippetCategoryId>,
        prefix: &str,
        depth: usize,
        excluded: Option<SnippetCategoryId>,
        options: &mut Vec<(SnippetCategoryId, SharedString)>,
    ) {
        if depth > categories.len() {
            return;
        }
        let mut children: Vec<&SnippetCategory> = categories
            .iter()
            .filter(|category| category.parent == parent && Some(category.id) != excluded)
            .collect();
        children.sort_by(|a, b| {
            listed_order(
                (a.sort_order, &a.name, a.id.0),
                (b.sort_order, &b.name, b.id.0),
            )
        });
        for category in children {
            let path = if prefix.is_empty() {
                category.name.to_string()
            } else {
                format!("{prefix} / {}", category.name)
            };
            options.push((category.id, path.as_str().into()));
            collect(
                categories,
                Some(category.id),
                &path,
                depth + 1,
                excluded,
                options,
            );
        }
    }
    let mut options = Vec::new();
    collect(categories, None, "", 0, excluded, &mut options);
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{SnippetCategoryDraft, SnippetDraft};

    fn category(id: u64, name: &str, parent: Option<u64>) -> SnippetCategory {
        SnippetCategory::new(
            SnippetCategoryId(id),
            SnippetCategoryDraft::new(name, parent.map(SnippetCategoryId)),
        )
    }

    fn snippet(id: u64, name: &str, category: Option<u64>) -> Snippet {
        Snippet::new(
            SnippetId(id),
            SnippetDraft::new(
                name,
                format!("echo {name}"),
                category.map(SnippetCategoryId),
            ),
        )
    }

    fn heading(id: u64, depth: usize, count: usize, folded: bool) -> SnippetRow {
        SnippetRow::Category {
            id: SnippetCategoryId(id),
            depth,
            count,
            folded,
        }
    }

    fn item(id: u64, depth: usize) -> SnippetRow {
        SnippetRow::Snippet {
            id: SnippetId(id),
            depth,
        }
    }

    #[test]
    fn without_categories_the_snippets_are_listed_by_name() {
        let snippets = [snippet(1, "uptime", None), snippet(2, "df", None)];
        assert_eq!(
            rows(&[], &snippets, "", &HashSet::new()),
            [item(2, 0), item(1, 0)]
        );
    }

    #[test]
    fn categories_nest_and_the_rest_sits_at_the_top_after_them() {
        let categories = [
            category(1, "日志", None),
            category(2, "Docker", None),
            category(3, "空", None),
            category(4, "compose", Some(2)),
        ];
        let snippets = [
            snippet(1, "tail", Some(1)),
            snippet(2, "ps", Some(2)),
            snippet(3, "images", Some(2)),
            snippet(4, "df", None),
            snippet(5, "up", Some(4)),
        ];
        assert_eq!(
            rows(&categories, &snippets, "", &HashSet::new()),
            [
                heading(2, 0, 3, false),
                heading(4, 1, 1, false),
                item(5, 2),
                item(3, 1),
                item(2, 1),
                heading(1, 0, 1, false),
                item(1, 1),
                heading(3, 0, 0, false),
                item(4, 0),
            ]
        );

        // Folded, a category keeps its heading and count, and hides what
        // is inside it, however deep.
        let folded = HashSet::from([SnippetCategoryId(2)]);
        assert_eq!(
            rows(&categories, &snippets, "", &folded),
            [
                heading(2, 0, 3, true),
                heading(1, 0, 1, false),
                item(1, 1),
                heading(3, 0, 0, false),
                item(4, 0),
            ]
        );

        // A search opens what it finds, with the categories it is in, and
        // leaves out the rest.
        assert_eq!(
            rows(&categories, &snippets, "UP", &folded),
            [heading(2, 0, 1, false), heading(4, 1, 1, false), item(5, 2)]
        );
    }

    #[test]
    fn the_pickers_list_every_path_but_a_category_s_own() {
        let categories = [
            category(1, "Docker", None),
            category(2, "compose", Some(1)),
            category(3, "日志", None),
            category(4, "swarm", Some(2)),
        ];
        let paths = |excluded| -> Vec<String> {
            category_options(&categories, excluded)
                .into_iter()
                .map(|(_, path)| path.to_string())
                .collect()
        };
        assert_eq!(
            paths(None),
            [
                "Docker",
                "Docker / compose",
                "Docker / compose / swarm",
                "日志"
            ]
        );
        assert_eq!(paths(Some(SnippetCategoryId(2))), ["Docker", "日志"]);
    }

    #[test]
    fn a_drag_s_order_comes_before_the_names() {
        let mut categories = [category(1, "a", None), category(2, "b", None)];
        categories[0].sort_order = 1;
        let mut snippets = [snippet(1, "x", None), snippet(2, "y", None)];
        snippets[1].sort_order = -1;
        assert_eq!(
            rows(&categories, &snippets, "", &HashSet::new()),
            [
                heading(2, 0, 0, false),
                heading(1, 0, 0, false),
                item(2, 0),
                item(1, 0),
            ]
        );
    }
}
