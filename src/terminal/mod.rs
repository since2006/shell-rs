//! 终端: shared terminal emulation, transports and Dock panels.

mod engine;
mod exec;
mod font;
mod links;
mod local_pty;
mod local_terminal_panel;
mod model;
mod mouse;
mod search;
mod terminal_panel;
mod terminal_view;
mod transport;

pub use engine::{TerminalCell, TerminalEngine, TerminalEvent, TerminalSnapshot};
pub use exec::{ExecTarget, exec_answer};
pub use font::{
    DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, TerminalFont,
    TerminalFontPreview, is_font_installed, monospace_font_families,
};
pub use local_pty::LocalPtyTransportFactory;
pub use local_terminal_panel::{LocalTerminalPanel, LocalTerminalPanelEvent};
pub use model::{
    LocalTerminalId, RemoteTerminalId, TerminalLifecycle, TerminalSize, TerminalStatus,
};
pub use search::{SearchDirection, SearchMark, SearchPosition};
pub use terminal_panel::{TerminalPanel, TerminalPanelEvent};
pub(crate) use terminal_view::terminal_key_bindings;
pub use terminal_view::{
    TERMINAL_FIND_KEY_CONTEXT, TERMINAL_KEY_CONTEXT, TerminalMenuItems, TerminalView,
};
pub(crate) use transport::send_event;
pub use transport::{
    ExecRequest, ExecResult, FixedRemoteTerminalTransportProvider, RemoteTerminalTransportProvider,
    SharedRemoteTerminalTransportProvider, SharedTerminalTransportFactory, TerminalTransport,
    TerminalTransportCommand, TerminalTransportEvent, TerminalTransportFactory,
};
