//! The command and the app's server talking over a real socket, with a
//! fake backend behind the server.

use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clap::Parser as _;

use super::client::ascii_json;
use super::client::{self, Console};
use super::link::OpenLink;
use super::manage::CliChange;
use super::protocol::{
    AuthChoice, CliError, CredentialDetails, ErrorCode, HostDetails, HostFields, HostInfo, Reply,
    Request, RouteDetails, Secret, TransferCounters, TransferSummary,
};
use super::server::{CliBackend, CliServer, CliTarget, CliUse};
use super::{Cli, Command, CredentialsCommand, HostsCommand};
use super::{ConsoleText, ExecJson, exec_request, normalize_command};
use crate::app::cli_endpoint;
use crate::host::{
    AuthKind, CredentialDraft, CredentialKind, GroupDraft, Host, HostDraft, HostId, HostStore,
    SshLink,
};
use crate::secrets::SecretRef;
use crate::ssh::ExecStream;
use crate::terminal::{RunFailure, RunHandle, TerminalTransportCommand};

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
            deleted: 0,
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

    fn sync(
        &self,
        target: &CliTarget,
        source: &Path,
        destination: &str,
        delete: bool,
        _: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        self.calls.lock().unwrap().push(format!(
            "sync {} {} {destination} {delete}",
            target.host().name,
            source.display()
        ));
        Ok(TransferSummary {
            files: 2,
            bytes: 2048,
            skipped: 40,
            failed: 0,
            deleted: 3,
            failures: Vec::new(),
        })
    }

    /// Only a host's own password, of what the fixture has.
    fn is_saved(&self, secret: &SecretRef) -> bool {
        matches!(secret, SecretRef::Password { user, .. } if user == "root")
    }
}

/// web-01 in 生产 and db-01 in 生产/数据库.
fn store() -> HostStore {
    let mut store = HostStore::empty();
    let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
    let database = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
    let draft = |name: &str, address: &str, group| {
        HostDraft::new(name, address, 22, "root", AuthKind::Password, Some(group))
    };
    store.insert_unnotified(draft("web-01", "10.0.1.12", production));
    store.insert_unnotified(draft("db-01", "10.0.2.5", database));
    store
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
    let store = store();
    let web = store.hosts()[0].clone();
    server.set_hosts(&store);
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
            exec_json: false,
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
fn the_kinds_of_command_served_are_counted_and_nothing_else() {
    let fixture = fixture();
    run(&fixture.socket, Request::List { query: None }, true);
    run(
        &fixture.socket,
        Request::Exec {
            host: id(&fixture.web),
            command: "uname -a".into(),
        },
        false,
    );
    assert_eq!(fixture.server.take_usage(), [CliUse::Hosts, CliUse::Exec]);
    assert_eq!(fixture.server.take_usage(), []);
    // Refused, or ShellRS opened again: not a command served.
    fixture.server.set_enabled(false);
    run(&fixture.socket, Request::List { query: None }, true);
    assert!(client::activate_running_app(&fixture.socket, None));
    assert_eq!(fixture.server.take_usage(), []);
}

#[test]
fn opening_shellrs_again_brings_the_running_one_forward() {
    let fixture = fixture();
    // Whatever 启用外部 CLI says: this is not the external CLI.
    fixture.server.set_enabled(false);
    assert_eq!(fixture.server.take_activation(), None);

    assert!(client::activate_running_app(&fixture.socket, None));
    assert_eq!(fixture.server.take_activation(), Some(Vec::new()));
    // Heard once, acted on once.
    assert_eq!(fixture.server.take_activation(), None);
    assert!(fixture.backend.calls.lock().unwrap().is_empty());

    // The switch still guards everything else.
    let (code, _, stderr) = run(&fixture.socket, Request::List { query: None }, true);
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [not_enabled]")
    );
    assert_eq!(fixture.server.take_activation(), None);
}

