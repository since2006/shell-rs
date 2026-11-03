//! Observe genuine input before components consume it. Painting only installs
//! listeners; output, redraws and background jobs never reset the idle clock.
use gpui_kit::*;

use super::AppLock;

pub fn activity_tracker(cx: &App) -> impl IntoElement {
    let lock = AppLock::global(cx);
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let Some(lock) = lock else {
                return;
            };
            observe_mouse::<MouseDownEvent>(&lock, window);
            observe_mouse::<MouseUpEvent>(&lock, window);
            observe_mouse::<MouseMoveEvent>(&lock, window);
            observe_mouse::<ScrollWheelEvent>(&lock, window);
        },
    )
    .absolute()
}

fn observe_mouse<Event: MouseEvent>(lock: &Entity<AppLock>, window: &mut Window) {
    let lock = lock.downgrade();
    window.on_mouse_event(move |_: &Event, phase, _, cx| {
        if phase == DispatchPhase::Capture {
            let _ = lock.update(cx, |lock, cx| lock.record_activity(cx));
        }
    });
}
