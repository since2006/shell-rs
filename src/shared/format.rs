//! How sizes, shares and durations read, the same in every tool.

use std::time::Duration;

use crate::i18n::tn;

const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];

/// 「421.02 MB」, in units of 1024.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    format!("{value:.2} {}", UNITS[unit])
}

/// 「43.3%」.
pub fn format_percent(percent: f32) -> String {
    format!("{percent:.1}%")
}

/// 「32 天 23:40:26」, or 「23:40:26」 within the first day: how long a host
/// or a process has been up, or the CPU time a process has used.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let days = seconds / 86_400;
    let clock = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    );
    if days > 0 {
        tn!("shared.duration.days", days, clock = clock).to_string()
    } else {
        clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_shares_read_naturally() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(441_470_976), "421.02 MB");
        assert_eq!(format_bytes(9_889_644_544), "9.21 GB");
        assert_eq!(format_percent(43.27), "43.3%");
    }

    #[test]
    fn durations_read_as_days_and_a_clock() {
        assert_eq!(
            format_duration(Duration::from_secs(32 * 86_400 + 23 * 3600 + 40 * 60 + 26)),
            "32 天 23:40:26"
        );
        assert_eq!(
            format_duration(Duration::from_secs_f64(5. * 3600. + 12. * 60. + 9.7)),
            "05:12:09"
        );
        assert_eq!(format_duration(Duration::from_secs(30)), "00:00:30");
    }
}
