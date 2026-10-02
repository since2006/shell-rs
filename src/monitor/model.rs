//! What the monitor reads off a host, and the shares and rates it shows.

use std::time::{Duration, Instant};

use crate::shared::format_bytes;

/// One look at a host: its counters at that moment, and the parts read
/// less often when this reading asked for them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Read once per connection.
    pub system: Option<SystemInfo>,
    pub uptime: Option<Duration>,
    /// CPU time since boot: the whole machine, then each core.
    pub cpu: CpuTimes,
    pub cores: Vec<CpuTimes>,
    pub memory: Memory,
    pub interfaces: Vec<InterfaceCounters>,
    /// The interface the default route goes out of: the host's main one.
    pub default_route: Option<String>,
    /// Read every half minute or so.
    pub disks: Option<Vec<DiskUsage>>,
}

/// What does not change while connected.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemInfo {
    pub host_name: String,
    pub arch: String,
    /// The distribution's own name, 「Debian GNU/Linux 12 (bookworm)」.
    pub os: String,
    pub cpu_model: String,
}

/// CPU time since boot, in the kernel's ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub idle: u64,
}

/// In bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

impl Memory {
    pub fn percent(&self) -> f32 {
        share(self.used, self.total)
    }

    pub fn swap_percent(&self) -> f32 {
        share(self.swap_used, self.swap_total)
    }
}

/// Bytes through a network interface since it came up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InterfaceCounters {
    pub name: String,
    pub received: u64,
    pub transmitted: u64,
}

/// A mounted file system, in bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskUsage {
    pub mount: String,
    pub total: u64,
    pub used: u64,
    pub available: u64,
}

impl DiskUsage {
    /// As `df` counts it: the share taken of the space ordinary users can
    /// have, which leaves out what is reserved for root.
    pub fn percent(&self) -> f32 {
        share(self.used, self.used + self.available)
    }
}

fn share(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.
    } else {
        (part as f64 * 100. / whole as f64) as f32
    }
}

/// The load the panel shows: a reading, and how it moved since the one
/// before.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub uptime: Option<Duration>,
    pub cores: usize,
    /// Percent busy since the reading before; `None` for the first.
    pub cpu: Option<CpuLoad>,
    pub memory: Memory,
    pub interfaces: Vec<InterfaceRate>,
    pub default_route: Option<String>,
}

impl Snapshot {
    /// The interfaces worth a row: not loopback, and not one that never
    /// carried a byte.
    pub fn busy_interfaces(&self) -> Vec<&InterfaceRate> {
        self.interfaces
            .iter()
            .filter(|interface| !interface.idle)
            .collect()
    }

