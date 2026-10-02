//! The command and the app's server talking over a real socket, with a
//! fake backend behind the server.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clap::Parser as _;

use super::client::{self, Console};
use super::protocol::{CliError, ErrorCode, HostInfo, Request, TransferCounters, TransferSummary};
use super::server::{CliBackend, CliServer, CliTarget};
use super::{Cli, Command};
use super::{ConsoleText, normalize_command};
use crate::app::cli_endpoint;
use crate::host::{AuthKind, GroupId, Host, HostDraft, HostId, HostLogin};
use crate::ssh::ExecStream;

/// Records what it was asked and answers from a script.
#[derive(Default)]
struct FakeBackend {
    calls: Mutex<Vec<String>>,
}

impl CliBackend for FakeBackend {
    fn exec(
        &self,
        target: &CliTarget,
        command: &str,
        output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    ) -> Result<i32, CliError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("exec {} {command}", target.host().name));
        if command == "unreachable" {
            return Err(CliError::new(ErrorCode::HostKeyUnknown, "尚未信任"));
        }
        output(ExecStream::Stdout, b"out\n").unwrap();
        output(ExecStream::Stderr, &[0xff, b'\n']).unwrap();
        Ok(3)
    }

    fn upload(
        &self,
        target: &CliTarget,
        source: &Path,
        destination: &str,
        progress: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        self.calls.lock().unwrap().push(format!(
            "upload {} {} {destination}",
            target.host().name,
            source.display()
        ));
        progress(TransferCounters {
            files: 1,
            total_files: 2,
            bytes: 10,
            total_bytes: 20,
        })
        .unwrap();
        Ok(TransferSummary {
            files: 1,
            bytes: 20,
            skipped: 0,
            failed: 1,
            failures: vec!["无法上传：permission denied".into()],
        })
    }

    fn download(
        &self,
        _: &CliTarget,
        _: &str,
        _: &Path,
        _: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        Ok(TransferSummary::default())
    }
}

fn host(id: u64, name: &str, host: &str) -> Host {
    Host::new(
        HostId(id),
        HostDraft::new(name, host, 22, "root", AuthKind::Password, Some(GroupId(1))),
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    server: CliServer,
    backend: Arc<FakeBackend>,
    web: Host,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let socket = cli_endpoint(dir.path());
    let backend = Arc::new(FakeBackend::default());
    let server = CliServer::start(socket.clone(), backend.clone()).unwrap();
    let web = host(1, "web-01", "10.0.1.12");
    let db = host(2, "db-01", "10.0.2.5");
    server.set_targets(vec![
        CliTarget::new(&web, HostLogin::of(&web, None), Some("生产".into())),
        CliTarget::new(&db, HostLogin::of(&db, None), Some("生产/数据库".into())),
    ]);
    server.set_enabled(true);
    Fixture {
        _dir: dir,
        socket,
        server,
        backend,
        web,
    }
}

/// Run one request the way the command does; its exit code, stdout and
/// stderr.
fn run(socket: &Path, request: Request, json: bool) -> (i32, String, Vec<u8>) {
    run_on(socket, request, json, false)
}

fn run_on(
    socket: &Path,
    request: Request,
    json: bool,
    stderr_is_terminal: bool,
) -> (i32, String, Vec<u8>) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = client::run(
        socket,
        request,
        &mut Console {
            stdout: &mut stdout,
            stderr: &mut stderr,
            json,
            stderr_is_terminal,
        },
    );
    (code, String::from_utf8(stdout).unwrap(), stderr)
}

