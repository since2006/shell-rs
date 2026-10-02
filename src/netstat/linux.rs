//! Reading a Linux host's sockets: one command, and the parser for what it
//! prints.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use super::model::{Counts, Processes, Protocol, Socket, SocketProcess, SocketState, Table};

/// The most sockets a reading lists; the host still counts them all. A busy
/// server has tens of thousands, mostly in TIME_WAIT, more than a reading's
/// output may hold (`ss` writes some 200 bytes a socket). `ss` lists UDP
/// and what listens before the connections, so those always make it.
pub const LIMIT: usize = 3000;

/// `ss` lines through, the first `LIMIT` of them, and a count of every
/// protocol and state after them.
const SS_AWK: &str = "$1 == \"Netid\" { next } \
{ n[$1 \" \" $2]++; if (++r <= LIMIT) print } \
END { print \"@@counts\"; for (k in n) print n[k], k }";

/// A `/proc/net` file's address, peer, state, uid and inode, the first
/// `LIMIT` of them, and a count of every state after them.
const PROC_AWK: &str = "FNR > 1 { n[$4]++; if (++r <= LIMIT) print $2, $3, $4, $8, $10 } \
END { print \"@@counts\"; for (k in n) print n[k], p, k }";

/// What 网络连接 runs on the host for a reading: who the login is, the
/// host's user names, and `ss -tuanpe` (every TCP and UDP socket, numeric,
/// with its processes, its uid and inode). A host without `ss` (BusyBox)
/// has `/proc/net` read instead, which names no processes. Each part under
/// an `@@` line of its own.
///
/// One line, in single quotes for `sh -c`, so that whatever the login shell
/// is (bash, zsh, fish, csh) it hands the script to `sh` untouched: the
/// script has no single quote, and no `!` for csh to expand.
pub fn command() -> String {
    format!(
        "sh -c 'export LC_ALL=C PATH=$PATH:/usr/sbin:/sbin; \
         if test -r /proc/net/tcp; then \
         echo @@uid; id -u; \
         echo @@users; while IFS=: read -r name x uid rest; do echo \"$uid $name\"; done < /etc/passwd; \
         if command -v ss >/dev/null 2>&1; then \
         echo @@ss; ss -tuanpe 2>/dev/null | awk {ss}; \
         else for f in tcp tcp6 udp udp6; do if test -r /proc/net/$f; then \
         echo @@$f; awk -v p=$f {proc} /proc/net/$f; fi; done; fi; \
         else echo @@unsupported; uname -s; fi'",
        ss = double_quoted(&SS_AWK.replace("LIMIT", &LIMIT.to_string())),
        proc = double_quoted(&PROC_AWK.replace("LIMIT", &LIMIT.to_string())),
    )
}

