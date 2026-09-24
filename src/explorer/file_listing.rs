use crate::app::ExplorerDispatch as _;
use crate::{
    app::{CatalogIcon, ExplorerAction, ExplorerCommand},
    session::SessionId,
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::RefCell, cmp::Ordering, path::PathBuf, rc::Rc};

use super::{
    ClickMode, FileEntry, FileKind, FilePane, PaneSide, Selection, format_changed,
    format_kilobytes, format_rights,
};

/// What the row closures need from the pane, pushed in by `FilePane` so
/// rendering never reads another entity.
#[derive(Clone)]
pub(super) struct ListingContext {
    pub session: SessionId,
    pub path: String,
    pub selection: Rc<Selection>,
    pub transfer_enabled: bool,
    pub dispatch: Option<FocusHandle>,
    pub pane: Option<WeakEntity<FilePane>>,
    /// The row under the last right-click, read by the list's context menu.
    pub menu_hit: Rc<RefCell<Option<String>>>,
}

impl Default for ListingContext {
    fn default() -> Self {
        Self {
            session: SessionId(0),
            path: String::new(),
            selection: Rc::default(),
            transfer_enabled: false,
            dispatch: None,
            pane: None,
            menu_hit: Rc::default(),
        }
    }
}

/// The rows and columns of one explorer pane, for `DataTable`. Selection is
/// the pane's; the table's own row selection stays off.
pub struct FileListing {
    columns: Vec<Column>,
    rows: Vec<FileEntry>,
    sort: Option<(usize, ColumnSort)>,
    side: PaneSide,
    context: ListingContext,
}

impl FileListing {
    pub fn new(side: PaneSide, rows: Vec<FileEntry>) -> Self {
        // Column widths are an API boundary that takes `Pixels`.
        let columns = match side {
            PaneSide::Local => vec![
                Column::new("name", "名称").width(px(260.)).sortable(),
                Column::new("size", "大小").width(px(100.)).sortable(),
                Column::new("type", "类型").width(px(100.)).sortable(),
                Column::new("modified", "修改时间")
                    .width(px(150.))
                    .sortable(),
            ],
            PaneSide::Remote => vec![
                Column::new("name", "名称").width(px(240.)).sortable(),
                Column::new("size", "大小").width(px(100.)).sortable(),
                Column::new("modified", "修改时间")
                    .width(px(150.))
                    .sortable(),
                Column::new("rights", "权限").width(px(96.)).sortable(),
                Column::new("owner", "所有者").width(px(80.)).sortable(),
            ],
        };
        let mut listing = Self {
            columns,
            rows: Vec::new(),
            sort: None,
            side,
            context: ListingContext::default(),
        };
        listing.set_rows(rows);
        listing
    }

    pub(super) fn configure(&mut self, context: ListingContext) {
        self.context = context;
    }

    pub fn rows(&self) -> &[FileEntry] {
        &self.rows
    }

    /// Row names in display order, `..` included.
    pub fn order(&self) -> Vec<&str> {
        self.rows.iter().map(|row| row.name.as_ref()).collect()
    }

    pub fn entry(&self, ix: usize) -> Option<&FileEntry> {
        self.rows.get(ix)
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.name.as_ref() == name)
    }

    pub fn set_rows(&mut self, rows: Vec<FileEntry>) {
        self.rows = rows;
        self.apply_sort();
    }

    /// Sort by the active column, by name without one. `..` stays first and
    /// directories (links to directories included) stay ahead of files.
    fn apply_sort(&mut self) {
        let (key, descending) = self
            .sort
            .and_then(|(col_ix, sort)| {
                self.columns
                    .get(col_ix)
                    .map(|column| (column.key.clone(), sort == ColumnSort::Descending))
            })
            .unwrap_or_else(|| ("name".into(), false));
        let start = usize::from(self.rows.first().is_some_and(FileEntry::is_parent));
        self.rows[start..].sort_by(|a, b| {
            let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
            let by_key = match key.as_ref() {
                "size" => a.size.cmp(&b.size),
                "modified" => a.modified.cmp(&b.modified),
                "type" => a.type_label().cmp(&b.type_label()),
                "rights" => a
                    .permissions
                    .map(format_rights)
                    .cmp(&b.permissions.map(format_rights)),
                "owner" => a.owner.cmp(&b.owner),
                _ => Ordering::Equal,
            }
            .then_with(by_name);
            let by_key = if descending { by_key.reverse() } else { by_key };
            b.is_dir().cmp(&a.is_dir()).then(by_key)
        });
    }
}

