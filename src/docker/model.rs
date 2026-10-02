//! What the Docker tool reads off a host: containers grouped by their
//! compose project, volumes, images and networks.

use std::collections::BTreeMap;

/// What a container is doing, as `docker ps` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerState {
    Running,
    Paused,
    Restarting,
    Created,
    Exited,
    Dead,
    Removing,
}

impl ContainerState {
    /// `docker ps`'s `State`, or, from a Docker before 20.10 without one,
    /// its `Status`: 「Up 3 days」「Exited (0) 2 hours ago」.
    pub fn parse(state: &str, status: &str) -> Self {
        match state {
            "running" => ContainerState::Running,
            "paused" => ContainerState::Paused,
            "restarting" => ContainerState::Restarting,
            "created" => ContainerState::Created,
            "exited" => ContainerState::Exited,
            "dead" => ContainerState::Dead,
            "removing" => ContainerState::Removing,
            _ if status.starts_with("Up") && status.contains("(Paused)") => ContainerState::Paused,
            _ if status.starts_with("Up") => ContainerState::Running,
            _ if status.starts_with("Restarting") => ContainerState::Restarting,
            _ if status.starts_with("Created") => ContainerState::Created,
            _ if status.starts_with("Dead") => ContainerState::Dead,
            _ if status.starts_with("Removal") => ContainerState::Removing,
            _ => ContainerState::Exited,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContainerState::Running => "运行中",
            ContainerState::Paused => "已暂停",
            ContainerState::Restarting => "正在重启",
            ContainerState::Created => "已创建",
            ContainerState::Exited => "已停止",
            ContainerState::Dead => "异常",
            ContainerState::Removing => "正在删除",
        }
    }

    /// Whether it is up: what stops and restarts, rather than starts.
    pub fn is_up(self) -> bool {
        matches!(
            self,
            ContainerState::Running | ContainerState::Paused | ContainerState::Restarting
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    /// The full ID.
    pub id: String,
    pub name: String,
    /// As it was run: 「nginx:1.27」, 「hello-world」.
    pub image: String,
    pub state: ContainerState,
    /// 「56000→9000/tcp, :::56000→9000/tcp」, empty with none published.
    pub ports: String,
    /// The compose project it belongs to, and that project's directory.
    pub project: Option<String>,
    pub working_dir: Option<String>,
    /// The volumes (by name) and host paths mounted into it.
    pub mounts: Vec<String>,
    pub networks: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    /// The containers it is mounted into, running or not, by name: Docker
    /// will not remove it while there are any.
    pub used_by: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// 「4a3b5c6d7e8f」.
    pub id: String,
    /// 「nginx:1.27」, `None` for a dangling image (「<none>」).
    pub reference: Option<String>,
    /// 「187.69 MB」.
    pub size: String,
    /// 「2026-09-07」.
    pub created: String,
    /// The containers made from it, by name.
    pub used_by: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    /// The containers attached to it, by name.
    pub used_by: Vec<String>,
}

impl Network {
    /// One of the three Docker makes itself, which cannot be removed.
    pub fn builtin(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }
}

/// A compose project: the containers sharing its label.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub name: String,
    pub working_dir: Option<String>,
    /// By their place in `DockerTable::containers`, by name.
    pub containers: Vec<usize>,
    pub running: usize,
}

/// What can be done to containers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ContainerCommand {
    Start,
    Stop,
    Restart,
}

impl ContainerCommand {
    pub fn verb(self) -> &'static str {
        match self {
            ContainerCommand::Start => "start",
            ContainerCommand::Stop => "stop",
            ContainerCommand::Restart => "restart",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ContainerCommand::Start => "启动",
            ContainerCommand::Stop => "停止",
            ContainerCommand::Restart => "重启",
        }
    }

    /// What it did, after the name: 「已停止」.
    pub fn done(self) -> &'static str {
        match self {
            ContainerCommand::Start => "已启动",
            ContainerCommand::Stop => "已停止",
            ContainerCommand::Restart => "已重启",
        }
    }
}

/// What kind of thing a removal removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DockerObject {
    Container,
    Image,
    Volume,
    Network,
}

impl DockerObject {
    pub fn label(self) -> &'static str {
        match self {
            DockerObject::Container => "容器",
            DockerObject::Image => "镜像",
            DockerObject::Volume => "卷",
            DockerObject::Network => "网络",
        }
    }
}

