//! Port-forwarding rules. They live with the hosts because they are stored
//! in the same database and go with their host when it is deleted; the
//! `forward` module that runs them depends on this one, never the reverse.

use std::fmt;

use gpui_kit::SharedString;

use super::HostId;
use crate::i18n::t;

/// Stable identity of a forwarding rule. Never reused within a process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ForwardId(pub u64);

/// Which end listens and which end the connections come out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ForwardKind {
    /// `ssh -L`: this machine listens, the server connects to the target.
    #[default]
    Local,
    /// `ssh -R`: the server listens, this machine connects to the target.
    Remote,
    /// `ssh -D`: this machine listens as a SOCKS proxy, the server connects
    /// wherever each client asks.
    Dynamic,
}

impl ForwardKind {
    /// Every kind, in the order the form lists them.
    pub const ALL: [ForwardKind; 3] = [
        ForwardKind::Local,
        ForwardKind::Remote,
        ForwardKind::Dynamic,
    ];

    pub fn label(self) -> SharedString {
        match self {
            ForwardKind::Local => t!("host.forward_kind.local"),
            ForwardKind::Remote => t!("host.forward_kind.remote"),
            ForwardKind::Dynamic => t!("host.forward_kind.dynamic"),
        }
    }

    /// The stored spelling. Kept separate from `label` so translating the UI
    /// cannot rewrite what is already in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            ForwardKind::Local => "local",
            ForwardKind::Remote => "remote",
            ForwardKind::Dynamic => "dynamic",
        }
    }

    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "local" => Some(ForwardKind::Local),
            "remote" => Some(ForwardKind::Remote),
            "dynamic" => Some(ForwardKind::Dynamic),
            _ => None,
        }
    }

    /// A dynamic forward has no fixed target: each client names its own.
    pub fn has_target(self) -> bool {
        !matches!(self, ForwardKind::Dynamic)
    }
}

/// A host and a port: where a forward listens, or where it connects.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ForwardEndpoint {
    pub host: SharedString,
    pub port: u16,
}

impl ForwardEndpoint {
    pub fn new(host: impl Into<SharedString>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }

    /// The host as a resolver wants it: without the brackets a URL puts
    /// around an IPv6 literal.
    pub fn bare_host(&self) -> &str {
        bare_host(&self.host)
    }

    /// Whether only the machine itself can reach this address.
    pub fn is_loopback(&self) -> bool {
        let host = self.bare_host();
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    }
}

/// `host:port`, with an IPv6 literal in brackets so the port stays readable.
impl fmt::Display for ForwardEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = self.bare_host();
        if host.contains(':') {
            write!(formatter, "[{host}]:{}", self.port)
        } else {
            write!(formatter, "{host}:{}", self.port)
        }
    }
}

/// The host without the brackets a URL puts around an IPv6 literal.
fn bare_host(host: &str) -> &str {
    let host = host.trim();
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

/// The address a new rule listens on unless the user changes it.
pub const DEFAULT_BIND_HOST: &str = "127.0.0.1";

/// Why a draft cannot be saved. `Display` is the text the form shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardDraftError {
    BindHost,
    BindPort,
    TargetHost,
    TargetPort,
}

impl fmt::Display for ForwardDraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&match self {
            ForwardDraftError::BindHost => t!("host.forward_draft.bind_host"),
            ForwardDraftError::BindPort => t!("host.forward_draft.bind_port"),
            ForwardDraftError::TargetHost => t!("host.forward_draft.target_host"),
            ForwardDraftError::TargetPort => t!("host.forward_draft.target_port"),
        })
    }
}

impl std::error::Error for ForwardDraftError {}

/// The values the forward form commits.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ForwardDraft {
    /// Optional. An empty name shows the rule's summary instead.
    pub name: SharedString,
    pub kind: ForwardKind,
    /// The host whose server the forward goes through.
    pub host: HostId,
    /// Where the forward listens: on this machine for local and dynamic
    /// forwards, on the server for a remote one.
    pub bind: ForwardEndpoint,
    /// Where connections end up. `None` exactly for a dynamic forward.
    pub target: Option<ForwardEndpoint>,
    /// Start the forward when the application launches.
    pub auto_start: bool,
}

impl ForwardDraft {
    pub fn new(
        kind: ForwardKind,
        host: HostId,
        bind: ForwardEndpoint,
        target: Option<ForwardEndpoint>,
    ) -> Self {
        Self {
            name: SharedString::default(),
            kind,
            host,
            bind,
            target,
            auto_start: false,
        }
    }