#[test]
fn opening_shellrs_with_links_hands_them_to_the_running_one_in_order() {
    let fixture = fixture();
    fixture.server.set_enabled(false);
    let link = |url: &str, tab: Option<&str>| OpenLink {
        url: url.into(),
        tab: tab.map(str::to_string),
    };
    let first = link("ssh://token:p%40ss@172.16.0.28:12024", Some("堡垒机"));
    let second = link("ssh://root@10.0.0.9", None);
    assert!(client::activate_running_app(
        &fixture.socket,
        Some(first.clone())
    ));
    assert!(client::activate_running_app(&fixture.socket, None));
    assert!(client::activate_running_app(
        &fixture.socket,
        Some(second.clone())
    ));
    assert_eq!(fixture.server.take_activation(), Some(vec![first, second]));
    assert_eq!(fixture.server.take_activation(), None);
}

#[test]
fn a_host_opened_from_a_link_is_listed_as_temporary_and_logged_in_to_afresh() {
    let mut store = HostStore::empty();
    let saved = store.insert_unnotified(HostDraft::new(
        "web",
        "10.0.1.12",
        22,
        "root",
        AuthKind::Password,
        None,
    ));
    let link = store.insert_external_unnotified(
        SshLink::parse("ssh://token:secret@10.0.0.9:2222", Some("堡垒机")).unwrap(),
    );
    let targets = CliTarget::all(&store);
    let ids: Vec<HostId> = targets.iter().map(|target| target.host().id).collect();
    assert_eq!(ids, [saved, link]);
    // With the link's password, which only memory holds.
    assert_eq!(targets[1].login().password, SecretRef::temporary(link.0));

    let fixture = fixture();
    fixture.server.set_hosts(&store);
    let (code, stdout, _) = run(&fixture.socket, Request::List { query: None }, true);
    assert_eq!(code, 0);
    let hosts: Vec<HostInfo> = serde_json::from_str(&stdout).unwrap();
    let listed: Vec<(&str, bool)> = hosts
        .iter()
        .map(|host| (host.name.as_str(), host.temporary))
        .collect();
    assert_eq!(listed, [("web", false), ("堡垒机", true)]);
    let temporary = store.host(link).unwrap().public_id.to_string();
    assert_eq!(hosts[1].id, temporary);
    assert_eq!(hosts[1].address(), "token@10.0.0.9:2222");

    // Reached by that ID like a saved host.
    let (code, _, _) = run(
        &fixture.socket,
        Request::Exec {
            host: temporary,
            command: "uptime".into(),
        },
        false,
    );
    assert_eq!(code, 3);

    // A table says so where the group goes.
    let (_, table, _) = run(&fixture.socket, Request::List { query: None }, false);
    assert!(
        table.lines().nth(2).unwrap().contains("（未保存）"),
        "{table}"
    );
}

#[test]
fn an_older_apps_list_reads_as_saved_hosts() {
    let host: HostInfo = serde_json::from_str(
        r#"{"id":"VmLkf1snMOuPKJ07","name":"web","group":null,"user":"root","host":"10.0.1.12","port":22,"os":null}"#,
    )
    .unwrap();
    assert!(!host.temporary);
}