/// The tabs over the list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DockerTab {
    #[default]
    Containers,
    Volumes,
    Images,
    Networks,
}

impl DockerTab {
    pub const ALL: [DockerTab; 4] = [
        DockerTab::Containers,
        DockerTab::Volumes,
        DockerTab::Images,
        DockerTab::Networks,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DockerTab::Containers => "容器",
            DockerTab::Volumes => "卷",
            DockerTab::Images => "镜像",
            DockerTab::Networks => "网络",
        }
    }
}

/// A line of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerRow {
    /// A compose project by its place in `projects()`, with its containers
    /// under it when unfolded.
    Project {
        index: usize,
        unfolded: bool,
    },
    /// The heading over the containers of no project.
    Standalone,
    Container(usize),
    Volume(usize),
    Image(usize),
    Network(usize),
}

/// One reading of a host's Docker.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DockerTable {
    /// 「26.0.0」.
    pub version: Option<String>,
    /// 「v2.25.0」.
    pub compose: Option<String>,
    containers: Vec<Container>,
    projects: Vec<Project>,
    volumes: Vec<Volume>,
    images: Vec<Image>,
    networks: Vec<Network>,
}

impl DockerTable {
    /// Sorts each kind by name, groups the containers by project, and
    /// marks what the containers use.
    pub fn new(
        version: Option<String>,
        compose: Option<String>,
        mut containers: Vec<Container>,
        mut volumes: Vec<Volume>,
        mut images: Vec<Image>,
        mut networks: Vec<Network>,
    ) -> Self {
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        let mut grouped: BTreeMap<String, Project> = BTreeMap::new();
        for (index, container) in containers.iter().enumerate() {
            let Some(name) = &container.project else {
                continue;
            };
            let project = grouped.entry(name.clone()).or_insert_with(|| Project {
                name: name.clone(),
                working_dir: None,
                containers: Vec::new(),
                running: 0,
            });
            if project.working_dir.is_none() {
                project.working_dir = container.working_dir.clone();
            }
            project.containers.push(index);
            if container.state.is_up() {
                project.running += 1;
            }
        }

        let using = |uses: &dyn Fn(&Container) -> bool| -> Vec<String> {
            containers
                .iter()
                .filter(|container| uses(container))
                .map(|container| container.name.clone())
                .collect()
        };
        for volume in &mut volumes {
            volume.used_by = using(&|container| container.mounts.contains(&volume.name));
        }
        volumes.sort_by(|a, b| a.name.cmp(&b.name));

        for image in &mut images {
            image.used_by = using(&|container| {
                let used = container.image.as_str();
                // A container whose image lost its name since shows its ID.
                let id = used.trim_start_matches("sha256:");
                let by_id = id.len() >= 12 && id.chars().all(|c| c.is_ascii_hexdigit());
                image.reference.as_deref().is_some_and(|reference| {
                    used == reference || reference.strip_suffix(":latest") == Some(used)
                }) || (by_id && (id.starts_with(&image.id) || image.id.starts_with(id)))
            });
        }
        // Named ones first, by name; the dangling after.
        images.sort_by(|a, b| {
            (a.reference.is_none(), &a.reference, &a.id).cmp(&(
                b.reference.is_none(),
                &b.reference,
                &b.id,
            ))
        });

        for network in &mut networks {
            network.used_by = using(&|container| container.networks.contains(&network.name));
        }
        networks.sort_by(|a, b| a.name.cmp(&b.name));

        Self {
            version,
            compose,
            containers,
            projects: grouped.into_values().collect(),
            volumes,
            images,
            networks,
        }
    }

    pub fn containers(&self) -> &[Container] {
        &self.containers
    }

    pub fn container(&self, id: &str) -> Option<&Container> {
        self.containers.iter().find(|container| container.id == id)
    }

