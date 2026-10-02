//! Reading a Linux host: one command, and the parser for what it prints.

use std::collections::HashMap;
use std::time::Duration;

use super::model::{CpuTimes, DiskUsage, InterfaceCounters, Memory, Reading, SystemInfo};

/// Which parts a reading asks for besides the load, which every reading
/// does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    /// Host name, architecture, distribution and CPU model: once per
    /// connection, as they do not change while connected.
    pub system: bool,
    /// Every half minute or so: disks fill slowly, and `df` is the slowest
    /// part of a reading.
    pub disks: bool,
}

/// The parts asked once per connection.
const SYSTEM: &str = "echo @@host; uname -n; echo @@arch; uname -m; \
echo @@os; (. /etc/os-release && echo \"$PRETTY_NAME\") 2>/dev/null || uname -sr; \
echo @@cpu; grep -E \"^(model name|Hardware|Processor|cpu model)\" /proc/cpuinfo | head -n 1; ";

/// The load, every reading. Shell built-ins only (`read`, `case`, `echo`),
/// so the one process a reading starts on the host is the `sh` running it;
/// `/proc/stat` and `/proc/meminfo` are read only as far as the lines it
/// needs, which come first.
const LOAD: &str = "echo @@uptime; read -r line < /proc/uptime; echo \"$line\"; \
echo @@stat; while read -r line; do case \"$line\" in cpu*) echo \"$line\";; *) break;; esac; \
done < /proc/stat; \
echo @@memory; while read -r line; do case \"$line\" in \
MemTotal:*|MemFree:*|MemAvailable:*|Buffers:*|Cached:*|SwapTotal:*) echo \"$line\";; \
SwapFree:*) echo \"$line\"; break;; esac; done < /proc/meminfo; \
echo @@net; while read -r line; do echo \"$line\"; done < /proc/net/dev; \
echo @@route; while read -r line; do echo \"$line\"; done < /proc/net/route; ";

/// `df -l` keeps to local file systems, so a dead network mount cannot hold
/// it up; a `df` without `-l` (BusyBox) runs plain.
const DISKS: &str = "echo @@df; df -kPl 2>/dev/null || df -kP 2>/dev/null; ";

/// What the monitor runs on the host for a reading: `/proc` and `df`, each
/// part under an `@@` line of its own.
///
/// One line, in single quotes for `sh -c`, so that whatever the login shell
/// is (bash, zsh, fish, csh) it hands the script to `sh` untouched: the
/// script has no single quote, and no `!` for csh to expand.
pub fn command(parts: Parts) -> String {
    format!(
        "sh -c 'export LC_ALL=C; if test -r /proc/stat; then {}{}{}\
         else echo @@unsupported; uname -s; fi'",
        if parts.system { SYSTEM } else { "" },
        LOAD,
        if parts.disks { DISKS } else { "" },
    )
}

/// What the command's output said.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Reading(Box<Reading>),
    /// Not a Linux host. Holds `uname -s` when the host has one: 「Darwin」.
    Unsupported(Option<String>),
}

pub fn parse(output: &str) -> Parsed {
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current = None;
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("@@") {
            current = Some(name.trim());
            sections.entry(name.trim()).or_default();
        } else if let Some(name) = current {
            sections.entry(name).or_default().push(line);
        }
    }
    let section = |name: &str| sections.get(name).map(Vec::as_slice).unwrap_or_default();
    let first = |name: &str| {
        section(name)
            .iter()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
            .map(str::to_owned)
    };

    let (cpu, cores) = parse_stat(section("stat"));
    let Some(cpu) = cpu else {
        // A shell other than sh's (cmd.exe) prints nothing we know.
        return Parsed::Unsupported(first("unsupported"));
    };
    Parsed::Reading(Box::new(Reading {
        // Only when the reading asked for it.
        system: sections.contains_key("host").then(|| SystemInfo {
            host_name: first("host").unwrap_or_default(),
            arch: first("arch").unwrap_or_default(),
            os: first("os").unwrap_or_else(|| "Linux".into()),
            cpu_model: first("cpu")
                .and_then(|line| Some(line.split_once(':')?.1.trim().to_owned()))
                .unwrap_or_default(),
        }),
        uptime: first("uptime")
            .and_then(|line| line.split_whitespace().next()?.parse::<f64>().ok())
            .map(Duration::from_secs_f64),
        cpu,
        cores,
        memory: parse_memory(section("memory")),
        interfaces: parse_interfaces(section("net")),
        default_route: parse_default_route(section("route")),
        disks: sections
            .contains_key("df")
            .then(|| parse_disks(section("df"))),
    }))
}

