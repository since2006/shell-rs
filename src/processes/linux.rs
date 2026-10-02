//! Reading a Linux host's processes, and ending one: the commands, and the
//! parsers for what they print.

use std::collections::HashMap;

use super::model::{ProcessInfo, ProcessState, ProcessTimes, Reading};

/// The most processes a reading lists: some 300 bytes of `/proc` each, and
/// a reading's output holds 1 MB.
pub const LIMIT: usize = 2500;

/// `PF_KTHREAD` in `/proc/<pid>/stat`'s flags: a kernel thread, which has
/// no program to manage.
const KERNEL_THREAD: u64 = 0x0020_0000;

/// What 进程管理 runs on the host for a reading: the uptime and the
/// memory, the clock tick and page size, the host's user names, and each
/// process's `stat` line with its owner's uid from `status`. Each part
/// under an `@@` line of its own.
///
/// Shell built-ins only (`read`, `case`, `echo`, `for`) besides `getconf`,
/// so a reading starts two short processes on the host, and the `sh`
/// running it. One line, in single quotes for `sh -c`, so that whatever the
/// login shell is (bash, zsh, fish, csh) it hands the script to `sh`
/// untouched: the script has no single quote, and no `!` for csh to
/// expand.
pub fn command() -> String {
    format!(
        "sh -c 'export LC_ALL=C; if test -r /proc/self/stat; then \
         echo @@uptime; read -r line < /proc/uptime; echo \"$line\"; \
         echo @@memory; while read -r key value rest; do case $key in MemTotal:) echo \"$value\"; break;; esac; done < /proc/meminfo; \
         echo @@clock; getconf CLK_TCK 2>/dev/null; \
         echo @@page; getconf PAGESIZE 2>/dev/null; \
         echo @@users; while IFS=: read -r name x uid rest; do echo \"$uid $name\"; done < /etc/passwd; \
         echo @@processes; n=0; for d in /proc/[0-9]*; do \
         n=$((n+1)); if test $n -gt {LIMIT}; then echo @@more; break; fi; \
         read -r stat < $d/stat || continue; uid=; \
         while read -r key value rest; do case $key in Uid:) uid=$value; break;; esac; done < $d/status; \
         echo \"$uid $stat\"; done 2>/dev/null; \
         else echo @@unsupported; uname -s; fi'"
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
    };
    let number = |name: &str| {
        first(name).and_then(|line| line.split_whitespace().next()?.parse::<f64>().ok())
    };

    let Some(uptime) = number("uptime") else {
        // A shell other than sh's (cmd.exe) prints nothing we know.
        return Parsed::Unsupported(first("unsupported").map(str::to_owned));
    };
    let users: HashMap<&str, &str> = section("users")
        .iter()
        .filter_map(|line| line.trim().split_once(' '))
        .collect();
    let page = number("page").map_or(4096, |page| page as u64);
    Parsed::Reading(Box::new(Reading {
        uptime,
        memory_total: number("memory").map_or(0, |kib| kib as u64 * 1024),
        ticks_per_second: number("clock").map_or(100, |hz| hz as u64),
        processes: section("processes")
            .iter()
            .filter_map(|line| parse_process(line, &users, page))
            .collect(),
        truncated: sections.contains_key("more"),
    }))
}

