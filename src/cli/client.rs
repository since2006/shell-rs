//! The command's end of the CLI socket: send one request to the running
//! app and print what comes back.

use std::{
    io::{self, BufReader, Write},
    path::Path,
    sync::mpsc,
    time::Duration,
};

use unicode_width::UnicodeWidthStr as _;

use super::link::OpenLink;
use super::protocol::{
    AuthChoice, CredentialDeleted, CredentialDetails, CredentialKindChoice, Envelope, ErrorCode,
    FrameKind, HostDeleted, HostDetails, HostInfo, PROTOCOL_VERSION, ProxyChoice, Reply, Request,
    RouteDetails, TransferCounters, TransferSummary, parse_json, read_frame, write_json,
};
use crate::host::HostOs;

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
    /// `exec --json`: the output gathered into one ASCII JSON object, and
    /// errors in JSON as well.
    pub exec_json: bool,
    /// Progress lines only make sense on a terminal.
    pub stderr_is_terminal: bool,
}

impl Console<'_> {
    pub fn error(&mut self, code: ErrorCode, message: &str) -> i32 {
        if self.exec_json {
            let error = serde_json::json!({
                "error": { "code": code.as_str(), "message": message }
            });
            let _ = writeln!(self.stdout, "{}", ascii_json(&error));
        } else {
            let _ = writeln!(self.stderr, "shellrs: [{}] {message}", code.as_str());
        }
        FAILURE_EXIT
    }
}

/// What `exec --json` prints once the command ends. Output that is not
/// UTF-8 has U+FFFD in place of what is not.
#[derive(serde::Serialize)]
struct ExecResult<'a> {
    exit_code: i32,
    stdout: std::borrow::Cow<'a, str>,
    stderr: std::borrow::Cow<'a, str>,
}