    /// A volume, an image or a network by what Docker knows it by, as its
    /// card and its details show it.
    pub fn summary_of(&self, object: DockerObject, id: &str) -> Option<ObjectSummary> {
        match object {
            DockerObject::Container => None,
            DockerObject::Volume => self
                .volumes
                .iter()
                .find(|volume| volume.name == id)
                .map(Volume::summary),
            DockerObject::Image => self
                .images
                .iter()
                .find(|image| image.id == id)
                .map(Image::summary),
            DockerObject::Network => self
                .networks
                .iter()
                .find(|network| network.id == id)
                .map(Network::summary),
        }
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn volumes(&self) -> &[Volume] {
        &self.volumes
    }

    pub fn images(&self) -> &[Image] {
        &self.images
    }

    pub fn networks(&self) -> &[Network] {
        &self.networks
    }

    /// 「Docker 26.0.0 · Compose v2.25.0」.
    pub fn summary(&self) -> String {
        let mut parts = vec![
            format!("Docker {}", self.version.as_deref().unwrap_or(""))
                .trim()
                .to_owned(),
        ];
        if let Some(compose) = &self.compose {
            parts.push(format!("Compose {compose}"));
        }
        parts.join(" · ")
    }

    /// How many of each the tabs count, in their order.
    pub fn counts(&self) -> [usize; 4] {
        [
            self.containers.len(),
            self.volumes.len(),
            self.images.len(),
            self.networks.len(),
        ]
    }

    /// The list under `tab`. Containers: each compose project, its
    /// containers under it when `unfolded` says so, then the containers
    /// of no project under their heading.
    pub fn rows(&self, tab: DockerTab, unfolded: impl Fn(&Project) -> bool) -> Vec<DockerRow> {
        match tab {
            DockerTab::Containers => {
                let mut rows: Vec<DockerRow> = self
                    .projects
                    .iter()
                    .enumerate()
                    .map(|(index, project)| DockerRow::Project {
                        index,
                        unfolded: unfolded(project),
                    })
                    .collect();
                let standalone: Vec<DockerRow> = self
                    .containers
                    .iter()
                    .enumerate()
                    .filter(|(_, container)| container.project.is_none())
                    .map(|(index, _)| DockerRow::Container(index))
                    .collect();
                if !standalone.is_empty() {
                    rows.push(DockerRow::Standalone);
                    rows.extend(standalone);
                }
                rows
            }
            DockerTab::Volumes => (0..self.volumes.len()).map(DockerRow::Volume).collect(),
            DockerTab::Images => (0..self.images.len()).map(DockerRow::Image).collect(),
            DockerTab::Networks => (0..self.networks.len()).map(DockerRow::Network).collect(),
        }
    }
}

/// How a tag reads: what is in use stands out, the rest is quiet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Quiet,
}

/// A volume, an image or a network as its card and its details show it.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectSummary {
    pub object: DockerObject,
    /// What Docker knows it by, for its commands: the ID, a volume's name.
    pub id: String,
    pub name: String,
    /// A line about it: 「local · /var/lib/docker/volumes/data/_data」.
    pub detail: String,
    pub tag: Option<(String, Tone)>,
    /// Whether it can be removed, and why not.
    pub removable: Result<(), &'static str>,
    /// The containers using it, by name.
    pub used_by: Vec<String>,
}

