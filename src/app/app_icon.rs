//! The logo as the Dock icon of a ShellRS that is not in an app bundle.
//!
//! A packaged ShellRS takes its icon from the bundle (`ShellRS.icns`, see
//! `packaging/macos`). Run straight from `cargo run` there is no bundle,
//! and macOS shows a generic program icon; this sets the logo at runtime
//! instead. Windows reads the icon built into the executable (`build.rs`);
//! Linux takes it from the desktop entry the AppImage carries.

/// Show the logo in the Dock when ShellRS runs outside an app bundle.
#[cfg(target_os = "macos")]
pub fn show_logo_when_unbundled(cx: &gpui_kit::App) {
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let bundled = cx
        .app_path()
        .is_ok_and(|path| path.extension().is_some_and(|extension| extension == "app"));
    if bundled {
        return;
    }
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(include_bytes!("../../assets/logo/shellrs.png"));
    let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
        return;
    };
    // SAFETY: a valid image, set on the main thread.
    unsafe { NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image)) };
}

#[cfg(not(target_os = "macos"))]
pub fn show_logo_when_unbundled(_: &gpui_kit::App) {}
