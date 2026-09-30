use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, separator::Separator,
    status_bar::StatusBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::explorer::ExplorerStatus;
use crate::session::{ConnectionState, Session};
use crate::terminal::{TerminalLifecycle, TerminalStatus};

/// The active connection/process state on the left and terminal facts on the
/// right. Local terminals report their real emulator cursor coordinates.
#[derive(IntoElement)]
pub enum WorkspaceStatus {
    Session(Option<Session>),
    Local(TerminalStatus),
    /// An SFTP tab: its own connection and what went wrong in it.
    Explorer(Option<Session>, ExplorerStatus),
}

/// The icon for a connection state; a dropped one, like a problem, in red.
fn state_icon(state: ConnectionState, cx: &App) -> Icon {
    match state {
        ConnectionState::Connected => {
            Icon::new(IconName::CircleCheck).text_color(cx.theme().success)
        }
        ConnectionState::Connecting => {
            Icon::new(IconName::LoaderCircle).text_color(cx.theme().warning)
        }
        ConnectionState::Disconnected => {
            Icon::new(CatalogIcon::Unplug).text_color(cx.theme().danger)
        }
    }
}

impl RenderOnce for WorkspaceStatus {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        // Red when something is wrong: a dropped connection, or an SFTP
        // tab's directory that could not be read.
        let mut alarming = false;
        let (text, icon, address, cursor): (SharedString, Icon, Option<String>, String) = match self
        {
            WorkspaceStatus::Session(active) => match active {
                Some(session) => {
                    alarming = session.state == ConnectionState::Disconnected;
                    (
                        format!("{} {}", session.state.label(), session.name).into(),
                        state_icon(session.state, cx),
                        Some(session.address()),
                        "行 1，列 1".into(),
                    )
                }
                None => (
                    ConnectionState::Disconnected.label().into(),
                    Icon::new(CatalogIcon::Unplug).text_color(muted),
                    None,
                    "行 1，列 1".into(),
                ),
            },
            WorkspaceStatus::Explorer(session, status) => {
                let name = session
                    .as_ref()
                    .map_or_else(|| "SFTP".into(), |session| session.name.clone());
                let state = format!("{} {name}", status.state.label());
                let (text, icon) = match (status.state, status.problem) {
                    (ConnectionState::Disconnected, Some(problem)) => {
                        alarming = true;
                        (format!("{state}：{problem}"), state_icon(status.state, cx))
                    }
                    // Connected, but a directory could not be read.
                    (ConnectionState::Connected, Some(problem)) => {
                        alarming = true;
                        (
                            problem.to_string(),
                            Icon::new(IconName::CircleX).text_color(cx.theme().danger),
                        )
                    }
                    (now, _) => {
                        alarming = now == ConnectionState::Disconnected;
                        (state, state_icon(now, cx))
                    }
                };
                (
                    text.into(),
                    icon,
                    session.map(|session| session.address()),
                    "行 1，列 1".into(),
                )
            }
            WorkspaceStatus::Local(status) => {
                let lifecycle = status.lifecycle();
                let icon = match lifecycle {
                    TerminalLifecycle::Running => {
                        Icon::new(IconName::CircleCheck).text_color(cx.theme().success)
                    }
                    TerminalLifecycle::Starting => {
                        Icon::new(IconName::LoaderCircle).text_color(cx.theme().warning)
                    }
                    TerminalLifecycle::Exited { .. }
                    | TerminalLifecycle::Failed(_)
                    | TerminalLifecycle::Closing => {
                        Icon::new(CatalogIcon::Terminal).text_color(muted)
                    }
                };
                (
                    format!("{} 本地终端", lifecycle.label()).into(),
                    icon,
                    None,
                    format!(
                        "行 {}，列 {}",
                        status.cursor_row() + 1,
                        status.cursor_column() + 1
                    ),
                )
            }
        };

        let danger = cx.theme().danger;
        StatusBar::new()
            .left(
                div()
                    .id("status-connection")
                    .test_support()
                    .aria_label(text.clone())
                    .child(
                        h_flex()
                            .gap_1()
                            .when(alarming, |this| this.text_color(danger))
                            .child(icon.small())
                            .child(text),
                    ),
            )
            .when_some(address, |bar, address| {
                bar.left(Separator::vertical()).left(address)
            })
            .right("编码 UTF-8")
            .right(Separator::vertical())
            .right("终端 xterm-256color")
            .right(Separator::vertical())
            .right(cursor)
    }
}
