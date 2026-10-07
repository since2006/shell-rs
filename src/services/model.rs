//! What 系统服务 reads off a host, and how its tabs, groups and search
//! pick from it.

use gpui_kit::SharedString;

use crate::i18n::t;

/// Where a service's unit file lives, which the list groups by.
const CUSTOM_UNITS: &str = "/etc/systemd/system/";

/// What a service is doing, as systemd's `ActiveState` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveState {
    Active,
    Reloading,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Other,
}

impl ActiveState {
    pub fn parse(state: &str) -> Self {
        match state {
            "active" => ActiveState::Active,
            "reloading" => ActiveState::Reloading,
            "inactive" => ActiveState::Inactive,
            "failed" => ActiveState::Failed,
            "activating" => ActiveState::Activating,
            "deactivating" => ActiveState::Deactivating,
            _ => ActiveState::Other,
        }
    }

    pub fn label(self) -> SharedString {
        match self {
            ActiveState::Active => t!("services.active.active"),
            ActiveState::Reloading => t!("services.active.reloading"),
            ActiveState::Inactive | ActiveState::Other => t!("services.active.inactive"),
            ActiveState::Failed => t!("services.active.failed"),
            ActiveState::Activating => t!("services.active.activating"),
            ActiveState::Deactivating => t!("services.active.deactivating"),
        }
    }

    /// Which tab it is under.
    pub fn kind(self) -> ServiceKind {
        match self {
            ActiveState::Active
            | ActiveState::Reloading
            | ActiveState::Activating
            | ActiveState::Deactivating => ServiceKind::Running,
            ActiveState::Failed => ServiceKind::Failed,
            ActiveState::Inactive | ActiveState::Other => ServiceKind::Stopped,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceKind {
    Running,
    Stopped,
    Failed,
}

/// The finer state under `ActiveState`, as systemd's `SubState` says: 「已退出」
/// for a one-shot service that did its work.
pub fn sub_state_label(state: &str) -> SharedString {
    match state {
        "running" => t!("services.sub.running"),
        "exited" => t!("services.sub.exited"),
        "dead" => t!("services.sub.dead"),
        "failed" => t!("services.sub.failed"),
        "waiting" => t!("services.sub.waiting"),
        "listening" => t!("services.sub.listening"),
        "reload" => t!("services.sub.reload"),
        "auto-restart" => t!("services.sub.auto_restart"),
        "condition" => t!("services.sub.condition"),
        state if state.starts_with("start") => t!("services.sub.starting"),
        state if state.starts_with("stop") || state.starts_with("final") => {
            t!("services.sub.stopping")
        }
        state => state.to_owned().into(),
    }
}

/// Whether it starts at boot, as systemd's `UnitFileState` says; `None`
/// with nothing to say.
pub fn file_state_label(state: &str) -> Option<SharedString> {
    Some(match state {
        "enabled" => t!("services.file.enabled"),
        "enabled-runtime" => t!("services.file.enabled_runtime"),
        "disabled" => t!("services.file.disabled"),
        "static" => t!("services.file.static"),
        "masked" | "masked-runtime" => t!("services.file.masked"),
        "generated" => t!("services.file.generated"),
        "indirect" => t!("services.file.indirect"),
        "linked" | "linked-runtime" => t!("services.file.linked"),
        "transient" => t!("services.file.transient"),
        "alias" => t!("services.file.alias"),
        "bad" => t!("services.file.bad"),
        _ => return None,
    })
}

/// Whether the unit file was found and read, as systemd's `LoadState` says.
pub fn load_state_label(state: &str) -> SharedString {
    match state {
        "loaded" => t!("services.load.loaded"),
        "not-found" => t!("services.load.not_found"),
        "masked" => t!("services.load.masked"),
        "error" => t!("services.load.error"),
        "bad-setting" => t!("services.load.bad_setting"),
        state => state.to_owned().into(),
    }
}

/// How the machine as a whole is doing, as `systemctl is-system-running`
/// says: 「降级运行」 when some unit failed.
pub fn system_state_label(state: &str) -> SharedString {
    match state {
        "running" => t!("services.system.running"),
        "degraded" => t!("services.system.degraded"),
        "starting" => t!("services.system.starting"),
        "initializing" => t!("services.system.initializing"),
        "maintenance" => t!("services.system.maintenance"),
        "stopping" => t!("services.system.stopping"),
        "offline" => t!("services.system.offline"),
        state => state.to_owned().into(),
    }
}

/// What can be done to a service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServiceCommand {
    Start,
    Stop,
    Restart,
    /// Start it at boot.
    Enable,
    /// No longer start it at boot.
    Disable,
}

impl ServiceCommand {
    /// The `systemctl` verb.
    pub fn verb(self) -> &'static str {
        match self {
            ServiceCommand::Start => "start",
            ServiceCommand::Stop => "stop",
            ServiceCommand::Restart => "restart",
            ServiceCommand::Enable => "enable",
            ServiceCommand::Disable => "disable",
        }
    }

    /// The button: 「停止」「禁用开机启动」.
    pub fn label(self) -> SharedString {
        match self {
            ServiceCommand::Start => t!("services.command.start"),
            ServiceCommand::Stop => t!("services.command.stop"),
            ServiceCommand::Restart => t!("services.command.restart"),
            ServiceCommand::Enable => t!("services.command.enable"),
            ServiceCommand::Disable => t!("services.command.disable"),
        }
    }
}

/// A service as the list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    /// 「nginx.service」.
    pub name: String,
    pub description: String,
    pub active: ActiveState,
    /// systemd's `UnitFileState`: 「enabled」「static」, empty without a
    /// unit file.
    pub file_state: String,
    /// Its unit file is the administrator's, in `/etc/systemd/system`.
    pub custom: bool,
}

