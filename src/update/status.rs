//! How the updater's state reads: the line on 设置 › 关于, the title bar
//! button, which buttons apply, and what a restart interrupts. Pure, so
//! every wording is tested without a window.

use gpui_kit::SharedString;

use super::updater::{Phase, Stage, UpdateSnapshot};
use crate::i18n::{join_list, t, tn};

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
    Warning,
    Danger,
}

/// The one thing to do about the version on offer, when there is one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStep {
    /// Restart into the version downloaded.
    Restart,
    /// Download it by hand from the website.
    DownloadPage,
    /// Download it here; `retry` after a download that failed.
    Download { retry: bool },
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

    /// The line under 当前版本 on 设置 › 关于.
    pub fn status_line(&self) -> (String, Tone) {
        let version = self.offered_version().unwrap_or_default();
        let version = version.as_str();
        let (text, tone) = match &self.phase {
            Phase::Off => (
                match &self.unsupported {
                    Some(reason) => t!("update.status.unsupported", reason = reason.reason()),
                    None => t!("update.status.off"),
                },
                Tone::Plain,
            ),
            Phase::Idle => (t!("update.status.idle"), Tone::Plain),
            Phase::Checking => (t!("update.status.checking"), Tone::Plain),
            Phase::UpToDate => (t!("update.status.up_to_date"), Tone::Plain),
            Phase::Available if self.manual.is_some() => (
                t!("update.status.available_manual", version = version),
                Tone::Warning,
            ),
            Phase::Available => match &self.unsupported {
                Some(reason) => (
                    t!(
                        "update.status.available_unsupported",
                        version = version,
                        reason = reason.reason()
                    ),
                    Tone::Warning,
                ),
                None => (
                    t!("update.status.available", version = version),
                    Tone::Plain,
                ),
            },
            Phase::Downloading { done, total } => (
                t!(
                    "update.status.downloading",
                    version = version,
                    percent = percent(*done, *total)
                ),
                Tone::Plain,
            ),
            Phase::Verifying => (
                t!("update.status.verifying", version = version),
                Tone::Plain,
            ),
            Phase::Ready => (t!("update.status.ready", version = version), Tone::Plain),
            Phase::Installing => (
                t!("update.status.installing", version = version),
                Tone::Plain,
            ),
            Phase::Failed { stage, error } => (
                match stage {
                    Stage::Check => t!("update.status.check_failed", error = error),
                    Stage::Download => {
                        t!(
                            "update.status.download_failed",
                            version = version,
                            error = error
                        )
                    }
                    Stage::Install => {
                        t!(
                            "update.status.install_failed",
                            version = version,
                            error = error
                        )
                    }
                },
                Tone::Danger,
            ),
        };
        (text.to_string(), tone)
    }

    /// What the version on offer needs next, for the button next to the
    /// version and the update dialog's.
    pub fn step(&self) -> Option<UpdateStep> {
        if self.phase == Phase::Ready {
            Some(UpdateStep::Restart)
        } else if self.needs_download_page() {
            Some(UpdateStep::DownloadPage)
        } else if self.can_download() {
            Some(UpdateStep::Download {
                retry: matches!(self.phase, Phase::Failed { .. }),
            })
        } else {
            None
        }
    }

    /// The title bar button's label, when the title bar shows one: a
    /// version ready to install, one that has to be downloaded by hand, or
    /// automatic downloads that keep failing.
    pub fn badge(&self) -> Option<UpdateBadge> {
        let version = self.offered_version()?;
        let version = version.as_str();
        let (label, trouble) = match &self.phase {
            Phase::Ready => (t!("update.badge.ready", version = version), false),
            Phase::Available if self.needs_download_page() => {
                (t!("update.badge.available", version = version), false)
            }
            Phase::Failed {
                stage: Stage::Download,
                ..
            } if self.download_failures >= super::updater::FAILURES_BEFORE_BADGE => {
                (t!("update.badge.download_failed", version = version), true)
            }
            Phase::Failed {
                stage: Stage::Install,
                ..
            } => (t!("update.badge.install_failed", version = version), true),
            _ => return None,
        };
        let label = label.to_string();
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
    /// Editor tabs with changes not saved yet; a restart loses them.
    pub unsaved_files: usize,
}

/// 「重启会关闭 2 个远程终端和 1 个 SFTP 标签，停止 1 个传输（保留续传进度）。」,
/// or nothing when a restart interrupts nothing.
pub fn restart_note(impact: RestartImpact) -> Option<String> {
    let closes = phrases(&[
        (impact.remote_terminals > 0)
            .then(|| tn!("update.restart.remote_terminals", impact.remote_terminals)),
        (impact.sftp_tabs > 0).then(|| tn!("update.restart.sftp_tabs", impact.sftp_tabs)),
        (impact.local_terminals > 0)
            .then(|| tn!("update.restart.local_terminals", impact.local_terminals)),
    ]);
    let stops = phrases(&[
        (impact.transfers > 0).then(|| tn!("update.restart.transfers", impact.transfers)),
        (impact.forwards > 0).then(|| tn!("update.restart.forwards", impact.forwards)),
    ]);
    let note = match (closes, stops) {
        (None, None) => None,
        (Some(closes), None) => Some(t!("update.restart.closes", closes = closes)),
        (None, Some(stops)) => Some(t!("update.restart.stops", stops = stops)),
        (Some(closes), Some(stops)) => Some(t!(
            "update.restart.closes_and_stops",
            closes = closes,
            stops = stops
        )),
    };
    let unsaved = (impact.unsaved_files > 0)
        .then(|| tn!("update.restart.unsaved_files", impact.unsaved_files));
    let note = match (note, unsaved) {
        (Some(note), Some(unsaved)) => Some(t!(
            "update.restart.and_unsaved_files",
            note = note,
            unsaved = unsaved
        )),
        (note, unsaved) => note.or(unsaved),
    };
    note.map(|note| note.to_string())
}

