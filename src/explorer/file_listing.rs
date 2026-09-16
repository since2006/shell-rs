use crate::app::ExplorerDispatch as _;
use crate::{
    app::{ExplorerAction, ExplorerCommand},
    session::SessionId,
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::*;
use std::{collections::HashSet, path::PathBuf, rc::Rc};

use gpui_kit::component::checkbox::Checkbox;

use super::{FileEntry, FileKind, PaneSide, format_size};

/// The rows and columns of one explorer pane, for `DataTable`.
pub struct FileListing {
    columns: Vec<Column>,
    rows: Vec<FileEntry>,
    sort: Option<(usize, ColumnSort)>,
    active_name: Option<String>,
    side: PaneSide,
    session: SessionId,
    path: String,
    checked: Rc<HashSet<String>>,
    enabled: bool,
    dispatch: Option<FocusHandle>,
}

impl FileListing {
    pub fn new(side: PaneSide, rows: Vec<FileEntry>) -> Self {
        // Column widths are an API boundary that takes `Pixels`.
        let mut columns = vec![
            Column::new("name", "名称").width(px(240.)).sortable(),
            Column::new("size", "大小")
                .width(px(90.))
                .text_right()
                .sortable(),
            Column::new("type", "类型").width(px(96.)),
            Column::new("modified", "修改时间")
                .width(px(150.))
                .sortable(),
        ];
        if side == PaneSide::Local {
            columns.insert(0, Column::new("check", "").width(px(36.)));
        }
        if side == PaneSide::Remote {
            columns.push(Column::new("permissions", "权限").width(px(110.)));
        }
        Self {
            columns,
            rows,
            sort: None,
            active_name: None,
            side,
            session: SessionId(0),
            path: String::new(),
            checked: Rc::new(HashSet::new()),
            enabled: false,
            dispatch: None,
        }
    }

    pub fn set_active_name(&mut self, name: Option<String>) {
        self.active_name = name;
    }
    pub fn configure(
        &mut self,
        session: SessionId,
        path: String,
        checked: Rc<HashSet<String>>,
        enabled: bool,
        dispatch: FocusHandle,
    ) {
        self.session = session;
        self.path = path;
        self.checked = checked;
        self.enabled = enabled;
        self.dispatch = Some(dispatch);
    }

    pub fn rows(&self) -> &[FileEntry] {
        &self.rows
    }

    pub fn entry(&self, ix: usize) -> Option<&FileEntry> {
        self.rows.get(ix)
    }

    pub fn set_rows(&mut self, rows: Vec<FileEntry>) {
        self.rows = rows;
        self.apply_sort();
    }

    /// Re-sort by the active column; `..` stays first and directories stay
    /// ahead of files.
    fn apply_sort(&mut self) {
        let Some((col_ix, sort)) = self.sort else {
            return;
        };
        let Some(key) = self.columns.get(col_ix).map(|column| column.key.clone()) else {
            return;
        };
        let has_parent = self.rows.first().is_some_and(FileEntry::is_parent);
        let start = usize::from(has_parent);
        let rows = &mut self.rows[start..];
        rows.sort_by(|a, b| {
            let by_kind = b.is_dir().cmp(&a.is_dir());
            let by_key = match key.as_ref() {
                "size" => a.size.cmp(&b.size),
                "modified" => a.modified.cmp(&b.modified),
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            };
            let by_key = if sort == ColumnSort::Descending {
                by_key.reverse()
            } else {
                by_key
            };
            by_kind.then(by_key)
        });
    }
}

impl TableDelegate for FileListing {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(entry) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        match self.columns[col_ix].key.as_ref() {
            "check" => {
                if entry.is_parent() {
                    return div().into_any_element();
                }
                let name = entry.name.to_string();
                let checked = self.checked.contains(&name);
                let dispatch = self.dispatch.clone();
                let sid = self.session;
                Checkbox::new(ElementId::Name(format!("check:{name}").into()))
                    .checked(checked)
                    .on_change(move |checked, window, cx| {
                        if let Some(dispatch) = &dispatch {
                            dispatch.dispatch_explorer_action(
                                &ExplorerAction::new(
                                    sid,
                                    ExplorerCommand::Check {
                                        name: name.clone(),
                                        checked: *checked,
                                        extend: window.modifiers().shift,
                                    },
                                ),
                                window,
                                cx,
                            );
                        }
                    })
                    .into_any_element()
            }
            "name" => {
                let icon = match entry.kind {
                    FileKind::Dir => IconName::Folder,
                    FileKind::File => IconName::File,
                    FileKind::Symlink => IconName::ExternalLink,
                };
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(icon)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(entry.name.clone())
                    .into_any_element()
            }
            "size" => if entry.is_dir() {
                "—".to_string()
            } else {
                format_size(entry.size)
            }
            .into_any_element(),
            "type" => if entry.is_parent() {
                String::new()
            } else {
                entry.type_label()
            }
            .into_any_element(),
            "modified" => entry.modified.clone().into_any_element(),
            "permissions" => div()
                .font_family(cx.theme().mono_font_family.clone())
                .child(entry.permissions.clone())
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(entry) = self.rows.get(row_ix) else {
            return div().id(("empty-file-row", row_ix));
        };
        let name = entry.name.to_string();
        let row = div().id(ElementId::Name(format!("file:{name}").into()));
        let Some(dispatch) = self.dispatch.clone() else {
            return row;
        };
        let sid = self.session;
        if self.side == PaneSide::Local && !entry.is_parent() {
            let names = if self.checked.contains(&name) {
                self.checked.iter().cloned().collect::<Vec<_>>()
            } else {
                vec![name.clone()]
            };
            let paths = names
                .iter()
                .map(|name| PathBuf::from(&self.path).join(name))
                .collect();
            row.on_drag(LocalFilesDrag { paths }, |drag, _, _, cx| {
                cx.new(|_| FileDragPreview {
                    count: drag.paths.len(),
                })
            })
        } else if self.side == PaneSide::Remote && entry.is_dir() && !entry.is_parent() {
            let target = format!("{}/{name}", self.path.trim_end_matches('/'));
            let enabled = self.enabled;
            row.on_drop({
                let dispatch = dispatch.clone();
                let target = target.clone();
                move |paths: &ExternalPaths, window, cx| {
                    if enabled {
                        dispatch.dispatch_explorer_action(
                            &ExplorerAction::new(
                                sid,
                                ExplorerCommand::UploadPaths {
                                    paths: paths.paths().to_vec(),
                                    target: target.clone(),
                                },
                            ),
                            window,
                            cx,
                        );
                        cx.stop_propagation();
                    }
                }
            })
            .on_drop(move |drag: &LocalFilesDrag, window, cx| {
                if enabled {
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(
                            sid,
                            ExplorerCommand::UploadPaths {
                                paths: drag.paths.clone(),
                                target: target.clone(),
                            },
                        ),
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
            })
        } else {
            row
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.sort = match sort {
            ColumnSort::Default => None,
            sort => Some((col_ix, sort)),
        };
        self.apply_sort();
        let index = self
            .active_name
            .as_ref()
            .and_then(|name| self.rows.iter().position(|e| e.name.as_ref() == name));
        let entity = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |table, cx| {
                if let Some(index) = index {
                    table.set_selected_row(index, cx);
                } else {
                    table.clear_selection(cx);
                }
            });
        });
        cx.notify();
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("空目录")
    }
}

#[derive(Clone)]
pub(super) struct LocalFilesDrag {
    pub paths: Vec<PathBuf>,
}
struct FileDragPreview {
    count: usize,
}
impl Render for FileDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(format!("上传 {} 个项目", self.count))
    }
}
