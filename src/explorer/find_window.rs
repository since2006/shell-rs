//! 查找文件, after WinSCP's Find Files, in a window of its own beside the
//! main one: the files can be browsed while a search runs, and its results
//! gone to one after another. A file name and a folder on the server; every
//! folder below is read in turn, and what matches is listed with the folder
//! it is in. Each SFTP tab has at most one, which closes with the tab.
//!
//! What it does to the files goes to the main window's workspace as the
//! tab's commands, as a button there would send them.

use super::{
    ClickMode, CursorMotion, ExplorerPanel, FileIcon, FileIconStyle, FileSizeFormat, PaneSide,
    Selection, ShowHiddenFiles, expand_path,
    find::{FileMask, FileSearch, FoundColumn, FoundFile, SearchEnd, sort_found},
    format_changed,
    marquee::{self, ListGeometry, ListMarquee, MarqueeList, Pressed, ends_a_drag},
};
use crate::{
    app::{
        CatalogIcon, CloseFindFiles, CopyFindResults, ExplorerCommand, ExplorerShortcut,
        FIND_FILES_CONTEXT,
    },
    host::ConnectionState,
    i18n::{UiLocale, t, tn},
    sftp::{DirectoryListing, RemotePath, SftpCommand, describe_sftp_error},
    shared::{dismiss_form_error, form_error_notification},
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Root, Sizable as _, Theme, TitleBar,
    WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    dialog::DialogButtonProps,
    form::{Field, Form},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenu},
    notification::Notification,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::RefCell, rc::Rc};

/// The search's listings have request ids from here up, far above the
/// remote pane's own, so the tab tells their answers apart.
pub(super) const FIND_REQUESTS: u64 = 1 << 62;

/// Notifications go above a dialog, as in the main window, without covering
/// an open menu.
const NOTIFICATION_PRIORITY: usize = gpui_kit::base::POPUP_PRIORITY - 1;

/// An SFTP tab's 查找文件 window, while it is open.
pub(super) struct FindWindow {
    window: WindowHandle<Root>,
    view: WeakEntity<FindFiles>,
}

/// What a new window starts from.
struct FindStart {
    explorer: WeakEntity<ExplorerPanel>,
    main: AnyWindowHandle,
    host: SharedString,
    commands: async_channel::Sender<SftpCommand>,
    mask: String,
    current: String,
    home: String,
}

/// The window's content: the two fields, the results and what to do with
/// them.
pub(super) struct FindFiles {
    /// The SFTP tab, and the window its workspace is in, where commands go.
    explorer: WeakEntity<ExplorerPanel>,
    main: AnyWindowHandle,
    /// This window, where what went wrong deleting is said.
    window: AnyWindowHandle,
    host: SharedString,
    focus_handle: FocusHandle,
    mask: Entity<InputState>,
    folder: Entity<InputState>,
    table: Entity<TableState<FoundFiles>>,
    /// Where the results are, and the selection rectangle.
    marquee: ListMarquee,
    /// The result a right-click landed on, by path; none on empty space.
    menu_hit: Rc<RefCell<Option<String>>>,
    search: Option<FileSearch>,
    next_request: u64,
    commands: async_channel::Sender<SftpCommand>,
    /// The remote pane's folder and home when the window was opened or
    /// brought forward: a relative folder and `~` mean what they do there.
    current: String,
    home: String,
    _subscriptions: Vec<Subscription>,
}

