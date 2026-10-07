//! What 进程管理 reads off a host, and how it sorts and searches the list.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use gpui_kit::SharedString;

use crate::i18n::t;

/// One look at a host's processes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    /// Seconds since boot, the clock the CPU times are read against.
    pub uptime: f64,
    /// Physical memory, in bytes.
    pub memory_total: u64,
    /// Clock ticks a second, which CPU times and start times count in.
    pub ticks_per_second: u64,
    pub processes: Vec<ProcessTimes>,
    /// Whether the host has more processes than the reading lists.
    pub truncated: bool,
}

/// A process as `/proc/<pid>/stat` has it.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessTimes {
    pub pid: u32,
    /// The executable's name, cut to 15 bytes by the kernel.
    pub name: String,
    pub state: ProcessState,
    /// The name, or the uid when the host has no name for it.
    pub user: Option<String>,
    /// When it started, in ticks since boot.
    pub started: u64,
    /// CPU time used, user and system, in ticks.
    pub cpu: u64,
    /// Resident memory, in bytes.
    pub memory: u64,
    pub info: ProcessInfo,
}

/// What a process's details say besides its times and memory, as
/// `/proc/<pid>/stat` has it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessInfo {
    /// The parent's PID; 0 for the first process.
    pub parent: u32,
    /// `ps`'s state letters: 「Sl」 asleep with threads, 「Ss+」 a session
    /// leader in the foreground of its terminal.
    pub letters: String,
    /// The controlling terminal: 「pts/0」, `None` without one.
    pub terminal: Option<String>,
    /// The kernel's priority, as `top` shows it: 20 at nice 0.
    pub priority: i64,
    pub nice: i64,
    pub threads: u64,
    /// Virtual memory, in bytes.
    pub virtual_memory: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    Sleeping,
    /// Waiting on the disk or the like, and deaf to signals meanwhile.
    Uninterruptible,
    Stopped,
    Zombie,
    /// A kernel worker with nothing to do.
    Idle,
    Other,
}

impl ProcessState {
    /// The state's letter in `/proc/<pid>/stat`.
    pub fn from_letter(letter: &str) -> Self {
        match letter {
            "R" => ProcessState::Running,
            "S" => ProcessState::Sleeping,
            "D" => ProcessState::Uninterruptible,
            "T" | "t" => ProcessState::Stopped,
            "Z" => ProcessState::Zombie,
            "I" => ProcessState::Idle,
            _ => ProcessState::Other,
        }
    }

    pub fn label(self) -> SharedString {
        match self {
            ProcessState::Running => t!("processes.state.running"),
            ProcessState::Sleeping => t!("processes.state.sleeping"),
            ProcessState::Uninterruptible => t!("processes.state.uninterruptible"),
            ProcessState::Stopped => t!("processes.state.stopped"),
            ProcessState::Zombie => t!("processes.state.zombie"),
            ProcessState::Idle => t!("processes.state.idle"),
            ProcessState::Other => t!("processes.state.misc"),
        }
    }
}

/// A process as the list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub state: ProcessState,
    pub user: Option<String>,
    /// When it started, as seconds since the Unix epoch.
    pub started: i64,
    pub memory: u64,
    pub memory_percent: f32,
    /// Its share of one core since the last reading, the way `top` counts
    /// it: a process busy on two cores is at 200%. `None` on the first
    /// reading of it.
    pub cpu: Option<f32>,
    /// CPU time used since it started.
    pub cpu_time: Duration,
    /// How long it has been running.
    pub running: Duration,
    pub info: ProcessInfo,
}

/// A process with what its details say about its family.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessDetails {
    pub process: Process,
    /// The parent: 「systemd (1)」, or 「PID 1」 when the list does not have
    /// it; `None` for the first process, which has none.
    pub parent: Option<String>,
    /// The processes it started.
    pub children: usize,
    /// Those, the processes they started, and so on down.
    pub descendants: usize,
}

impl Process {
    /// What the search looks in, lowercased.
    fn haystack(&self) -> String {
        format!(
            "{} {} {}",
            self.name,
            self.pid,
            self.user.as_deref().unwrap_or_default()
        )
        .to_lowercase()
    }
}

/// The host's processes at one reading, with what changed since the last.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    processes: Vec<Process>,
    /// Lowercased, beside each process.
    haystacks: Vec<String>,
    truncated: bool,
}

impl Snapshot {
    pub fn processes(&self) -> &[Process] {
        &self.processes
    }

    pub fn process(&self, pid: u32) -> Option<&Process> {
        self.processes.iter().find(|process| process.pid == pid)
    }

    /// A process with its parent named and its children counted, from
    /// the list.
    pub fn details(&self, pid: u32) -> Option<ProcessDetails> {
        let process = self.process(pid)?.clone();
        let parent = match process.info.parent {
            0 => None,
            parent => Some(match self.process(parent) {
                Some(found) => format!("{} ({parent})", found.name),
                None => format!("PID {parent}"),
            }),
        };
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for each in &self.processes {
            children.entry(each.info.parent).or_default().push(each.pid);
        }
        let direct = children.get(&pid).map_or(0, Vec::len);
        // Down the tree; a PID seen twice (a list read mid-change) counts
        // once.
        let mut seen = HashSet::from([pid]);
        let mut next = vec![pid];
        while let Some(parent) = next.pop() {
            for &child in children.get(&parent).into_iter().flatten() {
                if seen.insert(child) {
                    next.push(child);
                }
            }
        }
        Some(ProcessDetails {
            process,
            parent,
            children: direct,
            descendants: seen.len() - 1,
        })
    }

