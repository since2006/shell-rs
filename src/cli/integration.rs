use gpui_kit::*;

use super::install::{
    AgentKind, BinaryStatus, IntegrationPaths, SkillStatus, binary_status, skill_status,
};

/// What the settings page shows about the `shellrs` command and the agent
/// skills: where they go and whether they are there.
///
/// Without paths nothing can be installed. That is the state UI tests start
/// in, so no test writes to the real home directory by accident; they inject
/// a temporary one.
pub struct CliIntegration {
    paths: Option<IntegrationPaths>,
    /// `None` until the first check comes back.
    status: Option<IntegrationStatus>,
    _check: Option<Task<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationStatus {
    pub binary: BinaryStatus,
    skills: Vec<(AgentKind, SkillStatus)>,
}

impl IntegrationStatus {
    pub fn skill(&self, agent: AgentKind) -> SkillStatus {
        self.skills
            .iter()
            .find(|(kind, _)| *kind == agent)
            .map_or(SkillStatus::Missing, |(_, status)| *status)
    }

    fn read(paths: &IntegrationPaths) -> Self {
        Self {
            binary: binary_status(paths),
            skills: AgentKind::ALL
                .iter()
                .map(|agent| (*agent, skill_status(paths, *agent)))
                .collect(),
        }
    }
}

impl CliIntegration {
    pub fn new(paths: Option<IntegrationPaths>) -> Self {
        Self {
            paths,
            status: None,
            _check: None,
        }
    }

    pub fn paths(&self) -> Option<&IntegrationPaths> {
        self.paths.as_ref()
    }

    pub fn status(&self) -> Option<&IntegrationStatus> {
        self.status.as_ref()
    }

    pub fn set_paths(&mut self, paths: Option<IntegrationPaths>, cx: &mut Context<Self>) {
        self.paths = paths;
        self.status = None;
        self.refresh(cx);
    }

    /// Look at the file system again, off the main thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(paths) = self.paths.clone() else {
            self._check = None;
            cx.notify();
            return;
        };
        self._check = Some(cx.spawn(async move |this, cx| {
            let status = cx
                .background_spawn(async move { IntegrationStatus::read(&paths) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.status = Some(status);
                cx.notify();
            });
        }));
    }
}