fn joined(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

impl Volume {
    pub fn summary(&self) -> ObjectSummary {
        let in_use = !self.used_by.is_empty();
        ObjectSummary {
            object: DockerObject::Volume,
            id: self.name.clone(),
            name: self.name.clone(),
            detail: joined(&[&self.driver, &self.mountpoint]),
            tag: Some(if in_use {
                ("使用中".into(), Tone::Good)
            } else {
                ("未使用".into(), Tone::Quiet)
            }),
            removable: if in_use {
                Err("有容器在用，不能删除")
            } else {
                Ok(())
            },
            used_by: self.used_by.clone(),
        }
    }
}

impl Image {
    pub fn summary(&self) -> ObjectSummary {
        let in_use = !self.used_by.is_empty();
        ObjectSummary {
            object: DockerObject::Image,
            id: self.id.clone(),
            name: self
                .reference
                .clone()
                .unwrap_or_else(|| "未命名镜像".into()),
            detail: joined(&[&self.id, &self.size, &self.created]),
            tag: in_use.then(|| ("使用中".into(), Tone::Good)),
            removable: if in_use {
                Err("有容器在用，不能删除")
            } else {
                Ok(())
            },
            used_by: self.used_by.clone(),
        }
    }
}

impl Network {
    pub fn summary(&self) -> ObjectSummary {
        let (tag, removable) = if self.builtin() {
            (
                ("内置".to_owned(), Tone::Quiet),
                Err("Docker 自带的网络，不能删除"),
            )
        } else if !self.used_by.is_empty() {
            (
                (format!("{} 个容器", self.used_by.len()), Tone::Good),
                Err("有容器连着，不能删除"),
            )
        } else {
            (("未使用".to_owned(), Tone::Quiet), Ok(()))
        };
        ObjectSummary {
            object: DockerObject::Network,
            id: self.id.clone(),
            name: self.name.clone(),
            detail: joined(&[&self.driver, &self.scope]),
            tag: Some(tag),
            removable,
            used_by: self.used_by.clone(),
        }
    }
}

/// A part of something's details, under its heading.
#[derive(Clone, Debug, PartialEq)]
pub struct DetailSection {
    /// For the test ids of its lines: 「basics」.
    pub id: &'static str,
    pub title: &'static str,
    pub body: SectionBody,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SectionBody {
    /// Labels and their values: 「挂载点」 「/var/lib/docker/volumes/…」.
    Rows(Vec<(String, String)>),
    /// Names, one a line: the containers using it.
    List(Vec<String>),
    /// Lines of text as they are, in a fixed-width face: the environment.
    Text(Vec<String>),
}

impl SectionBody {
    pub fn is_empty(&self) -> bool {
        match self {
            SectionBody::Rows(rows) => rows.is_empty(),
            SectionBody::List(lines) | SectionBody::Text(lines) => lines.is_empty(),
        }
    }
}

/// Labels and values, with 「—」 for the empty values.
pub fn rows(rows: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>) -> SectionBody {
    SectionBody::Rows(
        rows.into_iter()
            .map(|(label, value)| {
                let value = value.into();
                (
                    label.into(),
                    if value.is_empty() {
                        "—".into()
                    } else {
                        value
                    },
                )
            })
            .collect(),
    )
}

impl ContainerDetails {
    /// Its sections, in the order its dialog shows them.
    pub fn sections(&self) -> Vec<DetailSection> {
        vec![
            DetailSection {
                id: "basics",
                title: "基础信息",
                body: rows([
                    ("名称", self.name.clone()),
                    ("ID", self.id.clone()),
                    ("镜像", self.image.clone()),
                    ("创建时间", self.created.clone()),
                    ("入口", self.entrypoint.clone()),
                    ("命令", self.command.clone()),
                ]),
            },
            DetailSection {
                id: "ports",
                title: "端口映射",
                body: rows(self.ports.clone()),
            },
            DetailSection {
                id: "mounts",
                title: "挂载",
                body: rows(self.mounts.clone()),
            },
            DetailSection {
                id: "environment",
                title: "环境变量",
                body: SectionBody::Text(self.environment.clone()),
            },
            DetailSection {
                id: "labels",
                title: "标签",
                body: rows(self.labels.clone()),
            },
        ]
    }
}

/// A container's details, as `docker inspect` says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContainerDetails {
    pub name: String,
    pub id: String,
    pub image: String,
    pub state: String,
    /// 「2026-09-07T10:59:16.371325789Z」.
    pub created: String,
    pub entrypoint: String,
    pub command: String,
    /// The container's port and where on the host it is published:
    /// 「4321/tcp」, 「127.0.0.1:4321」 (empty when it is not).
    pub ports: Vec<(String, String)>,
    /// Where from and where to: 「/srv/data」, 「/data」, with 「rw」 or 「ro」.
    pub mounts: Vec<(String, String)>,
    pub environment: Vec<String>,
    pub labels: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(name: &str, state: ContainerState, project: Option<&str>) -> Container {
        Container {
            id: format!("{name}-id"),
            name: name.into(),
            image: format!("{name}:latest"),
            state,
            ports: String::new(),
            project: project.map(str::to_owned),
            working_dir: project.map(|project| format!("/srv/{project}")),
            mounts: Vec::new(),
            networks: vec!["bridge".into()],
        }
    }

    #[test]
    fn containers_group_under_their_project_and_the_rest_stand_alone() {
        let table = DockerTable::new(
            Some("26.0.0".into()),
            Some("v2.25.0".into()),
            vec![
                container("forex-web", ContainerState::Running, None),
                container("vaultwarden", ContainerState::Exited, Some("vaultwarden")),
                container("php56", ContainerState::Running, Some("php-56")),
                container("exciting_tesla", ContainerState::Exited, None),
            ],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(table.summary(), "Docker 26.0.0 · Compose v2.25.0");
        let projects: Vec<(&str, usize, usize)> = table
            .projects()
            .iter()
            .map(|project| {
                (
                    project.name.as_str(),
                    project.running,
                    project.containers.len(),
                )
            })
            .collect();
        assert_eq!(projects, [("php-56", 1, 1), ("vaultwarden", 0, 1)]);
        let rows = table.rows(DockerTab::Containers, |project| project.running > 0);
        let names: Vec<String> = rows
            .iter()
            .map(|row| match row {
                DockerRow::Project { index, unfolded } => {
                    format!("{} {unfolded}", table.projects()[*index].name)
                }
                DockerRow::Standalone => "独立容器".into(),
                DockerRow::Container(index) => table.containers()[*index].name.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            names,
            [
                "php-56 true",
                "vaultwarden false",
                "独立容器",
                "exciting_tesla",
                "forex-web",
            ]
        );
        assert_eq!(table.counts(), [4, 0, 0, 0]);
    }

    #[test]
    fn what_the_containers_use_is_marked() {
        let mut web = container("web", ContainerState::Running, None);
        web.mounts = vec!["web-data".into(), "/srv/web".into()];
        web.image = "nginx".into();
        web.networks = vec!["web_default".into()];
        let volume = |name: &str| Volume {
            name: name.into(),
            driver: "local".into(),
            mountpoint: String::new(),
            used_by: Vec::new(),
        };
        let image = |id: &str, reference: Option<&str>| Image {
            id: id.into(),
            reference: reference.map(str::to_owned),
            size: String::new(),
            created: String::new(),
            used_by: Vec::new(),
        };
        let network = |name: &str| Network {
            id: name.into(),
            name: name.into(),
            driver: "bridge".into(),
            scope: "local".into(),
            used_by: Vec::new(),
        };
        let table = DockerTable::new(
            None,
            None,
            vec![web],
            vec![volume("web-data"), volume("old-data")],
            vec![
                image("aaa", None),
                image("bbb", Some("nginx:latest")),
                image("ccc", Some("redis:7")),
            ],
            vec![network("web_default"), network("bridge")],
        );
        assert_eq!(table.summary(), "Docker");
        let volumes: Vec<(&str, Vec<String>)> = table
            .volumes()
            .iter()
            .map(|volume| (volume.name.as_str(), volume.used_by.clone()))
            .collect();
        assert_eq!(
            volumes,
            [("old-data", vec![]), ("web-data", vec!["web".to_owned()])]
        );
        let images: Vec<(&str, usize)> = table
            .images()
            .iter()
            .map(|image| (image.id.as_str(), image.used_by.len()))
            .collect();
        // Named first; nginx:latest is what 「nginx」 runs.
        assert_eq!(images, [("bbb", 1), ("ccc", 0), ("aaa", 0)]);
        let networks: Vec<(&str, usize, bool)> = table
            .networks()
            .iter()
            .map(|network| {
                (
                    network.name.as_str(),
                    network.used_by.len(),
                    network.builtin(),
                )
            })
            .collect();
        assert_eq!(networks, [("bridge", 0, true), ("web_default", 1, false)]);

        // The card and the details say what is in use and what can go.
        let summary = |object, id: &str| table.summary_of(object, id).expect(id);
        let data = summary(DockerObject::Volume, "web-data");
        assert_eq!(data.tag, Some(("使用中".into(), Tone::Good)));
        assert_eq!(data.removable, Err("有容器在用，不能删除"));
        assert_eq!(data.used_by, ["web"]);
        assert_eq!(summary(DockerObject::Volume, "old-data").removable, Ok(()));
        let dangling = summary(DockerObject::Image, "aaa");
        assert_eq!(dangling.name, "未命名镜像");
        assert_eq!(dangling.tag, None);
        assert_eq!(
            summary(DockerObject::Network, "bridge").removable,
            Err("Docker 自带的网络，不能删除")
        );
        assert_eq!(
            summary(DockerObject::Network, "web_default").tag,
            Some(("1 个容器".into(), Tone::Good))
        );
        assert_eq!(table.summary_of(DockerObject::Volume, "gone"), None);
    }

    #[test]
    fn a_state_reads_from_an_old_dockers_status_too() {
        assert_eq!(
            ContainerState::parse("", "Up 3 days"),
            ContainerState::Running
        );
        assert_eq!(
            ContainerState::parse("", "Up 3 days (Paused)"),
            ContainerState::Paused
        );
        assert_eq!(
            ContainerState::parse("", "Exited (0) 2 hours ago"),
            ContainerState::Exited
        );
        assert_eq!(
            ContainerState::parse("running", "Up 1 second"),
            ContainerState::Running
        );
        assert!(ContainerState::Restarting.is_up());
        assert!(!ContainerState::Created.is_up());
    }
}
