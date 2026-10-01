//! 端口转发: SSH port forwards (local, remote and dynamic), each over a
//! connection of its own, and the list and dialog that manage them. The
//! rules themselves are stored with the hosts (`host::ForwardRule`).

mod diagram;
mod forward_dialog;
mod forward_panel;
mod manager;
#[cfg(test)]
mod protocol_tests;
mod socks;
mod transport;
mod worker;

pub use forward_dialog::{ForwardForm, open_forward_dialog};
pub use forward_panel::ForwardPanel;
pub use manager::{ForwardManager, ForwardManagerEvent, ForwardStatus};
pub use transport::{
    ForwardCommand, ForwardEvent, ForwardTransport, ForwardTransportProvider,
    SharedForwardTransportProvider,
};
pub use worker::SshForwardTransportProvider;
