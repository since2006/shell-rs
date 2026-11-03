//! The notification-area icon and its native menu, on the window's UI
//! thread. Native callbacks only record requests; GPUI polls and dispatches
//! the application actions after the native callback has returned.

use std::{cell::Cell, io, ptr, rc::Rc};

use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
            NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
            RemoveWindowSubclass, SetWindowSubclass, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, GetSystemMetrics,
            HICON, HMENU, IMAGE_ICON, IsIconic, LR_DEFAULTCOLOR, LoadImageW, MF_STRING,
            PostMessageW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SW_HIDE, SW_RESTORE,
            SW_SHOW, SetForegroundWindow, SetMenuDefaultItem, ShowWindow, TPM_NONOTIFY,
            TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_CONTEXTMENU, WM_NCDESTROY, WM_NULL,
        },
    },
};

const SHOW: usize = 1;
const QUIT: usize = 2;
// NIN_KEYSELECT = NIN_SELECT | NINF_KEY, absent from windows-sys 0.61.
const KEY_SELECT: u32 = NIN_SELECT | 1;

#[derive(Clone, Copy)]
pub(super) enum TrayEvent {
    Show,
    Quit,
    Unavailable,
}

struct State {
    data: NOTIFYICONDATAW,
    menu: HMENU,
    taskbar_created: u32,
    alive: Cell<bool>,
    registered: Cell<bool>,
    event: Cell<Option<TrayEvent>>,
}

impl State {
    fn add(&self) -> io::Result<()> {
        // SAFETY: the data names our live window and retained icon; both
        // remain valid until the subclass and notification icon are removed.
        unsafe {
            if Shell_NotifyIconW(NIM_ADD, &self.data) == 0 {
                return Err(io::Error::other("Windows could not create the tray icon"));
            }
            self.registered.set(true);
            if Shell_NotifyIconW(NIM_SETVERSION, &self.data) == 0 {
                self.remove();
                return Err(io::Error::other("Windows refused the tray icon version"));
            }
        }
        Ok(())
    }

    fn remove(&self) {
        if self.registered.replace(false) {
            // SAFETY: the structure identifies only this application's icon.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data) };
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        // SAFETY: these owned resources outlive any callback's Rc clone.
        unsafe {
            DestroyMenu(self.menu);
            DestroyIcon(self.data.hIcon);
        }
    }
}

pub(super) struct NativeTray(Rc<State>);

impl NativeTray {
    pub(super) fn new(hwnd: HWND, show: &str, quit: &str) -> io::Result<Self> {
        // SAFETY: called on the thread that owns hwnd, with live buffers
        // throughout each call. The executable embeds its logo as resource 1.
        let state = unsafe {
            let instance = GetModuleHandleW(ptr::null());
            let icon = LoadImageW(
                instance,
                1_usize as _,
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_DEFAULTCOLOR,
            ) as HICON;
            if icon.is_null() {
                return Err(io::Error::last_os_error());
            }
            let menu = CreatePopupMenu();
            if menu.is_null() {
                let error = io::Error::last_os_error();
                DestroyIcon(icon);
                return Err(error);
            }
            let callback = RegisterWindowMessageW(wide("ShellRS.TrayIcon").as_ptr());
            let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
            let mut data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as _,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP,
                uCallbackMessage: callback,
                hIcon: icon,
                ..Default::default()
            };
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            for (slot, unit) in data.szTip.iter_mut().zip("ShellRS".encode_utf16()) {
                *slot = unit;
            }
            let state = Rc::new(State {
                data,
                menu,
                taskbar_created,
                alive: Cell::new(false),
                registered: Cell::new(false),
                event: Cell::new(None),
            });
            if callback == 0
                || taskbar_created == 0
                || AppendMenuW(menu, MF_STRING, SHOW, wide(show).as_ptr()) == 0
                || AppendMenuW(menu, MF_STRING, QUIT, wide(quit).as_ptr()) == 0
            {
                return Err(io::Error::last_os_error());
            }
            SetMenuDefaultItem(menu, SHOW as _, 0);
            if SetWindowSubclass(
                hwnd,
                Some(callback_proc),
                callback as _,
                Rc::as_ptr(&state) as _,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            state.alive.set(true);
            state
        };
        let tray = Self(state);
        tray.0.add()?;
        Ok(tray)
    }

    pub(super) fn take_event(&self) -> Option<TrayEvent> {
        self.0.event.take()
    }