impl FindFiles {
    fn new(start: FindStart, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mask = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("explorer.find.mask_placeholder"))
                .default_value(&start.mask)
        });
        let folder = cx.new(|cx| InputState::new(window, cx).default_value(&start.current));
        // The selection is the list's own, by path, as in the main window's
        // lists; the table's single row selection stays off.
        let view = cx.weak_entity();
        let marquee = ListMarquee::default();
        let geometry = marquee.geometry.clone();
        let menu_hit = Rc::<RefCell<Option<String>>>::default();
        let table = cx.new(|cx| {
            TableState::new(
                FoundFiles::new(view, geometry, menu_hit.clone()),
                window,
                cx,
            )
            .col_movable(false)
            .row_selectable(false)
            .col_selectable(false)
            .cell_selectable(false)
        });
        // Return in either field starts a search, or stops the one running.
        let press_enter = |this: &mut Self,
                           _: &Entity<InputState>,
                           event: &InputEvent,
                           window: &mut Window,
                           cx: &mut Context<Self>| {
            if let InputEvent::PressEnter { .. } = event {
                this.start_or_stop(window, cx);
            }
        };
        let refresh = |_: &mut Self, window: &mut Window, _: &mut Context<Self>| window.refresh();
        let subscriptions = vec![
            cx.subscribe_in(&mask, window, press_enter),
            cx.subscribe_in(&folder, window, press_enter),
            // The buttons follow the selection.
            cx.observe(&table, |_, _, cx| cx.notify()),
            // Widths dragged stay as the results come and go.
            cx.subscribe(&table, |_, table, event: &TableEvent, cx| {
                if let TableEvent::ColumnWidthsChanged(widths) = event {
                    table.update(cx, |table, _| table.delegate_mut().set_widths(widths));
                }
            }),
            // The settings are applied to the main window, which redraws;
            // this one redraws itself.
            cx.observe_global_in::<Theme>(window, refresh),
            cx.observe_global_in::<FileIconStyle>(window, refresh),
            cx.observe_global_in::<FileSizeFormat>(window, refresh),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                window.set_window_title(&this.title());
                this.mask.update(cx, |mask, cx| {
                    mask.set_placeholder(t!("explorer.find.mask_placeholder"), window, cx)
                });
                this.table.update(cx, |table, cx| {
                    table.delegate_mut().retitle_columns();
                    table.refresh(cx);
                });
            }),
        ];
        Self {
            explorer: start.explorer,
            main: start.main,
            window: window.window_handle(),
            host: start.host,
            focus_handle: cx.focus_handle(),
            mask,
            folder,
            table,
            marquee,
            menu_hit,
            search: None,
            next_request: FIND_REQUESTS,
            commands: start.commands,
            current: start.current,
            home: start.home,
            _subscriptions: subscriptions,
        }
    }

    fn title(&self) -> SharedString {
        t!("explorer.find.window_title", host = self.host)
    }

    /// 查找文件 again while the window is open: it comes forward, on the
    /// folder the remote pane shows now unless a search is running.
    fn reopen(
        &mut self,
        current: String,
        home: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_running() {
            self.folder
                .update(cx, |folder, cx| folder.set_value(&current, window, cx));
            self.current = current;
            self.home = home;
        }
        self.focus_mask(window, cx);
    }

    /// The name is ready to be typed over, as in WinSCP.
    fn focus_mask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mask.update(cx, |mask, cx| {
            mask.select_all(window, cx);
            mask.focus(window, cx);
        });
    }

    fn is_running(&self) -> bool {
        self.search.as_ref().is_some_and(FileSearch::is_running)
    }

    /// 查找, or 停止 while a search runs.
    fn start_or_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running() {
            self.stop(cx);
        } else {
            self.start(window, cx);
        }
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.mask.read(cx).value().to_string();
        let Some(mask) = FileMask::parse(&text) else {
            window.push_notification(
                form_error_notification(t!("explorer.find.mask_missing")),
                cx,
            );
            return;
        };
        let folder = match self.folder.read(cx).value().trim() {
            "" => {
                window.push_notification(
                    form_error_notification(t!("explorer.find.folder_missing")),
                    cx,
                );
                return;
            }
            text => expand_path(text, &self.current, &self.home, true),
        };
        dismiss_form_error(window, cx);
        // The next window of this tab starts with it.
        let _ = self
            .explorer
            .update(cx, |explorer, _| explorer.find_mask = text);
        let show_hidden = ShowHiddenFiles::get(PaneSide::Remote, cx);
        self.search = Some(FileSearch::new(folder, mask, show_hidden));
        self.table.update(cx, |table, cx| {
            table.delegate_mut().rows.clear();
            table.clear_selection(cx);
            table.refresh(cx);
        });
        self.read_next(cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(search) = &mut self.search
            && search.is_running()
        {
            search.stop();
            self.sync_rows(cx);
        }
    }

    /// The SFTP connection dropped: nothing asked for will answer.
    fn connection_lost(&mut self, cx: &mut Context<Self>) {
        if let Some(search) = &mut self.search
            && search.is_running()
        {
            search.fail(t!("explorer.connection.lost").into());
            self.sync_rows(cx);
        }
    }

    /// Ask the worker for the folders the search is ready to read.
    fn read_next(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &mut self.search else {
            return;
        };
        let next = &mut self.next_request;
        let reads = search.next_reads(|| {
            *next += 1;
            *next
        });
        let mut refused = Vec::new();
        for (request_id, folder) in reads {
            match RemotePath::new(folder) {
                Ok(path) => {
                    let _ = self
                        .commands
                        .try_send(SftpCommand::List { request_id, path });
                }
                Err(error) => refused.push((request_id, Err(describe_sftp_error(&error)))),
            }
        }
        for (request_id, result) in refused {
            self.listed(request_id, result, cx);
        }
        cx.notify();
    }

    fn listed(
        &mut self,
        request_id: u64,
        result: Result<DirectoryListing, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = &mut self.search else {
            return;
        };
        search.listed(request_id, result);
        self.sync_rows(cx);
        self.read_next(cx);
    }

    /// Show what has been found, sorted as the list is, the selection kept:
    /// whenever a listing adds results, and once the search is over.
    fn sync_rows(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &self.search else {
            return;
        };
        let found = search.found();
        let state = if search.is_running() {
            ListState::Searching
        } else {
            ListState::Done
        };
        self.table.update(cx, |table, cx| {
            let list = table.delegate_mut();
            if list.rows.len() != found.len() || list.state != state {
                list.set_rows(found.to_vec());
            }
            list.state = state;
            table.refresh(cx);
        });
        cx.notify();
    }

    /// The selected results, in the order shown: what 下载… and 删除 act on.
    fn targets(&self, cx: &App) -> Vec<FoundFile> {
        self.table.read(cx).delegate().targets()
    }

    /// The one result selected, for 定位 and 编辑.
    fn single(&self, cx: &App) -> Option<FoundFile> {
        match self.targets(cx).as_slice() {
            [found] => Some(found.clone()),
            _ => None,
        }
    }

    /// Send the tab a command through the main window's workspace. What
    /// shows its result there brings that window forward; this one stays
    /// open behind.
    fn send(
        &self,
        activate: bool,
        command: impl FnOnce(&ExplorerPanel, &App) -> ExplorerCommand,
        cx: &mut App,
    ) {
        let Some(explorer) = self.explorer.upgrade() else {
            return;
        };
        let sender = explorer.read(cx).sender();
        let command = command(explorer.read(cx), cx);
        let _ = self.main.update(cx, |_, window, cx| {
            if activate {
                window.activate_window();
            }
            sender.send(command, window, cx);
        });
    }

    /// 定位: the remote pane goes to the result's folder and selects it.
    fn reveal(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(found) = self.single(cx) {
            self.send(
                true,
                |_, _| ExplorerCommand::Reveal { path: found.path() },
                cx,
            );
        }
    }

    fn edit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(found) = self.single(cx).filter(|found| !found.entry.is_dir()) {
            self.send(
                true,
                |_, _| ExplorerCommand::Edit {
                    remote: true,
                    path: Some(found.path()),
                },
                cx,
            );
        }
    }

    /// 下载…: the usual question, starting from the local pane's folder.
    fn download(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<String> = self.targets(cx).iter().map(FoundFile::path).collect();
        if !paths.is_empty() {
            self.send(
                true,
                |explorer, cx| ExplorerCommand::DownloadPaths {
                    paths,
                    target: explorer.local().read(cx).path(),
                },
                cx,
            );
        }
    }

    /// 删除, asked here as the main window asks: the results deleted leave
    /// the list once the server is done.
    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets = self.targets(cx);
        if targets.is_empty() || window.has_active_dialog(cx) {
            return;
        }
        let Some(explorer) = self.explorer.upgrade() else {
            return;
        };
        // Refused here, not out of sight in the main window.
        let refusal = {
            let explorer = explorer.read(cx);
            if explorer.connection_state() != ConnectionState::Connected {
                Some(t!("explorer.connection.lost"))
            } else if explorer.remote().read(cx).is_busy() {
                Some(t!("explorer.find.busy"))
            } else {
                None
            }
        };
        if let Some(refusal) = refusal {
            window.push_notification(Notification::error(refusal), cx);
            return;
        }
        let title = if let [found] = targets.as_slice() {
            t!("explorer.delete.title_one", name = found.entry.name)
        } else {
            tn!("explorer.delete.title_several", targets.len())
        };
        let paths: Vec<String> = targets.iter().map(FoundFile::path).collect();
        let view = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .description(t!("explorer.delete.remote"))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(t!("common.delete"))
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text(t!("common.cancel")),
                )
                .show_cancel(true)
                .on_ok({
                    let (view, paths) = (view.clone(), paths.clone());
                    move |_, _, cx| {
                        let paths = paths.clone();
                        let _ = view.update(cx, |view, cx| {
                            view.send(false, |_, _| ExplorerCommand::DeletePaths { paths }, cx)
                        });
                        true
                    }
                })
        });
    }

    /// The tab's answer to 删除.
    fn deleted(&mut self, paths: Vec<String>, result: Result<(), String>, cx: &mut Context<Self>) {
        match result {
            Ok(()) => {
                if let Some(search) = &mut self.search {
                    search.forget(&paths);
                    let found = search.found().to_vec();
                    self.table.update(cx, |table, cx| {
                        table.delegate_mut().set_rows(found);
                        table.refresh(cx);
                    });
                }
                cx.notify();
            }
            Err(message) => {
                let _ = self.window.update(cx, |_, window, cx| {
                    window.push_notification(
                        Notification::error(message).title(t!("explorer.operation.delete_failed")),
                        cx,
                    )
                });
            }
        }
    }

    /// The list's keys, the main window's file list's: they come as its
    /// commands, which this window carries out on its results.
    fn on_shortcut(
        &mut self,
        action: &ExplorerShortcut,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &action.0 {
            ExplorerCommand::MoveCursor { motion, extend, .. } => {
                let (motion, extend) = (*motion, *extend);
                self.table.update(cx, |table, cx| {
                    table.set_right_clicked_row(None, cx);
                    let page = table.visible_range().rows().len();
                    if let Some(ix) = table.delegate_mut().move_cursor(motion, page, extend) {
                        table.scroll_to_row(ix, cx);
                    }
                    cx.notify();
                });
            }
            ExplorerCommand::ToggleSelection { .. } => self.table.update(cx, |table, cx| {
                table.delegate_mut().toggle_cursor();
                cx.notify();
            }),
            ExplorerCommand::SelectAll { .. } => self.table.update(cx, |table, cx| {
                table.delegate_mut().select_all();
                cx.notify();
            }),
            ExplorerCommand::Open { .. } => self.reveal(window, cx),
            ExplorerCommand::Edit { .. } => self.edit(window, cx),
            ExplorerCommand::Transfer { .. } => self.download(window, cx),
            ExplorerCommand::Delete { .. } => self.delete(window, cx),
            _ => {}
        }
    }

    /// 复制结果: every result's path, one a line.
    fn copy(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<String> = self
            .table
            .read(cx)
            .delegate()
            .rows
            .iter()
            .map(FoundFile::path)
            .collect();
        if !paths.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(paths.join("\n")));
        }
    }

    /// What the context menu offers, read when it opens.
    fn menu_state(&self, cx: &App) -> ResultsMenu {
        let targets = self.targets(cx);
        ResultsMenu {
            selected: targets.len(),
            edits_file: matches!(targets.as_slice(), [found] if !found.entry.is_dir()),
            has_results: !self.table.read(cx).delegate().rows.is_empty(),
        }
    }

    /// Where the search is, in WinSCP's status bar's words.
    fn status(&self) -> Option<SharedString> {
        let search = self.search.as_ref()?;
        let count = search.found().len();
        Some(match search.end() {
            None => t!("explorer.find.searching", folder = search.current()),
            Some(SearchEnd::Finished) if count == 0 => t!("explorer.find.nothing"),
            Some(SearchEnd::Finished) => tn!("explorer.find.found", count),
            Some(SearchEnd::Stopped) => tn!("explorer.find.stopped", count),
            Some(SearchEnd::Failed(error)) => error.clone().into(),
        })
    }
}

