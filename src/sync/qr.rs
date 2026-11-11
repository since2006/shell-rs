//! A QR code, drawn: the setup code as a phone's camera reads it.

use std::rc::Rc;

use gpui_kit::*;
use qrcodegen::{QrCode, QrCodeEcc};

/// The blank modules a reader needs around a code.
const QUIET: usize = 4;

/// A QR code's dark modules, as runs along its rows.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Qr {
    /// The modules along a side.
    size: usize,
    runs: Vec<Run>,
}

/// Dark modules side by side in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Run {
    row: usize,
    start: usize,
    len: usize,
}

impl Qr {
    /// The code of `text`, with what error correction fits it: on a screen
    /// nothing wears a code off, so a long text gives it up before the
    /// modules get any smaller. `None` for more than a code holds.
    pub(super) fn of(text: &str) -> Option<Self> {
        let code = QrCode::encode_text(text, QrCodeEcc::Medium)
            .or_else(|_| QrCode::encode_text(text, QrCodeEcc::Low))
            .ok()?;
        let size = code.size();
        let mut runs = Vec::new();
        for y in 0..size {
            let mut x = 0;
            while x < size {
                if !code.get_module(x, y) {
                    x += 1;
                    continue;
                }
                let start = x;
                while x < size && code.get_module(x, y) {
                    x += 1;
                }
                runs.push(Run {
                    row: y as usize,
                    start: start as usize,
                    len: (x - start) as usize,
                });
            }
        }
        Some(Self {
            size: size as usize,
            runs,
        })
    }

    /// The code in a square `side` wide, with the blank border a reader
    /// needs.
    pub(super) fn element(self: &Rc<Self>, side: Rems) -> impl IntoElement + use<> {
        let qr = self.clone();
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| qr.paint(bounds, window),
        )
        .w(side)
        .h(side)
        .flex_shrink_0()
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let scale = window.scale_factor();
        let units = (self.size + 2 * QUIET) as f32;
        let side = bounds.size.width.min(bounds.size.height);
        // A whole number of device pixels to a module, so each is as wide
        // as the next and the runs meet without a seam.
        let module = px((side.as_f32() * scale / units).floor().max(1.) / scale);
        let drawn = module * units;
        let origin = bounds.origin
            + point(
                (bounds.size.width - drawn) / 2.,
                (bounds.size.height - drawn) / 2.,
            );
        // Black on white in every theme: the colors are the data a camera
        // reads, not the interface's.
        window.paint_quad(fill(Bounds::new(origin, size(drawn, drawn)), white()));
        for run in &self.runs {
            let at = origin
                + point(
                    module * (QUIET + run.start) as f32,
                    module * (QUIET + run.row) as f32,
                );
            window.paint_quad(fill(
                Bounds::new(at, size(module * run.len as f32, module)),
                black(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    // Named one by one: the glob above would bring GPUI's `test` along.
    use qrcodegen::{QrCode, QrCodeEcc};

    use super::{Qr, Run};

    /// The modules `qr` draws dark, row by row.
    fn dark(qr: &Qr) -> Vec<Vec<bool>> {
        let mut rows = vec![vec![false; qr.size]; qr.size];
        for run in &qr.runs {
            for dark in &mut rows[run.row][run.start..run.start + run.len] {
                assert!(!*dark, "runs overlap");
                *dark = true;
            }
        }
        rows
    }

    #[test]
    fn the_runs_are_the_code_s_dark_modules() {
        let text = "shellrs-sync:U1JTQwEAAAAIAAAAAQAAAAE";
        let qr = Qr::of(text).unwrap();
        let code = QrCode::encode_text(text, QrCodeEcc::Medium).unwrap();
        assert_eq!(qr.size, code.size() as usize);
        let rows = dark(&qr);
        for (y, row) in rows.iter().enumerate() {
            for (x, dark) in row.iter().enumerate() {
                assert_eq!(*dark, code.get_module(x as i32, y as i32), "{x},{y}");
            }
        }
        // A finder pattern in three corners: seven dark modules along the
        // edge.
        let last = qr.size - 7;
        for (row, start) in [(0, 0), (0, last), (last, 0)] {
            assert!(
                qr.runs.contains(&Run { row, start, len: 7 }),
                "{row},{start}"
            );
        }
        // No two runs of a row touch: each is as long as it can be.
        for pair in qr.runs.windows(2) {
            assert!(
                pair[0].row != pair[1].row || pair[0].start + pair[0].len < pair[1].start,
                "{pair:?}"
            );
        }
    }

    /// A setup code of the longest kind, an S3 bucket's with long names,
    /// stays a code a phone reads off a screen.
    #[test]
    fn a_long_setup_code_fits_a_code() {
        let long = format!("shellrs-sync:{}", "a".repeat(900));
        let qr = Qr::of(&long).unwrap();
        assert!(qr.size <= 125, "{}", qr.size);
        // What only fits with less error correction still makes one.
        assert!(Qr::of(&"a".repeat(2800)).is_some());
        assert!(Qr::of(&"a".repeat(4096)).is_none());
    }
}
