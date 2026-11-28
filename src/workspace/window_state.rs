//! 设置 › 应用 › 窗口: the main window's size, maximized state and place,
//! kept in `window.json` as the window closes or ShellRS quits, and where the
//! next launch opens the window from them.
//!
//! Read from the window then, once, rather than followed as it moves: on
//! Windows the close button takes the window down before ShellRS quits, and
//! what it reported on the way was not where it had been.

use std::path::Path;

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::settings::WindowSettings;

use super::Workspace;

/// A new window's size, and the size of one whose size is not remembered.
fn default_size() -> Size<Pixels> {
    size(px(1280.), px(800.))
}

/// The main window as it last was.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    /// The screen it was on, as the system names it across restarts.
    display: Option<String>,
    /// Its size and place when neither maximized nor full screen, as GPUI
    /// gives and takes them: from the screen's top left on macOS, the
    /// desktop's elsewhere.
    bounds: Bounds<Pixels>,
    maximized: bool,
}

impl WindowState {
    /// Waking after an external display was disconnected can leave the window
    /// without a screen temporarily. Keep the last valid state until macOS
    /// assigns it to a screen again.
    pub fn capture(window: &Window, cx: &App) -> Option<Self> {
        // GPUI refreshes the window's display before notifying bounds observers.
        // Check it before is_maximized, whose macOS implementation assumes one.
        let display = window.display(cx)?;
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => (bounds, window.is_maximized()),
            WindowBounds::Maximized(bounds) => (bounds, true),
            WindowBounds::Fullscreen(bounds) => (bounds, false),
        };
        Some(Self {
            display: display.uuid().ok().map(|uuid| uuid.to_string()),
            bounds,
            maximized,
        })
    }

    /// The state saved at `path`; none when there is none or it does not
    /// read, and the window opens as a new one would.
    pub fn load(path: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Written whole, through a temporary file, as the settings are.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temporary, path)
    }
}

/// Keep the window's size and place for the next launch, as they are now:
/// when its close button is pressed (hiding it on macOS, taking it down
/// elsewhere) and when ShellRS quits. Nothing when the window cannot say
/// where it is (on no screen), leaving what was kept before. Always kept:
/// 设置 › 应用 › 窗口 decides what of it the next launch uses.
pub fn keep_window_place(window: &Window, cx: &App) {
    if let Some(state) = WindowState::capture(window, cx) {
        write(&state, &crate::app::window_state_path());
    }
}

impl Workspace {
    /// Keep the window's place as ShellRS quits with the window still there:
    /// ⌘Q, the tray's 退出, 退出 after asking, 重启并安装. The app's, not
    /// this view's, as the view may be gone by then: on Windows and Linux
    /// the close button takes the window down first, having kept its place.
    pub(super) fn keep_window_place_on_quit(&self, window: &Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        App::on_app_quit(cx, move |cx| {
            handle
                .update(cx, |_, window, cx| keep_window_place(window, cx))
                .ok();
            async {}
        })
        .detach();
    }
}

/// Saving the window's place is a convenience: a failure is only logged.
fn write(state: &WindowState, path: &Path) {
    if let Err(error) = state.save(path) {
        eprintln!("shellrs: cannot save the window's place: {error}");
    }
}

/// Where the main window opens: `saved`, as much of it as `settings`
/// remember, on the screens there are now.
pub fn window_placement(
    saved: Option<&WindowState>,
    settings: WindowSettings,
    cx: &App,
) -> (WindowBounds, Option<DisplayId>) {
    let screens: Vec<Screen> = cx
        .displays()
        .iter()
        .map(|display| Screen {
            id: display.id(),
            uuid: display.uuid().ok().map(|uuid| uuid.to_string()),
            visible: display.visible_bounds(),
        })
        .collect();
    let primary = cx.primary_display().map(|display| display.id());
    placement(
        saved,
        settings,
        &screens,
        primary,
        cfg!(target_os = "macos"),
    )
}

/// A screen the window could open on.
struct Screen {
    id: DisplayId,
    uuid: Option<String>,
    /// Without the menu bar, Dock or taskbar.
    visible: Bounds<Pixels>,
}

