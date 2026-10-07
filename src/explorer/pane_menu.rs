//! Menus of one file pane: the list's and the path label's context menus,
//! and the toolbar's bookmark and 新建 menus. Each item dispatches the same
//! `ExplorerCommand` as the toolbar button with that verb.

use super::{ExplorerId, FileSizeFormat, NewEntryKind, PaneSide, PreviewKind};
use crate::app::{
    CatalogIcon, ExplorerAction, ExplorerCommand, SetFileSizeFormat, ToggleHiddenFiles,
};
use crate::i18n::t;
use gpui_kit::component::{Icon, IconName, menu::PopupMenu};
use gpui_kit::*;

/// What the menus need to know about a pane, read when a menu opens.
#[derive(Clone, Debug)]
pub(super) struct PaneMenuState {
    pub remote: bool,
    pub explorer: ExplorerId,
    /// The rows the item commands act on (the selection).
    pub targets: Vec<String>,
    /// The single selected row is a file the editor can try to open: its
    /// full path.
    pub edits_file: Option<String>,
    /// How that file can be previewed, if it can.
    pub preview_kind: Option<PreviewKind>,
    pub can_go_up: bool,
    pub can_go_home: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub path: String,
    pub bookmarks: Vec<String>,
    /// Commands that change files can run now.
    pub can_modify: bool,
    /// A transfer can start now.
    pub can_transfer: bool,
    /// 显示隐藏文件 is on for this side.
    pub show_hidden: bool,
}

impl PaneMenuState {
    fn action(&self, command: ExplorerCommand) -> Box<dyn Action> {
        Box::new(ExplorerAction::new(self.explorer, command))
    }
}

/// Right-click on a row: commands for the selection.
pub(super) fn item_menu(menu: PopupMenu, state: &PaneMenuState) -> PopupMenu {
    let remote = state.remote;
    let none = state.targets.is_empty() || !state.can_modify;
    // A directory has no 打开: a double-click goes in.
    let menu = if let Some(path) = state.edits_file.clone() {
        // A picture is only previewed: as text it would be refused. An SVG
        // is text, and is both edited and previewed.
        let kind = state.preview_kind;
        let menu = if kind.is_some_and(|kind| kind.preview_only()) {
            menu
        } else {
            menu.menu_with_icon(
                t!("explorer.command.edit"),
                Icon::new(CatalogIcon::FilePenLine),
                state.action(ExplorerCommand::Edit {
                    remote,
                    path: Some(path.clone()),
                }),
            )
        };
        let menu = if kind.is_some() {
            menu.menu_with_icon(
                t!("explorer.command.preview"),
                Icon::new(CatalogIcon::Eye),
                state.action(ExplorerCommand::Preview {
                    remote,
                    path: Some(path),
                }),
            )
        } else {
            menu
        };
        menu.separator()
    } else {
        menu
    };
    let (verb, icon) = if remote {
        (t!("explorer.command.download"), CatalogIcon::Download)
    } else {
        (t!("explorer.command.upload"), CatalogIcon::Upload)
    };
    menu.menu_with_icon_and_disabled(
        verb,
        Icon::new(icon),
        state.action(ExplorerCommand::Transfer { remote }),
        state.targets.is_empty() || !state.can_transfer,
    )
    .menu_with_icon_and_disabled(
        t!("explorer.command.copy_path"),
        Icon::new(IconName::Copy),
        state.action(ExplorerCommand::CopySelectedPaths { remote }),
        state.targets.is_empty(),
    )
    .separator()
    .menu_with_icon_and_disabled(
        t!("common.delete"),
        Icon::new(CatalogIcon::Trash),
        state.action(ExplorerCommand::Delete { remote }),
        none,
    )
    .menu_with_icon_and_disabled(
        t!("explorer.command.rename"),
        Icon::new(CatalogIcon::SquarePen),
        state.action(ExplorerCommand::Rename { remote }),
        none || state.targets.len() != 1,
    )
    .separator()
    .menu_with_icon_and_disabled(
        t!("explorer.command.properties"),
        Icon::new(IconName::Info),
        state.action(ExplorerCommand::Properties { remote }),
        none,
    )
}

/// Right-click on empty space: commands for the directory itself.
pub(super) fn directory_menu(
    menu: PopupMenu,
    state: &PaneMenuState,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let create = state.clone();
    let menu = refresh_items(menu, state, window, cx)
        .menu_with_check(
            t!("explorer.command.show_hidden_files"),
            state.show_hidden,
            Box::new(ToggleHiddenFiles(PaneSide::from_remote(state.remote))),
        )
        .separator();
    add_bookmark_item(menu, state).separator().submenu(
        t!("explorer.command.new"),
        window,
        cx,
        move |menu, _, _| new_menu(menu, &create),
    )
}