/// `2 个远程终端、1 个 SFTP 标签和 1 个本地终端`, leaving out what is none.
fn phrases(parts: &[Option<SharedString>]) -> Option<String> {
    let parts: Vec<&SharedString> = parts.iter().flatten().collect();
    (!parts.is_empty()).then(|| join_list(&parts))
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
                unsaved_files: 0,
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
        assert_eq!(
            restart_note(RestartImpact {
                sftp_tabs: 1,
                unsaved_files: 2,
                ..RestartImpact::default()
            })
            .as_deref(),
            Some("重启会关闭 1 个 SFTP 标签。2 个文件有未保存的修改，重启后会丢失。")
        );
    }

    #[test]
    fn restarting_says_what_it_interrupts_in_english() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        assert_eq!(
            restart_note(RestartImpact {
                remote_terminals: 2,
                sftp_tabs: 1,
                local_terminals: 1,
                transfers: 1,
                forwards: 3,
                unsaved_files: 0,
            })
            .as_deref(),
            Some(
                "Restarting closes 2 remote terminals, 1 SFTP tab and 1 local terminal \
                 and stops 1 transfer (its progress is kept) and 3 port forwards."
            )
        );
        assert_eq!(
            restart_note(RestartImpact {
                sftp_tabs: 2,
                unsaved_files: 1,
                ..RestartImpact::default()
            })
            .as_deref(),
            Some("Restarting closes 2 SFTP tabs. Unsaved changes in 1 file will be lost.")
        );
        assert_eq!(
            restart_note(RestartImpact {
                unsaved_files: 3,
                ..RestartImpact::default()
            })
            .as_deref(),
            Some("Unsaved changes in 3 files will be lost.")
        );
        assert_eq!(
            snapshot(Phase::Downloading {
                done: 45,
                total: 100
            })
            .status_line()
            .0,
            "Downloading ShellRS 0.2.0… 45%"
        );
        let failed = snapshot(Phase::Failed {
            stage: Stage::Download,
            error: UpdateError::Http(503),
        });
        assert_eq!(
            failed.status_line().0,
            "Couldn’t download ShellRS 0.2.0: the update server answered HTTP 503"
        );
        let mut off = snapshot(Phase::Off);
        off.unsupported = Some(Unsupported::DevelopmentBuild);
        assert_eq!(
            off.status_line().0,
            "Development builds don’t check for updates."
        );
    }

    #[test]
    fn each_phase_has_its_line() {
        assert_eq!(snapshot(Phase::Checking).status_line().0, "正在检查更新…");
        assert_eq!(
            snapshot(Phase::UpToDate).status_line(),
            ("当前已是最新版本。".into(), Tone::Plain)
        );
        assert_eq!(
            snapshot(Phase::Downloading {
                done: 45,
                total: 100
            })
            .status_line()
            .0,
            "正在下载新版本 0.2.0… 45%"
        );
        assert_eq!(
            snapshot(Phase::Ready).status_line().0,
            "新版本 0.2.0 已下载，重启 ShellRS 即可完成更新。"
        );
        let failed = snapshot(Phase::Failed {
            stage: Stage::Download,
            error: UpdateError::Http(503),
        });
        assert_eq!(
            failed.status_line(),
            (
                "下载新版本 0.2.0 失败：更新服务器返回 HTTP 503".into(),
                Tone::Danger
            )
        );
        let mut off = snapshot(Phase::Off);
        off.unsupported = Some(Unsupported::DevelopmentBuild);
        assert_eq!(off.status_line().0, "开发构建，不检查更新。");
    }

    #[test]
    fn one_step_applies_at_a_time() {
        assert_eq!(snapshot(Phase::Ready).step(), Some(UpdateStep::Restart));
        assert_eq!(
            snapshot(Phase::Available).step(),
            Some(UpdateStep::Download { retry: false })
        );
        let failed = snapshot(Phase::Failed {
            stage: Stage::Download,
            error: UpdateError::Stalled,
        });
        assert_eq!(failed.step(), Some(UpdateStep::Download { retry: true }));
        assert_eq!(
            snapshot(Phase::Downloading { done: 1, total: 2 }).step(),
            None
        );
        let mut unsupported = snapshot(Phase::Available);
        unsupported.unsupported = Some(Unsupported::NotAppImage);
        assert_eq!(unsupported.step(), Some(UpdateStep::DownloadPage));
        let mut up_to_date = snapshot(Phase::UpToDate);
        up_to_date.release = None;
        assert_eq!(up_to_date.step(), None);
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