    /// The host's main interface, the one shown while the rest are folded
    /// away: where the default route goes out, or else the one that carried
    /// the most.
    pub fn main_interface(&self) -> Option<&InterfaceRate> {
        let busy = self.busy_interfaces();
        busy.iter()
            .find(|interface| Some(&interface.name) == self.default_route.as_ref())
            .or_else(|| busy.iter().max_by_key(|interface| interface.bytes))
            .copied()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CpuLoad {
    pub total: f32,
    pub cores: Vec<f32>,
}

/// Bytes per second through a network interface since the reading before;
/// `None` for the first.
#[derive(Clone, Debug, PartialEq)]
pub struct InterfaceRate {
    pub name: String,
    pub received: Option<f64>,
    pub transmitted: Option<f64>,
    /// Bytes both ways since the interface came up.
    pub bytes: u64,
    /// Loopback, or never carried a byte: not worth a row.
    pub idle: bool,
}

/// Readings further apart than this are no base for rates: what they would
/// show is an average over minutes, not what the host is doing.
const STALE: Duration = Duration::from_secs(10);

/// Turns readings into snapshots. Rates need the reading before, which is
/// what this keeps.
#[derive(Default)]
pub struct Tracker {
    last: Option<(Reading, Instant)>,
}

impl Tracker {
    pub fn update(&mut self, reading: Reading, at: Instant) -> Snapshot {
        let before = self.last.take().and_then(|(reading, then)| {
            let elapsed = at.checked_duration_since(then)?;
            (!elapsed.is_zero() && elapsed <= STALE).then_some((reading, elapsed))
        });
        let snapshot = snapshot(&reading, before.as_ref().map(|(r, e)| (r, *e)));
        self.last = Some((reading, at));
        snapshot
    }
}

fn snapshot(reading: &Reading, before: Option<(&Reading, Duration)>) -> Snapshot {
    let cpu = before.and_then(|(before, _)| {
        let total = busy_percent(reading.cpu, before.cpu)?;
        // Per core only when the cores are the same ones as before.
        let cores = if reading.cores.len() == before.cores.len() {
            reading
                .cores
                .iter()
                .zip(&before.cores)
                .map(|(now, then)| busy_percent(*now, *then).unwrap_or(0.))
                .collect()
        } else {
            Vec::new()
        };
        Some(CpuLoad { total, cores })
    });
    let interfaces = reading
        .interfaces
        .iter()
        .map(|interface| {
            let then = before.and_then(|(before, elapsed)| {
                let then = before
                    .interfaces
                    .iter()
                    .find(|then| then.name == interface.name)?;
                Some((then, elapsed.as_secs_f64()))
            });
            // A counter that went down was reset: the interface came up again.
            let rate = |now: u64, then: u64, seconds: f64| {
                now.checked_sub(then).map(|bytes| bytes as f64 / seconds)
            };
            InterfaceRate {
                name: interface.name.clone(),
                received: then
                    .and_then(|(then, seconds)| rate(interface.received, then.received, seconds)),
                transmitted: then.and_then(|(then, seconds)| {
                    rate(interface.transmitted, then.transmitted, seconds)
                }),
                bytes: interface.received + interface.transmitted,
                idle: interface.name == "lo"
                    || (interface.received == 0 && interface.transmitted == 0),
            }
        })
        .collect();
    Snapshot {
        uptime: reading.uptime,
        cores: reading.cores.len(),
        cpu,
        memory: reading.memory,
        interfaces,
        default_route: reading.default_route.clone(),
    }
}

/// `None` when the counters went down, which they only do across a reboot.
fn busy_percent(now: CpuTimes, then: CpuTimes) -> Option<f32> {
    let busy = now.busy.checked_sub(then.busy)?;
    let idle = now.idle.checked_sub(then.idle)?;
    let total = busy + idle;
    (total > 0).then(|| (busy as f64 * 100. / total as f64) as f32)
}

/// How hard a resource is worked, for its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    Normal,
    High,
    Critical,
}

impl Load {
    pub fn of(percent: f32) -> Self {
        if percent >= 90. {
            Load::Critical
        } else if percent >= 70. {
            Load::High
        } else {
            Load::Normal
        }
    }
}

/// 「5.44 KB/s」.
pub fn format_rate(bytes_per_second: f64) -> String {
    format!(
        "{}/s",
        format_bytes(bytes_per_second.max(0.).round() as u64)
    )
}

/// How many of the per-core bars fit on a row `width` wide, at the
/// interface's `rem`: bars are 0.375 rem with 0.125 rem between.
pub fn bars_per_row(width: f32, rem: f32) -> usize {
    let (bar, gap) = (0.375 * rem, 0.125 * rem);
    // Layout rounds; half a pixel to spare keeps a bar that fits exactly.
    (((width + gap + 0.5) / (bar + gap)).floor() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(cpu: (u64, u64), received: u64) -> Reading {
        Reading {
            cpu: CpuTimes {
                busy: cpu.0,
                idle: cpu.1,
            },
            cores: vec![CpuTimes {
                busy: cpu.0,
                idle: cpu.1,
            }],
            interfaces: vec![
                InterfaceCounters {
                    name: "eth0".into(),
                    received,
                    transmitted: received / 2,
                },
                InterfaceCounters {
                    name: "lo".into(),
                    received: 10,
                    transmitted: 10,
                },
            ],
            ..Reading::default()
        }
    }

    #[test]
    fn the_first_reading_has_no_rates_and_the_next_one_does() {
        let mut tracker = Tracker::default();
        let start = Instant::now();
        let first = tracker.update(reading((100, 900), 1_000), start);
        assert_eq!(first.cpu, None);
        assert_eq!(first.interfaces[0].received, None);

        // 25 busy ticks of 100, and 4 KB in 2 seconds.
        let second = tracker.update(
            reading((125, 975), 1_000 + 4096),
            start + Duration::from_secs(2),
        );
        let cpu = second.cpu.expect("a load against the first reading");
        assert_eq!(cpu.total, 25.);
        assert_eq!(cpu.cores, vec![25.]);
        assert_eq!(second.interfaces[0].received, Some(2048.));
        assert_eq!(second.interfaces[0].transmitted, Some(1024.));
        assert!(!second.interfaces[0].idle);
        assert!(second.interfaces[1].idle, "loopback is never worth a row");
    }

    #[test]
    fn the_main_interface_is_the_default_routes_or_else_the_busiest() {
        let interface = |name: &str, bytes: u64| InterfaceRate {
            name: name.into(),
            received: None,
            transmitted: None,
            bytes,
            idle: name == "lo" || bytes == 0,
        };
        let mut snapshot = Snapshot {
            uptime: None,
            cores: 1,
            cpu: None,
            memory: Memory::default(),
            interfaces: vec![
                interface("lo", 900),
                interface("docker0", 5000),
                interface("eth0", 3000),
                interface("wg0", 0),
            ],
            default_route: Some("eth0".into()),
        };
        let names = |snapshot: &Snapshot| {
            snapshot
                .busy_interfaces()
                .iter()
                .map(|interface| interface.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&snapshot), ["docker0", "eth0"]);
        assert_eq!(snapshot.main_interface().unwrap().name, "eth0");
        // No default route, or one through an interface not worth a row.
        snapshot.default_route = Some("wg0".into());
        assert_eq!(snapshot.main_interface().unwrap().name, "docker0");
        snapshot.default_route = None;
        assert_eq!(snapshot.main_interface().unwrap().name, "docker0");
    }

    #[test]
    fn a_reading_long_after_the_last_starts_over() {
        let mut tracker = Tracker::default();
        let start = Instant::now();
        tracker.update(reading((100, 900), 0), start);
        let later = tracker.update(reading((200, 1800), 0), start + Duration::from_secs(60));
        assert_eq!(later.cpu, None);
    }

    #[test]
    fn counters_that_went_down_give_no_rate() {
        let mut tracker = Tracker::default();
        let start = Instant::now();
        tracker.update(reading((500, 5000), 9_000), start);
        let rebooted = tracker.update(reading((10, 90), 100), start + Duration::from_secs(2));
        assert_eq!(rebooted.cpu, None);
        assert_eq!(rebooted.interfaces[0].received, None);
    }

    #[test]
    fn disks_count_like_df_without_the_reserved_space() {
        let disk = DiskUsage {
            mount: "/".into(),
            total: 100,
            used: 33,
            available: 62,
        };
        assert!((disk.percent() - 34.7).abs() < 0.1);
    }

    #[test]
    fn rates_read_naturally() {
        assert_eq!(format_rate(5570.), "5.44 KB/s");
    }

    #[test]
    fn a_row_of_the_usual_width_holds_twenty_three_cores() {
        // The column beside the gauge in the default 320 px sidebar.
        assert_eq!(bars_per_row(184., 16.), 23);
        assert_eq!(bars_per_row(183.9, 16.), 23);
        assert_eq!(bars_per_row(400., 16.), 50);
        assert_eq!(bars_per_row(0., 16.), 1);
    }

    #[test]
    fn load_goes_high_at_seventy_and_critical_at_ninety() {
        assert_eq!(Load::of(69.9), Load::Normal);
        assert_eq!(Load::of(70.), Load::High);
        assert_eq!(Load::of(90.), Load::Critical);
    }
}