#[test]
fn with_nothing_running_shellrs_is_free_to_start() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!client::activate_running_app(
        &cli_endpoint(dir.path()),
        None
    ));

    // Nor does a socket left behind by a crash count as a running app.
    #[cfg(unix)]
    {
        let stale = dir.path().join("stale.sock");
        // No process may start meanwhile and keep the socket open.
        let _forks = crate::testing::no_forks();
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        assert!(!client::activate_running_app(&stale, None));
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
    // JSON brings the host and the command with it, and nothing else does.
    assert!(matches!(
        parse(&["shellrs", "exec", "--json"]),
        Ok(Command::Exec {
            id: None,
            command: None,
            json: true,
            ..
        })
    ));
    assert!(parse(&["shellrs", "exec"]).is_err());
    assert!(parse(&["shellrs", "exec", "ID", "--json"]).is_err());
    assert!(parse(&["shellrs", "exec", "--json", "--stdin"]).is_err());
    // Split words are a mistake, not a longer command.
    assert!(parse(&["shellrs", "exec", "ID", "ls", "-la"]).is_err());
    // Typed into the open terminal, in any of the forms.
    assert!(matches!(
        parse(&["shellrs", "exec", "ID", "--terminal", "ls"]),
        Ok(Command::Exec { terminal: true, command: Some(command), .. }) if command == "ls"
    ));
    assert!(matches!(
        parse(&["shellrs", "exec", "--json", "--terminal"]),
        Ok(Command::Exec {
            terminal: true,
            json: true,
            ..
        })
    ));
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

#[test]
fn exec_json_reads_the_host_and_the_command() {
    let asked = |host: &str, command: &str, terminal| ExecJson {
        host: host.into(),
        command: command.into(),
        terminal,
    };
    assert_eq!(
        exec_request("\u{feff}{\"host\": \"ID\", \"command\": \"echo \\\"hi\\\" | wc -c\"}"),
        Ok(asked("ID", "echo \"hi\" | wc -c", false))
    );
    // Escaped as an agent may write it in PowerShell 5.1, which pipes ASCII.
    assert_eq!(
        exec_request(r#"{"host":"ID","command":"grep \u751f\u4ea7 a.log"}"#),
        Ok(asked("ID", "grep 生产 a.log", false))
    );
    assert_eq!(
        exec_request(r#"{"host":"ID","command":"ls","terminal":true}"#),
        Ok(asked("ID", "ls", true))
    );
    assert!(exec_request(r#"{"command":"ls"}"#).is_err());
    assert!(exec_request("ls -la").is_err());
}

#[test]
fn exec_json_output_is_ascii_whatever_it_holds() {
    assert_eq!(
        ascii_json(&serde_json::json!({ "s": "生产 😀\n" })),
        r#"{"s":"\u751f\u4ea7 \ud83d\ude00\n"}"#
    );
}

/// `exec --json` against the fixture: the exit code, and stdout parsed.
fn run_exec_json(socket: &Path, request: Request) -> (i32, serde_json::Value, Vec<u8>) {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let code = client::run(
        socket,
        request,
        &mut Console {
            stdout: &mut stdout,
            stderr: &mut stderr,
            json: true,
            exec_json: true,
            stderr_is_terminal: false,
        },
    );
    assert!(stdout.is_ascii(), "{}", String::from_utf8_lossy(&stdout));
    (code, serde_json::from_slice(&stdout).unwrap(), stderr)
}

#[test]
fn exec_json_gathers_the_output_and_says_errors_in_json() {
    let fixture = fixture();
    let (code, result, stderr) = run_exec_json(
        &fixture.socket,
        Request::Exec {
            host: id(&fixture.web),
            command: "uname -a".into(),
        },
    );
    assert_eq!(code, 3);
    assert_eq!(
        result,
        serde_json::json!({ "exit_code": 3, "stdout": "out\n", "stderr": "\u{fffd}\n" })
    );
    assert!(stderr.is_empty());

    fixture.server.set_enabled(false);
    let (code, result, stderr) = run_exec_json(
        &fixture.socket,
        Request::Exec {
            host: id(&fixture.web),
            command: "true".into(),
        },
    );
    assert_eq!(code, 255);
    assert_eq!(result["error"]["code"], "not_enabled");
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("设置 → 外部 CLI")
    );
    assert!(stderr.is_empty());
}

#[test]
fn the_new_commands_read_their_arguments() {
    let parse = |args: &[&str]| Cli::try_parse_from(args).map(|cli| cli.command);
    assert!(matches!(
        parse(&["shellrs", "hosts", "show", "ID", "--json"]),
        Ok(Command::Hosts { command: HostsCommand::Show { id, json: true } }) if id == "ID"
    ));
    assert!(matches!(
        parse(&["shellrs", "hosts", "delete", "ID", "--force"]),
        Ok(Command::Hosts {
            command: HostsCommand::Delete { force: true, .. }
        })
    ));
    assert!(matches!(
        parse(&["shellrs", "credentials", "list", "-q", "deploy"]),
        Ok(Command::Credentials { command: CredentialsCommand::List { query: Some(query), .. } })
            if query == "deploy"
    ));
    assert!(matches!(
        parse(&["shellrs", "sync", "ID", "./dist", "/srv/app", "--delete"]),
        Ok(Command::Sync { delete: true, remote, .. }) if remote == "/srv/app"
    ));
    // Hosts are listed under `hosts`, like the rest of what is done to them.
    assert!(matches!(
        parse(&["shellrs", "hosts", "list", "-q", "web"]),
        Ok(Command::Hosts { command: HostsCommand::List { query: Some(query), .. } })
            if query == "web"
    ));
    assert!(parse(&["shellrs", "list"]).is_err());
    // `credentials` has no --force: nothing of it stays open.
    assert!(parse(&["shellrs", "credentials", "delete", "ID", "--force"]).is_err());
    assert!(parse(&["shellrs", "sync", "ID", "./dist"]).is_err());
}

fn saved_host(name: &str) -> HostDetails {
    HostDetails {
        id: "Jwg5rHvXCxw89paM".into(),
        name: name.into(),
        group: None,
        host: "10.0.0.9".into(),
        port: 22,
        user: "root".into(),
        auth: AuthChoice::Password,
        credential: None,
        password_saved: Some(true),
        route: RouteDetails::Direct,
        notes: String::new(),
        os: None,
        temporary: false,
    }
}

/// The app's end of a change: wait for it to come in, and hand it over
/// with what to answer it with.
fn take_change(server: &CliServer) -> (CliChange, super::server::ChangeReply) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(taken) = server.take_change() {
            return taken;
        }
        assert!(std::time::Instant::now() < deadline, "no change came in");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_change_waits_for_the_app_and_comes_back_as_it_made_it() {
    let fixture = fixture();
    let socket = fixture.socket.clone();
    let fields = HostFields {
        name: Some("db-02".into()),
        host: Some("10.0.0.9".into()),
        password: Some(Some(Secret::new("hunter2"))),
        ..HostFields::default()
    };
    let command = std::thread::spawn(move || run(&socket, Request::CreateHost { fields }, false));
    let (change, reply) = take_change(&fixture.server);
    let CliChange::CreateHost(fields) = change else {
        panic!("{change:?}");
    };
    // The password is there to be saved, and nowhere it could be printed.
    assert_eq!(
        fields
            .password
            .as_ref()
            .map(|password| password.as_ref().map(Secret::expose)),
        Some(Some("hunter2"))
    );
    assert!(!format!("{fields:?}").contains("hunter2"));
    reply.send(Ok(Reply::Host(saved_host("db-02"))));
    let (code, stdout, _) = command.join().unwrap();
    assert_eq!(code, 0);
    assert_eq!(stdout, "已创建主机「db-02」，ID Jwg5rHvXCxw89paM\n");

    // What went wrong, said the way every error is.
    let socket = fixture.socket.clone();
    let command = std::thread::spawn(move || {
        run(
            &socket,
            Request::DeleteHost {
                host: "x".into(),
                force: false,
            },
            false,
        )
    });
    let (_, reply) = take_change(&fixture.server);
    reply.send(Err(CliError::new(
        ErrorCode::HostInUse,
        "主机「x」有打开的标签",
    )));
    let (code, _, stderr) = command.join().unwrap();
    assert_eq!(code, 255);
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "shellrs: [host_in_use] 主机「x」有打开的标签\n"
    );
}

#[test]
fn a_change_nobody_takes_up_is_taken_back_and_one_dropped_says_so() {
    let fixture = fixture();
    // The tests' wait in line is short; nothing takes it up.
    let (code, _, stderr) = run(
        &fixture.socket,
        Request::DeleteCredential {
            credential: "x".into(),
        },
        false,
    );
    assert_eq!(code, 255);
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(stderr.contains("改动没有做"), "{stderr}");
    assert!(fixture.server.take_change().is_none(), "taken back");

    // Taken up, then the app goes without an answer.
    let socket = fixture.socket.clone();
    let command = std::thread::spawn(move || {
        run(
            &socket,
            Request::DeleteCredential {
                credential: "x".into(),
            },
            false,
        )
    });
    let (_, reply) = take_change(&fixture.server);
    drop(reply);
    let (code, _, stderr) = command.join().unwrap();
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .contains("ShellRS 在处理改动时关闭了")
    );
}

#[test]
fn with_the_switch_off_no_change_reaches_the_app() {
    let fixture = fixture();
    fixture.server.set_enabled(false);
    let (code, _, stderr) = run(
        &fixture.socket,
        Request::CreateHost {
            fields: HostFields::default(),
        },
        false,
    );
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [not_enabled]")
    );
    assert!(fixture.server.take_change().is_none());
}

#[test]
fn showing_a_host_or_a_credential_says_what_is_saved_and_never_what() {
    let fixture = fixture();
    let mut store = store();
    let credential = store.insert_credential_unnotified(CredentialDraft::new(
        "部署",
        CredentialKind::Password,
        "deploy",
    ));
    let keychain_id = store
        .credential(credential)
        .unwrap()
        .keychain_id
        .to_string();
    let db = store.hosts()[1].id;
    let draft = store.host(db).unwrap().draft().with_credential(credential);
    store.update_unnotified(db, draft);
    fixture.server.set_hosts(&store);
    let web_host = store.hosts()[0].clone();

    let show = |host: &Host| {
        let (code, stdout, _) = run(&fixture.socket, Request::ShowHost { host: id(host) }, true);
        assert_eq!(code, 0);
        serde_json::from_str::<HostDetails>(&stdout).unwrap()
    };
    let web = show(&web_host);
    assert_eq!(
        (web.auth, web.password_saved),
        (AuthChoice::Password, Some(true))
    );
    assert_eq!(web.group.as_deref(), Some("生产"));
    let db = show(store.host(db).unwrap());
    assert_eq!(db.auth, AuthChoice::Credential);
    assert_eq!(db.credential.as_deref(), Some(keychain_id.as_str()));
    assert_eq!((db.user.as_str(), db.password_saved), ("deploy", None));

    let (_, table, _) = run(
        &fixture.socket,
        Request::ShowHost {
            host: id(&web_host),
        },
        false,
    );
    assert!(table.contains("认证      密码（已保存）"), "{table}");
    assert!(table.contains("连接方式  直接连接"), "{table}");

    let (code, stdout, _) = run(
        &fixture.socket,
        Request::ListCredentials {
            query: Some("DEPLOY".into()),
        },
        true,
    );
    assert_eq!(code, 0);
    let listed: Vec<CredentialDetails> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].hosts, std::slice::from_ref(&db.id));
    assert_eq!(listed[0].password_saved, None, "only looked up when shown");
    let (_, stdout, _) = run(
        &fixture.socket,
        Request::ShowCredential {
            credential: keychain_id.clone(),
        },
        true,
    );
    let shown: CredentialDetails = serde_json::from_str(&stdout).unwrap();
    assert_eq!(shown.password_saved, Some(false));
    let (code, _, stderr) = run(
        &fixture.socket,
        Request::ShowCredential {
            credential: "nope".into(),
        },
        false,
    );
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [credential_not_found]")
    );
}

