//! The `shellrs-cli` program run as an agent runs it: a separate process
//! with piped stdio, finding the app through `SHELLRS_DATA_DIR`. A fake
//! backend stands in for SSH, so this runs on every platform, including the
//! Windows CI where it is the one test of the named pipe between processes.

use std::{
    io::{self, Write as _},
    path::Path,
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
};

use shellrs::app::cli_endpoint;
use shellrs::cli::{
    CliBackend, CliError, CliServer, CliTarget, HostInfo, TransferCounters, TransferSummary,
};
use shellrs::host::{AuthKind, Host, HostDraft, HostId, HostLogin};
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
    let output = shellrs(data_dir.path(), &["list"], b"");
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
    let web = Host::new(
        HostId(1),
        HostDraft::new("web-01", "10.0.1.12", 22, "root", AuthKind::Password, None),
    );
    server.set_targets(vec![CliTarget::new(&web, HostLogin::of(&web, None), None)]);
    server.set_enabled(true);
    let id = web.public_id.to_string();

    // A pipe for stdout means JSON.
    let output = shellrs(data_dir.path(), &["list"], b"");
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

    // Everything arrives, though the app is done long before.
    let output = shellrs(data_dir.path(), &["exec", &id, "large"], b"");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout.len(), LARGE_OUTPUT);

    server.set_enabled(false);
    let output = shellrs(data_dir.path(), &["exec", &id, "true"], b"");
    assert_eq!(output.status.code(), Some(255));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("shellrs: [not_enabled]")
    );
}
