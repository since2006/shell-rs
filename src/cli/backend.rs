//! The real backend: `exec` through the SSH connector, transfers through
//! the same SFTP engine as the SFTP tab, both without anyone to ask.

use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};

use super::protocol::{CliError, ErrorCode, TransferCounters, TransferSummary};
use super::server::CliBackend;
use crate::connection::{ConnectionPromptKind, ConnectionPromptReply};
use crate::session::Session;
use crate::sftp::{
    DownloadRequest, RemotePath, SftpCommand, SftpEvent, SharedSftpTransportProvider,
    TransferAnswer, TransferChoice, TransferPhase, TransferProgress, TransferQuestionKind,
    UploadRequest,
};
use crate::ssh::{
    ExecErrorKind, ExecExit, ExecStream, SshConnectionConfig, SshConnector, run_command,
};

/// How often a transfer reports progress to the command.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

pub struct SshCliBackend {
    connector: SshConnector,
    sftp: SharedSftpTransportProvider,
}

impl SshCliBackend {
    /// The same connector as the terminals, so host trust goes through one
    /// file and lock, and the same SFTP provider as the SFTP tab.
    pub fn new(connector: SshConnector, sftp: SharedSftpTransportProvider) -> Self {
        Self { connector, sftp }
    }

    /// Run one transfer on a connection of its own, answering every
    /// question the way an unattended copy should.
    fn transfer(
        &self,
        session: &Session,
        command: SftpCommand,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        let transport = self.sftp.create(session);
        let (commands, command_receiver) = async_channel::unbounded();
        let (event_sender, events) = async_channel::unbounded();
        let worker = std::thread::Builder::new()
            .name("shellrs-cli-sftp".into())
            .spawn(move || transport.run(command_receiver, event_sender))
            .map_err(|error| CliError::new(ErrorCode::ConnectFailed, error.to_string()))?;
        let mut run = Transfer::default();

        let result = (|| {
            // Connected first, so a login that cannot go ahead is reported
            // as such rather than retried.
            loop {
                match events.recv_blocking() {
                    Ok(SftpEvent::Connected { .. }) => break,
                    Ok(SftpEvent::Prompt(prompt)) => run.refuse(&commands, &prompt),
                    Ok(SftpEvent::Disconnected(reason)) => return Err(run.failure(reason)),
                    Ok(_) => {}
                    Err(_) => return Err(run.failure("SFTP 连接意外结束".into())),
                }
            }
            let _ = commands.send_blocking(command);
            let mut reported = Instant::now() - PROGRESS_INTERVAL;
            loop {
                match events.recv_blocking() {
                    Ok(SftpEvent::Progress(state)) => {
                        let due = reported.elapsed() >= PROGRESS_INTERVAL;
                        if due && !run.abandoned {
                            reported = Instant::now();
                            if progress(counters(&state)).is_err() {
                                run.abandoned = true;
                                let _ = commands.send_blocking(SftpCommand::Cancel);
                            }
                        }
                        run.last = Some(state);
                    }
                    Ok(SftpEvent::Question(question)) => {
                        let choice = match (run.abandoned, question.kind()) {
                            (true, _) => TransferChoice::Cancel,
                            (_, TransferQuestionKind::Conflict) => TransferChoice::Overwrite,
                            (
                                _,
                                TransferQuestionKind::Resume | TransferQuestionKind::InvalidResume,
                            ) => TransferChoice::Restart,
                            (_, TransferQuestionKind::Error) => {
                                run.failures.push(question.message().to_string());
                                TransferChoice::Skip
                            }
                        };
                        let _ = commands.send_blocking(SftpCommand::Answer {
                            request_id: question.id(),
                            answer: TransferAnswer::new(
                                choice,
                                question.kind() == TransferQuestionKind::Conflict,
                            ),
                        });
                    }
                    Ok(SftpEvent::Prompt(prompt)) => run.refuse(&commands, &prompt),
                    Ok(SftpEvent::Notice(notice)) => run.notice = Some(notice),
                    Ok(SftpEvent::Disconnected(reason)) => run.notice = Some(reason),
                    Ok(SftpEvent::Idle) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            run.summary()
        })();

        let _ = commands.send_blocking(SftpCommand::Shutdown);
        let _ = worker.join();
        result
    }
}

impl CliBackend for SshCliBackend {
    fn exec(
        &self,
        session: &Session,
        command: &str,
        output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    ) -> Result<i32, CliError> {
        let config = SshConnectionConfig::from(session);
        match run_command(&self.connector, &config, command, output) {
            Ok(ExecExit::Code(code)) => Ok(i32::try_from(code).unwrap_or(255)),
            Ok(ExecExit::Signal(signal)) => {
                let _ = output(
                    ExecStream::Stderr,
                    format!("shellrs: 远程命令被信号 {signal} 终止\n").as_bytes(),
                );
                Ok(signal_exit_code(&signal))
            }
            Err(error) => Err(CliError::new(
                match error.kind {
                    ExecErrorKind::HostKeyUnknown => ErrorCode::HostKeyUnknown,
                    ExecErrorKind::HostKeyChanged => ErrorCode::HostKeyChanged,
                    ExecErrorKind::MissingCredential => ErrorCode::MissingCredential,
                    ExecErrorKind::Connect | ExecErrorKind::Aborted => ErrorCode::ConnectFailed,
                },
                error.message,
            )),
        }
    }

    fn upload(
        &self,
        session: &Session,
        source: &Path,
        destination: &str,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        let request = UploadRequest::scp(source.to_path_buf(), remote_path(destination)?);
        self.transfer(session, SftpCommand::Upload(request), progress)
    }

    fn download(
        &self,
        session: &Session,
        source: &str,
        destination: &Path,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        let request = DownloadRequest::scp(remote_path(source)?, destination.to_path_buf())
            .map_err(|error| CliError::new(ErrorCode::BadRequest, error.to_string()))?;
        self.transfer(session, SftpCommand::Download(request), progress)
    }
}

/// What one transfer ran into along the way.
#[derive(Default)]
struct Transfer {
    last: Option<TransferProgress>,
    failures: Vec<String>,
    /// Why the login could not go ahead, from the question it would have
    /// asked.
    refused: Option<CliError>,
    notice: Option<String>,
    /// The command went away; the transfer is being cancelled.
    abandoned: bool,
}

impl Transfer {
    /// Decline a question only a person can answer, remembering why.
    fn refuse(
        &mut self,
        commands: &async_channel::Sender<SftpCommand>,
        prompt: &crate::connection::ConnectionPrompt,
    ) {
        let refused = match prompt.kind() {
            ConnectionPromptKind::UnknownHost(_) => CliError::new(
                ErrorCode::HostKeyUnknown,
                "尚未信任这台主机的密钥：请先在 ShellRS 中连接一次这台主机",
            ),
            ConnectionPromptKind::HostKeyChanged(_) => CliError::new(
                ErrorCode::HostKeyChanged,
                "主机密钥与已保存的不一致，已拒绝连接。请先核实服务器身份",
            ),
            ConnectionPromptKind::Authentication(_) => CliError::new(
                ErrorCode::MissingCredential,
                "这台主机没有保存可用的密码或口令：请先在 ShellRS 中连接一次这台主机并保存密码",
            ),
        };
        self.refused.get_or_insert(refused);
        let _ = commands.send_blocking(SftpCommand::PromptReply {
            request_id: prompt.request_id(),
            reply: ConnectionPromptReply::Cancel,
        });
    }

    fn failure(&mut self, reason: String) -> CliError {
        self.refused
            .take()
            .unwrap_or_else(|| CliError::new(ErrorCode::ConnectFailed, reason))
    }

    fn summary(&mut self) -> Result<TransferSummary, CliError> {
        match self.last.take() {
            Some(state) if state.phase() == TransferPhase::Completed => Ok(TransferSummary {
                files: state.succeeded() as u64,
                bytes: state.completed_bytes(),
                skipped: state.skipped() as u64,
                failed: state.failed() as u64,
                failures: std::mem::take(&mut self.failures),
            }),
            _ => Err(self.refused.take().unwrap_or_else(|| {
                CliError::new(
                    ErrorCode::TransferFailed,
                    self.notice
                        .take()
                        .unwrap_or_else(|| "传输未完成".to_string()),
                )
            })),
        }
    }
}

fn counters(state: &TransferProgress) -> TransferCounters {
    TransferCounters {
        files: (state.succeeded() + state.skipped() + state.failed()) as u64,
        total_files: state.total() as u64,
        bytes: state.completed_bytes(),
        total_bytes: state.total_bytes(),
    }
}

/// A remote path as a person types it. `~` means the login directory,
/// where SFTP resolves relative paths anyway; a bare name is given a `./`
/// so its parent is the login directory rather than the root.
fn remote_path(path: &str) -> Result<RemotePath, CliError> {
    let path = path.trim();
    let path = match path {
        "" => {
            return Err(CliError::new(ErrorCode::BadRequest, "远程路径不能为空"));
        }
        "~" => ".".to_string(),
        _ if path.starts_with("~/") => format!(".{}", &path[1..]),
        _ if path.starts_with('/') || path.starts_with("./") || path == "." => path.to_string(),
        _ => format!("./{path}"),
    };
    RemotePath::new(path).map_err(|error| CliError::new(ErrorCode::BadRequest, error.to_string()))
}

/// The shell's convention for a command killed by a signal: 128 plus the
/// signal's number.
fn signal_exit_code(signal: &str) -> i32 {
    let number = match signal.trim_start_matches("SIG") {
        "HUP" => 1,
        "INT" => 2,
        "QUIT" => 3,
        "ILL" => 4,
        "ABRT" => 6,
        "FPE" => 8,
        "KILL" => 9,
        "USR1" => 10,
        "SEGV" => 11,
        "PIPE" => 13,
        "ALRM" => 14,
        "TERM" => 15,
        _ => return 255,
    };
    128 + number
}

#[cfg(test)]
mod tests {
    use super::{remote_path, signal_exit_code};

    #[test]
    fn remote_paths_read_like_scp() {
        let path = |input: &str| remote_path(input).unwrap().as_str().to_string();
        assert_eq!(path("/var/log"), "/var/log");
        assert_eq!(path("~"), ".");
        assert_eq!(path("~/app.tar.gz"), "./app.tar.gz");
        assert_eq!(path("release"), "./release");
        assert_eq!(path("./release"), "./release");
        assert!(remote_path("").is_err());
    }

    #[test]
    fn a_signal_exits_the_way_a_shell_reports_it() {
        assert_eq!(signal_exit_code("TERM"), 143);
        assert_eq!(signal_exit_code("KILL"), 137);
        assert_eq!(signal_exit_code("Custom(\"XCPU\")"), 255);
    }
}