#[test]
fn a_sync_says_what_had_not_changed_and_what_went() {
    let fixture = fixture();
    let (code, stdout, _) = run(
        &fixture.socket,
        Request::Sync {
            host: id(&fixture.web),
            source: local("dist"),
            destination: "/srv/app".into(),
            delete: true,
        },
        false,
    );
    assert_eq!(code, 0);
    assert_eq!(
        stdout,
        "已传输 2 个文件，共 2.0 KB，未变 40 个，删除 3 个\n"
    );
    assert_eq!(
        fixture.backend.calls.lock().unwrap().last().unwrap(),
        &format!("sync web-01 {} /srv/app true", local("dist").display())
    );
    // Relative is the command's to resolve.
    let (code, _, _) = run(
        &fixture.socket,
        Request::Sync {
            host: id(&fixture.web),
            source: "dist".into(),
            destination: "/srv/app".into(),
            delete: false,
        },
        false,
    );
    assert_eq!(code, 255);
}

/// What an app sees of a request it does not know, and of a version it
/// does not speak, sent the way a later command would.
#[cfg(unix)]
#[test]
fn a_request_from_a_later_command_is_told_apart_from_a_bad_one() {
    use super::protocol::{parse_json, read_frame, write_json};

    let fixture = fixture();
    let answer = |envelope: serde_json::Value| {
        let stream = std::os::unix::net::UnixStream::connect(&fixture.socket).unwrap();
        write_json(&mut &stream, &envelope).unwrap();
        let (_, payload) = read_frame(&mut &stream).unwrap().unwrap();
        parse_json::<Reply>(&payload).unwrap()
    };
    let Reply::Error { code, message } =
        answer(serde_json::json!({ "version": 2, "request": { "type": "teleport" } }))
    else {
        panic!("answered");
    };
    assert_eq!(code, ErrorCode::BadRequest);
    // Which the command takes for an app older than itself.
    assert!(message.contains("unknown variant"), "{message}");
    let Reply::Error { code, .. } =
        answer(serde_json::json!({ "version": 3, "request": { "type": "teleport" } }))
    else {
        panic!("answered");
    };
    assert_eq!(code, ErrorCode::VersionMismatch);
}

