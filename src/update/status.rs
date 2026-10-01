//! How the updater's state reads: the line on 设置 › 关于, the title bar
//! button, which buttons apply, and what a restart interrupts. Pure, so
//! every wording is tested without a window.

use super::updater::{Phase, Stage, UpdateSnapshot};

/// The title bar's update button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateBadge {
    /// Its tooltip and accessible name.
    pub label: String,
    /// Something went wrong, rather than something being ready.
    pub trouble: bool,
}

/// The colour a status line is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Success,
    Warning,
    Danger,
}

impl UpdateSnapshot {
    /// The version on offer, found or needing the download page.
    pub fn offered_version(&self) -> Option<String> {
        self.release
            .as_ref()
            .map(|release| release.version.to_string())
            .or_else(|| {
                self.manual
                    .as_ref()
                    .map(|manual| manual.version.to_string())
            })
    }

    /// Whether the version on offer has to be downloaded by hand.
    pub fn needs_download_page(&self) -> bool {
        self.manual.is_some() || (self.release.is_some() && self.unsupported.is_some())
    }

    /// The line under 更新 on 设置 › 关于.
    pub fn status_line(&self) -> (String, Tone) {
        let offered = self.offered_version().unwrap_or_default();
        match &self.phase {
            Phase::Off => (
                self.unsupported
                    .as_ref()
                    .map(|reason| reason.reason())
                    .unwrap_or_else(|| "不检查更新".into()),
                Tone::Plain,
            ),
            Phase::Idle => ("尚未检查更新".into(), Tone::Plain),
            Phase::Checking => ("正在检查更新…".into(), Tone::Plain),
            Phase::UpToDate => (
                match self.checked_at {
                    Some(at) => format!("已是最新版本 · 上次检查 {}", at.format("%H:%M")),
                    None => "已是最新版本".into(),
                },
                Tone::Success,
            ),
            Phase::Available if self.manual.is_some() => (
                format!("新版本 {offered} 需要从官网下载安装"),
                Tone::Warning,
            ),
            Phase::Available => match &self.unsupported {
                Some(reason) => (
                    format!("发现新版本 {offered}。{}", reason.reason()),
                    Tone::Warning,
                ),
                None => (format!("发现新版本 {offered}"), Tone::Plain),
            },
            Phase::Downloading { done, total } => (
                format!("正在下载 {offered} · {}%", percent(*done, *total)),
                Tone::Plain,
            ),
            Phase::Verifying => (format!("正在校验 {offered}…"), Tone::Plain),
            Phase::Ready => (
                format!("{offered} 已就绪，退出或重启 ShellRS 时安装"),
                Tone::Success,
            ),
            Phase::Installing => (format!("正在安装 {offered}…"), Tone::Plain),
            Phase::Failed { stage, error } => (
                match stage {
                    Stage::Check => format!("检查失败：{error}"),
                    Stage::Download => format!("下载 {offered} 失败：{error}"),
                    Stage::Install => format!("安装 {offered} 失败：{error}"),
                },
                Tone::Danger,
            ),
        }
    }

    /// The title bar button's label, when the title bar shows one: a
    /// version ready to install, one that has to be downloaded by hand, or
    /// automatic downloads that keep failing.
    pub fn badge(&self) -> Option<UpdateBadge> {
        let offered = self.offered_version()?;
        let (label, trouble) = match &self.phase {
            Phase::Ready => (format!("新版本 {offered} 已就绪"), false),
            Phase::Available if self.needs_download_page() => {
                (format!("新版本 {offered} 可用"), false)
            }
            Phase::Failed {
                stage: Stage::Download,
                ..
            } if self.download_failures >= super::updater::FAILURES_BEFORE_BADGE => {
                (format!("新版本 {offered} 下载失败"), true)
            }
            Phase::Failed {
                stage: Stage::Install,
                ..
            } => (format!("新版本 {offered} 安装失败"), true),
            _ => return None,
        };
        Some(UpdateBadge { label, trouble })
    }

    /// 检查更新 applies; `false` while something is already under way.
    pub fn can_check(&self) -> bool {
        matches!(
            self.phase,
            Phase::Idle | Phase::UpToDate | Phase::Available | Phase::Failed { .. }
        )
    }

    /// 下载 (or 重试) applies: a version this copy can install is waiting.
    pub fn can_download(&self) -> bool {
        self.release.is_some()
            && self.unsupported.is_none()
            && matches!(
                self.phase,
                Phase::Available
                    | Phase::Failed {
                        stage: Stage::Download | Stage::Install,
                        ..
                    }
            )
    }
}

/// `45` for 45 %. An empty total counts as done.
pub fn percent(done: u64, total: u64) -> u64 {
    (done.min(total).saturating_mul(100))
        .checked_div(total)
        .unwrap_or(100)
}

/// What a restart would interrupt, counted by the workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RestartImpact {
    pub remote_terminals: usize,
    pub sftp_tabs: usize,
    pub local_terminals: usize,
    pub transfers: usize,
    pub forwards: usize,
}

