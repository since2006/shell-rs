//! The `shellrs-cli` program run as an agent runs it: a separate process
//! with piped stdio, finding the app through `SHELLRS_DATA_DIR`. A fake
//! backend stands in for SSH, so this runs on every platform, including the
//! Windows CI where it is the one test of the named pipe between processes.
//! Also ShellRS itself opened with a link, as a bastion host opens it.

use std::{
    io::{self, Write as _},
    path::Path,
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use shellrs::app::cli_endpoint;
use shellrs::cli::{
    AuthChoice, CliBackend, CliChange, CliError, CliServer, CliTarget, HostDetails, HostInfo,
    OpenLink, Reply, RouteDetails, TransferCounters, TransferSummary,
};
use shellrs::host::{AuthKind, HostDraft, HostStore};
use shellrs::secrets::SecretRef;
use shellrs::ssh::ExecStream;

/// More than a pipe's buffer, so the app finishes writing before the
/// command has read it all.
const LARGE_OUTPUT: usize = 256 * 1024;

#[derive(Default)]
struct FakeBackend {
    commands: Mutex<Vec<String>>,
}

impl CliBackend for FakeBackend {
    fn exec(
        &self,
        _: &CliTarget,
        command: &str,
        output: &mut dyn FnMut(ExecStream, &[u8]) -> io::Result<()>,
    ) -> Result<i32, CliError> {
        self.commands.lock().unwrap().push(command.to_string());
        if command == "large" {
            let line = [b'x'; 1023]
                .iter()
                .chain(b"\n")
                .copied()
                .collect::<Vec<_>>();
            for _ in 0..LARGE_OUTPUT / line.len() {
                output(ExecStream::Stdout, &line).unwrap();
            }
            return Ok(0);
        }
        output(ExecStream::Stdout, "输出\n".as_bytes()).unwrap();
        output(ExecStream::Stderr, b"warning\n").unwrap();
        Ok(7)
    }

    fn upload(
        &self,
        _: &CliTarget,
        _: &Path,
        _: &str,
        _: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        Ok(TransferSummary::default())
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
        _: &CliTarget,
        source: &Path,
        destination: &str,
        delete: bool,
        _: &mut dyn FnMut(TransferCounters) -> io::Result<()>,
    ) -> Result<TransferSummary, CliError> {
        self.commands
            .lock()
            .unwrap()
            .push(format!("sync {} {destination} {delete}", source.display()));
        Ok(TransferSummary {
            files: 1,
            bytes: 10,
            skipped: 5,
            deleted: 2,
            ..TransferSummary::default()
        })
    }

    fn is_saved(&self, _: &SecretRef) -> bool {
        false
    }
}

/// Run the command against the app whose data lives in `data_dir`.
fn shellrs(data_dir: &Path, args: &[&str], stdin: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_shellrs-cli"))
        .args(args)
        .env("SHELLRS_DATA_DIR", data_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn without_the_app_the_command_says_so_and_exits_255() {
    let data_dir = tempfile::tempdir().unwrap();
    let output = shellrs(data_dir.path(), &["hosts", "list"], b"");
    assert_eq!(output.status.code(), Some(255));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("shellrs: [not_running]")
    );
}

#[test]
fn the_command_finds_the_app_through_its_data_directory() {
    let data_dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(FakeBackend::default());
    let server = CliServer::start(cli_endpoint(data_dir.path()), backend.clone()).unwrap();
    let mut store = HostStore::empty();
    let web = store.insert_unnotified(HostDraft::new(
        "web-01",
        "10.0.1.12",
        22,
        "root",
        AuthKind::Password,
        None,
    ));
    server.set_hosts(&store);
    server.set_enabled(true);
    let id = store.host(web).unwrap().public_id.to_string();

    // A pipe for stdout means JSON.
    let output = shellrs(data_dir.path(), &["hosts", "list"], b"");
    assert_eq!(output.status.code(), Some(0));
    let hosts: Vec<HostInfo> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(hosts[0].id, id);

    // What PowerShell pipes in: a BOM and CRLF line ends.
    let output = shellrs(
        data_dir.path(),
        &["exec", &id, "--stdin"],
        "\u{feff}cd /srv\r\nls\r\n".as_bytes(),
    );
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, "输出\n".as_bytes());
    assert_eq!(output.stderr, b"warning\n");
    assert_eq!(
        backend.commands.lock().unwrap().last().unwrap(),
        "cd /srv\nls\n"
    );

    // JSON in, JSON out, in ASCII alone.
    let request = serde_json::json!({ "host": id, "command": "ls" }).to_string();
    let output = shellrs(data_dir.path(), &["exec", "--json"], request.as_bytes());
    assert_eq!(output.status.code(), Some(7));
    assert!(output.stdout.is_ascii());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({ "exit_code": 7, "stdout": "输出\n", "stderr": "warning\n" })
    );
    assert!(output.stderr.is_empty());
    let output = shellrs(data_dir.path(), &["exec", "--json"], b"ls");
    assert_eq!(output.status.code(), Some(255));
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "bad_request");

    // Everything arrives, though the app is done long before.
    let output = shellrs(data_dir.path(), &["exec", &id, "large"], b"");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout.len(), LARGE_OUTPUT);

    // A folder's contents, from where the command runs.
    let output = shellrs(
        data_dir.path(),
        &[
            "sync",
            &id,
            data_dir.path().to_str().unwrap(),
            "/srv/app",
            "--delete",
        ],
        b"",
    );
    assert_eq!(output.status.code(), Some(0));
    let summary: TransferSummary = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!((summary.skipped, summary.deleted), (5, 2));
    assert!(
        backend
            .commands
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .ends_with(" /srv/app true")
    );

    server.set_enabled(false);
    let output = shellrs(data_dir.path(), &["exec", &id, "true"], b"");
    assert_eq!(output.status.code(), Some(255));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("shellrs: [not_enabled]")
    );
}

