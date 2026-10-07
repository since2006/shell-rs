//! The command's end of the CLI socket: send one request to the running
//! app and print what comes back.

use std::{
    io::{self, BufReader, Write},
    path::Path,
    sync::mpsc,
    time::Duration,
};

use gpui_kit::SharedString;
use unicode_width::UnicodeWidthStr as _;

use super::link::OpenLink;
use super::protocol::{
    AuthChoice, CredentialDeleted, CredentialDetails, CredentialKindChoice, Envelope, ErrorCode,
    FrameKind, HostDeleted, HostDetails, HostInfo, PROTOCOL_VERSION, ProxyChoice, Reply, Request,
    RouteDetails, TransferCounters, TransferSummary, parse_json, read_frame, write_json,
};
use crate::host::HostOs;
use crate::i18n::{t, tn};

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

/// Send `request` to the app listening at `endpoint` and print the answer;
/// the exit code.
pub fn run(endpoint: &Path, request: Request, console: &mut Console) -> i32 {
    let result = connect(endpoint).and_then(|stream| talk(&stream, request, console));
    match result {
        Ok(code) => code,
        Err(Failure::NotRunning) => {
            console.error(ErrorCode::NotRunning, &t!("cli.client.not_running"))
        }
        Err(Failure::Refused(message)) => console.error(ErrorCode::ConnectFailed, &message),
        Err(Failure::Broken(error)) => console.error(
            ErrorCode::ConnectFailed,
            &t!("cli.client.broken", error = error),
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
    Refused(SharedString),
    Broken(io::Error),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Failure::Broken(error)
    }
}

#[cfg(unix)]
fn connect(socket: &Path) -> Result<std::os::unix::net::UnixStream, Failure> {
    std::os::unix::net::UnixStream::connect(socket).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Failure::NotRunning,
        io::ErrorKind::PermissionDenied => Failure::Refused(no_permission()),
        _ => error.into(),
    })
}

/// What a sandbox that forbids local connections looks like.
fn no_permission() -> SharedString {
    t!("cli.client.no_permission")
}

/// Open the app's pipe, and make sure it is the app's before saying
/// anything: the name is the machine's, so someone else could hold it.
#[cfg(windows)]
fn connect(pipe: &Path) -> Result<std::fs::File, Failure> {
    use super::pipe_windows;

    let pipe = pipe_windows::open(pipe).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => Failure::NotRunning,
        io::ErrorKind::PermissionDenied => Failure::Refused(no_permission()),
        _ => error.into(),
    })?;
    if !pipe_windows::owned_by_current_user(&pipe)? {
        return Err(Failure::Refused(t!("cli.client.foreign_pipe")));
    }
    Ok(pipe)
}

