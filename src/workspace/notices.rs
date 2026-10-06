//! What terminals tell the user while they look elsewhere: notifications
//! programs ask for and the bell. The system's notification when the window
//! is not in front, one in the window when another tab is.

use std::time::{Duration, Instant};

use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::*;

use super::Workspace;
use crate::app::CenterTab;
use crate::settings::NotificationSettings;
use crate::shared::RenamableTab as _;
use crate::terminal::{LocalTerminalId, RemoteTerminalId, TerminalNotice};

/// How often one terminal may notify: a program at most every 2 seconds,
/// the bell every 10. More in between are dropped.
const PROGRAM_INTERVAL: Duration = Duration::from_secs(2);
const BELL_INTERVAL: Duration = Duration::from_secs(10);

/// Where a notice goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Delivery {
    /// The system's notification center: the window is not in front.
    System,
    /// A notification in the window.
    InApp,
    None,
}

/// Where `notice` goes. A program asked to notify, so it does so even over
/// its own terminal; the bell rings for typing too (a Tab with nothing to
/// complete), so it only tells of a terminal out of sight.
pub(super) fn delivery(
    notice: &TerminalNotice,
    settings: NotificationSettings,
    window_active: bool,
    in_front: bool,
) -> Delivery {
    let (allowed, quiet_in_front) = match notice {
        TerminalNotice::Program { .. } => (settings.programs, false),
        TerminalNotice::Bell { .. } => (settings.bell, true),
    };
    if !allowed {
        Delivery::None
    } else if !window_active {
        Delivery::System
    } else if in_front && quiet_in_front {
        Delivery::None
    } else {
        Delivery::InApp
    }
}

/// The title and body: the tab's name first, so a notification says which
/// terminal it is from, and a program cannot pass for another application.
pub(super) fn notice_text(tab: &str, notice: &TerminalNotice) -> (String, String) {
    match notice {
        TerminalNotice::Program {
            title: Some(title),
            body,
        } => (format!("{tab}：{title}"), body.clone()),
        TerminalNotice::Program { title: None, body } => (tab.to_string(), body.clone()),
        TerminalNotice::Bell { line } => (format!("{tab}：响铃"), line.clone()),
    }
}

/// Whether a notice may go out now, the last of its kind having gone at
/// `last`.
pub(super) fn due(last: Option<Instant>, now: Instant, interval: Duration) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= interval)
}

/// The system notification's tag: one terminal's newest replaces its last,
/// and a click on it says which terminal to show.
pub(super) fn notice_tag(tab: CenterTab) -> Option<String> {
    match tab {
        CenterTab::Terminal(id) => Some(format!("terminal:remote:{}", id.0)),
        CenterTab::LocalTerminal(id) => Some(format!("terminal:local:{}", id.0)),
        _ => None,
    }
}

pub(super) fn tab_of_tag(tag: &str) -> Option<CenterTab> {
    let rest = tag.strip_prefix("terminal:")?;
    let (kind, id) = rest.split_once(':')?;
    let id = id.parse().ok()?;
    match kind {
        "remote" => Some(CenterTab::Terminal(RemoteTerminalId(id))),
        "local" => Some(CenterTab::LocalTerminal(LocalTerminalId(id))),
        _ => None,
    }
}

impl Workspace {
    /// A terminal has something to tell: say it where the user will see it.
    pub(super) fn on_terminal_notice(
        &mut self,
        tab: CenterTab,
        notice: &TerminalNotice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings = self.settings().read(cx).settings().notifications;
        let in_front = self.active_tab == Some(tab);
        let delivery = delivery(notice, settings, window.is_window_active(), in_front);
        let (Some(tag), Some(tab_title)) = (notice_tag(tab), self.notice_tab_title(tab, cx)) else {
            return;
        };
        if delivery == Delivery::None {
            return;
        }
        let (kind, interval) = match notice {
            TerminalNotice::Program { .. } => ("program", PROGRAM_INTERVAL),
            TerminalNotice::Bell { .. } => ("bell", BELL_INTERVAL),
        };
        let key = format!("{tag}:{kind}");
        // The executor's clock, which tests can move on.
        let now = cx.background_executor().now();
        if !due(self.notice_times.get(&key).copied(), now, interval) {
            return;
        }
        self.notice_times.insert(key, now);
        let (title, body) = notice_text(&tab_title, notice);
        match delivery {
            Delivery::System => cx.show_system_notification(SystemNotification {
                tag: tag.into(),
                title: title.into(),
                body: body.into(),
                actions: Vec::new(),
            }),
            Delivery::InApp => {
                let workspace = cx.weak_entity();
                let notification = Notification::new()
                    .id1::<TerminalNoticeTag>(SharedString::from(tag))
                    .title(title)
                    .message(body)
                    .on_click(move |_, window, cx| {
                        _ = workspace.update(cx, |workspace, cx| {
                            workspace.show_terminal_tab(tab, window, cx)
                        });
                    });
                window.push_notification(notification, cx);
            }
            Delivery::None => {}
        }
    }

