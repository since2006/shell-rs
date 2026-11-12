//! Reading and running a host's tmux: the commands, what is typed into the
//! terminal, and the parser for what the reading prints.

use std::collections::HashMap;

use super::model::{TmuxSession, TmuxTable, valid_id, valid_name, valid_tty};
use crate::i18n::t;

/// Where `tmux` may be besides the login's `PATH`: built by hand, or by
/// Homebrew on a Mac.
const TMUX_PATH: &str = "export PATH=$PATH:/usr/local/bin:/opt/homebrew/bin; ";

/// What the tmux tool runs on the host for a reading: tmux's version, the
/// SSH terminal's tty, the clients attached to sessions, then the sessions,
/// each part under an `@@` line of its own, and how listing the sessions
/// exited. Only `@@missing` without tmux.
///
/// The terminal's tty comes from the connection's own `sshd`: it started
/// both the terminal's shell and this command, and of its children only
/// the shell has a terminal. Up from this shell to the first `sshd` (or
/// Dropbear), then its child with a tty. The client on that tty, if any, is
/// the terminal's, and its session the one the terminal is in.
///
/// One line, in single quotes for `sh -c`, so that whatever the login shell
/// is (bash, zsh, fish, csh) it hands the script to `sh` untouched: the
/// script has no single quote and no `!` for csh to expand.
pub fn command() -> String {
    format!(
        "sh -c '{TMUX_PATH}if command -v tmux >/dev/null 2>&1; then \
         echo @@version; tmux -V 2>&1; \
         p=$$; while test \"$p\" -gt 1 2>/dev/null; do \
         case $(ps -o comm= -p \"$p\" 2>/dev/null) in *sshd*|*dropbear*) break;; esac; \
         p=$(ps -o ppid= -p \"$p\" 2>/dev/null | tr -d \" \"); done; \
         echo @@tty; if test \"$p\" -gt 1 2>/dev/null; then \
         ps -e -o ppid= -o tty= 2>/dev/null | while read q t; do \
         if test \"$q\" = \"$p\"; then case $t in \"?\"*|-|\"\") ;; *) echo \"$t\";; esac; fi; done; fi; \
         echo @@clients; tmux -u list-clients -F \"#{{client_tty}} #{{session_id}}\" 2>/dev/null; \
         echo @@sessions; tmux -u list-sessions \
         -F \"#{{session_id}}:#{{session_attached}}:#{{session_windows}}:#{{session_name}}:#{{session_path}}\" 2>&1; \
         echo \"@@status $?\"; \
         else echo @@missing; fi'"
    )
}

/// `script`, saying how it went in an `@@status` line with tmux's
/// complaint before it.
fn with_status(script: &str) -> String {
    format!("sh -c '{TMUX_PATH}{script} 2>&1; echo @@status $?'")
}

/// What moves the terminal's `client` over to the session `id`.
pub fn switch_command(client: &str, id: &str) -> Option<String> {
    (valid_tty(client) && valid_id(id))
        .then(|| with_status(&format!("tmux switch-client -c {client} -t \\{id}")))
}

/// What starts a session called `name`, or numbered by tmux, and moves the
/// terminal's `client` over to it.
pub fn new_command(name: Option<&str>, client: &str) -> Option<String> {
    let name = match name {
        Some(name) if !valid_name(name) => return None,
        Some(name) => format!(" -s {name}"),
        None => String::new(),
    };
    valid_tty(client).then(|| {
        with_status(&format!(
            "if id=$(tmux new-session -d{name} -P -F \"#{{session_id}}\" 2>&1); \
             then tmux switch-client -c {client} -t \"$id\"; else echo \"$id\"; false; fi"
        ))
    })
}

/// What detaches the terminal's `client`, leaving its session running.
pub fn detach_command(client: &str) -> Option<String> {
    valid_tty(client).then(|| with_status(&format!("tmux detach-client -t {client}")))
}

/// What gives the session `id` the name `name`.
pub fn rename_command(id: &str, name: &str) -> Option<String> {
    (valid_id(id) && valid_name(name))
        .then(|| with_status(&format!("tmux rename-session -t \\{id} {name}")))
}

/// What ends the session `id` and every program in it.
pub fn kill_command(id: &str) -> Option<String> {
    valid_id(id).then(|| with_status(&format!("tmux kill-session -t \\{id}")))
}

