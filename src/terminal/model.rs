use std::fmt;

use alacritty_terminal::grid::Dimensions;
use gpui_kit::SharedString;

use crate::i18n::t;

/// Stable identity for one local-terminal tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LocalTerminalId(pub u64);

/// Stable identity for one remote-terminal connection and its Dock tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RemoteTerminalId(pub u64);

/// The dimensions shared by the emulator and a terminal transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalSize {
    columns: usize,
    rows: usize,
    cell_width: u16,
    cell_height: u16,
}

impl TerminalSize {
    pub const DEFAULT: Self = Self::new(80, 24, 8, 16);

    pub const fn new(columns: usize, rows: usize, cell_width: u16, cell_height: u16) -> Self {
        Self {
            columns,
            rows,
            cell_width,
            cell_height,
        }
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cell_width(&self) -> u16 {
        self.cell_width
    }

    pub fn cell_height(&self) -> u16 {
        self.cell_height
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Process state shown by the terminal panel and the workspace status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalLifecycle {
    Starting,
    Running,
    Exited { code: u32, signal: Option<String> },
    Failed(String),
    Closing,
}

impl TerminalLifecycle {
    pub fn accepts_input(&self) -> bool {
        matches!(self, Self::Running)
    }

    pub fn label(&self) -> SharedString {
        match self {
            Self::Starting => t!("terminal.lifecycle.starting"),
            Self::Running => t!("terminal.lifecycle.running"),
            Self::Exited { .. } => t!("terminal.lifecycle.exited"),
            Self::Failed(_) => t!("terminal.lifecycle.failed"),
            Self::Closing => t!("terminal.lifecycle.closing"),
        }
    }
}

impl fmt::Display for TerminalLifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Exited {
                code,
                signal: Some(signal),
            } => t!(
                "terminal.lifecycle.exited_signal",
                signal = signal,
                code = code
            ),
            Self::Exited { code, signal: None } => {
                t!("terminal.lifecycle.exited_code", code = code)
            }
            Self::Failed(error) => t!("terminal.lifecycle.failed_with", error = error),
            state => state.label(),
        };
        formatter.write_str(&text)
    }
}

/// What the window's status bar shows of a terminal: whether it runs, and
/// how many columns and rows its screen has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalStatus {
    lifecycle: TerminalLifecycle,
    columns: usize,
    rows: usize,
}

impl TerminalStatus {
    pub fn new(lifecycle: TerminalLifecycle, columns: usize, rows: usize) -> Self {
        Self {
            lifecycle,
            columns,
            rows,
        }
    }

    pub fn lifecycle(&self) -> &TerminalLifecycle {
        &self.lifecycle
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn rows(&self) -> usize {
        self.rows
    }
}
