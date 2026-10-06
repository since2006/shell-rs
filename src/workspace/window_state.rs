//! 设置 › 应用 › 窗口: the main window's size, maximized state and place,
//! kept in `window.json` as they change, and where the next launch opens
//! the window from them.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::settings::WindowSettings;

use super::Workspace;

/// How long a move or resize has to settle before it is written: a drag
/// reports every step.
const SAVE_DELAY: Duration = Duration::from_millis(500);

/// A new window's size, and the size of one whose size is not remembered.
fn default_size() -> Size<Pixels> {
    size(px(1280.), px(800.))
}

/// How much of a window has to be on its screen for it to open where it
/// was: enough of the title bar to drag it by.
const VISIBLE_WIDTH: f32 = 120.;
const VISIBLE_HEIGHT: f32 = 40.;

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
    pub fn capture(window: &Window, cx: &App) -> Self {
        // macOS never reports a window maximized, only its frame filling
        // the screen; a full screen window comes back as it was before.
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(bounds) => (bounds, window.is_maximized()),
            WindowBounds::Maximized(bounds) => (bounds, true),
            WindowBounds::Fullscreen(bounds) => (bounds, false),
        };
        let display = window
            .display(cx)
            .and_then(|display| display.uuid().ok())
            .map(|uuid| uuid.to_string());
        Self {
            display,
            bounds,
            maximized,
        }
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

impl Workspace {
    /// Keep the window's size and place at `path` as they change, for the
    /// next launch. Always kept: 设置 › 应用 › 窗口 decides what of it the
    /// next launch uses.
    pub(super) fn remember_window(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let latest: Rc<RefCell<Option<WindowState>>> = Rc::default();
        // Written again on quitting, in case a change has not settled. The
        // app's, not this view's: on Windows and Linux closing the window
        // has dropped the view by then.
        App::on_app_quit(cx, {
            let (latest, path) = (latest.clone(), path.clone());
            move |_| {
                if let Some(state) = latest.borrow().as_ref() {
                    write(state, &path);
                }
                async {}
            }
        })
        .detach();
        let subscription = cx.observe_window_bounds(window, move |this, window, cx| {
            let state = WindowState::capture(window, cx);
            if latest.borrow().as_ref() == Some(&state) {
                return;
            }
            *latest.borrow_mut() = Some(state.clone());
            let path = path.clone();
            this.window_state_save = Some(cx.spawn(async move |_, cx| {
                cx.background_executor().timer(SAVE_DELAY).await;
                cx.background_spawn(async move { write(&state, &path) })
                    .await;
            }));
        });
        self._subscriptions.push(subscription);
    }
}

/// Saving the window's place is a convenience: a failure is only logged.
fn write(state: &WindowState, path: &Path) {
    if let Err(error) = state.save(path) {
        eprintln!("shellrs: 无法保存窗口位置：{error}");
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
            bounds: display.bounds(),
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
    bounds: Bounds<Pixels>,
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
    let centered = |screen: Option<&Screen>, size: Size<Pixels>| match screen {
        Some(screen) => Bounds::centered_at(screen.bounds.center(), size),
        None => Bounds::new(point(px(0.), px(0.)), size),
    };
    let Some(saved) = saved.filter(|_| settings.remember_size || settings.remember_position) else {
        return (
            WindowBounds::Windowed(centered(primary, default_size())),
            None,
        );
    };

    // The screen it was on, if remembered and still there; else the main
    // one.
    let remembered = saved
        .display
        .as_ref()
        .filter(|_| settings.remember_position)
        .and_then(|uuid| {
            screens
                .iter()
                .find(|screen| screen.uuid.as_ref() == Some(uuid))
        });
    let screen = remembered.or(primary);
    let size = if settings.remember_size {
        // Taken from a larger screen, it shrinks to fit this one.
        match screen {
            Some(screen) => size(
                saved.bounds.size.width.min(screen.visible.size.width),
                saved.bounds.size.height.min(screen.visible.size.height),
            ),
            None => saved.bounds.size,
        }
    } else {
        default_size()
    };
    let bounds = match remembered {
        Some(screen) if reachable(Bounds::new(saved.bounds.origin, size), screen.bounds) => {
            Bounds::new(saved.bounds.origin, size)
        }
        _ => centered(screen, size),
    };
    let display = remembered.map(|screen| screen.id);
    if settings.remember_size && saved.maximized {
        return match screen {
            Some(screen) if macos => (WindowBounds::Windowed(screen.visible), display),
            _ => (WindowBounds::Maximized(bounds), display),
        };
    }
    (WindowBounds::Windowed(bounds), display)
}

/// Whether enough of `bounds` is on the screen to take hold of: its top
/// edge on the screen, and a stretch of title bar with it.
fn reachable(bounds: Bounds<Pixels>, screen: Bounds<Pixels>) -> bool {
    let overlap = bounds.intersect(&screen);
    bounds.top() >= screen.top()
        && bounds.top() <= screen.bottom() - px(VISIBLE_HEIGHT)
        && overlap.size.width >= px(VISIBLE_WIDTH)
        && overlap.size.height >= px(VISIBLE_HEIGHT)
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
                bounds: rect(0., 0., 1512., 982.),
                visible: rect(0., 33., 1512., 890.),
            },
            Screen {
                id: DisplayId::new(2),
                uuid: Some("monitor".into()),
                bounds: rect(1512., 0., 2560., 1440.),
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
            rect(0., 0., 1512., 982.).center(),
            size,
        ))
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
    fn a_window_whose_screen_is_gone_or_that_is_off_it_is_centered() {
        let state = saved("projector", rect(100., 100., 1000., 700.), false);
        assert_eq!(
            place(Some(&state), WindowSettings::default(), false),
            (centered_on_laptop(size(px(1000.), px(700.))), None)
        );
        // Dragged up past the top, or almost all off the side.
        for was in [
            rect(100., -200., 1000., 700.),
            rect(1450., 100., 1000., 700.),
        ] {
            let state = saved("laptop", was, false);
            let (bounds, display) = place(Some(&state), WindowSettings::default(), false);
            assert_eq!(bounds, centered_on_laptop(was.size));
            assert_eq!(display, Some(DisplayId::new(1)));
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
