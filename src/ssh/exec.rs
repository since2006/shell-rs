//! Run one command on a session's host and pass on what it prints: the
//! external CLI's `exec`. Nobody is there to answer a question, so the
//! connection never asks one: a host not trusted yet or a password not saved
//! fails at once and says so.

use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use russh::ChannelMsg;
use tokio::sync::watch;

use super::connection::{
    MissingCredential, SshConnectionConfig, SshConnector, SshPrompts, lock,
    timeout_excluding_prompts,
};
use super::tester::describe_login_error;
use crate::connection::{ConnectionPrompt, ConnectionPromptKind};

/// How long logging in may take.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Which of the command's outputs some bytes came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecStream {
    Stdout,
    Stderr,
}

/// How the command ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecExit {
    Code(u32),
    /// Killed by a signal, such as `TERM`.
    Signal(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecErrorKind {
    /// The host key is not in ShellRS's trust store yet.
    HostKeyUnknown,
    /// The host key is not the one trusted before.
    HostKeyChanged,
    /// Logging in needs a password, passphrase or answer ShellRS has not
    /// saved, or the saved one was refused.
    MissingCredential,
    /// Everything else between dialling and the command's end.
    Connect,
    /// Whoever wanted the output stopped taking it.
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecError {
    pub kind: ExecErrorKind,
    pub message: String,
}

impl ExecError {
    fn new(kind: ExecErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Log in, run `command`, pass its output to `output` as it arrives, and
/// return how it ended. Blocks, with a runtime of its own, so it belongs on
/// a worker thread. The command gets no stdin: it is closed right away, so a
/// program waiting for input ends instead of hanging.
///
/// When `output` fails, the command is abandoned and the connection closed.
pub fn run_command(
    connector: &SshConnector,
    config: &SshConnectionConfig,
    command: &str,
    output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
) -> Result<ExecExit, ExecError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            ExecError::new(ExecErrorKind::Connect, format!("无法启动连接：{error}"))
        })?;
    runtime.block_on(run(connector, config, command, output))
}

/// What the host-key check saw, which the connect error alone does not say.
#[derive(Clone, Copy, Default)]
struct HostKeySeen {
    unknown: bool,
    changed: bool,
}

async fn run(
    connector: &SshConnector,
    config: &SshConnectionConfig,
    command: &str,
    output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
) -> Result<ExecExit, ExecError> {
    let seen = Arc::new(Mutex::new(HostKeySeen::default()));
    // Dropping the sender would read as a shutdown and cancel the login.
    let (_shutdown, shutdown) = watch::channel(false);
    let events = {
        let seen = seen.clone();
        Arc::new(move |prompt: ConnectionPrompt| match prompt.kind() {
            // Declining fails the login at once. Answering later would
            // leave it waiting for someone who is not there.
            ConnectionPromptKind::UnknownHost(_) => {
                lock(&seen).unknown = true;
                false
            }
            ConnectionPromptKind::HostKeyChanged(_) => {
                lock(&seen).changed = true;
                true
            }
            ConnectionPromptKind::Authentication(_) => false,
        })
    };
    let prompts = Arc::new(SshPrompts::new(events, shutdown).non_interactive());
    let connected = timeout_excluding_prompts(
        connector.connect(config, prompts.clone()),
        prompts.prompt_activity_receiver(),
        LOGIN_TIMEOUT,
    )
    .await
    .and_then(|result| result);
    let (handle, _) = connected.map_err(|error| describe_failure(&error, *lock(&seen), config))?;

    let result = run_on(&handle, command, output).await;
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "command finished", "en")
        .await;
    result
}

async fn run_on(
    handle: &super::SshHandle,
    command: &str,
    output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
) -> Result<ExecExit, ExecError> {
    let channel_error = |error: russh::Error| {
        ExecError::new(ExecErrorKind::Connect, format!("无法执行命令：{error}"))
    };
    let mut channel = handle.channel_open_session().await.map_err(channel_error)?;
    channel.exec(true, command).await.map_err(channel_error)?;
    channel.eof().await.map_err(channel_error)?;
    let aborted = |_| ExecError::new(ExecErrorKind::Aborted, "输出已无人接收，命令已放弃");
    let mut exit = None;
    // Until the channel closes, not just until EOF: the exit status can
    // arrive after the last output.
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => {
                output(ExecStream::Stdout, &data).map_err(aborted)?
            }
            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                output(ExecStream::Stderr, &data).map_err(aborted)?
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                exit = Some(ExecExit::Code(exit_status))
            }
            Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                exit = Some(ExecExit::Signal(format!("{signal_name:?}")))
            }
            Some(ChannelMsg::Failure) => {
                return Err(ExecError::new(ExecErrorKind::Connect, "服务器拒绝执行命令"));
            }
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    exit.ok_or_else(|| ExecError::new(ExecErrorKind::Connect, "连接在命令结束前断开"))
}

fn describe_failure(
    error: &anyhow::Error,
    seen: HostKeySeen,
    config: &SshConnectionConfig,
) -> ExecError {
    let endpoint = config.endpoint();
    if seen.changed {
        return ExecError::new(
            ExecErrorKind::HostKeyChanged,
            format!("{endpoint} 的主机密钥与已保存的不一致，已拒绝连接。请先核实服务器身份"),
        );
    }
    if seen.unknown {
        return ExecError::new(
            ExecErrorKind::HostKeyUnknown,
            format!("尚未信任 {endpoint} 的主机密钥：请先在 ShellRS 中连接一次这台主机"),
        );
    }
    if let Some(need) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<MissingCredential>())
    {
        let what = match need {
            MissingCredential::Password { rejected: false } => "这台主机没有保存密码",
            MissingCredential::Password { rejected: true } => "保存的密码被服务器拒绝",
            MissingCredential::Passphrase { rejected: false } => "私钥已加密，但没有保存口令",
            MissingCredential::Passphrase { rejected: true } => "保存的私钥口令不正确",
            MissingCredential::KeyboardInteractive => "服务器要求键盘交互式认证",
        };
        return ExecError::new(
            ExecErrorKind::MissingCredential,
            format!("{what}：请先在 ShellRS 中连接一次这台主机并保存密码"),
        );
    }
    ExecError::new(ExecErrorKind::Connect, describe_login_error(error))
}
