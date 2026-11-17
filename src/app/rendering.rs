//! Windows drawn by the processor rather than a graphics card: Windows' own
//! WARP in a virtual machine without 3D acceleration, Mesa's llvmpipe or
//! lavapipe on Linux. Every frame then costs processor time, for the whole
//! window at once, so ShellRS draws fewer of them: terminal output at most
//! every [`SOFTWARE_FRAME_INTERVAL`], a cursor that does not blink, and no
//! animations.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::{App, GpuSpecs, Window};

static SOFTWARE: AtomicBool = AtomicBool::new(false);

/// How often terminal output is drawn at most while the processor draws.
pub const SOFTWARE_FRAME_INTERVAL: Duration = Duration::from_millis(50);

/// Note how `window`, the main window, is drawn. `main` calls this once the
/// window is open; the UI tests never do, so they draw as with a GPU.
pub fn note_renderer(window: &Window, cx: &mut App) {
    let software = window.gpu_specs().is_some_and(|specs| is_software(&specs));
    SOFTWARE.store(software, Ordering::Relaxed);
    if software {
        // Not `cx.set_reduce_motion`: gpui-kit follows the system's setting
        // too (live on Linux), and would turn motion back on with it.
        gpui_kit::base::set_reduce_motion_preference(
            gpui_kit::base::ReduceMotionPreference::Reduce,
            cx,
        );
    }
}

/// Whether the processor draws ShellRS's windows.
pub fn software_rendering() -> bool {
    SOFTWARE.load(Ordering::Relaxed)
}

/// Whether a frame waiting since output came in may be drawn `now`, the
/// last having been drawn at `last`: always with a graphics card; with the
/// processor once [`SOFTWARE_FRAME_INTERVAL`] has passed, and right away
/// after a quiet spell, so that typing still echoes at once.
pub fn frame_due(last: Option<Instant>, now: Instant) -> bool {
    let interval = if software_rendering() {
        SOFTWARE_FRAME_INTERVAL
    } else {
        Duration::ZERO
    };
    frame_due_after(last, now, interval)
}

fn frame_due_after(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= interval)
}

fn is_software(specs: &GpuSpecs) -> bool {
    // Microsoft's own adapters (vendor 0x1414: the Basic Render Driver, the
    // remote desktop's) draw on the processor, in case one is not marked so.
    specs.is_software_emulated || specs.driver_name.ends_with("(ID: 0x1414)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_processor_draws_at_most_every_interval_but_at_once_after_a_pause() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let interval = SOFTWARE_FRAME_INTERVAL;
        assert!(frame_due_after(None, at(0), interval));
        assert!(!frame_due_after(Some(at(0)), at(16), interval));
        assert!(!frame_due_after(Some(at(0)), at(48), interval));
        assert!(frame_due_after(Some(at(0)), at(64), interval));
        // A graphics card draws whenever there is something new.
        assert!(frame_due_after(Some(at(0)), at(0), Duration::ZERO));
    }

    #[test]
    fn warp_and_llvmpipe_count_as_the_processor() {
        let specs = |software: bool, driver: &str| GpuSpecs {
            is_software_emulated: software,
            driver_name: driver.into(),
            ..GpuSpecs::default()
        };
        assert!(is_software(&specs(true, "llvmpipe")));
        assert!(is_software(&specs(false, "Unknown Vendor (ID: 0x1414)")));
        assert!(!is_software(&specs(false, "NVIDIA Corporation")));
        assert!(!is_software(&specs(false, "Unknown Vendor (ID: 0x15AD)")));
    }
}
