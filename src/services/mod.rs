//! 系统服务: the right sidebar's tool that lists the systemd services of the
//! host of the SSH terminal in front, starts, stops and restarts them, and
//! shows their state and journal, over that terminal's own connection.
//! Linux hosts run by systemd only, for now.

mod linux;
mod model;
mod service_details;
mod service_panel;

pub use linux::{control_command, controlled};
pub use model::{Service, ServiceCommand};
pub use service_details::open_service_dialog;
pub use service_panel::ServicePanel;