fn icon_for(entry: &FileEntry) -> Icon {
    if entry.is_parent() {
        return Icon::new(CatalogIcon::FolderUp);
    }
    match (entry.kind, entry.target) {
        (FileKind::Dir, _) => Icon::new(IconName::Folder),
        (FileKind::File, _) => Icon::new(IconName::File),
        (FileKind::Symlink, Some(FileKind::Dir)) => Icon::new(CatalogIcon::FolderSymlink),
        (FileKind::Symlink, _) => Icon::new(CatalogIcon::FileSymlink),
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

    /// `Column::text_right` is not applied by `DataTable`, so the size
    /// column aligns itself.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        h_flex()
            .size_full()
            .when(column.key.as_ref() == "size", |this| this.justify_end())
            .child(column.name.clone())
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
        let parent = entry.is_parent();
        match self.columns[col_ix].key.as_ref() {
            "name" => h_flex()
                .id(ElementId::Name(format!("name:{}", entry.name).into()))
                .test_support()
                .aria_selected(self.context.selection.contains(&entry.name))
                .gap_2()
                .child(
                    icon_for(entry)
                        .small()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(entry.name.clone())
                .into_any_element(),
            "size" => h_flex()
                .w_full()
                .justify_end()
                .when(!entry.is_dir(), |this| {
                    this.child(format_kilobytes(entry.size))
                })
                .into_any_element(),
            "type" => entry.type_label().into_any_element(),
            "modified" => if parent {
                String::new()
            } else {
                entry.modified.map(format_changed).unwrap_or_default()
            }
            .into_any_element(),
            "rights" => div()
                .font_family(cx.theme().mono_font_family.clone())
                .when(!parent, |this| {
                    this.children(entry.permissions.map(format_rights))
                })
                .into_any_element(),
            "owner" => div()
                .when(!parent, |this| this.children(entry.owner.clone()))
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(entry) = self.rows.get(row_ix) else {
            return div().id(("empty-file-row", row_ix));
        };
        let name = entry.name.to_string();
        let context = &self.context;
        let selected = context.selection.contains(&name);
        let cursor = context.selection.cursor() == Some(name.as_str());
        let row = div()
            .id(ElementId::Name(format!("file:{name}").into()))
            // Selected rows and the cursor are painted here, under the cells;
            // the hover background replaces a plain row background.
            .when(selected, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(cx.theme().tokens.table_active),
                )
            })
            .when(cursor, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(cx.theme().table_active_border),
                )
            });
        let (Some(dispatch), Some(pane)) = (context.dispatch.clone(), context.pane.clone()) else {
            return row;
        };
        let sid = context.session;
        let remote = self.side == PaneSide::Remote;
        let row = row
            .on_click({
                let name = name.clone();
                let pane = pane.clone();
                let dispatch = dispatch.clone();
                move |event, window, cx| {
                    let modifiers = event.modifiers();
                    let mode = if modifiers.shift {
                        ClickMode::Extend
                    } else if modifiers.secondary() {
                        ClickMode::Toggle
                    } else {
                        ClickMode::Replace
                    };
                    let _ = pane.update(cx, |pane, cx| pane.click_row(&name, mode, cx));
                    if mode == ClickMode::Replace && event.click_count() == 2 {
                        dispatch.dispatch_explorer_action(
                            &ExplorerAction::new(sid, ExplorerCommand::Open { remote }),
                            window,
                            cx,
                        );
                    }
                }
            })
            .on_mouse_down(MouseButton::Right, {
                let name = name.clone();
                let menu_hit = context.menu_hit.clone();
                move |_, _, cx| {
                    let hit = (name != "..").then(|| name.clone());
                    if let Some(hit) = &hit {
                        let _ = pane.update(cx, |pane, cx| pane.select_for_menu(hit, cx));
                    }
                    *menu_hit.borrow_mut() = hit;
                }
            });
        if entry.is_parent() {
            return row;
        }
        let names = if selected {
            context.selection.targets(&self.order())
        } else {
            vec![name.clone()]
        };
        let enabled = context.transfer_enabled;
        let child = |name: &str| match self.side {
            PaneSide::Local => PathBuf::from(&context.path)
                .join(name)
                .to_string_lossy()
                .into_owned(),
            PaneSide::Remote => format!("{}/{name}", context.path.trim_end_matches('/')),
        };
        // Dragging carries the selection when the dragged row is part of it.
        let row = match self.side {
            PaneSide::Local => {
                let paths = names
                    .iter()
                    .map(|name| PathBuf::from(child(name)))
                    .collect();
                row.on_drag(LocalFilesDrag { paths }, |drag, _, _, cx| {
                    cx.new(|_| FileDragPreview {
                        verb: "上传",
                        count: drag.paths.len(),
                    })
                })
            }
            PaneSide::Remote => {
                let paths = names.iter().map(|name| child(name)).collect();
                row.on_drag(RemoteFilesDrag { paths }, |drag, _, _, cx| {
                    cx.new(|_| FileDragPreview {
                        verb: "下载",
                        count: drag.paths.len(),
                    })
                })
            }
        };
        if !entry.is_dir() {
            return row;
        }
        // Dropping on a directory row puts the items in that directory.
        let target = child(&name);
        match self.side {
            PaneSide::Remote => row
                .on_drop({
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
                }),
            PaneSide::Local => row.on_drop(move |drag: &RemoteFilesDrag, window, cx| {
                if enabled {
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(
                            sid,
                            ExplorerCommand::DownloadPaths {
                                paths: drag.paths.clone(),
                                target: target.clone(),
                            },
                        ),
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
            }),
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
/// Remote rows dragged toward the local pane, as remote paths.
#[derive(Clone)]
pub(super) struct RemoteFilesDrag {
    pub paths: Vec<String>,
}
struct FileDragPreview {
    verb: &'static str,
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
            .child(format!("{} {} 个项目", self.verb, self.count))
    }
}
