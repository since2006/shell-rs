//! 终端: shared terminal emulation, transports and Dock panels.

mod box_drawing;
mod broadcast;
mod directory;
mod directory_setup;
mod engine;
mod exec;
mod font;
mod highlight;
mod input;
mod kept_output;
mod links;
mod local_pty;
mod local_shell;
mod local_terminal_panel;
mod log;
mod model;
mod mouse;
mod notices;
mod run;
mod scrollbar;
mod search;
mod send;
mod send_commands;
mod terminal_panel;
mod terminal_view;
mod theme;
mod transport;
mod zmodem;

pub(crate) use broadcast::BroadcastSelection;
pub use directory::ReportedDirectory;
pub use directory_setup::{
    FollowCheck, FollowSetup, check_command as directory_check_command,
    install_command as directory_install_command, installed as directory_installed,
    parse_check as parse_directory_check,
};
pub use engine::{TerminalCell, TerminalEngine, TerminalEvent, TerminalSnapshot};
pub use exec::{ExecTarget, exec_answer};
pub use font::{
    DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, FONT_SIZE_RANGE, LINE_HEIGHT_RANGE, TerminalFont,
    TerminalFontPreview, is_font_installed, monospace_font_families,
};
pub use highlight::{
    HighlightColor, HighlightRule, HighlightSet, PatternError, TerminalHighlights, default_rules,
};
pub use input::{TerminalInput, TerminalInputEvent};
pub use kept_output::KEPT_LINES;
pub use local_pty::LocalPtyTransportProvider;
pub use local_shell::{LocalLaunch, LocalShell, default_shell_name, installed_shells, quote_word};
pub use local_terminal_panel::{LocalTerminalPanel, LocalTerminalPanelEvent, SavedName};
pub use log::{
    DEFAULT_LOG_TEMPLATE, DEFAULT_LOG_TIMESTAMP_FORMAT, SessionLog, SessionLogIdentity,
    SessionLogPlan, SessionLogSettings, default_log_directory, expand_template, log_path,
    valid_timestamp_format,
};
pub use model::{
    LocalTerminalId, RemoteTerminalId, RightClick, TerminalInteraction, TerminalLifecycle,
    TerminalSize, TerminalStatus,
};
pub use notices::TerminalNotice;
pub use run::{
    BusyReason, RunContext, RunEvent, RunFailure, RunHandle, RunProgress, RunShell, RunTiming,
    TerminalRun,
};
pub use search::{SearchDirection, SearchMark, SearchPosition};
pub use send::{CHARACTER_DELAY_RANGE, LINE_DELAY_RANGE, SendDelay, delayed_steps};
pub use send_commands::open_send_commands_dialog;
pub use terminal_panel::{TerminalPanel, TerminalPanelEvent};
pub(crate) use terminal_view::terminal_key_bindings;
pub use terminal_view::{
    TERMINAL_FIND_KEY_CONTEXT, TERMINAL_KEY_CONTEXT, TerminalMenuItems, TerminalView,
};
pub use theme::{TerminalColors, TerminalTheme};
pub(crate) use transport::send_event;
pub use transport::{
    ChannelStream, ExecRequest, ExecResult, FixedLocalTerminalTransportProvider,
    FixedRemoteTerminalTransportProvider, FixedSerialTerminalTransportProvider,
    FixedTelnetTerminalTransportProvider, HostTerminalTransports, LocalTerminalTransportProvider,
    RemoteTerminalTransportProvider, SerialTerminalTransportProvider, SftpChannel,
    SftpChannelRequest, SharedLocalTerminalTransportProvider,
    SharedRemoteTerminalTransportProvider, SharedSerialTerminalTransportProvider,
    SharedTelnetTerminalTransportProvider, SharedTerminalTransportFactory,
    TelnetTerminalTransportProvider, TerminalChannels, TerminalTransport, TerminalTransportCommand,
    TerminalTransportEvent, TerminalTransportFactory,
};
pub use zmodem::{ZmodemDirection, ZmodemReply};
