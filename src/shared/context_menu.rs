//! Right-click menus that may have nothing to offer where the press landed.

use gpui_kit::component::menu::{self, ContextMenu, PopupMenu};
use gpui_kit::{App, Context, InteractiveElement, ParentElement, Styled, Window};

/// gpui-kit's `context_menu`, for every right-click menu here.
///
/// gpui-kit gives an opening context menu focus before it knows whether the
/// menu has anything in it, and never draws an empty one, so nothing hands
/// focus back: buttons and shortcuts that dispatch through focus, such as
/// 设置 or closing a tab, stop answering until something else takes it
/// (issue #154). An empty menu hands focus straight back to what had it.
pub trait RightClickMenu: InteractiveElement + ParentElement + Styled + Sized {
    /// Open the menu `build` makes on a right-click; it is built as it
    /// opens, not while the element renders.
    #[track_caller]
    fn context_menu(
        self,
        build: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> ContextMenu<Self> {
        menu::ContextMenuExt::context_menu(self, move |menu, window, cx| {
            let menu = build(menu, window, cx);
            if menu.is_empty() {
                hand_back_focus(window, cx);
            }
            menu
        })
    }
}

impl<E: InteractiveElement + ParentElement + Styled> RightClickMenu for E {}

/// Called while the menu is built, before it takes focus: once it has,
/// focus goes back to what has it now.
fn hand_back_focus(window: &mut Window, cx: &mut App) {
    let focused = window.focused(cx);
    window.defer(cx, move |window, cx| match focused {
        Some(focused) => focused.focus(window, cx),
        None => window.blur(cx),
    });
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    fn sources(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                sources(&path, files);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }

    /// gpui-kit's own `context_menu` would leave focus on an empty menu.
    #[test]
    fn right_click_menus_go_through_this_module() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&root, &mut files);
        let this = root.join("shared/context_menu.rs");
        let offenders: Vec<_> = files
            .iter()
            .filter(|path| **path != this)
            .filter(|path| {
                fs::read_to_string(path)
                    .expect("readable source")
                    .contains("ContextMenuExt")
            })
            .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        assert!(
            offenders.is_empty(),
            "use `crate::shared::RightClickMenu`: {offenders:?}"
        );
    }
}