/// `value` as JSON in ASCII alone: every other character escaped, which
/// only ever happens inside strings. PowerShell and the Windows console
/// read it right whatever code page they assume.
pub(super) fn ascii_json(value: &impl serde::Serialize) -> String {
    let json = serde_json::to_string(value).unwrap_or_default();
    let mut ascii = String::with_capacity(json.len());
    for character in json.chars() {
        if character.is_ascii() {
            ascii.push(character);
        } else {
            for unit in character.encode_utf16(&mut [0; 2]) {
                ascii.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    ascii
}

/// What the request did, for the words its answer is printed with.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Asked {
    Show,
    Create,
    Update,
    Sync,
    Other,
}

impl Asked {
    fn of(request: &Request) -> Self {
        match request {
            Request::ShowHost { .. } | Request::ShowCredential { .. } => Asked::Show,
            Request::CreateHost { .. } | Request::CreateCredential { .. } => Asked::Create,
            Request::UpdateHost { .. } | Request::UpdateCredential { .. } => Asked::Update,
            Request::Sync { .. } => Asked::Sync,
            _ => Asked::Other,
        }
    }
}

/// What an app from before a request was added says about it: it cannot
/// read the request, and serde names what it did not know.
const OLDER_APP: &str =
    "正在运行的 ShellRS 不认识这个命令：请把 ShellRS 升级到最新版本，然后重新启动它";

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

/// How long a running app gets to say it heard that it should come
/// forward. One too busy to answer is running all the same.
const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Ask the ShellRS listening at `endpoint` to bring its window forward,
/// and to open `open` there; whether there is one. `false` means nothing is
/// running there and the caller is free to start.
///
/// Whatever takes the connection counts as running, whether or not it
/// understands the request: an older ShellRS answers with an error, and a
/// second copy on the same data would be worse than a window left behind.
pub fn activate_running_app(endpoint: &Path, open: Option<OpenLink>) -> bool {
    let Ok(stream) = connect(endpoint) else {
        return false;
    };
    // On a thread, because a pipe cannot be read with a timeout and an app
    // that hangs must not hang the one being opened.
    let (done, heard) = mpsc::channel();
    let asked = std::thread::Builder::new()
        .name("shellrs-activate".into())
        .spawn(move || {
            let _ = done.send(ask_to_come_forward(&stream, open));
        });
    if asked.is_ok() {
        let _ = heard.recv_timeout(ACTIVATION_TIMEOUT);
    }
    true
}

/// Send the request and wait for the answer, whatever it says: by then the
/// app has the request.
fn ask_to_come_forward<S>(stream: S, open: Option<OpenLink>) -> io::Result<()>
where
    S: Copy + io::Read + Write,
{
    let mut writer = stream;
    write_json(
        &mut writer,
        &Envelope {
            version: PROTOCOL_VERSION,
            request: Request::Activate { open },
        },
    )?;
    read_frame(&mut BufReader::new(stream)).map(|_| ())
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
    let asked = Asked::of(&request);
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
    // What `exec --json` holds until the command ends.
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    loop {
        let Some((kind, payload)) = read_frame(&mut reader)? else {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        };
        match kind {
            FrameKind::Stdout if console.exec_json => stdout.extend_from_slice(&payload),
            FrameKind::Stderr if console.exec_json => stderr.extend_from_slice(&payload),
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
                    Reply::Hosts { hosts } => {
                        print_hosts(&hosts, console)?;
                        return Ok(0);
                    }
                    Reply::TransferDone(summary) => {
                        return Ok(print_summary(&summary, asked == Asked::Sync, console)?);
                    }
                    Reply::Host(host) => {
                        print_host(&host, asked, console)?;
                        return Ok(0);
                    }
                    Reply::HostDeleted(deleted) => {
                        print_host_deleted(&deleted, console)?;
                        return Ok(0);
                    }
                    Reply::Credentials { credentials } => {
                        print_credentials(&credentials, console)?;
                        return Ok(0);
                    }
                    Reply::Credential(credential) => {
                        print_credential(&credential, asked, console)?;
                        return Ok(0);
                    }
                    Reply::CredentialDeleted(deleted) => {
                        print_credential_deleted(&deleted, console)?;
                        return Ok(0);
                    }
                    Reply::Exit { code } if console.exec_json => {
                        let result = ExecResult {
                            exit_code: code,
                            stdout: String::from_utf8_lossy(&stdout),
                            stderr: String::from_utf8_lossy(&stderr),
                        };
                        writeln!(console.stdout, "{}", ascii_json(&result))?;
                        return Ok(code);
                    }
                    Reply::Exit { code } => return Ok(code),
                    Reply::Activated => return Ok(0),
                    Reply::Error {
                        code: ErrorCode::BadRequest,
                        message,
                    } if message.contains("unknown variant") => {
                        return Ok(console.error(ErrorCode::VersionMismatch, OLDER_APP));
                    }
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

fn print_json(value: &impl serde::Serialize, console: &mut Console) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *console.stdout, value).map_err(io::Error::other)?;
    writeln!(console.stdout)
}

/// Rows lined up in columns, wide characters counted as two.
fn print_table<const N: usize>(
    header: [&str; N],
    rows: &[[String; N]],
    console: &mut Console,
) -> io::Result<()> {
    let header = header.map(String::from);
    let widths: Vec<usize> = (0..N)
        .map(|column| {
            std::iter::once(&header)
                .chain(rows)
                .map(|row| row[column].width())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in std::iter::once(&header).chain(rows) {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell}{}", " ".repeat(width - cell.width())))
            .collect();
        writeln!(console.stdout, "{}", line.join("  ").trim_end())?;
    }
    Ok(())
}

/// Labelled lines, the labels lined up.
fn print_fields(fields: &[(&str, String)], console: &mut Console) -> io::Result<()> {
    let width = fields
        .iter()
        .map(|(label, _)| label.width())
        .max()
        .unwrap_or(0);
    for (label, value) in fields {
        let padding = " ".repeat(width - label.width());
        writeln!(console.stdout, "{label}{padding}  {value}")?;
    }
    Ok(())
}

fn saved_word(saved: Option<bool>) -> &'static str {
    match saved {
        Some(true) => "（已保存）",
        Some(false) => "（未保存）",
        None => "",
    }
}

fn print_host(host: &HostDetails, asked: Asked, console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(host, console);
    }
    match asked {
        Asked::Create => {
            return writeln!(
                console.stdout,
                "已创建主机「{}」，ID {}",
                host.name, host.id
            );
        }
        Asked::Update => {
            return writeln!(
                console.stdout,
                "已保存主机「{}」（ID {}）",
                host.name, host.id
            );
        }
        _ => {}
    }
    let auth = match host.auth {
        AuthChoice::Password => format!("密码{}", saved_word(host.password_saved)),
        AuthChoice::Credential => {
            format!("凭据 {}", host.credential.as_deref().unwrap_or_default())
        }
        AuthChoice::NoPassword => "无密码".to_string(),
    };
    let route = match &host.route {
        RouteDetails::Direct => "直接连接".to_string(),
        RouteDetails::Jump { hosts } => format!(
            "SSH 跳板 {}",
            hosts
                .iter()
                .map(|hop| hop.as_deref().unwrap_or("（已删除的主机）"))
                .collect::<Vec<_>>()
                .join(" → ")
        ),
        RouteDetails::Proxy {
            kind,
            host,
            port,
            user,
            password_saved,
        } => format!(
            "{} {}{host}:{port}{}",
            match kind {
                ProxyChoice::Http => "HTTP 代理",
                ProxyChoice::Socks5 => "SOCKS5 代理",
            },
            user.as_deref()
                .map(|user| format!("{user}@"))
                .unwrap_or_default(),
            password_saved
                .map(|saved| if saved {
                    "（密码已保存）"
                } else {
                    "（密码未保存）"
                })
                .unwrap_or_default()
        ),
    };
    let mut fields = vec![
        ("ID", host.id.clone()),
        ("名称", host.name.clone()),
        (
            "分组",
            if host.temporary {
                "（未保存）".to_string()
            } else {
                host.group.clone().unwrap_or_default()
            },
        ),
        ("地址", format!("{}@{}:{}", host.user, host.host, host.port)),
        ("认证", auth),
        ("连接方式", route),
    ];
    if let Some(os) = host.os.as_deref().and_then(HostOs::from_stored) {
        fields.push(("系统", os.label().to_string()));
    }
    if !host.notes.is_empty() {
        fields.push(("备注", host.notes.clone()));
    }
    print_fields(&fields, console)
}

fn print_host_deleted(deleted: &HostDeleted, console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(deleted, console);
    }
    let mut line = format!("已删除主机「{}」", deleted.host.name);
    if deleted.forwards > 0 {
        line.push_str(&format!("，连同它的 {} 条端口转发规则", deleted.forwards));
    }
    if deleted.jump_users > 0 {
        line.push_str(&format!(
            "；{} 台主机经它跳转，现在那一跳是「已删除的主机」",
            deleted.jump_users
        ));
    }
    writeln!(console.stdout, "{line}")
}

fn kind_label(kind: CredentialKindChoice) -> &'static str {
    match kind {
        CredentialKindChoice::Password => "密码",
        CredentialKindChoice::Key => "密钥",
        CredentialKindChoice::Agent => "SSH Agent",
    }
}