    /// Whether the host has more processes than the reading listed.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The processes `query` finds in their name, PID or user, ignoring
    /// case, in `order`: by their place in `processes()`.
    pub fn listed(&self, query: &str, order: Order) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let mut listed: Vec<usize> = self
            .haystacks
            .iter()
            .enumerate()
            .filter(|(_, haystack)| query.is_empty() || haystack.contains(&query))
            .map(|(index, _)| index)
            .collect();
        listed.sort_by(|&a, &b| order.compare(&self.processes[a], &self.processes[b]));
        listed
    }
}

/// What the list is sorted by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProcessSort {
    #[default]
    Memory,
    Cpu,
}

/// What the list is sorted by, and which way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order {
    pub by: ProcessSort,
    /// The most first.
    pub descending: bool,
}

impl Default for Order {
    fn default() -> Self {
        Self {
            by: ProcessSort::Memory,
            descending: true,
        }
    }
}

impl Order {
    /// Sort by `by`, the most first; by the same again, the other way.
    pub fn toggle(self, by: ProcessSort) -> Self {
        Self {
            by,
            descending: self.by != by || !self.descending,
        }
    }

    fn compare(self, a: &Process, b: &Process) -> std::cmp::Ordering {
        // A CPU share not known yet goes below every known one.
        let cpu = |process: &Process| process.cpu.unwrap_or(-1.);
        let ordering = match self.by {
            ProcessSort::Memory => a.memory.cmp(&b.memory).then(cpu(a).total_cmp(&cpu(b))),
            ProcessSort::Cpu => cpu(a).total_cmp(&cpu(b)).then(a.memory.cmp(&b.memory)),
        };
        let ordering = if self.descending {
            ordering.reverse()
        } else {
            ordering
        };
        ordering.then(a.pid.cmp(&b.pid))
    }
}

/// Turns readings into snapshots: a CPU share needs the CPU time of the
/// reading before.
#[derive(Debug, Default)]
pub struct Tracker {
    last: Option<LastReading>,
}

#[derive(Debug)]
struct LastReading {
    uptime: f64,
    /// Each process's CPU time, by PID and start time: a PID taken again
    /// by a new process is another process.
    cpu: HashMap<(u32, u64), u64>,
}

impl Tracker {
    /// `now`: the time of the reading, as seconds since the Unix epoch,
    /// which dates the processes' starts.
    pub fn update(&mut self, reading: Reading, now: i64) -> Snapshot {
        let hz = reading.ticks_per_second.max(1) as f64;
        // Seconds of the host's own clock since the last reading; none
        // when it went back (another machine, or one rebooted).
        let elapsed = self
            .last
            .as_ref()
            .map(|last| reading.uptime - last.uptime)
            .filter(|elapsed| *elapsed > 0.);
        let boot = now as f64 - reading.uptime;
        let processes: Vec<Process> = reading
            .processes
            .iter()
            .map(|process| {
                let cpu = elapsed.and_then(|elapsed| {
                    let last = self.last.as_ref()?;
                    let before = last.cpu.get(&(process.pid, process.started))?;
                    let used = process.cpu.checked_sub(*before)? as f64 / hz;
                    Some((used / elapsed * 100.) as f32)
                });
                Process {
                    pid: process.pid,
                    name: process.name.clone(),
                    state: process.state,
                    user: process.user.clone(),
                    started: (boot + process.started as f64 / hz).round() as i64,
                    memory: process.memory,
                    memory_percent: if reading.memory_total == 0 {
                        0.
                    } else {
                        (process.memory as f64 / reading.memory_total as f64 * 100.) as f32
                    },
                    cpu,
                    cpu_time: Duration::from_secs_f64(process.cpu as f64 / hz),
                    running: Duration::from_secs_f64(
                        (reading.uptime - process.started as f64 / hz).max(0.),
                    ),
                    info: process.info.clone(),
                }
            })
            .collect();
        self.last = Some(LastReading {
            uptime: reading.uptime,
            cpu: reading
                .processes
                .iter()
                .map(|process| ((process.pid, process.started), process.cpu))
                .collect(),
        });
        Snapshot {
            haystacks: processes.iter().map(Process::haystack).collect(),
            processes,
            truncated: reading.truncated,
        }
    }
}

