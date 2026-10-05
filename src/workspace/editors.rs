//! The workspace's part in editing files: opening an editor tab for a file
//! an SFTP tab read, showing the one already open instead of a second,
//! closing them, and what has to be asked first when changes would be lost.

use std::rc::Rc;

use gpui_kit::component::dock::{DockPlacement, panel_handle};
use gpui_kit::*;

use super::Workspace;
use crate::app::{CenterTab, CloseEditor, EditorAction, EditorCommand, EditorShortcut};
use crate::editor::{EditorId, EditorKey, EditorPanel, EditorPanelEvent, EditorSource};
use crate::explorer::{ExplorerId, FileLocation};
use crate::sftp::TextFile;
use crate::shared::confirm_danger;

impl Workspace {
    /// The editor of one tab, for tests and the status line.
    pub fn editor(&self, id: EditorId) -> Option<&Entity<EditorPanel>> {
        self.editors.get(&id)
    }

    /// The editor already open for a file of an SFTP tab's host.
    pub(super) fn editor_for_location(
        &self,
        explorer: ExplorerId,
        location: &FileLocation,
        cx: &App,
    ) -> Option<Entity<EditorPanel>> {
        let key = match location {
            FileLocation::Remote(path) => EditorKey::Remote(
                self.explorers.get(&explorer)?.read(cx).host_id(),
                path.clone(),
            ),
            FileLocation::Local(path) => EditorKey::Local(path.clone()),
        };
        self.editors
            .values()
            .find(|editor| editor.read(cx).key() == key)
            .cloned()
    }

    /// Bring an editor's tab to the front, with focus in its text.
    pub(super) fn show_editor(
        &self,
        editor: &Entity<EditorPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group = editor.read(cx).tab_group();
        self.activate_tab(group, editor.entity_id(), window, cx);
        let focus = editor.read(cx).state().read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    /// A new editor tab for a file an SFTP tab has read, or the tab already
    /// open for it (it was opened again while being read).
    pub(super) fn open_editor(
        &mut self,
        explorer: ExplorerId,
        location: FileLocation,
        file: Rc<TextFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.editor_for_location(explorer, &location, cx) {
            self.show_editor(&editor, window, cx);
            return;
        }
        let Some(panel) = self.explorers.get(&explorer) else {
            return;
        };
        let source = match &location {
            FileLocation::Remote(_) => EditorSource::Remote {
                explorer: panel.downgrade(),
                explorer_id: explorer,
                host: panel.read(cx).host_id(),
            },
            FileLocation::Local(_) => EditorSource::Local {
                provider: self.local_directory_provider.clone(),
            },
        };
        let file = Rc::try_unwrap(file).unwrap_or_else(|file| (*file).clone());
        let id = EditorId(self.next_editor_id);
        self.next_editor_id += 1;
        let (store, dispatch) = (self.store.clone(), self.focus_handle.clone());
        let editor =
            cx.new(|cx| EditorPanel::new(id, source, location, file, store, dispatch, window, cx));
        let subscription = cx.subscribe_in(&editor, window, Self::on_editor_event);
        self._subscriptions.push(subscription);
        self.editors.insert(id, editor.clone());
        self.dock_area.update(cx, |area, cx| {
            area.add_panel_view(
                panel_handle(editor),
                DockPlacement::Center,
                None,
                window,
                cx,
            );
        });
    }

    fn on_editor_event(
        &mut self,
        editor: &Entity<EditorPanel>,
        event: &EditorPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            EditorPanelEvent::Activated(id) => {
                let host = editor.read(cx).host_id();
                self.store
                    .update(cx, |store, cx| store.set_active(host, cx));
                self.set_active_tab(Some(CenterTab::Editor(*id)), window, cx);
                cx.notify();
            }
            EditorPanelEvent::Closed(id) => {
                self.editors.remove(id);
                if self.active_tab == Some(CenterTab::Editor(*id)) {
                    self.set_active_tab(None, window, cx);
                }
                cx.notify();
            }
            EditorPanelEvent::Changed(id) => {
                if self.active_tab == Some(CenterTab::Editor(*id)) {
                    cx.notify();
                }
            }
            // The panes that show the file's folder show its new size and
            // time: for a remote file the SFTP tabs of its host, for a
            // local one every SFTP tab.
            EditorPanelEvent::Saved(_) => {
                let (location, host) = {
                    let editor = editor.read(cx);
                    (editor.location().clone(), editor.host_id())
                };
                let explorers: Vec<_> = self
                    .explorers
                    .values()
                    .filter(|panel| host.is_none_or(|host| panel.read(cx).host_id() == host))
                    .cloned()
                    .collect();
                for panel in explorers {
                    panel.update(cx, |panel, cx| {
                        panel.reload_if_showing(&location, window, cx)
                    });
                }
            }
            EditorPanelEvent::CloseRequested(id) => self.remove_editor(*id, window, cx),
        }
    }

    pub(super) fn on_close_editor(
        &mut self,
        action: &CloseEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editors.get(&action.0).cloned() else {
            return;
        };
        if editor.read(cx).is_dirty() {
            self.show_editor(&editor, window, cx);
            editor.update(cx, |editor, cx| editor.confirm_close(window, cx));
        } else {
            self.remove_editor(action.0, window, cx);
        }
    }

