//! The external CLI: the `shellrs` command AI agents use to run commands
//! and move files on the user's saved sessions. The command holds no
//! credentials and opens no database: it hands each request to the running
//! app over a local socket, and the app connects with what it has saved.

mod backend;
mod client;
mod install;
#[cfg(any(windows, test))]
mod install_windows;
mod integration;
#[cfg(windows)]
mod pipe_windows;
mod protocol;
mod server;
#[cfg(test)]
mod tests;

use std::{
    ffi::OsString,
    io::{self, IsTerminal as _, Read as _, Write},
    path::PathBuf,
};

use clap::{Parser, Subcommand};

pub use backend::SshCliBackend;
pub use client::{Console, FAILURE_EXIT, PARTIAL_EXIT, activate_running_app};
pub use install::{
    AgentKind, BinaryStatus, IntegrationPaths, SKILL, SkillStatus, UserPath, binary_status,
    install_binary, install_skill, remove_binary, remove_skill, skill_status,
    update_outdated_binary, update_outdated_skills,
};
pub use integration::{CliIntegration, IntegrationStatus};
pub use protocol::{CliError, ErrorCode, Request, SessionInfo, TransferCounters, TransferSummary};
pub use server::{CliBackend, CliServer, CliTarget};

const AFTER_HELP: &str = "\
Sessions are named by the 16-character ID that `shellrs list` prints (the one ShellRS copies with 复制 ID).
ShellRS must be running, with 设置 → 外部 CLI → 启用外部 CLI turned on.

Exit codes: exec exits with the remote command's code; 1 means a transfer finished with failures;
255 means shellrs could not do what was asked (the reason is printed as `shellrs: [code] message`).";

/// Run commands and move files on the SSH sessions saved in ShellRS.
///
/// ShellRS holds the passwords and keys and makes the connections; this
/// command never sees them.
#[derive(Debug, Parser)]
#[command(name = "shellrs", version, after_help = AFTER_HELP)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the saved sessions.
    List {
        /// Only sessions whose name, host, user, group or ID matches.
        #[arg(short, long)]
        query: Option<String>,
        /// Print JSON. Implied when stdout is not a terminal.
        #[arg(long)]
        json: bool,
    },
    /// Run one command on a session's host and print its output.
    ///
    /// Opens a temporary connection, runs the command in the login shell,
    /// and closes it. The command gets no stdin. The exit code is the
    /// remote command's.
    Exec {
        /// Session ID, from `shellrs list`.
        id: String,
        /// One complete remote shell command, quoted as one argument.
        #[arg(required_unless_present = "stdin")]
        command: Option<String>,
        /// Read the command from stdin instead, for commands with quotes,
        /// pipes, `$` or several lines.
        #[arg(long, conflicts_with = "command")]
        stdin: bool,
    },
    /// Copy a local file or folder to a session's host.
    ///
    /// Like scp: when the destination is an existing directory the source
    /// goes inside it under its own name; otherwise the destination is the
    /// copy's path. Folders are copied recursively; existing files are
    /// overwritten.
    Upload {
        /// Session ID, from `shellrs list`.
        id: String,
        /// Local file or folder.
        local: PathBuf,
        /// Remote destination. `~` is the login directory.
        remote: String,
        /// Print the summary as JSON. Implied when stdout is not a terminal.
        #[arg(long)]
        json: bool,
    },
    /// Copy a file or folder from a session's host to this machine.
    ///
    /// Destination rules are scp's, as for `upload`.
    Download {
        /// Session ID, from `shellrs list`.
        id: String,
        /// Remote file or folder. `~` is the login directory.
        remote: String,
        /// Local destination.
        local: PathBuf,
        /// Print the summary as JSON. Implied when stdout is not a terminal.
        #[arg(long)]
        json: bool,
    },
}

/// The command line entry: `args` without the program name. Returns the
/// process exit code.
pub fn main(args: Vec<OsString>) -> i32 {
    let cli = match Cli::try_parse_from(std::iter::once(OsString::from("shellrs")).chain(args)) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return error.exit_code();
        }
    };
    let (stdout, stderr) = (io::stdout(), io::stderr());
    let json = !stdout.is_terminal();
    let stderr_is_terminal = stderr.is_terminal();
    let mut stdout = ConsoleText::new(stdout, !json);
    let mut stderr = ConsoleText::new(stderr, stderr_is_terminal);
    let mut console = Console {
        stdout: &mut stdout,
        stderr: &mut stderr,
        json,
        stderr_is_terminal,
    };
    let code = match request(cli.command, &mut console) {
        Ok(request) => client::run(&crate::app::cli_socket_path(), request, &mut console),
        Err(code) => code,
    };
    let _ = stdout.finish();
    let _ = stderr.finish();
    code
}

