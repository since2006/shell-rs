//! 会话日志: what a terminal's output is written to, as plain text with an
//! ANSI-free line at a time, under a folder and a file name the settings
//! name.
//!
//! The settings are a global, like [`super::TerminalInteraction`], so the
//! terminal engine reads them when a connection starts and needs no window.
//! A log is one file per connection: a reconnection opens a new one, with
//! the placeholders of its template filled in afresh.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use alacritty_terminal::vte::{Parser, Perform};
use chrono::{DateTime, Local, NaiveDateTime};
use gpui_kit::{App, Global, SharedString};
use serde::{Deserialize, Serialize};

/// The file name used until the user writes another: a folder of the day,
/// then the time and the address.
pub const DEFAULT_LOG_TEMPLATE: &str = "%Y%M%D/%Y-%M-%D_%h%m%s_%H.log";
/// The line timestamp until the user writes another. The placeholders are
/// chrono's, not the file name's.
pub const DEFAULT_LOG_TIMESTAMP_FORMAT: &str = "[%Y-%m-%d %H:%M:%S] ";
/// How long a path component may be, in characters.
const COMPONENT_LIMIT: usize = 120;

/// 终端 → 会话日志.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionLogSettings {
    /// 记录会话日志: every connection made from now on is written down.
    pub enabled: bool,
    /// 保存目录: where the template is taken from; empty is the default.
    pub directory: Option<String>,
    /// 文件名模板: the path under the folder, with placeholders.
    pub template: String,
    /// 每行时间戳: each line begins with the time.
    pub timestamp: bool,
    /// chrono's format for that time.
    pub timestamp_format: String,
}

impl Default for SessionLogSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            directory: None,
            template: DEFAULT_LOG_TEMPLATE.to_string(),
            timestamp: true,
            timestamp_format: DEFAULT_LOG_TIMESTAMP_FORMAT.to_string(),
        }
    }
}

impl Global for SessionLogSettings {}

impl SessionLogSettings {
    /// What the terminals do now.
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_default()
    }

    /// The folder the template is taken from.
    pub fn directory(&self) -> PathBuf {
        self.directory
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(default_log_directory)
    }

    /// The settings with the blanks trimmed and nothing left that would
    /// make a log fail to open.
    pub fn normalized(mut self) -> Self {
        self.directory = self
            .directory
            .map(|directory| directory.trim().to_string())
            .filter(|directory| !directory.is_empty());
        self.template = self.template.trim().to_string();
        if self.template.is_empty() {
            self.template = DEFAULT_LOG_TEMPLATE.to_string();
        }
        // The timestamp format is left as written, so a format halfway
        // through being typed is not replaced under the caret; a log takes
        // the default when it cannot write the time with it.
        self
    }
}

/// Where logs go when the settings name no folder.
pub fn default_log_directory() -> PathBuf {
    crate::app::data_dir().join("logs")
}

/// Who a connection is, for the `%H` and `%S` of a log's file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLogIdentity {
    /// `%S`: the host's name, or「本地终端 N」.
    pub name: SharedString,
    /// `%H`: the address, or `local`.
    pub address: SharedString,
}

/// What a connection needs to open its log: the settings in force when it
/// started, and who it is.
#[derive(Clone, Debug)]
pub struct SessionLogPlan {
    pub settings: SessionLogSettings,
    pub identity: SessionLogIdentity,
}

/// A log being written: the file is made here, the text goes to a thread of
/// its own so the parser thread never waits for the disk.
pub struct SessionLog {
    path: PathBuf,
    sender: Option<mpsc::Sender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
}

