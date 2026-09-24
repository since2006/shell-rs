use super::{
    ClickMode, CursorMotion, FileEntry, FileListing, LoadIntent, NavigationHistory, Selection,
    file_listing::ListingContext,
    pane_menu::{PaneMenuState, bookmark_menu, directory_menu, item_menu, new_menu},
    path_ancestors,
};
use crate::app::ExplorerDispatch as _;
use crate::{
    app::{
        CatalogIcon, ExplorerAction, ExplorerCommand, ExplorerShortcut, LOCAL_FILE_LIST_CONTEXT,
        REMOTE_FILE_LIST_CONTEXT,
    },
    session::{BookmarkSide, SessionId, SessionStore},
    sftp::{DirectoryListing, SharedLocalDirectoryProvider},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _, DropdownButton},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, DropdownMenu as _},
    searchable_list::{SearchableGroup, SearchableListItem, SearchableVec},
    select::{Select, SelectEvent, SelectState},
    separator::Separator,
    table::{DataTable, TableDelegate as _, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::RefCell, path::PathBuf, rc::Rc};

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
    fn key_context(self) -> &'static str {
        if self == Self::Local {
            LOCAL_FILE_LIST_CONTEXT
        } else {
            REMOTE_FILE_LIST_CONTEXT
        }
    }
    fn bookmark_side(self) -> BookmarkSide {
        BookmarkSide::from_remote(self == Self::Remote)
    }
}

/// One entry of the 目录列表 select: a directory on the way to the current
/// one, or (local only) a well-known place.
#[derive(Clone)]
pub struct PathChoice {
    title: SharedString,
    path: SharedString,
    depth: usize,
    home: bool,
    side: PaneSide,
}
impl SearchableListItem for PathChoice {
    type Value = SharedString;
    fn title(&self) -> SharedString {
        self.title.clone()
    }
    fn value(&self) -> &SharedString {
        &self.path
    }
    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let icon = if self.home {
            Icon::new(CatalogIcon::House)
        } else {
            Icon::new(IconName::Folder)
        };
        h_flex()
            .gap_2()
            .pl(rems(0.75 * self.depth as f32))
            .child(icon.small().text_color(cx.theme().muted_foreground))
            .child(self.title.clone())
    }
    /// The trigger names the side, since the panes carry no other label.
    fn display_title(&self) -> Option<AnyElement> {
        let icon = if self.side == PaneSide::Remote {
            Icon::new(CatalogIcon::Server)
        } else {
            Icon::new(IconName::HardDrive)
        };
        Some(
            h_flex()
                .gap_2()
                .min_w_0()
                .child(icon.small())
                .child(div().min_w_0().text_ellipsis().child(self.title.clone()))
                .into_any_element(),
        )
    }
}
type PathChoices = SearchableVec<SearchableGroup<PathChoice>>;

