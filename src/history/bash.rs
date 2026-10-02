//! Reading bash's history file: one command, and the parser for what it
//! prints.

use super::model::History;

/// The most of the history file a reading takes, from its end: thousands
/// of commands, well within what a reading's output may hold. A file kept
/// longer (`HISTFILESIZE` set high) is read for its newest commands.
pub const LIMIT: u64 = 512 * 1024;

/// What 历史命令 runs on the host: how long `~/.bash_history` is, and its
/// last `LIMIT` bytes, each under an `@@` line of its own; `@@missing` when
/// there is no such file to read.
///
/// One line, in single quotes for `sh -c`, so that whatever the login shell
/// is (bash, zsh, fish, csh) it hands the script to `sh` untouched: the
/// script has no single quote, and no `!` for csh to expand.
pub fn command() -> String {
    format!(
        "sh -c 'f=$HOME/.bash_history; if test -r \"$f\"; then \
         echo @@size; wc -c < \"$f\"; echo @@history; tail -c {LIMIT} \"$f\"; \
         else echo @@missing; fi'"
    )
}

/// What the command's output said.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    History(History),
    /// No history file, or not one the login can read.
    Missing,
    /// Not what the command prints: no `sh` there to run it.
    Unsupported,
}

pub fn parse(output: &str) -> Parsed {
    // The history comes last, as it is: a command may start with 「@@」.
    let (head, body) = match output.find("@@history\n") {
        Some(at) if at == 0 || output[..at].ends_with('\n') => {
            (&output[..at], &output[at + "@@history\n".len()..])
        }
        _ => (output, ""),
    };
    let mut lines = head.lines().map(str::trim);
    if !lines.clone().any(|line| line == "@@size") {
        return if head.lines().any(|line| line.trim() == "@@missing") {
            Parsed::Missing
        } else {
            Parsed::Unsupported
        };
    }
    let size = lines
        .find(|line| *line == "@@size")
        .and_then(|_| lines.next())
        .and_then(|size| size.parse::<u64>().ok())
        .unwrap_or_default();
    let truncated = size > LIMIT;
    let mut body = body;
    // Cut off in the middle of a line, which is not a command.
    if truncated {
        body = body.split_once('\n').map_or("", |(_, rest)| rest);
    }
    Parsed::History(History::new(commands(body), truncated))
}

