//! Reading and running a host's Docker: the commands, and the parsers for
//! what they print.

use std::collections::HashMap;

use serde_json::Value;

use super::model::{
    Container, ContainerCommand, ContainerDetails, ContainerState, DetailSection, DockerObject,
    DockerTable, Image, Network, RowLabel, SectionBody, Volume, rows,
};
use crate::i18n::t;
use crate::shared::format_bytes;

/// Where `docker` may be besides the login's `PATH` (Docker Desktop on a
/// Mac, Homebrew), and the `docker` to run as `$d`: itself for root or a
/// login allowed at its socket (the `docker` group), else through `sudo`
/// without a password, which fails at once where one is needed.
const DOCKER: &str = "export LC_ALL=C PATH=$PATH:/usr/local/bin:/opt/homebrew/bin; \
if test \"$(id -u)\" = 0 || docker version >/dev/null 2>&1; then d=docker; \
else d=\"sudo -n docker\"; fi; ";

/// What the Docker tool runs on the host for a reading: the engine's and
/// Compose's versions, then every container, volume, image and network,
/// one JSON object a line. Each part under an `@@` line of its own.
///
/// One line, in single quotes for `sh -c`, so that whatever the login shell
/// is (bash, zsh, fish, csh) it hands the script to `sh` untouched: the
/// script has no single quote, no `!` for csh to expand, and no two
/// backslashes in a row for fish to make one.
pub fn command() -> String {
    format!(
        "sh -c '{DOCKER}if command -v docker >/dev/null 2>&1; then \
         echo @@version; $d version --format \"{{{{.Server.Version}}}}\" 2>&1; \
         echo @@compose; $d compose version --short 2>/dev/null || docker-compose version --short 2>/dev/null; \
         echo @@containers; $d ps -a --no-trunc --format \"{{{{json .}}}}\" 2>/dev/null; \
         echo @@volumes; $d volume ls --format \"{{{{json .}}}}\" 2>/dev/null; \
         echo @@images; $d images --format \"{{{{json .}}}}\" 2>/dev/null; \
         echo @@networks; $d network ls --no-trunc --format \"{{{{json .}}}}\" 2>/dev/null; \
         else echo @@missing; uname -s; fi'"
    )
}

/// Whether `id` is an ID or a name Docker gave or allows, and nothing else,
/// so that it can go into a command. An image's `repository:tag` may name
/// a registry, so `/` is allowed too (`registry.example.com/app:1.0`).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 255
        && id.starts_with(|character: char| character.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_.:-/".contains(character))
}

/// `script` with `$d` for `docker`, saying how it went in an `@@status`
/// line with Docker's complaint before it.
fn with_status(script: &str) -> String {
    format!("sh -c '{DOCKER}{script} 2>&1; echo @@status $?'")
}