impl Service {
    pub fn new(name: &str, description: &str, active: &str, file_state: &str, path: &str) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            active: ActiveState::parse(active),
            file_state: file_state.to_owned(),
            custom: path.starts_with(CUSTOM_UNITS),
        }
    }

    /// The commands its buttons offer, the one that matters first: stop
    /// and restart while it runs, start otherwise.
    pub fn commands(&self) -> &'static [ServiceCommand] {
        match self.active.kind() {
            ServiceKind::Running => &[ServiceCommand::Stop, ServiceCommand::Restart],
            ServiceKind::Stopped | ServiceKind::Failed => &[ServiceCommand::Start],
        }
    }

    /// Enable or disable, when its unit file can be either.
    pub fn boot_command(&self) -> Option<ServiceCommand> {
        match self.file_state.as_str() {
            "enabled" | "enabled-runtime" => Some(ServiceCommand::Disable),
            "disabled" => Some(ServiceCommand::Enable),
            _ => None,
        }
    }

    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.name.to_lowercase().contains(query)
            || self.description.to_lowercase().contains(query)
    }
}

/// The tabs over the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServiceFilter {
    #[default]
    All,
    Running,
    Stopped,
    Failed,
}

impl ServiceFilter {
    pub const ALL: [ServiceFilter; 4] = [
        ServiceFilter::All,
        ServiceFilter::Running,
        ServiceFilter::Stopped,
        ServiceFilter::Failed,
    ];

    pub fn label(self) -> SharedString {
        match self {
            ServiceFilter::All => t!("services.filter.all"),
            ServiceFilter::Running => t!("services.filter.running"),
            ServiceFilter::Stopped => t!("services.filter.stopped"),
            ServiceFilter::Failed => t!("services.filter.failed"),
        }
    }

    fn lets_through(self, service: &Service) -> bool {
        match self {
            ServiceFilter::All => true,
            ServiceFilter::Running => service.active.kind() == ServiceKind::Running,
            ServiceFilter::Stopped => service.active.kind() == ServiceKind::Stopped,
            ServiceFilter::Failed => service.active.kind() == ServiceKind::Failed,
        }
    }
}

/// A line of the list: a group's heading, or a service by its place in
/// `ServiceTable::services`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceRow {
    Group { custom: bool, count: usize },
    Service(usize),
}

/// One reading of a host's services, by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServiceTable {
    services: Vec<Service>,
    /// 「systemd 249」.
    version: Option<String>,
    /// `systemctl is-system-running`: 「degraded」.
    state: Option<String>,
}

impl ServiceTable {
    pub fn new(mut services: Vec<Service>, version: Option<String>, state: Option<String>) -> Self {
        services.sort_by(|a, b| a.name.cmp(&b.name));
        services.dedup_by(|a, b| a.name == b.name);
        Self {
            services,
            version,
            state,
        }
    }

    pub fn services(&self) -> &[Service] {
        &self.services
    }

