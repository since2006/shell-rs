//! Windows' optional close-to-tray behavior. Only `main` installs it;
//! headless workspaces never create native icons or intercept OS messages.

#[path = "tray/windows.rs"]
mod windows;

use std::time::Duration;

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::{App, Global, Subscription, Task, Window};
use raw_window_handle::RawWindowHandle;

use crate::i18n::t;

use windows::{NativeTray, TrayEvent};

struct Tray {
    choice: Option<(bool, &'static str)>,
    icon: Option<NativeTray>,
    error: Option<String>,
    _events: Task<()>,
    _quit: Subscription,
}

impl Global for Tray {}

/// Install the production main window's tray service, including while the
/// application is locked. The saved setting decides whether it has an icon.
pub fn listen_for_tray(window: &Window, cx: &mut App) {
    let handle = window.window_handle();
    let events = cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            let event = cx.update(|cx| {
                cx.try_global::<Tray>()
                    .and_then(|tray| tray.icon.as_ref())
                    .and_then(NativeTray::take_event)
            });
            let Some(event) = event else { continue };
            cx.update(|cx| match event {
                TrayEvent::Show => cx.dispatch_action(&super::ShowApp),
                TrayEvent::Quit => {
                    // Show any unsaved-editor confirmation before asking
                    // the normal Quit handler. Closing to tray never quits.
                    cx.dispatch_action(&super::ShowApp);
                    cx.dispatch_action(&super::Quit);
                }
                TrayEvent::Unavailable => {
                    cx.dispatch_action(&super::ShowApp);
                    report(handle, "Windows could not restore the tray icon", cx);
                }
            });
        }
    });
    let quit = cx.on_app_quit(|cx| {
        cx.global_mut::<Tray>().icon.take();
        async {}
    });
    cx.set_global(Tray {
        choice: None,
        icon: None,
        error: None,
        _events: events,
        _quit: quit,
    });
}

/// Apply a change immediately, including the language of the native menu.
/// Without the production service this is deliberately a no-op.
pub(crate) fn sync(enabled: bool, window: &mut Window, cx: &mut App) {
    let choice = (enabled, crate::i18n::locale());
    let Some(tray) = cx.try_global::<Tray>() else {
        return;
    };
    if tray.choice == Some(choice) {
        return;
    }
    let tray = cx.global_mut::<Tray>();
    tray.choice = Some(choice);
    // Remove the old icon and its subclass before creating a replacement
    // with the same icon id, including when the language changes.
    tray.icon.take();
    if !enabled {
        show_window(window);
    }
    let icon = if enabled {
        native_handle(window)
            .and_then(|handle| NativeTray::new(handle, &t!("app.tray.show"), &t!("app.tray.quit")))
            .map(Some)
    } else {
        Ok(None)
    };
    let tray = cx.global_mut::<Tray>();
    let error = match icon {
        Ok(icon) => {
            tray.icon = icon;
            None
        }
        Err(error) => {
            tray.icon = None;
            Some(error.to_string())
        }
    };
    tray.error = error.clone();
    if let Some(error) = error {
        super::bring_forward(window, cx);
        report(window.window_handle(), &error, cx);
    }
}

/// True means the close was handled: either hidden, or kept open because
/// hiding would leave no usable tray icon. Never dispose of the workspace.
pub(super) fn hide_on_close(window: &mut Window, cx: &mut App) -> bool {
    let Some(tray) = cx.try_global::<Tray>() else {
        return false;
    };
    if !tray.choice.is_some_and(|(enabled, _)| enabled) {
        return false;
    }
    let result = match &tray.icon {
        Some(icon) => icon.hide(),
        None => Err(std::io::Error::other(
            tray.error.as_deref().unwrap_or("Tray is unavailable"),
        )),
    };
    if let Err(error) = result {
        report(window.window_handle(), &error.to_string(), cx);
    }
    true
}

/// A hidden HWND must be shown before GPUI can activate it. Keep its
/// maximized state; only a minimized window needs SW_RESTORE.
pub(super) fn show_window(window: &Window) {
    if let Ok(handle) = native_handle(window) {
        windows::show_window(handle);
    }
}

fn native_handle(window: &Window) -> std::io::Result<windows_sys::Win32::Foundation::HWND> {
    match raw_window_handle::HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => Ok(handle.hwnd.get() as _),
        _ => Err(std::io::Error::other(
            "The main window has no Windows handle",
        )),
    }
}

fn report(handle: gpui_kit::AnyWindowHandle, error: &str, cx: &mut App) {
    let message = t!("app.tray.failed", error = error);
    // Startup applies settings before Root exists. Defer until it can show
    // the notification, rather than trying to access Root while constructing it.
    cx.defer(move |cx| {
        _ = handle.update(cx, |_, window, cx| {
            window.push_notification(Notification::error(message), cx);
        });
    });
}
