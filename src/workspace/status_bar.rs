use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, separator::Separator,
    status_bar::StatusBar,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::CatalogIcon;
use crate::session::{ConnectionState, Session};
use crate::terminal::{TerminalLifecycle, TerminalStatus};

pub enum WorkspaceStatusSource {
    Session(Option<Session>),
    Local(TerminalStatus),
}

/// The active connection/process state on the left and terminal facts on the
/// right. Local terminals report their real emulator cursor coordinates.
#[derive(IntoElement)]
pub struct WorkspaceStatus {
    source: WorkspaceStatusSource,
}

impl WorkspaceStatus {
    pub fn session(active: Option<Session>) -> Self {
        Self {
            source: WorkspaceStatusSource::Session(active),
        }
    }

    pub fn local(status: TerminalStatus) -> Self {
        Self {
            source: WorkspaceStatusSource::Local(status),
        }
    }
}

impl RenderOnce for WorkspaceStatus {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let (text, icon, address, cursor): (SharedString, Icon, Option<String>, String) =
            match self.source {
                WorkspaceStatusSource::Session(active) => match active {
                    Some(session) => {
                        let icon = match session.state {
                            ConnectionState::Connected => {
                                Icon::new(IconName::CircleCheck).text_color(cx.theme().success)
                            }
                            ConnectionState::Connecting => {
                                Icon::new(IconName::LoaderCircle).text_color(cx.theme().warning)
                            }
                            ConnectionState::Disconnected => {
                                Icon::new(CatalogIcon::Unplug).text_color(muted)
                            }
                        };
                        (
                            format!("{} {}", session.state.label(), session.name).into(),
                            icon,
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
                WorkspaceStatusSource::Local(status) => {
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

        StatusBar::new()
            .left(
                div()
                    .id("status-connection")
                    .test_support()
                    .aria_label(text.clone())
                    .child(h_flex().gap_1().child(icon.small()).child(text)),
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
