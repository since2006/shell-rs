//! Commands the right sidebar's tools run beside an SSH terminal's shell.

use std::sync::mpsc;
use std::time::Duration;

use gpui_kit::*;

use super::{ExecResult, RemoteTerminalId, TerminalView};

/// How often an answer on its way is looked for. It comes from the
/// terminal's transport thread, which must not wake the UI itself.
const POLL: Duration = Duration::from_millis(50);

/// The SSH terminal whose host a tool reads, over that terminal's own
/// connection.
#[derive(Clone, PartialEq)]
pub struct ExecTarget {
    pub terminal: RemoteTerminalId,
    pub view: WeakEntity<TerminalView>,
}

/// Look for a command's answer until it comes. `None` when the transport
/// drops the request instead: the terminal went away.
pub async fn exec_answer(
    reply: mpsc::Receiver<ExecResult>,
    cx: &mut AsyncApp,
) -> Option<ExecResult> {
    loop {
        match reply.try_recv() {
            Ok(result) => return Some(result),
            Err(mpsc::TryRecvError::Empty) => cx.background_executor().timer(POLL).await,
            Err(mpsc::TryRecvError::Disconnected) => return None,
        }
    }
}