#[cfg(not(any(unix, windows)))]
fn connect(_: &Path) -> Result<std::fs::File, Failure> {
    Err(Failure::Refused(t!("cli.unsupported")))
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
                        // What an app from before the request was added
                        // says: serde names what it did not know.
                        return Ok(
                            console.error(ErrorCode::VersionMismatch, &t!("cli.client.older_app"))
                        );
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
    header: [SharedString; N],
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
fn print_fields(fields: &[(SharedString, String)], console: &mut Console) -> io::Result<()> {
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

/// `label` saying whether its password is saved, when that is known.
fn with_saved(label: SharedString, saved: Option<bool>) -> String {
    let text = match saved {
        Some(true) => t!("cli.show.saved", label = label),
        Some(false) => t!("cli.show.not_saved", label = label),
        None => label,
    };
    text.to_string()
}

fn print_host(host: &HostDetails, asked: Asked, console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(host, console);
    }
    match asked {
        Asked::Create => {
            return writeln!(
                console.stdout,
                "{}",
                t!("cli.host.created", name = host.name, id = host.id)
            );
        }
        Asked::Update => {
            return writeln!(
                console.stdout,
                "{}",
                t!("cli.host.saved", name = host.name, id = host.id)
            );
        }
        _ => {}
    }
    let auth = match host.auth {
        AuthChoice::Password => with_saved(t!("cli.auth.password"), host.password_saved),
        AuthChoice::Credential => t!(
            "cli.auth.credential",
            id = host.credential.as_deref().unwrap_or_default()
        )
        .to_string(),
        AuthChoice::NoPassword => t!("cli.auth.no_password").to_string(),
    };
    let route = match &host.route {
        RouteDetails::Direct => t!("cli.route.direct").to_string(),
        RouteDetails::Jump { hosts } => {
            let deleted = t!("cli.route.deleted_host");
            let hosts = hosts
                .iter()
                .map(|hop| hop.as_deref().unwrap_or(deleted.as_str()))
                .collect::<Vec<_>>()
                .join(" → ");
            t!("cli.route.jump", hosts = hosts).to_string()
        }
        RouteDetails::Proxy {
            kind,
            host,
            port,
            user,
            password_saved,
        } => {
            let user = user
                .as_deref()
                .map(|user| format!("{user}@"))
                .unwrap_or_default();
            let address = format!("{user}{host}:{port}");
            let proxy = match kind {
                ProxyChoice::Http => t!("cli.route.http", address = address),
                ProxyChoice::Socks5 => t!("cli.route.socks5", address = address),
            };
            let proxy = match password_saved {
                Some(true) => t!("cli.route.password_saved", proxy = proxy),
                Some(false) => t!("cli.route.password_not_saved", proxy = proxy),
                None => proxy,
            };
            proxy.to_string()
        }
    };
    let mut fields = vec![
        (t!("cli.field.id"), host.id.clone()),
        (t!("cli.field.name"), host.name.clone()),
        (
            t!("cli.field.group"),
            if host.temporary {
                t!("cli.group.not_saved").to_string()
            } else {
                host.group.clone().unwrap_or_default()
            },
        ),
        (
            t!("cli.field.address"),
            format!("{}@{}:{}", host.user, host.host, host.port),
        ),
        (t!("cli.field.auth"), auth),
        (t!("cli.field.route"), route),
    ];
    if let Some(os) = host.os.as_deref().and_then(HostOs::from_stored) {
        fields.push((t!("cli.field.os"), os.label().to_string()));
    }
    if !host.notes.is_empty() {
        fields.push((t!("cli.field.notes"), host.notes.clone()));
    }
    print_fields(&fields, console)
}

fn print_host_deleted(deleted: &HostDeleted, console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(deleted, console);
    }
    let name = &deleted.host.name;
    let mut line = if deleted.forwards > 0 {
        tn!(
            "cli.host.deleted_with_forwards",
            deleted.forwards,
            name = name
        )
    } else {
        t!("cli.host.deleted", name = name)
    };
    if deleted.jump_users > 0 {
        line = tn!(
            "cli.host.deleted_jump_users",
            deleted.jump_users,
            deleted = line
        );
    }
    writeln!(console.stdout, "{line}")
}

fn kind_label(kind: CredentialKindChoice) -> SharedString {
    match kind {
        CredentialKindChoice::Password => t!("cli.kind.password"),
        CredentialKindChoice::Key => t!("cli.kind.key"),
        CredentialKindChoice::Agent => t!("cli.kind.agent"),
    }
}

