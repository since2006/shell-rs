use crate::app::ExplorerDispatch as _;
use crate::app::{CatalogIcon, ExplorerAction, ExplorerCommand};
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex,
    table::{Column, ColumnSort, TableDelegate, TableState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{
    cell::{Cell, RefCell},
    cmp::Ordering,
    path::PathBuf,
    rc::Rc,
};

use super::{
    ClickMode, ExplorerId, FileEntry, FileKind, FilePane, FileSizeFormat, PaneSide, Selection,
    child_path, format_changed, format_rights,
};

/// What the row closures need from the pane, pushed in by `FilePane` so
/// rendering never reads another entity.
#[derive(Clone, Default)]
pub(super) struct ListingContext {
    pub explorer: ExplorerId,
    pub path: String,
    pub selection: Rc<Selection>,
    pub transfer_enabled: bool,
    pub dispatch: Option<FocusHandle>,
    pub pane: Option<WeakEntity<FilePane>>,
    /// What the last right-click landed on, read by the list's context menu.
    pub menu_hit: Rc<RefCell<Option<MenuHit>>>,
    /// Where the rows are, for the selection rectangle.
    pub geometry: Rc<ListGeometry>,
    /// What the list says while it has no rows.
    pub placeholder: SharedString,
}

/// Where the list and its rows are on screen, recorded as they paint, so a
/// selection rectangle can tell which rows it covers. Rows are all the same
/// height, so one row tells where all of them are.
#[derive(Default)]
pub(super) struct ListGeometry {
    /// Where the first row's top is (scrolled out of view or not), and the
    /// row height.
    pub rows: Cell<Option<(Pixels, Pixels)>>,
    /// The list, header included.
    pub list: Cell<Option<Bounds<Pixels>>>,
    /// Where the left button went down in the list last.
    pub press: Cell<Option<Point<Pixels>>>,
    /// What the last press in the list landed on.
    pub pressed: Cell<Pressed>,
}

/// What a press in the list landed on. As in WinSCP without full row select,
/// everything but the name cells and the column titles is empty space:
/// clicking there clears the selection, and dragging from there draws a
/// rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Pressed {
    #[default]
    Empty,
    /// A name cell, which clicks and right-clicks select. Dragging its icon
    /// and text moves files; dragging the rest of it draws a rectangle.
    Name,
    /// A column title.
    Title,
}

/// How far a press may move and still be a click: GPUI's own drag
/// threshold, which is private.
const DRAG_THRESHOLD: f64 = 2.;

/// Whether a click is the end of a drag instead. GPUI forgets every pending
/// press once it paints a frame with a drag active, but a drag started and
/// let go within one frame (a quick flick) still clicks each element both
/// ends were over, bar the drag's own: a rectangle or a file drag let go
/// over the name cell it began in must not select that row alone.
fn ends_a_drag(event: &ClickEvent) -> bool {
    match event {
        ClickEvent::Mouse(click) => {
            (click.up.position - click.down.position).magnitude() > DRAG_THRESHOLD
        }
        _ => false,
    }
}

/// Where a right-click in the list landed, when not on empty space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MenuHit {
    /// A file or folder, by name.
    Item(String),
    /// A column title, by the column's key.
    Column(SharedString),
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

/// WinSCP's columns for each side. Column widths are an API boundary that
/// takes `Pixels`.
fn columns(side: PaneSide) -> Vec<Column> {
    match side {
        PaneSide::Local => vec![
            name_column(px(260.)),
            Column::new("size", "大小").width(px(100.)).sortable(),
            Column::new("type", "类型").width(px(100.)).sortable(),
            Column::new("modified", "修改时间")
                .width(px(150.))
                .sortable(),
        ],
        PaneSide::Remote => vec![
            name_column(px(240.)),
            Column::new("size", "大小").width(px(100.)).sortable(),
            Column::new("modified", "修改时间")
                .width(px(150.))
                .sortable(),
            Column::new("rights", "权限").width(px(96.)).sortable(),
            Column::new("owner", "所有者").width(px(80.)).sortable(),
        ],
    }
}

