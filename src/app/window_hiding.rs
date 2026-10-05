//! On macOS the window's close button hides ShellRS instead of closing it.
//!
//! ShellRS has one window, and the workspace in it owns everything that
//! runs: connections, transfers, port forwards, the external CLI. Closing
//! it would stop all of that yet leave the app in the Dock with no window,
//! and the Dock icon could not bring one back. So, like Mac terminals,
//! closing hides the app: the Dock icon, ⌘Tab or opening ShellRS again
//! shows the window as it was, and ⌘Q quits. Windows and Linux quit when
//! the window closes, as GPUI does there by default, but like ⌘Q only once
//! the quit guard lets them (unsaved changes in an editor are asked about).

use gpui_kit::{App, Window};

/// Make the window's close button hide ShellRS on macOS. Elsewhere closing
/// quits, after asking the quit guard.
#[cfg(target_os = "macos")]
pub fn hide_when_closed(window: &Window, cx: &App) {
    window.on_window_should_close(cx, |_, cx| {
        cx.hide();
        false
    });
}

#[cfg(not(target_os = "macos"))]
pub fn hide_when_closed(window: &Window, cx: &App) {
    window.on_window_should_close(cx, |window, cx| !super::quit::quit_held_back(window, cx));
}

/// Show the window again, in front of other apps: when the Dock icon is
/// clicked, or ShellRS is opened again while it runs.
pub fn bring_forward(window: &mut Window, cx: &mut App) {
    // Activating alone may leave a hidden app's windows hidden.
    #[cfg(target_os = "macos")]
    if let Some(main_thread) = objc2::MainThreadMarker::new() {
        objc2_app_kit::NSApplication::sharedApplication(main_thread).unhide(None);
    }
    cx.activate(true);
    window.activate_window();
}
