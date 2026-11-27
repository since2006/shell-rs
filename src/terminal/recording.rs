//! 录制当前终端 (issue #121): what a terminal shows, from when the user
//! starts until they stop, into the file they chose, as plain text.
//!
//! The text is the screen's, line by line: colors and controls gone,
//! carriage returns and a wrapped line's rows already applied. What is
//! typed is there as the shell echoes it, so a password asked for without
//! an echo is not; nor is a full-screen program's screen, which vim or top
//! redraw rather than print. The parser thread takes each line as a line
//! feed finishes it, as 关键字高亮 does (`advance_watching`), and hands it
//! over: a thread of the recording's own writes the file, so a slow or
//! stuck disk holds up neither the output nor the window.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use chrono::NaiveDateTime;

/// How a recording ended, once it has: all of it written, or why not;
/// disconnected when its thread could not even say. The window asks it on
/// a timer: a thread of its own must not wake the window's tasks.
pub type RecordingOutcome = mpsc::Receiver<io::Result<()>>;

/// Where the parser thread hands a recording its lines, while there is one.
pub(super) type RecordingLines = Arc<Mutex<Option<mpsc::Sender<Recorded>>>>;

/// What the recording's thread is given.
pub(super) enum Recorded {
    /// Lines the terminal finished.
    Lines(Vec<String>),
    /// The end, with the line the cursor is on, unfinished, when it has
    /// text.
    End(Option<String>),
}

/// Lines end as text files do on the system the file is saved on: tools on
/// Windows expect CRLF.
const NEWLINE: &[u8] = if cfg!(windows) { b"\r\n" } else { b"\n" };

/// The name a new recording's file is offered: `ShellRS-20261123-174322.log`,
/// by the local time it starts.
pub fn recording_file_name(now: NaiveDateTime) -> String {
    format!("ShellRS-{}.log", now.format("%Y%m%d-%H%M%S"))
}

/// The name an exported terminal output file is offered:
/// `ShellRS-20261123-174322.txt`, by the local time it is exported.
pub fn export_file_name(now: NaiveDateTime) -> String {
    format!("ShellRS-{}.txt", now.format("%Y%m%d-%H%M%S"))
}

/// Format plain terminal text for saving to disk, using the platform's line
/// ending and a trailing newline when non-empty.
pub fn export_text_bytes(text: &str) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(text.len() + NEWLINE.len());
    for line in text.split('\n') {
        out.extend_from_slice(line.as_bytes());
        out.extend_from_slice(NEWLINE);
    }
    out
}

/// Write what comes over the returned sender into `file`, on a thread of
/// its own, until `Recorded::End` or the sender goes.
pub(super) fn record(file: File) -> io::Result<(mpsc::Sender<Recorded>, RecordingOutcome)> {
    let (sender, receiver) = mpsc::channel();
    let (outcome, ended) = mpsc::channel();
    thread::Builder::new()
        .name("shellrs-terminal-recording".into())
        .spawn(move || {
            let _ = outcome.send(write(BufWriter::new(file), receiver));
        })?;
    Ok((sender, ended))
}

fn write(mut file: impl Write, recorded: mpsc::Receiver<Recorded>) -> io::Result<()> {
    while let Ok(recorded) = recorded.recv() {
        match recorded {
            Recorded::Lines(lines) => {
                for line in &lines {
                    write_line(&mut file, line)?;
                }
                // On the disk as it comes, not only at the end: what was
                // shown before a crash is kept.
                file.flush()?;
            }
            Recorded::End(last) => {
                if let Some(line) = last {
                    write_line(&mut file, &line)?;
                }
                break;
            }
        }
    }
    file.flush()
}

fn write_line(file: &mut impl Write, line: &str) -> io::Result<()> {
    file.write_all(line.as_bytes())?;
    file.write_all(NEWLINE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(recorded: Vec<Recorded>) -> String {
        let (sender, receiver) = mpsc::channel();
        for recorded in recorded {
            sender.send(recorded).unwrap();
        }
        drop(sender);
        let mut file = Vec::new();
        write(&mut file, receiver).unwrap();
        String::from_utf8(file)
            .unwrap()
            .replace(std::str::from_utf8(NEWLINE).unwrap(), "\n")
    }

    #[test]
    fn lines_go_in_as_they_come_and_the_open_one_at_the_end() {
        let lines =
            |lines: &[&str]| Recorded::Lines(lines.iter().map(|line| line.to_string()).collect());
        assert_eq!(
            written(vec![
                lines(&["root@web1:~# cat /etc/hostname", "web1"]),
                lines(&["root@web1:~# exit"]),
                Recorded::End(Some("logout".into())),
                // Nothing after the end.
                lines(&["later"]),
            ]),
            "root@web1:~# cat /etc/hostname\nweb1\nroot@web1:~# exit\nlogout\n"
        );
        // A terminal gone without saying: what it gave is there.
        assert_eq!(written(vec![lines(&["web1"])]), "web1\n");
        assert_eq!(written(vec![Recorded::End(None)]), "");
    }

    /// A disk that refuses the file ends the recording, saying why.
    #[test]
    fn a_file_that_cannot_be_written_ends_it() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (sender, receiver) = mpsc::channel();
        sender.send(Recorded::Lines(vec!["web1".into()])).unwrap();
        let error = write(Full, receiver).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    }

    #[test]
    fn a_new_recording_is_named_after_when_it_starts() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 11, 23)
            .unwrap()
            .and_hms_opt(17, 43, 22)
            .unwrap();
        assert_eq!(recording_file_name(now), "ShellRS-20261123-174322.log");
        assert_eq!(export_file_name(now), "ShellRS-20261123-174322.txt");
    }

    #[test]
    fn exported_text_uses_platform_newlines() {
        assert!(export_text_bytes("").is_empty());
        let bytes = export_text_bytes("first\nsecond");
        let expected = format!(
            "first{nl}second{nl}",
            nl = std::str::from_utf8(NEWLINE).unwrap()
        );
        assert_eq!(bytes, expected.as_bytes());
    }
}