/// 「2026-09-15 17:53:54」, in this computer's time zone.
pub fn format_started(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn times(pid: u32, name: &str, started: u64, cpu: u64, memory: u64) -> ProcessTimes {
        ProcessTimes {
            pid,
            name: name.into(),
            state: ProcessState::Sleeping,
            user: Some("root".into()),
            started,
            cpu,
            memory,
            info: ProcessInfo::default(),
        }
    }

    fn reading(uptime: f64, processes: Vec<ProcessTimes>) -> Reading {
        Reading {
            uptime,
            memory_total: 1000,
            ticks_per_second: 100,
            processes,
            truncated: false,
        }
    }

    #[test]
    fn cpu_shares_come_from_two_readings_of_the_same_process() {
        let mut tracker = Tracker::default();
        let first = tracker.update(
            reading(
                1000.,
                vec![
                    times(10, "java", 500, 1_000, 280),
                    times(20, "node", 600, 50, 70),
                ],
            ),
            1_800_000_000,
        );
        assert!(
            first
                .processes()
                .iter()
                .all(|process| process.cpu.is_none())
        );
        let java = first.process(10).expect("java");
        assert!((java.memory_percent - 28.).abs() < 0.01);
        // Booted 1000 s before now, started 5 s after that.
        assert_eq!(java.started, 1_800_000_000 - 1000 + 5);

        // 15 s later: java used 3 s of CPU, a fifth of a core; node is
        // another process under the same PID.
        let second = tracker.update(
            reading(
                1015.,
                vec![
                    times(10, "java", 500, 1_300, 280),
                    times(20, "node", 99_000, 60, 70),
                ],
            ),
            1_800_000_015,
        );
        assert_eq!(
            second.process(10).and_then(|process| process.cpu),
            Some(20.)
        );
        assert_eq!(second.process(20).and_then(|process| process.cpu), None);

        // A clock gone back is another machine: no shares.
        let third = tracker.update(
            reading(5., vec![times(10, "java", 500, 1_400, 280)]),
            1_800_000_030,
        );
        assert_eq!(third.process(10).and_then(|process| process.cpu), None);
    }

    #[test]
    fn the_list_sorts_by_memory_or_cpu_either_way_and_searches() {
        let mut tracker = Tracker::default();
        tracker.update(
            reading(
                100.,
                vec![
                    times(1, "systemd", 1, 0, 100),
                    times(10, "java", 50, 0, 500),
                    times(20, "node", 60, 0, 200),
                ],
            ),
            0,
        );
        let snapshot = tracker.update(
            reading(
                110.,
                vec![
                    times(1, "systemd", 1, 0, 100),
                    times(10, "java", 50, 10, 500),
                    times(20, "node", 60, 500, 200),
                    times(30, "sshd", 100, 0, 300),
                ],
            ),
            0,
        );
        let names = |listed: Vec<usize>| -> Vec<&str> {
            listed
                .into_iter()
                .map(|index| snapshot.processes()[index].name.as_str())
                .collect()
        };
        let memory = Order::default();
        assert_eq!(
            names(snapshot.listed("", memory)),
            ["java", "sshd", "node", "systemd"]
        );
        let cpu = memory.toggle(ProcessSort::Cpu);
        assert_eq!(
            cpu,
            Order {
                by: ProcessSort::Cpu,
                descending: true
            }
        );
        // sshd is new: its share is not known yet, so it goes last.
        assert_eq!(
            names(snapshot.listed("", cpu)),
            ["node", "java", "systemd", "sshd"]
        );
        let least = cpu.toggle(ProcessSort::Cpu);
        assert!(!least.descending);
        assert_eq!(
            names(snapshot.listed("", least)),
            ["sshd", "systemd", "java", "node"]
        );
        assert!(least.toggle(ProcessSort::Memory).descending);

        assert_eq!(names(snapshot.listed(" JAVA ", memory)), ["java"]);
        assert_eq!(names(snapshot.listed("20", memory)), ["node"]);
        assert_eq!(snapshot.listed("root", memory).len(), 4);
        assert!(snapshot.listed("nginx", memory).is_empty());
    }

    #[test]
    fn details_name_the_parent_and_count_the_children_and_theirs() {
        let child = |pid: u32, name: &str, parent: u32| {
            let mut process = times(pid, name, 100, 4_250, 10);
            process.info.parent = parent;
            process
        };
        let mut tracker = Tracker::default();
        let snapshot = tracker.update(
            reading(
                10_000.,
                vec![
                    child(1, "systemd", 0),
                    child(500, "sshd", 1),
                    child(600, "sshd", 500),
                    child(601, "bash", 600),
                    child(602, "vim", 601),
                    // Its parent is a kernel thread, which the list leaves out.
                    child(700, "kworker-ish", 2),
                ],
            ),
            0,
        );
        let details = snapshot.details(500).expect("sshd");
        assert_eq!(details.parent.as_deref(), Some("systemd (1)"));
        assert_eq!((details.children, details.descendants), (1, 3));
        assert_eq!(snapshot.details(1).expect("systemd").parent, None);
        assert_eq!(
            snapshot.details(700).expect("700").parent.as_deref(),
            Some("PID 2")
        );
        assert_eq!(snapshot.details(1).expect("systemd").descendants, 4);
        assert_eq!(snapshot.details(42), None);

        // 42.5 s of CPU; started at tick 100, a second after boot.
        let process = &details.process;
        assert_eq!(process.cpu_time, Duration::from_millis(42_500));
        assert_eq!(process.running, Duration::from_secs(9_999));
    }
}
