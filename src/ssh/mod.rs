//! The SSH core (`shellrs-ssh`) as the app uses it, and what is the app's
//! own: the words for its prompts and errors (`describe`), the remote
//! terminal's adapter, and testing a host's connection.
mod describe;
#[cfg(test)]
mod route_tests;
mod tester;
mod transport;
mod worker;
pub use describe::{
    authentication_fields, authentication_instructions, authentication_title, describe_chain,
    describe_channel_error, describe_error, describe_exec_error, describe_link,
    describe_login_error, describe_missing_credential, latency_label, unknown_host_description,
};
pub use shellrs_ssh::*;
pub use tester::SshConnectionTester;
pub use transport::SshTerminalTransportProvider;
pub(crate) use worker::run_terminal_worker;