/// Right-click on the path label: WinSCP's panel menu.
pub(super) fn path_menu(
    menu: PopupMenu,
    state: &PaneMenuState,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let remote = state.remote;
    add_bookmark_item(refresh_items(menu, state, window, cx).separator(), state)
        .menu_with_icon(
            t!("explorer.command.copy_path"),
            Icon::new(IconName::Copy),
            state.action(ExplorerCommand::CopyPath { remote }),
        )
        .separator()
        .menu_with_icon(
            t!("explorer.command.open_directory"),
            Icon::new(IconName::FolderOpen),
            state.action(ExplorerCommand::OpenDirectory { remote }),
        )
}

/// 前往 ▸ and 刷新.
fn refresh_items(
    menu: PopupMenu,
    state: &PaneMenuState,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let remote = state.remote;
    let go = state.clone();
    menu.submenu(t!("explorer.command.go"), window, cx, move |menu, _, _| {
        menu.menu_with_icon_and_disabled(
            t!("explorer.command.up"),
            Icon::new(CatalogIcon::FolderUp),
            go.action(ExplorerCommand::Up { remote }),
            !go.can_go_up,
        )
        .menu_with_icon_and_disabled(
            t!("explorer.command.root"),
            Icon::new(CatalogIcon::FolderRoot),
            go.action(ExplorerCommand::Root { remote }),
            !go.can_go_up,
        )
        .menu_with_icon_and_disabled(
            t!("explorer.command.home"),
            Icon::new(CatalogIcon::House),
            go.action(ExplorerCommand::Home { remote }),
            !go.can_go_home,
        )
        .separator()
        .menu_with_icon_and_disabled(
            t!("explorer.command.back"),
            Icon::new(IconName::ArrowLeft),
            go.action(ExplorerCommand::Back { remote }),
            !go.can_go_back,
        )
        .menu_with_icon_and_disabled(
            t!("explorer.command.forward"),
            Icon::new(IconName::ArrowRight),
            go.action(ExplorerCommand::Forward { remote }),
            !go.can_go_forward,
        )
    })
    .menu_with_icon(
        t!("explorer.command.refresh"),
        Icon::new(CatalogIcon::RefreshCw),
        state.action(ExplorerCommand::Refresh { remote }),
    )
}

fn add_bookmark_item(menu: PopupMenu, state: &PaneMenuState) -> PopupMenu {
    menu.menu_with_icon_and_disabled(
        t!("explorer.command.add_bookmark"),
        Icon::new(CatalogIcon::Bookmark),
        state.action(ExplorerCommand::AddBookmark {
            remote: state.remote,
            path: None,
        }),
        state.bookmarks.contains(&state.path),
    )
}

/// The 大小 column title's menu: WinSCP's 文件大小显示为, with the format in
/// use checked. The choice holds for every SFTP tab and is saved.
pub(super) fn size_format_menu(menu: PopupMenu, current: FileSizeFormat) -> PopupMenu {
    FileSizeFormat::ALL.into_iter().fold(
        menu.label(t!("explorer.command.size_format")),
        |menu, format| {
            menu.menu_with_check(
                format.label(),
                format == current,
                Box::new(SetFileSizeFormat(format)),
            )
        },
    )
}

/// The toolbar's 新建 menu, also the context menu's 新建 submenu.
pub(super) fn new_menu(menu: PopupMenu, state: &PaneMenuState) -> PopupMenu {
    let remote = state.remote;
    menu.menu_with_icon_and_disabled(
        t!("explorer.command.new_folder"),
        Icon::new(CatalogIcon::FolderPlus),
        state.action(ExplorerCommand::New {
            remote,
            kind: NewEntryKind::Folder,
        }),
        !state.can_modify,
    )
    .menu_with_icon_and_disabled(
        t!("explorer.command.new_file"),
        Icon::new(CatalogIcon::FilePlus),
        state.action(ExplorerCommand::New {
            remote,
            kind: NewEntryKind::File,
        }),
        !state.can_modify,
    )
}

/// The toolbar's bookmark menu: this pane's bookmarks, then add or remove the
/// current path.
pub(super) fn bookmark_menu(menu: PopupMenu, state: &PaneMenuState) -> PopupMenu {
    let remote = state.remote;
    let menu = if state.bookmarks.is_empty() {
        menu.label(t!("explorer.bookmarks.none"))
    } else {
        state.bookmarks.iter().fold(menu, |menu, path| {
            menu.menu_with_check(
                path.clone(),
                *path == state.path,
                state.action(ExplorerCommand::Navigate {
                    remote,
                    path: path.clone(),
                }),
            )
        })
    };
    let menu = menu.separator();
    if state.bookmarks.contains(&state.path) {
        menu.menu(
            t!("explorer.command.remove_bookmark"),
            state.action(ExplorerCommand::RemoveBookmark {
                remote,
                path: state.path.clone(),
            }),
        )
    } else {
        menu.menu_with_disabled(
            t!("explorer.command.add_bookmark"),
            state.action(ExplorerCommand::AddBookmark { remote, path: None }),
            state.path.is_empty(),
        )
    }
}