/// The request a command line asks for, or the exit code of why it
/// cannot be sent.
fn request(command: Command, console: &mut Console) -> Result<Request, i32> {
    Ok(match command {
        Command::List { query, json } => {
            console.json |= json;
            Request::List { query }
        }
        Command::Exec { id, command, stdin } => {
            let command = if stdin {
                let mut text = String::new();
                if let Err(error) = std::io::stdin().read_to_string(&mut text) {
                    return Err(
                        console.error(ErrorCode::BadRequest, &format!("无法读取标准输入：{error}"))
                    );
                }
                text
            } else {
                command.unwrap_or_default()
            };
            let command = normalize_command(&command);
            if command.trim().is_empty() {
                return Err(console.error(ErrorCode::BadRequest, "命令不能为空"));
            }
            Request::Exec {
                session: id,
                command,
            }
        }
        Command::Upload {
            id,
            local,
            remote,
            json,
        } => {
            console.json |= json;
            Request::Upload {
                session: id,
                source: absolute(local, console)?,
                destination: remote,
            }
        }
        Command::Download {
            id,
            remote,
            local,
            json,
        } => {
            console.json |= json;
            Request::Download {
                session: id,
                source: remote,
                destination: absolute(local, console)?,
            }
        }
    })
}

/// A command as the remote shell should see it. PowerShell ends every line
/// it pipes to a program with CRLF, and a POSIX shell would take the CR as
/// part of the command; a BOM would be the start of the first word.
fn normalize_command(command: &str) -> String {
    command
        .strip_prefix('\u{feff}')
        .unwrap_or(command)
        .replace("\r\n", "\n")
}

/// The app runs in another directory, so local paths travel absolute.
fn absolute(path: PathBuf, console: &mut Console) -> Result<PathBuf, i32> {
    std::path::absolute(&path).map_err(|error| {
        console.error(
            ErrorCode::BadRequest,
            &format!("无法解析路径 {}：{error}", path.display()),
        )
    })
}

/// Standard output or error, made safe for a Windows console: remote output
/// may not be UTF-8 (a GBK server, a binary file), and the Windows console
/// refuses bytes that are not. Invalid bytes become U+FFFD there; a
/// character split across two writes is held until its second half comes.
/// Anywhere else, bytes pass through untouched.
struct ConsoleText<W: Write> {
    inner: W,
    lossy: bool,
    /// The start of a character whose rest has not been written yet.
    pending: Vec<u8>,
}

impl<W: Write> ConsoleText<W> {
    fn new(inner: W, is_terminal: bool) -> Self {
        Self {
            inner,
            lossy: cfg!(windows) && is_terminal,
            pending: Vec::new(),
        }
    }

    /// Write out a character cut off at the very end, and flush.
    fn finish(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            self.pending.clear();
            self.inner.write_all("\u{fffd}".as_bytes())?;
        }
        self.inner.flush()
    }
}

impl<W: Write> Write for ConsoleText<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.lossy {
            return self.inner.write(bytes);
        }
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        let mut rest: &[u8] = &self.pending;
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    rest = &[];
                }
                Err(error) => {
                    let (valid, after) = rest.split_at(error.valid_up_to());
                    text.push_str(&String::from_utf8_lossy(valid));
                    match error.error_len() {
                        Some(invalid) => {
                            text.push('\u{fffd}');
                            rest = &after[invalid..];
                        }
                        // The rest of the character is still to come.
                        None => break,
                    }
                }
            }
        }
        self.pending = rest.to_vec();
        self.inner.write_all(text.as_bytes())?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Whether the process was started as the command rather than the app:
/// with arguments, apart from the one macOS adds when launching an app.
pub fn command_line_arguments() -> Option<Vec<OsString>> {
    let args: Vec<OsString> = std::env::args_os()
        .skip(1)
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn_"))
        .collect();
    (!args.is_empty()).then_some(args)
}
