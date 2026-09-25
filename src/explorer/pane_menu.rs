//! Menus of one file pane: the list's and the path label's context menus,
//! and the toolbar's bookmark and 新建 menus. Each item dispatches the same
//! `ExplorerCommand` as the toolbar button with that verb.

use super::{ExplorerId, NewEntryKind};
use crate::app::{CatalogIcon, ExplorerAction, ExplorerCommand};
use gpui_kit::component::{Icon, IconName, menu::PopupMenu};
use gpui_kit::*;

/// What the menus need to know about a pane, read when a menu opens.
#[derive(Clone, Debug)]
pub(super) struct PaneMenuState {
    pub remote: bool,
    pub explorer: ExplorerId,
    /// The rows the item commands act on (the selection).
    pub targets: Vec<String>,
    /// The single selected row is a directory (or a link to one).
    pub opens_directory: bool,
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
    let menu = if state.opens_directory && state.targets.len() == 1 {
        menu.menu_with_icon(
            "打开",
            Icon::new(IconName::FolderOpen),
            state.action(ExplorerCommand::Open { remote }),
        )
        .separator()
    } else {
        menu
    };
    let (verb, icon) = if remote {
        ("下载…", CatalogIcon::Download)
    } else {
        ("上传…", CatalogIcon::Upload)
    };
    menu.menu_with_icon_and_disabled(
        verb,
        Icon::new(icon),
        state.action(ExplorerCommand::Transfer { remote }),
        state.targets.is_empty() || !state.can_transfer,
    )
    .separator()
    .menu_with_icon_and_disabled(
        "删除",
        Icon::new(CatalogIcon::Trash),
        state.action(ExplorerCommand::Delete { remote }),
        none,
    )
    .menu_with_icon_and_disabled(
        "重命名…",
        Icon::new(CatalogIcon::SquarePen),
        state.action(ExplorerCommand::Rename { remote }),
        none || state.targets.len() != 1,
    )
    .separator()
    .menu_with_icon_and_disabled(
        "属性…",
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
    add_bookmark_item(refresh_items(menu, state, window, cx), state)
        .separator()
        .submenu("新建", window, cx, move |menu, _, _| {
            new_menu(menu, &create)
        })
}

/// Right-click on the path label: WinSCP's panel menu.
pub(super) fn path_menu(
    menu: PopupMenu,
    state: &PaneMenuState,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let remote = state.remote;
    add_bookmark_item(refresh_items(menu, state, window, cx), state)
        .menu_with_icon(
            "复制路径",
            Icon::new(IconName::Copy),
            state.action(ExplorerCommand::CopyPath { remote }),
        )
        .separator()
        .menu_with_icon(
            "打开目录/书签…",
            Icon::new(IconName::FolderOpen),
            state.action(ExplorerCommand::OpenDirectory { remote }),
        )
}

/// 前往 ▸ and 刷新, then a separator.
fn refresh_items(
    menu: PopupMenu,
    state: &PaneMenuState,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let remote = state.remote;
    let go = state.clone();
    menu.submenu("前往", window, cx, move |menu, _, _| {
        menu.menu_with_icon_and_disabled(
            "上级目录",
            Icon::new(CatalogIcon::FolderUp),
            go.action(ExplorerCommand::Up { remote }),
            !go.can_go_up,
        )
        .menu_with_icon_and_disabled(
            "根目录",
            Icon::new(CatalogIcon::FolderRoot),
            go.action(ExplorerCommand::Root { remote }),
            !go.can_go_up,
        )
        .menu_with_icon_and_disabled(
            "主目录",
            Icon::new(CatalogIcon::House),
            go.action(ExplorerCommand::Home { remote }),
            !go.can_go_home,
        )
        .separator()
        .menu_with_icon_and_disabled(
            "后退",
            Icon::new(IconName::ArrowLeft),
            go.action(ExplorerCommand::Back { remote }),
            !go.can_go_back,
        )
        .menu_with_icon_and_disabled(
            "前进",
            Icon::new(IconName::ArrowRight),
            go.action(ExplorerCommand::Forward { remote }),
            !go.can_go_forward,
        )
    })
    .menu_with_icon(
        "刷新",
        Icon::new(CatalogIcon::RefreshCw),
        state.action(ExplorerCommand::Refresh { remote }),
    )
    .separator()
}

fn add_bookmark_item(menu: PopupMenu, state: &PaneMenuState) -> PopupMenu {
    menu.menu_with_icon_and_disabled(
        "添加路径到书签",
        Icon::new(CatalogIcon::Bookmark),
        state.action(ExplorerCommand::AddBookmark {
            remote: state.remote,
            path: None,
        }),
        state.bookmarks.contains(&state.path),
    )
}

/// The toolbar's 新建 menu, also the context menu's 新建 submenu.
pub(super) fn new_menu(menu: PopupMenu, state: &PaneMenuState) -> PopupMenu {
    let remote = state.remote;
    menu.menu_with_icon_and_disabled(
        "文件夹…",
        Icon::new(CatalogIcon::FolderPlus),
        state.action(ExplorerCommand::New {
            remote,
            kind: NewEntryKind::Folder,
        }),
        !state.can_modify,
    )
    .menu_with_icon_and_disabled(
        "文件…",
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
        menu.label("暂无书签")
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
            "从书签中移除路径",
            state.action(ExplorerCommand::RemoveBookmark {
                remote,
                path: state.path.clone(),
            }),
        )
    } else {
        menu.menu_with_disabled(
            "添加路径到书签",
            state.action(ExplorerCommand::AddBookmark { remote, path: None }),
            state.path.is_empty(),
        )
    }
}
