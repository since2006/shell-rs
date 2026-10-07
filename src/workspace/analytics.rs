//! 匿名使用统计 as the workspace runs it: the window coming forward, the
//! features used and the settings, told to the analytics thread. Nothing
//! here waits for anything: the thread works out what to send, and when.

use gpui_kit::component::ThemeMode;
use gpui_kit::*;

use crate::analytics::{Analytics, Counter, Snapshot};
use crate::app::{ShortcutOverrides, ToolKind};
use crate::host::{AuthKind, ForwardKind, HostId, Route};
use crate::settings::Choice as _;

use super::Workspace;

/// The handle, and the switches as last seen, to count their changes.
pub(super) struct Reporting {
    analytics: Analytics,
    highlight: bool,
    external_cli: bool,
}

impl Workspace {
    /// Report to `analytics` from now on. `Workspace::new` hands in the real
    /// one; the tests one whose signals they read.
    pub fn set_analytics(
        &mut self,
        analytics: Analytics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = self.settings().read(cx).settings();
        self.analytics = Some(Reporting {
            analytics,
            highlight: settings.terminal_highlight.enabled,
            external_cli: settings.external_cli.enabled,
        });
        // The system's event, not a timer: a window left in front across
        // midnight misses that day, which is close enough.
        let subscription = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.report_active(cx);
            }
        });
        self._subscriptions.push(subscription);
    }

    /// ShellRS has started, with what is set up.
    pub(super) fn report_started(&self, cx: &App) {
        if let Some(reporting) = &self.analytics {
            reporting.analytics.started(self.usage_snapshot(cx));
        }
    }

    fn report_active(&self, cx: &App) {
        if let Some(reporting) = &self.analytics {
            reporting.analytics.window_active(self.usage_snapshot(cx));
        }
    }

    /// A use of a feature.
    pub(super) fn count(&self, counter: Counter) {
        if let Some(reporting) = &self.analytics {
            reporting.analytics.count(counter);
        }
    }

    /// An SSH terminal of `host` connected, or an SFTP tab: counted, then
    /// counted again by how.
    pub(super) fn count_connection(&self, host: HostId, sftp: bool, cx: &App) {
        if self.analytics.is_none() {
            return;
        }
        let store = self.store.read(cx);
        let Some(saved) = store.host(host) else {
            return;
        };
        let mut counters = Vec::new();
        if sftp {
            counters.push(Counter::Sftp);
            if store.is_external(host) {
                counters.push(Counter::SftpExternal);
            } else if store.is_temporary(host) {
                counters.push(Counter::SftpTemporary);
            }
        } else {
            counters.push(Counter::Ssh);
            match saved.route {
                Route::Direct => {}
                Route::Jump(_) => counters.push(Counter::SshJump),
                Route::Proxy(_) => counters.push(Counter::SshProxy),
            }
            if saved.credential.is_some() {
                counters.push(Counter::SshCredential);
            } else if saved.auth == AuthKind::NoPassword {
                counters.push(Counter::SshNoPassword);
            }
            if store.is_external(host) {
                counters.push(Counter::SshExternal);
            } else if store.is_temporary(host) {
                counters.push(Counter::SshTemporary);
            }
        }
        for counter in counters {
            self.count(counter);
        }
    }

    pub(super) fn count_forward(&self, kind: ForwardKind) {
        self.count(match kind {
            ForwardKind::Local => Counter::ForwardLocal,
            ForwardKind::Remote => Counter::ForwardRemote,
            ForwardKind::Dynamic => Counter::ForwardDynamic,
        });
    }

    pub(super) fn count_tool(&self, tool: ToolKind) {
        self.count(match tool {
            ToolKind::Snippets => Counter::ToolSnippets,
            ToolKind::History => Counter::ToolHistory,
            ToolKind::Docker => Counter::ToolDocker,
            ToolKind::Services => Counter::ToolServices,
            ToolKind::Processes => Counter::ToolProcesses,
            ToolKind::Connections => Counter::ToolNetstat,
            ToolKind::Monitor => Counter::ToolMonitor,
        });
    }

    /// The rules marked to start with ShellRS, which started with it.
    pub(super) fn count_automatic_forwards(&self, cx: &App) {
        let kinds: Vec<ForwardKind> = self
            .store
            .read(cx)
            .forwards()
            .iter()
            .filter(|rule| rule.auto_start)
            .map(|rule| rule.kind)
            .collect();
        for kind in kinds {
            self.count_forward(kind);
        }
    }

    /// 发送匿名使用统计 from the settings, and the switches whose changes
    /// are counted. Turned on, today is reported if it was not yet.
    pub(super) fn sync_analytics(&mut self, window: &Window, cx: &App) {
        let settings = self.settings().read(cx).settings();
        let snapshot = self.usage_snapshot(cx);
        let Some(reporting) = &mut self.analytics else {
            return;
        };
        let turned_on = settings.analytics.enabled && !reporting.analytics.is_enabled();
        reporting.analytics.set_enabled(settings.analytics.enabled);
        let highlight = settings.terminal_highlight.enabled;
        if highlight != reporting.highlight {
            reporting.highlight = highlight;
            reporting.analytics.count(if highlight {
                Counter::HighlightOn
            } else {
                Counter::HighlightOff
            });
        }
        let external_cli = settings.external_cli.enabled;
        if external_cli != reporting.external_cli {
            reporting.external_cli = external_cli;
            reporting.analytics.count(if external_cli {
                Counter::ExternalCliOn
            } else {
                Counter::ExternalCliOff
            });
        }
        if turned_on && window.is_window_active() {
            reporting.analytics.window_active(snapshot);
        }
    }

    /// How ShellRS is set up, in the terms the statistics use: values from
    /// fixed sets, and how many hosts and such there are, never what they
    /// are.
    fn usage_snapshot(&self, cx: &App) -> Snapshot {
        let settings = self.settings().read(cx).settings();
        let store = self.store.read(cx);
        Snapshot {
            appearance: settings.appearance.key(),
            language: settings.language.key(),
            light_theme: settings.terminal_theme.theme(ThemeMode::Light).key(),
            dark_theme: settings.terminal_theme.theme(ThemeMode::Dark).key(),
            ui_follows_theme: settings.terminal_theme.app_follows,
            highlight: settings.terminal_highlight.enabled,
            external_cli: settings.external_cli.enabled,
            auto_update: settings.update.automatic,
            update_channel: settings.update.channel.key(),
            custom_shortcuts: settings.shortcuts != ShortcutOverrides::default(),
            hosts: store.hosts().len(),
            credentials: store.credentials().len(),
            forwards: store.forwards().len(),
            snippets: store.snippets().len(),
        }
    }
}
