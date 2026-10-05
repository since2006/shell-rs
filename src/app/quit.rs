//! ⌘Q asks first when quitting would lose something: an editor's changes
//! that are not saved yet.
//!
//! The guard is a global rather than a listener on the workspace's element:
//! after a dialog closes nothing is focused, and then an action reaches only
//! the root of the window, never the workspace, while a global listener
//! hears it wherever focus is.

use std::rc::Rc;

use gpui_kit::*;

type Ask = Rc<dyn Fn(&mut Window, &mut App) -> bool>;

/// Asks the window whether a quit may go ahead. `ask` returns true when it
/// held the quit back, having asked the user (and quitting itself if they
/// agree).
struct QuitGuard {
    window: AnyWindowHandle,
    ask: Ask,
}

impl Global for QuitGuard {}

/// Put a quit through `ask`, run in `window`, from now on.
pub fn set_quit_guard(
    window: AnyWindowHandle,
    ask: impl Fn(&mut Window, &mut App) -> bool + 'static,
    cx: &mut App,
) {
    cx.set_global(QuitGuard {
        window,
        ask: Rc::new(ask),
    });
}

/// The `Quit` action: quit, unless the guard holds it back.
pub(super) fn quit(cx: &mut App) {
    let Some(guard) = cx.try_global::<QuitGuard>() else {
        cx.quit();
        return;
    };
    let (window, ask) = (guard.window, guard.ask.clone());
    // The action is dispatched while the window is being updated, so it can
    // only be asked once that is over.
    cx.defer(move |cx| {
        let held = window
            .update(cx, |_, window, cx| ask(window, cx))
            .unwrap_or(false);
        if !held {
            cx.quit();
        }
    });
}

/// Whether the guard holds a quit back, asked from inside the window: when
/// its close button would quit (Windows and Linux).
pub fn quit_held_back(window: &mut Window, cx: &mut App) -> bool {
    match cx.try_global::<QuitGuard>().map(|guard| guard.ask.clone()) {
        Some(ask) => ask(window, cx),
        None => false,
    }
}