/// `/proc/stat`'s `cpu` lines: the machine, then each core.
fn parse_stat(lines: &[&str]) -> (Option<CpuTimes>, Vec<CpuTimes>) {
    let mut total = None;
    let mut cores = Vec::new();
    for line in lines {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        // user nice system idle iowait irq softirq steal; guest time is
        // already counted in user.
        let ticks: Vec<u64> = fields
            .take(8)
            .filter_map(|field| field.parse().ok())
            .collect();
        if ticks.len() < 4 {
            continue;
        }
        let tick = |index: usize| ticks.get(index).copied().unwrap_or(0);
        let times = CpuTimes {
            busy: tick(0) + tick(1) + tick(2) + tick(5) + tick(6) + tick(7),
            idle: tick(3) + tick(4),
        };
        if name == "cpu" {
            total = Some(times);
        } else if name.starts_with("cpu") {
            cores.push(times);
        }
    }
    (total, cores)
}

/// `/proc/meminfo`, whose sizes are in KiB.
fn parse_memory(lines: &[&str]) -> Memory {
    let values: HashMap<&str, u64> = lines
        .iter()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let kib = value.split_whitespace().next()?.parse::<u64>().ok()?;
            Some((name.trim(), kib * 1024))
        })
        .collect();
    let value = |name: &str| values.get(name).copied().unwrap_or(0);
    let total = value("MemTotal");
    // Kernels before 3.14 have no MemAvailable; free, buffers and cache come
    // closest.
    let available = values
        .get("MemAvailable")
        .copied()
        .unwrap_or_else(|| value("MemFree") + value("Buffers") + value("Cached"));
    Memory {
        total,
        used: total.saturating_sub(available),
        swap_total: value("SwapTotal"),
        swap_used: value("SwapTotal").saturating_sub(value("SwapFree")),
    }
}

/// `/proc/net/dev`: received bytes first, transmitted bytes ninth.
fn parse_interfaces(lines: &[&str]) -> Vec<InterfaceCounters> {
    lines
        .iter()
        .filter_map(|line| {
            let (name, counters) = line.split_once(':')?;
            let counters: Vec<u64> = counters
                .split_whitespace()
                .map(|field| field.parse().ok())
                .collect::<Option<_>>()?;
            Some(InterfaceCounters {
                name: name.trim().to_owned(),
                received: *counters.first()?,
                transmitted: *counters.get(8)?,
            })
        })
        .collect()
}

/// `/proc/net/route`: the interface of the route to everywhere (destination
/// and mask all zeros), the lowest metric when there are several.
fn parse_default_route(lines: &[&str]) -> Option<String> {
    lines
        .iter()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (name, destination, metric, mask) = (
                fields.first()?,
                fields.get(1)?,
                fields.get(6)?,
                fields.get(7)?,
            );
            (*destination == "00000000" && *mask == "00000000")
                .then(|| (metric.parse::<u32>().unwrap_or(u32::MAX), name.to_string()))
        })
        .min()
        .map(|(_, name)| name)
}

/// `df -kP`, without what is not a disk: memory file systems, the kernel's
/// own, snaps and container layers.
fn parse_disks(lines: &[&str]) -> Vec<DiskUsage> {
    let mut disks: Vec<DiskUsage> = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let kib = |index: usize| fields[index].parse::<u64>().ok().map(|kib| kib * 1024);
        let (Some(total), Some(used), Some(available)) = (kib(1), kib(2), kib(3)) else {
            // The heading.
            continue;
        };
        // A mount point can hold spaces.
        let mount = fields[5..].join(" ");
        if total == 0 || !is_disk(fields[0], &mount) || disks.iter().any(|d| d.mount == mount) {
            continue;
        }
        disks.push(DiskUsage {
            mount,
            total,
            used,
            available,
        });
    }
    disks
}

