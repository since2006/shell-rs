//! The saved local terminals' part of `HostData`: their groups, the
//! folders they were last in, and which of them were open, written through
//! like everything else.

use std::path::PathBuf;

use gpui_shared_string::SharedString;

use super::{Change, HostData, reordered};
use crate::{
    SavedTerminal, SavedTerminalId, SavedTerminalTab, TerminalDrop, TerminalGroup, TerminalGroupId,
    TerminalNode, next_terminal_name,
};

impl HostData {
    /// Every group of saved terminals, in the order they were made; their
    /// `sort_order` says the order they show in.
    pub fn terminal_groups(&self) -> &[TerminalGroup] {
        &self.terminal_groups
    }

    pub fn terminal_group(&self, id: TerminalGroupId) -> Option<&TerminalGroup> {
        self.terminal_groups.iter().find(|group| group.id == id)
    }

    /// Every saved terminal, in the order they were made; their
    /// `sort_order` says the order they show in within their group.
    pub fn saved_terminals(&self) -> &[SavedTerminal] {
        &self.saved_terminals
    }

    pub fn saved_terminal(&self, id: SavedTerminalId) -> Option<&SavedTerminal> {
        self.saved_terminals
            .iter()
            .find(|terminal| terminal.id == id)
    }

    /// The saved terminals of a group, or of the top, in the order they
    /// show in.
    pub fn terminals_in(&self, group: Option<TerminalGroupId>) -> Vec<&SavedTerminal> {
        let mut terminals: Vec<_> = self
            .saved_terminals
            .iter()
            .filter(|terminal| terminal.group == group)
            .collect();
        terminals.sort_by_key(|terminal| (terminal.sort_order, terminal.id));
        terminals
    }

    pub fn insert_terminal_group(&mut self, name: SharedString) -> TerminalGroupId {
        let id = self.insert_terminal_group_unnotified(name);
        if let Some(group) = self.terminal_group(id) {
            self.persist(Change::NewTerminalGroup, |db| {
                db.insert_terminal_group(group)
            });
        }
        self.changed();
        id
    }

    pub fn insert_terminal_group_unnotified(&mut self, name: SharedString) -> TerminalGroupId {
        let id = TerminalGroupId(self.next_terminal_group_id);
        self.next_terminal_group_id += 1;
        let sort_order = self
            .terminal_groups
            .iter()
            .map(|group| group.sort_order + 1)
            .max()
            .unwrap_or(0);
        self.terminal_groups.push(TerminalGroup {
            id,
            name,
            sort_order,
            expanded: true,
        });
        id
    }

    pub fn rename_terminal_group(&mut self, id: TerminalGroupId, name: SharedString) -> bool {
        let Some(group) = self.terminal_groups.iter_mut().find(|group| group.id == id) else {
            return false;
        };
        group.name = name;
        let group = group.clone();
        self.persist(Change::SaveTerminalGroup, |db| {
            db.update_terminal_group(&group)
        });
        self.changed();
        true
    }

    /// Show a group's terminals below it, or hide them. Returns whether it
    /// changed.
    pub fn set_terminal_group_expanded(&mut self, id: TerminalGroupId, expanded: bool) -> bool {
        let Some(group) = self.terminal_groups.iter_mut().find(|group| group.id == id) else {
            return false;
        };
        if group.expanded == expanded {
            return false;
        }
        group.expanded = expanded;
        let group = group.clone();
        self.persist(Change::GroupExpanded, |db| db.update_terminal_group(&group));
        self.changed();
        true
    }

    /// Delete a group with the terminals in it. Returns those terminals.
    pub fn remove_terminal_group(&mut self, id: TerminalGroupId) -> Vec<SavedTerminalId> {
        if self.terminal_group(id).is_none() {
            return Vec::new();
        }
        let removed = self.remove_terminal_group_unnotified(id);
        // The database takes the terminals and their tabs by its cascades.
        self.persist(Change::DeleteTerminalGroup, |db| {
            db.remove_terminal_group(id)
        });
        for terminal in &removed {
            self.keep_output(*terminal, None);
        }
        self.changed();
        removed
    }

