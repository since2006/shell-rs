//! The assistant's commands, run where the user said: beside the terminal's
//! shell on a channel of its own (后台执行), or typed into the shell
//! (在终端中执行), as `exec --terminal` types. Either way the answer goes
//! straight to the session from the thread the command ends on.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::*;
use shellrs_assistant::{
    Answerer, CommandAnswer, CommandEnd, FILE_LIMIT, OUTPUT_HEAD, OUTPUT_TAIL, Output, RanAs,
    RunIn, ToolAnswer,
};

use super::describe::run_failure;
use crate::i18n::t;
use crate::ssh::ExecExit;
use crate::terminal::{
    ExecEnd, ExecLimits, ExecOutcome, KeptBytes, RunProgress, RunTiming, TerminalRun, TerminalView,
};

/// How much of each output is kept: what the model is sent of it.
const LIMITS: ExecLimits = ExecLimits {
    head: OUTPUT_HEAD,
    tail: OUTPUT_TAIL,
};

/// A command running for the assistant, to stop.
pub(super) struct RunningCommand {
    stop: Option<Stop>,
}

enum Stop {
    Background(tokio::sync::oneshot::Sender<()>),
    Terminal(Arc<AtomicBool>),
}

impl RunningCommand {
    fn none() -> Self {
        Self { stop: None }
    }

    /// Stop it: `TERM` to one beside the shell, Ctrl+C to one in it. Its
    /// answer still comes, as stopped.
    pub(super) fn stop(&mut self) {
        match self.stop.take() {
            Some(Stop::Background(stop)) => {
                let _ = stop.send(());
            }
            Some(Stop::Terminal(stop)) => stop.store(true, Ordering::SeqCst),
            None => {}
        }
    }
}

/// `cd` into `cwd` first, when the model gave one.
pub(super) fn with_cwd(command: &str, cwd: Option<&str>) -> String {
    match cwd {
        Some(cwd) => format!("cd -- '{}' && {command}", cwd.replace('\'', r"'\''")),
        None => command.to_owned(),
    }
}

/// Run `command` beside `view`'s shell, read on the assistant's `runtime`;
/// stopped after `timeout`.
pub(super) fn run_beside(
    view: &Entity<TerminalView>,
    runtime: &tokio::runtime::Handle,
    command: String,
    timeout: Duration,
    (id, answerer, remember): (String, Answerer, bool),
    cx: &App,
) -> RunningCommand {
    let opened = view.read(cx).channels(cx).open_exec(command.clone());
    let not_connected = t!("assistant.run.not_connected").to_string();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    runtime.spawn(async move {
        let answer = match opened.await {
            Err(_) => ToolAnswer::Failed {
                reason: not_connected,
            },
            Ok(Err(reason)) => ToolAnswer::Failed { reason },
            Ok(Ok(running)) => {
                let timed_out = Arc::new(AtomicBool::new(false));
                let late = timed_out.clone();
                let stop = async move {
                    tokio::select! {
                        _ = stopped => {}
                        () = tokio::time::sleep(timeout) => late.store(true, Ordering::SeqCst),
                    }
                };
                match running.finish(LIMITS, stop).await {
                    Ok(outcome) => ToolAnswer::Command(Box::new(beside_answer(
                        command,
                        outcome,
                        timed_out.load(Ordering::SeqCst),
                    ))),
                    Err(reason) => ToolAnswer::Failed { reason },
                }
            }
        };
        answerer.answer(id, answer, remember);
    });
    RunningCommand {
        stop: Some(Stop::Background(stop)),
    }
}

fn output(kept: &KeptBytes) -> Output {
    Output {
        head: kept.head().to_vec(),
        tail: kept.tail(),
        skipped: kept.skipped(),
    }
}

fn beside_answer(command: String, outcome: ExecOutcome, timed_out: bool) -> CommandAnswer {
    let end = match outcome.end {
        ExecEnd::Exited(Some(ExecExit::Code(code))) => CommandEnd::Exited(Some(i64::from(code))),
        ExecEnd::Exited(Some(ExecExit::Signal(signal))) => CommandEnd::Signal(signal),
        ExecEnd::Exited(None) => CommandEnd::Exited(None),
        ExecEnd::Stopped if timed_out => CommandEnd::TimedOut,
        ExecEnd::Stopped => CommandEnd::Stopped,
    };
    CommandAnswer {
        command,
        run_in: RunIn::Background,
        stdout: output(&outcome.stdout),
        stderr: Some(output(&outcome.stderr)),
        end,
        ran_as: None,
    }
}

