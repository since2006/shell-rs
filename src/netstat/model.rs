//! What 网络连接 lists, and how its search and filters pick from the list.

use std::collections::{HashMap, HashSet};

use gpui_kit::SharedString;

use crate::i18n::{t, tn};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SocketState {
    /// A TCP socket taking connections, or a UDP socket bound to a port and
    /// connected to no peer: either takes what comes to its port.
    Listen,
    Established,
    SynSent,
    SynReceived,
    FinWait1,
    FinWait2,
    TimeWait,
    Close,
    CloseWait,
    LastAck,
    Closing,
}

impl SocketState {
    /// The state as `ss` names it: 「LISTEN」「ESTAB」「TIME-WAIT」. A UDP
    /// socket connected to no peer is 「UNCONN」; a TCP one in that state
    /// is bound and neither listening nor connected.
    pub fn from_ss(protocol: Protocol, name: &str) -> Option<Self> {
        Some(match (protocol, name) {
            (Protocol::Udp, "UNCONN") => SocketState::Listen,
            (Protocol::Tcp, "UNCONN") => SocketState::Close,
            (_, "LISTEN") => SocketState::Listen,
            (_, "ESTAB") => SocketState::Established,
            (_, "SYN-SENT") => SocketState::SynSent,
            (_, "SYN-RECV") => SocketState::SynReceived,
            (_, "FIN-WAIT-1") => SocketState::FinWait1,
            (_, "FIN-WAIT-2") => SocketState::FinWait2,
            (_, "TIME-WAIT") => SocketState::TimeWait,
            (_, "CLOSE-WAIT") => SocketState::CloseWait,
            (_, "LAST-ACK") => SocketState::LastAck,
            (_, "CLOSING") => SocketState::Closing,
            _ => return None,
        })
    }

    /// The state as `/proc/net/tcp` and its kin write it, in hex. UDP uses
    /// only two: connected (01) and not (07).
    pub fn from_proc(protocol: Protocol, hex: &str) -> Option<Self> {
        Some(match (protocol, u8::from_str_radix(hex, 16).ok()?) {
            (Protocol::Udp, 0x07) => SocketState::Listen,
            (Protocol::Udp, 0x01) => SocketState::Established,
            (Protocol::Udp, _) => return None,
            (Protocol::Tcp, 0x01) => SocketState::Established,
            (Protocol::Tcp, 0x02) => SocketState::SynSent,
            (Protocol::Tcp, 0x03 | 0x0C) => SocketState::SynReceived,
            (Protocol::Tcp, 0x04) => SocketState::FinWait1,
            (Protocol::Tcp, 0x05) => SocketState::FinWait2,
            (Protocol::Tcp, 0x06) => SocketState::TimeWait,
            (Protocol::Tcp, 0x07) => SocketState::Close,
            (Protocol::Tcp, 0x08) => SocketState::CloseWait,
            (Protocol::Tcp, 0x09) => SocketState::LastAck,
            (Protocol::Tcp, 0x0A) => SocketState::Listen,
            (Protocol::Tcp, 0x0B) => SocketState::Closing,
            (Protocol::Tcp, _) => return None,
        })
    }

    /// How the list says it: 「监听」「已连接」, and the TCP name for the
    /// states only the TCP-minded look for: 「TIME_WAIT」.
    pub fn label(self) -> SharedString {
        match self {
            SocketState::Listen => t!("netstat.state.listen"),
            SocketState::Established => t!("netstat.state.established"),
            other => other.netstat_name().into(),
        }
    }

    /// The name netstat gives it, which a search finds as well.
    fn netstat_name(self) -> &'static str {
        match self {
            SocketState::Listen => "LISTEN",
            SocketState::Established => "ESTABLISHED",
            SocketState::SynSent => "SYN_SENT",
            SocketState::SynReceived => "SYN_RECV",
            SocketState::FinWait1 => "FIN_WAIT1",
            SocketState::FinWait2 => "FIN_WAIT2",
            SocketState::TimeWait => "TIME_WAIT",
            SocketState::Close => "CLOSE",
            SocketState::CloseWait => "CLOSE_WAIT",
            SocketState::LastAck => "LAST_ACK",
            SocketState::Closing => "CLOSING",
        }
    }

    /// Where the list puts it: what listens first, then the live
    /// connections, the ones on their way out last.
    fn rank(self) -> u8 {
        match self {
            SocketState::Listen => 0,
            SocketState::Established => 1,
            SocketState::CloseWait => 2,
            SocketState::SynSent | SocketState::SynReceived => 3,
            SocketState::FinWait1
            | SocketState::FinWait2
            | SocketState::Closing
            | SocketState::LastAck
            | SocketState::Close => 4,
            SocketState::TimeWait => 5,
        }
    }
}

