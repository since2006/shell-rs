use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

use super::Workspace;
use crate::analytics::Counter;
use crate::app::CatalogIcon;
use crate::editor::{EditorKey, EditorSource};
use crate::explorer::{FileEntry, FileIcon, FileIconKind, FileLocation};
use crate::i18n::t;
use crate::sftp::{SharedLocalDirectoryProvider, describe_read_failure, read_local_text};
use crate::terminal::LocalTerminalId;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _,
    WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::ListItem,
    notification::Notification,
    v_flex,
};
use gpui_kit::*;

pub(super) struct LocalFiles {
    provider: SharedLocalDirectoryProvider,
    directory: PathBuf,
    terminal_directory: PathBuf,
    follow: bool,
    entries: Rc<Vec<FileEntry>>,
    error: Option<String>,
    load: Option<Task<()>>,
}

struct OpenFile(PathBuf);
impl EventEmitter<OpenFile> for LocalFiles {}

impl Workspace {
    pub(super) fn local_files_for(
        &mut self,
        id: LocalTerminalId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<LocalFiles> {
        if let Some(files) = self.local_files.get(&id) {
            return files.clone();
        }
        let provider = self.local_directory_provider.clone();
        let directory = self
            .local_terminals
            .get(&id)
            .and_then(|terminal| terminal.read(cx).directory())
            .map(Path::to_owned)
            .unwrap_or_else(|| provider.home());
        let follow = self.directory_follow_setting(cx);
        let files = cx.new(|cx| {
            let mut files = LocalFiles {
                provider,
                directory: directory.clone(),
                terminal_directory: directory.clone(),
                follow,
                entries: Rc::default(),
                error: None,
                load: None,
            };
            files.navigate(directory, window, cx);
            files
        });
        cx.subscribe_in(&files, window, |this, _, event: &OpenFile, window, cx| {
            this.open_sidebar_file(event.0.clone(), window, cx);
        })
        .detach();
        self.local_files.insert(id, files.clone());
        files
    }

    fn show_local_editor(&self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let key = EditorKey::Local(path.to_owned());
        if let Some(editor) = self
            .editors
            .values()
            .find(|editor| editor.read(cx).key() == key)
        {
            self.show_editor(editor, window, cx);
            return true;
        }
        false
    }

    fn open_sidebar_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_local_editor(&path, window, cx) {
            return;
        }
        let provider = self.local_directory_provider.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (path, result) = cx
                .background_spawn(async move {
                    let result = read_local_text(provider.as_ref(), &path);
                    (path, result)
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(file) => {
                    if !this.show_local_editor(&path, window, cx) {
                        this.count(Counter::EditorLocal);
                        this.create_editor(
                            EditorSource::Local {
                                provider: this.local_directory_provider.clone(),
                            },
                            FileLocation::Local(path),
                            Rc::new(file),
                            window,
                            cx,
                        );
                    }
                }
                Err(error) => window.push_notification(
                    Notification::error(t!("explorer.file.open_failed", name = path.display()))
                        .message(describe_read_failure(&error)),
                    cx,
                ),
            })
            .ok();
        })
        .detach();
    }
}

impl LocalFiles {
    pub(super) fn terminal_moved(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.terminal_directory = path.to_owned();
        if self.follow && self.directory != path {
            self.navigate(path.to_owned(), window, cx);
        }
    }

    fn navigate(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.directory = path.clone();
        self.entries = Rc::default();
        self.error = None;
        let provider = self.provider.clone();
        self.load = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let listing = provider.list(&path)?;
                    let mut entries: Vec<_> = listing
                        .entries()
                        .iter()
                        .map(FileEntry::from_directory_entry)
                        .collect();
                    entries
                        .sort_by_cached_key(|entry| (!entry.is_dir(), entry.name.to_lowercase()));
                    anyhow::Ok((PathBuf::from(listing.path()), entries))
                })
                .await;
            this.update(cx, |this, cx| {
                this.load = None;
                match result {
                    Ok((directory, entries)) => {
                        this.directory = directory;
                        this.entries = Rc::new(entries);
                    }
                    Err(error) => {
                        this.error =
                            Some(t!("explorer.list.read_failed", error = error).to_string())
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }
}

impl Render for LocalFiles {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entries = self.entries.clone();
        let directory = self.directory.clone();
        let panel = cx.weak_entity();
        let toolbar = h_flex()
            .gap_1()
            .px_2()
            .child(
                Button::new("local-files-up")
                    .ghost()
                    .small()
                    .icon(IconName::ArrowUp)
                    .tooltip(t!("explorer.command.up"))
                    .disabled(directory.parent().is_none())
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(parent) = this.directory.parent() {
                            this.navigate(parent.to_owned(), window, cx);
                        }
                    })),
            )
            .child(
                Button::new("local-files-refresh")
                    .ghost()
                    .small()
                    .icon(Icon::new(CatalogIcon::RefreshCw))
                    .tooltip(t!("explorer.command.refresh"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.navigate(this.directory.clone(), window, cx)
                    })),
            )
            .child(
                Button::new("local-files-follow")
                    .ghost()
                    .small()
                    .label(t!("explorer.follow.label"))
                    .selected(self.follow)
                    .tooltip(t!("explorer.follow.tooltip"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.follow = !this.follow;
                        if this.follow {
                            this.navigate(this.terminal_directory.clone(), window, cx);
                        }
                        cx.notify();
                    })),
            );
        let content = if let Some(error) = &self.error {
            div()
                .px_2()
                .text_sm()
                .child(error.clone())
                .into_any_element()
        } else if entries.is_empty() {
            let label = if self.load.is_some() {
                t!("explorer.list.reading")
            } else {
                t!("explorer.list.empty")
            };
            div().px_2().text_sm().child(label).into_any_element()
        } else {
            uniform_list("local-files-rows", entries.len(), move |range, _, _| {
                range
                    .map(|index| {
                        let entry = &entries[index];
                        let dir = entry.is_dir();
                        let kind = match (dir, entry.is_link()) {
                            (true, true) => FileIconKind::FolderLink,
                            (true, false) => FileIconKind::Folder,
                            (false, true) => FileIconKind::FileLink,
                            (false, false) => FileIconKind::File,
                        };
                        let path =
                            directory.join(entry.target_path.as_deref().unwrap_or(&entry.name));
                        let panel = panel.clone();
                        ListItem::new(SharedString::from(format!("local-file:{}", entry.name)))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(FileIcon::new(entry.name.clone(), kind))
                                    .child(div().truncate().child(entry.name.clone())),
                            )
                            .on_click(move |_, window, cx| {
                                panel
                                    .update(cx, |this, cx| {
                                        if dir {
                                            this.navigate(path.clone(), window, cx);
                                        } else {
                                            cx.emit(OpenFile(path.clone()));
                                        }
                                    })
                                    .ok();
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .size_full()
            .into_any_element()
        };
        v_flex()
            .size_full()
            .child(toolbar)
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(self.directory.display().to_string()),
            )
            .child(div().flex_1().min_h_0().child(content))
    }
}