    pub fn service(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|service| service.name == name)
    }

    /// 「systemd 249 · 降级运行」.
    pub fn summary(&self) -> String {
        [
            self.version.clone(),
            self.state
                .as_deref()
                .map(|state| system_state_label(state).into()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }

    /// Whether some unit failed, so the machine is not quite well.
    pub fn degraded(&self) -> bool {
        self.state.as_deref() == Some("degraded")
    }

    /// How many services `query` finds under each tab, in the tabs' order.
    pub fn counts(&self, query: &str) -> [usize; 4] {
        let query = query.trim().to_lowercase();
        ServiceFilter::ALL.map(|filter| {
            self.services
                .iter()
                .filter(|service| filter.lets_through(service) && service.matches(&query))
                .count()
        })
    }

    /// The list under `filter` for `query` (in the name or description,
    /// ignoring case): the administrator's own services, then the rest,
    /// each group under its heading; a group with none is left out.
    pub fn rows(&self, filter: ServiceFilter, query: &str) -> Vec<ServiceRow> {
        let query = query.trim().to_lowercase();
        let mut rows = Vec::new();
        for custom in [true, false] {
            let services: Vec<usize> = self
                .services
                .iter()
                .enumerate()
                .filter(|(_, service)| {
                    service.custom == custom
                        && filter.lets_through(service)
                        && service.matches(&query)
                })
                .map(|(index, _)| index)
                .collect();
            if !services.is_empty() {
                rows.push(ServiceRow::Group {
                    custom,
                    count: services.len(),
                });
                rows.extend(services.into_iter().map(ServiceRow::Service));
            }
        }
        rows
    }
}

/// A service's state in full, for its details, as `systemctl show` says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServiceStatus {
    pub description: String,
    pub load: String,
    pub active: String,
    pub sub: String,
    pub file_state: String,
    pub main_pid: Option<u32>,
    /// Bytes; `None` without memory accounting.
    pub memory: Option<u64>,
    pub tasks: Option<u64>,
    pub restarts: Option<u64>,
    pub exit_status: Option<i64>,
    /// 「Tue 2026-09-15 17:49:07 CST」, as the host writes it.
    pub started: Option<String>,
    pub stopped: Option<String>,
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> ServiceTable {
        ServiceTable::new(
            vec![
                Service::new(
                    "socialc.service",
                    "Social Callback Proxy Service",
                    "active",
                    "enabled",
                    "/etc/systemd/system/socialc.service",
                ),
                Service::new(
                    "ssh.service",
                    "OpenBSD Secure Shell server",
                    "active",
                    "enabled",
                    "/lib/systemd/system/ssh.service",
                ),
                Service::new(
                    "apport.service",
                    "LSB: automatic crash report generation",
                    "active",
                    "generated",
                    "/run/systemd/generator.late/apport.service",
                ),
                Service::new(
                    "ModemManager.service",
                    "Modem Manager",
                    "inactive",
                    "disabled",
                    "/lib/systemd/system/ModemManager.service",
                ),
                Service::new(
                    "certbot.service",
                    "Certbot",
                    "failed",
                    "static",
                    "/etc/systemd/system/certbot.service",
                ),
                // Read twice under two names, once as itself.
                Service::new(
                    "ssh.service",
                    "OpenBSD Secure Shell server",
                    "active",
                    "enabled",
                    "/lib/systemd/system/ssh.service",
                ),
            ],
            Some("systemd 249".into()),
            Some("degraded".into()),
        )
    }

    fn names(table: &ServiceTable, rows: &[ServiceRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                ServiceRow::Group {
                    custom: true,
                    count,
                } => format!("自定义服务 {count}"),
                ServiceRow::Group {
                    custom: false,
                    count,
                } => format!("系统服务 {count}"),
                ServiceRow::Service(index) => table.services()[*index].name.clone(),
            })
            .collect()
    }

    #[test]
    fn the_administrators_services_come_first_under_each_tab() {
        let table = table();
        assert_eq!(table.summary(), "systemd 249 · 降级运行");
        assert!(table.degraded());
        assert_eq!(table.counts(""), [5, 3, 1, 1]);
        assert_eq!(
            names(&table, &table.rows(ServiceFilter::All, "")),
            [
                "自定义服务 2",
                "certbot.service",
                "socialc.service",
                "系统服务 3",
                "ModemManager.service",
                "apport.service",
                "ssh.service",
            ]
        );
        assert_eq!(
            names(&table, &table.rows(ServiceFilter::Running, "")),
            [
                "自定义服务 1",
                "socialc.service",
                "系统服务 2",
                "apport.service",
                "ssh.service",
            ]
        );
        assert_eq!(
            names(&table, &table.rows(ServiceFilter::Failed, "")),
            ["自定义服务 1", "certbot.service"]
        );
    }

    #[test]
    fn the_search_finds_names_and_descriptions() {
        let table = table();
        assert_eq!(
            names(&table, &table.rows(ServiceFilter::All, " SECURE ")),
            ["系统服务 1", "ssh.service"]
        );
        assert_eq!(
            names(&table, &table.rows(ServiceFilter::All, "modem")),
            ["系统服务 1", "ModemManager.service"]
        );
        assert_eq!(table.counts("cert"), [1, 0, 0, 1]);
        assert!(table.rows(ServiceFilter::Running, "cert").is_empty());
    }

    #[test]
    fn a_service_offers_what_its_state_allows() {
        let table = table();
        let service = |name: &str| table.service(name).expect(name);
        let ssh = service("ssh.service");
        assert_eq!(
            ssh.commands(),
            [ServiceCommand::Stop, ServiceCommand::Restart]
        );
        assert_eq!(ssh.boot_command(), Some(ServiceCommand::Disable));
        let modem = service("ModemManager.service");
        assert_eq!(modem.commands(), [ServiceCommand::Start]);
        assert_eq!(modem.boot_command(), Some(ServiceCommand::Enable));
        // Neither a static unit nor a generated one can be enabled.
        assert_eq!(service("certbot.service").boot_command(), None);
        assert_eq!(service("apport.service").boot_command(), None);
        assert_eq!(
            service("certbot.service").commands(),
            [ServiceCommand::Start]
        );
    }

    #[test]
    fn states_read_in_chinese() {
        assert_eq!(sub_state_label("exited"), "已退出");
        assert_eq!(sub_state_label("stop-sigterm"), "正在停止");
        assert_eq!(sub_state_label("mounted"), "mounted");
        assert_eq!(file_state_label("generated"), Some("自动生成".into()));
        assert_eq!(file_state_label(""), None);
        assert_eq!(load_state_label("loaded"), "已加载");
        assert_eq!(system_state_label("running"), "运行正常");
    }
}