    /// A system notification was clicked: bring the window forward on the
    /// terminal it came from, if that is still open.
    pub(super) fn on_notice_clicked(
        &mut self,
        tag: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::app::bring_forward(window, cx);
        if let Some(tab) = tab_of_tag(tag) {
            self.show_terminal_tab(tab, window, cx);
        }
    }

    fn show_terminal_tab(&self, tab: CenterTab, window: &mut Window, cx: &mut Context<Self>) {
        let target = match tab {
            CenterTab::Terminal(id) => self
                .terminals
                .get(&id)
                .map(|panel| (panel.read(cx).tab_group(), panel.entity_id())),
            CenterTab::LocalTerminal(id) => self
                .local_terminals
                .get(&id)
                .map(|panel| (panel.read(cx).tab_group(), panel.entity_id())),
            _ => None,
        };
        if let Some((group, panel)) = target {
            self.activate_tab(group, panel, window, cx);
        }
    }

    fn notice_tab_title(&self, tab: CenterTab, cx: &App) -> Option<String> {
        match tab {
            CenterTab::Terminal(id) => Some(self.terminals.get(&id)?.read(cx).tab_title(cx).into()),
            CenterTab::LocalTerminal(id) => Some(
                self.local_terminals
                    .get(&id)?
                    .read(cx)
                    .default_title()
                    .to_string(),
            ),
            _ => None,
        }
    }
}

/// The in-window notifications of terminals, one a terminal at a time.
struct TerminalNoticeTag;

#[cfg(test)]
mod tests {
    use super::{
        BELL_INTERVAL, CenterTab, Delivery, Duration, Instant, LocalTerminalId,
        NotificationSettings, RemoteTerminalId, TerminalNotice, delivery, due, notice_tag,
        notice_text, tab_of_tag,
    };

    fn program(title: Option<&str>) -> TerminalNotice {
        TerminalNotice::Program {
            title: title.map(str::to_string),
            body: "完成".into(),
        }
    }

    fn bell() -> TerminalNotice {
        TerminalNotice::Bell {
            line: "y/N?".into(),
        }
    }

    #[test]
    fn the_system_tells_of_a_window_away_and_the_window_of_another_tab() {
        let on = NotificationSettings::default();
        for notice in [program(None), bell()] {
            assert_eq!(delivery(&notice, on, false, true), Delivery::System);
            assert_eq!(delivery(&notice, on, true, false), Delivery::InApp);
        }
        // In sight, a program still notifies; the bell does not.
        assert_eq!(delivery(&program(None), on, true, true), Delivery::InApp);
        assert_eq!(delivery(&bell(), on, true, true), Delivery::None);
        let off = NotificationSettings {
            programs: false,
            bell: false,
        };
        assert_eq!(delivery(&program(None), off, false, false), Delivery::None);
        assert_eq!(delivery(&bell(), off, false, false), Delivery::None);
    }

    #[test]
    fn a_notice_is_titled_with_its_tab() {
        assert_eq!(
            notice_text("web-01", &program(Some("构建"))),
            ("web-01：构建".to_string(), "完成".to_string())
        );
        assert_eq!(
            notice_text("web-01", &program(None)),
            ("web-01".to_string(), "完成".to_string())
        );
        assert_eq!(
            notice_text("本地终端 1", &bell()),
            ("本地终端 1：响铃".to_string(), "y/N?".to_string())
        );
    }

    #[test]
    fn notices_are_spaced_out() {
        let start = Instant::now();
        assert!(due(None, start, BELL_INTERVAL));
        assert!(!due(
            Some(start),
            start + Duration::from_secs(9),
            BELL_INTERVAL
        ));
        assert!(due(
            Some(start),
            start + Duration::from_secs(10),
            BELL_INTERVAL
        ));
    }

    #[test]
    fn a_tag_names_its_terminal() {
        for tab in [
            CenterTab::Terminal(RemoteTerminalId(3)),
            CenterTab::LocalTerminal(LocalTerminalId(12)),
        ] {
            assert_eq!(tab_of_tag(&notice_tag(tab).unwrap()), Some(tab));
        }
        assert_eq!(notice_tag(CenterTab::Settings), None);
        assert_eq!(tab_of_tag("terminal:local:x"), None);
        assert_eq!(tab_of_tag("other:local:1"), None);
    }
}