    pub fn remove_terminal_group_unnotified(
        &mut self,
        id: TerminalGroupId,
    ) -> Vec<SavedTerminalId> {
        let removed: Vec<_> = self
            .terminals_in(Some(id))
            .into_iter()
            .map(|terminal| terminal.id)
            .collect();
        self.terminal_groups.retain(|group| group.id != id);
        for terminal in &removed {
            self.remove_saved_terminal_unnotified(*terminal);
        }
        removed
    }

    /// Keep a new terminal in `group`, or at the top, named after the group
    /// (`shellrs 2`, or `<ungrouped> 2` at the top: the app's `终端 2`) and
    /// starting in `directory`. `None` when the group is gone.
    pub fn insert_saved_terminal(
        &mut self,
        group: Option<TerminalGroupId>,
        directory: Option<String>,
        ungrouped: &str,
    ) -> Option<SavedTerminalId> {
        let id = self.insert_saved_terminal_unnotified(group, directory, ungrouped)?;
        if let Some(terminal) = self.saved_terminal(id) {
            self.persist(Change::NewSavedTerminal, |db| {
                db.insert_saved_terminal(terminal)
            });
        }
        self.changed();
        Some(id)
    }

    pub fn insert_saved_terminal_unnotified(
        &mut self,
        group: Option<TerminalGroupId>,
        directory: Option<String>,
        ungrouped: &str,
    ) -> Option<SavedTerminalId> {
        let prefix = match group {
            Some(group) => self.terminal_group(group)?.name.clone(),
            None => ungrouped.into(),
        };
        let taken: Vec<&str> = self
            .terminals_in(group)
            .into_iter()
            .map(|terminal| terminal.name.as_ref())
            .collect();
        let name = next_terminal_name(&prefix, &taken);
        let sort_order = self
            .terminals_in(group)
            .last()
            .map_or(0, |terminal| terminal.sort_order + 1);
        let id = SavedTerminalId(self.next_saved_terminal_id);
        self.next_saved_terminal_id += 1;
        self.saved_terminals.push(SavedTerminal {
            id,
            group,
            name: name.into(),
            renamed: false,
            title: None,
            directory,
            sort_order,
        });
        Some(id)
    }

    /// Name a terminal by hand, or with `None` give it a name of its group's
    /// again (`shellrs 3`, `<ungrouped> 3` at the top), which gives way to
    /// the titles of its tab.
    pub fn rename_saved_terminal(
        &mut self,
        id: SavedTerminalId,
        name: Option<SharedString>,
        ungrouped: &str,
    ) -> bool {
        if !self.rename_saved_terminal_unnotified(id, name, ungrouped) {
            return false;
        }
        if let Some(terminal) = self.saved_terminal(id) {
            self.persist(Change::SaveSavedTerminal, |db| {
                db.update_saved_terminal(terminal)
            });
        }
        self.changed();
        true
    }

    pub fn rename_saved_terminal_unnotified(
        &mut self,
        id: SavedTerminalId,
        name: Option<SharedString>,
        ungrouped: &str,
    ) -> bool {
        let Some(group) = self.saved_terminal(id).map(|terminal| terminal.group) else {
            return false;
        };
        let (name, renamed) = match name {
            Some(name) => (name, true),
            None => {
                let prefix = match group.and_then(|group| self.terminal_group(group)) {
                    Some(group) => group.name.clone(),
                    None => ungrouped.into(),
                };
                let taken: Vec<&str> = self
                    .terminals_in(group)
                    .into_iter()
                    .filter(|terminal| terminal.id != id)
                    .map(|terminal| terminal.name.as_ref())
                    .collect();
                (next_terminal_name(&prefix, &taken).into(), false)
            }
        };
        let Some(terminal) = self
            .saved_terminals
            .iter_mut()
            .find(|terminal| terminal.id == id)
        else {
            return false;
        };
        terminal.name = name;
        terminal.renamed = renamed;
        true
    }

