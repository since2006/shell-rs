//! A tiny canned shell so the terminal mock feels alive.

use gpui_kit::SharedString;

use crate::session::Session;

/// What a command asks the terminal to do besides printing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellEffect {
    /// Clear the scrollback.
    Clear,
    /// End the session.
    Exit,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellReply {
    pub lines: Vec<SharedString>,
    pub effect: Option<ShellEffect>,
}

impl ShellReply {
    fn lines<I, S>(lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        Self {
            lines: lines.into_iter().map(Into::into).collect(),
            effect: None,
        }
    }
}

/// `root@web-01:~$ `
pub fn prompt(session: &Session) -> String {
    let symbol = if session.user.as_ref() == "root" {
        "#"
    } else {
        "$"
    };
    format!("{}@{}:~{symbol} ", session.user, session.name)
}

/// The lines shown when a terminal opens.
pub fn banner(session: &Session) -> Vec<SharedString> {
    vec![
        format!("Connecting to {}...", session.address()).into(),
        "Welcome to Ubuntu 24.04 LTS (GNU/Linux 6.8.0-45-generic x86_64)".into(),
        "".into(),
        " * Documentation:  https://help.ubuntu.com".into(),
        " * Support:        https://ubuntu.com/pro".into(),
        "".into(),
        format!(
            "Last login: Wed Sep  9 18:42:07 2026 from 10.0.0.2 ({})",
            session.host
        )
        .into(),
    ]
}

/// The canned reply to a command line.
pub fn reply(session: &Session, command: &str) -> ShellReply {
    let command = command.trim();
    let (program, rest) = command.split_once(' ').unwrap_or((command, ""));
    match program {
        "" => ShellReply::default(),
        "clear" => ShellReply {
            lines: Vec::new(),
            effect: Some(ShellEffect::Clear),
        },
        "exit" | "logout" => ShellReply {
            lines: vec!["logout".into()],
            effect: Some(ShellEffect::Exit),
        },
        "ls" => ShellReply::lines(["backups  deploy.sh"]),
        "pwd" => ShellReply::lines([format!("/home/{}", session.user)]),
        "whoami" => ShellReply::lines([session.user.to_string()]),
        "hostname" => ShellReply::lines([session.name.to_string()]),
        "uname" => ShellReply::lines([if rest.contains('a') {
            format!(
                "Linux {} 6.8.0-45-generic #45-Ubuntu SMP x86_64 GNU/Linux",
                session.name
            )
        } else {
            "Linux".to_string()
        }]),
        "uptime" => ShellReply::lines([
            " 20:41:13 up 37 days,  4:12,  1 user,  load average: 0.08, 0.11, 0.09",
        ]),
        "date" => ShellReply::lines(["Thu Sep 10 20:41:13 CST 2026"]),
        "echo" => ShellReply::lines([rest.to_string()]),
        "help" => ShellReply::lines([
            "mock shell — 可用命令: ls pwd whoami hostname uname [-a] uptime date echo clear exit",
        ]),
        _ => ShellReply::lines([format!("bash: {program}: command not found")]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionStore;

    fn session() -> Session {
        SessionStore::seed().sessions()[0].clone()
    }

    #[test]
    fn unknown_command_reports_not_found() {
        let reply = reply(&session(), "frobnicate --now");
        assert_eq!(reply.lines, ["bash: frobnicate: command not found"]);
        assert_eq!(reply.effect, None);
    }

    #[test]
    fn clear_and_exit_carry_effects() {
        assert_eq!(reply(&session(), "clear").effect, Some(ShellEffect::Clear));
        assert_eq!(reply(&session(), "exit").effect, Some(ShellEffect::Exit));
        assert_eq!(reply(&session(), "   ").lines.len(), 0);
    }

    #[test]
    fn prompt_uses_hash_for_root() {
        let session = session();
        assert_eq!(prompt(&session), "root@web-01:~# ");
    }
}
