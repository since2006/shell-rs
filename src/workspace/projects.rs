//! The workspace's share of 项目: the local terminals kept in groups, the
//! tabs it opens them in, the folders their shells move to, and which of
//! them the next launch opens again.
//!
//! A saved terminal is one tab at most. Closing the tab keeps it in its
//! group; a click opens it again in the folder its shell was last in.
//! Deleting it, or its group, closes its tab too. The saved terminals
//! open are saved as they change, in tab order with the one last in front,
//! so a launch after a quit, a crash or an update brings them back. Plain
//! local terminals are not among them.
//!
//! A saved terminal's last title is kept when its tab closes and when the
//! window goes (ShellRS quitting), and stands for the title until its
//! shell or a program gives another. With 保留终端内容 on, its last lines
//! of output are kept then too, and drawn again, above a line saying so,
//! when it next opens.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::component::dock::PaneRef;
use gpui_kit::*;

use crate::analytics::Counter;
use crate::app::{
    CenterTab, CloseLocalTerminal, CloseSavedTerminal, DeleteSavedTerminal, DeleteTerminalGroup,
    MoveTerminalNode, NewSavedTerminal, NewTerminalGroup, OpenSavedTerminal, RenameSavedTerminal,
    RenameTerminalGroup, ToggleTerminalGroup,
};
use crate::host::{HostStore, SavedTerminal, SavedTerminalId, SavedTerminalTab};
use crate::i18n::t;
use crate::project::{NameDialog, confirm_delete_terminal_group, open_name_dialog};
use crate::settings::AppSettings;
use crate::shared::confirm_delete;
use crate::terminal::{LocalLaunch, LocalShell, LocalTerminalId, LocalTerminalPanel, SavedName};

use super::workspace_view::Workspace;

impl Workspace {
    pub(super) fn on_new_terminal_group(
        &mut self,
        _: &NewTerminalGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        open_name_dialog(NameDialog::NewGroup, self.store.clone(), window, cx);
    }