/// An app from before a command was added, as the command hears it.
#[cfg(unix)]
#[test]
fn an_app_that_does_not_know_the_command_is_said_to_be_older() {
    use super::protocol::{read_frame, write_json};

    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("old.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let app = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        read_frame(&mut &stream).unwrap();
        write_json(
            &mut &stream,
            &Reply::Error {
                code: ErrorCode::BadRequest,
                message: "unknown variant `show_host`, expected one of `list`, `exec`".into(),
            },
        )
        .unwrap();
    });
    let (code, _, stderr) = run(&socket, Request::ShowHost { host: "x".into() }, false);
    app.join().unwrap();
    assert_eq!(code, 255);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("shellrs: [version_mismatch] 正在运行的 ShellRS 不认识这个命令"),
    );
}

/// The app's end of an `exec --terminal` run: wait for it to come in.
fn take_run(server: &CliServer) -> (String, RunHandle) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(taken) = server.take_run() {
            return taken;
        }
        assert!(std::time::Instant::now() < deadline, "no run came in");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A terminal taking `run` up, with a shell whose program is `shell`: it
/// answers the probe, then the command line with `output` and `code`,
/// slowly enough for the server to check on the caller meanwhile.
/// Everything typed, once nothing more is.
fn answer_run(
    run: RunHandle,
    shell: &'static str,
    output: &'static str,
    code: i32,
) -> std::thread::JoinHandle<String> {
    let (input, typed) = std::sync::mpsc::channel();
    assert!(run.accept(input));
    std::thread::spawn(move || {
        let mut all = String::new();
        while let Ok(TerminalTransportCommand::Write(bytes)) =
            typed.recv_timeout(std::time::Duration::from_millis(500))
        {
            let line = String::from_utf8(bytes).unwrap();
            all.push_str(&line);
            // ` sh -c '<script>' sh NONCE …`
            let Some(at) = line.find("' sh ") else {
                continue;
            };
            let nonce = &line[at + 5..at + 17];
            if line.contains("6973;P;") {
                run.feed(format!("\x1b]6973;P;{nonce};{shell}\x07").as_bytes());
            } else if line.contains("6973;B;") {
                let context: String = "deploy\nasset\n/home/deploy\nbash"
                    .bytes()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                run.feed(format!("\x1b]6973;B;{nonce};{context}\x07").as_bytes());
                std::thread::sleep(std::time::Duration::from_millis(100));
                run.feed(format!("{output}\x1b]6973;E;{nonce};{code}\x07").as_bytes());
            }
        }
        all
    })
}

