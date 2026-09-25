//! The command's end of the CLI socket: send one request to the running
//! app and print what comes back.

use std::{
    io::{self, BufReader, Write},
    path::Path,
};

use unicode_width::UnicodeWidthStr as _;

use super::protocol::{
    Envelope, ErrorCode, FrameKind, PROTOCOL_VERSION, Reply, Request, SessionInfo,
    TransferCounters, TransferSummary, parse_json, read_frame, write_json,
};
use crate::session::HostOs;

/// Exit code when the command could not do what was asked at all, as ssh
/// uses it.
pub const FAILURE_EXIT: i32 = 255;
/// Exit code when a transfer finished but some items failed.
pub const PARTIAL_EXIT: i32 = 1;

/// Where output goes, and what it goes to.
pub struct Console<'a> {
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
    /// JSON instead of text, asked for or implied by a pipe.
    pub json: bool,
    /// Progress lines only make sense on a terminal.
    pub stderr_is_terminal: bool,
}

impl Console<'_> {
    pub fn error(&mut self, code: ErrorCode, message: &str) -> i32 {
        let _ = writeln!(self.stderr, "shellrs: [{}] {message}", code.as_str());
        FAILURE_EXIT
    }
}

/// Send `request` to the app listening at `socket` and print the answer;
/// the exit code.
pub fn run(socket: &Path, request: Request, console: &mut Console) -> i32 {
    match exchange(socket, request, console) {
        Ok(code) => code,
        Err(Failure::NotRunning) => console.error(
            ErrorCode::NotRunning,
            "ShellRS 未运行：请先打开 ShellRS，并在 设置 → 外部 CLI 中打开「启用外部 CLI」",
        ),
        Err(Failure::Broken(error)) => console.error(
            ErrorCode::ConnectFailed,
            &format!("与 ShellRS 的连接意外中断：{error}"),
        ),
    }
}

enum Failure {
    NotRunning,
    Broken(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Broken(error)
    }
}

#[cfg(unix)]
fn exchange(socket: &Path, request: Request, console: &mut Console) -> Result<i32, Failure> {
    use std::os::unix::net::UnixStream;

    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Failure::NotRunning);
        }
        Err(error) => return Err(error.into()),
    };
    write_json(
        &mut &stream,
        &Envelope {
            version: PROTOCOL_VERSION,
            request,
        },
    )?;
    let mut reader = BufReader::new(&stream);
    let mut progress_shown = false;
    loop {
        let Some((kind, payload)) = read_frame(&mut reader)? else {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        };
        match kind {
            FrameKind::Stdout => {
                console.stdout.write_all(&payload)?;
                console.stdout.flush()?;
            }
            FrameKind::Stderr => {
                console.stderr.write_all(&payload)?;
                console.stderr.flush()?;
            }
            FrameKind::Json => {
                let reply: Reply = parse_json(&payload)?;
                if progress_shown && !matches!(reply, Reply::Progress(_)) {
                    // Off the progress line before anything else is said.
                    let _ = write!(console.stderr, "\r\x1b[2K");
                }
                match reply {
                    Reply::Progress(counters) => {
                        if console.stderr_is_terminal {
                            let _ = write!(console.stderr, "\r\x1b[2K{}", progress_line(counters));
                            let _ = console.stderr.flush();
                            progress_shown = true;
                        }
                    }
                    Reply::Sessions { sessions } => {
                        print_sessions(&sessions, console)?;
                        return Ok(0);
                    }
                    Reply::TransferDone(summary) => return Ok(print_summary(&summary, console)?),
                    Reply::Exit { code } => return Ok(code),
                    Reply::Error { code, message } => return Ok(console.error(code, &message)),
                }
            }
        }
    }
}

#[cfg(not(unix))]
fn exchange(_: &Path, _: Request, console: &mut Console) -> Result<i32, Failure> {
    Ok(console.error(ErrorCode::BadRequest, "此系统暂不支持外部 CLI"))
}

fn print_sessions(sessions: &[SessionInfo], console: &mut Console) -> io::Result<()> {
    if console.json {
        serde_json::to_writer_pretty(&mut *console.stdout, sessions).map_err(io::Error::other)?;
        return writeln!(console.stdout);
    }
    if sessions.is_empty() {
        return writeln!(console.stderr, "没有匹配的会话");
    }
    let rows: Vec<[String; 5]> = sessions
        .iter()
        .map(|session| {
            [
                session.id.clone(),
                session.name.clone(),
                session.group.clone().unwrap_or_default(),
                session.address(),
                session
                    .os
                    .as_deref()
                    .and_then(HostOs::from_stored)
                    .map(|os| os.label().to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    let header = ["ID", "名称", "分组", "地址", "系统"].map(String::from);
    let widths: Vec<usize> = (0..5)
        .map(|column| {
            std::iter::once(&header)
                .chain(&rows)
                .map(|row| row[column].width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(&rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell}{}", " ".repeat(width - cell.width())))
            .collect();
        writeln!(console.stdout, "{}", line.join("  ").trim_end())?;
    }
    Ok(())
}

fn print_summary(summary: &TransferSummary, console: &mut Console) -> io::Result<i32> {
    if console.json {
        serde_json::to_writer_pretty(&mut *console.stdout, summary).map_err(io::Error::other)?;
        writeln!(console.stdout)?;
    } else {
        writeln!(
            console.stdout,
            "已传输 {} 个文件，共 {}{}",
            summary.files,
            format_bytes(summary.bytes),
            match (summary.skipped, summary.failed) {
                (0, 0) => String::new(),
                (skipped, 0) => format!("，跳过 {skipped} 个"),
                (0, failed) => format!("，失败 {failed} 个"),
                (skipped, failed) => format!("，跳过 {skipped} 个，失败 {failed} 个"),
            }
        )?;
        for failure in &summary.failures {
            writeln!(console.stderr, "{failure}")?;
        }
    }
    Ok(if summary.failed > 0 { PARTIAL_EXIT } else { 0 })
}

fn progress_line(counters: TransferCounters) -> String {
    format!(
        "{}/{} 个文件，{} / {}",
        counters.files,
        counters.total_files,
        format_bytes(counters.bytes),
        format_bytes(counters.total_bytes)
    )
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.;
    let mut unit = 0;
    while value >= 1024. && unit < UNITS.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