    pub(super) fn on_editor_action(
        &mut self,
        action: &EditorAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action.command() == EditorCommand::CloseConfirmed {
            self.remove_editor(action.editor(), window, cx);
        } else if let Some(editor) = self.editors.get(&action.editor()).cloned() {
            editor.update(cx, |editor, cx| {
                editor.execute(action.command(), window, cx)
            });
        }
    }

    pub(super) fn on_editor_shortcut(
        &mut self,
        action: &EditorShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = self
            .editors
            .values()
            .find(|editor| editor.read(cx).contains_focus(window, cx))
            .cloned();
        if let Some(editor) = focused {
            editor.update(cx, |editor, cx| editor.execute(action.0, window, cx));
        }
    }

    /// Close an editor tab without asking.
    pub(super) fn remove_editor(
        &mut self,
        id: EditorId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.editors.get(&id).cloned() {
            self.dock_area
                .update(cx, |area, cx| area.remove_panel(editor, window, cx));
        }
    }

    /// The editors of remote files read and written through an SFTP tab.
    pub(super) fn editors_of(&self, explorer: ExplorerId, cx: &App) -> Vec<EditorId> {
        self.editors
            .iter()
            .filter(|(_, editor)| editor.read(cx).explorer_id() == Some(explorer))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The names of the files with changes not saved yet, among `editors`.
    pub(super) fn unsaved_files(&self, editors: &[EditorId], cx: &App) -> Vec<String> {
        let mut names: Vec<String> = editors
            .iter()
            .filter_map(|id| self.editors.get(id))
            .filter(|editor| editor.read(cx).is_dirty())
            .map(|editor| editor.read(cx).name())
            .collect();
        names.sort();
        names
    }

    /// Every editor with changes not saved yet.
    pub(super) fn all_unsaved_files(&self, cx: &App) -> Vec<String> {
        let ids: Vec<EditorId> = self.editors.keys().copied().collect();
        self.unsaved_files(&ids, cx)
    }

    /// The quit guard: with changes not saved, ask before quitting and hold
    /// the quit back (true) until the user agrees.
    pub(super) fn ask_before_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let unsaved = self.all_unsaved_files(cx);
        if unsaved.is_empty() {
            return false;
        }
        // A hidden window (closed on macOS) would ask where nobody sees it.
        crate::app::bring_forward(window, cx);
        confirm_danger(
            "退出 ShellRS？".into(),
            Some(format!("{}有未保存的修改，退出后会丢失。", describe_files(&unsaved)).into()),
            "放弃修改并退出",
            Rc::new(|_, cx| cx.quit()),
            window,
            cx,
        );
        true
    }

    /// Close several center tabs of one batch (关闭其他, 关闭全部…). Editors
    /// with changes, theirs or those of SFTP tabs in the batch, are asked
    /// about once for all, not one dialog each.
    pub(super) fn close_center_tabs(
        &mut self,
        tabs: Vec<CenterTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut editors = Vec::new();
        for tab in &tabs {
            match tab {
                CenterTab::Editor(id) => editors.push(*id),
                CenterTab::Explorer(id) => editors.extend(self.editors_of(*id, cx)),
                _ => {}
            }
        }
        let unsaved = self.unsaved_files(&editors, cx);
        if unsaved.is_empty() {
            for tab in tabs {
                self.close_center_tab(tab, window, cx);
            }
            return;
        }
        let this = cx.weak_entity();
        confirm_danger(
            format!("关闭 {} 个标签？", tabs.len()).into(),
            Some(format!("{}有未保存的修改，关闭后会丢失。", describe_files(&unsaved)).into()),
            "放弃修改并关闭",
            Rc::new(move |window, cx| {
                let tabs = tabs.clone();
                let _ = this.update(cx, |this, cx| {
                    for tab in tabs {
                        match tab {
                            CenterTab::Editor(id) => this.remove_editor(id, window, cx),
                            CenterTab::Explorer(id) => this.close_explorer(id, false, window, cx),
                            tab => this.close_center_tab(tab, window, cx),
                        }
                    }
                });
            }),
            window,
            cx,
        );
    }
}

/// 「“a.conf”」, 「“a.conf”和“b.conf”」, 「“a.conf”等 4 个文件」: the files
/// whose changes a close or a quit would lose.
pub(super) fn describe_files(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => format!("“{one}”"),
        [one, two] => format!("“{one}”和“{two}”"),
        [one, ..] => format!("“{one}”等 {} 个文件", names.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::describe_files;

    #[test]
    fn unsaved_files_are_named_while_they_are_few() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(describe_files(&names(&["a.conf"])), "“a.conf”");
        assert_eq!(
            describe_files(&names(&["a.conf", "b.yml"])),
            "“a.conf”和“b.yml”"
        );
        assert_eq!(
            describe_files(&names(&["a.conf", "b.yml", "c.sh"])),
            "“a.conf”等 3 个文件"
        );
    }
}