/// [`window_placement`] over given screens. `macos`: maximized is a frame
/// filling the screen's visible part, as macOS shows it, not a state.
fn placement(
    saved: Option<&WindowState>,
    settings: WindowSettings,
    screens: &[Screen],
    primary: Option<DisplayId>,
    macos: bool,
) -> (WindowBounds, Option<DisplayId>) {
    let primary = screens
        .iter()
        .find(|screen| Some(screen.id) == primary)
        .or(screens.first());
    let saved = saved.filter(|_| settings.remember_size || settings.remember_position);

    // The screen it was on, if remembered and still there; else the main
    // one.
    let remembered = saved
        .and_then(|saved| saved.display.as_ref())
        .filter(|_| settings.remember_position)
        .and_then(|uuid| {
            screens
                .iter()
                .find(|screen| screen.uuid.as_ref() == Some(uuid))
        });
    let screen = remembered.or(primary);
    let sized = saved.filter(|_| settings.remember_size);
    // Taken from a larger screen, it shrinks to fit this one.
    let size = sized.map_or(default_size(), |saved| saved.bounds.size);
    let size = match screen {
        Some(screen) => size.min(&screen.visible.size),
        None => size,
    };
    let bounds = match (saved, remembered) {
        // Where it was, moved back onto its screen where that has changed
        // since (another resolution or scaling) and left it partly off.
        (Some(saved), Some(screen)) => onto(Bounds::new(saved.bounds.origin, size), screen.visible),
        // Clear of the menu bar, Dock and taskbar.
        _ => match screen {
            Some(screen) => Bounds::centered_at(screen.visible.center(), size),
            None => Bounds::new(point(px(0.), px(0.)), size),
        },
    };
    let display = remembered.map(|screen| screen.id);
    let maximized = match sized {
        Some(saved) => saved.maximized,
        // A new window's size that the screen has no room for, as on a
        // laptop at 150%: maximized, and it restores to as much as fits.
        None => screen.is_some_and(|screen| {
            default_size().width > screen.visible.size.width
                || default_size().height > screen.visible.size.height
        }),
    };
    if maximized {
        return match screen {
            Some(screen) if macos => (WindowBounds::Windowed(screen.visible), display),
            _ => (WindowBounds::Maximized(bounds), display),
        };
    }
    (WindowBounds::Windowed(bounds), display)
}

