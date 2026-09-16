use super::{FileEntry, FileListing};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{CatalogIcon, ExplorerAction, ExplorerCommand},
    session::SessionId,
    sftp::{DirectoryListing, EntryKind, SharedLocalDirectoryProvider},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    table::{DataTable, TableEvent, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{collections::HashSet, path::PathBuf, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneSide {
    Local,
    Remote,
}
impl PaneSide {
    pub fn label(self) -> &'static str {
        if self == Self::Local {
            "本地"
        } else {
            "远程"
        }
    }
    fn pane_id(self) -> &'static str {
        if self == Self::Local {
            "local-pane"
        } else {
            "remote-pane"
        }
    }
    fn path_id(self) -> &'static str {
        if self == Self::Local {
            "local-path"
        } else {
            "remote-path"
        }
    }
}
pub struct FilePane {
    side: PaneSide,
    session_id: SessionId,
    path: String,
    home: String,
    table: Entity<TableState<FileListing>>,
    path_input: Entity<InputState>,
    selected: Option<String>,
    checked: HashSet<String>,
    anchor: Option<String>,
    request_id: u64,
    loading: bool,
    error: Option<String>,
    transfer_enabled: bool,
    dispatch: FocusHandle,
    _subscriptions: Vec<Subscription>,
    load_task: Option<Task<()>>,
}
impl FilePane {
    pub fn new(
        side: PaneSide,
        session_id: SessionId,
        home: String,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut listing = FileListing::new(side, Vec::new());
        listing.configure(
            session_id,
            home.clone(),
            Rc::new(HashSet::new()),
            false,
            dispatch.clone(),
        );
        let table = cx.new(|cx| {
            TableState::new(listing, window, cx)
                .row_selectable(true)
                .col_selectable(false)
                .cell_selectable(false)
        });
        let path_input = cx.new(|cx| InputState::new(window, cx).default_value(&home));
        let subscriptions = vec![
            cx.subscribe_in(
                &table,
                window,
                |this, table, event: &TableEvent, window, cx| match event {
                    TableEvent::SelectRow(ix) => {
                        this.selected = table
                            .read(cx)
                            .delegate()
                            .entry(*ix)
                            .map(|e| e.name.to_string());
                        table.update(cx, |table, _| {
                            table.delegate_mut().set_active_name(this.selected.clone())
                        });
                        cx.notify();
                    }
                    TableEvent::DoubleClickedRow(ix) => {
                        let entry = table.read(cx).delegate().entry(*ix).cloned();
                        if let Some(entry) = entry
                            && entry.is_dir()
                        {
                            let command = if entry.is_parent() {
                                ExplorerCommand::Up {
                                    remote: this.side == PaneSide::Remote,
                                }
                            } else {
                                ExplorerCommand::Navigate {
                                    remote: this.side == PaneSide::Remote,
                                    path: this.child_path(&entry.name),
                                }
                            };
                            this.dispatch.dispatch_explorer_action(
                                &ExplorerAction::new(this.session_id, command),
                                window,
                                cx,
                            );
                        }
                    }
                    TableEvent::ClearSelection => {
                        this.selected = None;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &path_input,
                window,
                |this, state, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.dispatch.dispatch_explorer_action(
                            &ExplorerAction::new(
                                this.session_id,
                                ExplorerCommand::Navigate {
                                    remote: this.side == PaneSide::Remote,
                                    path: state.read(cx).value().to_string(),
                                },
                            ),
                            window,
                            cx,
                        );
                    }
                },
            ),
        ];
        Self {
            side,
            session_id,
            path: home.clone(),
            home,
            table,
            path_input,
            selected: None,
            checked: HashSet::new(),
            anchor: None,
            request_id: 0,
            loading: false,
            error: None,
            transfer_enabled: false,
            dispatch,
            _subscriptions: subscriptions,
            load_task: None,
        }
    }
    pub fn side(&self) -> PaneSide {
        self.side
    }
    pub fn path(&self) -> String {
        self.path.clone()
    }
    pub fn selected_entry(&self, cx: &App) -> Option<FileEntry> {
        self.selected.as_ref().and_then(|name| {
            self.table
                .read(cx)
                .delegate()
                .rows()
                .iter()
                .find(|e| e.name.as_ref() == name)
                .cloned()
        })
    }
    fn child_path(&self, name: &str) -> String {
        if self.side == PaneSide::Local {
            PathBuf::from(&self.path)
                .join(name)
                .to_string_lossy()
                .into_owned()
        } else {
            format!("{}/{name}", self.path.trim_end_matches('/'))
        }
    }
    pub fn parent_path(&self) -> String {
        if self.side == PaneSide::Local {
            PathBuf::from(&self.path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.clone())
        } else {
            crate::sftp::RemotePath::new(&self.path)
                .map(|p| p.parent().to_string())
                .unwrap_or_else(|_| "/".into())
        }
    }
    pub fn expanded_path(&self, path: &str) -> String {
        if path == "~" {
            self.home.clone()
        } else if let Some(rest) = path.strip_prefix("~/") {
            format!("{}/{rest}", self.home.trim_end_matches('/'))
        } else if (self.side == PaneSide::Local && !PathBuf::from(path).is_absolute())
            || (self.side == PaneSide::Remote && !path.starts_with('/'))
        {
            self.child_path(path)
        } else {
            path.to_string()
        }
    }
    pub fn set_home(&mut self, home: String) {
        if self.home.is_empty() {
            self.path = home.clone();
        }
        self.home = home;
    }
    pub fn set_available(&mut self, available: bool, cx: &mut Context<Self>) {
        self.transfer_enabled = available;
        self.sync_listing(cx);
        cx.notify();
    }
    fn sync_listing(&mut self, cx: &mut Context<Self>) {
        let checked = Rc::new(self.checked.clone());
        self.table.update(cx, |table, cx| {
            table.delegate_mut().configure(
                self.session_id,
                self.path.clone(),
                checked,
                self.transfer_enabled,
                self.dispatch.clone(),
            );
            cx.notify();
        });
    }
    pub fn check(&mut self, name: &str, checked: bool, extend: bool, cx: &mut Context<Self>) {
        let names: Vec<_> = self
            .table
            .read(cx)
            .delegate()
            .rows()
            .iter()
            .filter(|e| !e.is_parent())
            .map(|e| e.name.to_string())
            .collect();
        if !names.iter().any(|n| n == name) {
            return;
        }
        let mut chosen = vec![name.to_string()];
        if extend
            && let (Some(start), Some(end)) = (
                self.anchor
                    .as_ref()
                    .and_then(|anchor| names.iter().position(|n| n == anchor)),
                names.iter().position(|n| n == name),
            )
        {
            chosen = names[start.min(end)..=start.max(end)].to_vec();
        }
        for name in chosen {
            if checked {
                self.checked.insert(name);
            } else {
                self.checked.remove(&name);
            }
        }
        self.anchor = Some(name.into());
        self.sync_listing(cx);
        cx.notify();
    }
    pub fn toggle_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(name) = self.selected.clone() {
            self.check(&name, !self.checked.contains(&name), false, cx);
        }
    }
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.checked = self
            .table
            .read(cx)
            .delegate()
            .rows()
            .iter()
            .filter(|e| !e.is_parent())
            .map(|e| e.name.to_string())
            .collect();
        self.sync_listing(cx);
        cx.notify();
    }
    pub fn upload_sources(&self) -> Vec<PathBuf> {
        let mut sources: Vec<_> = self
            .checked
            .iter()
            .map(|name| PathBuf::from(&self.path).join(name))
            .collect();
        sources.sort();
        sources
    }
    pub fn disconnected(&mut self, message: String, cx: &mut Context<Self>) {
        self.request_id += 1;
        self.loading = false;
        self.error = Some(message);
        cx.notify();
    }
    pub fn begin_load(&mut self, cx: &mut Context<Self>) -> u64 {
        self.request_id += 1;
        self.loading = true;
        self.error = None;
        cx.notify();
        self.request_id
    }
    pub fn apply_listing(
        &mut self,
        request_id: u64,
        result: Result<DirectoryListing, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if request_id != self.request_id {
            return;
        }
        self.loading = false;
        match result {
            Ok(listing) => {
                if listing.path() != self.path {
                    self.checked.clear();
                    self.selected = None;
                    self.anchor = None;
                }
                self.path = listing.path().into();
                self.error = None;
                let mut rows: Vec<_> = listing
                    .entries()
                    .iter()
                    .map(|entry| {
                        let metadata = entry.metadata();
                        let modified = metadata
                            .modified()
                            .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                            .map(|t| {
                                t.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d %H:%M")
                                    .to_string()
                            })
                            .unwrap_or_else(|| "—".into());
                        let permissions = metadata
                            .permissions()
                            .map(|p| format!("{:03o}", p & 0o777))
                            .unwrap_or_else(|| "—".into());
                        match metadata.kind() {
                            EntryKind::Directory => {
                                FileEntry::dir(entry.name(), &modified, &permissions)
                            }
                            EntryKind::Symlink => {
                                FileEntry::symlink(entry.name(), &modified, &permissions)
                            }
                            _ => FileEntry::file(
                                entry.name(),
                                metadata.size(),
                                &modified,
                                &permissions,
                            ),
                        }
                    })
                    .collect();
                rows.sort_by(|a, b| {
                    b.is_dir()
                        .cmp(&a.is_dir())
                        .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                self.checked
                    .retain(|name| rows.iter().any(|e| e.name.as_ref() == name));
                if self.parent_path() != self.path {
                    rows.insert(0, FileEntry::parent());
                }
                self.table.update(cx, |table, cx| {
                    table.delegate_mut().set_rows(rows);
                    table.clear_selection(cx);
                    table.refresh(cx);
                });
                self.sync_listing(cx);
                let display = if self.path == self.home {
                    "~".into()
                } else if let Some(rest) = self
                    .path
                    .strip_prefix(&format!("{}/", self.home.trim_end_matches('/')))
                {
                    format!("~/{rest}")
                } else {
                    self.path.clone()
                };
                self.path_input
                    .update(cx, |input, cx| input.set_value(display, window, cx));
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }
    pub fn load_local(
        &mut self,
        path: String,
        provider: SharedLocalDirectoryProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.begin_load(cx);
        self.load_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    provider
                        .list(&PathBuf::from(path))
                        .map_err(|e| format!("无法读取目录：{e}"))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.apply_listing(id, result, window, cx)
            });
        }));
    }
}
impl Render for FilePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let remote = self.side == PaneSide::Remote;
        let sid = self.session_id;
        let button =
            |id: &'static str, icon: IconName, tip: &'static str, command: ExplorerCommand| {
                let dispatch = self.dispatch.clone();
                Button::new(id)
                    .ghost()
                    .small()
                    .icon(icon)
                    .tooltip(tip)
                    .on_click(move |_, window, cx| {
                        dispatch.dispatch_explorer_action(
                            &ExplorerAction::new(sid, command.clone()),
                            window,
                            cx,
                        )
                    })
            };
        let dispatch = self.dispatch.clone();
        let file_count = self
            .table
            .read(cx)
            .delegate()
            .rows()
            .iter()
            .filter(|e| !e.is_parent())
            .count();
        v_flex()
            .id((self.side.pane_id(), sid.0))
            .test_support()
            .size_full()
            .min_w_0()
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .w_10()
                            .flex_shrink_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.side.label()),
                    )
                    .child(
                        Input::new(&self.path_input)
                            .id(self.side.path_id())
                            .small()
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(
                        button(
                            "up",
                            IconName::ArrowUp,
                            "上级目录",
                            ExplorerCommand::Up { remote },
                        )
                        .disabled(self.parent_path() == self.path),
                    )
                    .child(button(
                        "refresh",
                        IconName::Redo,
                        "刷新",
                        ExplorerCommand::Refresh { remote },
                    )),
            )
            .when(!remote, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("upload")
                                .outline()
                                .small()
                                .icon(Icon::new(CatalogIcon::Upload))
                                .label("上传所选…")
                                .tooltip("上传所选文件（F5）")
                                .disabled(!self.transfer_enabled || self.checked.is_empty())
                                .on_click({
                                    let dispatch = dispatch.clone();
                                    move |_, w, cx| {
                                        dispatch.dispatch_explorer_action(
                                            &ExplorerAction::new(
                                                sid,
                                                ExplorerCommand::UploadSelected,
                                            ),
                                            w,
                                            cx,
                                        )
                                    }
                                }),
                        )
                        .child(
                            Button::new("choose-upload")
                                .ghost()
                                .small()
                                .label("选择文件上传…")
                                .disabled(!self.transfer_enabled)
                                .on_click(move |_, w, cx| {
                                    dispatch.dispatch_explorer_action(
                                        &ExplorerAction::new(sid, ExplorerCommand::ChooseFiles),
                                        w,
                                        cx,
                                    )
                                }),
                        ),
                )
            })
            .when(remote, |this| {
                this.child(
                    h_flex()
                        .px_2()
                        .py_1()
                        .h_8()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("拖入文件或目录以上传"),
                )
            })
            .when(self.loading, |this| {
                this.child(h_flex().px_2().py_1().text_sm().child("正在读取目录…"))
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("directory-error")
                        .test_support()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .key_context(if remote {
                        "RemoteFileList"
                    } else {
                        "LocalFileList"
                    })
                    .when(remote, |this| {
                        let dispatch = self.dispatch.clone();
                        let path = self.path.clone();
                        let enabled = self.transfer_enabled;
                        this.drag_over::<ExternalPaths>(|style, _, _, cx| {
                            style.bg(cx.theme().muted)
                        })
                        .on_drop(move |paths: &ExternalPaths, window, cx| {
                            if enabled {
                                dispatch.dispatch_explorer_action(
                                    &ExplorerAction::new(
                                        sid,
                                        ExplorerCommand::UploadPaths {
                                            paths: paths.paths().to_vec(),
                                            target: path.clone(),
                                        },
                                    ),
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            }
                        })
                        .on_drop({
                            let dispatch = self.dispatch.clone();
                            let path = self.path.clone();
                            move |drag: &super::file_listing::LocalFilesDrag, window, cx| {
                                if enabled {
                                    dispatch.dispatch_explorer_action(
                                        &ExplorerAction::new(
                                            sid,
                                            ExplorerCommand::UploadPaths {
                                                paths: drag.paths.clone(),
                                                target: path.clone(),
                                            },
                                        ),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }
                            }
                        })
                    })
                    .child(
                        DataTable::new(&self.table)
                            .stripe(false)
                            .bordered(false)
                            .small(),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if remote {
                        format!("{file_count} 个项目")
                    } else {
                        format!("{file_count} 个项目 · 已选择 {} 项", self.checked.len())
                    }),
            )
    }
}
