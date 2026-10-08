use std::sync::mpsc;
use std::time::Duration;

use super::*;

/// Timing for runs whose terminal is a thread of the test.
const QUICK: RunTiming = RunTiming {
    settle: Duration::ZERO,
    probe: Duration::from_millis(200),
    start: Duration::from_millis(200),
    tick: Duration::from_millis(10),
};

fn nonce(run: &TerminalRun) -> String {
    lock(&run.state).nonce.clone()
}

fn marker(kind: &str, nonce: &str, payload: &str) -> String {
    format!("\x1b]6973;{kind};{nonce};{payload}\x07")
}

fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// What was typed into the terminal so far, as text.
fn typed(input: &mpsc::Receiver<TerminalTransportCommand>) -> String {
    input
        .try_iter()
        .map(|command| match command {
            TerminalTransportCommand::Write(bytes) => String::from_utf8(bytes).unwrap(),
            _ => String::new(),
        })
        .collect()
}

/// A run a terminal took up, with the terminal's input.
fn accepted(
    command: &str,
) -> (
    TerminalRun,
    mpsc::Receiver<RunEvent>,
    mpsc::Receiver<TerminalTransportCommand>,
) {
    let (run, events) = TerminalRun::new(command.to_owned());
    let (input, typed) = mpsc::channel();
    assert!(run.handle().accept(input));
    assert_eq!(events.try_recv(), Ok(RunEvent::Accepted));
    (run, events, typed)
}

/// What the scanner found, with the output of successive chunks joined.
fn joined(found: Vec<Scanned>) -> Vec<Scanned> {
    let mut joined: Vec<Scanned> = Vec::new();
    for scanned in found {
        match (joined.last_mut(), scanned) {
            (Some(Scanned::Output(before)), Scanned::Output(more)) => before.extend(more),
            (_, scanned) => joined.push(scanned),
        }
    }
    joined
}

#[test]
fn markers_are_found_however_the_output_is_split() {
    let mut scanner = RunScanner::new("abc");
    let begin = marker("B", "abc", "6869");
    let mut found = scanner.scan(format!("$ echo\r\n{begin}out\r\nline2\r").as_bytes());
    found.extend(scanner.scan(b"\n\x1b]69"));
    found.extend(scanner.scan(b"73;E;abc;3\x07$ "));
    assert_eq!(
        joined(found),
        vec![
            Scanned::Begin(b"6869".to_vec()),
            Scanned::Output(b"out\nline2\n".to_vec()),
            Scanned::End(3),
        ]
    );
    // After E, the prompt is not the command's.
    assert!(scanner.scan(b"more\r\n").is_empty());
}

