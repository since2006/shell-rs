//! 网络连接: the right sidebar's tool that lists the TCP and UDP sockets of
//! the host of the SSH terminal in front, with the processes holding them,
//! read over that terminal's own connection. Linux hosts only, for now.

mod linux;
mod model;
mod netstat_panel;

pub use netstat_panel::NetstatPanel;