fn print_credentials(credentials: &[CredentialDetails], console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(&credentials, console);
    }
    if credentials.is_empty() {
        return writeln!(console.stderr, "{}", t!("cli.credentials.none"));
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
    print_table(
        [
            t!("cli.column.id"),
            t!("cli.column.name"),
            t!("cli.column.kind"),
            t!("cli.column.user"),
            t!("cli.column.hosts"),
        ],
        &rows,
        console,
    )
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
                "{}",
                t!(
                    "cli.credential.created",
                    name = credential.name,
                    id = credential.id
                )
            );
        }
        Asked::Update => {
            return writeln!(
                console.stdout,
                "{}",
                t!(
                    "cli.credential.saved",
                    name = credential.name,
                    id = credential.id
                )
            );
        }
        _ => {}
    }
    let mut fields = vec![
        (t!("cli.field.id"), credential.id.clone()),
        (t!("cli.field.name"), credential.name.clone()),
        (
            t!("cli.field.kind"),
            with_saved(kind_label(credential.kind), credential.password_saved),
        ),
        (t!("cli.field.user"), credential.user.clone()),
    ];
    if credential.kind == CredentialKindChoice::Key {
        let key = match (&credential.key_path, credential.kept) {
            (Some(path), _) => path.clone(),
            (None, _) => t!("cli.credential.kept_key").to_string(),
        };
        fields.push((t!("cli.field.private_key"), key));
        if let Some(saved) = credential.passphrase_saved {
            let saved = if saved {
                t!("cli.credential.passphrase_saved")
            } else {
                t!("cli.credential.passphrase_not_saved")
            };
            fields.push((t!("cli.field.passphrase"), saved.to_string()));
        }
    }
    fields.push((t!("cli.field.hosts"), credential.hosts.join(", ")));
    print_fields(&fields, console)
}

fn print_credential_deleted(deleted: &CredentialDeleted, console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(deleted, console);
    }
    let mut line = t!("cli.credential.deleted", name = deleted.credential.name);
    if !deleted.released.is_empty() {
        let names: Vec<&str> = deleted
            .released
            .iter()
            .map(|host| host.name.as_str())
            .collect();
        let separator = t!("common.list.separator");
        line = tn!(
            "cli.credential.deleted_released",
            names.len(),
            deleted = line,
            hosts = names.join(separator.as_str())
        );
    }
    writeln!(console.stdout, "{line}")
}

