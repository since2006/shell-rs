//! 撰写窗格's history: the commands it sent, which ↑ and ↓ go back through
//! as a shell's do, kept in `compose-history.json` for the next launch.
//!
//! As bash keeps it with `HISTCONTROL=ignoreboth`: a command the same as
//! the one before, or one that starts with a space, is not kept. As
//! readline does, what is being written and the changes made to the
//! commands gone back to stay while going through them, until the next
//! send.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many commands are kept, the newest: bash's default `HISTSIZE`.
const LIMIT: usize = 500;

#[derive(Default)]
pub(super) struct ComposeHistory {
    /// What was sent, oldest first.
    commands: Vec<String>,
    /// Which one shows: `commands.len()` for what is being written.
    at: usize,
    /// While going through them, by place: what was being written, and what
    /// the commands gone back to were changed to.
    edits: HashMap<usize, String>,
    /// Where it is kept; none keeps it until quitting.
    path: Option<PathBuf>,
}

/// The file.
#[derive(Serialize, Deserialize)]
struct Saved {
    commands: Vec<String>,
}

impl ComposeHistory {
    /// The history kept at `path`, which keeps it from now on. Empty when
    /// there is none or it does not read: a convenience, as the window's
    /// place is.
    pub(super) fn load(path: PathBuf) -> Self {
        let mut commands = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
            .map(|saved| saved.commands)
            .unwrap_or_default();
        let over = commands.len().saturating_sub(LIMIT);
        commands.drain(..over);
        Self {
            at: commands.len(),
            commands,
            edits: HashMap::new(),
            path: Some(path),
        }
    }

    /// One command back from `shown`, what the pane holds: what to show
    /// instead, none at the oldest.
    pub(super) fn back(&mut self, shown: &str) -> Option<String> {
        let to = self.at.checked_sub(1)?;
        Some(self.go(shown, to))
    }

    /// One command forward from `shown`, the last step to what was being
    /// written: what to show instead, none there.
    pub(super) fn forward(&mut self, shown: &str) -> Option<String> {
        if self.at >= self.commands.len() {
            return None;
        }
        Some(self.go(shown, self.at + 1))
    }

    fn go(&mut self, shown: &str, to: usize) -> String {
        match self.commands.get(self.at) {
            Some(command) if command == shown => {
                self.edits.remove(&self.at);
            }
            _ => {
                self.edits.insert(self.at, shown.to_owned());
            }
        }
        self.at = to;
        self.edits
            .get(&to)
            .or(self.commands.get(to))
            .cloned()
            .unwrap_or_default()
    }

    /// `command` went out: keep it, unless it is empty, the same as the
    /// last, or starts with a space; going through starts again from the
    /// newest.
    pub(super) fn record(&mut self, command: &str) {
        self.edits.clear();
        let kept = !command.trim().is_empty()
            && !command.starts_with(' ')
            && self.commands.last().is_none_or(|last| last != command);
        if kept {
            self.commands.push(command.to_owned());
            let over = self.commands.len().saturating_sub(LIMIT);
            self.commands.drain(..over);
            if let Some(path) = &self.path {
                write(&self.commands, path);
            }
        }
        self.at = self.commands.len();
    }
}

