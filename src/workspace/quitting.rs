//! The quit guard: ⌘Q (and on Windows and Linux the window's close button)
//! asks first while any tab is open, the start page being all there is
//! otherwise (退出前确认, issue #152), and always when an editor has
//! changes not saved yet.

use std::rc::Rc;

use gpui_kit::component::dock::DockPlacement;
use gpui_kit::*;

use crate::i18n::t;
use crate::shared::confirm_danger;

use super::editors::describe_files;
use super::workspace_view::Workspace;

impl Workspace {
    /// Hold the quit back (true) and ask, while tabs are open or changes
    /// would be lost; then, with something not synced yet, hold it back
    /// for a last round.
    pub(super) fn ask_before_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let unsaved = self.all_unsaved_files(cx);
        let tabs_open = !self.dock_area.read(cx).is_empty(DockPlacement::Center, cx);
        let confirm = self.settings().read(cx).settings().window.confirm_quit;
        if unsaved.is_empty() && !(confirm && tabs_open) {
            return self.sync_before_quit(cx);
        }
        // A hidden window (closed on macOS) would ask where nobody sees it.
        crate::app::bring_forward(window, cx);
        let (note, verb) = if unsaved.is_empty() {
            (t!("workspace.quit_note"), t!("workspace.quit"))
        } else {
            (
                t!("workspace.unsaved.quit", files = describe_files(&unsaved)),
                t!("workspace.unsaved.discard_and_quit"),
            )
        };
        let this = cx.weak_entity();
        confirm_danger(
            t!("workspace.unsaved.quit_title"),
            Some(note),
            verb,
            // As without asking: a last round of sync, then quit.
            Rc::new(move |_, cx| {
                let held = this
                    .update(cx, |this, cx| this.sync_before_quit(cx))
                    .unwrap_or(false);
                if !held {
                    cx.quit();
                }
            }),
            window,
            cx,
        );
        true
    }
}
