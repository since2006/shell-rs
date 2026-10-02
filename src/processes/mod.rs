//! 进程管理: the right sidebar's tool that lists the processes of the host
//! of the SSH terminal in front, with their memory and CPU, and ends them,
//! over that terminal's own connection. Linux hosts only, for now.

mod linux;
mod model;
mod process_details;
mod process_panel;

pub use linux::{end_command, ended};
pub use model::{Process, ProcessDetails, ProcessSort};
pub use process_details::open_process_dialog;
pub use process_panel::ProcessPanel;
