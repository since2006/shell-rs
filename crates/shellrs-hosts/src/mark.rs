//! The colour a host is drawn in when its system has none of its own: the
//! same for an address on every machine, in every version and in the
//! mobile apps, so a host keeps its colour wherever it is.

/// The colours a host without one of its own is drawn on, picked by its
/// address with [`mark_color`]. Saturated and dark enough for a white glyph
/// (4.5:1 at least), and never grey: grey is what a tab draws a host that is
/// not connected in. No red, which reads as an error, nor yellow, which a
/// white glyph cannot be read on. Data, like the brand colours, so the same
/// in either appearance.
const MARK_COLORS: [u32; 13] = [
    0x25_63EB, // blue
    0x4F_46E5, // indigo
    0x7C_3AED, // violet
    0x93_33EA, // purple
    0xA2_1CAF, // fuchsia
    0xBE_185D, // pink
    0xC2_410C, // orange
    0xB4_5309, // amber
    0x15_803D, // green
    0x04_7857, // emerald
    0x0F_766E, // teal
    0x0E_7490, // cyan
    0x03_69A1, // sky
];

/// The colour of a host's mark when its system has none (or is not known
/// yet), as `0xRRGGBB`: the same for an address every time, on every machine
/// and in every version, so a host keeps its colour. Blanks around it and
/// its case do not count: `COM3` is `com3`, and a host name is a host name
/// in any case.
pub fn mark_color(address: &str) -> u32 {
    MARK_COLORS[mark_index(address)]
}

/// Which of [`MARK_COLORS`] `address` gets.
fn mark_index(address: &str) -> usize {
    // FNV-1a: std's hashers may change between Rust versions, and with them
    // every host's colour.
    let hash = address
        .trim()
        .to_lowercase()
        .bytes()
        .fold(0x811C_9DC5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
    hash as usize % MARK_COLORS.len()
}

/// WCAG relative luminance of `0xRRGGBB`.
pub(crate) fn luminance(hex: u32) -> f32 {
    let linear = |shift: u32| {
        let channel = ((hex >> shift) & 0xFF) as f32 / 255.0;
        if channel <= 0.03928 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0)
}

#[cfg(test)]
mod tests {
    use super::{MARK_COLORS, luminance, mark_color, mark_index};

    /// HSL saturation of `0xRRGGBB`.
    fn saturation(hex: u32) -> f32 {
        let channels = [16, 8, 0].map(|shift| ((hex >> shift) & 0xFF) as f32 / 255.0);
        let max = channels.into_iter().fold(0.0, f32::max);
        let min = channels.into_iter().fold(1.0, f32::min);
        let lightness = (max + min) / 2.0;
        if max == min {
            0.0
        } else {
            (max - min) / (1.0 - (2.0 * lightness - 1.0).abs())
        }
    }

    #[test]
    fn an_address_keeps_its_colour_whatever_its_case_and_blanks() {
        assert_eq!(mark_color("10.0.1.12"), mark_color(" 10.0.1.12 "));
        assert_eq!(
            mark_color("Web-01.Example.com"),
            mark_color("web-01.example.com")
        );
        assert_eq!(mark_color("COM3"), mark_color("com3"));
        // FNV-1a of the address, fixed for good: a host keeps its colour
        // across versions.
        assert_eq!(mark_index(""), 0x811C_9DC5 % MARK_COLORS.len());
        assert_eq!(mark_index("a"), 0xE40C_292C % MARK_COLORS.len());
    }

    #[test]
    fn every_colour_is_vivid_and_carries_a_white_glyph() {
        for hex in MARK_COLORS {
            assert!(saturation(hex) >= 0.5, "{hex:06x} is too grey");
            let contrast = 1.05 / (luminance(hex) + 0.05);
            assert!(contrast >= 4.5, "{hex:06x}: {contrast}");
        }
        let mut unique = MARK_COLORS.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), MARK_COLORS.len());
    }

    #[test]
    fn neighbouring_addresses_spread_over_the_colours() {
        let mut used: Vec<usize> = (1..=50)
            .map(|host| mark_index(&format!("10.0.0.{host}")))
            .collect();
        used.sort_unstable();
        used.dedup();
        assert!(used.len() > MARK_COLORS.len() / 2, "{}", used.len());
    }
}