/// What a socket is for the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Takes what comes to its port.
    Listening,
    /// Came in to a port something on the host listens on.
    Inbound,
    /// Went out from the host.
    Outbound,
}

impl Role {
    pub fn label(self) -> SharedString {
        match self {
            Role::Listening => t!("netstat.role.listening"),
            Role::Inbound => t!("netstat.role.inbound"),
            Role::Outbound => t!("netstat.role.outbound"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketProcess {
    pub name: String,
    pub pid: u32,
}

/// One line of the list: a socket, or the sockets sharing one address
/// (a server's workers listening on the same port).
#[derive(Clone, Debug, PartialEq)]
pub struct Socket {
    pub protocol: Protocol,
    pub state: SocketState,
    /// 「0.0.0.0:22」「[::1]:631」「*:5678」, as `ss` writes it.
    pub local: String,
    /// 「203.0.113.7:51234」, or 「0.0.0.0:*」 with no peer.
    pub peer: String,
    /// Empty when the login cannot see it (another user's, without root),
    /// and for a socket no process holds (in TIME_WAIT).
    pub processes: Vec<SocketProcess>,
    /// Whose it is: the name, or the uid when the host has no name for it.
    /// `None` for a socket no process holds.
    pub user: Option<String>,
    pub role: Role,
}

impl Socket {
    pub fn new(protocol: Protocol, state: SocketState, local: String, peer: String) -> Self {
        Self {
            protocol,
            state,
            local,
            peer,
            processes: Vec::new(),
            user: None,
            role: Role::Outbound,
        }
    }

    /// The port it is bound to, `None` for 「*」.
    pub fn port(&self) -> Option<u16> {
        port_of(&self.local)
    }

    /// 「sshd (PID 891)」「nginx (PID 913756 等 4 个)」, `None` with none.
    pub fn process_label(&self) -> Option<String> {
        let first = self.processes.first()?;
        Some(match self.processes.len() {
            1 => format!("{} (PID {})", first.name, first.pid),
            count => t!(
                "netstat.process.several",
                name = first.name,
                pid = first.pid,
                count = count
            )
            .into(),
        })
    }

    /// What the search looks in, lowercased; besides the state as the
    /// list says it, which follows the interface language.
    fn haystack(&self) -> String {
        let mut haystack = format!(
            "{} {} {} {}",
            self.protocol.label(),
            self.state.netstat_name(),
            self.local,
            self.peer,
        );
        for process in &self.processes {
            haystack.push_str(&format!(" {} {}", process.name, process.pid));
        }
        if let Some(user) = &self.user {
            haystack.push(' ');
            haystack.push_str(user);
        }
        haystack.to_lowercase()
    }
}

fn port_of(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

/// How many sockets there are, of all and of the two kinds the summary
/// names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub total: usize,
    pub listening: usize,
    pub connected: usize,
}

impl Counts {
    pub fn add(&mut self, state: SocketState, count: usize) {
        self.total += count;
        match state {
            SocketState::Listen => self.listening += count,
            SocketState::Established => self.connected += count,
            _ => {}
        }
    }

    /// 「共 96 条 · 15 个监听端口 · 21 条已连接」.
    pub fn summary(&self) -> String {
        format!(
            "{} · {} · {}",
            tn!("netstat.summary.total", self.total),
            tn!("netstat.summary.listening", self.listening),
            tn!("netstat.summary.connected", self.connected)
        )
    }
}

/// Whose processes a reading names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Processes {
    /// Every socket's: the login is root.
    All,
    /// Only the login's own sockets'.
    Own,
    /// None: the host has no `ss`, and `/proc/net` names no processes.
    None,
}

/// One reading of a host's sockets, in list order.
#[derive(Debug, PartialEq)]
pub struct Table {
    sockets: Vec<Socket>,
    /// Lowercased, beside each socket.
    haystacks: Vec<String>,
    counts: Counts,
    truncated: bool,
    processes: Processes,
}

impl Table {
    /// `sockets` as the host listed them. `counted`: how many the host
    /// counted, when it listed only so many; then there are more than
    /// `sockets`.
    pub fn new(sockets: Vec<Socket>, counted: Option<Counts>, processes: Processes) -> Self {
        let listed = sockets.len();
        let mut merged: Vec<Socket> = Vec::with_capacity(listed);
        let mut seen: HashMap<(Protocol, SocketState, String, String), usize> = HashMap::new();
        for socket in sockets {
            let key = (
                socket.protocol,
                socket.state,
                socket.local.clone(),
                socket.peer.clone(),
            );
            match seen.get(&key) {
                Some(&index) => {
                    let into = &mut merged[index];
                    for process in socket.processes {
                        if !into.processes.contains(&process) {
                            into.processes.push(process);
                        }
                    }
                    if into.user.is_none() {
                        into.user = socket.user;
                    }
                }
                None => {
                    seen.insert(key, merged.len());
                    merged.push(socket);
                }
            }
        }

        let listening: HashSet<(Protocol, u16)> = merged
            .iter()
            .filter(|socket| socket.state == SocketState::Listen)
            .filter_map(|socket| Some((socket.protocol, socket.port()?)))
            .collect();
        for socket in &mut merged {
            socket.role = if socket.state == SocketState::Listen {
                Role::Listening
            } else if socket
                .port()
                .is_some_and(|port| listening.contains(&(socket.protocol, port)))
            {
                Role::Inbound
            } else {
                Role::Outbound
            };
        }
        merged.sort_by(|a, b| {
            (a.state.rank(), a.protocol, a.port(), &a.local, &a.peer).cmp(&(
                b.state.rank(),
                b.protocol,
                b.port(),
                &b.local,
                &b.peer,
            ))
        });

        let truncated = counted.is_some_and(|counted| counted.total > listed);
        let counts = match counted {
            Some(counted) if truncated => counted,
            _ => merged.iter().fold(Counts::default(), |mut counts, socket| {
                counts.add(socket.state, 1);
                counts
            }),
        };
        Self {
            haystacks: merged.iter().map(Socket::haystack).collect(),
            sockets: merged,
            counts,
            truncated,
            processes,
        }
    }

    pub fn sockets(&self) -> &[Socket] {
        &self.sockets
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Whether the host had more sockets than it listed.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    pub fn processes(&self) -> Processes {
        self.processes
    }

    /// The sockets `filter` lets through, by their place in `sockets()`.
    pub fn filtered(&self, filter: &Filter) -> Vec<usize> {
        let query = filter.query.trim().to_lowercase();
        // The states as the list says them now, which the search finds.
        let mut labels: HashMap<SocketState, bool> = HashMap::new();
        let mut label_matches = |state: SocketState| {
            *labels
                .entry(state)
                .or_insert_with(|| state.label().to_lowercase().contains(&query))
        };
        self.sockets
            .iter()
            .zip(&self.haystacks)
            .enumerate()
            .filter(|(_, (socket, haystack))| {
                filter.protocol.lets_through(socket.protocol)
                    && filter.state.lets_through(socket.state)
                    && (query.is_empty()
                        || haystack.contains(&query)
                        || label_matches(socket.state))
            })
            .map(|(index, _)| index)
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProtocolFilter {
    #[default]
    All,
    Tcp,
    Udp,
}

impl ProtocolFilter {
    pub const ALL: [ProtocolFilter; 3] = [
        ProtocolFilter::All,
        ProtocolFilter::Tcp,
        ProtocolFilter::Udp,
    ];

    pub fn label(self) -> SharedString {
        match self {
            ProtocolFilter::All => t!("netstat.filter.all_protocols"),
            ProtocolFilter::Tcp => "TCP".into(),
            ProtocolFilter::Udp => "UDP".into(),
        }
    }

    fn lets_through(self, protocol: Protocol) -> bool {
        match self {
            ProtocolFilter::All => true,
            ProtocolFilter::Tcp => protocol == Protocol::Tcp,
            ProtocolFilter::Udp => protocol == Protocol::Udp,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StateFilter {
    #[default]
    All,
    Listen,
    Established,
    TimeWait,
    CloseWait,
    /// The states not named above.
    Other,
}

impl StateFilter {
    pub const ALL: [StateFilter; 6] = [
        StateFilter::All,
        StateFilter::Listen,
        StateFilter::Established,
        StateFilter::TimeWait,
        StateFilter::CloseWait,
        StateFilter::Other,
    ];

    pub fn label(self) -> SharedString {
        match self {
            StateFilter::All => t!("netstat.filter.all_states"),
            StateFilter::Listen => SocketState::Listen.label(),
            StateFilter::Established => SocketState::Established.label(),
            StateFilter::TimeWait => SocketState::TimeWait.label(),
            StateFilter::CloseWait => SocketState::CloseWait.label(),
            StateFilter::Other => t!("netstat.filter.other_states"),
        }
    }

    fn lets_through(self, state: SocketState) -> bool {
        match self {
            StateFilter::All => true,
            StateFilter::Listen => state == SocketState::Listen,
            StateFilter::Established => state == SocketState::Established,
            StateFilter::TimeWait => state == SocketState::TimeWait,
            StateFilter::CloseWait => state == SocketState::CloseWait,
            StateFilter::Other => !matches!(
                state,
                SocketState::Listen
                    | SocketState::Established
                    | SocketState::TimeWait
                    | SocketState::CloseWait
            ),
        }
    }
}

/// What the list is narrowed to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub protocol: ProtocolFilter,
    pub state: StateFilter,
    /// Looked for in the addresses, the state, the processes and their
    /// PIDs and the user, ignoring case.
    pub query: String,
}

impl Filter {
    /// Whether it narrows the list at all.
    pub fn is_active(&self) -> bool {
        self.protocol != ProtocolFilter::All
            || self.state != StateFilter::All
            || !self.query.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(protocol: Protocol, state: SocketState, local: &str, peer: &str) -> Socket {
        Socket::new(protocol, state, local.into(), peer.into())
    }

    fn process(name: &str, pid: u32) -> SocketProcess {
        SocketProcess {
            name: name.into(),
            pid,
        }
    }

    fn sample() -> Table {
        let mut sshd = socket(
            Protocol::Tcp,
            SocketState::Listen,
            "0.0.0.0:22",
            "0.0.0.0:*",
        );
        sshd.processes = vec![process("sshd", 891)];
        sshd.user = Some("root".into());
        let worker = |pid| {
            let mut nginx = socket(
                Protocol::Tcp,
                SocketState::Listen,
                "0.0.0.0:80",
                "0.0.0.0:*",
            );
            nginx.processes = vec![process("nginx", pid)];
            nginx.user = Some("nginx".into());
            nginx
        };
        let (first, second) = (worker(100), worker(101));
        Table::new(
            vec![
                socket(
                    Protocol::Tcp,
                    SocketState::TimeWait,
                    "10.0.0.5:80",
                    "198.51.100.2:6000",
                ),
                socket(
                    Protocol::Tcp,
                    SocketState::Established,
                    "10.0.0.5:48210",
                    "140.82.112.4:443",
                ),
                socket(
                    Protocol::Tcp,
                    SocketState::Established,
                    "10.0.0.5:22",
                    "203.0.113.7:51234",
                ),
                first,
                socket(
                    Protocol::Udp,
                    SocketState::Listen,
                    "127.0.0.53%lo:53",
                    "0.0.0.0:*",
                ),
                sshd,
                second,
                socket(
                    Protocol::Tcp,
                    SocketState::CloseWait,
                    "10.0.0.5:39000",
                    "10.0.0.9:3306",
                ),
            ],
            None,
            Processes::All,
        )
    }

    #[test]
    fn what_listens_comes_first_and_shares_of_a_port_make_one_line() {
        let table = sample();
        let labels: Vec<SharedString> = table
            .sockets()
            .iter()
            .map(|socket| socket.state.label())
            .collect();
        let lines: Vec<(&str, &str, Role)> = table
            .sockets()
            .iter()
            .zip(&labels)
            .map(|(socket, label)| (label.as_ref(), socket.local.as_str(), socket.role))
            .collect();
        assert_eq!(
            lines,
            [
                ("监听", "0.0.0.0:22", Role::Listening),
                ("监听", "0.0.0.0:80", Role::Listening),
                ("监听", "127.0.0.53%lo:53", Role::Listening),
                ("已连接", "10.0.0.5:22", Role::Inbound),
                ("已连接", "10.0.0.5:48210", Role::Outbound),
                ("CLOSE_WAIT", "10.0.0.5:39000", Role::Outbound),
                ("TIME_WAIT", "10.0.0.5:80", Role::Inbound),
            ]
        );
        let nginx = &table.sockets()[1];
        assert_eq!(
            nginx.process_label().as_deref(),
            Some("nginx (PID 100 等 2 个)")
        );
        assert_eq!(
            table.counts(),
            Counts {
                total: 7,
                listening: 3,
                connected: 2
            }
        );
        assert!(!table.truncated());
        assert_eq!(
            table.counts().summary(),
            "共 7 条 · 3 个监听端口 · 2 条已连接"
        );
    }

    #[test]
    fn a_host_that_counted_more_than_it_listed_is_said_to_have_more() {
        let counted = Counts {
            total: 50_000,
            listening: 12,
            connected: 9_000,
        };
        let table = Table::new(
            vec![socket(
                Protocol::Tcp,
                SocketState::Listen,
                "0.0.0.0:22",
                "0.0.0.0:*",
            )],
            Some(counted),
            Processes::All,
        );
        assert!(table.truncated());
        assert_eq!(table.counts(), counted);

        // A count that matches the list is no news.
        let table = Table::new(
            vec![socket(
                Protocol::Tcp,
                SocketState::Listen,
                "0.0.0.0:22",
                "0.0.0.0:*",
            )],
            Some(Counts {
                total: 1,
                listening: 1,
                connected: 0,
            }),
            Processes::All,
        );
        assert!(!table.truncated());
    }

    #[test]
    fn the_search_finds_addresses_states_processes_pids_and_users() {
        let table = sample();
        let local = |filter: Filter| -> Vec<String> {
            table
                .filtered(&filter)
                .into_iter()
                .map(|index| table.sockets()[index].local.clone())
                .collect()
        };
        let query = |text: &str| Filter {
            query: text.into(),
            ..Filter::default()
        };
        assert_eq!(local(query("NGINX")), ["0.0.0.0:80"]);
        assert_eq!(local(query("101")), ["0.0.0.0:80"]);
        assert_eq!(local(query(":22")), ["0.0.0.0:22", "10.0.0.5:22"]);
        assert_eq!(
            local(query("established")),
            ["10.0.0.5:22", "10.0.0.5:48210"]
        );
        assert_eq!(local(query("time_wait")), ["10.0.0.5:80"]);
        assert_eq!(local(query("203.0.113")), ["10.0.0.5:22"]);
        assert_eq!(local(query(" root ")), ["0.0.0.0:22"]);
        assert_eq!(local(Filter::default()).len(), 7);

        let udp = Filter {
            protocol: ProtocolFilter::Udp,
            ..Filter::default()
        };
        assert_eq!(local(udp), ["127.0.0.53%lo:53"]);
        let other = Filter {
            state: StateFilter::Other,
            ..Filter::default()
        };
        assert!(local(other).is_empty());
        let close_wait = Filter {
            state: StateFilter::CloseWait,
            ..Filter::default()
        };
        assert!(close_wait.is_active());
        assert_eq!(local(close_wait), ["10.0.0.5:39000"]);
        assert!(!query("  ").is_active());
    }

    #[test]
    fn states_read_from_ss_and_from_proc_agree() {
        for (name, hex, state) in [
            ("LISTEN", "0A", SocketState::Listen),
            ("ESTAB", "01", SocketState::Established),
            ("TIME-WAIT", "06", SocketState::TimeWait),
            ("CLOSE-WAIT", "08", SocketState::CloseWait),
            ("SYN-RECV", "03", SocketState::SynReceived),
            ("FIN-WAIT-2", "05", SocketState::FinWait2),
        ] {
            assert_eq!(SocketState::from_ss(Protocol::Tcp, name), Some(state));
            assert_eq!(SocketState::from_proc(Protocol::Tcp, hex), Some(state));
        }
        assert_eq!(
            SocketState::from_ss(Protocol::Udp, "UNCONN"),
            Some(SocketState::Listen)
        );
        assert_eq!(
            SocketState::from_proc(Protocol::Udp, "07"),
            Some(SocketState::Listen)
        );
        assert_eq!(SocketState::from_ss(Protocol::Tcp, "Netid"), None);
    }
}