/// What the results' context menu needs to know, read when it opens.
struct ResultsMenu {
    selected: usize,
    /// The one result selected is a file.
    edits_file: bool,
    has_results: bool,
}

impl ResultsMenu {
    /// Right-click on a result: the buttons below the list, for the
    /// selection, with their keys.
    fn result_menu(&self, menu: PopupMenu) -> PopupMenu {
        let remote = true;
        let shortcut = |command| Box::new(ExplorerShortcut(command));
        let menu = menu
            .menu_with_icon_and_disabled(
                t!("explorer.find.reveal"),
                Icon::new(CatalogIcon::LocateFixed),
                shortcut(ExplorerCommand::Open { remote }),
                self.selected != 1,
            )
            .menu_with_icon_and_disabled(
                t!("explorer.command.edit"),
                Icon::new(CatalogIcon::FilePenLine),
                shortcut(ExplorerCommand::Edit { remote, path: None }),
                !self.edits_file,
            )
            .menu_with_icon_and_disabled(
                t!("explorer.command.download"),
                Icon::new(CatalogIcon::Download),
                shortcut(ExplorerCommand::Transfer { remote }),
                self.selected == 0,
            )
            .separator()
            .menu_with_icon_and_disabled(
                t!("common.delete"),
                Icon::new(CatalogIcon::Trash),
                shortcut(ExplorerCommand::Delete { remote }),
                self.selected == 0,
            )
            .separator();
        self.copy_item(menu)
    }