    pub(super) fn on_rename_terminal_group(
        &mut self,
        action: &RenameTerminalGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).terminal_group(action.0).is_some() {
            open_name_dialog(
                NameDialog::RenameGroup(action.0),
                self.store.clone(),
                window,
                cx,
            );
        }
    }

    /// Delete a group with its terminals, and their open tabs, after
    /// asking.
    pub(super) fn on_delete_terminal_group(
        &mut self,
        action: &DeleteTerminalGroup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let store = self.store.read(cx);
        let Some(name) = store.terminal_group(id).map(|group| group.name.clone()) else {
            return;
        };
        let terminals: Vec<_> = store
            .terminals_in(Some(id))
            .iter()
            .map(|terminal| terminal.id)
            .collect();
        let open = terminals
            .iter()
            .filter(|terminal| self.tab_of(**terminal).is_some())
            .count();
        let workspace = cx.entity().downgrade();
        confirm_delete_terminal_group(
            &name,
            terminals.len(),
            open,
            Rc::new(move |window, cx| {
                workspace
                    .update(cx, |this, cx| {
                        this.close_saved_terminals(&terminals, window, cx);
                        this.store.update(cx, |store, cx| {
                            store.remove_terminal_group(id, cx);
                        });
                    })
                    .ok();
            }),
            window,
            cx,
        );
    }

    /// A group's 「+」, or the one on the title bar: a new terminal kept in
    /// the group or at the top, open at once. In a group it starts where
    /// the group's last terminal was, so the next one of a project is in
    /// the project's folder too; at the top, in the home folder.
    pub(super) fn on_new_saved_terminal(
        &mut self,
        action: &NewSavedTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = action.0.and_then(|group| {
            self.store
                .read(cx)
                .terminals_in(Some(group))
                .into_iter()
                .rev()
                .find_map(|terminal| terminal.directory.clone())
        });
        let Some(id) = self.store.update(cx, |store, cx| {
            store.insert_saved_terminal(action.0, directory, cx)
        }) else {
            return;
        };
        self.open_saved_terminal(id, window, cx);
        self.count(Counter::ProjectTerminal);
    }

    /// A click on a saved terminal: its tab to the front, or a tab for it
    /// in the folder it was last in.
    pub(super) fn on_open_saved_terminal(
        &mut self,
        action: &OpenSavedTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_saved_terminal(action.0, window, cx);
    }

    pub(super) fn on_close_saved_terminal(
        &mut self,
        action: &CloseSavedTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tab_of(action.0) {
            self.on_close_local_terminal(&CloseLocalTerminal(tab), window, cx);
        }
    }

    pub(super) fn on_rename_saved_terminal(
        &mut self,
        action: &RenameSavedTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.store.read(cx).saved_terminal(action.0).is_some() {
            open_name_dialog(
                NameDialog::RenameTerminal(action.0),
                self.store.clone(),
                window,
                cx,
            );
        }
    }

    /// Delete a saved terminal with its tab. Closed, it goes without asking:
    /// there is little to it. Open, only after asking, for whatever runs in
    /// it ends.
    pub(super) fn on_delete_saved_terminal(
        &mut self,
        action: &DeleteSavedTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        let Some(panel) = self
            .tab_of(id)
            .and_then(|tab| self.local_terminals.get(&tab))
        else {
            self.store.update(cx, |store, cx| {
                store.remove_saved_terminal(id, cx);
            });
            return;
        };
        let name = panel.read(cx).display_title(cx);
        let workspace = cx.entity().downgrade();
        confirm_delete(
            &name,
            Some(t!("project.delete_terminal.open")),
            Rc::new(move |window, cx| {
                workspace
                    .update(cx, |this, cx| {
                        this.close_saved_terminals(&[id], window, cx);
                        this.store.update(cx, |store, cx| {
                            store.remove_saved_terminal(id, cx);
                        });
                    })
                    .ok();
            }),
            window,
            cx,
        );
    }

    /// Close the tabs of saved terminals about to be deleted, keeping
    /// nothing of what they show.
    fn close_saved_terminals(
        &mut self,
        terminals: &[SavedTerminalId],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for terminal in terminals {
            if let Some(tab) = self.tab_of(*terminal) {
                self.saved_terminals.remove(&tab);
                self.on_close_local_terminal(&CloseLocalTerminal(tab), window, cx);
            }
        }
    }

    /// A group's heading: hide its terminals, or show them again, for the
    /// next launch too.
    pub(super) fn on_toggle_terminal_group(
        &mut self,
        action: &ToggleTerminalGroup,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = action.0;
        self.store.update(cx, |store, cx| {
            let expanded = store.terminal_group(id).is_some_and(|group| group.expanded);
            store.set_terminal_group_expanded(id, !expanded, cx);
        });
    }

    /// A row of 项目 dropped where it goes.
    pub(super) fn on_move_terminal_node(
        &mut self,
        action: &MoveTerminalNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.update(cx, |store, cx| {
            store.move_terminal_node(action.source, action.destination, cx);
        });
    }

    /// The tab a saved terminal is open in.
    fn tab_of(&self, id: SavedTerminalId) -> Option<LocalTerminalId> {
        self.saved_terminals
            .iter()
            .find(|(_, saved)| **saved == id)
            .map(|(tab, _)| *tab)
    }

    fn open_saved_terminal(
        &mut self,
        id: SavedTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tab_of(id)
            && let Some(panel) = self.local_terminals.get(&tab).cloned()
        {
            let group = panel.read(cx).tab_group();
            self.activate_tab(group, panel.entity_id(), window, cx);
            let focus = panel.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
            return;
        }
        if let Some(terminal) = self.store.read(cx).saved_terminal(id).cloned() {
            self.open_local_terminal(Some(&terminal), window, cx);
        }
    }

    /// A saved terminal's shell moved to another folder: the next opening
    /// starts there.
    pub(super) fn on_local_terminal_moved(
        &mut self,
        tab: LocalTerminalId,
        folder: &Path,
        cx: &mut Context<Self>,
    ) {
        let (Some(id), Some(folder)) = (self.saved_terminals.get(&tab).copied(), folder.to_str())
        else {
            return;
        };
        self.store.update(cx, |store, cx| {
            store.set_saved_terminal_directory(id, folder.to_owned(), cx);
        });
    }

    /// A local terminal's shell may have titled it anew: 项目 shows a saved
    /// one's title for its name, as its tab does.
    pub(super) fn on_local_terminal_retitled(
        &mut self,
        tab: LocalTerminalId,
        cx: &mut Context<Self>,
    ) {
        let (Some(id), Some(panel)) = (
            self.saved_terminals.get(&tab).copied(),
            self.local_terminals.get(&tab),
        ) else {
            return;
        };
        let title = panel.read(cx).shell_title(cx);
        self.tools
            .update(cx, |tools, cx| tools.set_terminal_title(id, title, cx));
    }

    /// The store changed: tabs of terminals deleted elsewhere are plain
    /// terminals now, and those renamed are called so.
    pub(super) fn on_saved_terminals_changed(&mut self, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let before = self.saved_terminals.len();
        self.saved_terminals
            .retain(|_, id| store.saved_terminal(*id).is_some());
        let names: Vec<_> = self
            .saved_terminals
            .iter()
            .filter_map(|(tab, id)| {
                let panel = self.local_terminals.get(tab)?.clone();
                Some((panel, saved_name(store.saved_terminal(*id)?)))
            })
            .collect();
        for (panel, name) in names {
            panel.update(cx, |panel, cx| panel.set_name(name, cx));
        }
        if self.saved_terminals.len() != before {
            self.sync_saved_terminals(cx);
        }
    }

    /// Keep what a saved terminal's tab shows, its title and output, for
    /// the next time it opens. Not on every new title: a program may change
    /// it several times a second.
    pub(super) fn keep_what_it_shows(
        &self,
        tab: LocalTerminalId,
        panel: &Entity<LocalTerminalPanel>,
        cx: &mut App,
    ) {
        let Some(id) = self.saved_terminals.get(&tab).copied() else {
            return;
        };
        if let Some(title) = panel.read(cx).shell_title(cx) {
            self.store.update(cx, |store, cx| {
                store.set_saved_terminal_title(id, title, cx);
            });
        }
        if !self
            .settings()
            .read(cx)
            .settings()
            .local_terminal
            .keep_output
        {
            return;
        }
        let output = panel.read(cx).terminal().read(cx).kept_output(cx);
        self.store.read(cx).keep_output(id, output);
    }

    /// The window is going: keep what every saved terminal open shows.
    pub(super) fn keep_on_close(&self, cx: &mut App) {
        for (tab, panel) in &self.local_terminals {
            self.keep_what_it_shows(*tab, panel, cx);
        }
    }

    /// Remember the saved terminals open now, for the next launch: in the
    /// center's tab order, the one last in front marked. Tell 项目 too.
    pub(super) fn sync_saved_terminals(&mut self, cx: &mut Context<Self>) {
        let mut order = Vec::new();
        if let Some(tree) = self
            .dock_area
            .read(cx)
            .layout(gpui_kit::component::dock::DockPlacement::Center)
        {
            tree.root().walk(&mut |node| {
                if let PaneRef::Tabs { panels, .. } = node.kind() {
                    order.extend(panels.iter().copied());
                }
            });
        }
        let tabs: Vec<SavedTerminalTab> = order
            .into_iter()
            .filter_map(|panel| match self.center_tab_for_panel(panel) {
                Some(CenterTab::LocalTerminal(tab)) => Some(SavedTerminalTab {
                    terminal: *self.saved_terminals.get(&tab)?,
                    front: self.front_saved_terminal == Some(tab),
                }),
                _ => None,
            })
            .collect();
        let open: HashSet<SavedTerminalId> = tabs.iter().map(|tab| tab.terminal).collect();
        let front = match self.active_tab {
            Some(CenterTab::LocalTerminal(tab)) => self.saved_terminals.get(&tab).copied(),
            _ => None,
        };
        self.store
            .update(cx, |store, cx| store.set_saved_terminal_tabs(tabs, cx));
        self.tools
            .update(cx, |tools, cx| tools.set_open_terminals(open, front, cx));
    }
}

