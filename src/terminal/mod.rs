//! 终端: shared terminal emulation, transports and Dock panels.

mod engine;
mod local_pty;
mod local_terminal_panel;
mod mock_shell;
mod mock_transport;
mod model;
mod terminal_panel;
mod terminal_view;
mod transport;

pub use engine::{TerminalCell, TerminalEngine, TerminalEngineEvent, TerminalSnapshot};
pub use local_pty::LocalPtyTransportFactory;
pub use local_terminal_panel::{LocalTerminalPanel, LocalTerminalPanelEvent};
pub use mock_shell::*;
pub use mock_transport::MockSshTransportFactory;
pub use model::{LocalTerminalId, TerminalLifecycle, TerminalSize, TerminalStatus};
pub use terminal_panel::{TerminalPanel, TerminalPanelEvent};
pub(crate) use terminal_view::terminal_key_bindings;
pub use terminal_view::{TERMINAL_KEY_CONTEXT, TerminalView, TerminalViewEvent};
pub(crate) use transport::send_event;
pub use transport::{
    SharedTerminalTransportFactory, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory,
};
