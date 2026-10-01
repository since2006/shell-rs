//! Shared SSH connection service and remote terminal adapter.
mod connection;
mod exec;
mod latency;
mod probe;
mod proxy;
#[cfg(test)]
mod route_tests;
mod tester;
mod transport;
pub use connection::{
    AgentLocation, ForwardedTcpip, MissingCredential, SshConnectionConfig, SshConnector, SshHandle,
    SshPrompts, is_network_error,
};
pub use exec::{ExecError, ExecErrorKind, ExecExit, ExecStream, run_command};
pub use latency::{LATENCY_INTERVAL, round_trip};
pub use tester::{SshConnectionTester, describe_login_error};
pub use transport::SshTerminalTransportProvider;