fn is_disk(source: &str, mount: &str) -> bool {
    const NOT_DISKS: [&str; 8] = [
        "tmpfs", "devtmpfs", "udev", "overlay", "shm", "none", "run", "efivarfs",
    ];
    const SYSTEM_MOUNTS: [&str; 6] = [
        "/dev",
        "/run",
        "/sys",
        "/proc",
        "/snap/",
        "/var/lib/docker/",
    ];
    !NOT_DISKS.contains(&source)
        && !source.starts_with("/dev/loop")
        && !SYSTEM_MOUNTS.iter().any(|prefix| mount.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a small Debian VPS answers.
    const DEBIAN: &str = "\
@@host
VM236A61A0D4D4D22
@@arch
x86_64
@@os
Debian GNU/Linux 12 (bookworm)
@@uptime
5359162.33 5293480.12
@@cpu
model name\t: AMD EPYC-Milan Processor
@@stat
cpu  4705 356 584 3699176 2311 0 158 77 0 0
cpu0 4705 356 584 3699176 2311 0 158 77 0 0
@@memory
MemTotal:         996608 kB
MemFree:          120000 kB
MemAvailable:     565480 kB
Buffers:           40000 kB
Cached:           400000 kB
SwapTotal:       1048572 kB
SwapFree:         996144 kB
@@net
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:   12345      67    0    0    0     0          0         0    12345      67    0    0    0     0       0          0
  eth0: 987654321  1000    0    0    0     0          0         0 123456789   900    0    0    0     0       0          0
docker0:       0       0    0    0    0     0          0         0        0     0    0    0    0     0       0          0
@@route
Iface	Destination	Gateway 	Flags	RefCnt	Use	Metric	Mask		MTU	Window	IRTT
eth0	00000000	0101A8C0	0003	0	0	100	00000000	0	0	0
wg0	00000000	00000000	0001	0	0	50	80000000	0	0	0
eth0	0001A8C0	00000000	0001	0	0	100	00FFFFFF	0	0	0
@@df
Filesystem     1024-blocks    Used Available Capacity Mounted on
udev                483152       0    483152       0% /dev
tmpfs                99664     560     99104       1% /run
/dev/vda1          9656904 3240180   5994708      36% /
tmpfs               498304       0    498304       0% /dev/shm
/dev/vda15          126678   12068    114610      10% /boot/efi
/dev/loop0           65536   65536         0     100% /snap/core20/2105
/dev/sdb1         20511312  102400  19343952       1% /mnt/My Data
";

    #[test]
    fn a_debian_host_reads_whole() {
        let Parsed::Reading(reading) = parse(DEBIAN) else {
            panic!("a Linux host is read");
        };
        assert_eq!(
            reading.system,
            Some(SystemInfo {
                host_name: "VM236A61A0D4D4D22".into(),
                arch: "x86_64".into(),
                os: "Debian GNU/Linux 12 (bookworm)".into(),
                cpu_model: "AMD EPYC-Milan Processor".into(),
            })
        );
        assert_eq!(reading.uptime, Some(Duration::from_secs_f64(5359162.33)));
        assert_eq!(
            reading.cpu,
            CpuTimes {
                busy: 4705 + 356 + 584 + 158 + 77,
                idle: 3699176 + 2311,
            }
        );
        assert_eq!(reading.cores.len(), 1);
        assert_eq!(reading.memory.total, 996608 * 1024);
        assert_eq!(reading.memory.used, (996608 - 565480) * 1024);
        assert_eq!(reading.memory.swap_used, (1048572 - 996144) * 1024);
        let names: Vec<_> = reading.interfaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["lo", "eth0", "docker0"]);
        assert_eq!(reading.interfaces[1].received, 987654321);
        assert_eq!(reading.interfaces[1].transmitted, 123456789);
        // wg0's route covers half the addresses only.
        assert_eq!(reading.default_route.as_deref(), Some("eth0"));
        let disks = reading.disks.expect("asked for");
        let mounts: Vec<_> = disks.iter().map(|d| d.mount.as_str()).collect();
        assert_eq!(mounts, ["/", "/boot/efi", "/mnt/My Data"]);
        assert_eq!(disks[0].used, 3240180 * 1024);
    }

    #[test]
    fn an_old_kernel_without_mem_available_counts_free_buffers_and_cache() {
        let memory = parse_memory(&[
            "MemTotal: 1000 kB",
            "MemFree: 100 kB",
            "Buffers: 50 kB",
            "Cached: 250 kB",
        ]);
        assert_eq!(memory.used, 600 * 1024);
        assert_eq!(memory.swap_total, 0);
    }

    #[test]
    fn a_host_without_proc_says_what_it_is() {
        assert_eq!(
            parse("@@unsupported\nDarwin\n"),
            Parsed::Unsupported(Some("Darwin".into()))
        );
        // cmd.exe has nothing to say to `sh -c`.
        assert_eq!(parse(""), Parsed::Unsupported(None));
    }

    /// Every way a reading can be asked for.
    fn commands() -> Vec<(Parts, String)> {
        [(false, false), (false, true), (true, false), (true, true)]
            .into_iter()
            .map(|(system, disks)| {
                let parts = Parts { system, disks };
                (parts, command(parts))
            })
            .collect()
    }

    fn script(command: &str) -> &str {
        command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .expect("one single-quoted script")
    }

    #[test]
    fn the_commands_survive_any_login_shell() {
        for (_, command) in commands() {
            // Inside the single quotes there is nothing for the login shell.
            let script = script(&command);
            assert!(!script.contains('\''), "{command}");
            assert!(!script.contains('!'), "{command}");
            assert!(!script.contains('\n'), "{command}");
        }
    }

    #[test]
    fn a_reading_asks_only_for_its_parts() {
        let light = command(Parts {
            system: false,
            disks: false,
        });
        assert!(light.contains("@@stat"));
        assert!(!light.contains("@@host"));
        assert!(!light.contains("@@df"));
        // The load starts no process of its own.
        for program in ["cat ", "grep ", "head ", "uname -n", "df "] {
            assert!(!light.contains(program), "{program} in {light}");
        }
        let full = command(Parts {
            system: true,
            disks: true,
        });
        assert!(full.contains("@@host") && full.contains("@@df"));
    }

    /// The script run for real by `sh`, against `/proc` files laid out in a
    /// directory of the test's own: the built-ins must read what `cat` and
    /// `grep` would have.
    #[cfg(unix)]
    #[test]
    fn the_script_reads_proc_with_shell_built_ins() {
        let directory = tempfile::tempdir().unwrap();
        let proc = directory.path().join("proc");
        std::fs::create_dir_all(proc.join("net")).unwrap();
        let files = [
            ("uptime", "5359162.33 5293480.12\n"),
            (
                "cpuinfo",
                "processor\t: 0\nmodel name\t: AMD EPYC-Milan Processor\nflags\t: fpu\n",
            ),
            (
                "stat",
                "cpu  4705 356 584 3699176 2311 0 158 77 0 0\n\
                 cpu0 4705 356 584 3699176 2311 0 158 77 0 0\n\
                 intr 123 4 5 6\nctxt 999\ncpu9 1 1 1 1 1 1 1 1 0 0\n",
            ),
            (
                "meminfo",
                "MemTotal:         996608 kB\nMemFree:          120000 kB\n\
                 MemAvailable:     565480 kB\nBuffers:           40000 kB\n\
                 Cached:           400000 kB\nSwapCached:            0 kB\n\
                 SwapTotal:       1048572 kB\nSwapFree:         996144 kB\n\
                 MemTotal:              1 kB\n",
            ),
            (
                "net/route",
                "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
                 eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\n",
            ),
            (
                "net/dev",
                "Inter-|   Receive |  Transmit\n face |bytes packets|bytes packets\n\
                 \x20   lo: 10 1 0 0 0 0 0 0 10 1 0 0 0 0 0 0\n\
                 \x20 eth0: 987654321 1000 0 0 0 0 0 0 123456789 900 0 0 0 0 0 0\n",
            ),
        ];
        for (name, content) in files {
            std::fs::write(proc.join(name), content).unwrap();
        }
        let full = command(Parts {
            system: true,
            disks: false,
        });
        let script = script(&full).replace("/proc/", &format!("{}/", proc.display()));
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .expect("sh runs");
        let output = String::from_utf8(output.stdout).unwrap();

        let Parsed::Reading(reading) = parse(&output) else {
            panic!("read as Linux: {output}");
        };
        let system = reading.system.expect("asked for");
        assert_eq!(system.cpu_model, "AMD EPYC-Milan Processor");
        assert_eq!(reading.uptime, Some(Duration::from_secs_f64(5359162.33)));
        // The cpu lines at the top, and nothing after the first other line.
        assert_eq!(reading.cores.len(), 1);
        assert_eq!(reading.cpu.idle, 3699176 + 2311);
        // Up to SwapFree, and no further.
        assert_eq!(reading.memory.total, 996608 * 1024);
        assert_eq!(reading.memory.used, (996608 - 565480) * 1024);
        assert_eq!(reading.memory.swap_used, (1048572 - 996144) * 1024);
        let eth0 = &reading.interfaces[1];
        assert_eq!(
            (eth0.name.as_str(), eth0.received, eth0.transmitted),
            ("eth0", 987654321, 123456789)
        );
        assert_eq!(reading.default_route.as_deref(), Some("eth0"));
        assert_eq!(reading.disks, None, "not asked for");
    }

    #[cfg(unix)]
    #[test]
    fn every_command_is_valid_sh() {
        for (parts, command) in commands() {
            let status = std::process::Command::new("sh")
                .args(["-n", "-c", script(&command)])
                .status()
                .expect("sh runs");
            assert!(status.success(), "{parts:?}: {command}");
        }
    }
}