/// A local path the app accepts: absolute on this system.
fn local(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

fn id(host: &Host) -> String {
    host.public_id.to_string()
}

#[test]
fn listing_filters_by_name_host_group_or_id() {
    let fixture = fixture();
    let list = |query: Option<&str>| {
        let (code, stdout, _) = run(
            &fixture.socket,
            Request::List {
                query: query.map(String::from),
            },
            true,
        );
        assert_eq!(code, 0);
        let hosts: Vec<HostInfo> = serde_json::from_str(&stdout).unwrap();
        hosts.into_iter().map(|host| host.name).collect::<Vec<_>>()
    };
    assert_eq!(list(None), ["web-01", "db-01"]);
    assert_eq!(list(Some("WEB")), ["web-01"]);
    assert_eq!(list(Some("10.0.2")), ["db-01"]);
    assert_eq!(list(Some("数据库")), ["db-01"]);
    assert_eq!(list(Some(&id(&fixture.web))), ["web-01"]);
    assert!(list(Some("nothing")).is_empty());

    let (_, stdout, _) = run(&fixture.socket, Request::List { query: None }, true);
    let hosts: Vec<HostInfo> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(hosts[0].id, id(&fixture.web));
    assert_eq!(hosts[0].group.as_deref(), Some("生产"));
    assert_eq!(hosts[0].address(), "root@10.0.1.12:22");
}

#[test]
fn a_terminal_gets_a_table_lined_up_for_chinese() {
    let fixture = fixture();
    let (code, stdout, _) = run(&fixture.socket, Request::List { query: None }, false);
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(lines[0].starts_with("ID"));
    assert!(lines[1].starts_with(&id(&fixture.web)));
    // 分组 starts in the same column on every line, counting CJK as two.
    let column = |line: &str, text: &str| {
        use unicode_width::UnicodeWidthStr as _;
        line[..line.find(text).unwrap()].width()
    };
    assert_eq!(column(lines[1], "生产"), column(lines[0], "分组"));
    assert_eq!(column(lines[2], "生产/数据库"), column(lines[0], "分组"));
}

#[test]
fn exec_passes_output_through_as_bytes_and_exits_with_the_remote_code() {
    let fixture = fixture();
    let (code, stdout, stderr) = run(
        &fixture.socket,
        Request::Exec {
            host: id(&fixture.web),
            command: "uname -a".into(),
        },
        false,
    );
    assert_eq!(code, 3);
    assert_eq!(stdout, "out\n");
    assert_eq!(stderr, [0xff, b'\n']);
    assert_eq!(
        fixture.backend.calls.lock().unwrap().as_slice(),
        ["exec web-01 uname -a"]
    );
}

#[test]
fn failures_carry_their_code_and_exit_255() {
    let fixture = fixture();
    let (code, _, stderr) = run(
        &fixture.socket,
        Request::Exec {
            host: id(&fixture.web),
            command: "unreachable".into(),
        },
        false,
    );
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [host_key_unknown]")
    );

    let (code, _, stderr) = run(
        &fixture.socket,
        Request::Exec {
            host: "nobody".into(),
            command: "true".into(),
        },
        false,
    );
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("[host_not_found]")
    );

    // A path relative to the command is meaningless to the app.
    let (code, _, stderr) = run(
        &fixture.socket,
        Request::Upload {
            host: id(&fixture.web),
            source: "relative/file".into(),
            destination: "/tmp".into(),
        },
        false,
    );
    assert_eq!(code, 255);
    assert!(String::from_utf8(stderr).unwrap().contains("[bad_request]"));
    // Only the exec that got as far as the backend.
    assert_eq!(
        fixture.backend.calls.lock().unwrap().as_slice(),
        ["exec web-01 unreachable"]
    );
}

#[test]
fn with_the_switch_off_nothing_reaches_the_backend() {
    let fixture = fixture();
    fixture.server.set_enabled(false);
    let (code, stdout, stderr) = run(
        &fixture.socket,
        Request::Exec {
            host: id(&fixture.web),
            command: "true".into(),
        },
        false,
    );
    assert_eq!(code, 255);
    assert!(stdout.is_empty());
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(stderr.starts_with("shellrs: [not_enabled]"));
    assert!(stderr.contains("设置 → 外部 CLI"));
    assert!(fixture.backend.calls.lock().unwrap().is_empty());
}

#[test]
fn without_the_app_the_command_says_it_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let (code, _, stderr) = run(
        &cli_endpoint(dir.path()),
        Request::List { query: None },
        true,
    );
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [not_running]")
    );

    // Nor when a crashed app left its socket behind, with nobody on it.
    #[cfg(unix)]
    {
        let stale = dir.path().join("stale.sock");
        // No process may start meanwhile and keep the socket open.
        let _forks = crate::testing::no_forks();
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        let (code, _, stderr) = run(&stale, Request::List { query: None }, true);
        assert_eq!(code, 255);
        assert!(
            String::from_utf8(stderr)
                .unwrap()
                .starts_with("shellrs: [not_running]")
        );
    }
}

#[test]
fn opening_shellrs_again_brings_the_running_one_forward() {
    let fixture = fixture();
    // Whatever 启用外部 CLI says: this is not the external CLI.
    fixture.server.set_enabled(false);
    assert!(!fixture.server.take_activation());

    assert!(client::activate_running_app(&fixture.socket));
    assert!(fixture.server.take_activation());
    // Heard once, acted on once.
    assert!(!fixture.server.take_activation());
    assert!(fixture.backend.calls.lock().unwrap().is_empty());

    // The switch still guards everything else.
    let (code, _, stderr) = run(&fixture.socket, Request::List { query: None }, true);
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [not_enabled]")
    );
    assert!(!fixture.server.take_activation());
}

#[test]
fn with_nothing_running_shellrs_is_free_to_start() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!client::activate_running_app(&cli_endpoint(dir.path())));

    // Nor does a socket left behind by a crash count as a running app.
    #[cfg(unix)]
    {
        let stale = dir.path().join("stale.sock");
        // No process may start meanwhile and keep the socket open.
        let _forks = crate::testing::no_forks();
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        assert!(!client::activate_running_app(&stale));
    }
}

