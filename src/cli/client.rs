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

/// Send `request` to the app listening at `endpoint` and print the answer;
/// the exit code.
pub fn run(endpoint: &Path, request: Request, console: &mut Console) -> i32 {
    let result = connect(endpoint).and_then(|stream| talk(&stream, request, console));
    match result {
        Ok(code) => code,
        Err(Failure::NotRunning) => console.error(
            ErrorCode::NotRunning,
            "ShellRS 未运行：请先打开 ShellRS，并在 设置 → 外部 CLI 中打开「启用外部 CLI」",
        ),
        Err(Failure::Refused(message)) => console.error(ErrorCode::ConnectFailed, message),
        Err(Failure::Broken(error)) => console.error(
            ErrorCode::ConnectFailed,
            &format!("与 ShellRS 的连接意外中断：{error}"),
        ),
    }
}

enum Failure {
    NotRunning,
    /// Not allowed to talk to the app, or not willing to.
    Refused(&'static str),
    Broken(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Broken(error)
    }
}

/// What a sandbox that forbids local connections looks like.
const NO_PERMISSION: &str =
    "没有权限连接 ShellRS：Agent 可能运行在沙箱中，需要允许它访问本机的进程间通信";

#[cfg(unix)]
fn connect(socket: &Path) -> Result<std::os::unix::net::UnixStream, Failure> {
    std::os::unix::net::UnixStream::connect(socket).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Failure::NotRunning,
        io::ErrorKind::PermissionDenied => Failure::Refused(NO_PERMISSION),
        _ => error.into(),
    })
}

/// Open the app's pipe, and make sure it is the app's before saying
/// anything: the name is the machine's, so someone else could hold it.
#[cfg(windows)]
fn connect(pipe: &Path) -> Result<std::fs::File, Failure> {
    use super::pipe_windows;

    let pipe = pipe_windows::open(pipe).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Failure::NotRunning,
        io::ErrorKind::PermissionDenied => Failure::Refused(NO_PERMISSION),
        _ => error.into(),
    })?;
    if !pipe_windows::owned_by_current_user(&pipe)? {
        return Err(Failure::Refused(
            "外部 CLI 的管道不属于当前用户，已拒绝连接",
        ));
    }
    Ok(pipe)
}

#[cfg(not(any(unix, windows)))]
fn connect(_: &Path) -> Result<std::fs::File, Failure> {
    Err(Failure::Refused("此系统暂不支持外部 CLI"))
}

/// Send the request and print the replies until the last one.
fn talk<S>(stream: S, request: Request, console: &mut Console) -> Result<i32, Failure>
where
    S: Copy + io::Read + Write,
{
    let mut writer = stream;
    write_json(
        &mut writer,
        &Envelope {
            version: PROTOCOL_VERSION,
            request,
        },
    )?;
    let mut reader = BufReader::new(stream);
    // The progress line on screen, to be blanked before anything else.
    let mut progress = ProgressLine::default();
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
                if !matches!(reply, Reply::Progress(_)) {
                    progress.clear(console.stderr);
                }
                match reply {
                    Reply::Progress(counters) => {
                        if console.stderr_is_terminal {
                            progress.show(console.stderr, &progress_line(counters));
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

/// A line rewritten in place with a carriage return. Shorter text is
/// padded with spaces rather than cleared with an escape sequence, which
/// the older Windows console does not understand.
#[derive(Default)]
struct ProgressLine {
    /// Columns the line on screen takes up.
    width: usize,
}

impl ProgressLine {
    fn show(&mut self, out: &mut dyn Write, text: &str) {
        let width = text.width();
        let padding = self.width.saturating_sub(width);
        let _ = write!(out, "\r{text}{}", " ".repeat(padding));
        if padding > 0 {
            // Back to the end of the text.
            let _ = write!(out, "\r{text}");
        }
        let _ = out.flush();
        self.width = width;
    }

    fn clear(&mut self, out: &mut dyn Write) {
        if self.width > 0 {
            let _ = write!(out, "\r{}\r", " ".repeat(self.width));
            let _ = out.flush();
            self.width = 0;
        }
    }
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
