use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::*;

use super::{FileEntry, FileKind, PaneSide, format_size};

/// The rows and columns of one explorer pane, for `DataTable`.
pub struct FileListing {
    columns: Vec<Column>,
    rows: Vec<FileEntry>,
    sort: Option<(usize, ColumnSort)>,
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
        if side == PaneSide::Remote {
            columns.push(Column::new("permissions", "权限").width(px(110.)));
        }
        Self {
            columns,
            rows,
            sort: None,
        }
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
        let name = self
            .rows
            .get(row_ix)
            .map(|entry| entry.name.to_string())
            .unwrap_or_default();
        div().id(ElementId::Name(format!("file:{name}").into()))
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