fn print_hosts(hosts: &[HostInfo], console: &mut Console) -> io::Result<()> {
    if console.json {
        return print_json(&hosts, console);
    }
    if hosts.is_empty() {
        return writeln!(console.stderr, "{}", t!("cli.hosts.none"));
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
                    t!("cli.group.not_saved").to_string()
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
    print_table(
        [
            t!("cli.column.id"),
            t!("cli.column.name"),
            t!("cli.column.group"),
            t!("cli.column.address"),
            t!("cli.column.os"),
        ],
        &rows,
        console,
    )
}

/// A sync's skipped items are the ones that had not changed.
fn print_summary(summary: &TransferSummary, sync: bool, console: &mut Console) -> io::Result<i32> {
    if console.json {
        print_json(summary, console)?;
    } else {
        let mut line = tn!(
            "cli.summary.transferred",
            summary.files,
            size = format_bytes(summary.bytes)
        );
        if summary.skipped > 0 {
            line = if sync {
                t!(
                    "cli.summary.unchanged",
                    summary = line,
                    count = summary.skipped
                )
            } else {
                t!(
                    "cli.summary.skipped",
                    summary = line,
                    count = summary.skipped
                )
            };
        }
        if summary.deleted > 0 {
            line = t!(
                "cli.summary.deleted",
                summary = line,
                count = summary.deleted
            );
        }
        if summary.failed > 0 {
            line = t!("cli.summary.failed", summary = line, count = summary.failed);
        }
        writeln!(console.stdout, "{line}")?;
        for failure in &summary.failures {
            writeln!(console.stderr, "{failure}")?;
        }
    }
    Ok(if summary.failed > 0 { PARTIAL_EXIT } else { 0 })
}

fn progress_line(counters: TransferCounters) -> String {
    t!(
        "cli.progress",
        files = counters.files,
        total_files = counters.total_files,
        bytes = format_bytes(counters.bytes),
        total_bytes = format_bytes(counters.total_bytes)
    )
    .to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What `print` writes to stdout, as on a terminal.
    fn printed(print: impl FnOnce(&mut Console) -> io::Result<()>) -> String {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        print(&mut Console {
            stdout: &mut stdout,
            stderr: &mut stderr,
            json: false,
            exec_json: false,
            stderr_is_terminal: false,
        })
        .unwrap();
        String::from_utf8(stdout).unwrap()
    }

    fn host(name: &str) -> HostDetails {
        HostDetails {
            id: "Jwg5rHvXCxw89paM".into(),
            name: name.into(),
            group: None,
            host: "10.0.0.9".into(),
            port: 22,
            user: "root".into(),
            auth: AuthChoice::Password,
            credential: None,
            password_saved: None,
            route: RouteDetails::Direct,
            notes: String::new(),
            os: None,
            temporary: false,
        }
    }

    fn deleted_host(forwards: u64, jump_users: u64) -> String {
        let deleted = HostDeleted {
            host: host("web"),
            forwards,
            jump_users,
        };
        printed(|console| print_host_deleted(&deleted, console))
    }

    fn deleted_credential(released: &[&str]) -> String {
        let info = |name: &&str| HostInfo {
            id: String::new(),
            name: name.to_string(),
            group: None,
            user: "root".into(),
            host: "10.0.0.9".into(),
            port: 22,
            os: None,
            temporary: false,
        };
        let deleted = CredentialDeleted {
            credential: CredentialDetails {
                id: String::new(),
                name: "deploy".into(),
                kind: CredentialKindChoice::Password,
                user: "deploy".into(),
                key_path: None,
                kept: false,
                hosts: Vec::new(),
                password_saved: None,
                passphrase_saved: None,
            },
            released: released.iter().map(info).collect(),
        };
        printed(|console| print_credential_deleted(&deleted, console))
    }

    fn summary(files: u64, skipped: u64, sync: bool) -> String {
        let summary = TransferSummary {
            files,
            bytes: 2048,
            skipped,
            failed: 1,
            deleted: 0,
            failures: Vec::new(),
        };
        printed(|console| print_summary(&summary, sync, console).map(drop))
    }

    #[test]
    fn what_was_done_reads_as_one_line_in_either_language() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("zh-CN");
        assert_eq!(deleted_host(0, 0), "已删除主机「web」\n");
        assert_eq!(
            deleted_host(2, 3),
            "已删除主机「web」，连同它的 2 条端口转发规则；\
             3 台主机经它跳转，现在那一跳是「已删除的主机」\n"
        );
        assert_eq!(
            deleted_credential(&["web", "db"]),
            "已删除凭据「deploy」；2 台主机改为自己登录：web、db\n"
        );
        assert_eq!(
            summary(2, 40, true),
            "已传输 2 个文件，共 2.0 KB，未变 40 个，失败 1 个\n"
        );

        crate::i18n::set_locale("en");
        assert_eq!(deleted_host(0, 0), "Deleted host \"web\"\n");
        assert_eq!(
            deleted_host(1, 1),
            "Deleted host \"web\" and its port forwarding rule; \
             1 host jumped through it and now has a deleted jump host\n"
        );
        assert_eq!(
            deleted_credential(&["web", "db"]),
            "Deleted credential \"deploy\"; 2 hosts now log in on their own: web, db\n"
        );
        assert_eq!(
            summary(1, 4, false),
            "Transferred 1 file (2.0 KB), 4 skipped, 1 failed\n"
        );
    }

    #[test]
    fn an_english_show_lines_up_its_labels() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        let mut shown = host("web");
        shown.password_saved = Some(true);
        let shown = printed(|console| print_host(&shown, Asked::Show, console));
        assert!(
            shown.contains("\nAuthentication  Password (saved)\n"),
            "{shown}"
        );
        assert!(shown.contains("\nConnection      Direct\n"), "{shown}");
        assert!(
            shown.starts_with("ID              Jwg5rHvXCxw89paM\n"),
            "{shown}"
        );
    }
}