#[test]
fn a_command_typed_into_the_terminal_passes_on_its_output_and_where_it_ran() {
    let fixture = fixture();
    let exec = |json| {
        let socket = fixture.socket.clone();
        let request = Request::ExecInTerminal {
            host: id(&fixture.web),
            command: "ls /srv".into(),
        };
        std::thread::spawn(move || {
            if json {
                let (code, result, _) = run_exec_json(&socket, request);
                (code, result.to_string())
            } else {
                let (code, stdout, _) = run(&socket, request, false);
                (code, stdout)
            }
        })
    };

    let command = exec(false);
    let (host, run) = take_run(&fixture.server);
    assert_eq!(host, id(&fixture.web));
    let terminal = answer_run(run, "/usr/bin/bash", "a\r\nb\r\n", 2);
    assert_eq!(command.join().unwrap(), (2, "a\nb\n".to_string()));
    assert!(terminal.join().unwrap().contains(" 'ls /srv'\r"));

    let command = exec(true);
    let (_, run) = take_run(&fixture.server);
    answer_run(run, "/bin/busybox", "a\r\n", 0);
    let (code, result) = command.join().unwrap();
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&result).unwrap(),
        serde_json::json!({
            "exit_code": 0,
            "stdout": "a\n",
            "stderr": "",
            "context": {
                "user": "deploy",
                "host": "asset",
                "cwd": "/home/deploy",
                "shell": "busybox",
                "interp": "bash",
            },
        })
    );
    assert_eq!(
        fixture.server.take_usage(),
        [CliUse::ExecTerminal, CliUse::ExecTerminal]
    );
}

