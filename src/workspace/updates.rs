//! The workspace's share of updating: the commands, what a restart would
//! interrupt, and the word after an update.

use std::rc::Rc;

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use crate::app::{
    CheckForUpdates, DownloadUpdate, OpenChangelog, OpenDownloadPage, RestartToUpdate, ShowUpdate,
};
use crate::host::ConnectionState;
use crate::terminal::TerminalLifecycle;
use crate::update::build_info::CHANGELOG_PAGE;
use crate::update::{RestartImpact, UpdaterEvent, open_update_dialog};

use super::workspace_view::Workspace;

impl Workspace {
    pub(super) fn on_check_for_updates(
        &mut self,
        _: &CheckForUpdates,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.updater.update(cx, |updater, cx| updater.check(cx));
    }

    pub(super) fn on_download_update(
        &mut self,
        _: &DownloadUpdate,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.updater.update(cx, |updater, cx| updater.download(cx));
    }

    pub(super) fn on_show_update(
        &mut self,
        _: &ShowUpdate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let updater = self.updater.read(cx).snapshot();
        if updater.offered_version().is_none() {
            return;
        }
        let workspace = cx.weak_entity();
        let impact = Rc::new(move |cx: &App| {
            workspace
                .read_with(cx, |workspace, cx| workspace.restart_impact(cx))
                .unwrap_or_default()
        });
        open_update_dialog(
            self.updater.clone(),
            impact,
            self.focus_handle.clone(),
            window,
            cx,
        );
    }

    pub(super) fn on_restart_to_update(
        &mut self,
        _: &RestartToUpdate,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.updater.update(cx, |updater, cx| updater.restart(cx));
    }

    pub(super) fn on_open_download_page(
        &mut self,
        _: &OpenDownloadPage,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = self.updater.read(cx).download_page();
        cx.open_url(&page);
    }

    /// The whole changelog, not one version's section.
    pub(super) fn on_open_changelog(
        &mut self,
        _: &OpenChangelog,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.open_url(CHANGELOG_PAGE);
    }

    /// The title bar shows the update button only in some phases; the
    /// version after an update is said once the new one runs.
    pub(super) fn on_updater_event(
        &mut self,
        event: &UpdaterEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            UpdaterEvent::Changed => cx.notify(),
            UpdaterEvent::Updated {
                to,
                completed: true,
                ..
            } => window.push_notification(
                Notification::success(format!("ShellRS 已更新到 {to}。")).title("更新完成"),
                cx,
            ),
            UpdaterEvent::Updated { from, to, .. } => window.push_notification(
                Notification::error(format!(
                    "没有装上 {to}，ShellRS 仍是 {from}。可以到官网下载安装。"
                ))
                .title("更新没有完成"),
                cx,
            ),
        }
    }

    /// 自动升级 and 更新渠道, from the settings.
    pub(super) fn sync_updater(&self, cx: &mut App) {
        let settings = self.settings().read(cx).settings().update.clone();
        self.updater.update(cx, |updater, cx| {
            updater.set_automatic(settings.automatic, cx);
            updater.set_channel(settings.channel, cx);
        });
    }

    /// What restarting now would close or stop.
    fn restart_impact(&self, cx: &App) -> RestartImpact {
        RestartImpact {
            remote_terminals: self
                .terminals
                .values()
                .filter(|panel| panel.read(cx).lifecycle(cx) == TerminalLifecycle::Running)
                .count(),
            sftp_tabs: self
                .explorers
                .values()
                .filter(|panel| panel.read(cx).connection_state() == ConnectionState::Connected)
                .count(),
            local_terminals: self
                .local_terminals
                .values()
                .filter(|panel| {
                    panel
                        .read(cx)
                        .terminal()
                        .read(cx)
                        .lifecycle(cx)
                        .accepts_input()
                })
                .count(),
            transfers: self
                .explorers
                .values()
                .filter(|panel| panel.read(cx).is_transferring())
                .count(),
            forwards: self.forwards.read(cx).active_count(),
            unsaved_files: self.all_unsaved_files(cx).len(),
        }
    }
}