/// 「0 1234 (java) S 1 …」: the owner's uid, then `/proc/<pid>/stat`, whose
/// name is in parentheses and may hold spaces and parentheses itself, so
/// the fields are counted from the last `)`. `None` for a kernel thread.
fn parse_process(line: &str, users: &HashMap<&str, &str>, page: u64) -> Option<ProcessTimes> {
    let (uid, stat) = line.split_once(' ')?;
    let (pid, rest) = stat.split_once(" (")?;
    let (name, rest) = rest.rsplit_once(") ")?;
    // Fields 3 on: state, ppid, pgrp, session, tty, tpgid, flags, four
    // fault counts, utime, stime, …
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let field = |number: usize| -> Option<u64> { fields.get(number - 3)?.parse().ok() };
    let signed = |number: usize| -> Option<i64> { fields.get(number - 3)?.parse().ok() };
    if field(9)? & KERNEL_THREAD != 0 {
        return None;
    }
    let pid: u32 = pid.trim().parse().ok()?;
    let state = fields.first()?;
    let (group, session, terminal, foreground) = (signed(5)?, signed(6)?, signed(7)?, signed(8)?);
    let (nice, threads) = (signed(19)?, field(20)?);
    // `ps`'s letters after the state: priority, session leader, threads,
    // foreground.
    let mut letters = state.to_string();
    if nice < 0 {
        letters.push('<');
    } else if nice > 0 {
        letters.push('N');
    }
    if session == i64::from(pid) {
        letters.push('s');
    }
    if threads > 1 {
        letters.push('l');
    }
    if terminal != 0 && foreground == group {
        letters.push('+');
    }
    Some(ProcessTimes {
        pid,
        name: name.to_owned(),
        state: ProcessState::from_letter(state),
        user: (!uid.is_empty()).then(|| users.get(uid).unwrap_or(&uid).to_string()),
        started: field(22)?,
        cpu: field(14)? + field(15)?,
        memory: field(24)? * page,
        info: ProcessInfo {
            parent: field(4)? as u32,
            letters,
            terminal: terminal_name(terminal),
            priority: signed(18)?,
            nice,
            threads,
            virtual_memory: field(23)?,
        },
    })
}

/// A terminal's device number as `stat` has it, by name: 「pts/0」「tty1」.
fn terminal_name(device: i64) -> Option<String> {
    if device == 0 {
        return None;
    }
    let device = device as u32;
    let major = (device >> 8) & 0xfff;
    let minor = (device & 0xff) | ((device >> 12) & 0xfff00);
    Some(match major {
        136..=143 => format!("pts/{}", minor + (major - 136) * 256),
        4 if minor < 64 => format!("tty{minor}"),
        4 => format!("ttyS{}", minor - 64),
        _ => format!("{major}:{minor}"),
    })
}

/// What reads a process's whole command line: its arguments, each ended
/// by a NUL.
pub fn command_line_command(pid: u32) -> String {
    format!("sh -c 'cat /proc/{pid}/cmdline'")
}

/// The command line as `ps` shows it, the arguments joined by spaces;
/// `None` for a process without one (a zombie), or gone.
pub fn command_line(output: &str) -> Option<String> {
    let line = output
        .split('\0')
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    (!line.trim().is_empty()).then_some(line)
}

/// What ends `pid`: SIGTERM, which lets it clean up, or with `force`
/// SIGKILL, which it cannot refuse. Says how `kill` went in an `@@status`
/// line, with its complaint before it.
pub fn end_command(pid: u32, force: bool) -> String {
    let signal = if force { "KILL" } else { "TERM" };
    format!("sh -c 'kill -{signal} {pid} 2>&1; echo @@status $?'")
}