#[test]
fn only_this_runs_markers_count_and_either_terminator_ends_them() {
    let mut scanner = RunScanner::new("abc");
    let found = scanner.scan(
        format!(
            "{}{}\x1b]6973;P;abc;/usr/bin/bash\x1b\\",
            marker("P", "other", "/bin/zsh"),
            marker("E", "abc", "oops"),
        )
        .as_bytes(),
    );
    assert_eq!(
        found,
        vec![Scanned::End(255), Scanned::Probe("/usr/bin/bash".into())]
    );
    // The echo of a typed line has the escape as text.
    assert!(
        scanner
            .scan(br#" sh -c 'printf "\033]6973;P;%s" "$1"' sh abc"#)
            .is_empty()
    );
}

#[test]
fn a_lone_carriage_return_stays_and_crlf_becomes_lf() {
    let mut scanner = RunScanner::new("n");
    let found = scanner.scan(
        format!(
            "{}50%\r100%\r\n{}",
            marker("B", "n", ""),
            marker("E", "n", "0")
        )
        .as_bytes(),
    );
    assert_eq!(found[1], Scanned::Output(b"50%\r100%\n".to_vec()));
}

#[test]
fn shells_are_told_by_their_program() {
    assert_eq!(RunShell::of("/usr/bin/bash"), Some(RunShell::Bash));
    // Upgraded under the running shell.
    assert_eq!(
        RunShell::of("/usr/bin/bash (deleted)"),
        Some(RunShell::Bash)
    );
    assert_eq!(RunShell::of("/usr/bin/dash"), Some(RunShell::Dash));
    assert_eq!(RunShell::of("/bin/busybox\n"), Some(RunShell::Busybox));
    assert_eq!(RunShell::of("/usr/bin/zsh"), None);
    assert_eq!(RunShell::of(""), None);
    assert_eq!(program_name("/tmp/mysh"), "mysh");
    assert_eq!(RunShell::Busybox.line_limit(), 1000);
    assert_eq!(RunShell::Dash.line_limit(), 4000);
}

#[test]
fn the_context_keeps_a_directory_with_newlines_whole() {
    let context = RunContext::read(hex("root\nweb-01\n/srv/a\nb\nbash").as_bytes(), "dash");
    assert_eq!(
        context,
        RunContext {
            user: "root".into(),
            host: "web-01".into(),
            cwd: "/srv/a\nb".into(),
            shell: "dash".into(),
            interp: "bash".into(),
        }
    );
    // `od` missing: nothing known, the run goes on.
    assert_eq!(RunContext::read(b"", "bash").user, "");
}

#[test]
fn a_run_probes_then_types_its_command_and_reads_its_output() {
    let (run, events, input) = accepted("uname -a");
    let handle = run.handle();
    let nonce = nonce(&run);
    run.type_probe();
    let probe = typed(&input);
    assert!(probe.starts_with(" sh -c '") && probe.ends_with(&format!(" sh {nonce}\r")));

    assert!(handle.feed(marker("P", &nonce, "/usr/bin/bash").as_bytes()));
    assert_eq!(events.try_recv(), Ok(RunEvent::Typed));
    let command = typed(&input);
    assert!(
        command.ends_with(&format!(" sh {nonce} 'uname -a'\r")),
        "{command}"
    );

    let context = hex("root\nweb-01\n/root\nbash");
    let output = format!(
        "{}Linux\r\n{}$ ",
        marker("B", &nonce, &context),
        marker("E", &nonce, "0")
    );
    assert!(!handle.feed(output.as_bytes()));
    let RunEvent::Begun(context) = events.try_recv().unwrap() else {
        panic!("not begun");
    };
    assert_eq!(
        (context.user.as_str(), context.shell.as_str()),
        ("root", "bash")
    );
    assert_eq!(events.try_recv(), Ok(RunEvent::Output(b"Linux\n".to_vec())));
    assert_eq!(events.try_recv(), Ok(RunEvent::Exited(0)));
    // Finished: dropping it sends nothing.
    drop(run);
    assert_eq!(typed(&input), "");
}

#[test]
fn an_answer_after_the_run_was_given_up_types_nothing() {
    let (run, events, input) = accepted("rm -rf build");
    let handle = run.handle();
    run.type_probe();
    typed(&input);
    assert!(run.give_up(BusyReason::NoAnswer));
    assert!(!handle.feed(marker("P", &nonce(&run), "/usr/bin/bash").as_bytes()));
    assert_eq!(typed(&input), "");
    assert!(events.try_recv().is_err());
    assert!(!handle.is_active());
}

#[test]
fn only_a_typed_command_gets_a_ctrl_c() {
    let (run, _events, input) = accepted("true");
    run.type_probe();
    typed(&input);
    // Probing: the probe is harmless where it waits.
    assert!(!run.give_up(BusyReason::NotStarted));
    run.cancel();
    assert_eq!(typed(&input), "");

    let (run, _events, input) = accepted("sleep 9");
    run.type_probe();
    run.handle()
        .feed(marker("P", &nonce(&run), "/bin/busybox").as_bytes());
    typed(&input);
    drop(run);
    assert_eq!(typed(&input), "\x03");
}

#[test]
fn a_shell_other_than_the_three_or_a_line_too_long_types_nothing() {
    let (run, events, input) = accepted("true");
    run.type_probe();
    typed(&input);
    run.handle()
        .feed(marker("P", &nonce(&run), "/usr/bin/zsh").as_bytes());
    assert_eq!(
        events.try_recv(),
        Ok(RunEvent::Failed(RunFailure::UnsupportedShell("zsh".into())))
    );
    assert_eq!(typed(&input), "");

    let (run, events, input) = accepted(&"x".repeat(1000));
    run.type_probe();
    typed(&input);
    run.handle()
        .feed(marker("P", &nonce(&run), "/bin/busybox").as_bytes());
    assert_eq!(
        events.try_recv(),
        Ok(RunEvent::Failed(RunFailure::TooLong { limit: 1000 }))
    );
    assert_eq!(typed(&input), "");
}

#[test]
fn a_terminal_gone_says_whether_the_command_may_have_run() {
    let (run, events, _input) = accepted("true");
    run.type_probe();
    run.handle().disconnected();
    assert_eq!(
        events.try_recv(),
        Ok(RunEvent::Failed(RunFailure::NotConnected))
    );

    let (run, events, _input) = accepted("true");
    run.type_probe();
    run.handle()
        .feed(marker("P", &nonce(&run), "/usr/bin/bash").as_bytes());
    events.try_recv().unwrap();
    run.handle().disconnected();
    assert_eq!(
        events.try_recv(),
        Ok(RunEvent::Failed(RunFailure::Disconnected))
    );
}

/// How a fake shell answers the command line.
#[derive(Clone, Copy)]
enum Answer {
    /// B, this output and E with 7.
    Runs(&'static str),
    /// B alone: still running.
    Starts,
    /// Nothing: held back.
    Nothing,
}

/// Answers a run's lines the way a shell would, on a thread: P at once,
/// then the command line as `answer` says. Stops once nothing is typed
/// for a while (the run's handle keeps the input open); everything typed.
fn fake_shell(
    run: &TerminalRun,
    input: mpsc::Receiver<TerminalTransportCommand>,
    answer: Answer,
) -> std::thread::JoinHandle<String> {
    let (handle, nonce) = (run.handle(), nonce(run));
    std::thread::spawn(move || {
        let mut all = String::new();
        while let Ok(TerminalTransportCommand::Write(bytes)) =
            input.recv_timeout(Duration::from_millis(500))
        {
            let line = String::from_utf8(bytes).unwrap();
            all.push_str(&line);
            let reply = if line.contains("6973;P;") {
                marker("P", &nonce, "/usr/bin/bash")
            } else if line.contains("6973;B;") {
                match answer {
                    Answer::Runs(output) => format!(
                        "{}{output}{}",
                        marker("B", &nonce, ""),
                        marker("E", &nonce, "7")
                    ),
                    Answer::Starts => marker("B", &nonce, ""),
                    Answer::Nothing => continue,
                }
            } else {
                continue;
            };
            handle.feed(reply.as_bytes());
        }
        all
    })
}

#[test]
fn driving_a_run_passes_its_output_on_and_ends_with_its_code() {
    let (run, events, input) = accepted("make");
    let shell = fake_shell(&run, input, Answer::Runs("built\r\n"));
    let mut progress = Vec::new();
    let code = run.drive(&events, QUICK, &mut |step| {
        if step != RunProgress::Idle {
            progress.push(step);
        }
        Ok(())
    });
    assert_eq!(code, Ok(7));
    assert!(matches!(progress[0], RunProgress::Context(_)));
    assert_eq!(progress[1], RunProgress::Output(b"built\n".to_vec()));
    drop(run);
    assert!(!shell.join().unwrap().contains('\x03'));
}

#[test]
fn a_run_nobody_answers_or_that_never_starts_is_busy() {
    let (run, events, _input) = accepted("true");
    assert_eq!(
        run.drive(&events, QUICK, &mut |_| Ok(())),
        Err(RunFailure::Busy(BusyReason::NoAnswer))
    );

    // Held back by a command filter: withdrawn with ^C.
    let (run, events, input) = accepted("reboot");
    let shell = fake_shell(&run, input, Answer::Nothing);
    assert_eq!(
        run.drive(&events, QUICK, &mut |_| Ok(())),
        Err(RunFailure::Busy(BusyReason::NotStarted))
    );
    drop(run);
    assert!(shell.join().unwrap().ends_with('\x03'));
}

#[test]
fn a_caller_gone_cancels_the_command() {
    let (run, events, input) = accepted("sleep 100");
    let shell = fake_shell(&run, input, Answer::Starts);
    let mut begun = false;
    let result = run.drive(&events, QUICK, &mut |step| match step {
        RunProgress::Context(_) => {
            begun = true;
            Ok(())
        }
        RunProgress::Idle if begun => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        _ => Ok(()),
    });
    assert_eq!(result, Err(RunFailure::Disconnected));
    drop(run);
    assert!(shell.join().unwrap().ends_with('\x03'));
}

#[test]
fn the_encoded_command_has_nothing_a_line_editor_or_quotes_would_take() {
    let encoded = encode_command("echo 'it''s' \"$HOME\" a\\b !x\t中\n".as_bytes());
    assert!(encoded.bytes().all(|byte| (0x20..0x7f).contains(&byte)));
    assert!(!encoded.contains(['\'', '"', '!']));
    assert!(!encoded.contains("\\\\"));
    assert!(encoded.starts_with("echo \\0047it"), "{encoded}");
    assert!(encoded.contains("$HOME"));
}

/// The scripts run for real by this machine's `sh`.
#[cfg(unix)]
mod sh {
    use super::*;
    use crate::testing::{sh, sh_accepts};

    #[test]
    fn the_fixed_scripts_are_plain_sh() {
        assert!(sh_accepts(PROBE_SCRIPT));
        assert!(sh_accepts(COMMAND_SCRIPT));
        for script in [PROBE_SCRIPT, COMMAND_SCRIPT] {
            assert!(!script.contains(['\'', '!']) && !script.contains("\\\\"));
        }
    }

    #[test]
    fn printf_b_reads_every_byte_back() {
        let bytes: Vec<u8> = (0..=255).collect();
        let output = sh(
            &format!("printf %b '{}' | od -An -tx1", encode_command(&bytes)),
            None,
        );
        let dumped: String = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .collect();
        let expected: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(dumped, expected);
    }

    /// The command line as the interactive shell would run it, its output
    /// read back by the scanner.
    fn run_line(command: &str) -> (Vec<Scanned>, String) {
        let nonce = "Nonce0123456";
        let line = command_line(nonce, command);
        let output = sh(line.trim_start(), None);
        let mut scanner = RunScanner::new(nonce);
        (scanner.scan(&output.stdout), line)
    }

    #[test]
    fn the_command_line_runs_any_command_and_reports_its_status() {
        let (found, _) =
            run_line("printf '%s\\n' \"it's\" 'a\\b' '$HOME' '!x' 中文; echo err >&2; exit 3");
        let [
            Scanned::Begin(context),
            Scanned::Output(output),
            Scanned::End(code),
        ] = found.as_slice()
        else {
            panic!("{found:?}");
        };
        assert_eq!(
            String::from_utf8_lossy(output),
            "it's\na\\b\n$HOME\n!x\n中文\nerr\n"
        );
        assert_eq!(*code, 3);
        let context = RunContext::read(context, "bash");
        assert!(!context.user.is_empty() && context.cwd.starts_with('/'));
        assert!(["bash", "sh"].contains(&context.interp.as_str()));

        // Several lines, and a command killed by a signal.
        let (found, _) = run_line("cd /\npwd\nkill -TERM $$");
        assert_eq!(found[1], Scanned::Output(b"/\n".to_vec()));
        assert_eq!(found[2], Scanned::End(143));
        let (found, _) = run_line("kill -INT $$");
        assert_eq!(found.last(), Some(&Scanned::End(130)));
    }
}

/// The run against real shells in containers, on a real terminal: what
/// the tests above cannot show. Needs Docker and the images; run with
/// `cargo test --lib terminal::run -- --ignored` (CI: `shells.yml`).
#[cfg(unix)]
mod shells {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use super::*;
    use crate::testing::{Pty, pty};

    /// A shell in a container, waited for until it shows its prompt.
    fn start(image: &str, shell: &[&str]) -> Pty {
        let mut argv = vec!["docker", "run", "--rm", "-i", "-t", image];
        argv.extend_from_slice(shell);
        let pty = pty(&argv);
        pty.output
            .recv_timeout(Duration::from_secs(60))
            .expect("the shell starts");
        settle(&pty);
        pty
    }

    /// Wait until the terminal has been quiet a while.
    fn settle(pty: &Pty) {
        while pty.output.recv_timeout(Duration::from_millis(500)).is_ok() {}
    }

    struct Outcome {
        result: Result<i32, RunFailure>,
        output: String,
        context: Option<RunContext>,
    }

    /// Run `command` the way the app does: ^C, then the run. `interrupt`
    /// presses ^C that long after the command started, as the user would.
    fn run(pty: Pty, command: &str, interrupt: Option<Duration>) -> (Pty, Outcome) {
        let (run, events) = TerminalRun::new(command.to_owned());
        let handle = run.handle();
        let (input, typed) = mpsc::channel();
        assert!(handle.accept(input.clone()));
        input
            .send(TerminalTransportCommand::Write(b"\x03".to_vec()))
            .unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let pump = {
            let done = done.clone();
            let handle = handle.clone();
            std::thread::spawn(move || {
                let mut pty = pty;
                let mut begun_at = None;
                while !done.load(Ordering::Acquire) {
                    while let Ok(TerminalTransportCommand::Write(bytes)) = typed.try_recv() {
                        pty.write(&bytes);
                    }
                    if let Ok(bytes) = pty.output.recv_timeout(Duration::from_millis(10)) {
                        let was_begun = lock(&handle.state).phase == Phase::Begun;
                        handle.feed(&bytes);
                        if !was_begun && lock(&handle.state).phase == Phase::Begun {
                            begun_at = Some(Instant::now());
                        }
                    }
                    if let (Some(after), Some(at)) = (interrupt, begun_at)
                        && at.elapsed() >= after
                    {
                        pty.write(b"\x03");
                        begun_at = None;
                    }
                }
                pty
            })
        };
        let (mut output, mut context) = (Vec::new(), None);
        let timing = RunTiming {
            probe: Duration::from_secs(10),
            tick: Duration::from_millis(100),
            ..RunTiming::STANDARD
        };
        let result = run.drive(&events, timing, &mut |step| {
            match step {
                RunProgress::Context(found) => context = Some(found),
                RunProgress::Output(bytes) => output.extend(bytes),
                RunProgress::Idle => {}
            }
            Ok(())
        });
        drop(run);
        done.store(true, Ordering::Release);
        let pty = pump.join().unwrap();
        settle(&pty);
        let output = String::from_utf8(output).unwrap();
        (
            pty,
            Outcome {
                result,
                output,
                context,
            },
        )
    }

    fn dump(text: &str) -> String {
        format!("od -An -tx1 <<'SHELLRS_END'\n{text}\nSHELLRS_END")
    }

    /// Every ASCII character but NUL (which no shell string holds),
    /// characters whose UTF-8 has every kind of lead and continuation
    /// byte, and random ones, through a heredoc into `od`: in as many runs
    /// as the shell's line takes.
    fn round_trip(mut pty: Pty, limit: usize) -> Pty {
        let mut text: Vec<char> = (1..=127_u8).map(char::from).collect();
        text.extend("\u{80}\u{7ff}\u{800}中文\u{ffff}\u{10000}😀\u{10ffff}".chars());
        let mut random = rand::rng();
        text.extend(
            std::iter::repeat_with(|| random.random::<char>())
                .filter(|character| *character != '\0')
                .take(100),
        );
        let fits = |piece: &str| command_line("Nonce0123456", &dump(piece)).len() < limit;
        let mut rest = text.as_slice();
        while !rest.is_empty() {
            let length = (1..=rest.len())
                .take_while(|&length| fits(&rest[..length].iter().collect::<String>()))
                .last()
                .expect("one character fits");
            let piece: String = rest[..length].iter().collect();
            rest = &rest[length..];
            let (next, outcome) = run(pty, &dump(&piece), None);
            pty = next;
            assert_eq!(outcome.result, Ok(0), "{piece:?}: {}", outcome.output);
            let dumped: String = outcome.output.split_whitespace().collect();
            assert_eq!(dumped, hex(&format!("{piece}\n")), "{piece:?}");
        }
        pty
    }

    /// The longest command that fits `limit`, and one byte more.
    fn at_limit(pty: Pty, limit: usize) -> Pty {
        let fill = |length: usize| format!("echo {}", "x".repeat(length));
        let base = command_line("Nonce0123456", &fill(0)).len() + 1;
        let (pty, outcome) = run(pty, &fill(limit - base), None);
        assert_eq!(outcome.result, Ok(0));
        assert_eq!(outcome.output.trim_end().len(), limit - base);
        let (pty, outcome) = run(pty, &fill(limit - base + 1), None);
        assert_eq!(outcome.result, Err(RunFailure::TooLong { limit }));
        pty
    }

    /// Whatever has the terminal, ^C hands it back.
    fn takes_over(mut pty: Pty) -> Pty {
        for busy in [
            "sleep 100\r",
            "cat\r",
            "read -p 'Password: ' secret\r",
            "echo partial",
        ] {
            pty.write(busy.as_bytes());
            settle(&pty);
            let (next, outcome) = run(pty, "echo ok", None);
            pty = next;
            assert_eq!(outcome.result, Ok(0), "{busy:?}");
            assert_eq!(outcome.output, "ok\n", "{busy:?}");
        }
        pty
    }

    fn interrupted(pty: Pty) -> Pty {
        let (pty, outcome) = run(pty, "sleep 30", Some(Duration::from_secs(2)));
        assert_eq!(outcome.result, Ok(130));
        pty
    }

    fn check(image: &str, shell: &[&str], expected: &str, interp: &str) {
        let (pty, outcome) = run(start(image, shell), "id -un", None);
        assert_eq!(outcome.result, Ok(0), "{image}: {}", outcome.output);
        let context = outcome.context.expect("context");
        assert_eq!(context.shell, expected, "{image}");
        assert_eq!(context.interp, interp, "{image}");
        assert_eq!((context.user.as_str(), context.cwd.as_str()), ("root", "/"));
        assert_eq!(outcome.output, "root\n");
        let limit = RunShell::of(expected).unwrap().line_limit();
        let pty = round_trip(pty, limit);
        let pty = at_limit(pty, limit);
        let pty = takes_over(pty);
        interrupted(pty);
    }

    #[test]
    #[ignore = "needs Docker"]
    fn bash_4_2() {
        check("bash:4.2", &["bash"], "bash", "bash");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn bash_4_4() {
        check("bash:4.4", &["bash"], "bash", "bash");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn bash_5_0() {
        check("bash:5.0", &["bash"], "bash", "bash");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn bash_5_1() {
        check("bash:5.1", &["bash"], "bash", "bash");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn bash_5_2() {
        check("bash:5.2", &["bash"], "bash", "bash");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn bash_started_as_sh() {
        check("rockylinux:9", &["sh"], "bash", "bash");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn dash() {
        check("debian:stable-slim", &["sh"], "dash", "bash");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn busybox_ash() {
        check("alpine:3", &["sh"], "busybox", "sh");
    }

    #[test]
    #[ignore = "needs Docker"]
    fn another_shell_is_refused() {
        // A shell program by another name: bash, which runs whatever its
        // file is called.
        let shell = start(
            "bash:5.2",
            &[
                "sh",
                "-c",
                "cp /usr/local/bin/bash /tmp/mysh && exec /tmp/mysh",
            ],
        );
        let (_, outcome) = run(shell, "true", None);
        assert_eq!(
            outcome.result,
            Err(RunFailure::UnsupportedShell("mysh".into()))
        );
    }
}