pub struct FilePane {
    side: PaneSide,
    session_id: SessionId,
    path: String,
    home: String,
    /// Well-known local places for the 目录列表 select; empty for remote.
    places: Vec<(SharedString, String)>,
    table: Entity<TableState<FileListing>>,
    path_input: Entity<InputState>,
    path_select: Entity<SelectState<PathChoices>>,
    selection: Selection,
    menu_hit: Rc<RefCell<Option<String>>>,
    history: NavigationHistory,
    /// The load in flight, why it started, and where it started from.
    pending: Option<(u64, LoadIntent, String)>,
    /// A row to select once the next listing arrives (after create/rename).
    select_after_load: Option<String>,
    request_id: u64,
    loading: bool,
    error: Option<String>,
    connected: bool,
    transfer_enabled: bool,
    /// A file operation on this pane is running.
    busy: bool,
    store: Entity<SessionStore>,
    dispatch: FocusHandle,
    _subscriptions: Vec<Subscription>,
    load_task: Option<Task<()>>,
}
impl FilePane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        side: PaneSide,
        session_id: SessionId,
        home: String,
        places: Vec<(SharedString, String)>,
        store: Entity<SessionStore>,
        dispatch: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let listing = FileListing::new(side, Vec::new());
        // The pane owns selection (multi-select by name); the table's single
        // row selection stays off so it never paints a competing highlight.
        let table = cx.new(|cx| {
            TableState::new(listing, window, cx)
                .row_selectable(false)
                .col_selectable(false)
                .cell_selectable(false)
        });
        let path_input = cx.new(|cx| InputState::new(window, cx).default_value(&home));
        let path_select = cx.new(|cx| SelectState::new(PathChoices::new(vec![]), None, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(
                &path_input,
                window,
                |this, state, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let path = state.read(cx).value().to_string();
                        let remote = this.is_remote();
                        this.dispatch_command(
                            ExplorerCommand::Navigate { remote, path },
                            window,
                            cx,
                        );
                    }
                },
            ),
            cx.subscribe_in(
                &path_select,
                window,
                |this, _, event: &SelectEvent<PathChoices>, window, cx| {
                    let SelectEvent::Confirm(Some(path)) = event else {
                        return;
                    };
                    if path.as_ref() != this.path {
                        let path = path.to_string();
                        let remote = this.is_remote();
                        this.dispatch_command(
                            ExplorerCommand::Navigate { remote, path },
                            window,
                            cx,
                        );
                    }
                },
            ),
            cx.observe(&store, |_, _, cx| cx.notify()),
        ];
        let mut pane = Self {
            side,
            session_id,
            path: home.clone(),
            home,
            places,
            table,
            path_input,
            path_select,
            selection: Selection::default(),
            menu_hit: Rc::default(),
            history: NavigationHistory::default(),
            pending: None,
            select_after_load: None,
            request_id: 0,
            loading: false,
            error: None,
            connected: side == PaneSide::Local,
            transfer_enabled: false,
            busy: false,
            store,
            dispatch,
            _subscriptions: subscriptions,
            load_task: None,
        };
        pane.sync_listing(cx);
        pane
    }
    pub fn side(&self) -> PaneSide {
        self.side
    }
    fn is_remote(&self) -> bool {
        self.side == PaneSide::Remote
    }
    pub fn path(&self) -> String {
        self.path.clone()
    }
    pub fn home(&self) -> String {
        self.home.clone()
    }
    pub fn is_connected(&self) -> bool {
        self.connected
    }
    fn dispatch_command(&self, command: ExplorerCommand, window: &mut Window, cx: &mut App) {
        self.dispatch.dispatch_explorer_action(
            &ExplorerAction::new(self.session_id, command),
            window,
            cx,
        );
    }
    /// The selected names in display order: what file commands act on.
    pub fn selected_names(&self, cx: &App) -> Vec<String> {
        let table = self.table.read(cx);
        self.selection.targets(&table.delegate().order())
    }
    pub fn selected_entries(&self, cx: &App) -> Vec<FileEntry> {
        let table = self.table.read(cx);
        let listing = table.delegate();
        self.selection
            .targets(&listing.order())
            .iter()
            .filter_map(|name| listing.entry(listing.position(name)?).cloned())
            .collect()
    }
    /// The row under the keyboard cursor.
    pub fn cursor_entry(&self, cx: &App) -> Option<FileEntry> {
        let cursor = self.selection.cursor()?;
        let table = self.table.read(cx);
        let listing = table.delegate();
        listing.entry(listing.position(cursor)?).cloned()
    }
    pub fn entries(&self, cx: &App) -> Vec<FileEntry> {
        self.table.read(cx).delegate().rows().to_vec()
    }
    pub fn column_names(&self, cx: &App) -> Vec<SharedString> {
        let table = self.table.read(cx);
        let listing = table.delegate();
        (0..listing.columns_count(cx))
            .map(|ix| listing.column(ix, cx).name)
            .collect()
    }
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.table.read(cx).focus_handle(cx)
    }
    pub fn menu_hit(&self) -> Option<String> {
        self.menu_hit.borrow().clone()
    }
    pub fn bookmarks(&self, cx: &App) -> Vec<String> {
        self.store
            .read(cx)
            .bookmarks(self.session_id, self.side.bookmark_side())
            .to_vec()
    }
    pub fn back_target(&self) -> Option<String> {
        self.history.back_target().map(str::to_string)
    }
    pub fn forward_target(&self) -> Option<String> {
        self.history.forward_target().map(str::to_string)
    }
    pub fn child_path_of(&self, name: &str) -> String {
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
    /// `/` on the server; the drive or volume root of the current path locally.
    pub fn root_path(&self) -> String {
        if self.side == PaneSide::Local {
            std::path::Path::new(&self.path)
                .ancestors()
                .last()
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "/".into())
        } else {
            "/".into()
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
            self.child_path_of(path)
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
    pub fn set_available(&mut self, connected: bool, transfer: bool, cx: &mut Context<Self>) {
        self.connected = connected;
        self.transfer_enabled = transfer;
        self.sync_listing(cx);
        cx.notify();
    }
    pub fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.busy = busy;
        cx.notify();
    }
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    /// Select this row once the next listing arrives.
    pub fn select_after_load(&mut self, name: String) {
        self.select_after_load = Some(name);
    }
    fn sync_listing(&mut self, cx: &mut Context<Self>) {
        let context = ListingContext {
            session: self.session_id,
            path: self.path.clone(),
            selection: Rc::new(self.selection.clone()),
            transfer_enabled: self.transfer_enabled,
            dispatch: Some(self.dispatch.clone()),
            pane: Some(cx.entity().downgrade()),
            menu_hit: self.menu_hit.clone(),
        };
        self.table.update(cx, |table, cx| {
            table.delegate_mut().configure(context);
            cx.notify();
        });
    }
    fn update_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Selection, &[&str]),
    ) {
        let order: Vec<String> = self
            .table
            .read(cx)
            .delegate()
            .order()
            .into_iter()
            .map(str::to_string)
            .collect();
        let order: Vec<&str> = order.iter().map(String::as_str).collect();
        change(&mut self.selection, &order);
        self.sync_listing(cx);
        cx.notify();
    }
    /// A pointer click on a row, with the modifiers already resolved.
    pub fn click_row(&mut self, name: &str, mode: ClickMode, cx: &mut Context<Self>) {
        self.clear_menu_row(cx);
        self.update_selection(cx, |selection, order| selection.click(name, mode, order));
    }
    /// Right-clicking an unselected row selects it first, as in WinSCP, so the
    /// context menu acts on what is highlighted.
    pub fn select_for_menu(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.selection.contains(name) {
            self.update_selection(cx, |selection, order| {
                selection.click(name, ClickMode::Replace, order)
            });
        }
    }
    /// The table outlines the right-clicked row until something clears it;
    /// selection is ours, so the next click or move does.
    fn clear_menu_row(&mut self, cx: &mut Context<Self>) {
        self.table
            .update(cx, |table, cx| table.set_right_clicked_row(None, cx));
    }
    pub fn select_only(&mut self, name: &str, cx: &mut Context<Self>) {
        self.update_selection(cx, |selection, order| selection.select_only(name, order));
        self.scroll_to_cursor(cx);
    }
    pub fn move_cursor(&mut self, motion: CursorMotion, extend: bool, cx: &mut Context<Self>) {
        self.clear_menu_row(cx);
        let page = self.table.read(cx).visible_range().rows().len();
        self.update_selection(cx, |selection, order| {
            selection.move_cursor(motion, page, extend, order);
        });
        self.scroll_to_cursor(cx);
    }
    /// Scroll only when the cursor row leaves the visible rows; the table's
    /// own `scroll_to_row` always puts the row at the top.
    fn scroll_to_cursor(&mut self, cx: &mut Context<Self>) {
        let Some(cursor) = self.selection.cursor().map(str::to_string) else {
            return;
        };
        self.table.update(cx, |table, cx| {
            let Some(ix) = table.delegate().position(&cursor) else {
                return;
            };
            let visible = table.visible_range().rows().clone();
            if visible.is_empty() || ix < visible.start {
                table.scroll_to_row(ix, cx);
            } else if ix + 1 >= visible.end {
                table.scroll_to_row(ix + 2 - visible.len().max(1), cx);
            }
        });
    }
    pub fn toggle_selection(&mut self, cx: &mut Context<Self>) {
        self.update_selection(cx, |selection, order| selection.toggle_cursor(order));
    }
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.update_selection(cx, |selection, order| selection.select_all(order));
    }
    pub fn upload_sources(&self, cx: &App) -> Vec<PathBuf> {
        self.selected_names(cx)
            .into_iter()
            .map(|name| PathBuf::from(&self.path).join(name))
            .collect()
    }
    pub fn disconnected(&mut self, message: String, cx: &mut Context<Self>) {
        self.request_id += 1;
        self.pending = None;
        self.loading = false;
        self.error = Some(message);
        cx.notify();
    }
    pub fn begin_load(&mut self, intent: LoadIntent, cx: &mut Context<Self>) -> u64 {
        self.request_id += 1;
        self.pending = Some((self.request_id, intent, self.path.clone()));
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
        let pending = self.pending.take();
        match result {
            Ok(listing) => {
                if let Some((_, intent, from)) = pending {
                    self.history.record(intent, &from, listing.path());
                }
                if listing.path() != self.path {
                    self.selection.clear();
                }
                self.path = listing.path().into();
                self.error = None;
                let mut rows: Vec<_> = listing
                    .entries()
                    .iter()
                    .map(FileEntry::from_directory_entry)
                    .collect();
                if self.parent_path() != self.path {
                    rows.insert(0, FileEntry::parent());
                }
                self.table.update(cx, |table, cx| {
                    table.delegate_mut().set_rows(rows);
                    table.refresh(cx);
                });
                self.update_selection(cx, |selection, order| selection.retain(order));
                if let Some(name) = self.select_after_load.take() {
                    self.select_only(&name, cx);
                }
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
                self.sync_path_select(window, cx);
            }
            Err(error) => {
                self.select_after_load = None;
                self.error = Some(error);
            }
        }
        cx.notify();
    }
    /// Rebuild the 目录列表 select: the path from the root to here, then the
    /// local well-known places.
    fn sync_path_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let side = self.side;
        let chain = path_ancestors(&self.path, side == PaneSide::Remote);
        let mut groups = vec![
            SearchableGroup::new("当前路径").items(chain.into_iter().enumerate().map(
                |(depth, (title, path))| PathChoice {
                    title: title.into(),
                    path: path.into(),
                    depth,
                    home: false,
                    side,
                },
            )),
        ];
        if !self.places.is_empty() {
            groups.push(
                SearchableGroup::new("位置").items(self.places.iter().enumerate().map(
                    |(ix, (title, path))| PathChoice {
                        title: title.clone(),
                        path: path.clone().into(),
                        depth: 0,
                        home: ix == 0,
                        side,
                    },
                )),
            );
        }
        let current: SharedString = self.path.clone().into();
        self.path_select.update(cx, |select, cx| {
            select.set_items(PathChoices::new(groups), window, cx);
            select.set_selected_value(&current, window, cx);
        });
    }
    pub fn load_local(
        &mut self,
        path: String,
        intent: LoadIntent,
        provider: SharedLocalDirectoryProvider,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.begin_load(intent, cx);
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
    /// What the menus show, read when a menu opens.
    fn menu_state(&self, cx: &App) -> PaneMenuState {
        let targets = self.selected_names(cx);
        let entries = self.selected_entries(cx);
        PaneMenuState {
            remote: self.is_remote(),
            session: self.session_id,
            opens_directory: entries.len() == 1 && entries[0].is_dir(),
            targets,
            can_go_up: self.parent_path() != self.path,
            can_go_home: !self.home.is_empty() && self.home != self.path,
            can_go_back: self.history.back_target().is_some(),
            can_go_forward: self.history.forward_target().is_some(),
            path: self.path.clone(),
            bookmarks: self.bookmarks(cx),
            can_modify: self.connected && !self.busy,
            can_transfer: self.transfer_enabled,
        }
    }
}

