//! How a live connection ends: hung up by ShellRS, or lost under it.

use std::time::Duration;

use super::connection::SshHandle;

/// How long hanging up waits for the server to answer what was sent before.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long ending a connection waits for its session task to finish.
const END_TIMEOUT: Duration = Duration::from_secs(2);

/// End the connection the way OpenSSH does: tell the server, with
/// `description`, then wait briefly for the session task to have written
/// that and finished. Queuing the message is not enough. A runtime that ends
/// right after, as a terminal's does, drops the task before it runs again,
/// and the server sees the socket closed without a word (issue #140).
pub async fn hang_up(handle: SshHandle, description: &str) {
    // First a ping, which the server answers in turn: once it has, what it
    // sent before has been read, such as its close of a channel just closed
    // (russh does not wait for that). Left unread when the socket closes,
    // it would be answered with a reset, and the server could lose the
    // goodbye.
    let _ = tokio::time::timeout(SETTLE_TIMEOUT, handle.send_ping()).await;
    let _ = tokio::time::timeout(END_TIMEOUT, async {
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, description, "en")
            .await;
        let _ = handle.await;
    })
    .await;
}

/// Why the connection ended, once its channels have ended with it: the
/// session's own error (the socket's, keepalives left unanswered). `None`
/// when the server said goodbye, or the session has not ended yet.
pub async fn why_lost(handle: SshHandle) -> Option<anyhow::Error> {
    match tokio::time::timeout(END_TIMEOUT, handle).await {
        Ok(Err(error)) => Some(error),
        Ok(Ok(())) | Err(_) => None,
    }
}
