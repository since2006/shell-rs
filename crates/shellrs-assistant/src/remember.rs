//! What the user said not to ask about again in a conversation, with
//! 「执行，本对话不再问」: a command's kinds (its programs, as
//! `command_kinds` tells them: approving `systemctl restart nginx` lets every
//! `systemctl …` run), the command as written when its kinds cannot be told,
//! and a file read by its path.

use std::collections::HashSet;

use crate::danger::command_kinds;
use crate::tools::ToolCall;

#[derive(Clone, Debug, Default)]
pub struct Remembered {
    keys: HashSet<String>,
}

impl Remembered {
    /// Not to ask about `call`, and the calls of its kind, again.
    pub fn remember(&mut self, call: &ToolCall) {
        match call {
            ToolCall::RunCommand { command, .. } => match command_kinds(command) {
                Some(kinds) => self.keys.extend(kinds.iter().map(|kind| kind_key(kind))),
                None => {
                    self.keys.insert(command_key(command));
                }
            },
            ToolCall::ReadFile { path } => {
                self.keys.insert(file_key(path));
            }
            ToolCall::ReadTerminal { .. } => {}
        }
    }

    /// Whether `call` runs without asking: every kind of command it runs
    /// was remembered, or the command as written, or the file.
    pub fn covers(&self, call: &ToolCall) -> bool {
        match call {
            ToolCall::RunCommand { command, .. } => {
                self.keys.contains(&command_key(command))
                    || command_kinds(command).is_some_and(|kinds| {
                        kinds.iter().all(|kind| self.keys.contains(&kind_key(kind)))
                    })
            }
            ToolCall::ReadFile { path } => self.keys.contains(&file_key(path)),
            ToolCall::ReadTerminal { .. } => false,
        }
    }
}

fn kind_key(kind: &str) -> String {
    format!("kind\0{kind}")
}

fn command_key(command: &str) -> String {
    format!("run\0{command}")
}

fn file_key(path: &str) -> String {
    format!("read\0{path}")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn run(command: &str) -> ToolCall {
        ToolCall::RunCommand {
            command: command.into(),
            cwd: None,
            timeout: Duration::from_secs(60),
        }
    }

    fn remembered(command: &str) -> Remembered {
        let mut remembered = Remembered::default();
        remembered.remember(&run(command));
        remembered
    }

    #[test]
    fn a_command_of_a_remembered_kind_runs_unasked() {
        let remembered = remembered("systemctl restart nginx");
        assert!(remembered.covers(&run("systemctl restart nginx")));
        assert!(remembered.covers(&run("systemctl status php-fpm --no-pager")));
        assert!(remembered.covers(&run("cd /etc && systemctl reload nginx")));
        assert!(!remembered.covers(&run("journalctl -u nginx")));
        // Every part must be of a remembered kind.
        assert!(!remembered.covers(&run("systemctl status nginx | grep active")));
        // As another user, it is another kind.
        assert!(!remembered.covers(&run("sudo systemctl restart nginx")));
    }

    #[test]
    fn each_part_of_a_command_is_remembered() {
        let remembered = remembered("ps aux | grep nginx");
        assert!(remembered.covers(&run("ps -ef")));
        assert!(remembered.covers(&run("grep -r listen /etc/nginx")));
        assert!(remembered.covers(&run("ps aux | grep -v grep | grep php")));
    }

    #[test]
    fn sudo_is_remembered_with_what_it_runs() {
        let remembered = remembered("sudo -n systemctl restart nginx");
        assert!(remembered.covers(&run("sudo -n systemctl stop php-fpm")));
        assert!(remembered.covers(&run("sudo systemctl status nginx")));
        assert!(!remembered.covers(&run("sudo -n rm -rf /var/log/old")));
        assert!(!remembered.covers(&run("systemctl restart nginx")));
    }

    #[test]
    fn code_handed_to_a_shell_is_remembered_only_as_written() {
        let remembered = remembered("bash -c 'df -h'");
        assert!(remembered.covers(&run("bash -c 'df -h'")));
        assert!(!remembered.covers(&run("bash -c 'rm -rf /'")));
        let remembered = self::remembered("echo $(cat /etc/hostname)");
        assert!(!remembered.covers(&run("echo $(cat /etc/shadow)")));
    }

    #[test]
    fn a_file_is_remembered_by_its_path() {
        let mut remembered = Remembered::default();
        remembered.remember(&ToolCall::ReadFile {
            path: "/etc/nginx/nginx.conf".into(),
        });
        assert!(remembered.covers(&ToolCall::ReadFile {
            path: "/etc/nginx/nginx.conf".into()
        }));
        assert!(!remembered.covers(&ToolCall::ReadFile {
            path: "/etc/nginx/conf.d/site.conf".into()
        }));
    }
}