impl SessionLog {
    /// Make the file the settings name, making the folders on the way, and
    /// start writing it. A name already taken gets `-2`, `-3` and so on:
    /// an earlier log is never overwritten.
    pub fn open(
        settings: &SessionLogSettings,
        identity: &SessionLogIdentity,
        now: DateTime<Local>,
    ) -> io::Result<Self> {
        let path = log_path(settings, identity, now.naive_local());
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let path = free_path(path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;

        let format = settings
            .timestamp
            .then(|| log_timestamp_format(settings).to_string());
        let (sender, receiver) = mpsc::channel::<Vec<u8>>();
        let writer = thread::Builder::new()
            .name("shellrs-session-log".into())
            .spawn(move || {
                let mut file = file;
                let prefix: Option<Box<dyn Fn() -> String + Send>> = format.map(|format| {
                    let prefix: Box<dyn Fn() -> String + Send> =
                        Box::new(move || Local::now().format(&format).to_string());
                    prefix
                });
                let mut text = LogText::new(prefix);
                let mut buffer = Vec::new();
                while let Ok(bytes) = receiver.recv() {
                    buffer.clear();
                    text.feed(&bytes, &mut buffer);
                    if file.write_all(&buffer).is_err() {
                        return;
                    }
                }
                buffer.clear();
                text.finish(&mut buffer);
                let _ = file.write_all(&buffer);
                let _ = file.flush();
            })?;

        Ok(Self {
            path,
            sender: Some(sender),
            writer: Some(writer),
        })
    }

    /// The file being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hand output to the writer; the parser thread does not wait for it.
    pub fn write(&self, bytes: &[u8]) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(bytes.to_vec());
        }
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        // Closing the channel ends the writer's loop, which then writes
        // what it still holds and flushes the file.
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// The file a connection starting at `now` logs to, its placeholders filled
/// in. The template's `/` and `\` both make folders.
pub fn log_path(
    settings: &SessionLogSettings,
    identity: &SessionLogIdentity,
    now: NaiveDateTime,
) -> PathBuf {
    let mut path = settings.directory();
    for component in expand_template(&settings.template, identity, now).split(['/', '\\']) {
        // Doubled separators and `.`/`..` are not part of a log's name.
        if component.is_empty() || component == "." || component == ".." {
            continue;
        }
        path.push(safe_component(component));
    }
    if path == settings.directory() {
        path.push("session.log");
    }
    path
}

/// `template` with the time and the connection's names in it:
///
/// - `%Y` the year, `%M` the month, `%D` the day,
/// - `%h` the hour, `%m` the minute, `%s` the second,
/// - `%H` the address, `%S` the name, `%%` a percent sign.
///
/// Anything else after a `%` stays as it is written.
pub fn expand_template(
    template: &str,
    identity: &SessionLogIdentity,
    now: NaiveDateTime,
) -> String {
    let mut expanded = String::with_capacity(template.len());
    let mut characters = template.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            expanded.push(character);
            continue;
        }
        match characters.next() {
            Some('Y') => expanded.push_str(&now.format("%Y").to_string()),
            Some('M') => expanded.push_str(&now.format("%m").to_string()),
            Some('D') => expanded.push_str(&now.format("%d").to_string()),
            Some('h') => expanded.push_str(&now.format("%H").to_string()),
            Some('m') => expanded.push_str(&now.format("%M").to_string()),
            Some('s') => expanded.push_str(&now.format("%S").to_string()),
            Some('H') => expanded.push_str(&identity.address),
            Some('S') => expanded.push_str(&identity.name),
            Some('%') => expanded.push('%'),
            Some(other) => {
                expanded.push('%');
                expanded.push(other);
            }
            None => expanded.push('%'),
        }
    }
    expanded
}

/// Whether `format` is one chrono can write a time with. Checked before the
/// line is written, whose formatter would panic on an error.
pub fn valid_timestamp_format(format: &str) -> bool {
    !format.is_empty()
        && !chrono::format::StrftimeItems::new(format)
            .any(|item| matches!(item, chrono::format::Item::Error))
}

/// The format a log's lines start with: what the settings say, or the
/// default when that is not one chrono can write.
fn log_timestamp_format(settings: &SessionLogSettings) -> &str {
    if valid_timestamp_format(&settings.timestamp_format) {
        &settings.timestamp_format
    } else {
        DEFAULT_LOG_TIMESTAMP_FORMAT
    }
}

/// A name a filesystem takes: the characters Windows forbids made `_`, the
/// trailing spaces and dots it drops gone, and the names it reserves
/// prefixed.
fn safe_component(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .take(COMPONENT_LIMIT)
        .collect();
    let cleaned = cleaned.trim_end_matches([' ', '.']);
    let cleaned = if cleaned.is_empty() { "_" } else { cleaned };
    if is_reserved_name(cleaned) {
        format!("_{cleaned}")
    } else {
        cleaned.to_string()
    }
}