impl Render for FilePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let remote = self.is_remote();
        let sid = self.session_id;
        let context = self.side.key_context();
        let state = self.menu_state(cx);
        let selected = state.targets.len();
        let navigable = self.connected;
        // A toolbar button dispatches the same command as its key binding,
        // and its tooltip shows that binding.
        let tool = |id: &'static str, icon: Icon, tip: &'static str, command: ExplorerCommand| {
            let dispatch = self.dispatch.clone();
            let shortcut = ExplorerShortcut(command.clone());
            Button::new(id)
                .ghost()
                .small()
                .icon(icon)
                .accessibility_label(tip)
                .tooltip_with_action(tip, &shortcut, Some(context))
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(sid, command.clone()),
                        window,
                        cx,
                    )
                })
        };
        let file_count = self
            .table
            .read(cx)
            .delegate()
            .rows()
            .iter()
            .filter(|e| !e.is_parent())
            .count();
        let toolbar = || {
            h_flex()
                .flex_wrap()
                .gap_1()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(cx.theme().border)
        };
        let navigation = toolbar()
            // `Select` fills its parent (`size_full`), so it needs a sized
            // box of its own or it covers the whole toolbar row.
            .child(
                div().w(rems(10.)).flex_shrink_0().child(
                    Select::new(&self.path_select)
                        .id("path-select")
                        .small()
                        .menu_width(rems(18.))
                        .accessibility_label(format!("{}目录", self.side.label()))
                        .disabled(!navigable),
                ),
            )
            .child({
                let menu = state.clone();
                Button::new("bookmarks")
                    .ghost()
                    .small()
                    .icon(Icon::new(CatalogIcon::Bookmark))
                    .dropdown_caret(true)
                    .accessibility_label("书签")
                    .tooltip("书签")
                    .disabled(!navigable)
                    .dropdown_menu(move |popup, _, _| bookmark_menu(popup, &menu))
            })
            .child(Separator::vertical())
            .child(
                tool(
                    "up",
                    Icon::new(CatalogIcon::FolderUp),
                    "上级目录",
                    ExplorerCommand::Up { remote },
                )
                .disabled(!navigable || !state.can_go_up),
            )
            .child(
                tool(
                    "root",
                    Icon::new(CatalogIcon::FolderRoot),
                    "根目录",
                    ExplorerCommand::Root { remote },
                )
                .disabled(!navigable || !state.can_go_up),
            )
            .child(
                tool(
                    "home",
                    Icon::new(CatalogIcon::House),
                    "主目录",
                    ExplorerCommand::Home { remote },
                )
                .disabled(!navigable || !state.can_go_home),
            )
            .child(
                tool(
                    "refresh",
                    Icon::new(CatalogIcon::RefreshCw),
                    "刷新",
                    ExplorerCommand::Refresh { remote },
                )
                .disabled(!navigable),
            )
            .child(Separator::vertical())
            .child(
                tool(
                    "back",
                    Icon::new(IconName::ArrowLeft),
                    "后退",
                    ExplorerCommand::Back { remote },
                )
                .disabled(!navigable || !state.can_go_back),
            )
            .child(
                tool(
                    "forward",
                    Icon::new(IconName::ArrowRight),
                    "前进",
                    ExplorerCommand::Forward { remote },
                )
                .disabled(!navigable || !state.can_go_forward),
            );
        let transfer_shortcut = ExplorerShortcut(ExplorerCommand::Transfer { remote });
        let transfer = if remote {
            let dispatch = self.dispatch.clone();
            Button::new("download")
                .ghost()
                .small()
                .icon(Icon::new(CatalogIcon::Download))
                .label("下载…")
                .tooltip_with_action("下载所选项目", &transfer_shortcut, Some(context))
                .disabled(selected == 0 || !state.can_transfer)
                .on_click(move |_, window, cx| {
                    dispatch.dispatch_explorer_action(
                        &ExplorerAction::new(sid, ExplorerCommand::Transfer { remote }),
                        window,
                        cx,
                    )
                })
                .into_any_element()
        } else {
            let dispatch = self.dispatch.clone();
            DropdownButton::new("upload-menu")
                .ghost()
                .small()
                .button(
                    Button::new("upload")
                        .icon(Icon::new(CatalogIcon::Upload))
                        .label("上传…")
                        .tooltip_with_action("上传所选项目", &transfer_shortcut, Some(context))
                        .disabled(selected == 0 || !state.can_transfer)
                        .on_click(move |_, window, cx| {
                            dispatch.dispatch_explorer_action(
                                &ExplorerAction::new(sid, ExplorerCommand::Transfer { remote }),
                                window,
                                cx,
                            )
                        }),
                )
                .disabled(!state.can_transfer)
                .dropdown_menu(move |menu, _, _| {
                    menu.menu(
                        "选择文件上传…",
                        Box::new(ExplorerAction::new(sid, ExplorerCommand::ChooseFiles)),
                    )
                })
                .into_any_element()
        };
        let operations = toolbar()
            .child(transfer)
            .child(Separator::vertical())
            .child(
                tool(
                    "delete",
                    Icon::new(CatalogIcon::Trash),
                    "删除",
                    ExplorerCommand::Delete { remote },
                )
                .disabled(selected == 0 || !state.can_modify),
            )
            .child(
                tool(
                    "rename",
                    Icon::new(CatalogIcon::SquarePen),
                    "重命名…",
                    ExplorerCommand::Rename { remote },
                )
                .disabled(selected != 1 || !state.can_modify),
            )
            .child(
                tool(
                    "properties",
                    Icon::new(IconName::Info),
                    "属性…",
                    ExplorerCommand::Properties { remote },
                )
                .disabled(selected == 0 || !state.can_modify),
            )
            .child(Separator::vertical())
            .child({
                let menu = state.clone();
                Button::new("new")
                    .ghost()
                    .small()
                    .icon(Icon::new(CatalogIcon::FolderPlus))
                    .label("新建")
                    .dropdown_caret(true)
                    .disabled(!state.can_modify)
                    .dropdown_menu(move |popup, _, _| new_menu(popup, &menu))
            });
        let pane = cx.entity().downgrade();
        let menu_hit = self.menu_hit.clone();
        v_flex()
            .id((self.side.pane_id(), sid.0))
            .test_support()
            .size_full()
            .min_w_0()
            .child(navigation)
            .child(operations)
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Input::new(&self.path_input)
                            .id(self.side.path_id())
                            .small()
                            .flex_1()
                            .min_w_0(),
                    ),
            )
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
                    .id(("file-list", u64::from(remote)))
                    .flex_1()
                    .min_h_0()
                    .key_context(context)
                    // The row's right-click handler records the hit after
                    // this capture-phase reset, so empty space finds none.
                    .capture_any_mouse_down({
                        let menu_hit = menu_hit.clone();
                        move |event, _, _| {
                            if event.button == MouseButton::Right {
                                *menu_hit.borrow_mut() = None;
                            }
                        }
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
                    .when(!remote, |this| {
                        let dispatch = self.dispatch.clone();
                        let path = self.path.clone();
                        let enabled = self.transfer_enabled;
                        this.drag_over::<super::file_listing::RemoteFilesDrag>(|style, _, _, cx| {
                            style.bg(cx.theme().muted)
                        })
                        .on_drop(
                            move |drag: &super::file_listing::RemoteFilesDrag, window, cx| {
                                if enabled {
                                    dispatch.dispatch_explorer_action(
                                        &ExplorerAction::new(
                                            sid,
                                            ExplorerCommand::DownloadPaths {
                                                paths: drag.paths.clone(),
                                                target: path.clone(),
                                            },
                                        ),
                                        window,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }
                            },
                        )
                    })
                    // One menu for rows and empty space, on the container:
                    // see `SessionPanel` for why menus stay off virtual rows.
                    .context_menu(move |menu, window, cx| {
                        let Some(state) = pane.upgrade().map(|pane| pane.read(cx).menu_state(cx))
                        else {
                            return menu;
                        };
                        if menu_hit.borrow().is_some() {
                            item_menu(menu, &state)
                        } else {
                            directory_menu(menu, &state, window, cx)
                        }
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
                    .child(if selected == 0 {
                        format!("{file_count} 个项目")
                    } else {
                        format!("{file_count} 个项目 · 已选择 {selected} 项")
                    }),
            )
    }
}