    pub(super) fn hide(&self) -> io::Result<()> {
        if !self.0.alive.get() {
            return Err(io::Error::other("The tray window was closed"));
        }
        if !self.0.registered.get() {
            self.0.add()?;
        }
        // SAFETY: the subclass's alive flag is cleared when hwnd is destroyed.
        unsafe { ShowWindow(self.0.data.hWnd, SW_HIDE) };
        Ok(())
    }
}

impl Drop for NativeTray {
    fn drop(&mut self) {
        self.0.remove();
        if self.0.alive.replace(false) {
            // SAFETY: same UI thread; detach before releasing the callback's
            // state. WM_NCDESTROY already did so if the window closed first.
            unsafe {
                RemoveWindowSubclass(
                    self.0.data.hWnd,
                    Some(callback_proc),
                    self.0.data.uCallbackMessage as _,
                );
            }
        }
    }
}

pub(super) fn show_window(hwnd: HWND) {
    // SAFETY: the caller obtains hwnd from a live GPUI window. SW_SHOW keeps
    // a maximized hidden window maximized; minimized windows need restoring.
    unsafe {
        ShowWindow(
            hwnd,
            if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        )
    };
}

unsafe extern "system" fn callback_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: installed from NativeTray on this thread. Clone before making
    // native calls, whose nested message loop may remove the tray service.
    let state = unsafe {
        let pointer = data as *const State;
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    };
    if message == state.data.uCallbackMessage {
        match lparam as u32 & 0xffff {
            NIN_SELECT | KEY_SELECT => state.event.set(Some(TrayEvent::Show)),
            WM_CONTEXTMENU => {
                let mut position = windows_sys::Win32::Foundation::POINT {
                    x: wparam as u16 as i16 as i32,
                    y: (wparam >> 16) as u16 as i16 as i32,
                };
                // SAFETY: menu and hwnd are retained by this callback's Rc.
                unsafe {
                    if position.x == -1 && position.y == -1 {
                        GetCursorPos(&mut position);
                    }
                    SetForegroundWindow(hwnd);
                    let command = TrackPopupMenu(
                        state.menu,
                        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                        position.x,
                        position.y,
                        0,
                        hwnd,
                        ptr::null(),
                    );
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                    match command as usize {
                        SHOW => state.event.set(Some(TrayEvent::Show)),
                        QUIT => state.event.set(Some(TrayEvent::Quit)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        return 0;
    }
    if message == state.taskbar_created {
        // Explorer has restarted: its old icon no longer exists.
        state.registered.set(false);
        if state.add().is_err() {
            state.event.set(Some(TrayEvent::Unavailable));
        }
        return 0;
    }
    if message == WM_NCDESTROY {
        state.remove();
        state.alive.set(false);
        // SAFETY: this is the callback installed under id on hwnd.
        unsafe { RemoveWindowSubclass(hwnd, Some(callback_proc), id) };
    }
    // SAFETY: all unrelated messages, including shutdown, keep GPUI's handling.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, ptr, rc::Rc};

    use windows_sys::Win32::UI::{
        Shell::{NIN_SELECT, NOTIFYICONDATAW, SetWindowSubclass},
        WindowsAndMessaging::{
            CreatePopupMenu, CreateWindowExW, DestroyWindow, RegisterWindowMessageW, SendMessageW,
            WM_QUERYENDSESSION,
        },
    };

    use super::{KEY_SELECT, NativeTray, State, TrayEvent, callback_proc, wide};

    /// Exercise the real subclass on a native, invisible window, without
    /// creating a tray icon or requiring Explorer on the CI worker.
    #[test]
    fn native_callbacks_queue_requests_and_detach_when_the_window_closes() {
        // SAFETY: this test owns the hidden system-class window and all of
        // its resources on this thread; State survives window destruction.
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            assert!(!hwnd.is_null());
            let callback = RegisterWindowMessageW(wide("ShellRS.TrayIcon.Test").as_ptr());
            assert_ne!(callback, 0);
            let state = Rc::new(State {
                data: NOTIFYICONDATAW {
                    hWnd: hwnd,
                    uCallbackMessage: callback,
                    ..Default::default()
                },
                menu: CreatePopupMenu(),
                taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
                alive: Cell::new(true),
                registered: Cell::new(false),
                event: Cell::new(None),
            });
            let tray = NativeTray(state.clone());
            assert_ne!(
                SetWindowSubclass(
                    hwnd,
                    Some(callback_proc),
                    callback as _,
                    Rc::as_ptr(&state) as _
                ),
                0
            );
            for event in [NIN_SELECT, KEY_SELECT] {
                SendMessageW(hwnd, callback, 0, ((1_u32 << 16) | event) as _);
                assert!(matches!(tray.take_event(), Some(TrayEvent::Show)));
                assert!(tray.take_event().is_none());
                assert!(state.alive.get());
            }
            // System shutdown retains the normal window procedure, rather
            // than becoming another request to hide in the tray.
            SendMessageW(hwnd, WM_QUERYENDSESSION, 0, 0);
            assert!(tray.take_event().is_none());
            assert_ne!(DestroyWindow(hwnd), 0);
            assert!(!state.alive.get());
            assert!(tray.hide().is_err());
            drop(tray);
        }
    }
}
