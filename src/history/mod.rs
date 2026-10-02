//! 历史命令: the right sidebar's tool that lists the commands bash kept in
//! the history file of the host of the SSH terminal in front, read over
//! that terminal's own connection. A click puts one on the terminal's input
//! line; 执行 runs it there.

mod bash;
mod history_panel;
mod model;

pub use history_panel::HistoryPanel;