    /// Remember the folder a terminal's shell is in now.
    pub fn set_saved_terminal_directory(&mut self, id: SavedTerminalId, directory: String) -> bool {
        let Some(terminal) = self
            .saved_terminals
            .iter_mut()
            .find(|terminal| terminal.id == id)
        else {
            return false;
        };
        if terminal.directory.as_ref() == Some(&directory) {
            return false;
        }
        terminal.directory = Some(directory);
        let terminal = terminal.clone();
        self.persist(Change::SaveSavedTerminal, |db| {
            db.update_saved_terminal(&terminal)
        });
        self.changed();
        true
    }

    /// Remember the title its shell or a program gave a terminal's tab
    /// last.
    pub fn set_saved_terminal_title(&mut self, id: SavedTerminalId, title: SharedString) -> bool {
        let Some(terminal) = self
            .saved_terminals
            .iter_mut()
            .find(|terminal| terminal.id == id)
        else {
            return false;
        };
        if terminal.title.as_ref() == Some(&title) {
            return false;
        }
        terminal.title = Some(title);
        let terminal = terminal.clone();
        self.persist(Change::SaveSavedTerminal, |db| {
            db.update_saved_terminal(&terminal)
        });
        self.changed();
        true
    }

    pub fn remove_saved_terminal(&mut self, id: SavedTerminalId) -> bool {
        if !self.remove_saved_terminal_unnotified(id) {
            return false;
        }
        // The database takes its tab through `ON DELETE CASCADE`.
        self.persist(Change::DeleteSavedTerminal, |db| {
            db.remove_saved_terminal(id)
        });
        self.keep_output(id, None);
        self.changed();
        true
    }

    pub fn remove_saved_terminal_unnotified(&mut self, id: SavedTerminalId) -> bool {
        let before = self.saved_terminals.len();
        self.saved_terminals.retain(|terminal| terminal.id != id);
        if self.saved_terminals.len() == before {
            return false;
        }
        self.saved_terminal_tabs.retain(|tab| tab.terminal != id);
        true
    }

    /// Move a dragged row: a group among the groups, a terminal among the
    /// terminals of a group, its own or another, or of the top.
    pub fn move_terminal_node(&mut self, source: TerminalNode, drop: TerminalDrop) -> bool {
        if !self.move_terminal_node_unnotified(source, drop) {
            return false;
        }
        self.persist(Change::MoveTerminalNode, |db| {
            db.save_terminal_order(&self.terminal_groups, &self.saved_terminals)
        });
        self.changed();
        true
    }