/// Written whole, through a temporary file, as the settings are. A failure
/// is only logged: the history stays until quitting.
fn write(commands: &[String], path: &Path) {
    let written = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let saved = Saved {
            commands: commands.to_vec(),
        };
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(&saved)?)?;
        std::fs::rename(temporary, path)
    })();
    if let Err(error) = written {
        eprintln!("shellrs: cannot save the compose pane's history: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{ComposeHistory, LIMIT};

    fn sent(commands: &[&str]) -> ComposeHistory {
        let mut history = ComposeHistory::default();
        for command in commands {
            history.record(command);
        }
        history
    }

    #[test]
    fn back_goes_to_the_older_commands_and_forward_to_what_was_written() {
        let mut history = sent(&["ls", "pwd"]);
        assert_eq!(history.back("draft").as_deref(), Some("pwd"));
        assert_eq!(history.back("pwd").as_deref(), Some("ls"));
        // The oldest: nothing further back.
        assert_eq!(history.back("ls"), None);
        assert_eq!(history.forward("ls").as_deref(), Some("pwd"));
        assert_eq!(history.forward("pwd").as_deref(), Some("draft"));
        assert_eq!(history.forward("draft"), None);
    }

    #[test]
    fn an_empty_history_goes_nowhere() {
        let mut history = ComposeHistory::default();
        assert_eq!(history.back("draft"), None);
        assert_eq!(history.forward("draft"), None);
    }

    #[test]
    fn changes_to_a_command_gone_back_to_stay_until_the_next_send() {
        let mut history = sent(&["ls", "pwd"]);
        history.back("");
        assert_eq!(history.back("pwd -P").as_deref(), Some("ls"));
        assert_eq!(history.forward("ls -l").as_deref(), Some("pwd -P"));
        assert_eq!(history.back("pwd -P").as_deref(), Some("ls -l"));
        assert_eq!(history.forward("ls -l").as_deref(), Some("pwd -P"));
        // Put back as it was, it is the command again.
        assert_eq!(history.back("pwd").as_deref(), Some("ls -l"));
        assert_eq!(history.forward("ls -l").as_deref(), Some("pwd"));
        history.record("ls -l");
        // Sent: the commands are as they were sent, with the new one.
        assert_eq!(history.back("").as_deref(), Some("ls -l"));
        assert_eq!(history.back("ls -l").as_deref(), Some("pwd"));
        assert_eq!(history.back("pwd").as_deref(), Some("ls"));
        assert_eq!(history.forward("ls").as_deref(), Some("pwd"));
        assert_eq!(history.forward("pwd").as_deref(), Some("ls -l"));
        assert_eq!(history.forward("ls -l").as_deref(), Some(""));
    }

    #[test]
    fn empty_repeated_and_space_led_commands_are_not_kept() {
        let mut history = sent(&["ls", "", "  \n", "ls", " export TOKEN=x", "pwd"]);
        assert_eq!(history.back("").as_deref(), Some("pwd"));
        assert_eq!(history.back("pwd").as_deref(), Some("ls"));
        assert_eq!(history.back("ls"), None);
    }

    #[test]
    fn several_lines_are_one_command() {
        let mut history = sent(&["cd /tmp\nls"]);
        assert_eq!(history.back("").as_deref(), Some("cd /tmp\nls"));
    }

    #[test]
    fn only_the_newest_are_kept() {
        let commands: Vec<String> = (0..LIMIT + 2).map(|n| format!("echo {n}")).collect();
        let mut history = ComposeHistory::default();
        for command in &commands {
            history.record(command);
        }
        let mut shown = String::new();
        let mut oldest = None;
        while let Some(older) = history.back(&shown) {
            oldest = Some(older.clone());
            shown = older;
        }
        assert_eq!(oldest.as_deref(), Some("echo 2"));
    }

    #[test]
    fn the_history_is_there_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("compose-history.json");
        let mut history = ComposeHistory::load(path.clone());
        assert_eq!(history.back(""), None);
        history.record("ls");
        history.record("pwd");
        drop(history);
        let mut history = ComposeHistory::load(path.clone());
        assert_eq!(history.back("").as_deref(), Some("pwd"));
        assert_eq!(history.back("pwd").as_deref(), Some("ls"));
        // Unreadable, it starts empty, and is written anew.
        std::fs::write(&path, "not json").unwrap();
        let mut history = ComposeHistory::load(path.clone());
        assert_eq!(history.back(""), None);
        history.record("uptime");
        let mut history = ComposeHistory::load(path);
        assert_eq!(history.back("").as_deref(), Some("uptime"));
        assert_eq!(history.back("uptime"), None);
    }
}