    /// Right-click on empty space, where nothing is selected: what is for
    /// all the results. With none, the menu does not open.
    fn empty_menu(&self, menu: PopupMenu) -> PopupMenu {
        if self.has_results {
            self.copy_item(menu)
        } else {
            menu
        }
    }

    fn copy_item(&self, menu: PopupMenu) -> PopupMenu {
        menu.menu_with_icon_and_disabled(
            t!("explorer.find.copy"),
            Icon::new(IconName::Copy),
            Box::new(CopyFindResults),
            !self.has_results,
        )
    }
}

impl MarqueeList for FindFiles {
    type Rows = FoundFiles;

    fn table(&self) -> &Entity<TableState<FoundFiles>> {
        &self.table
    }

    fn list_marquee(&self) -> &ListMarquee {
        &self.marquee
    }

    fn list_marquee_mut(&mut self) -> &mut ListMarquee {
        &mut self.marquee
    }

    fn selection(&self, cx: &App) -> Selection {
        self.table.read(cx).delegate().selection.clone()
    }

    fn update_selection(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Selection, &[&str]),
    ) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().with_order(change);
            cx.notify();
        });
    }

    /// Any press drops the outline `DataTable` draws around a right-clicked
    /// row: the selection alone shows what the menu acts on.
    fn on_list_press(&mut self, cx: &mut Context<Self>) {
        self.table
            .update(cx, |table, cx| table.set_right_clicked_row(None, cx));
    }
}