/// `ids` in double quotes, `None` when one is not an ID.
fn quoted(ids: &[String]) -> Option<String> {
    (!ids.is_empty() && ids.iter().all(|id| valid_id(id))).then(|| {
        ids.iter()
            .map(|id| format!("\"{id}\""))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

/// What starts, stops or restarts the containers `ids`.
pub fn control_command(ids: &[String], command: ContainerCommand) -> Option<String> {
    let ids = quoted(ids)?;
    Some(with_status(&format!("$d {} {ids}", command.verb())))
}

/// What removes a container, an image, a volume or a network.
pub fn remove_command(object: DockerObject, id: &str) -> Option<String> {
    let id = quoted(&[id.to_owned()])?;
    let verb = match object {
        DockerObject::Container => "rm",
        DockerObject::Image => "rmi",
        DockerObject::Volume => "volume rm",
        DockerObject::Network => "network rm",
    };
    Some(with_status(&format!("$d {verb} {id}")))
}

/// What reads a container, an image, a volume or a network in full, for
/// its details.
pub fn inspect_command(object: DockerObject, id: &str) -> Option<String> {
    let id = quoted(&[id.to_owned()])?;
    let verb = match object {
        DockerObject::Container => "inspect",
        DockerObject::Image => "image inspect",
        DockerObject::Volume => "volume inspect",
        DockerObject::Network => "network inspect",
    };
    Some(format!("sh -c '{DOCKER}$d {verb} {id}'"))
}

/// What reads a container's last 200 lines of output, with their times.
pub fn logs_command(id: &str) -> Option<String> {
    let id = quoted(&[id.to_owned()])?;
    Some(format!(
        "sh -c '{DOCKER}$d logs --tail 200 --timestamps {id} 2>&1'"
    ))
}

/// Docker's complaint, in ShellRS's words where it is one a user can do
/// something about.
fn complaint(said: &str) -> String {
    if said.contains("password is required")
        || (said.contains("permission denied") && said.contains("docker"))
    {
        t!("docker.error.permission").into()
    } else if said.contains("Cannot connect to the Docker daemon") {
        t!("docker.error.not_running").into()
    } else {
        let line = said.lines().next().unwrap_or(said).trim();
        line.trim_start_matches("Error response from daemon: ")
            .to_owned()
    }
}

/// How a command went, from what `with_status` printed: why not, when it
/// did not.
pub fn done(output: &str) -> Result<(), String> {
    let (said, status) = output
        .rsplit_once("@@status")
        .ok_or_else(|| t!("tools.no_answer").to_string())?;
    match status.trim() {
        "0" => Ok(()),
        status if said.trim().is_empty() => Err(t!("docker.error.status", status = status).into()),
        _ => Err(complaint(said.trim())),
    }
}

/// What the reading's command printed.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Table(DockerTable),
    /// Docker is there, but would not answer: why, in Chinese.
    Unreachable(String),
    /// No `docker` on the host. Holds `uname -s` when the host has one.
    Missing(Option<String>),
    /// Nothing we know: not a shell that runs `sh` (cmd.exe).
    Unsupported,
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
    if sections.contains_key("missing") {
        return Parsed::Missing(first("missing").map(str::to_owned));
    }
    if !sections.contains_key("version") {
        return Parsed::Unsupported;
    }
    let version = first("version").unwrap_or_default();
    if !version.starts_with(|character: char| character.is_ascii_digit()) {
        return Parsed::Unreachable(complaint(&section("version").join("\n")));
    }
    let objects = |name: &str| -> Vec<Value> {
        section(name)
            .iter()
            .filter_map(|line| serde_json::from_str(line.trim()).ok())
            .collect()
    };
    Parsed::Table(DockerTable::new(
        Some(version.to_owned()),
        first("compose").map(str::to_owned),
        objects("containers").iter().map(container).collect(),
        objects("volumes").iter().map(volume).collect(),
        objects("images").iter().map(image).collect(),
        objects("networks").iter().map(network).collect(),
    ))
}

fn text<'a>(object: &'a Value, key: &str) -> &'a str {
    object.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A comma-separated list, as `docker ps` writes mounts and networks.
fn list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `docker ps`'s labels, 「key=value,key=value」; a value may hold a comma
/// itself (Compose's list of config files), so a piece without `=` goes
/// with the value before it.
fn labels(text: &str) -> HashMap<String, String> {
    let mut labels: Vec<(String, String)> = Vec::new();
    for piece in text.split(',') {
        match piece.split_once('=') {
            Some((key, value)) => labels.push((key.to_owned(), value.to_owned())),
            None => {
                if let Some((_, value)) = labels.last_mut() {
                    value.push(',');
                    value.push_str(piece);
                }
            }
        }
    }
    labels.into_iter().collect()
}

/// 「0.0.0.0:56000->9000/tcp, :::56000->9000/tcp」 to 「56000→9000/tcp,
/// :::56000→9000/tcp」: published on every address goes without saying.
pub fn format_ports(ports: &str) -> String {
    ports
        .split(", ")
        .filter(|port| !port.is_empty())
        .map(|port| {
            port.strip_prefix("0.0.0.0:")
                .unwrap_or(port)
                .replace("->", "→")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// 「187MB」 to 「187 MB」.
fn format_size(size: &str) -> String {
    match size.find(|character: char| character.is_ascii_alphabetic()) {
        Some(unit) => format!("{} {}", &size[..unit], &size[unit..]),
        None => size.to_owned(),
    }
}

fn container(object: &Value) -> Container {
    let labels = labels(text(object, "Labels"));
    Container {
        id: text(object, "ID").to_owned(),
        name: text(object, "Names").to_owned(),
        image: text(object, "Image").to_owned(),
        state: ContainerState::parse(text(object, "State"), text(object, "Status")),
        ports: format_ports(text(object, "Ports")),
        project: labels.get("com.docker.compose.project").cloned(),
        working_dir: labels
            .get("com.docker.compose.project.working_dir")
            .cloned(),
        mounts: list(text(object, "Mounts")),
        networks: list(text(object, "Networks")),
    }
}

fn volume(object: &Value) -> Volume {
    Volume {
        name: text(object, "Name").to_owned(),
        driver: text(object, "Driver").to_owned(),
        mountpoint: text(object, "Mountpoint").to_owned(),
        used_by: Vec::new(),
    }
}

fn image(object: &Value) -> Image {
    let (repository, tag) = (text(object, "Repository"), text(object, "Tag"));
    let id = text(object, "ID").trim_start_matches("sha256:").to_owned();
    let tagged = repository != "<none>" && !matches!(tag, "<none>" | "");
    Image {
        // 「nginx」 alone would be read as nginx:latest, another image.
        key: if tagged {
            format!("{repository}:{tag}")
        } else {
            id.clone()
        },
        id,
        // 「nginx:<none>」 as `docker images` puts it: plain 「nginx」 would
        // say nginx:latest, and the containers of that would count as its.
        reference: (repository != "<none>").then(|| match tag {
            "<none>" | "" => format!("{repository}:<none>"),
            tag => format!("{repository}:{tag}"),
        }),
        size: format_size(text(object, "Size")),
        // 「2026-09-07 10:59:16 +0800 CST」: the day is enough beside the
        // ID and the size, and fits the card.
        created: text(object, "CreatedAt").chars().take(10).collect(),
        used_by: Vec::new(),
    }
}

fn network(object: &Value) -> Network {
    Network {
        id: text(object, "ID").to_owned(),
        name: text(object, "Name").to_owned(),
        driver: text(object, "Driver").to_owned(),
        scope: text(object, "Scope").to_owned(),
        used_by: Vec::new(),
    }
}

/// A container's details from what `inspect_command` printed.
pub fn parse_details(output: &str) -> Option<ContainerDetails> {
    let inspected: Value = serde_json::from_str(output.trim()).ok()?;
    let container = inspected.as_array()?.first()?;
    let path = |keys: &[&str]| {
        keys.iter()
            .try_fold(container, |value, key| value.get(*key))
            .filter(|value| !value.is_null())
    };
    let words = |keys: &[&str]| {
        path(keys)
            .and_then(Value::as_array)
            .map(|words| {
                words
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    };
    let string = |keys: &[&str]| {
        path(keys)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };

    let mut ports: Vec<(String, String)> = path(&["NetworkSettings", "Ports"])
        .and_then(Value::as_object)
        .map(|ports| {
            ports
                .iter()
                .map(|(port, bindings)| {
                    let published = bindings
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|binding| {
                            let (host, port) = (text(binding, "HostIp"), text(binding, "HostPort"));
                            match host {
                                "" | "0.0.0.0" => port.to_owned(),
                                host if host.contains(':') => format!("[{host}]:{port}"),
                                host => format!("{host}:{port}"),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    (port.clone(), published)
                })
                .collect()
        })
        .unwrap_or_default();
    ports.sort();

    let mounts = path(&["Mounts"])
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|mount| {
            let source = match text(mount, "Type") {
                "volume" => text(mount, "Name"),
                _ => text(mount, "Source"),
            };
            let read_only = mount.get("RW").and_then(Value::as_bool) == Some(false);
            let destination = text(mount, "Destination");
            (
                source.to_owned(),
                if read_only {
                    t!("docker.mount.read_only", path = destination).into()
                } else {
                    destination.to_owned()
                },
            )
        })
        .collect();

    let mut labels: Vec<(String, String)> = path(&["Config", "Labels"])
        .and_then(Value::as_object)
        .map(|labels| {
            labels
                .iter()
                .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    labels.sort();

    Some(ContainerDetails {
        name: string(&["Name"]).trim_start_matches('/').to_owned(),
        id: string(&["Id"]),
        image: string(&["Config", "Image"]),
        state: string(&["State", "Status"]),
        created: string(&["Created"]),
        entrypoint: words(&["Config", "Entrypoint"]),
        command: words(&["Config", "Cmd"]),
        ports,
        mounts,
        environment: path(&["Config", "Env"])
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        labels,
    })
}

/// The one object `docker … inspect` printed, in its JSON array.
fn inspected(output: &str) -> Option<Value> {
    let inspected: Value = serde_json::from_str(output.trim()).ok()?;
    inspected.as_array()?.first().cloned()
}

/// The value at `keys` down `value`, when it is there and not null.
fn at<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .try_fold(value, |value, key| value.get(*key))
        .filter(|value| !value.is_null())
}

fn string_at(value: &Value, keys: &[&str]) -> String {
    at(value, keys)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// An array of strings, each as it is.
fn strings_at(value: &Value, keys: &[&str]) -> Vec<String> {
    at(value, keys)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// An object of strings, by key.
fn pairs_at(value: &Value, keys: &[&str]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = at(value, keys)
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, value)| (key.clone(), value.as_str().unwrap_or_default().to_owned()))
        .collect();
    pairs.sort();
    pairs
}

fn yes_no(value: &Value, keys: &[&str]) -> String {
    match at(value, keys).and_then(Value::as_bool) {
        Some(true) => t!("docker.value.yes").into(),
        Some(false) => t!("docker.value.no").into(),
        None => String::new(),
    }
}

/// The containers using it, from the list: Docker's inspect of a volume or
/// an image does not say.
fn used_by(used_by: &[String]) -> DetailSection {
    DetailSection {
        id: "users",
        title: "docker.section.users",
        body: SectionBody::List(used_by.to_vec()),
    }
}

/// A volume's details from what `inspect_command` printed; `used_by` from
/// the list.
pub fn volume_details(output: &str, users: &[String]) -> Option<Vec<DetailSection>> {
    let volume = inspected(output)?;
    Some(vec![
        DetailSection {
            id: "basics",
            title: "docker.section.basics",
            body: rows([
                (
                    RowLabel::Field("docker.field.name"),
                    string_at(&volume, &["Name"]),
                ),
                (
                    RowLabel::Field("docker.field.driver"),
                    string_at(&volume, &["Driver"]),
                ),
                (
                    RowLabel::Field("docker.field.mountpoint"),
                    string_at(&volume, &["Mountpoint"]),
                ),
                (
                    RowLabel::Field("docker.field.scope"),
                    string_at(&volume, &["Scope"]),
                ),
                (
                    RowLabel::Field("docker.field.created"),
                    string_at(&volume, &["CreatedAt"]),
                ),
            ]),
        },
        used_by(users),
        DetailSection {
            id: "options",
            title: "docker.section.options",
            body: rows(pairs_at(&volume, &["Options"])),
        },
        DetailSection {
            id: "labels",
            title: "docker.section.labels",
            body: rows(pairs_at(&volume, &["Labels"])),
        },
    ])
}

/// An image's details from what `inspect_command` printed; `used_by` from
/// the list.
pub fn image_details(output: &str, users: &[String]) -> Option<Vec<DetailSection>> {
    let image = inspected(output)?;
    let platform = [
        string_at(&image, &["Os"]),
        string_at(&image, &["Architecture"]),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("/");
    let layers = at(&image, &["RootFS", "Layers"])
        .and_then(Value::as_array)
        .map_or(String::new(), |layers| layers.len().to_string());
    let mut ports: Vec<String> = at(&image, &["Config", "ExposedPorts"])
        .and_then(Value::as_object)
        .map(|ports| ports.keys().cloned().collect())
        .unwrap_or_default();
    ports.sort();
    Some(vec![
        DetailSection {
            id: "basics",
            title: "docker.section.basics",
            body: rows([
                (
                    RowLabel::Raw("ID".into()),
                    string_at(&image, &["Id"])
                        .trim_start_matches("sha256:")
                        .to_owned(),
                ),
                (
                    RowLabel::Field("docker.field.tags"),
                    strings_at(&image, &["RepoTags"]).join("\n"),
                ),
                (
                    RowLabel::Field("docker.field.digests"),
                    strings_at(&image, &["RepoDigests"]).join("\n"),
                ),
                (
                    RowLabel::Field("docker.field.created"),
                    string_at(&image, &["Created"]),
                ),
                (
                    RowLabel::Field("docker.field.size"),
                    at(&image, &["Size"])
                        .and_then(Value::as_u64)
                        .map_or(String::new(), format_bytes),
                ),
                (RowLabel::Field("docker.field.platform"), platform),
                (RowLabel::Field("docker.field.layers"), layers),
                (
                    RowLabel::Field("docker.field.author"),
                    string_at(&image, &["Author"]),
                ),
            ]),
        },
        used_by(users),
        DetailSection {
            id: "config",
            title: "docker.section.config",
            body: rows([
                (
                    RowLabel::Field("docker.field.entrypoint"),
                    strings_at(&image, &["Config", "Entrypoint"]).join(" "),
                ),
                (
                    RowLabel::Field("docker.field.command"),
                    strings_at(&image, &["Config", "Cmd"]).join(" "),
                ),
                (
                    RowLabel::Field("docker.field.working_dir"),
                    string_at(&image, &["Config", "WorkingDir"]),
                ),
                (
                    RowLabel::Field("docker.field.user"),
                    string_at(&image, &["Config", "User"]),
                ),
                (
                    RowLabel::Field("docker.field.exposed_ports"),
                    ports.join(", "),
                ),
            ]),
        },
        DetailSection {
            id: "environment",
            title: "docker.section.environment",
            body: SectionBody::Text(strings_at(&image, &["Config", "Env"])),
        },
        DetailSection {
            id: "labels",
            title: "docker.section.labels",
            body: rows(pairs_at(&image, &["Config", "Labels"])),
        },
    ])
}

/// A network's details from what `inspect_command` printed. Its
/// containers are those running on it, with their addresses.
pub fn network_details(output: &str) -> Option<Vec<DetailSection>> {
    let network = inspected(output)?;
    let subnets: Vec<(String, String)> = at(&network, &["IPAM", "Config"])
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|config| {
            (
                text(config, "Subnet").to_owned(),
                match text(config, "Gateway") {
                    "" => String::new(),
                    gateway => t!("docker.network.gateway", address = gateway).into(),
                },
            )
        })
        .collect();
    let mut containers: Vec<(String, String)> = at(&network, &["Containers"])
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(_, container)| {
            (
                text(container, "Name").to_owned(),
                [
                    text(container, "IPv4Address"),
                    text(container, "IPv6Address"),
                ]
                .into_iter()
                .filter(|address| !address.is_empty())
                .collect::<Vec<_>>()
                .join(", "),
            )
        })
        .collect();
    containers.sort();
    Some(vec![
        DetailSection {
            id: "basics",
            title: "docker.section.basics",
            body: rows([
                (
                    RowLabel::Field("docker.field.name"),
                    string_at(&network, &["Name"]),
                ),
                (RowLabel::Raw("ID".into()), string_at(&network, &["Id"])),
                (
                    RowLabel::Field("docker.field.driver"),
                    string_at(&network, &["Driver"]),
                ),
                (
                    RowLabel::Field("docker.field.scope"),
                    string_at(&network, &["Scope"]),
                ),
                (
                    RowLabel::Field("docker.field.created"),
                    string_at(&network, &["Created"]),
                ),
                (
                    RowLabel::Raw("IPv6".into()),
                    yes_no(&network, &["EnableIPv6"]),
                ),
                (
                    RowLabel::Field("docker.field.internal"),
                    yes_no(&network, &["Internal"]),
                ),
            ]),
        },
        DetailSection {
            id: "subnets",
            title: "docker.section.subnets",
            body: rows(subnets),
        },
        DetailSection {
            id: "containers",
            title: "docker.section.containers",
            body: rows(containers),
        },
        DetailSection {
            id: "options",
            title: "docker.section.options",
            body: rows(pairs_at(&network, &["Options"])),
        },
        DetailSection {
            id: "labels",
            title: "docker.section.labels",
            body: rows(pairs_at(&network, &["Labels"])),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const READING: &str = r#"@@version
26.0.0
@@compose
v2.25.0
@@containers
{"Command":"\"docker-php-entrypoint php-fpm\"","ID":"aaa111","Image":"php:5.6-fpm","Labels":"com.docker.compose.project=php-56,com.docker.compose.project.config_files=/srv/php-5.6/a.yml,/srv/php-5.6/b.yml,com.docker.compose.project.working_dir=/srv/php-5.6,com.docker.compose.service=php56","Mounts":"/srv/php-5.6/www","Names":"php56","Networks":"php-56_default","Ports":"0.0.0.0:56000->9000/tcp, :::56000->9000/tcp","State":"running","Status":"Up 3 days"}
{"ID":"bbb222","Image":"registry.example.com/forex-web:latest","Labels":"","Mounts":"","Names":"forex-web","Networks":"bridge","Ports":"127.0.0.1:4321->4321/tcp","State":"running","Status":"Up 2 hours"}
{"ID":"ccc333","Image":"hello-world","Labels":"","Mounts":"","Names":"exciting_tesla","Networks":"bridge","Ports":"","Status":"Exited (0) 3 weeks ago"}
@@volumes
{"Driver":"local","Labels":"","Mountpoint":"/var/lib/docker/volumes/vw-data/_data","Name":"vw-data","Scope":"local"}
@@images
{"CreatedAt":"2026-09-07 10:59:16 +0800 CST","CreatedSince":"3 weeks ago","ID":"4a3b5c6d7e8f","Repository":"hello-world","Size":"13.3kB","Tag":"latest"}
{"CreatedAt":"2026-08-01 08:00:00 +0800 CST","ID":"0f0f0f0f0f0f","Repository":"<none>","Size":"187MB","Tag":"<none>"}
@@networks
{"Driver":"bridge","ID":"n1","Name":"bridge","Scope":"local"}
{"Driver":"bridge","ID":"n2","Name":"php-56_default","Scope":"local"}
"#;

    fn table(output: &str) -> DockerTable {
        match parse(output) {
            Parsed::Table(table) => table,
            other => panic!("not a table: {other:?}"),
        }
    }

    #[test]
    fn a_reading_gives_containers_by_project_and_the_rest() {
        let table = table(READING);
        assert_eq!(table.summary(), "Docker 26.0.0 · Compose v2.25.0");
        assert_eq!(table.counts(), [3, 1, 2, 2]);
        let php = table.container("aaa111").expect("php56");
        assert_eq!(php.project.as_deref(), Some("php-56"));
        // The comma in the list of config files does not cut the labels
        // after it short.
        assert_eq!(php.working_dir.as_deref(), Some("/srv/php-5.6"));
        assert_eq!(php.ports, "56000→9000/tcp, :::56000→9000/tcp");
        let web = table.container("bbb222").expect("forex-web");
        assert_eq!(web.ports, "127.0.0.1:4321→4321/tcp");
        // An old Docker without State: from the status.
        assert_eq!(
            table.container("ccc333").expect("tesla").state,
            ContainerState::Exited
        );
        let images: Vec<(Option<&str>, &str, &str, bool)> = table
            .images()
            .iter()
            .map(|image| {
                (
                    image.reference.as_deref(),
                    image.size.as_str(),
                    image.created.as_str(),
                    !image.used_by.is_empty(),
                )
            })
            .collect();
        assert_eq!(
            images,
            [
                (Some("hello-world:latest"), "13.3 kB", "2026-09-07", true),
                (None, "187 MB", "2026-08-01", false),
            ]
        );
        assert_eq!(table.networks()[1].used_by, ["php56"]);
        assert!(table.volumes()[0].used_by.is_empty());
    }

    #[test]
    fn an_image_with_several_tags_is_a_line_for_each_named_by_its_tag() {
        let table = table(
            "@@version\n26.0.0\n@@containers\n@@volumes\n@@images\n\
             {\"ID\":\"aaa111\",\"Repository\":\"nginx\",\"Tag\":\"latest\",\"Size\":\"187MB\"}\n\
             {\"ID\":\"aaa111\",\"Repository\":\"nginx\",\"Tag\":\"1.27\",\"Size\":\"187MB\"}\n\
             {\"ID\":\"aaa111\",\"Repository\":\"registry.example.com/web/nginx\",\"Tag\":\"prod\",\"Size\":\"187MB\"}\n\
             {\"ID\":\"bbb222\",\"Repository\":\"nginx\",\"Tag\":\"<none>\",\"Size\":\"180MB\"}\n\
             {\"ID\":\"bbb222\",\"Repository\":\"mirror/nginx\",\"Tag\":\"<none>\",\"Size\":\"180MB\"}\n\
             @@networks\n",
        );
        let keys: Vec<&str> = table
            .images()
            .iter()
            .map(|image| image.key.as_str())
            .collect();
        // 「nginx」 with no tag is not nginx:latest: it goes by its ID, and
        // once, though two repositories hold it.
        assert_eq!(
            keys,
            [
                "nginx:1.27",
                "nginx:latest",
                "registry.example.com/web/nginx:prod",
                "bbb222"
            ]
        );
        assert_eq!(
            table.images()[3].reference.as_deref(),
            Some("mirror/nginx:<none>")
        );
        let summary = table
            .summary_of(DockerObject::Image, "nginx:1.27")
            .expect("by its tag");
        assert_eq!(summary.name, "nginx:1.27");
        assert_eq!(summary.id, "nginx:1.27");
        // Removing a line removes that tag, wherever the registry is.
        assert_eq!(
            remove_command(DockerObject::Image, "registry.example.com/web/nginx:prod").as_deref(),
            Some(
                "sh -c 'export LC_ALL=C PATH=$PATH:/usr/local/bin:/opt/homebrew/bin; \
                 if test \"$(id -u)\" = 0 || docker version >/dev/null 2>&1; then d=docker; \
                 else d=\"sudo -n docker\"; fi; $d rmi \"registry.example.com/web/nginx:prod\" 2>&1; \
                 echo @@status $?'"
            )
        );
    }

    #[test]
    fn a_docker_that_will_not_answer_says_why() {
        assert_eq!(
            parse(
                "@@version\nCannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?\n@@compose\n@@containers\n"
            ),
            Parsed::Unreachable("Docker 没有运行".into())
        );
        assert_eq!(
            parse("@@version\nsudo: a password is required\n"),
            Parsed::Unreachable(
                "没有权限：以 root 登录、把这个用户加入 docker 组，或给它配置免密码的 sudo".into()
            )
        );
        assert_eq!(
            parse("@@missing\nLinux\n"),
            Parsed::Missing(Some("Linux".into()))
        );
        assert_eq!(parse(""), Parsed::Unsupported);
    }

    #[test]
    fn details_come_from_docker_inspect() {
        let details = parse_details(
            r#"[{"Id":"f8664a4a9b8d","Name":"/forex-web","Created":"2026-09-07T10:59:16.371325789Z",
            "State":{"Status":"running"},
            "Config":{"Image":"registry.example.com/forex-web:latest","Entrypoint":["docker-entrypoint.sh"],
              "Cmd":["node","./dist/server/entry.mjs"],"Env":["TZ=Asia/Shanghai","PORT=4321"],"Labels":{}},
            "NetworkSettings":{"Ports":{"4321/tcp":[{"HostIp":"127.0.0.1","HostPort":"4321"}],"9229/tcp":null}},
            "Mounts":[{"Type":"volume","Name":"web-data","Source":"/var/lib/docker/volumes/web-data/_data","Destination":"/data","RW":true},
              {"Type":"bind","Source":"/etc/localtime","Destination":"/etc/localtime","RW":false}]}]"#,
        )
        .expect("details");
        assert_eq!(details.name, "forex-web");
        assert_eq!(details.entrypoint, "docker-entrypoint.sh");
        assert_eq!(details.command, "node ./dist/server/entry.mjs");
        assert_eq!(
            details.ports,
            [
                ("4321/tcp".to_owned(), "127.0.0.1:4321".to_owned()),
                ("9229/tcp".to_owned(), String::new()),
            ]
        );
        assert_eq!(
            details.mounts,
            [
                ("web-data".to_owned(), "/data".to_owned()),
                (
                    "/etc/localtime".to_owned(),
                    "/etc/localtime（只读）".to_owned()
                ),
            ]
        );
        assert_eq!(details.environment, ["TZ=Asia/Shanghai", "PORT=4321"]);
        assert!(details.labels.is_empty());
        assert_eq!(parse_details("Error: No such object"), None);
    }

    /// The rows of a section, by its id.
    fn section(sections: &[DetailSection], id: &str) -> SectionBody {
        sections
            .iter()
            .find(|section| section.id == id)
            .unwrap_or_else(|| panic!("no {id}"))
            .body
            .clone()
    }

    fn row(sections: &[DetailSection], id: &str, label: &str) -> String {
        let SectionBody::Rows(rows) = section(sections, id) else {
            panic!("{id} has no rows");
        };
        rows.into_iter()
            .find(|(key, _)| key.id() == label)
            .unwrap_or_else(|| panic!("no {label} in {id}"))
            .1
    }

    #[test]
    fn volumes_images_and_networks_have_details_too() {
        let volume = volume_details(
            r#"[{"CreatedAt":"2026-09-01T08:00:00+08:00","Driver":"local","Labels":{"com.docker.compose.project":"vaultwarden"},
            "Mountpoint":"/var/lib/docker/volumes/vw-data/_data","Name":"vw-data","Options":null,"Scope":"local"}]"#,
            &["vaultwarden".into()],
        )
        .expect("volume");
        assert_eq!(
            row(&volume, "basics", "mountpoint"),
            "/var/lib/docker/volumes/vw-data/_data"
        );
        assert_eq!(
            section(&volume, "users"),
            SectionBody::List(vec!["vaultwarden".into()])
        );
        assert_eq!(section(&volume, "options"), SectionBody::Rows(Vec::new()));
        assert_eq!(
            row(&volume, "labels", "com.docker.compose.project"),
            "vaultwarden"
        );

        let image = image_details(
            r#"[{"Id":"sha256:4a3b5c6d7e8f9a","RepoTags":["hello-world:latest"],"RepoDigests":[],
            "Created":"2025-08-09T10:00:00Z","Size":10072,"Os":"linux","Architecture":"amd64","Author":"",
            "Config":{"Cmd":["/hello"],"Entrypoint":null,"Env":["PATH=/usr/bin"],"WorkingDir":"","ExposedPorts":{"80/tcp":{}}},
            "RootFS":{"Layers":["sha256:a","sha256:b"]}}]"#,
            &[],
        )
        .expect("image");
        assert_eq!(row(&image, "basics", "ID"), "4a3b5c6d7e8f9a");
        assert_eq!(row(&image, "basics", "size"), "9.84 KB");
        assert_eq!(row(&image, "basics", "platform"), "linux/amd64");
        assert_eq!(row(&image, "basics", "layers"), "2");
        // Nothing to say is said with a dash.
        assert_eq!(row(&image, "basics", "author"), "—");
        assert_eq!(row(&image, "config", "command"), "/hello");
        assert_eq!(row(&image, "config", "exposed_ports"), "80/tcp");
        assert_eq!(
            section(&image, "environment"),
            SectionBody::Text(vec!["PATH=/usr/bin".into()])
        );
        assert!(section(&image, "users").is_empty());

        let network = network_details(
            r#"[{"Name":"php-56_default","Id":"n2","Created":"2026-09-01T08:00:00Z","Scope":"local","Driver":"bridge",
            "EnableIPv6":false,"Internal":false,"IPAM":{"Config":[{"Subnet":"172.18.0.0/16","Gateway":"172.18.0.1"}]},
            "Containers":{"aaa111":{"Name":"php56","IPv4Address":"172.18.0.2/16","IPv6Address":""}},"Options":{},"Labels":{}}]"#,
        )
        .expect("network");
        assert_eq!(row(&network, "basics", "IPv6"), "否");
        assert_eq!(row(&network, "subnets", "172.18.0.0/16"), "网关 172.18.0.1");
        assert_eq!(row(&network, "containers", "php56"), "172.18.0.2/16");
        assert_eq!(volume_details("Error: no such volume", &[]), None);
    }

    #[test]
    fn every_heading_and_label_has_its_text() {
        let mut sections = ContainerDetails::default().sections();
        sections.extend(volume_details("[{}]", &[]).expect("volume"));
        sections.extend(image_details("[{}]", &[]).expect("image"));
        sections.extend(network_details("[{}]").expect("network"));
        for section in sections {
            assert!(crate::i18n::has(section.title), "{}", section.title);
            if let SectionBody::Rows(rows) = section.body {
                for (label, _) in rows {
                    if let RowLabel::Field(key) = label {
                        assert!(crate::i18n::has(key), "{key}");
                    }
                }
            }
        }
    }

    #[test]
    fn only_an_id_goes_into_a_command() {
        assert!(valid_id("f8664a4a9b8d"));
        assert!(valid_id("vw-data"));
        assert!(valid_id("sha256:4a3b5c"));
        assert!(valid_id("registry.example.com:5000/team/app:1.0"));
        assert!(!valid_id("-rf"));
        assert!(!valid_id("/etc"));
        assert!(!valid_id("a\"b"));
        assert!(!valid_id("a b"));
        assert!(!valid_id("$(reboot)"));
        assert_eq!(remove_command(DockerObject::Volume, "a;b"), None);
        assert_eq!(
            control_command(&["aaa".into(), "bbb".into()], ContainerCommand::Stop).as_deref(),
            Some(
                "sh -c 'export LC_ALL=C PATH=$PATH:/usr/local/bin:/opt/homebrew/bin; \
                 if test \"$(id -u)\" = 0 || docker version >/dev/null 2>&1; then d=docker; \
                 else d=\"sudo -n docker\"; fi; $d stop \"aaa\" \"bbb\" 2>&1; echo @@status $?'"
            )
        );
        assert_eq!(
            done(
                "Error response from daemon: conflict: unable to remove volume: volume is in use\n@@status 1\n"
            ),
            Err("conflict: unable to remove volume: volume is in use".into())
        );
        assert_eq!(done("aaa\n@@status 0\n"), Ok(()));
    }

    fn scripts() -> Vec<String> {
        vec![
            command(),
            control_command(&["aaa".into()], ContainerCommand::Restart).unwrap(),
            remove_command(DockerObject::Image, "4a3b5c6d7e8f").unwrap(),
            inspect_command(DockerObject::Container, "aaa").unwrap(),
            inspect_command(DockerObject::Network, "n1").unwrap(),
            logs_command("aaa").unwrap(),
        ]
    }

    #[test]
    fn the_commands_survive_any_login_shell() {
        for command in scripts() {
            let script = command
                .strip_prefix("sh -c '")
                .and_then(|rest| rest.strip_suffix('\''))
                .expect("one sh -c in single quotes");
            assert!(!script.contains('\''), "{script}");
            assert!(!script.contains('!'), "{script}");
            assert!(!script.contains('\n'), "{script}");
            assert!(!script.contains("\\\\"), "{script}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_commands_are_valid_sh() {
        for command in scripts() {
            let script = command
                .strip_prefix("sh -c '")
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap();
            assert!(crate::testing::sh_accepts(script), "{script}");
        }
    }

    /// The reading's script, run by this machine's `sh` with a `docker`
    /// stood in for, hands the templates to `docker` intact.
    #[cfg(unix)]
    #[test]
    fn the_script_asks_docker_for_json() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temp dir");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let docker = bin.join("docker");
        std::fs::write(
            &docker,
            "#!/bin/sh\n\
             case \"$1 $2\" in\n\
             \"version \") exit 0;;\n\
             \"version --format\") echo 26.0.0;;\n\
             \"compose version\") echo v2.25.0;;\n\
             \"ps -a\") test \"$5\" = \"{{json .}}\" && echo '{\"ID\":\"aaa\",\"Names\":\"web\",\"State\":\"running\"}';;\n\
             \"volume ls\") echo '{\"Name\":\"data\",\"Driver\":\"local\"}';;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = command();
        let script = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = crate::testing::sh(script, Some(&path));
        let table = table(&String::from_utf8(output.stdout).unwrap());
        assert_eq!(table.summary(), "Docker 26.0.0 · Compose v2.25.0");
        assert_eq!(table.containers()[0].name, "web");
        assert_eq!(table.volumes()[0].name, "data");
    }
}