/// 「重启会关闭 2 个远程终端和 1 个 SFTP 标签，停止 1 个传输（保留续传进度）。」,
/// or nothing when a restart interrupts nothing.
pub fn restart_note(impact: RestartImpact) -> Option<String> {
    let closes = join(&[
        (impact.remote_terminals, "个远程终端"),
        (impact.sftp_tabs, "个 SFTP 标签"),
        (impact.local_terminals, "个本地终端"),
    ]);
    let stops = join(&[
        (impact.transfers, "个传输（保留续传进度）"),
        (impact.forwards, "条端口转发"),
    ]);
    let note = match (closes, stops) {
        (None, None) => return None,
        (Some(closes), None) => format!("重启会关闭 {closes}。"),
        (None, Some(stops)) => format!("重启会停止 {stops}。"),
        (Some(closes), Some(stops)) => format!("重启会关闭 {closes}，停止 {stops}。"),
    };
    Some(note)
}

/// `2 个远程终端、1 个 SFTP 标签和 1 个本地终端`, leaving out what is none.
fn join(parts: &[(usize, &str)]) -> Option<String> {
    let parts: Vec<String> = parts
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| format!("{count} {noun}"))
        .collect();
    match parts.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        [rest @ .., last] => Some(format!("{}和 {}", rest.join("、"), last)),
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;
    use crate::update::UpdateError;
    use crate::update::install::Unsupported;
    use crate::update::manifest::{Asset, Release};
    use crate::update::updater::ManualUpdate;

    fn snapshot(phase: Phase) -> UpdateSnapshot {
        UpdateSnapshot {
            phase,
            current: Version::new(0, 1, 0),
            release: Some(Release {
                version: Version::new(0, 2, 0),
                published_at: None,
                notes: String::new(),
                notes_url: None,
                asset: Asset {
                    urls: Vec::new(),
                    size: 100,
                    sha256: String::new(),
                },
                installer: None,
            }),
            manual: None,
            checked_at: None,
            unsupported: None,
            automatic: true,
            download_failures: 0,
        }
    }

    #[test]
    fn restarting_says_what_it_interrupts() {
        assert_eq!(restart_note(RestartImpact::default()), None);
        assert_eq!(
            restart_note(RestartImpact {
                remote_terminals: 2,
                sftp_tabs: 1,
                local_terminals: 1,
                transfers: 1,
                forwards: 3,
            })
            .as_deref(),
            Some(
                "重启会关闭 2 个远程终端、1 个 SFTP 标签和 1 个本地终端，\
                 停止 1 个传输（保留续传进度）和 3 条端口转发。"
            )
        );
        assert_eq!(
            restart_note(RestartImpact {
                forwards: 1,
                ..RestartImpact::default()
            })
            .as_deref(),
            Some("重启会停止 1 条端口转发。")
        );
    }

    #[test]
    fn each_phase_has_its_line() {
        assert_eq!(snapshot(Phase::Checking).status_line().0, "正在检查更新…");
        assert_eq!(
            snapshot(Phase::Downloading {
                done: 45,
                total: 100
            })
            .status_line()
            .0,
            "正在下载 0.2.0 · 45%"
        );
        let (ready, tone) = snapshot(Phase::Ready).status_line();
        assert_eq!(ready, "0.2.0 已就绪，退出或重启 ShellRS 时安装");
        assert_eq!(tone, Tone::Success);
        let failed = snapshot(Phase::Failed {
            stage: Stage::Download,
            error: UpdateError::Http(503),
        });
        assert_eq!(
            failed.status_line(),
            (
                "下载 0.2.0 失败：更新服务器返回 HTTP 503".into(),
                Tone::Danger
            )
        );
        let mut off = snapshot(Phase::Off);
        off.unsupported = Some(Unsupported::DevelopmentBuild);
        assert_eq!(off.status_line().0, "开发构建，不检查更新");
    }

    #[test]
    fn the_title_bar_offers_a_ready_or_manual_version_and_repeated_failures() {
        let label = |snapshot: &UpdateSnapshot| snapshot.badge().map(|badge| badge.label);
        assert_eq!(
            label(&snapshot(Phase::Ready)).as_deref(),
            Some("新版本 0.2.0 已就绪")
        );
        assert_eq!(snapshot(Phase::Available).badge(), None);
        assert_eq!(
            snapshot(Phase::Downloading { done: 1, total: 2 }).badge(),
            None
        );

        let mut unsupported = snapshot(Phase::Available);
        unsupported.unsupported = Some(Unsupported::NotAppImage);
        assert_eq!(label(&unsupported).as_deref(), Some("新版本 0.2.0 可用"));
        assert!(unsupported.needs_download_page());
        assert!(!unsupported.can_download());

        let mut manual = snapshot(Phase::Available);
        manual.release = None;
        manual.manual = Some(ManualUpdate {
            version: Version::new(3, 0, 0),
            installer: None,
        });
        assert_eq!(label(&manual).as_deref(), Some("新版本 3.0.0 可用"));

        let mut failing = snapshot(Phase::Failed {
            stage: Stage::Download,
            error: UpdateError::Stalled,
        });
        failing.download_failures = 1;
        assert_eq!(failing.badge(), None, "one failure is retried quietly");
        failing.download_failures = 2;
        assert_eq!(
            failing.badge(),
            Some(UpdateBadge {
                label: "新版本 0.2.0 下载失败".into(),
                trouble: true,
            })
        );
        assert!(failing.can_download());
    }

    #[test]
    fn percent_never_overflows_its_bar() {
        assert_eq!(percent(0, 0), 100);
        assert_eq!(percent(50, 200), 25);
        assert_eq!(percent(300, 200), 100);
    }
}