#[test]
fn a_transfer_reports_its_summary_and_exits_1_when_items_failed() {
    let fixture = fixture();
    let (code, stdout, _) = run(
        &fixture.socket,
        Request::Upload {
            host: id(&fixture.web),
            source: local("dist"),
            destination: "~/dist".into(),
        },
        true,
    );
    assert_eq!(code, 1);
    let summary: TransferSummary = serde_json::from_str(&stdout).unwrap();
    assert_eq!(summary.files, 1);
    assert_eq!(summary.failures, ["无法上传：permission denied"]);
    assert_eq!(
        fixture.backend.calls.lock().unwrap().as_slice(),
        [format!("upload web-01 {} ~/dist", local("dist").display())]
    );
}

#[test]
fn progress_is_rewritten_in_place_without_escape_sequences() {
    let fixture = fixture();
    let (code, _, stderr) = run_on(
        &fixture.socket,
        Request::Upload {
            host: id(&fixture.web),
            source: local("dist"),
            destination: "~/dist".into(),
        },
        false,
        true,
    );
    assert_eq!(code, 1);
    let stderr = String::from_utf8(stderr).unwrap();
    // Shown, then blanked with spaces before the failures are listed; the
    // old Windows console would print an escape sequence as it is.
    let line = "1/2 个文件，10 B / 20 B";
    let blank = " ".repeat(unicode_width::UnicodeWidthStr::width(line));
    assert_eq!(
        stderr,
        format!("\r{line}\r{blank}\r无法上传：permission denied\n")
    );
}

#[test]
fn the_socket_belongs_to_one_app_at_a_time_and_goes_with_it() {
    let fixture = fixture();
    let second = CliServer::start(fixture.socket.clone(), fixture.backend.clone());
    assert_eq!(
        second.err().map(|error| error.kind()),
        Some(io::ErrorKind::AddrInUse)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&fixture.socket)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let socket = fixture.socket.clone();
    drop(fixture.server);
    let (code, _, stderr) = run(&socket, Request::List { query: None }, true);
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [not_running]")
    );
    #[cfg(unix)]
    {
        assert!(!socket.exists());
        // A socket left behind by a crash is taken over.
        std::fs::write(&socket, "").unwrap();
    }
    let restarted = CliServer::start(socket.clone(), Arc::new(FakeBackend::default()));
    assert!(restarted.is_ok());
}

#[test]
fn exec_takes_its_command_as_one_argument_or_from_stdin() {
    let parse = |args: &[&str]| Cli::try_parse_from(args).map(|cli| cli.command);
    assert!(matches!(
        parse(&["shellrs", "exec", "ID", "df -h | head"]),
        Ok(Command::Exec { command: Some(command), stdin: false, .. }) if command == "df -h | head"
    ));
    assert!(matches!(
        parse(&["shellrs", "exec", "ID", "--stdin"]),
        Ok(Command::Exec {
            command: None,
            stdin: true,
            ..
        })
    ));
    // Neither, or both.
    assert!(parse(&["shellrs", "exec", "ID"]).is_err());
    assert!(parse(&["shellrs", "exec", "ID", "ls", "--stdin"]).is_err());
    // Split words are a mistake, not a longer command.
    assert!(parse(&["shellrs", "exec", "ID", "ls", "-la"]).is_err());
}

#[test]
fn a_command_reaches_the_remote_shell_with_unix_line_ends() {
    assert_eq!(
        normalize_command("\u{feff}cd /srv\r\nls -l\r\n"),
        "cd /srv\nls -l\n"
    );
    assert_eq!(normalize_command("printf 'a\\r\\n'"), "printf 'a\\r\\n'");
    assert_eq!(normalize_command("uname -a"), "uname -a");
}

/// What a Windows console gets for `bytes` written in the given pieces.
fn console_text(pieces: &[&[u8]]) -> String {
    let mut text = ConsoleText {
        inner: Vec::new(),
        lossy: true,
        pending: Vec::new(),
    };
    for piece in pieces {
        io::Write::write_all(&mut text, piece).unwrap();
    }
    text.finish().unwrap();
    String::from_utf8(text.inner).unwrap()
}

#[test]
fn a_windows_console_gets_text_even_from_bytes_that_are_not_utf8() {
    let word = "生产".as_bytes();
    // A character cut in two by the frames it came in.
    assert_eq!(console_text(&[&word[..2], &word[2..]]), "生产");
    // GBK for 中, and a stray byte.
    assert_eq!(
        console_text(&[b"a\xd6\xd0b", b"\xff"]),
        "a\u{fffd}\u{fffd}b\u{fffd}"
    );
    // Cut off for good at the end.
    assert_eq!(console_text(&[b"ok", &word[..1]]), "ok\u{fffd}");

    // Anywhere but a Windows console, bytes are passed on as they are.
    let mut raw = ConsoleText::new(Vec::new(), cfg!(not(windows)));
    io::Write::write_all(&mut raw, b"\xff\xd6").unwrap();
    raw.finish().unwrap();
    assert_eq!(raw.inner, b"\xff\xd6");
}