fn is_reserved_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let upper = upper.split('.').next().unwrap_or(&upper);
    matches!(upper, "CON" | "PRN" | "AUX" | "NUL")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

/// `path`, or the first `name-2.ext`, `name-3.ext` … beside it that is free.
fn free_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned());
    for number in 2..10_000 {
        let name = match &extension {
            Some(extension) => format!("{stem}-{number}.{extension}"),
            None => format!("{stem}-{number}"),
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path
}

/// Terminal output as a log reads it: the escape sequences gone, a line at
/// a time, each line with the time it began when timestamps are on.
struct LogText {
    parser: Parser,
    at_line_start: bool,
    /// The last character was a carriage return: a line feed right after it
    /// belongs to the same line break.
    after_carriage_return: bool,
    prefix: Option<Box<dyn Fn() -> String + Send>>,
}

impl LogText {
    fn new(prefix: Option<Box<dyn Fn() -> String + Send>>) -> Self {
        Self {
            parser: Parser::new(),
            at_line_start: true,
            after_carriage_return: false,
            prefix,
        }
    }

    /// Write what `bytes` says, in the emulator's own terms: the parser
    /// knows where a sequence ends and where a character's bytes are split.
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let mut text = PlainText {
            out,
            at_line_start: self.at_line_start,
            after_carriage_return: self.after_carriage_return,
            prefix: self.prefix.as_deref(),
        };
        self.parser.advance(&mut text, bytes);
        self.at_line_start = text.at_line_start;
        self.after_carriage_return = text.after_carriage_return;
    }

    /// End the line the output stopped in the middle of.
    fn finish(&mut self, out: &mut Vec<u8>) {
        if !self.at_line_start {
            out.push(b'\n');
            self.at_line_start = true;
        }
    }
}

struct PlainText<'a> {
    out: &'a mut Vec<u8>,
    at_line_start: bool,
    after_carriage_return: bool,
    prefix: Option<&'a (dyn Fn() -> String + Send)>,
}

impl PlainText<'_> {
    fn start_line(&mut self) {
        if !self.at_line_start {
            return;
        }
        if let Some(prefix) = self.prefix {
            self.out.extend_from_slice(prefix().as_bytes());
        }
        self.at_line_start = false;
    }
}