/// The commands of a history file, the oldest first, each with when it
/// ran when bash kept that.
///
/// With `HISTTIMEFORMAT` set, bash writes 「#1727846400」 before each
/// command, and a command of several lines as its lines; the lines up to
/// the next time are one command, the way bash reads them back. Without
/// times, every line is a command.
fn commands(body: &str) -> Vec<(String, Option<i64>)> {
    let mut commands: Vec<(String, Option<i64>)> = Vec::new();
    let mut time = None;
    // Whether the last command came after a time, so a line with no time
    // of its own belongs to it.
    let mut timed = false;
    for line in body.lines() {
        if let Some(seconds) = timestamp(line) {
            time = Some(seconds);
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        match (time.take(), commands.last_mut()) {
            (None, Some((command, _))) if timed => {
                command.push('\n');
                command.push_str(line);
            }
            (at, _) => {
                timed = at.is_some();
                commands.push((line.to_owned(), at));
            }
        }
    }
    commands
}

/// The time bash writes before a command: 「#」 and the seconds since 1970.
fn timestamp(line: &str) -> Option<i64> {
    let digits = line.strip_prefix('#')?.trim_end();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(parsed: Parsed) -> Vec<(String, Option<i64>, usize)> {
        let Parsed::History(history) = parsed else {
            panic!("no history: {parsed:?}");
        };
        history
            .entries()
            .iter()
            .map(|entry| (entry.command.clone(), entry.last_run, entry.runs))
            .collect()
    }

    fn entry(command: &str, last_run: Option<i64>, runs: usize) -> (String, Option<i64>, usize) {
        (command.to_owned(), last_run, runs)
    }

    #[test]
    fn every_line_is_a_command_the_newest_first_once_each() {
        let output = "@@size\n38\n@@history\nls\ncd /var/log\nls\ntail -f syslog\n\n";
        assert_eq!(
            listed(parse(output)),
            [
                entry("tail -f syslog", None, 1),
                entry("ls", None, 2),
                entry("cd /var/log", None, 1),
            ]
        );
    }

    #[test]
    fn the_times_bash_keeps_go_with_their_commands() {
        let output = "@@size\n80\n@@history\n\
#1727846400\nls\n\
#1727846460\nfor f in *.log\ndo gzip $f\ndone\n\
#1727846520\nls\n";
        assert_eq!(
            listed(parse(output)),
            [
                entry("ls", Some(1727846520), 2),
                entry("for f in *.log\ndo gzip $f\ndone", Some(1727846460), 1),
            ]
        );
    }

    #[test]
    fn commands_from_before_the_times_stay_lines_of_their_own() {
        let output = "@@size\n40\n@@history\nuptime\ndf -h\n#1727846400\nfree -m\n";
        assert_eq!(
            listed(parse(output)),
            [
                entry("free -m", Some(1727846400), 1),
                entry("df -h", None, 1),
                entry("uptime", None, 1),
            ]
        );
        // A comment is a command, not a time.
        let output = "@@size\n20\n@@history\n# backup\n#12a\n";
        assert_eq!(
            listed(parse(output)),
            [entry("#12a", None, 1), entry("# backup", None, 1)]
        );
    }

    #[test]
    fn a_file_read_from_its_middle_starts_at_a_whole_line() {
        let output = format!("@@size\n{}\n@@history\nt -lh\nps aux\n", LIMIT + 10);
        let Parsed::History(history) = parse(&output) else {
            panic!("no history");
        };
        assert!(history.truncated());
        assert_eq!(history.entries()[0].command, "ps aux");
        assert_eq!(history.entries().len(), 1);
    }

    #[test]
    fn a_command_may_look_like_a_section() {
        let output = "@@size\n20\n@@history\necho @@size\n@@history\n";
        assert_eq!(
            listed(parse(output)),
            [entry("@@history", None, 1), entry("echo @@size", None, 1)]
        );
    }

    #[test]
    fn without_a_file_or_sh_it_says_so() {
        assert_eq!(parse("@@missing\n"), Parsed::Missing);
        assert_eq!(parse(""), Parsed::Unsupported);
        assert_eq!(
            parse("'sh' is not recognized as an internal or external command\r\n"),
            Parsed::Unsupported
        );
        // An empty file is a history with nothing in it.
        assert_eq!(listed(parse("@@size\n0\n@@history\n")), []);
    }

    #[test]
    fn the_command_survives_any_login_shell() {
        let command = command();
        let script = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .expect("one sh -c in single quotes");
        assert!(!script.contains('\''));
        assert!(!script.contains('!'));
        assert!(!script.contains('\n'));
        assert!(!script.contains("\\\\"));
        assert!(crate::testing::sh_accepts(script));
    }

    /// The script, run by this machine's `sh` on a history file in a
    /// directory standing in for the home, gives what the parser reads;
    /// a file longer than `LIMIT` is read for its end.
    #[cfg(unix)]
    #[test]
    fn the_script_reads_the_end_of_the_history_file() {
        let home = tempfile::tempdir().expect("temp dir");
        let command = command();
        let script = command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap()
            .replace("$HOME", &home.path().display().to_string());
        let run = || {
            let output = crate::testing::sh(&script, None);
            assert!(output.status.success(), "{output:?}");
            parse(&String::from_utf8(output.stdout).expect("utf-8"))
        };
        assert_eq!(run(), Parsed::Missing);

        let file = home.path().join(".bash_history");
        std::fs::write(&file, "#1727846400\nls -l\n#1727846460\ngit status\n").unwrap();
        assert_eq!(
            listed(run()),
            [
                entry("git status", Some(1727846460), 1),
                entry("ls -l", Some(1727846400), 1),
            ]
        );

        let old = "make\n".repeat(LIMIT as usize / 5 + 100);
        std::fs::write(&file, format!("{old}cargo build --release\n")).unwrap();
        let Parsed::History(history) = run() else {
            panic!("no history");
        };
        assert!(history.truncated());
        assert_eq!(history.entries()[0].command, "cargo build --release");
        assert_eq!(history.entries()[1].command, "make");
    }
}