/// `text` in double quotes for `sh`, which expands nothing in it.
fn double_quoted(text: &str) -> String {
    let mut quoted = String::from("\"");
    for character in text.chars() {
        if matches!(character, '"' | '$' | '`' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// What the command's output said.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Table(Table),
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

    if !sections.contains_key("uid") {
        // A shell other than sh's (cmd.exe) prints nothing we know.
        return Parsed::Unsupported(first("unsupported").map(str::to_owned));
    }
    let root = first("uid") == Some("0");
    let users: HashMap<&str, &str> = section("users")
        .iter()
        .filter_map(|line| line.trim().split_once(' '))
        .collect();
    let user = |uid: &str| users.get(uid).unwrap_or(&uid).to_string();

    let mut counted = Counts::default();
    let mut sockets = Vec::new();
    let processes = if !sections.contains_key("ss") {
        Processes::None
    } else if root {
        Processes::All
    } else {
        Processes::Own
    };
    if sections.contains_key("ss") {
        sockets.extend(
            section("ss")
                .iter()
                .filter_map(|line| parse_ss(line, &user)),
        );
        for line in section("counts") {
            let mut fields = line.split_whitespace();
            let (Some(count), Some(netid), Some(state)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if let (Ok(count), Some(protocol)) = (count.parse(), protocol_of(netid))
                && let Some(state) = SocketState::from_ss(protocol, state)
            {
                counted.add(state, count);
            }
        }
    } else {
        for file in ["tcp", "tcp6", "udp", "udp6"] {
            let Some(protocol) = protocol_of(file) else {
                continue;
            };
            sockets.extend(
                section(file)
                    .iter()
                    .filter_map(|line| parse_proc(protocol, line, &user)),
            );
        }
        for line in section("counts") {
            let mut fields = line.split_whitespace();
            let (Some(count), Some(file), Some(state)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if let (Ok(count), Some(protocol)) = (count.parse(), protocol_of(file))
                && let Some(state) = SocketState::from_proc(protocol, state)
            {
                counted.add(state, count);
            }
        }
    }
    Parsed::Table(Table::new(sockets, Some(counted), processes))
}

/// 「tcp」「udp6」.
fn protocol_of(name: &str) -> Option<Protocol> {
    match name {
        "tcp" | "tcp6" => Some(Protocol::Tcp),
        "udp" | "udp6" => Some(Protocol::Udp),
        _ => None,
    }
}

/// A line of `ss -tuanpe`:
///
/// ```text
/// tcp LISTEN 0 4096 0.0.0.0:22 0.0.0.0:* users:(("sshd",pid=891,fd=3)) ino:21233 sk:5 <->
/// ```
///
/// `ss` leaves `uid:0` out, so a socket with an inode and no uid is
/// root's; one with inode 0 has no owner (TIME_WAIT).
fn parse_ss(line: &str, user: &impl Fn(&str) -> String) -> Option<Socket> {
    let mut fields = line.split_whitespace();
    let protocol = protocol_of(fields.next()?)?;
    let state = SocketState::from_ss(protocol, fields.next()?)?;
    let (_received, _sent) = (fields.next()?, fields.next()?);
    let (local, peer) = (fields.next()?, fields.next()?);
    let mut socket = Socket::new(protocol, state, local.to_owned(), peer.to_owned());

    // The rest: the processes, whose names may hold spaces, then the
    // details.
    let rest = after_fields(line, 6);
    socket.processes = parse_users(rest);
    let detail = |name: &str| {
        rest.split_whitespace()
            .find_map(|field| field.strip_prefix(name))
    };
    socket.user = match detail("ino:") {
        Some("0") | None => None,
        Some(_) => Some(user(detail("uid:").unwrap_or("0"))),
    };
    Some(socket)
}

/// `line` past its first `count` fields.
fn after_fields(line: &str, count: usize) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..count {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = rest[end..].trim_start();
    }
    rest
}

/// `users:(("nginx",pid=913756,fd=6),("nginx",pid=913757,fd=6))`; an old
/// `ss` writes `("nginx",913756,6)`.
fn parse_users(rest: &str) -> Vec<SocketProcess> {
    let Some(start) = rest.find("users:((") else {
        return Vec::new();
    };
    let mut processes: Vec<SocketProcess> = Vec::new();
    let mut remaining = &rest[start + "users:(".len()..];
    while let Some(open) = remaining.strip_prefix("(\"") {
        let Some((name, after)) = open.split_once("\",") else {
            break;
        };
        let Some((fields, after)) = after.split_once(')') else {
            break;
        };
        let pid = fields
            .split(',')
            .next()
            .map(|pid| pid.trim_start_matches("pid="))
            .and_then(|pid| pid.parse().ok());
        if let Some(pid) = pid
            && !processes.iter().any(|process| process.pid == pid)
        {
            processes.push(SocketProcess {
                name: name.to_owned(),
                pid,
            });
        }
        remaining = after.strip_prefix(',').unwrap_or(after);
    }
    processes
}

/// A line of `/proc/net/tcp` cut to address, peer, state, uid and inode:
///
/// ```text
/// 0100007F:0CEA 00000000:0000 0A 0 12345
/// ```
fn parse_proc(protocol: Protocol, line: &str, user: &impl Fn(&str) -> String) -> Option<Socket> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let [local, peer, state, uid, inode] = fields.as_slice() else {
        return None;
    };
    let state = SocketState::from_proc(protocol, state)?;
    let mut socket = Socket::new(protocol, state, proc_address(local)?, proc_address(peer)?);
    socket.user = (*inode != "0").then(|| user(uid));
    Some(socket)
}

/// 「0100007F:0CEA」 to 「127.0.0.1:3306」, as `ss` writes it: the address
/// in the host's byte order, a 32-bit word at a time, the port in hex; port
/// 0 is 「*」, IPv6 in brackets.
fn proc_address(hex: &str) -> Option<String> {
    let (address, port) = hex.split_once(':')?;
    let port = match u16::from_str_radix(port, 16).ok()? {
        0 => "*".to_string(),
        port => port.to_string(),
    };
    let words = (0..address.len() / 8)
        .map(|word| {
            let word = u32::from_str_radix(address.get(word * 8..word * 8 + 8)?, 16).ok()?;
            // `/proc` prints the in-memory word, which is little-endian on
            // every architecture Linux servers run on.
            Some(word.swap_bytes().to_be_bytes())
        })
        .collect::<Option<Vec<[u8; 4]>>>()?;
    match words.as_slice() {
        [ipv4] => Some(format!("{}:{port}", Ipv4Addr::from(*ipv4))),
        [a, b, c, d] => {
            let bytes: Vec<u8> = [a, b, c, d].into_iter().flatten().copied().collect();
            let ipv6 = Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?);
            Some(format!("[{ipv6}]:{port}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netstat::model::Role;

    const SS: &str = "\
@@uid
0
@@users
0 root
33 www-data
101 systemd-resolve
@@ss
udp   UNCONN    0      0       127.0.0.53%lo:53            0.0.0.0:*     users:((\"systemd-resolve\",pid=600,fd=13)) uid:101 ino:20123 sk:1 cgroup:/system.slice/systemd-resolved.service <->
tcp   LISTEN    0      4096          0.0.0.0:22            0.0.0.0:*     users:((\"sshd\",pid=891,fd=3)) ino:21233 sk:5 cgroup:/system.slice/ssh.service <->
tcp   LISTEN    0      511                 *:80                  *:*     users:((\"nginx\",pid=913757,fd=6),(\"nginx\",pid=913756,fd=6)) ino:30001 sk:6 v6only:0 <->
tcp   LISTEN    0      511                 *:80                  *:*     users:((\"nginx\",pid=913758,fd=6)) ino:30002 sk:7 v6only:0 <->
tcp   ESTAB     0      0            10.0.0.5:22        203.0.113.7:51234 users:((\"sshd\",pid=1234,fd=4),(\"sshd\",pid=1200,fd=4)) timer:(keepalive,119min,0) ino:123456 sk:9 <->
tcp   ESTAB     0      0            10.0.0.5:80       198.51.100.2:6001  users:((\"Web Content\",pid=4242,fd=9)) uid:33 ino:4444 sk:b <->
tcp   TIME-WAIT 0      0            10.0.0.5:80       198.51.100.2:6000  timer:(timewait,50sec,0) ino:0 sk:a
tcp   ESTAB     0      0     [::ffff:10.0.0.5]:3306    [::ffff:10.0.0.9]:39000 ino:777 sk:c uid:999 <->
@@counts
2 tcp ESTAB
1 udp UNCONN
3 tcp LISTEN
1 tcp TIME-WAIT
1 tcp ESTAB
";

    fn table(output: &str) -> Table {
        match parse(output) {
            Parsed::Table(table) => table,
            other => panic!("not a table: {other:?}"),
        }
    }

    #[test]
    fn ss_lines_give_the_sockets_their_processes_and_users() {
        let table = table(SS);
        assert_eq!(table.processes(), Processes::All);
        assert!(!table.truncated());
        let line = |local: &str, peer: &str| {
            table
                .sockets()
                .iter()
                .find(|socket| socket.local == local && socket.peer == peer)
                .unwrap_or_else(|| panic!("no {local} {peer}"))
        };

        let sshd = line("0.0.0.0:22", "0.0.0.0:*");
        assert_eq!(sshd.state, SocketState::Listen);
        assert_eq!(sshd.process_label().as_deref(), Some("sshd (PID 891)"));
        // No uid: root's.
        assert_eq!(sshd.user.as_deref(), Some("root"));

        // Two lines on one port make one, with every worker.
        let nginx = line("*:80", "*:*");
        assert_eq!(
            nginx.process_label().as_deref(),
            Some("nginx (PID 913757 等 3 个)")
        );

        let resolver = line("127.0.0.53%lo:53", "0.0.0.0:*");
        assert_eq!(resolver.protocol, Protocol::Udp);
        assert_eq!(resolver.state, SocketState::Listen);
        assert_eq!(resolver.user.as_deref(), Some("systemd-resolve"));

        let session = line("10.0.0.5:22", "203.0.113.7:51234");
        assert_eq!(session.role, Role::Inbound);
        assert_eq!(session.processes.len(), 2);

        // A name with a space in it.
        let browser = line("10.0.0.5:80", "198.51.100.2:6001");
        assert_eq!(
            browser.process_label().as_deref(),
            Some("Web Content (PID 4242)")
        );
        assert_eq!(browser.user.as_deref(), Some("www-data"));

        // No process holds a socket in TIME_WAIT, and no one owns it.
        let closing = line("10.0.0.5:80", "198.51.100.2:6000");
        assert_eq!(closing.state, SocketState::TimeWait);
        assert!(closing.processes.is_empty());
        assert_eq!(closing.user, None);

        // Another user's socket, seen without root: no process; a uid the
        // host has no name for stays a number.
        let database = line("[::ffff:10.0.0.5]:3306", "[::ffff:10.0.0.9]:39000");
        assert!(database.processes.is_empty());
        assert_eq!(database.user.as_deref(), Some("999"));

        assert_eq!(
            table.counts().summary(),
            "共 7 条 · 3 个监听端口 · 3 条已连接"
        );
    }

    #[test]
    fn a_host_with_more_sockets_than_a_reading_lists_says_how_many() {
        let output = SS.replace("1 tcp TIME-WAIT", "40000 tcp TIME-WAIT");
        let table = table(&output);
        assert!(table.truncated());
        assert_eq!(table.sockets().len(), 7);
        assert_eq!(
            table.counts().summary(),
            "共 40007 条 · 4 个监听端口 · 3 条已连接"
        );
    }

    #[test]
    fn proc_lines_are_read_where_there_is_no_ss() {
        let output = "\
@@uid
1000
@@users
0 root
1000 deploy
@@tcp
0100007F:0CEA 00000000:0000 0A 999 12345
0500000A:0016 0700A8C0:C350 01 1000 23456
0500000A:0050 0200A8C0:1770 06 0 0
@@counts
1 tcp 0A
1 tcp 01
1 tcp 06
@@tcp6
00000000000000000000000000000000:0016 00000000000000000000000000000000:0000 0A 0 777
@@counts
1 tcp6 0A
@@udp
00000000:0044 00000000:0000 07 0 888
@@counts
1 udp 07
";
        let table = table(output);
        assert_eq!(table.processes(), Processes::None);
        assert!(!table.truncated());
        let lines: Vec<(Protocol, &str, &str, Option<&str>)> = table
            .sockets()
            .iter()
            .map(|socket| {
                (
                    socket.protocol,
                    socket.local.as_str(),
                    socket.peer.as_str(),
                    socket.user.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                (Protocol::Tcp, "[::]:22", "[::]:*", Some("root")),
                (Protocol::Tcp, "127.0.0.1:3306", "0.0.0.0:*", Some("999")),
                (Protocol::Udp, "0.0.0.0:68", "0.0.0.0:*", Some("root")),
                (
                    Protocol::Tcp,
                    "10.0.0.5:22",
                    "192.168.0.7:50000",
                    Some("deploy")
                ),
                (Protocol::Tcp, "10.0.0.5:80", "192.168.0.2:6000", None),
            ]
        );
    }

    #[test]
    fn a_host_that_is_not_linux_says_what_it_is() {
        assert_eq!(
            parse("@@unsupported\nDarwin\n"),
            Parsed::Unsupported(Some("Darwin".into()))
        );
        // cmd.exe runs none of it.
        assert_eq!(parse(""), Parsed::Unsupported(None));
    }

    #[test]
    fn the_command_survives_any_login_shell() {
        let command = command();
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

    #[cfg(unix)]
    fn run(script: &str, path: &str) -> String {
        let output = crate::testing::sh(script, Some(path));
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).expect("utf-8")
    }

    /// The script, run by this machine's `sh` with `/proc`, `/etc/passwd`
    /// and `ss` stood in for, gives what the parser reads.
    #[cfg(unix)]
    #[test]
    fn the_script_reads_ss_or_else_proc() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temp dir");
        let at = |path: &str| root.path().join(path);
        std::fs::create_dir_all(at("proc/net")).unwrap();
        std::fs::create_dir_all(at("etc")).unwrap();
        std::fs::create_dir_all(at("bin")).unwrap();
        std::fs::write(at("etc/passwd"), "root:x:0:0:root:/root:/bin/bash\n").unwrap();
        let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
        std::fs::write(
            at("proc/net/tcp"),
            format!(
                "{header}   0: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 21233 1 0000000000000000 100 0 0 10 0\n"
            ),
        )
        .unwrap();
        std::fs::write(
            at("proc/net/udp"),
            format!(
                "{header}  1: 3500007F:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000   101        0 20123 2 0000000000000000 0\n"
            ),
        )
        .unwrap();
        let ss = at("bin/ss");
        std::fs::write(
            &ss,
            "#!/bin/sh\necho 'Netid State Recv-Q Send-Q Local Address:Port Peer Address:PortProcess'\n\
             echo 'tcp LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=891,fd=3)) ino:21233 sk:1'\n\
             echo 'tcp TIME-WAIT 0 0 10.0.0.5:22 10.0.0.9:4000 timer:(timewait,1sec,0) ino:0 sk:2'\n",
        )
        .unwrap();
        std::fs::set_permissions(&ss, std::fs::Permissions::from_mode(0o755)).unwrap();

        let command = command().replace("/proc/", &format!("{}/", at("proc").display()));
        let command = command.replace("/etc/passwd", &at("etc/passwd").display().to_string());
        let script = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap();
        let system = std::env::var("PATH").unwrap_or_default();

        let with_ss = table(&run(script, &format!("{}:{system}", at("bin").display())));
        let lines: Vec<(&str, Option<String>, Option<&str>)> = with_ss
            .sockets()
            .iter()
            .map(|socket| {
                (
                    socket.local.as_str(),
                    socket.process_label(),
                    socket.user.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                ("0.0.0.0:22", Some("sshd (PID 891)".into()), Some("root")),
                ("10.0.0.5:22", None, None),
            ]
        );
        assert!(!with_ss.truncated());

        // Without `ss`, the files themselves; tcp6 and udp6 are missing,
        // as on a host with IPv6 turned off.
        let script = script.replace("command -v ss ", "command -v no-such-ss ");
        let without = table(&run(&script, &system));
        let lines: Vec<(Protocol, &str, Option<&str>)> = without
            .sockets()
            .iter()
            .map(|socket| {
                (
                    socket.protocol,
                    socket.local.as_str(),
                    socket.user.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            lines,
            [
                (Protocol::Tcp, "0.0.0.0:22", Some("root")),
                (Protocol::Udp, "127.0.0.53:53", Some("101")),
            ]
        );
        assert_eq!(without.counts().total, 2);
    }

    #[cfg(unix)]
    #[test]
    fn the_command_is_valid_sh() {
        let command = command();
        let script = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap();
        assert!(crate::testing::sh_accepts(script));
    }
}