impl Perform for PlainText<'_> {
    fn print(&mut self, character: char) {
        self.start_line();
        let mut buffer = [0u8; 4];
        self.out
            .extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
        self.after_carriage_return = false;
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => {
                self.out.push(b'\n');
                self.at_line_start = true;
                self.after_carriage_return = true;
            }
            b'\n' => {
                if self.after_carriage_return {
                    self.after_carriage_return = false;
                } else {
                    self.out.push(b'\n');
                    self.at_line_start = true;
                }
            }
            b'\t' => {
                self.start_line();
                self.out.push(b'\t');
                self.after_carriage_return = false;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::NaiveDate;
    use tempfile::tempdir;

    use super::*;

    fn identity() -> SessionLogIdentity {
        SessionLogIdentity {
            name: "web-01".into(),
            address: "10.0.0.1".into(),
        }
    }

    fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(hour, minute, second)
            .unwrap()
    }

    fn logged(chunks: &[&[u8]], prefix: Option<Box<dyn Fn() -> String + Send>>) -> String {
        let mut text = LogText::new(prefix);
        let mut out = Vec::new();
        for chunk in chunks {
            text.feed(chunk, &mut out);
        }
        text.finish(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn placeholders_fill_in_the_time_and_the_names() {
        let template = "%Y%M%D/%Y-%M-%D_%h%m%s_%H_%S.log";
        assert_eq!(
            expand_template(template, &identity(), at(2026, 10, 22, 14, 30, 5)),
            "20261022/2026-10-22_143005_10.0.0.1_web-01.log"
        );
    }

    #[test]
    fn an_unknown_placeholder_stays_as_it_is_written() {
        assert_eq!(
            expand_template("%Q 100%% %", &identity(), at(2026, 1, 2, 3, 4, 5)),
            "%Q 100% %"
        );
    }

    #[test]
    fn a_template_takes_the_folder_as_well_as_the_name() {
        let settings = SessionLogSettings {
            directory: Some("C:\\logs".into()),
            template: "%Y%M%D\\%Y-%M-%D_%h%m%s_%H.log".into(),
            ..SessionLogSettings::default()
        };
        assert_eq!(
            log_path(&settings, &identity(), at(2026, 10, 22, 14, 30, 5)),
            PathBuf::from("C:\\logs")
                .join("20261022")
                .join("2026-10-22_143005_10.0.0.1.log")
        );
    }

    #[test]
    fn a_name_a_filesystem_refuses_is_made_safe() {
        assert_eq!(safe_component("web:01*?-02"), "web_01__-02");
        assert_eq!(safe_component("  dots...  "), "  dots");
        assert_eq!(safe_component(".."), "_");
        assert_eq!(safe_component("CON"), "_CON");
        assert_eq!(safe_component("lpt1.log"), "_lpt1.log");
        assert_eq!(safe_component("conifer"), "conifer");
    }

    #[test]
    fn escape_sequences_are_left_out_of_the_log() {
        assert_eq!(
            logged(&[b"\x1b[31mred\x1b[0m \x1b]0;title\x07done\r\n"], None),
            "red done\n"
        );
    }

    #[test]
    fn a_carriage_return_ends_the_line() {
        assert_eq!(logged(&[b"10%\r20%\r\n100%\n"], None), "10%\n20%\n100%\n");
    }

    #[test]
    fn each_line_begins_with_its_time() {
        assert_eq!(
            logged(
                &[b"one\ntwo\r\nthree"],
                Some(Box::new(|| "[T] ".to_string()))
            ),
            "[T] one\n[T] two\n[T] three\n"
        );
    }

    #[test]
    fn blank_lines_do_not_begin_with_a_time() {
        assert_eq!(
            logged(&[b"one\n\nthree\n"], Some(Box::new(|| "[T] ".to_string()))),
            "[T] one\n\n[T] three\n"
        );
    }

    #[test]
    fn a_character_split_between_chunks_is_written_whole() {
        let bytes = "你好".as_bytes();
        assert_eq!(
            logged(&[&bytes[..1], &bytes[1..4], &bytes[4..]], None),
            "你好\n"
        );
    }

    #[test]
    fn a_format_chrono_cannot_write_is_refused() {
        assert!(valid_timestamp_format("[%Y-%m-%d %H:%M:%S] "));
        assert!(!valid_timestamp_format("%Q"));
        assert!(!valid_timestamp_format(""));
    }

    #[test]
    fn a_format_chrono_cannot_write_falls_back_to_the_default() {
        let broken = SessionLogSettings {
            timestamp_format: "%Q".into(),
            ..SessionLogSettings::default()
        };
        assert_eq!(log_timestamp_format(&broken), DEFAULT_LOG_TIMESTAMP_FORMAT);
        let working = SessionLogSettings {
            timestamp_format: "%H:%M".into(),
            ..SessionLogSettings::default()
        };
        assert_eq!(log_timestamp_format(&working), "%H:%M");
    }

    #[test]
    fn a_format_halfway_through_being_typed_is_kept() {
        let settings = SessionLogSettings {
            timestamp_format: "[%Y".into(),
            ..SessionLogSettings::default()
        }
        .normalized();
        assert_eq!(settings.timestamp_format, "[%Y");
    }

    #[test]
    fn a_log_is_written_with_its_name_and_ends_with_a_newline() {
        let folder = tempdir().unwrap();
        let settings = SessionLogSettings {
            directory: Some(folder.path().to_string_lossy().into_owned()),
            template: "%Y%M%D/%H.log".into(),
            timestamp: false,
            ..SessionLogSettings::default()
        };
        let now = Local::now();
        let first = SessionLog::open(&settings, &identity(), now).unwrap();
        first.write(b"\x1b[32mhello\x1b[0m\r\n");
        let first_path = first.path().to_path_buf();
        drop(first);

        let second = SessionLog::open(&settings, &identity(), now).unwrap();
        let second_path = second.path().to_path_buf();
        drop(second);

        assert_ne!(first_path, second_path);
        assert!(first_path.ends_with("10.0.0.1.log"));
        assert_eq!(fs::read_to_string(&first_path).unwrap(), "hello\n");
    }
}