    pub fn with_name(mut self, name: impl Into<SharedString>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_auto_start(mut self, auto_start: bool) -> Self {
        self.auto_start = auto_start;
        self
    }

    /// The draft with surrounding blanks removed and the target a dynamic
    /// forward cannot have dropped, or the first thing wrong with it.
    ///
    /// Two rules may name the same listening port: they can take turns, and
    /// starting the second while the first runs reports the port as taken.
    pub fn validated(mut self) -> Result<Self, ForwardDraftError> {
        self.name = self.name.trim().to_string().into();
        self.bind.host = valid_host(&self.bind.host).ok_or(ForwardDraftError::BindHost)?;
        if self.bind.port == 0 {
            return Err(ForwardDraftError::BindPort);
        }
        self.target = match (self.kind.has_target(), self.target) {
            (false, _) => None,
            (true, None) => return Err(ForwardDraftError::TargetHost),
            (true, Some(mut target)) => {
                target.host = valid_host(&target.host).ok_or(ForwardDraftError::TargetHost)?;
                if target.port == 0 {
                    return Err(ForwardDraftError::TargetPort);
                }
                Some(target)
            }
        };
        Ok(self)
    }
}

fn valid_host(host: &str) -> Option<SharedString> {
    let host = host.trim();
    (!host.is_empty() && !host.contains(char::is_whitespace)).then(|| host.to_string().into())
}

/// A saved forwarding rule. Whether it is running is runtime state that the
/// `forward` module keeps; nothing here says so.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ForwardRule {
    pub id: ForwardId,
    pub name: SharedString,
    pub kind: ForwardKind,
    pub host: HostId,
    pub bind: ForwardEndpoint,
    pub target: Option<ForwardEndpoint>,
    pub auto_start: bool,
    /// Position in the forward list.
    pub sort_order: i64,
}

impl ForwardRule {
    pub fn new(id: ForwardId, draft: ForwardDraft) -> Self {
        Self {
            id,
            name: draft.name,
            kind: draft.kind,
            host: draft.host,
            bind: draft.bind,
            target: draft.target,
            auto_start: draft.auto_start,
            sort_order: 0,
        }
    }

    /// The editable fields, for pre-filling the forward form.
    pub fn draft(&self) -> ForwardDraft {
        ForwardDraft {
            name: self.name.clone(),
            kind: self.kind,
            host: self.host,
            bind: self.bind.clone(),
            target: self.target.clone(),
            auto_start: self.auto_start,
        }
    }

    /// The rule in one line: `8080 → db.internal:3306`, `9000 ← localhost:3000`
    /// or `SOCKS 1080`. The arrow points the way connections travel across
    /// the tunnel; the listening address is left out while it is the default.
    pub fn summary(&self) -> String {
        let bind = if self.bind.host == DEFAULT_BIND_HOST {
            self.bind.port.to_string()
        } else {
            self.bind.to_string()
        };
        match (self.kind, &self.target) {
            (ForwardKind::Dynamic, _) | (_, None) => format!("SOCKS {bind}"),
            (ForwardKind::Local, Some(target)) => format!("{bind} → {target}"),
            (ForwardKind::Remote, Some(target)) => format!("{bind} ← {target}"),
        }
    }

    /// What the list calls the rule: its name, or its summary without one.
    pub fn title(&self) -> SharedString {
        if self.name.is_empty() {
            self.summary().into()
        } else {
            self.name.clone()
        }
    }

    /// Whether applying `draft` changes what a running forward does, as
    /// opposed to its name or whether it starts with the application.
    pub fn needs_restart(&self, draft: &ForwardDraft) -> bool {
        self.kind != draft.kind
            || self.host != draft.host
            || self.bind != draft.bind
            || self.target != draft.target
    }
}