/// What a saved terminal's tab calls it.
pub(super) fn saved_name(saved: &SavedTerminal) -> SavedName {
    SavedName {
        name: saved.name.clone(),
        fixed: saved.renamed,
        title: saved.title.clone(),
    }
}

/// How a local terminal starts now: the shell the settings name, in the
/// folder a saved terminal was last in, drawing `replay` first.
pub(super) fn local_launch(
    settings: &AppSettings,
    saved: Option<&SavedTerminal>,
    replay: Option<Vec<u8>>,
) -> LocalLaunch {
    let shell = LocalShell::new(settings.local_terminal.shell.as_deref().unwrap_or_default());
    LocalLaunch {
        directory: saved
            .and_then(|terminal| terminal.directory.as_deref())
            .map(PathBuf::from),
        replay,
        environment: settings.local_terminal.environment,
        ..LocalLaunch::new(shell)
    }
}

/// What a saved terminal kept when it last closed, with a dim line under it
/// saying so: what it draws first when it opens. Nothing with 保留终端内容
/// off.
pub(super) fn kept_replay(
    store: &HostStore,
    settings: &AppSettings,
    id: SavedTerminalId,
) -> Option<Vec<u8>> {
    if !settings.local_terminal.keep_output {
        return None;
    }
    let mut replay = store.kept_output(id)?;
    let line = format!(
        "\x1b[0;2m── {} ──\x1b[0m\r\n",
        t!("workspace.projects.kept_output")
    );
    replay.extend_from_slice(line.as_bytes());
    Some(replay)
}

/// The saved terminals the last launch left open, to open again in its
/// order, and the position of the one in front.
pub(super) fn terminals_to_restore(store: &HostStore) -> (Vec<SavedTerminal>, Option<usize>) {
    let mut terminals = Vec::new();
    let mut front = None;
    for tab in store.saved_terminal_tabs() {
        let Some(terminal) = store.saved_terminal(tab.terminal) else {
            continue;
        };
        if tab.front {
            front = Some(terminals.len());
        }
        terminals.push(terminal.clone());
    }
    (terminals, front)
}
