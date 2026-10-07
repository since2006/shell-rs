//! Docker: the right sidebar's tool that lists the containers of the host of
//! the SSH terminal in front by compose project, with its volumes, images
//! and networks, starts, stops and restarts containers, shows their details
//! and output, and removes what nothing uses, over that terminal's own
//! connection.

mod container_details;
mod details;
mod docker_panel;
mod linux;
mod model;
mod object_details;

pub use container_details::open_container_dialog;
pub use docker_panel::DockerPanel;
pub use linux::{control_command, done, remove_command};
pub use model::{Container, ContainerCommand, ContainerSubject, DockerObject, ObjectSummary};
pub use object_details::open_object_dialog;