    pub fn move_terminal_node_unnotified(
        &mut self,
        source: TerminalNode,
        drop: TerminalDrop,
    ) -> bool {
        let (target, after) = match drop {
            TerminalDrop::Before(target) => (Some(target), false),
            TerminalDrop::After(target) => (Some(target), true),
            TerminalDrop::Into(_) | TerminalDrop::Top => (None, false),
        };
        match (source, target) {
            (TerminalNode::Group(id), Some(TerminalNode::Group(target))) => {
                if self.terminal_group(id).is_none() || self.terminal_group(target).is_none() {
                    return false;
                }
                let siblings = self
                    .terminal_groups
                    .iter()
                    .map(|group| (group.sort_order, group.id))
                    .collect();
                let Some(ids) = reordered(siblings, id, Some(target), after) else {
                    return false;
                };
                for group in &mut self.terminal_groups {
                    if let Some(order) = ids.iter().position(|sibling| *sibling == group.id) {
                        group.sort_order = order as i64;
                    }
                }
                true
            }
            (TerminalNode::Terminal(id), target) => {
                let (group, target) = match (target, drop) {
                    (Some(TerminalNode::Terminal(target)), _) => {
                        match self.saved_terminal(target) {
                            Some(terminal) => (terminal.group, Some(target)),
                            None => return false,
                        }
                    }
                    (None, TerminalDrop::Into(group)) => (Some(group), None),
                    (None, TerminalDrop::Top) => (None, None),
                    _ => return false,
                };
                if self.saved_terminal(id).is_none()
                    || group.is_some_and(|group| self.terminal_group(group).is_none())
                {
                    return false;
                }
                let before: Vec<_> = self
                    .terminals_in(group)
                    .into_iter()
                    .map(|terminal| terminal.id)
                    .collect();
                let mut ids: Vec<_> = before.iter().copied().filter(|t| *t != id).collect();
                let index = match target {
                    Some(target) => match ids.iter().position(|sibling| *sibling == target) {
                        Some(index) => index + usize::from(after),
                        None => return false,
                    },
                    None => ids.len(),
                };
                ids.insert(index, id);
                if ids == before {
                    return false;
                }
                for terminal in &mut self.saved_terminals {
                    if terminal.id == id {
                        terminal.group = group;
                    }
                    if let Some(order) = ids.iter().position(|sibling| *sibling == terminal.id) {
                        terminal.sort_order = order as i64;
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The output a saved terminal showed when it last closed, to draw
    /// again when it opens.
    pub fn kept_output(&self, id: SavedTerminalId) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.kept_output_path(id)?).ok()?;
        (!bytes.is_empty()).then_some(bytes)
    }

    /// Keep what a saved terminal shows now, or nothing. Without a folder
    /// for it nothing is kept; a file that cannot be written is only
    /// missed next time.
    pub fn keep_output(&self, id: SavedTerminalId, output: Option<Vec<u8>>) {
        let Some(path) = self.kept_output_path(id) else {
            return;
        };
        match output.filter(|output| !output.is_empty()) {
            Some(output) => {
                let written = path
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|_| std::fs::write(&path, output));
                if let Err(error) = written {
                    eprintln!("shellrs: cannot keep a terminal's output: {error}");
                }
            }
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Delete every terminal's kept output: 保留终端内容 was turned off.
    pub fn forget_kept_outputs(&self) {
        if let Some(dir) = &self.terminal_output_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Delete the kept output of terminals no longer saved: left behind
    /// when ShellRS stopped between deleting one and its file.
    pub(super) fn forget_kept_outputs_but_saved(&self) {
        let Some(entries) = self
            .terminal_output_dir
            .as_ref()
            .and_then(|dir| std::fs::read_dir(dir).ok())
        else {
            return;
        };
        let kept: Vec<PathBuf> = self
            .saved_terminals
            .iter()
            .filter_map(|terminal| self.kept_output_path(terminal.id))
            .collect();
        for entry in entries.flatten() {
            if !kept.contains(&entry.path()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn kept_output_path(&self, id: SavedTerminalId) -> Option<PathBuf> {
        let dir = self.terminal_output_dir.as_ref()?;
        Some(dir.join(format!("{}.ansi", id.0)))
    }

    /// The saved terminals open when ShellRS last had them, in tab order:
    /// what the next launch opens again.
    pub fn saved_terminal_tabs(&self) -> &[SavedTerminalTab] {
        &self.saved_terminal_tabs
    }

    /// Remember the saved terminals open now. Unlike the other changes this
    /// does not notify: nothing shows the list, and the workspace says it
    /// at every tab switch, which would rebuild every observer's view.
    pub fn set_saved_terminal_tabs(&mut self, tabs: Vec<SavedTerminalTab>) {
        if self.set_saved_terminal_tabs_unnotified(tabs) {
            let tabs = &self.saved_terminal_tabs;
            self.persist(Change::SavedTerminalTabs, |db| {
                db.replace_saved_terminal_tabs(tabs)
            });
        }
    }

    /// Returns whether the list changed. Terminals no longer there are left
    /// out, each terminal is listed once, and only the first tab marked as
    /// in front stays so.
    pub fn set_saved_terminal_tabs_unnotified(&mut self, mut tabs: Vec<SavedTerminalTab>) -> bool {
        let mut seen = Vec::new();
        tabs.retain(|tab| {
            let keep = self.saved_terminal(tab.terminal).is_some() && !seen.contains(&tab.terminal);
            seen.push(tab.terminal);
            keep
        });
        let mut front = false;
        for tab in &mut tabs {
            tab.front &= !front;
            front |= tab.front;
        }
        if tabs == self.saved_terminal_tabs {
            return false;
        }
        self.saved_terminal_tabs = tabs;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostDatabase;

    fn tab(terminal: SavedTerminalId, front: bool) -> SavedTerminalTab {
        SavedTerminalTab { terminal, front }
    }

    fn names(store: &HostData) -> Vec<String> {
        store
            .saved_terminals()
            .iter()
            .map(|terminal| terminal.name.to_string())
            .collect()
    }

    #[test]
    fn new_terminals_are_named_after_their_group() {
        let mut store = HostData::empty();
        let shellrs = store.insert_terminal_group_unnotified("shellrs".into());
        let blog = store.insert_terminal_group_unnotified("blog".into());
        let a = store
            .insert_saved_terminal_unnotified(Some(shellrs), None, "Terminal")
            .unwrap();
        store.insert_saved_terminal_unnotified(
            Some(shellrs),
            Some("/code/shellrs".into()),
            "Terminal",
        );
        store.insert_saved_terminal_unnotified(Some(blog), None, "Terminal");
        assert_eq!(names(&store), ["shellrs 1", "shellrs 2", "blog 1"]);
        // A number given back is taken again.
        store.remove_saved_terminal_unnotified(a);
        store.insert_saved_terminal_unnotified(Some(shellrs), None, "Terminal");
        assert_eq!(names(&store), ["shellrs 2", "blog 1", "shellrs 1"]);
        // A group no longer there gets none.
        assert_eq!(
            store.insert_saved_terminal_unnotified(Some(TerminalGroupId(9)), None, "Terminal"),
            None
        );
    }

    #[test]
    fn a_name_given_by_hand_is_kept_and_none_numbers_it_again() {
        let mut store = HostData::empty();
        let shellrs = store.insert_terminal_group_unnotified("shellrs".into());
        let a = store
            .insert_saved_terminal_unnotified(Some(shellrs), None, "Terminal")
            .unwrap();
        let b = store
            .insert_saved_terminal_unnotified(Some(shellrs), None, "Terminal")
            .unwrap();
        assert!(!store.saved_terminal(a).unwrap().renamed);
        assert!(store.rename_saved_terminal_unnotified(a, Some("claude".into()), "Terminal"));
        let renamed = store.saved_terminal(a).unwrap();
        assert_eq!((renamed.name.as_ref(), renamed.renamed), ("claude", true));
        // Numbered again past the others, its own old number free.
        assert!(store.rename_saved_terminal_unnotified(a, None, "Terminal"));
        let again = store.saved_terminal(a).unwrap();
        assert_eq!((again.name.as_ref(), again.renamed), ("shellrs 1", false));
        assert!(store.rename_saved_terminal_unnotified(b, None, "Terminal"));
        assert_eq!(store.saved_terminal(b).unwrap().name, "shellrs 2");
        assert!(!store.rename_saved_terminal_unnotified(SavedTerminalId(9), None, "Terminal"));
    }

    fn order(store: &HostData, group: TerminalGroupId) -> Vec<SavedTerminalId> {
        store
            .terminals_in(Some(group))
            .into_iter()
            .map(|terminal| terminal.id)
            .collect()
    }

    #[test]
    fn rows_move_among_their_kind() {
        let mut store = HostData::empty();
        let work = store.insert_terminal_group_unnotified("work".into());
        let blog = store.insert_terminal_group_unnotified("blog".into());
        let site = store.insert_terminal_group_unnotified("site".into());
        let a = store
            .insert_saved_terminal_unnotified(Some(work), None, "Terminal")
            .unwrap();
        let b = store
            .insert_saved_terminal_unnotified(Some(work), None, "Terminal")
            .unwrap();
        let c = store
            .insert_saved_terminal_unnotified(Some(work), None, "Terminal")
            .unwrap();
        let d = store
            .insert_saved_terminal_unnotified(Some(blog), None, "Terminal")
            .unwrap();

        // A group before another.
        assert!(store.move_terminal_node_unnotified(
            TerminalNode::Group(site),
            TerminalDrop::Before(TerminalNode::Group(work)),
        ));
        let mut groups: Vec<_> = store
            .terminal_groups()
            .iter()
            .map(|group| (group.sort_order, group.id))
            .collect();
        groups.sort();
        let groups: Vec<_> = groups.into_iter().map(|(_, id)| id).collect();
        assert_eq!(groups, [site, work, blog]);

        // A terminal after another of its group, then into another group,
        // then beside a terminal of a third.
        assert!(store.move_terminal_node_unnotified(
            TerminalNode::Terminal(a),
            TerminalDrop::After(TerminalNode::Terminal(c)),
        ));
        assert_eq!(order(&store, work), [b, c, a]);
        assert!(store.move_terminal_node_unnotified(
            TerminalNode::Terminal(b),
            TerminalDrop::Into(site),
        ));
        assert_eq!(order(&store, site), [b]);
        assert_eq!(order(&store, work), [c, a]);
        assert!(store.move_terminal_node_unnotified(
            TerminalNode::Terminal(c),
            TerminalDrop::Before(TerminalNode::Terminal(d)),
        ));
        assert_eq!(order(&store, blog), [c, d]);
        assert_eq!(store.saved_terminal(c).unwrap().group, Some(blog));

        // Nothing that would not change anything, or nest a group.
        assert!(!store.move_terminal_node_unnotified(
            TerminalNode::Terminal(d),
            TerminalDrop::After(TerminalNode::Terminal(c)),
        ));
        assert!(!store.move_terminal_node_unnotified(
            TerminalNode::Group(work),
            TerminalDrop::Into(blog),
        ));
        assert!(!store.move_terminal_node_unnotified(
            TerminalNode::Group(work),
            TerminalDrop::Before(TerminalNode::Terminal(d)),
        ));
        // A new terminal goes last in its group.
        let e = store
            .insert_saved_terminal_unnotified(Some(blog), None, "Terminal")
            .unwrap();
        assert_eq!(order(&store, blog), [c, d, e]);
    }

    #[test]
    fn a_group_takes_its_terminals_and_their_tabs_with_it() {
        let mut store = HostData::empty();
        let shellrs = store.insert_terminal_group_unnotified("shellrs".into());
        let blog = store.insert_terminal_group_unnotified("blog".into());
        let a = store
            .insert_saved_terminal_unnotified(Some(shellrs), None, "Terminal")
            .unwrap();
        let b = store
            .insert_saved_terminal_unnotified(Some(blog), None, "Terminal")
            .unwrap();
        store.set_saved_terminal_tabs_unnotified(vec![tab(a, true), tab(b, false)]);

        assert_eq!(store.remove_terminal_group_unnotified(shellrs), [a]);
        assert_eq!(names(&store), ["blog 1"]);
        assert_eq!(store.saved_terminal_tabs(), [tab(b, false)]);
    }

    #[test]
    fn saved_tabs_keep_known_terminals_once_and_one_in_front() {
        let mut store = HostData::empty();
        let group = store.insert_terminal_group_unnotified("shellrs".into());
        let a = store
            .insert_saved_terminal_unnotified(Some(group), None, "Terminal")
            .unwrap();
        let b = store
            .insert_saved_terminal_unnotified(Some(group), None, "Terminal")
            .unwrap();
        assert!(store.set_saved_terminal_tabs_unnotified(vec![
            tab(a, true),
            tab(SavedTerminalId(99), false),
            tab(b, true),
            tab(a, false),
        ]));
        assert_eq!(store.saved_terminal_tabs(), [tab(a, true), tab(b, false)]);
        // The same list again is no change.
        assert!(!store.set_saved_terminal_tabs_unnotified(vec![tab(a, true), tab(b, false)]));
        assert!(store.remove_saved_terminal_unnotified(a));
        assert_eq!(store.saved_terminal_tabs(), [tab(b, false)]);
    }

    #[test]
    fn kept_output_is_a_file_each_and_goes_with_its_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let outputs = dir.path().join("terminal-output");
        let mut store = HostData::empty();
        let group = store.insert_terminal_group_unnotified("work".into());
        let a = store
            .insert_saved_terminal_unnotified(Some(group), None, "Terminal")
            .unwrap();
        let b = store
            .insert_saved_terminal_unnotified(Some(group), None, "Terminal")
            .unwrap();
        // Nowhere to keep it: nothing kept.
        store.keep_output(a, Some(b"ls".to_vec()));
        assert_eq!(store.kept_output(a), None);

        std::fs::create_dir_all(&outputs).unwrap();
        std::fs::write(outputs.join("99.ansi"), b"gone").unwrap();
        let store = store.with_terminal_output_dir(&outputs);
        // What a terminal no longer saved left is gone.
        assert!(!outputs.join("99.ansi").exists());
        store.keep_output(a, Some(b"$ ls".to_vec()));
        store.keep_output(b, Some(b"$ pwd".to_vec()));
        assert_eq!(store.kept_output(a).as_deref(), Some(&b"$ ls"[..]));
        // Nothing to keep clears it.
        store.keep_output(b, Some(Vec::new()));
        assert_eq!(store.kept_output(b), None);
        assert!(!outputs.join(format!("{}.ansi", b.0)).exists());

        let mut store = store;
        store.remove_saved_terminal_unnotified(a);
        // The unnotified remove is the memory's alone; the files go with
        // the notified ones, and all of them when the setting is turned
        // off.
        store.forget_kept_outputs();
        assert!(!outputs.exists());
    }

    #[test]
    fn load_reads_them_back_and_resumes_the_id_sequences() {
        let database = HostDatabase::in_memory().unwrap();
        let group = TerminalGroup {
            id: TerminalGroupId(4),
            name: "shellrs".into(),
            sort_order: 2,
            expanded: false,
        };
        database.insert_terminal_group(&group).unwrap();
        let terminal = SavedTerminal {
            id: SavedTerminalId(9),
            group: Some(group.id),
            name: "claude".into(),
            renamed: true,
            title: Some("✳ Claude Code".into()),
            directory: Some("/code/shellrs".into()),
            sort_order: 5,
        };
        database.insert_saved_terminal(&terminal).unwrap();
        database
            .replace_saved_terminal_tabs(&[tab(terminal.id, true)])
            .unwrap();

        let mut store = HostData::load(database).unwrap();
        assert_eq!(store.terminal_groups(), [group]);
        assert_eq!(store.saved_terminals(), [terminal]);
        assert_eq!(store.saved_terminal_tabs(), [tab(SavedTerminalId(9), true)]);
        assert_eq!(
            store.insert_terminal_group_unnotified("blog".into()),
            TerminalGroupId(5)
        );
        assert!(store.terminal_group(TerminalGroupId(5)).unwrap().expanded);
        assert_eq!(
            store.insert_saved_terminal_unnotified(Some(TerminalGroupId(4)), None, "Terminal"),
            Some(SavedTerminalId(10))
        );
    }
}