fn print_credentials(credentials: &[CredentialDetails], console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(&credentials, console);
    }
    if credentials.is_empty() {
        return writeln!(console.stderr, "没有匹配的凭据");
    }
    let rows: Vec<[String; 5]> = credentials
        .iter()
        .map(|credential| {
            [
                credential.id.clone(),
                credential.name.clone(),
                kind_label(credential.kind).to_string(),
                credential.user.clone(),
                credential.hosts.len().to_string(),
            ]
        })
        .collect();
    print_table(["ID", "名称", "类型", "用户名", "主机数"], &rows, console)
}

fn print_credential(
    credential: &CredentialDetails,
    asked: Asked,
    console: &mut Console,
) -> io::Result<()> {
    if console.json {
        return print_json(credential, console);
    }
    match asked {
        Asked::Create => {
            return writeln!(
                console.stdout,
                "已创建凭据「{}」，ID {}",
                credential.name, credential.id
            );
        }
        Asked::Update => {
            return writeln!(
                console.stdout,
                "已保存凭据「{}」（ID {}）",
                credential.name, credential.id
            );
        }
        _ => {}
    }
    let mut fields = vec![
        ("ID", credential.id.clone()),
        ("名称", credential.name.clone()),
        (
            "类型",
            format!(
                "{}{}",
                kind_label(credential.kind),
                saved_word(credential.password_saved)
            ),
        ),
        ("用户名", credential.user.clone()),
    ];
    if credential.kind == CredentialKindChoice::Key {
        let key = match (&credential.key_path, credential.kept) {
            (Some(path), _) => path.clone(),
            (None, _) => "由 ShellRS 保存".to_string(),
        };
        fields.push(("私钥", key));
        if let Some(saved) = credential.passphrase_saved {
            fields.push(("口令", if saved { "已保存" } else { "未保存" }.to_string()));
        }
    }
    fields.push(("主机", credential.hosts.join(", ")));
    print_fields(&fields, console)
}

fn print_credential_deleted(deleted: &CredentialDeleted, console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(deleted, console);
    }
    let mut line = format!("已删除凭据「{}」", deleted.credential.name);
    if !deleted.released.is_empty() {
        let names: Vec<&str> = deleted
            .released
            .iter()
            .map(|host| host.name.as_str())
            .collect();
        line.push_str(&format!(
            "；{} 台主机改为自己登录：{}",
            names.len(),
            names.join("、")
        ));
    }
    writeln!(console.stdout, "{line}")
}

fn print_hosts(hosts: &[HostInfo], console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(&hosts, console);
    }
    if hosts.is_empty() {
        return writeln!(console.stderr, "没有匹配的主机");
    }
    let rows: Vec<[String; 5]> = hosts
        .iter()
        .map(|host| {
            [
                host.id.clone(),
                host.name.clone(),
                // 临时连接 and 外部连接 alike: an agent needs no telling
                // them apart, only that the host is not saved.
                if host.temporary {
                    "（未保存）".to_string()
                } else {
                    host.group.clone().unwrap_or_default()
                },
                host.address(),
                host.os
                    .as_deref()
                    .and_then(HostOs::from_stored)
                    .map(|os| os.label().to_string())
                    .unwrap_or_default(),
            ]
        })
        .collect();
    print_table(["ID", "名称", "分组", "地址", "系统"], &rows, console)
}

/// A sync's skipped items are the ones that had not changed.
fn print_summary(summary: &TransferSummary, sync: bool, console: &mut Console) -> io::Result<i32> {
    if console.json {
        print_json(summary, console)?;
    } else {
        let mut line = format!(
            "已传输 {} 个文件，共 {}",
            summary.files,
            format_bytes(summary.bytes)
        );
        if summary.skipped > 0 {
            let word = if sync { "未变" } else { "跳过" };
            line.push_str(&format!("，{word} {} 个", summary.skipped));
        }
        if summary.deleted > 0 {
            line.push_str(&format!("，删除 {} 个", summary.deleted));
        }
        if summary.failed > 0 {
            line.push_str(&format!("，失败 {} 个", summary.failed));
        }
        writeln!(console.stdout, "{line}")?;
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
