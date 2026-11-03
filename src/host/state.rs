//! Where a tab's connection stands. Not host data: a host has no
//! connection of its own, each tab has one.

use gpui_kit::SharedString;

use crate::i18n::t;

/// Where a tab's connection stands: a terminal's or an SFTP tab's own,
/// never a host's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

impl ConnectionState {
    pub fn label(self) -> SharedString {
        match self {
            ConnectionState::Disconnected => t!("host.state.disconnected"),
            ConnectionState::Connecting => t!("host.state.connecting"),
            ConnectionState::Connected => t!("host.state.connected"),
        }
    }

    pub fn is_connected(self) -> bool {
        matches!(self, ConnectionState::Connected)
    }
}