#[test]
fn a_run_that_cannot_be_typed_says_why() {
    let fixture = fixture();
    let exec = || {
        let socket = fixture.socket.clone();
        let host = id(&fixture.web);
        std::thread::spawn(move || {
            let request = Request::ExecInTerminal {
                host,
                command: "true".into(),
            };
            let (code, _, stderr) = run(&socket, request, false);
            (code, String::from_utf8(stderr).unwrap())
        })
    };
    let refused = |command: std::thread::JoinHandle<(i32, String)>, code: &str| {
        let (exit, stderr) = command.join().unwrap();
        assert_eq!(exit, 255);
        assert!(
            stderr.starts_with(&format!("shellrs: [{code}] ")),
            "{stderr}"
        );
        stderr
    };

    // No terminal for it.
    let command = exec();
    take_run(&fixture.server).1.fail(RunFailure::NoTerminal);
    refused(command, "no_terminal");

    // A shell it cannot type into: nothing but the probe was typed.
    let command = exec();
    let terminal = answer_run(take_run(&fixture.server).1, "/usr/bin/zsh", "", 0);
    assert!(refused(command, "unsupported_shell").contains("zsh"));
    assert!(!terminal.join().unwrap().contains("6973;B;"));

    // Nothing answers the probe.
    let command = exec();
    let (input, _typed) = std::sync::mpsc::channel();
    assert!(take_run(&fixture.server).1.accept(input));
    refused(command, "terminal_busy");

    // Nobody takes it up: taken back, never typed.
    let command = exec();
    refused(command, "connect_failed");
    assert!(fixture.server.take_run().is_none());

    // An unknown host never gets to the app.
    let socket = fixture.socket.clone();
    let request = Request::ExecInTerminal {
        host: "nope".into(),
        command: "true".into(),
    };
    assert_eq!(run(&socket, request, false).0, 255);
    assert!(fixture.server.take_run().is_none());
}