/// `bounds` moved as little as it takes to lie all on `area`, which it fits.
fn onto(bounds: Bounds<Pixels>, area: Bounds<Pixels>) -> Bounds<Pixels> {
    let x = bounds
        .left()
        .max(area.left())
        .min(area.right() - bounds.size.width);
    let y = bounds
        .top()
        .max(area.top())
        .min(area.bottom() - bounds.size.height);
    Bounds::new(point(x, y), bounds.size)
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, DisplayId, Pixels, WindowBounds, point, px, size};

    use super::{Screen, WindowState, default_size, placement};
    use crate::settings::WindowSettings;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    /// A laptop screen and a larger one to its right, the laptop's menu
    /// bar and Dock taken from its visible part.
    fn screens() -> Vec<Screen> {
        vec![
            Screen {
                id: DisplayId::new(1),
                uuid: Some("laptop".into()),
                visible: rect(0., 33., 1512., 890.),
            },
            Screen {
                id: DisplayId::new(2),
                uuid: Some("monitor".into()),
                visible: rect(1512., 0., 2560., 1440.),
            },
        ]
    }

    fn saved(display: &str, bounds: Bounds<Pixels>, maximized: bool) -> WindowState {
        WindowState {
            display: Some(display.into()),
            bounds,
            maximized,
        }
    }

    fn remember(remember_size: bool, remember_position: bool) -> WindowSettings {
        WindowSettings {
            remember_size,
            remember_position,
            ..WindowSettings::default()
        }
    }

    fn place(
        saved: Option<&WindowState>,
        settings: WindowSettings,
        macos: bool,
    ) -> (WindowBounds, Option<DisplayId>) {
        placement(saved, settings, &screens(), Some(DisplayId::new(1)), macos)
    }

    fn centered_on_laptop(size: gpui_kit::Size<Pixels>) -> WindowBounds {
        WindowBounds::Windowed(Bounds::centered_at(
            rect(0., 33., 1512., 890.).center(),
            size,
        ))
    }

    /// A 1920 × 1080 laptop screen at 150%, the taskbar taken from its
    /// visible part: no room for a new window's size.
    fn small_screen() -> Vec<Screen> {
        vec![Screen {
            id: DisplayId::new(1),
            uuid: Some("laptop".into()),
            visible: rect(0., 0., 1280., 672.),
        }]
    }

    #[test]
    fn a_first_launch_opens_in_the_middle_of_the_main_screen() {
        let opened = place(None, WindowSettings::default(), false);
        assert_eq!(opened, (centered_on_laptop(default_size()), None));
    }

    #[test]
    fn the_window_comes_back_where_it_was_on_the_screen_it_was_on() {
        let was = rect(1700., 120., 1600., 1000.);
        let state = saved("monitor", was, false);
        assert_eq!(
            place(Some(&state), WindowSettings::default(), false),
            (WindowBounds::Windowed(was), Some(DisplayId::new(2)))
        );

        // Without its place, it keeps its size in the middle of the main
        // screen, shrunk to fit there.
        assert_eq!(
            place(Some(&state), remember(true, false), false),
            (centered_on_laptop(size(px(1512.), px(890.))), None)
        );
        // Without its size, a new window's size where it was.
        assert_eq!(
            place(Some(&state), remember(false, true), false),
            (
                WindowBounds::Windowed(Bounds::new(was.origin, default_size())),
                Some(DisplayId::new(2))
            )
        );
        // Neither: as a new window.
        assert_eq!(
            place(Some(&state), remember(false, false), false),
            (centered_on_laptop(default_size()), None)
        );
    }

    #[test]
    fn a_window_whose_screen_is_gone_is_centered_and_one_off_its_screen_comes_back() {
        // Its screen unplugged: in the middle of the main one.
        let state = saved("projector", rect(100., 100., 1000., 700.), false);
        assert_eq!(
            place(Some(&state), WindowSettings::default(), false),
            (centered_on_laptop(size(px(1000.), px(700.))), None)
        );
        // Partly off its screen, as after a change of resolution or
        // scaling: moved back the least, onto its visible part.
        for (was, back) in [
            (rect(100., -200., 1000., 700.), rect(100., 33., 1000., 700.)),
            (
                rect(1450., 100., 1000., 700.),
                rect(512., 100., 1000., 700.),
            ),
            (rect(-300., 500., 1000., 700.), rect(0., 223., 1000., 700.)),
        ] {
            let state = saved("laptop", was, false);
            assert_eq!(
                place(Some(&state), WindowSettings::default(), false),
                (WindowBounds::Windowed(back), Some(DisplayId::new(1)))
            );
        }
    }

    #[test]
    fn a_maximized_window_opens_maximized_if_its_size_is_remembered() {
        let restored = rect(1700., 120., 1600., 1000.);
        let state = saved("monitor", restored, true);
        assert_eq!(
            place(Some(&state), WindowSettings::default(), false),
            (WindowBounds::Maximized(restored), Some(DisplayId::new(2)))
        );
        // On macOS, as a frame filling the screen but its menu bar and Dock.
        let state = saved("laptop", rect(0., 33., 1512., 890.), true);
        assert_eq!(
            place(Some(&state), WindowSettings::default(), true),
            (
                WindowBounds::Windowed(rect(0., 33., 1512., 890.)),
                Some(DisplayId::new(1))
            )
        );
        assert!(matches!(
            place(Some(&state), remember(false, true), true).0,
            WindowBounds::Windowed(bounds) if bounds.size == default_size()
        ));
    }

    #[test]
    fn a_new_window_too_large_for_the_screen_opens_maximized() {
        let place = |saved: Option<&WindowState>, settings, macos| {
            placement(
                saved,
                settings,
                &small_screen(),
                Some(DisplayId::new(1)),
                macos,
            )
        };
        // Restored, it fills the screen but the taskbar.
        let fitted = rect(0., 0., 1280., 672.);
        assert_eq!(
            place(None, WindowSettings::default(), false),
            (WindowBounds::Maximized(fitted), None)
        );
        assert_eq!(
            place(None, WindowSettings::default(), true),
            (WindowBounds::Windowed(fitted), None)
        );
        // So does one whose size is not remembered.
        let state = saved("laptop", rect(0., 0., 1000., 600.), false);
        assert_eq!(
            place(Some(&state), remember(false, true), false),
            (WindowBounds::Maximized(fitted), Some(DisplayId::new(1)))
        );
        // A remembered size shrinks to fit, clear of the taskbar, and stays
        // as it was.
        let state = saved("monitor", rect(1700., 120., 1600., 1000.), false);
        assert_eq!(
            place(Some(&state), WindowSettings::default(), false),
            (WindowBounds::Windowed(fitted), None)
        );
    }

    #[test]
    fn the_state_is_kept_in_a_file_and_a_bad_one_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("window.json");
        assert_eq!(WindowState::load(&path), None);
        let state = saved("monitor", rect(1700., 120., 1600., 1000.), true);
        state.save(&path).unwrap();
        assert_eq!(WindowState::load(&path), Some(state));
        std::fs::write(&path, "{\"bounds\": 7}").unwrap();
        assert_eq!(WindowState::load(&path), None);
    }
}
