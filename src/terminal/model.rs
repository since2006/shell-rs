use std::fmt;

use alacritty_terminal::grid::Dimensions;
use serde::Deserialize;

/// Stable identity for one local-terminal tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct LocalTerminalId(pub u64);

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

    pub fn label(&self) -> &'static str {
        match self {
            Self::Starting => "正在启动",
            Self::Running => "运行中",
            Self::Exited { .. } => "已退出",
            Self::Failed(_) => "启动失败",
            Self::Closing => "正在关闭",
        }
    }
}

impl fmt::Display for TerminalLifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited {
                code,
                signal: Some(signal),
            } => write!(formatter, "已退出（{signal}，退出码 {code}）"),
            Self::Exited { code, signal: None } => write!(formatter, "已退出（退出码 {code}）"),
            Self::Failed(error) => write!(formatter, "启动失败：{error}"),
            state => formatter.write_str(state.label()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalStatus {
    lifecycle: TerminalLifecycle,
    cursor_row: usize,
    cursor_column: usize,
    title: Option<String>,
}

impl TerminalStatus {
    pub fn new(
        lifecycle: TerminalLifecycle,
        cursor_row: usize,
        cursor_column: usize,
        title: Option<String>,
    ) -> Self {
        Self {
            lifecycle,
            cursor_row,
            cursor_column,
            title,
        }
    }

    pub fn lifecycle(&self) -> &TerminalLifecycle {
        &self.lifecycle
    }

    pub fn cursor_row(&self) -> usize {
        self.cursor_row
    }

    pub fn cursor_column(&self) -> usize {
        self.cursor_column
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
}
