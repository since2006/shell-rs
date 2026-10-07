//! The commands of a host's bash history, as the panel lists them.

use std::collections::HashMap;

use crate::i18n::{t, tn};

/// A command of the history, listed once however often it ran.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub command: String,
    /// When it last ran, in seconds since 1970, when bash kept the time
    /// (`HISTTIMEFORMAT` set).
    pub last_run: Option<i64>,
    /// How many times the history holds it.
    pub runs: usize,
}

impl Entry {
    /// The command on one line: a command of several lines shows its line
    /// breaks as 「↵」.
    pub fn one_line(&self) -> String {
        crate::shared::one_line(&self.command)
    }

    /// 「3 分钟前 · 执行 5 次」, or without a time 「执行 5 次」.
    pub fn summary(&self, now: i64) -> String {
        let runs = tn!("history.entry.runs", self.runs);
        match self.last_run {
            Some(at) => format!("{} · {runs}", format_when(at, now)),
            None => runs.into(),
        }
    }
}

/// A host's history: each command once, the newest first.
#[derive(Debug, PartialEq)]
pub struct History {
    entries: Vec<Entry>,
    /// Whether the file was longer than a reading takes, so older commands
    /// are left out.
    truncated: bool,
}

impl History {
    /// From the commands of the history file, the oldest first.
    pub fn new(commands: Vec<(String, Option<i64>)>, truncated: bool) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (command, at) in commands.into_iter().rev() {
            if let Some(&index) = seen.get(&command) {
                entries[index].runs += 1;
                continue;
            }
            seen.insert(command.clone(), entries.len());
            entries.push(Entry {
                command,
                last_run: at,
                runs: 1,
            });
        }
        Self { entries, truncated }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The entries holding every word of `query`, regardless of case: their
    /// indexes, the newest first.
    pub fn matching(&self, query: &str) -> Vec<usize> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                let command = entry.command.to_lowercase();
                words.iter().all(|word| command.contains(word.as_str()))
            })
            .map(|(index, _)| index)
            .collect()
    }
}

/// How long ago `at` was, at `now`: 「刚刚」, 「5 分钟前」, 「3 小时前」,
/// 「2 天前」, and further back the day, 「2026-08-15」.
pub fn format_when(at: i64, now: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    // A host whose clock runs ahead says its commands ran in the future.
    let ago = now - at;
    match ago {
        ..MINUTE => t!("history.when.just_now").into(),
        MINUTE..HOUR => tn!("history.when.minutes", ago / MINUTE).into(),
        HOUR..DAY => tn!("history.when.hours", ago / HOUR).into(),
        DAY..=MONTH => tn!("history.when.days", ago / DAY).into(),
        _ => chrono::DateTime::from_timestamp(at, 0)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(commands: &[&str]) -> History {
        History::new(
            commands
                .iter()
                .map(|command| (command.to_string(), None))
                .collect(),
            false,
        )
    }

    #[test]
    fn a_command_run_again_moves_up_and_counts() {
        let history = history(&["ls", "top", "ls", "df -h", "ls"]);
        let listed: Vec<(&str, usize)> = history
            .entries()
            .iter()
            .map(|entry| (entry.command.as_str(), entry.runs))
            .collect();
        assert_eq!(listed, [("ls", 3), ("df -h", 1), ("top", 1)]);
    }

    #[test]
    fn the_search_wants_every_word_in_any_case() {
        let history = history(&[
            "docker run -d --name web nginx",
            "docker ps",
            "systemctl restart Nginx",
        ]);
        assert_eq!(history.matching(""), [0, 1, 2]);
        assert_eq!(history.matching("nginx"), [0, 2]);
        assert_eq!(history.matching("docker  NGINX"), [2]);
        assert_eq!(history.matching("kubectl"), Vec::<usize>::new());
    }

    #[test]
    fn a_time_reads_as_how_long_ago_it_was() {
        let now = 1_727_846_400;
        assert_eq!(format_when(now - 20, now), "刚刚");
        assert_eq!(format_when(now + 300, now), "刚刚");
        assert_eq!(format_when(now - 5 * 60, now), "5 分钟前");
        assert_eq!(format_when(now - 3 * 3600 - 59, now), "3 小时前");
        assert_eq!(format_when(now - 2 * 86_400, now), "2 天前");
        assert_eq!(format_when(now - 30 * 86_400, now), "30 天前");
        let long_ago = format_when(now - 60 * 86_400, now);
        assert!(long_ago.starts_with("2024-08-0"), "{long_ago}");
    }

    #[test]
    fn english_says_how_long_ago_and_how_often() {
        crate::i18n::isolate_thread();
        crate::i18n::set_locale("en");
        let now = 1_727_846_400;
        assert_eq!(format_when(now - 20, now), "Just now");
        assert_eq!(format_when(now - 60, now), "1 minute ago");
        assert_eq!(format_when(now - 5 * 60, now), "5 minutes ago");
        assert_eq!(format_when(now - 3600, now), "1 hour ago");
        assert_eq!(format_when(now - 2 * 86_400, now), "2 days ago");
        let entry = Entry {
            command: "ls".into(),
            last_run: Some(now - 86_400),
            runs: 1,
        };
        assert_eq!(entry.summary(now), "1 day ago · Ran once");
        assert_eq!(
            Entry {
                last_run: None,
                runs: 4,
                ..entry
            }
            .summary(now),
            "Ran 4 times"
        );
    }

    #[test]
    fn a_command_of_several_lines_shows_on_one() {
        let entry = Entry {
            command: "for f in *\ndo echo $f\ndone".into(),
            last_run: Some(100),
            runs: 2,
        };
        assert_eq!(entry.one_line(), "for f in * ↵ do echo $f ↵ done");
        assert_eq!(entry.summary(100 + 120), "2 分钟前 · 执行 2 次");
        assert_eq!(
            Entry {
                last_run: None,
                ..entry
            }
            .summary(0),
            "执行 2 次"
        );
    }
}
