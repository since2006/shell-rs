//! Identify software rendering for the About page. Frame pacing and motion
//! remain unchanged; the terminal and Windows renderer use incremental drawing.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui_kit::{App, GpuSpecs, Window};

static SOFTWARE: AtomicBool = AtomicBool::new(false);

/// Note how `window`, the main window, is drawn. `main` calls this once the
/// window is open; the UI tests never do, so they draw as with a GPU.
pub fn note_renderer(window: &Window, _: &mut App) {
    let software = window.gpu_specs().is_some_and(|specs| is_software(&specs));
    SOFTWARE.store(software, Ordering::Relaxed);
}

/// Whether the processor draws ShellRS's windows.
pub fn software_rendering() -> bool {
    SOFTWARE.load(Ordering::Relaxed)
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