impl Render for FindFiles {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.is_running();
        let targets = self.targets(cx);
        let single = match targets.as_slice() {
            [found] => Some(found),
            _ => None,
        };
        let has_results = !self.table.read(cx).delegate().rows.is_empty();
        let unreadable = self.search.as_ref().map_or(0, FileSearch::unreadable);
        // The folder field is as wide as the name's, beside 查找.
        let side = || div().w(rems(5.)).flex_shrink_0();
        let action = |id: &'static str, label: SharedString| Button::new(id).small().label(label);
        // As in the main window: the names are the results, the rest is
        // empty space to click or drag from.
        let results = marquee::select_area(
            div()
                .id("find-results")
                .flex_1()
                .min_h_0()
                .border_1()
                .border_color(cx.theme().border)
                .rounded(cx.theme().radius)
                .overflow_hidden(),
            self,
            cx,
        )
        // A result's right-click records the hit after this capture-phase
        // reset, so empty space finds none.
        .capture_any_mouse_down({
            let menu_hit = self.menu_hit.clone();
            move |event, _, _| {
                if event.button == MouseButton::Right {
                    *menu_hit.borrow_mut() = None;
                }
            }
        })
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                marquee::press_empty(this, event.modifiers, cx)
            }),
        )
        // One menu for results and empty space, on the list, not on its
        // rows: see `FilePane`.
        .context_menu({
            let (view, menu_hit) = (cx.weak_entity(), self.menu_hit.clone());
            move |menu, _, cx| {
                let Some(view) = view.upgrade() else {
                    return menu;
                };
                let state = view.read(cx).menu_state(cx);
                if menu_hit.borrow().is_some() {
                    state.result_menu(menu)
                } else {
                    state.empty_menu(menu)
                }
            }
        })
        .child(
            DataTable::new(&self.table)
                .stripe(false)
                .bordered(false)
                .small(),
        )
        .child(self.marquee.canvas())
        .children(self.marquee.rectangle(cx));
        let theme = cx.theme();
        v_flex()
            .id("find-files")
            .test_support()
            .key_context(FIND_FILES_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &CloseFindFiles, window, _| window.remove_window()))
            .on_action(cx.listener(Self::on_shortcut))
            .on_action(cx.listener(|this, _: &CopyFindResults, window, cx| this.copy(window, cx)))
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(crate::app_lock::activity_tracker(cx))
            .child(
                TitleBar::new().child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(self.title()),
                ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_4()
                    .gap_3()
                    .child(
                        Form::new()
                            .child(
                                Field::new()
                                    .label(t!("explorer.find.mask"))
                                    .description(t!("explorer.find.mask_hint"))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                Input::new(&self.mask)
                                                    .id("find-mask")
                                                    .small()
                                                    .flex_1(),
                                            )
                                            .child(
                                                side().child(
                                                    Button::new("find-start")
                                                        .small()
                                                        .w_full()
                                                        .when(!running, |button| button.primary())
                                                        .label(if running {
                                                            t!("explorer.find.stop")
                                                        } else {
                                                            t!("explorer.find.start")
                                                        })
                                                        .on_click(cx.listener(
                                                            |this, _, window, cx| {
                                                                this.start_or_stop(window, cx)
                                                            },
                                                        )),
                                                ),
                                            ),
                                    ),
                            )
                            .child(
                                Field::new().label(t!("explorer.find.folder")).child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Input::new(&self.folder)
                                                .id("find-folder")
                                                .small()
                                                .flex_1(),
                                        )
                                        .child(side()),
                                ),
                            ),
                    )
                    .child(results)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_2()
                                    .text_sm()
                                    .children(self.status().map(|status| {
                                        div()
                                            .id("find-status")
                                            .test_support()
                                            .aria_label(status.clone())
                                            .min_w_0()
                                            .truncate()
                                            .child(status)
                                    }))
                                    .when(unreadable > 0, |row| {
                                        let skipped = tn!("explorer.find.unreadable", unreadable);
                                        row.child(
                                            div()
                                                .id("find-unreadable")
                                                .test_support()
                                                .aria_label(skipped.clone())
                                                .flex_shrink_0()
                                                .text_color(theme.muted_foreground)
                                                .child(skipped),
                                        )
                                    }),
                            )
                            .child(
                                action("find-reveal", t!("explorer.find.reveal"))
                                    .tooltip(t!("explorer.find.reveal_tip"))
                                    .disabled(single.is_none())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.reveal(window, cx)),
                                    ),
                            )
                            .child(
                                action("find-edit", t!("explorer.command.edit"))
                                    .disabled(!single.is_some_and(|found| !found.entry.is_dir()))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.edit(window, cx)),
                                    ),
                            )
                            .child(
                                action("find-download", t!("explorer.command.download"))
                                    .disabled(targets.is_empty())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.download(window, cx)
                                        }),
                                    ),
                            )
                            .child(
                                action("find-delete", t!("common.delete"))
                                    .disabled(targets.is_empty())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.delete(window, cx)),
                                    ),
                            )
                            .child(
                                action("find-copy", t!("explorer.find.copy"))
                                    .tooltip(t!("explorer.find.copy_tip"))
                                    .disabled(!has_results)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.copy(window, cx)),
                                    ),
                            ),
                    ),
            )
            // 删除's question, and what went wrong, above it as in the main
            // window.
            .children(Root::render_dialog_layer(window, cx))
            .children(
                Root::render_notification_layer(window, cx)
                    .map(|layer| deferred(layer).with_priority(NOTIFICATION_PRIORITY)),
            )
    }
}