/// What is typed into a terminal not in tmux to attach it to `session`:
/// by name when the name can be typed as it is, else by id, in single
/// quotes, which every login shell keeps as they are.
pub fn attach_typed(session: &TmuxSession) -> String {
    if valid_name(&session.name) {
        format!("tmux attach -t {}", session.name)
    } else {
        format!("tmux attach -t '{}'", session.id)
    }
}

/// What is typed into a terminal not in tmux to start a session called
/// `name`, or numbered by tmux, there.
pub fn new_typed(name: Option<&str>) -> String {
    match name {
        Some(name) => format!("tmux new -s {name}"),
        None => "tmux new".to_owned(),
    }
}

/// How a command went, from what `with_status` printed: why not, when it
/// did not.
pub fn done(output: &str) -> Result<(), String> {
    let (said, status) = output
        .rsplit_once("@@status")
        .ok_or_else(|| t!("tools.no_answer").to_string())?;
    match status.trim() {
        "0" => Ok(()),
        status => match said.lines().map(str::trim).find(|line| !line.is_empty()) {
            Some(line) => Err(line.to_owned()),
            None => Err(t!("tmux.error.status", status = status).into()),
        },
    }
}

/// What the reading's command printed.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Table(TmuxTable),
    /// No `tmux` on the host.
    Missing,
    /// tmux would not list its sessions: what it said.
    Failed(String),
    /// Nothing we know: not a shell that runs `sh` (cmd.exe).
    Unsupported,
}

/// What tmux says when no session has been started since the host booted,
/// or since the last one ended: no server, so no sessions.
const NO_SERVER: [&str; 3] = [
    "no server running",
    "error connecting to",
    "failed to connect to server",
];

pub fn parse(output: &str) -> Parsed {
    let mut sections: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current = None;
    let mut status = None;
    for line in output.lines() {
        if let Some(code) = line.strip_prefix("@@status") {
            status = code.trim().parse::<i32>().ok();
            current = None;
        } else if let Some(name) = line.strip_prefix("@@") {
            current = Some(name.trim());
            sections.entry(name.trim()).or_default();
        } else if let Some(name) = current {
            sections.entry(name).or_default().push(line);
        }
    }
    if sections.contains_key("missing") {
        return Parsed::Missing;
    }
    let (Some(lines), Some(status)) = (sections.get("sessions"), status) else {
        return Parsed::Unsupported;
    };
    let section = |name: &str| sections.get(name).map(Vec::as_slice).unwrap_or_default();
    let first = |name: &str| {
        section(name)
            .iter()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
    };
    let sessions = if status == 0 {
        lines.iter().filter_map(|line| session(line)).collect()
    } else {
        let said = lines.join("\n");
        if !NO_SERVER.iter().any(|words| said.contains(words)) {
            return Parsed::Failed(match first("sessions") {
                Some(line) => line.to_owned(),
                None => t!("tmux.error.status", status = status).into(),
            });
        }
        Vec::new()
    };
    let tty = first("tty").map(|tty| format!("/dev/{tty}"));
    let current = tty.as_deref().and_then(|tty| {
        section("clients").iter().find_map(|line| {
            let (client, id) = line.trim().split_once(' ')?;
            (client == tty).then(|| id.trim().to_owned())
        })
    });
    Parsed::Table(TmuxTable {
        version: first("version").unwrap_or("tmux").to_owned(),
        sessions,
        tty,
        current,
    })
}

