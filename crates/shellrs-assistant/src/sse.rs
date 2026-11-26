//! Server-sent events, as model services stream their answers: lines of
//! `field: value`, an event ending at a blank line. Read from bytes as they
//! come, cut anywhere.

/// The longest line taken: one far longer is not an event stream.
const LINE_LIMIT: usize = 8 << 20;

/// One event: its type, when it has one, and its data, the lines of it
/// joined by `\n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// A line longer than anything a model service sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineTooLong;

#[derive(Default)]
pub struct SseParser {
    pending: Vec<u8>,
    /// A `\r` ended the last line: a `\n` right after it belongs to it.
    after_cr: bool,
    event: Option<String>,
    data: Option<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take `bytes`, and give back the events they finish.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, LineTooLong> {
        let mut events = Vec::new();
        for &byte in bytes {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\n' | b'\r' => {
                    self.after_cr = byte == b'\r';
                    let line = std::mem::take(&mut self.pending);
                    if let Some(event) = self.line(&line) {
                        events.push(event);
                    }
                }
                _ => {
                    if self.pending.len() >= LINE_LIMIT {
                        return Err(LineTooLong);
                    }
                    self.pending.push(byte);
                }
            }
        }
        Ok(events)
    }

    /// The stream ended: an event it did not end with a blank line still
    /// counts.
    pub fn finish(&mut self) -> Option<SseEvent> {
        let line = std::mem::take(&mut self.pending);
        if let Some(event) = self.line(&line) {
            return Some(event);
        }
        self.line(b"")
    }

    fn line(&mut self, line: &[u8]) -> Option<SseEvent> {
        if line.is_empty() {
            let data = self.data.take();
            let event = self.event.take();
            return data.map(|data| SseEvent { event, data });
        }
        if line[0] == b':' {
            return None;
        }
        let line = String::from_utf8_lossy(line);
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_ref(), ""),
        };
        match field {
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            },
            "event" => self.event = Some(value.to_owned()),
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(bytes: &[u8]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        let mut events = parser.push(bytes).unwrap();
        events.extend(parser.finish());
        events
    }

    fn event(kind: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: kind.map(Into::into),
            data: data.into(),
        }
    }

    const STREAM: &[u8] = b"event: message_start\r\ndata: {\"a\":1}\r\n\r\n: ping\n\nevent: delta\ndata: one\ndata: two\n\ndata:[DONE]\n\n";

    #[test]
    fn events_end_at_blank_lines_whatever_the_line_ends() {
        assert_eq!(
            all(STREAM),
            [
                event(Some("message_start"), "{\"a\":1}"),
                event(Some("delta"), "one\ntwo"),
                event(None, "[DONE]"),
            ]
        );
        assert_eq!(
            all(b"data: x\r\rdata: y\r\r"),
            [event(None, "x"), event(None, "y")]
        );
    }

    #[test]
    fn cut_anywhere_the_events_are_the_same() {
        let whole = all(STREAM);
        for size in 1..STREAM.len() {
            let mut parser = SseParser::new();
            let mut events = Vec::new();
            for chunk in STREAM.chunks(size) {
                events.extend(parser.push(chunk).unwrap());
            }
            events.extend(parser.finish());
            assert_eq!(events, whole, "chunks of {size}");
        }
    }

    #[test]
    fn a_last_event_without_a_blank_line_still_counts() {
        assert_eq!(all(b"data: last"), [event(None, "last")]);
        assert_eq!(all(b"data: last\n"), [event(None, "last")]);
        assert!(all(b"event: nothing\n").is_empty());
    }

    #[test]
    fn a_line_without_end_is_refused() {
        let mut parser = SseParser::new();
        let chunk = vec![b'x'; 1 << 20];
        let mut failed = false;
        for _ in 0..9 {
            if parser.push(&chunk).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
    }
}