/// What the results say while there are none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ListState {
    /// No search yet.
    #[default]
    Ready,
    Searching,
    Done,
}

/// The results for `DataTable`, WinSCP's columns, sorted and selected as the
/// main window's lists are: a click on a title sorts, the name cell is the
/// result (⌘ and Shift clicks select several), the rest of a row is empty
/// space, and the keys are the same.
pub(super) struct FoundFiles {
    columns: Vec<Column>,
    rows: Vec<FoundFile>,
    state: ListState,
    sort: Option<(usize, ColumnSort)>,
    /// By path, so it holds as results come, go and are sorted.
    selection: Selection,
    /// Where the rows are, and what a press landed on, for the rectangle.
    geometry: Rc<ListGeometry>,
    /// The result a right-click landed on, for the menu.
    menu_hit: Rc<RefCell<Option<String>>>,
    view: WeakEntity<FindFiles>,
}

impl FoundFiles {
    fn new(
        view: WeakEntity<FindFiles>,
        geometry: Rc<ListGeometry>,
        menu_hit: Rc<RefCell<Option<String>>>,
    ) -> Self {
        Self {
            columns: columns(),
            rows: Vec::new(),
            state: ListState::Ready,
            sort: None,
            selection: Selection::default(),
            geometry,
            menu_hit,
            view,
        }
    }

    /// Title the columns in the interface language again, widths and
    /// arrows as they are.
    fn retitle_columns(&mut self) {
        for column in &mut self.columns {
            column.name = column_title(&column.key);
        }
    }

    /// Keep the widths the columns were dragged to: the table takes the
    /// columns again on each `refresh`.
    fn set_widths(&mut self, widths: &[Pixels]) {
        for (column, width) in self.columns.iter_mut().zip(widths) {
            column.width = *width;
        }
    }

    /// The results, sorted as the list is; the selection keeps what is
    /// still there.
    fn set_rows(&mut self, rows: Vec<FoundFile>) {
        self.rows = rows;
        self.apply_sort();
        self.with_order(|selection, order| selection.retain(order));
    }

    /// A click on a title sorts as one on its arrow does: descending, then
    /// ascending, then back to by folder.
    fn cycle_sort(&mut self, col_ix: usize) {
        let sort = match self.sort {
            Some((ix, ColumnSort::Descending)) if ix == col_ix => ColumnSort::Ascending,
            Some((ix, ColumnSort::Ascending)) if ix == col_ix => ColumnSort::Default,
            _ => ColumnSort::Descending,
        };
        self.set_sort(col_ix, sort);
    }

    /// Sort by a column, marking it on the columns too, which the table
    /// takes again on each `refresh`.
    fn set_sort(&mut self, col_ix: usize, sort: ColumnSort) {
        self.sort = match sort {
            ColumnSort::Default => None,
            sort => Some((col_ix, sort)),
        };
        for (ix, column) in self.columns.iter_mut().enumerate() {
            column.sort = Some(if ix == col_ix {
                sort
            } else {
                ColumnSort::Default
            });
        }
        self.apply_sort();
    }

    fn apply_sort(&mut self) {
        let column = self.sort.and_then(|(ix, sort)| {
            let column = match self.columns.get(ix)?.key.as_ref() {
                "name" => FoundColumn::Name,
                "folder" => FoundColumn::Folder,
                "size" => FoundColumn::Size,
                "modified" => FoundColumn::Modified,
                _ => return None,
            };
            Some((column, sort == ColumnSort::Descending))
        });
        sort_found(&mut self.rows, column);
    }

    /// Change the selection with the rows' paths in the order shown.
    fn with_order<R>(&mut self, change: impl FnOnce(&mut Selection, &[&str]) -> R) -> R {
        let paths: Vec<String> = self.rows.iter().map(FoundFile::path).collect();
        let order: Vec<&str> = paths.iter().map(String::as_str).collect();
        change(&mut self.selection, &order)
    }

    /// Right-clicking a result that is not selected selects it alone, so
    /// the menu acts on what is highlighted; a selected one keeps the
    /// selection.
    fn select_pressed(&mut self, path: &str) {
        if !self.selection.contains(path) {
            self.click(path, ClickMode::Replace);
        }
    }

    fn click(&mut self, path: &str, mode: ClickMode) {
        self.with_order(|selection, order| selection.click(path, mode, order));
    }

    fn move_cursor(&mut self, motion: CursorMotion, page: usize, extend: bool) -> Option<usize> {
        self.with_order(|selection, order| selection.move_cursor(motion, page, extend, order))
    }

