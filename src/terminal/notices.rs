//! What a terminal has to tell the user while they look elsewhere: a
//! notification a program asks for (OSC 9 as iTerm2 has it, OSC 777 as urxvt
//! and VTE have it), the bell, or a line a 关键字高亮 rule tells of.
//!
//! Alacritty drops both OSC sequences, so the parser thread runs the output
//! through `NoticeScanner` too.

use alacritty_terminal::vte::{Parser, Perform};

/// The longest title and body a notification shows, in characters.
const TITLE_LIMIT: usize = 80;
const BODY_LIMIT: usize = 240;
/// The longest pattern a notification's title names, in characters.
const PATTERN_LIMIT: usize = 40;

/// Something a terminal asks the user to look at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalNotice {
    /// A program asked for a notification. Its title, when it gave one.
    Program { title: Option<String>, body: String },
    /// The bell rang. The line the cursor is on, which a program waiting
    /// for an answer usually has its question on.
    Bell { line: String },
    /// A line of new output matched a 关键字高亮 rule that notifies: the
    /// rule's pattern, and the line.
    Keyword { pattern: String, line: String },
}

impl TerminalNotice {
    /// The bell, with the line the cursor is on.
    pub fn bell(line: &str) -> Self {
        TerminalNotice::Bell {
            line: clean_text(line, BODY_LIMIT),
        }
    }

    /// A rule with `pattern` matched `line`.
    pub fn keyword(pattern: &str, line: &str) -> Self {
        TerminalNotice::Keyword {
            pattern: clean_text(pattern, PATTERN_LIMIT),
            line: clean_text(line, BODY_LIMIT),
        }
    }
}

/// A notification a program asked for, as it asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramNotice {
    pub title: Option<String>,
    pub body: String,
}

impl ProgramNotice {
    /// The notice to show: control characters out, long text cut.
    pub fn cleaned(self) -> TerminalNotice {
        TerminalNotice::Program {
            title: self
                .title
                .map(|title| clean_text(&title, TITLE_LIMIT))
                .filter(|title| !title.is_empty()),
            body: clean_text(&self.body, BODY_LIMIT),
        }
    }
}

/// Finds the notifications programs ask for in terminal output, however the
/// output is split into chunks.
pub struct NoticeScanner {
    parser: Parser,
    found: Vec<ProgramNotice>,
}

impl Default for NoticeScanner {
    fn default() -> Self {
        Self {
            parser: Parser::new(),
            found: Vec::new(),
        }
    }
}

impl NoticeScanner {
    /// The notifications that end in `bytes`.
    pub fn scan(&mut self, bytes: &[u8]) -> Vec<ProgramNotice> {
        let mut found = Found(&mut self.found);
        self.parser.advance(&mut found, bytes);
        std::mem::take(&mut self.found)
    }
}

struct Found<'a>(&'a mut Vec<ProgramNotice>);

impl Perform for Found<'_> {
    fn osc_dispatch(&mut self, params: &[&[u8]], _: bool) {
        if let Some(notice) = program_notice(params) {
            self.0.push(notice);
        }
    }
}

/// The notification an OSC sequence asks for, if it is one:
/// - `9;body`, unless the body is a ConEmu command (`9;4;50` is a progress
///   bar, `9;9;/tmp` the working directory);
/// - `777;notify;title;body`.
fn program_notice(params: &[&[u8]]) -> Option<ProgramNotice> {
    // The text after the first parameters, `;` and all: a body may hold one.
    let rest = |from: usize| -> String {
        params
            .get(from..)
            .unwrap_or_default()
            .iter()
            .map(|param| String::from_utf8_lossy(param))
            .collect::<Vec<_>>()
            .join(";")
    };
    let notice = match *params.first()? {
        b"9" => {
            let body = rest(1);
            let conemu = params.len() > 1
                && !params[1].is_empty()
                && params[1].iter().all(u8::is_ascii_digit);
            ProgramNotice {
                title: None,
                body: (!conemu).then_some(body)?,
            }
        }
        b"777" if params.get(1) == Some(&&b"notify"[..]) => ProgramNotice {
            title: Some(String::from_utf8_lossy(params.get(2)?).into_owned()),
            body: rest(3),
        },
        _ => return None,
    };
    (!notice.body.trim().is_empty()).then_some(notice)
}

/// `text` on one line, as a notification shows it: control characters and
/// runs of white space become one space, and what is longer than `max`
/// characters is cut with an ellipsis.
pub fn clean_text(text: &str, max: usize) -> String {
    let words: Vec<&str> = text
        .split(|character: char| character.is_whitespace() || character.is_control())
        .filter(|word| !word.is_empty())
        .collect();
    let line = words.join(" ");
    if line.chars().count() <= max {
        return line;
    }
    let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(chunks: &[&[u8]]) -> Vec<ProgramNotice> {
        let mut scanner = NoticeScanner::default();
        chunks
            .iter()
            .flat_map(|chunk| scanner.scan(chunk))
            .collect()
    }

    fn notice(title: Option<&str>, body: &str) -> ProgramNotice {
        ProgramNotice {
            title: title.map(str::to_string),
            body: body.to_string(),
        }
    }

    #[test]
    fn osc_9_is_a_notification_unless_it_is_a_conemu_command() {
        assert_eq!(
            scan(&[b"x\x1b]9;\xe9\x83\xa8\xe7\xbd\xb2\xe5\xae\x8c\xe6\x88\x90\x07y"]),
            [notice(None, "部署完成")]
        );
        // A body may hold a `;`.
        assert_eq!(scan(&[b"\x1b]9;a;b\x1b\\"]), [notice(None, "a;b")]);
        assert!(scan(&[b"\x1b]9;4;1;50\x07\x1b]9;9;/tmp\x07\x1b]9;\x07\x1b]9;  \x07"]).is_empty());
    }

    #[test]
    fn osc_777_notify_has_a_title_and_a_body() {
        assert_eq!(
            scan(&["\x1b]777;notify;构建;完成\x07".as_bytes()]),
            [notice(Some("构建"), "完成")]
        );
        assert!(scan(&[b"\x1b]777;preexec\x07\x1b]777;notify;t\x07"]).is_empty());
        // Other sequences are not notifications.
        assert!(scan(&[b"\x1b]0;title\x07\x1b]52;c;aGk=\x07\x07"]).is_empty());
    }

    #[test]
    fn a_sequence_split_between_chunks_is_found_once_it_ends() {
        assert_eq!(
            scan(&[b"\x1b]777;noti", b"fy;t;", b"done\x1b", b"\\"]),
            [notice(Some("t"), "done")]
        );
    }

    #[test]
    fn notification_text_is_one_line_and_cut_when_long() {
        assert_eq!(clean_text(" a\tb\n\x1bc  ", 10), "a b c");
        assert_eq!(clean_text("一二三四五", 4), "一二三…");
        assert_eq!(clean_text("一二三四", 4), "一二三四");
        assert_eq!(
            TerminalNotice::keyword(" ERROR ", "12:00\tERROR\x1b boom"),
            TerminalNotice::Keyword {
                pattern: "ERROR".into(),
                line: "12:00 ERROR boom".into()
            }
        );
        assert_eq!(
            notice(Some(" "), "x").cleaned(),
            TerminalNotice::Program {
                title: None,
                body: "x".into()
            }
        );
    }
}