/// Open ShellRS itself, as a bastion host does, beside the app whose data
/// lives in `data_dir`; its exit code. Handing its link over, it exits at
/// once. One that does not is killed rather than left running as a second
/// app with a window of its own.
fn open_shellrs(data_dir: &Path, args: &[&str]) -> Option<i32> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_shellrs"))
        .args(args)
        .env("SHELLRS_DATA_DIR", data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("ShellRS opened with {args:?} did not hand its link over");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn shellrs_opened_with_a_link_hands_it_to_the_running_app() {
    let data_dir = tempfile::tempdir().unwrap();
    let server = CliServer::start(
        cli_endpoint(data_dir.path()),
        Arc::new(FakeBackend::default()),
    )
    .unwrap();
    // 启用外部 CLI is off: being opened again is not the external CLI.
    let xshell = "ssh://deploy:p%40ss@10.0.0.9:2222";
    let token = "ssh://b478e26f-811b-4a90-81c3-74929127898a@172.16.0.28:12024";
    assert_eq!(
        open_shellrs(
            data_dir.path(),
            &["-url", xshell, "-newtab", "跳板机", "-newwin"]
        ),
        Some(0)
    );
    assert_eq!(open_shellrs(data_dir.path(), &[token]), Some(0));
    // As for WinSCP.
    let winscp = "sftp://deploy:p%40ss@10.0.0.9:2222";
    assert_eq!(
        open_shellrs(data_dir.path(), &["/sessionname=文件", winscp]),
        Some(0)
    );
    assert_eq!(
        server.take_activation(),
        Some(vec![
            OpenLink {
                url: xshell.into(),
                tab: Some("跳板机".into()),
            },
            OpenLink {
                url: token.into(),
                tab: None,
            },
            OpenLink {
                url: winscp.into(),
                tab: Some("文件".into()),
            },
        ])
    );
}

#[test]
fn a_host_described_on_stdin_reaches_the_app_and_comes_back_saved() {
    let data_dir = tempfile::tempdir().unwrap();
    let server = Arc::new(
        CliServer::start(
            cli_endpoint(data_dir.path()),
            Arc::new(FakeBackend::default()),
        )
        .unwrap(),
    );
    server.set_enabled(true);
    // The app's side: take the change up and answer with the host saved.
    let app = std::thread::spawn({
        let server = server.clone();
        move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some((change, reply)) = server.take_change() {
                    let CliChange::CreateHost(fields) = change else {
                        panic!("not a new host: {change:?}");
                    };
                    let password = fields.password.clone().flatten();
                    reply.send(Ok(Reply::Host(HostDetails {
                        id: "Jwg5rHvXCxw89paM".into(),
                        name: fields.name.clone().unwrap_or_default(),
                        group: fields.group.clone().flatten(),
                        host: fields.host.clone().unwrap_or_default(),
                        port: fields.port.unwrap_or(22),
                        user: "root".into(),
                        auth: AuthChoice::Password,
                        credential: None,
                        password_saved: Some(password.is_some()),
                        route: RouteDetails::Direct,
                        notes: String::new(),
                        os: None,
                        temporary: false,
                    })));
                    return password.map(|password| password.expose().to_string());
                }
                assert!(Instant::now() < deadline, "the command sent nothing");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    });
    // As Windows PowerShell pipes it: a BOM, CRLF, and its escapes.
    let host = "\u{feff}{\"name\": \"\\u751f\\u4ea7 web\", \"address\": \"10.0.1.12\",\r\n \"group\": \"\u{751f}\u{4ea7}/web\", \"password\": \"s3cret\", \"id\": \"ignored\"}\r\n";
    let output = shellrs(data_dir.path(), &["hosts", "create"], host.as_bytes());
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let saved: HostDetails = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(saved.name, "生产 web");
    assert_eq!(saved.group.as_deref(), Some("生产/web"));
    assert_eq!(saved.password_saved, Some(true));
    assert_eq!(app.join().unwrap().as_deref(), Some("s3cret"));

    // A field the command does not know is refused before anything is sent.
    let output = shellrs(
        data_dir.path(),
        &["hosts", "create"],
        br#"{"name": "web", "adress": "10.0.1.12"}"#,
    );
    assert_eq!(output.status.code(), Some(255));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("shellrs: [bad_request]"), "{stderr}");
    assert!(stderr.contains("adress"), "{stderr}");
    assert!(server.take_change().is_none());
}