/// Whether a rule matches what was typed into the forward list's search box:
/// its name, its addresses, or the name of the host it goes through.
pub fn matches_forward_query(rule: &ForwardRule, host_name: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    [
        rule.name.to_string(),
        rule.summary(),
        rule.kind.label().to_string(),
        host_name.to_string(),
    ]
    .iter()
    .any(|text| text.to_lowercase().contains(&query))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local() -> ForwardDraft {
        ForwardDraft::new(
            ForwardKind::Local,
            HostId(1),
            ForwardEndpoint::new(DEFAULT_BIND_HOST, 8080),
            Some(ForwardEndpoint::new("db.internal", 3306)),
        )
    }

    #[test]
    fn every_kind_round_trips_through_the_database_spelling() {
        for kind in ForwardKind::ALL {
            assert_eq!(ForwardKind::from_stored(kind.as_str()), Some(kind));
        }
        assert_eq!(ForwardKind::from_stored("socks"), None);
        assert!(ForwardKind::Local.has_target());
        assert!(ForwardKind::Remote.has_target());
        assert!(!ForwardKind::Dynamic.has_target());
    }

    #[test]
    fn a_draft_is_trimmed_and_checked() {
        let mut draft = local().with_name("  数据库  ");
        draft.bind.host = " 127.0.0.1 ".into();
        let saved = draft.validated().unwrap();
        assert_eq!(saved.name.as_ref(), "数据库");
        assert_eq!(saved.bind.host.as_ref(), "127.0.0.1");

        let mut blank = local();
        blank.bind.host = "  ".into();
        assert_eq!(blank.validated(), Err(ForwardDraftError::BindHost));
        let mut spaced = local();
        spaced.bind.host = "my host".into();
        assert_eq!(spaced.validated(), Err(ForwardDraftError::BindHost));
        let mut zero = local();
        zero.bind.port = 0;
        assert_eq!(zero.validated(), Err(ForwardDraftError::BindPort));

        let mut no_target = local();
        no_target.target = None;
        assert_eq!(no_target.validated(), Err(ForwardDraftError::TargetHost));
        let mut bad_target = local();
        bad_target.target = Some(ForwardEndpoint::new("", 80));
        assert_eq!(bad_target.validated(), Err(ForwardDraftError::TargetHost));
        let mut zero_target = local();
        zero_target.target = Some(ForwardEndpoint::new("db", 0));
        assert_eq!(zero_target.validated(), Err(ForwardDraftError::TargetPort));
    }

    #[test]
    fn a_dynamic_forward_never_keeps_a_target() {
        let mut draft = local();
        draft.kind = ForwardKind::Dynamic;
        assert_eq!(draft.validated().unwrap().target, None);
    }

    #[test]
    fn the_summary_shows_the_direction_and_hides_the_default_address() {
        let rule = ForwardRule::new(ForwardId(1), local());
        assert_eq!(rule.summary(), "8080 → db.internal:3306");
        assert_eq!(rule.title().as_ref(), "8080 → db.internal:3306");

        let mut remote = local();
        remote.kind = ForwardKind::Remote;
        remote.bind = ForwardEndpoint::new("0.0.0.0", 9000);
        remote.target = Some(ForwardEndpoint::new("localhost", 3000));
        let rule = ForwardRule::new(ForwardId(2), remote.with_name("演示站"));
        assert_eq!(rule.summary(), "0.0.0.0:9000 ← localhost:3000");
        assert_eq!(rule.title().as_ref(), "演示站");

        let mut dynamic = local();
        dynamic.kind = ForwardKind::Dynamic;
        dynamic.bind.port = 1080;
        let rule = ForwardRule::new(ForwardId(3), dynamic.validated().unwrap());
        assert_eq!(rule.summary(), "SOCKS 1080");
    }

    #[test]
    fn ipv6_literals_are_bracketed_and_loopback_is_recognised() {
        assert_eq!(ForwardEndpoint::new("::1", 80).to_string(), "[::1]:80");
        assert_eq!(ForwardEndpoint::new("[::1]", 80).to_string(), "[::1]:80");
        assert!(ForwardEndpoint::new("::1", 80).is_loopback());
        assert!(ForwardEndpoint::new("127.0.0.1", 80).is_loopback());
        assert!(ForwardEndpoint::new("127.8.8.8", 80).is_loopback());
        assert!(ForwardEndpoint::new("LocalHost", 80).is_loopback());
        assert!(!ForwardEndpoint::new("0.0.0.0", 80).is_loopback());
        assert!(!ForwardEndpoint::new("192.168.1.2", 80).is_loopback());
    }

    #[test]
    fn only_changes_to_what_the_forward_does_need_a_restart() {
        let rule = ForwardRule::new(ForwardId(1), local());
        assert!(!rule.needs_restart(&local().with_name("x").with_auto_start(true)));
        let mut other_port = local();
        other_port.bind.port = 8081;
        assert!(rule.needs_restart(&other_port));
        let mut through_another = local();
        through_another.host = HostId(2);
        assert!(rule.needs_restart(&through_another));
        let mut other_target = local();
        other_target.target = Some(ForwardEndpoint::new("db.internal", 5432));
        assert!(rule.needs_restart(&other_target));
    }

    #[test]
    fn the_search_looks_at_the_name_the_addresses_and_the_host() {
        let rule = ForwardRule::new(ForwardId(1), local().with_name("数据库"));
        assert!(matches_forward_query(&rule, "web-01", ""));
        assert!(matches_forward_query(&rule, "web-01", "数据"));
        assert!(matches_forward_query(&rule, "web-01", "3306"));
        assert!(matches_forward_query(&rule, "web-01", "WEB"));
        assert!(matches_forward_query(&rule, "web-01", "本地"));
        assert!(!matches_forward_query(&rule, "web-01", "redis"));
    }
}