    fn toggle_cursor(&mut self) {
        self.with_order(|selection, order| selection.toggle_cursor(order));
    }

    fn select_all(&mut self) {
        self.with_order(|selection, order| selection.select_all(order));
    }

    /// The selected results, in the order shown.
    fn targets(&self) -> Vec<FoundFile> {
        self.rows
            .iter()
            .filter(|found| self.selection.contains(&found.path()))
            .cloned()
            .collect()
    }
}

/// A column's title, by its key, in the interface language.
fn column_title(key: &str) -> SharedString {
    match key {
        "name" => t!("explorer.column.name"),
        "folder" => t!("explorer.find.column_folder"),
        "size" => t!("explorer.column.size"),
        "modified" => t!("explorer.column.modified"),
        _ => SharedString::default(),
    }
}

/// Column widths are an API boundary that takes `Pixels`.
fn columns() -> Vec<Column> {
    let column = |key: &'static str, width: f32| {
        Column::new(key, column_title(key))
            .width(px(width))
            .sortable()
    };
    vec![
        // The name cell paints its selection across the whole cell, padding
        // included, so it pads itself, as the table would.
        column("name", 200.).paddings(Edges::all(px(0.))),
        column("folder", 250.),
        column("size", 90.),
        column("modified", 150.),
    ]
}

