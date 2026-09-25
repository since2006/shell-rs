//! 终端: shared terminal emulation, transports and Dock panels.

mod engine;
mod local_pty;
mod local_terminal_panel;
mod model;
mod search;
mod terminal_panel;
mod terminal_view;
mod transport;

pub use engine::{TerminalCell, TerminalEngine, TerminalEngineEvent, TerminalSnapshot};
pub use local_pty::LocalPtyTransportFactory;
pub use local_terminal_panel::{LocalTerminalPanel, LocalTerminalPanelEvent};
pub use model::{
    Latency, LatencyLevel, LocalTerminalId, RemoteTerminalId, TerminalLifecycle, TerminalSize,
    TerminalStatus,
};
pub use search::{SearchDirection, SearchMark, SearchPosition};
pub use terminal_panel::{TerminalPanel, TerminalPanelEvent};
pub(crate) use terminal_view::terminal_key_bindings;
pub use terminal_view::{
    TERMINAL_FIND_KEY_CONTEXT, TERMINAL_KEY_CONTEXT, TerminalMenuItems, TerminalView,
    TerminalViewEvent,
};
pub(crate) use transport::send_event;
pub use transport::{
    AuthenticationPrompt, FixedRemoteTerminalTransportProvider, HostKeyChangedPrompt,
    RemoteTerminalTransportProvider, SharedRemoteTerminalTransportProvider,
    SharedTerminalTransportFactory, TerminalPrompt, TerminalPromptField, TerminalPromptKind,
    TerminalPromptReply, TerminalSecret, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory, UnknownHostPrompt,
};