/// A line of `list-sessions`: id, clients, windows, name and folder, by
/// `:`. A name never has one (tmux makes it `_`); a folder may.
fn session(line: &str) -> Option<TmuxSession> {
    let mut fields = line.splitn(5, ':');
    let id = fields.next()?;
    if !valid_id(id) {
        return None;
    }
    Some(TmuxSession {
        id: id.to_owned(),
        attached: fields.next()?.trim().parse().ok()?,
        windows: fields.next()?.trim().parse().ok()?,
        name: fields.next()?.to_owned(),
        path: fields.next().unwrap_or_default().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const READING: &str = "\
@@version
tmux 3.2a
@@tty
pts/3
@@clients
/dev/pts/5 $2
/dev/pts/3 $1
@@sessions
$1:2:3:test:/root
$2:1:1:work:/srv/a:b
$3:0:1:my job:/home/alice
@@status 0
";

    fn table(output: &str) -> TmuxTable {
        match parse(output) {
            Parsed::Table(table) => table,
            other => panic!("not a table: {other:?}"),
        }
    }

    #[test]
    fn a_reading_gives_the_sessions_and_the_one_the_terminal_is_in() {
        let read = table(READING);
        assert_eq!(read.version, "tmux 3.2a");
        assert_eq!(read.tty.as_deref(), Some("/dev/pts/3"));
        assert_eq!(read.current.as_deref(), Some("$1"));
        assert_eq!(read.client(), Some("/dev/pts/3"));
        assert_eq!(
            read.sessions,
            [
                TmuxSession {
                    id: "$1".into(),
                    name: "test".into(),
                    attached: 2,
                    windows: 3,
                    path: "/root".into(),
                },
                TmuxSession {
                    id: "$2".into(),
                    name: "work".into(),
                    attached: 1,
                    windows: 1,
                    // A folder may have a colon in it.
                    path: "/srv/a:b".into(),
                },
                TmuxSession {
                    id: "$3".into(),
                    name: "my job".into(),
                    attached: 0,
                    windows: 1,
                    path: "/home/alice".into(),
                },
            ]
        );
    }

    #[test]
    fn a_terminal_not_in_tmux_or_not_found_has_no_session() {
        // Its tty, but no client there.
        let outside = table(&READING.replace("pts/3\n@@clients", "pts/9\n@@clients"));
        assert_eq!(outside.tty.as_deref(), Some("/dev/pts/9"));
        assert_eq!(outside.current, None);
        assert_eq!(outside.client(), None);
        // No tty found: no `sshd` up from the command, or no `ps`.
        let unknown = table(&READING.replace("@@tty\npts/3\n", "@@tty\n"));
        assert_eq!((unknown.tty, unknown.current), (None, None));
    }

    #[test]
    fn no_server_is_no_sessions_and_another_complaint_is_a_failure() {
        for said in [
            "no server running on /tmp/tmux-0/default",
            "error connecting to /tmp/tmux-1000/default (No such file or directory)",
        ] {
            let empty = table(&format!(
                "@@version\ntmux 3.4\n@@tty\npts/0\n@@clients\n@@sessions\n{said}\n@@status 1\n"
            ));
            assert!(empty.sessions.is_empty(), "{said}");
            assert_eq!(empty.current, None);
        }
        assert_eq!(
            parse(
                "@@version\ntmux 3.4\n@@tty\n@@clients\n@@sessions\n\
                 tmux: need UTF-8 locale (LC_CTYPE) but have ANSI_X3.4-1968\n@@status 1\n"
            ),
            Parsed::Failed("tmux: need UTF-8 locale (LC_CTYPE) but have ANSI_X3.4-1968".into())
        );
        assert_eq!(parse("@@missing\n"), Parsed::Missing);
        assert_eq!(parse(""), Parsed::Unsupported);
        assert_eq!(parse("'sh' is not recognized"), Parsed::Unsupported);
    }

    #[test]
    fn commands_name_sessions_by_id_and_refuse_what_is_not_one() {
        assert_eq!(
            switch_command("/dev/pts/3", "$2").as_deref(),
            Some(
                "sh -c 'export PATH=$PATH:/usr/local/bin:/opt/homebrew/bin; \
                 tmux switch-client -c /dev/pts/3 -t \\$2 2>&1; echo @@status $?'"
            )
        );
        assert!(
            rename_command("$3", "train")
                .unwrap()
                .contains("tmux rename-session -t \\$3 train 2>&1")
        );
        assert!(
            kill_command("$3")
                .unwrap()
                .contains("tmux kill-session -t \\$3 2>&1")
        );
        assert!(
            detach_command("/dev/pts/3")
                .unwrap()
                .contains("tmux detach-client -t /dev/pts/3 2>&1")
        );
        assert!(
            new_command(Some("build"), "/dev/pts/3")
                .unwrap()
                .contains("tmux new-session -d -s build -P")
        );
        assert!(
            new_command(None, "/dev/pts/3")
                .unwrap()
                .contains("tmux new-session -d -P")
        );
        assert_eq!(switch_command("/dev/pts/3", "work"), None);
        assert_eq!(switch_command("pts/3", "$2"), None);
        assert_eq!(rename_command("$3", "my job"), None);
        assert_eq!(rename_command("$3", "a;reboot"), None);
        assert_eq!(kill_command("$3;reboot"), None);
        assert_eq!(new_command(Some("a b"), "/dev/pts/3"), None);
        assert_eq!(detach_command("/dev/pts/3;x"), None);
    }

    #[test]
    fn what_is_typed_names_a_session_as_a_shell_reads_it() {
        let session = |id: &str, name: &str| TmuxSession {
            id: id.into(),
            name: name.into(),
            attached: 0,
            windows: 1,
            path: String::new(),
        };
        assert_eq!(attach_typed(&session("$2", "work")), "tmux attach -t work");
        assert_eq!(
            attach_typed(&session("$3", "my job")),
            "tmux attach -t '$3'"
        );
        assert_eq!(new_typed(Some("build")), "tmux new -s build");
        assert_eq!(new_typed(None), "tmux new");
    }

    #[test]
    fn done_says_why_not() {
        assert_eq!(done("@@status 0\n"), Ok(()));
        assert_eq!(
            done("can't find session: $9\n@@status 1\n"),
            Err("can't find session: $9".into())
        );
        assert_eq!(done("@@status 1\n"), Err("tmux 返回 1".into()));
        assert_eq!(done(""), Err("主机没有回答".into()));
    }

    #[test]
    fn the_commands_are_one_line_sh_reads_whatever_the_login_shell() {
        let commands = [
            command(),
            switch_command("/dev/pts/3", "$2").unwrap(),
            new_command(Some("build"), "/dev/pts/3").unwrap(),
            new_command(None, "/dev/pts/3").unwrap(),
            detach_command("/dev/pts/3").unwrap(),
            rename_command("$3", "train").unwrap(),
            kill_command("$3").unwrap(),
        ];
        for command in commands {
            assert!(!command.contains('\n'), "{command}");
            assert_eq!(command.matches('\'').count(), 2, "{command}");
            assert!(!command.contains('!'), "{command}");
            assert!(!command.contains("\\\\"), "{command}");
            #[cfg(unix)]
            assert!(crate::testing::sh_accepts(script(&command)), "{command}");
        }
    }

    #[cfg(unix)]
    fn script(command: &str) -> &str {
        command
            .strip_prefix("sh -c '")
            .and_then(|rest| rest.strip_suffix('\''))
            .unwrap()
    }

    /// The reading, run by this machine's `sh` with `tmux` and `ps` stood
    /// in for: up from the script to `sshd` (4000, two steps up), the
    /// terminal its child with a tty, and that tty's client.
    #[cfg(unix)]
    #[test]
    fn the_script_finds_the_terminal_and_its_session() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().expect("temp dir");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        tool(
            "ps",
            "case \"$*\" in\n\
             \"-o comm= -p 4000\") echo sshd;;\n\
             \"-o comm= -p \"*) echo bash;;\n\
             \"-o ppid= -p 3999\") echo \" 4000\";;\n\
             \"-o ppid= -p \"*) echo \" 3999\";;\n\
             \"-e -o ppid= -o tty=\") printf '%s\\n' \"    1 ?\" \" 4000 ?\" \" 4000 pts/3\" \" 3999 ?\" \" 4100 pts/4\";;\n\
             *) exit 2;;\n\
             esac\n",
        );
        tool(
            "tmux",
            "case \"$*\" in\n\
             -V) echo \"tmux 3.2a\";;\n\
             \"-u list-clients -F #{client_tty} #{session_id}\") printf '%s\\n' \"/dev/pts/4 \\$2\" \"/dev/pts/3 \\$1\";;\n\
             \"-u list-sessions -F #{session_id}:#{session_attached}:#{session_windows}:#{session_name}:#{session_path}\") \
             printf '%s\\n' \"\\$1:1:2:test:/root\" \"\\$2:1:1:work:/root\";;\n\
             *) exit 2;;\n\
             esac\n",
        );
        let path = format!("{}:/usr/bin:/bin", bin.display());
        let output = crate::testing::sh(script(&command()), Some(&path));
        let output = String::from_utf8(output.stdout).unwrap();
        let read = table(&output);
        assert_eq!(read.version, "tmux 3.2a", "{output}");
        assert_eq!(read.tty.as_deref(), Some("/dev/pts/3"), "{output}");
        assert_eq!(read.current.as_deref(), Some("$1"), "{output}");
        assert_eq!(read.names(), ["test", "work"]);

        // No tmux: only this directory on PATH, or a tmux the machine
        // happens to have (the CI images ship one) would answer instead.
        std::fs::remove_file(bin.join("tmux")).unwrap();
        // `sh` is looked up in the PATH the test sets, so it needs one here.
        std::os::unix::fs::symlink("/bin/sh", bin.join("sh")).unwrap();
        let only = bin.display().to_string();
        let output = crate::testing::sh(script(&command()), Some(&only));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "@@missing\n");
    }
}