/// Type `command` into `view`'s shell and read what it prints, on a thread
/// of its own: the run waits on the terminal. Stopped with Ctrl+C after
/// `timeout`.
pub(super) fn run_in_terminal(
    view: &Entity<TerminalView>,
    command: String,
    timeout: Duration,
    (id, answerer, remember): (String, Answerer, bool),
    cx: &App,
) -> RunningCommand {
    let (run, events) = TerminalRun::new(command.clone());
    if let Err(failure) = view.read(cx).run_command(run.handle(), cx) {
        let reason = run_failure(&failure).to_string();
        answerer.answer(id, ToolAnswer::Failed { reason }, remember);
        return RunningCommand::none();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let started = std::thread::Builder::new()
        .name("shellrs-assistant-run".into())
        .spawn({
            let (id, answerer) = (id.clone(), answerer.clone());
            move || {
                let deadline = Instant::now() + timeout;
                let mut kept = KeptBytes::new(LIMITS);
                let mut context = None;
                let mut timed_out = false;
                let result = run.drive(&events, RunTiming::STANDARD, &mut |progress| {
                    match progress {
                        RunProgress::Context(ran) => context = Some(ran),
                        RunProgress::Output(bytes) => kept.push(&bytes),
                        RunProgress::Idle => {}
                    }
                    if stopping.load(Ordering::SeqCst) {
                        return Err(io::Error::other("stopped"));
                    }
                    if Instant::now() >= deadline {
                        timed_out = true;
                        return Err(io::Error::other("timed out"));
                    }
                    Ok(())
                });
                let stopped = timed_out || stopping.load(Ordering::SeqCst);
                let end = match result {
                    Ok(code) => Some(CommandEnd::Exited(Some(i64::from(code)))),
                    Err(crate::terminal::RunFailure::Disconnected) if stopped => {
                        Some(if timed_out {
                            CommandEnd::TimedOut
                        } else {
                            CommandEnd::Stopped
                        })
                    }
                    Err(failure) => {
                        let reason = run_failure(&failure).to_string();
                        answerer.answer(id, ToolAnswer::Failed { reason }, remember);
                        return;
                    }
                };
                let answer = CommandAnswer {
                    command,
                    run_in: RunIn::Terminal,
                    stdout: output(&kept),
                    stderr: None,
                    end: end.unwrap_or(CommandEnd::Exited(None)),
                    ran_as: context.map(|ran| RanAs {
                        user: ran.user,
                        host: ran.host,
                        cwd: ran.cwd,
                    }),
                };
                answerer.answer(id, ToolAnswer::Command(Box::new(answer)), remember);
            }
        });
    if started.is_err() {
        answerer.answer(
            id,
            ToolAnswer::Failed {
                reason: t!("assistant.run.not_started").to_string(),
            },
            remember,
        );
        return RunningCommand::none();
    }
    RunningCommand {
        stop: Some(Stop::Terminal(stop)),
    }
}

/// Read the start of a text file over SFTP on `view`'s connection, on the
/// assistant's `runtime`.
pub(super) fn read_file(
    view: &Entity<TerminalView>,
    runtime: &tokio::runtime::Handle,
    path: String,
    (id, answerer, remember): (String, Answerer, bool),
    cx: &App,
) {
    let channels = view.read(cx).channels(cx);
    runtime.spawn(async move {
        let answer = match crate::sftp::read_file_start(&channels, &path, FILE_LIMIT as u64).await {
            Ok(start) => ToolAnswer::File {
                text: String::from_utf8_lossy(&start.bytes).into_owned(),
                total_bytes: start.size,
            },
            Err(error) => {
                let failure = crate::sftp::ReadFailure::from_error(error, |why| {
                    crate::sftp::describe_reason(&why)
                });
                ToolAnswer::Failed {
                    reason: crate::sftp::describe_read_failure(&failure),
                }
            }
        };
        answerer.answer(id, answer, remember);
    });
}

/// Put `command` on the shell's line for the user to run: what they see of
/// a command no channel can run.
pub(super) fn insert(
    view: &Entity<TerminalView>,
    command: &str,
    windows: bool,
    cx: &mut App,
) -> Result<(), SharedString> {
    view.update(cx, |view, cx| {
        view.enter_command(command, false, !windows, cx)
    })
}
