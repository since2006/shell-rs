use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, separator::Separator,
    status_bar::StatusBar, tooltip::Tooltip,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::explorer::ExplorerStatus;
use crate::host::{ConnectionState, Host};
use crate::terminal::{TerminalLifecycle, TerminalStatus};

/// The active connection/process state on the left and terminal facts on the
/// right, ending in the size of the terminal in front, local or remote. With
/// no terminal in front there is no size to show.
#[derive(IntoElement)]
pub enum WorkspaceStatus {
    /// The active host, with the terminal in front when that is one of
    /// its terminals: the start page and the settings have none.
    Host(Option<Host>, Option<TerminalStatus>),
    Local(TerminalStatus),
    /// An SFTP tab: its own connection and what went wrong in it.
    Explorer(Option<Host>, ExplorerStatus),
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
        let (text, icon, address, terminal): (
            SharedString,
            Icon,
            Option<String>,
            Option<TerminalStatus>,
        ) = match self {
            WorkspaceStatus::Host(active, terminal) => match active {
                Some(host) => {
                    alarming = host.state == ConnectionState::Disconnected;
                    (
                        format!("{} {}", host.state.label(), host.name).into(),
                        state_icon(host.state, cx),
                        Some(host.endpoint()),
                        terminal,
                    )
                }
                None => (
                    ConnectionState::Disconnected.label().into(),
                    Icon::new(CatalogIcon::Unplug).text_color(muted),
                    None,
                    terminal,
                ),
            },
            WorkspaceStatus::Explorer(host, status) => {
                let name = host
                    .as_ref()
                    .map_or_else(|| "SFTP".into(), |host| host.name.clone());
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
                (text.into(), icon, host.map(|host| host.endpoint()), None)
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
                    Some(status),
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
            .when_some(terminal, |bar, terminal| {
                let (columns, rows) = (terminal.columns(), terminal.rows());
                let size = format!("{columns}×{rows}");
                let explained = format!("终端尺寸：{columns} 列，{rows} 行");
                bar.right(Separator::vertical()).right(
                    div()
                        .id("status-terminal-size")
                        .test_support()
                        .aria_label(size.clone())
                        .tooltip(move |window, cx| {
                            Tooltip::new(explained.clone()).build(window, cx)
                        })
                        .child(size),
                )
            })
    }
}