impl TableDelegate for FoundFiles {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        self.columns[col_ix].clone()
    }

    /// `Column::text_right` is not applied by `DataTable`. A click on a
    /// title sorts by the column, as in the main window, rather than only
    /// one on the arrow.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let column = &self.columns[col_ix];
        let geometry = self.geometry.clone();
        h_flex()
            .id(ElementId::Name(format!("column:{}", column.key).into()))
            .test_support()
            .size_full()
            .when(column.key.as_ref() == "size", |this| this.justify_end())
            .when(column.key.as_ref() == "name", |this| this.px_1p5())
            .on_any_mouse_down(move |_, _, _| geometry.pressed.set(Pressed::Title))
            // The table keeps the arrows in its columns, which only a
            // `refresh` gives it again.
            .on_click(cx.listener(move |table, _, _, cx| {
                cx.stop_propagation();
                table.delegate_mut().cycle_sort(col_ix);
                table.refresh(cx);
                cx.notify();
            }))
            .child(column.name.clone())
    }

    /// The hover, the selection and the cursor show on the name cell, see
    /// `render_td`. `DataTable` paints its hover over the whole row after
    /// this row's own style, so the row covers it with the table's own
    /// background, under the cells.
    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let Some(found) = self.rows.get(row_ix) else {
            return div().id(("empty-found-row", row_ix));
        };
        let geometry = self.geometry.clone();
        div()
            .id(ElementId::Name(
                format!("found-row:{}", found.path()).into(),
            ))
            .child(div().absolute().inset_0().bg(cx.theme().tokens.table))
            // Where this row paints tells where every row is.
            .child(
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
                .size_full(),
            )
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(found) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        let entry = &found.entry;
        match self.columns[col_ix].key.as_ref() {
            // As in the main window, the name cell is the result: it shows
            // the hover, the selection and the cursor, and takes the clicks.
            "name" => {
                let path = found.path();
                let selected = self.selection.contains(&path);
                // The frame finds the cursor on a name that is not selected.
                let cursor = !selected && self.selection.cursor() == Some(path.as_str());
                let geometry = self.geometry.clone();
                let right_click = {
                    let (path, menu_hit) = (path.clone(), self.menu_hit.clone());
                    cx.listener(move |table, _: &MouseDownEvent, window, cx| {
                        table.delegate_mut().select_pressed(&path);
                        *menu_hit.borrow_mut() = Some(path.clone());
                        window.focus(&table.focus_handle(cx), cx);
                        cx.notify();
                    })
                };
                let label = h_flex()
                    .id(ElementId::Name(format!("found:{path}").into()))
                    .h_full()
                    .min_w_0()
                    .gap_2()
                    .child(FileIcon::entry(entry))
                    .child(div().min_w_0().truncate().child(entry.name.clone()))
                    .test_support()
                    .aria_selected(selected);
                let theme = cx.theme();
                h_flex()
                    .id(ElementId::Name(format!("found-cell:{path}").into()))
                    .size_full()
                    .px_1p5()
                    .border_1()
                    .border_color(if cursor {
                        theme.table_active_border
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .map(|this| {
                        if selected {
                            this.bg(theme.tokens.table_active)
                        } else {
                            this.hover(|this| this.bg(theme.tokens.table_hover))
                        }
                    })
                    .child(label)
                    .on_any_mouse_down(move |_, _, _| geometry.pressed.set(Pressed::Name))
                    .on_mouse_down(MouseButton::Right, right_click)
                    // Select (⌘ adds or removes, Shift extends); a
                    // double-click goes to the result, as WinSCP's does.
                    .on_click(cx.listener(move |table, event: &ClickEvent, window, cx| {
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
                        table.set_right_clicked_row(None, cx);
                        table.delegate_mut().click(&path, mode);
                        window.focus(&table.focus_handle(cx), cx);
                        if mode == ClickMode::Replace && event.click_count() == 2 {
                            // After this update: going there reads the list.
                            let view = table.delegate().view.clone();
                            window.defer(cx, move |window, cx| {
                                let _ = view.update(cx, |view, cx| view.reveal(window, cx));
                            });
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            }
            "folder" => div()
                .min_w_0()
                .truncate()
                .child(found.directory.clone())
                .into_any_element(),
            "size" => h_flex()
                .id(ElementId::Name(
                    format!("found-size:{}", found.path()).into(),
                ))
                .test_support()
                .w_full()
                .justify_end()
                .when(!entry.is_dir(), |this| {
                    this.child(
                        cx.try_global::<FileSizeFormat>()
                            .copied()
                            .unwrap_or_default()
                            .format(entry.size),
                    )
                })
                .into_any_element(),
            "modified" => entry
                .modified
                .map(format_changed)
                .unwrap_or_default()
                .into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.set_sort(col_ix, sort);
        cx.notify();
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let text = match self.state {
            ListState::Ready => SharedString::default(),
            ListState::Searching => t!("explorer.find.searching_short"),
            ListState::Done => t!("explorer.find.nothing"),
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }
}

impl ExplorerPanel {
    /// 查找文件, on the remote side as in WinSCP: the tab's window, opened
    /// over the middle of the main one, or brought forward when it is open.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (current, home) = {
            let remote = self.remote.read(cx);
            if !remote.is_connected() {
                return;
            }
            (remote.path(), remote.home())
        };
        if let Some(find) = &self.find
            && let Some(view) = find.view.upgrade()
            && find
                .window
                .update(cx, |_, window, cx| {
                    view.update(cx, |view, cx| {
                        view.reopen(current.clone(), home.clone(), window, cx)
                    });
                    window.activate_window();
                })
                .is_ok()
        {
            return;
        }
        let host = self
            .store
            .read(cx)
            .host(self.host_id())
            .map_or_else(|| "SFTP".into(), |host| host.name.clone());
        let start = FindStart {
            explorer: cx.entity().downgrade(),
            main: window.window_handle(),
            host,
            commands: self.commands.clone(),
            mask: self.find_mask.clone(),
            current,
            home,
        };
        // Window geometry is a platform boundary; `px` is the API's unit.
        let size = size(px(760.), px(560.));
        let origin = window.bounds().center() - point(size.width / 2., size.height / 2.);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size })),
            display_id: window.display(cx).map(|display| display.id()),
            window_min_size: Some(gpui_kit::size(px(520.), px(380.))),
            app_id: Some(crate::app::DESKTOP_APP_ID.into()),
            ..TitleBar::window_options()
        };
        let mut view = None;
        let opened = cx.open_window(options, |window, cx| {
            let find = cx.new(|cx| FindFiles::new(start, window, cx));
            window.set_window_title(&find.read(cx).title());
            window.activate_window();
            find.update(cx, |find, cx| find.focus_mask(window, cx));
            view = Some(find.downgrade());
            cx.new(|cx| Root::new(find, window, cx))
        });
        match (opened, view) {
            (Ok(window), Some(view)) => self.find = Some(FindWindow { window, view }),
            (Err(error), _) => window.push_notification(Notification::error(error.to_string()), cx),
            (Ok(_), None) => {}
        }
    }

    /// The tab's 查找文件 window, while it is open.
    pub fn find_window(&self) -> Option<AnyWindowHandle> {
        self.find
            .as_ref()
            .filter(|find| find.view.upgrade().is_some())
            .map(|find| find.window.into())
    }

    /// Close the separate search window, when the tab closes or the app locks.
    pub fn close_find(&mut self, cx: &mut App) {
        if let Some(find) = self.find.take() {
            let _ = find
                .window
                .update(cx, |_, window, _| window.remove_window());
        }
    }

    /// A listing the search asked for. One that comes after its window
    /// closed is dropped.
    pub(super) fn find_listed(
        &mut self,
        request_id: u64,
        result: Result<DirectoryListing, String>,
        cx: &mut App,
    ) {
        if let Some(view) = self.find.as_ref().and_then(|find| find.view.upgrade()) {
            view.update(cx, |view, cx| view.listed(request_id, result, cx));
        }
    }

    /// How 删除 in the window went.
    pub(super) fn find_deleted(
        &mut self,
        paths: Vec<String>,
        result: Result<(), String>,
        cx: &mut App,
    ) {
        if let Some(view) = self.find.as_ref().and_then(|find| find.view.upgrade()) {
            view.update(cx, |view, cx| view.deleted(paths, result, cx));
        }
    }

    pub(super) fn find_connection_lost(&mut self, cx: &mut App) {
        if let Some(view) = self.find.as_ref().and_then(|find| find.view.upgrade()) {
            view.update(cx, FindFiles::connection_lost);
        }
    }
}