impl FileListing {
    pub fn new(side: PaneSide) -> Self {
        Self {
            columns: columns(side),
            rows: Vec::new(),
            sort: None,
            side,
            context: ListingContext::default(),
        }
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

    /// Where a row's item is, on its side.
    fn child_path(&self, name: &str) -> String {
        child_path(&self.context.path, name, self.side == PaneSide::Remote)
    }

    /// Let `cell` drag its row's item to the other pane, with the rest of the
    /// selection when the row is part of it. Pressing it selects an item that
    /// is not selected yet, alone, so what is highlighted is what goes.
    fn drag_from(&self, cell: Stateful<Div>, name: &str) -> Stateful<Div> {
        let names = if self.context.selection.contains(name) {
            self.context.selection.targets(&self.order())
        } else {
            vec![name.to_string()]
        };
        // ⌘ and Shift presses are left to the name's click.
        let cell = match self.context.pane.clone() {
            Some(pane) => cell.on_mouse_down(MouseButton::Left, {
                let name = name.to_string();
                move |event, _, cx| {
                    if !event.modifiers.secondary() && !event.modifiers.shift {
                        let _ = pane.update(cx, |pane, cx| {
                            pane.clear_menu_row(cx);
                            pane.select_pressed(&name, cx);
                        });
                    }
                }
            }),
            None => cell,
        };
        let paths = names.iter().map(|name| self.child_path(name));
        match self.side {
            PaneSide::Local => drag_files(cell, paths.map(PathBuf::from).collect(), "上传"),
            PaneSide::Remote => drag_files(cell, paths.collect::<Vec<String>>(), "下载"),
        }
    }

    /// A name cell's clicks: select (⌘ adds or removes, Shift extends), open
    /// on a double click, and select before the context menu so it acts on
    /// what is highlighted.
    fn name_events(&self, cell: Stateful<Div>, name: &str) -> Stateful<Div> {
        let geometry = self.context.geometry.clone();
        let cell = cell.on_any_mouse_down(move |_, _, _| geometry.pressed.set(Pressed::Name));
        let (Some(dispatch), Some(pane)) =
            (self.context.dispatch.clone(), self.context.pane.clone())
        else {
            return cell;
        };
        let sid = self.context.explorer;
        let remote = self.side == PaneSide::Remote;
        cell.on_click({
            let name = name.to_string();
            let pane = pane.clone();
            move |event, window, cx| {
                if ends_a_drag(event) {
                    return;
                }
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
            let hit = (name != "..").then(|| name.to_string());
            let menu_hit = self.context.menu_hit.clone();
            move |_, _, cx| {
                if let Some(hit) = &hit {
                    let _ = pane.update(cx, |pane, cx| pane.select_pressed(hit, cx));
                }
                *menu_hit.borrow_mut() = hit.clone().map(MenuHit::Item);
            }
        })
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
    /// column aligns itself. A right-click on a title is recorded for the
    /// list's menu, which offers the size formats on 大小's, as WinSCP does.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let key = column.key.clone();
        let menu_hit = self.context.menu_hit.clone();
        let geometry = self.context.geometry.clone();
        h_flex()
            .id(ElementId::Name(format!("column:{key}").into()))
            .test_support()
            .size_full()
            .when(key.as_ref() == "size", |this| this.justify_end())
            .when(key.as_ref() == "name", |this| this.px_1p5())
            .on_mouse_down(MouseButton::Right, move |_, _, _| {
                *menu_hit.borrow_mut() = Some(MenuHit::Column(key.clone()));
            })
            .on_any_mouse_down(move |_, _, _| geometry.pressed.set(Pressed::Title))
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
            // As in WinSCP without full row select, the name cell is the item:
            // it shows the hover, the selection and the cursor, and takes the
            // clicks; the rest of the row is empty space. Only the icon and
            // text drag files: a drag from the rest of the cell draws a
            // selection rectangle.
            "name" => {
                let selection = &self.context.selection;
                let selected = selection.contains(&entry.name);
                // A selected name shows as selected whether it holds the
                // cursor or not; the frame only finds the cursor on a name
                // that is not selected: `..`, or one just taken out with ⌘.
                let cursor = !selected && selection.cursor() == Some(entry.name.as_ref());
                let label = h_flex()
                    .id(ElementId::Name(format!("name:{}", entry.name).into()))
                    .h_full()
                    .min_w_0()
                    .gap_2()
                    .child(
                        icon_for(entry)
                            .small()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(div().min_w_0().truncate().child(entry.name.clone()));
                let label = if parent {
                    label
                } else {
                    self.drag_from(label, &entry.name)
                };
                let cell = h_flex()
                    .id(ElementId::Name(format!("name-cell:{}", entry.name).into()))
                    .size_full()
                    .px_1p5()
                    .border_1()
                    .border_color(if cursor {
                        cx.theme().table_active_border
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .map(|this| {
                        if selected {
                            this.bg(cx.theme().tokens.table_active)
                        } else {
                            this.hover(|this| this.bg(cx.theme().tokens.table_hover))
                        }
                    })
                    .child(label.test_support().aria_selected(selected));
                self.name_events(cell, &entry.name)
                    .test_support()
                    .into_any_element()
            }
            "size" => {
                let size = (!entry.is_dir()).then(|| {
                    cx.try_global::<FileSizeFormat>()
                        .copied()
                        .unwrap_or_default()
                        .format(entry.size)
                });
                h_flex()
                    .id(ElementId::Name(format!("size:{}", entry.name).into()))
                    .test_support()
                    .aria_label(size.clone().unwrap_or_default())
                    .w_full()
                    .justify_end()
                    .children(size)
                    .into_any_element()
            }
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
        // The hover, the selection and the cursor show on the name cell, see
        // `render_td`. `DataTable` paints its hover over the whole row after
        // this row's own style, so the row covers it with the table's own
        // (opaque) background, under the cells.
        let row = div()
            .id(ElementId::Name(format!("file:{name}").into()))
            .child(div().absolute().inset_0().bg(cx.theme().tokens.table))
            // Where this row paints tells where every row is. Offsets are
            // explicit: an absolute child without them lands below the cells.
            .child({
                let geometry = context.geometry.clone();
                canvas(
                    move |bounds, _, _| {
                        let first = bounds.origin.y - bounds.size.height * row_ix as f32;
                        geometry.rows.set(Some((first, bounds.size.height)));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full()
            });
        let Some(dispatch) = context.dispatch.clone() else {
            return row;
        };
        if entry.is_parent() || !entry.is_dir() {
            return row;
        }
        // Dropping on a directory row puts the items in that directory.
        accept_drops(
            row,
            self.side,
            context.transfer_enabled,
            dispatch,
            context.explorer,
            self.child_path(&name),
        )
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
        let text = self.context.placeholder.clone();
        div()
            .id("list-placeholder")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text.clone())
            .test_support()
            .aria_label(text)
    }
}

/// The name column paints its selection across the whole cell, padding
/// included, so it has none of the table's: the cell and the title pad
/// themselves as the table would (`px_1p5`, the small table's 6 px).
fn name_column(width: Pixels) -> Column {
    Column::new("name", "名称")
        .width(width)
        .paddings(Edges::all(px(0.)))
        .sortable()
}

/// Rows dragged toward the other pane, as paths on their own side.
#[derive(Clone)]
pub(super) struct FilesDrag<P> {
    pub paths: Vec<P>,
    pub spot: DropSpot,
}
pub(super) type LocalFilesDrag = FilesDrag<PathBuf>;
pub(super) type RemoteFilesDrag = FilesDrag<String>;

/// Let `cell` be dragged to the other pane, carrying `paths`. `verb` is what
/// dropping them there does.
fn drag_files<P: 'static>(cell: Stateful<Div>, paths: Vec<P>, verb: &'static str) -> Stateful<Div> {
    let drag = FilesDrag {
        paths,
        spot: DropSpot::default(),
    };
    cell.on_drag(drag, move |drag, _, _, cx| {
        cx.new(|_| FileDragPreview {
            verb,
            count: drag.paths.len(),
            spot: drag.spot.clone(),
        })
    })
}

/// Show that the other pane's rows can be dropped on `element`, and tell
/// their drag so while the pointer is over it.
pub(super) fn offer_drop<P: 'static>(element: Stateful<Div>) -> Stateful<Div> {
    element
        .drag_over::<FilesDrag<P>>(|style, _, _, cx| style.bg(cx.theme().muted))
        .on_drag_move(|event: &DragMoveEvent<FilesDrag<P>>, _, cx| {
            event
                .drag(cx)
                .spot
                .offer(event.event.position, event.bounds)
        })
}

/// Let `element` take what is dropped on it for the directory `target` on
/// `side`: files from outside the app and the local pane's rows are uploaded
/// to a remote directory, the remote pane's rows downloaded to a local one.
/// A drop does nothing while transfers are not `enabled`.
pub(super) fn accept_drops(
    element: Stateful<Div>,
    side: PaneSide,
    enabled: bool,
    dispatch: FocusHandle,
    explorer: ExplorerId,
    target: String,
) -> Stateful<Div> {
    let send = move |command: ExplorerCommand, window: &mut Window, cx: &mut App| {
        if enabled {
            dispatch.dispatch_explorer_action(&ExplorerAction::new(explorer, command), window, cx);
            cx.stop_propagation();
        }
    };
    match side {
        PaneSide::Remote => element
            .on_drop({
                let (send, target) = (send.clone(), target.clone());
                move |paths: &ExternalPaths, window, cx| {
                    let (paths, target) = (paths.paths().to_vec(), target.clone());
                    send(ExplorerCommand::UploadPaths { paths, target }, window, cx)
                }
            })
            .on_drop(move |drag: &LocalFilesDrag, window, cx| {
                let (paths, target) = (drag.paths.clone(), target.clone());
                send(ExplorerCommand::UploadPaths { paths, target }, window, cx)
            }),
        PaneSide::Local => element.on_drop(move |drag: &RemoteFilesDrag, window, cx| {
            let (paths, target) = (drag.paths.clone(), target.clone());
            send(ExplorerCommand::DownloadPaths { paths, target }, window, cx)
        }),
    }
}

/// Where the pointer last was over a pane that takes a file drag, recorded
/// by that pane as the pointer moves. Only there does the drag show what it
/// will do; anywhere else, its own list included, the pointer says no, as in
/// WinSCP. A position rather than a flag, so nothing has to clear it: GPUI
/// tells every pane listening for the drag type about every move, and a pane
/// the pointer is not over leaves it alone; it goes stale as soon as the
/// pointer moves on.
#[derive(Clone, Default)]
pub(super) struct DropSpot(Rc<Cell<Option<Point<Pixels>>>>);

impl DropSpot {
    /// The pointer moved to `position`, over a pane taking the drag whose
    /// list is `bounds`.
    pub fn offer(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) {
        if bounds.contains(&position) {
            self.0.set(Some(position));
        }
    }

    fn is_under_pointer(&self, window: &Window) -> bool {
        self.0.get() == Some(window.mouse_position())
    }
}

struct FileDragPreview {
    verb: &'static str,
    count: usize,
    spot: DropSpot,
}
impl Render for FileDragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let droppable = self.spot.is_under_pointer(window);
        let cursor = if droppable {
            CursorStyle::DragCopy
        } else {
            CursorStyle::OperationNotAllowed
        };
        // The drag is painted after everything else, so its cursor wins.
        let pointer = canvas(
            |_, _, _| {},
            move |_, _, window, _| window.set_window_cursor_style(cursor),
        )
        .absolute()
        .top_0()
        .left_0();
        if !droppable {
            return div().child(pointer).into_any_element();
        }
        let text = format!("{} {} 个项目", self.verb, self.count);
        div()
            .id("file-drag-preview")
            .px_3()
            .py_2()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(pointer)
            .child(text.clone())
            .test_support()
            .aria_label(text)
            .into_any_element()
    }
}