/// How ending a process went, from what `end_command` printed: why not,
/// when it did not.
pub fn ended(output: &str) -> Result<(), String> {
    let (said, status) = output
        .rsplit_once("@@status")
        .ok_or_else(|| "主机没有回答".to_string())?;
    if status.trim() == "0" {
        return Ok(());
    }
    let said = said.trim();
    Err(if said.contains("not permitted") {
        "没有权限：只能结束自己的进程，root 才能结束所有进程".into()
    } else if said.contains("No such process") {
        "进程已经退出".into()
    } else if said.is_empty() {
        format!("kill 返回 {}", status.trim())
    } else {
        said.to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: &str = "\
@@uptime
1000.50 3800.00
@@memory
2000000
@@clock
100
@@page
4096
@@users
0 root
1000 ecs-user
@@processes
0 1 (systemd) S 0 1 1 0 -1 4194560 100 0 0 0 150 80 0 0 20 0 1 0 5 172000000 3000 18446744073709551615 1 1 0 0 0 0
0 2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 5 0 0 18446744073709551615 0 0 0 0 0 0
1000 1479 (node) R 1 1479 1479 0 -1 4194560 100 0 0 0 4000 500 0 0 20 0 11 0 9000 900000000 30000 18446744073709551615
33 2202 (Web Content) S 1 2202 2202 0 -1 4194560 0 0 0 0 10 10 0 0 20 0 1 0 9500 1000 10 0
1001 3000 (a) b) Z 1 0 0 0 -1 4194564 0 0 0 0 0 0 0 0 20 0 1 0 9600 0 0 0
 4000 (gone) S 1 0 0 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 9700 1000 1 0
";

    fn reading(output: &str) -> Reading {
        match parse(output) {
            Parsed::Reading(reading) => *reading,
            other => panic!("not a reading: {other:?}"),
        }
    }

    #[test]
    fn stat_lines_give_the_processes_but_not_the_kernels_threads() {
        let reading = reading(OUTPUT);
        assert_eq!(reading.uptime, 1000.5);
        assert_eq!(reading.memory_total, 2_048_000_000);
        assert_eq!(reading.ticks_per_second, 100);
        assert!(!reading.truncated);
        let processes: Vec<(u32, &str, ProcessState, Option<&str>)> = reading
            .processes
            .iter()
            .map(|process| {
                (
                    process.pid,
                    process.name.as_str(),
                    process.state,
                    process.user.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            processes,
            [
                (1, "systemd", ProcessState::Sleeping, Some("root")),
                (1479, "node", ProcessState::Running, Some("ecs-user")),
                // A uid with no name stays a number; a name may hold spaces
                // and parentheses.
                (2202, "Web Content", ProcessState::Sleeping, Some("33")),
                (3000, "a) b", ProcessState::Zombie, Some("1001")),
                // Its status went before it could be read.
                (4000, "gone", ProcessState::Sleeping, None),
            ]
        );
        let node = &reading.processes[1];
        assert_eq!(node.cpu, 4500);
        assert_eq!(node.started, 9000);
        assert_eq!(node.memory, 30000 * 4096);
        assert_eq!(
            node.info,
            ProcessInfo {
                parent: 1,
                // A session leader with threads.
                letters: "Rsl".into(),
                terminal: None,
                priority: 20,
                nice: 0,
                threads: 11,
                virtual_memory: 900_000_000,
            }
        );
    }

    #[test]
    fn a_process_on_a_terminal_in_front_of_it_reads_like_ps() {
        // vim on pts/1 (136 << 8 | 1), in the terminal's foreground group,
        // niced down.
        let line =
            "0 900 (vim) S 800 900 800 34817 900 4194304 0 0 0 0 5 1 0 0 25 5 1 0 100 1000 10 0";
        let process = parse_process(line, &HashMap::new(), 4096).expect("vim");
        assert_eq!(process.info.letters, "SN+");
        assert_eq!(process.info.terminal.as_deref(), Some("pts/1"));
        assert_eq!((process.info.priority, process.info.nice), (25, 5));
        assert_eq!(terminal_name((4 << 8) | 1).as_deref(), Some("tty1"));
        assert_eq!(terminal_name((4 << 8) | 64).as_deref(), Some("ttyS0"));
    }

    #[test]
    fn a_command_line_is_its_arguments_joined_by_spaces() {
        assert_eq!(command_line_command(2202), "sh -c 'cat /proc/2202/cmdline'");
        assert_eq!(
            command_line("/usr/bin/java\0-Xmx256m\0-jar\0app.jar\0").as_deref(),
            Some("/usr/bin/java -Xmx256m -jar app.jar")
        );
        // A zombie's is empty, and so is a gone process's.
        assert_eq!(command_line(""), None);
    }

    #[test]
    fn a_host_with_more_processes_than_a_reading_lists_says_so() {
        let reading = reading(&format!("{OUTPUT}@@more\n"));
        assert!(reading.truncated);
        assert_eq!(reading.processes.len(), 5);
    }

    #[test]
    fn a_host_that_is_not_linux_says_what_it_is() {
        assert_eq!(
            parse("@@unsupported\nDarwin\n"),
            Parsed::Unsupported(Some("Darwin".into()))
        );
        assert_eq!(parse(""), Parsed::Unsupported(None));
    }

    #[test]
    fn ending_a_process_says_why_it_did_not_end() {
        assert_eq!(
            end_command(2202, false),
            "sh -c 'kill -TERM 2202 2>&1; echo @@status $?'"
        );
        assert_eq!(
            end_command(2202, true),
            "sh -c 'kill -KILL 2202 2>&1; echo @@status $?'"
        );
        assert_eq!(ended("@@status 0\n"), Ok(()));
        assert_eq!(
            ended("sh: 1: kill: Operation not permitted\n@@status 2\n"),
            Err("没有权限：只能结束自己的进程，root 才能结束所有进程".into())
        );
        assert_eq!(
            ended("sh: kill: (2202) - No such process\n@@status 1\n"),
            Err("进程已经退出".into())
        );
        assert_eq!(ended("@@status 1\n"), Err("kill 返回 1".into()));
        assert_eq!(ended(""), Err("主机没有回答".into()));
    }

    #[test]
    fn the_commands_survive_any_login_shell() {
        for command in [command(), end_command(1, false), command_line_command(1)] {
            let script = command
                .strip_prefix("sh -c '")
                .and_then(|rest| rest.strip_suffix('\''))
                .expect("one sh -c in single quotes");
            assert!(!script.contains('\''));
            assert!(!script.contains('!'));
            assert!(!script.contains('\n'));
            // fish reads 「\\」 in single quotes as one backslash.
            assert!(!script.contains("\\\\"));
        }
    }

    /// The script, run by this machine's `sh` against a `/proc` and an
    /// `/etc/passwd` stood in for, gives what the parser reads.
    #[cfg(unix)]
    #[test]
    fn the_script_reads_proc_with_shell_built_ins() {
        let root = tempfile::tempdir().expect("temp dir");
        let at = |path: &str| root.path().join(path);
        let write = |path: &str, text: &str| {
            std::fs::create_dir_all(at(path).parent().unwrap()).unwrap();
            std::fs::write(at(path), text).unwrap();
        };
        write("etc/passwd", "root:x:0:0:root:/root:/bin/bash\n");
        write("proc/uptime", "1000.50 3800.00\n");
        write(
            "proc/meminfo",
            "MemTotal:        2000000 kB\nMemFree: 1 kB\n",
        );
        write("proc/self/stat", "1 (sh) S 0\n");
        write(
            "proc/1/stat",
            "1 (systemd) S 0 1 1 0 -1 4194560 100 0 0 0 150 80 0 0 20 0 1 0 5 172000000 3000 0\n",
        );
        write(
            "proc/1/status",
            "Name:\tsystemd\nState:\tS (sleeping)\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n",
        );
        write(
            "proc/2/stat",
            "2 (kthreadd) S 0 0 0 0 -1 2129984 0 0 0 0 0 0 0 0 20 0 1 0 5 0 0 0\n",
        );
        write("proc/2/status", "Name:\tkthreadd\nUid:\t0\t0\t0\t0\n");
        write(
            "proc/77/stat",
            "77 (Web Content) R 1 1 1 0 -1 4194560 0 0 0 0 40 2 0 0 20 0 1 0 900 1000 25 0\n",
        );
        write(
            "proc/77/status",
            "Name:\tWeb Content\nUid:\t1000\t1000\t1000\t1000\n",
        );
        // A process that went between the listing and the reading.
        std::fs::create_dir_all(at("proc/88")).unwrap();

        let command = command()
            .replace("/proc/", &format!("{}/", at("proc").display()))
            .replace("/etc/passwd", &at("etc/passwd").display().to_string())
            .replace("getconf CLK_TCK", "echo 100")
            .replace("getconf PAGESIZE", "echo 4096");
        let script = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap();
        let output = std::process::Command::new("sh")
            .args(["-c", script])
            .output()
            .expect("sh runs");
        let reading = reading(&String::from_utf8(output.stdout).unwrap());
        assert_eq!(reading.memory_total, 2_048_000_000);
        let processes: Vec<(u32, &str, Option<&str>, u64, u64)> = reading
            .processes
            .iter()
            .map(|process| {
                (
                    process.pid,
                    process.name.as_str(),
                    process.user.as_deref(),
                    process.cpu,
                    process.memory,
                )
            })
            .collect();
        assert_eq!(
            processes,
            [
                (1, "systemd", Some("root"), 230, 3000 * 4096),
                (77, "Web Content", Some("1000"), 42, 25 * 4096),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_commands_are_valid_sh() {
        for command in [command(), end_command(1, true), command_line_command(1)] {
            let script = command
                .strip_prefix("sh -c '")
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap();
            let status = std::process::Command::new("sh")
                .args(["-n", "-c", script])
                .status()
                .expect("sh runs");
            assert!(status.success());
        }
    }
}
