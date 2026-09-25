//! A minimal Server-Sent Events parser for the hub's event channel
//! (spec §4.1; hub § Wire contract framing). Pure: bytes in, frames out.

/// A single unterminated line may not grow past this many bytes (T4 fix
/// round 1): a hub bug or a malicious proxy sending an endless line would
/// otherwise buffer forever. 8 MiB comfortably covers the largest legitimate
/// frame (an inlined `project` event's ≤ 50 manifest rows).
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// One event's accumulated `data` (across every `data:` line before the
/// blank-line dispatch) may not grow past this many bytes either — the same
/// cap, guarding the case of many small lines instead of one huge one.
pub const MAX_DATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    Event {
        name: String,
        data: String,
    },
    Comment,
    Retry(u64),
    /// A line or one event's `data` exceeded its size cap. The parser stops
    /// trusting anything after this — the caller (`stream::pump`) ends the
    /// connection so the session reconnects fresh.
    TooLarge,
}

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    name: Option<String>,
    data: Option<String>,
    saw_cr: bool,
    /// Set once a cap is exceeded; every byte after is ignored (the frame
    /// reporting it was already emitted, and the caller is about to drop
    /// this parser along with the connection).
    overflowed: bool,
}

impl SseParser {
    /// Feed a chunk of bytes; returns every frame the chunk completed. Bytes
    /// are buffered until a full line, so a UTF-8 character split across two
    /// chunks (or a CRLF split across two chunks) is safe.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        let mut out = Vec::new();
        for &b in chunk {
            if self.overflowed {
                continue;
            }
            if self.saw_cr {
                self.saw_cr = false;
                if b == b'\n' {
                    continue; // CRLF: the CR already ended the line
                }
            }
            match b {
                b'\n' | b'\r' => {
                    self.saw_cr = b == b'\r';
                    let line = std::mem::take(&mut self.buf);
                    self.line(&String::from_utf8_lossy(&line), &mut out);
                }
                _ => {
                    if self.buf.len() >= MAX_LINE_BYTES {
                        tracing::warn!(
                            cap = MAX_LINE_BYTES,
                            "sse line exceeded the size cap; ending the stream"
                        );
                        self.overflowed = true;
                        self.buf.clear();
                        out.push(SseFrame::TooLarge);
                        continue;
                    }
                    self.buf.push(b);
                }
            }
        }
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<SseFrame>) {
        if line.is_empty() {
            // dispatch
            if let Some(data) = self.data.take() {
                let name = self.name.take().unwrap_or_else(|| "message".to_string());
                out.push(SseFrame::Event { name, data });
            }
            self.name = None;
            return;
        }
        if line.starts_with(':') {
            out.push(SseFrame::Comment);
            return;
        }
        let (field, value) = match line.find(':') {
            Some(i) => (
                &line[..i],
                line[i + 1..].strip_prefix(' ').unwrap_or(&line[i + 1..]),
            ),
            None => (line, ""),
        };
        match field {
            "event" => self.name = Some(value.to_string()),
            "data" => {
                let prior = self.data.as_ref().map_or(0, |d| d.len() + 1);
                if prior + value.len() > MAX_DATA_BYTES {
                    tracing::warn!(
                        cap = MAX_DATA_BYTES,
                        "sse event data exceeded the size cap; ending the stream"
                    );
                    self.data = None;
                    self.overflowed = true;
                    out.push(SseFrame::TooLarge);
                    return;
                }
                match &mut self.data {
                    Some(d) => {
                        d.push('\n');
                        d.push_str(value);
                    }
                    None => self.data = Some(value.to_string()),
                }
            }
            "retry" => {
                if let Ok(ms) = value.parse() {
                    out.push(SseFrame::Retry(ms));
                }
            }
            _ => {} // "id" and unknown fields are ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "retry: 3000\nevent: hello\ndata: {\"a\":1}\n\n:\n\nevent: holders\ndata: {\"b\":\ndata: 2}\n\n";

    fn expected() -> Vec<SseFrame> {
        vec![
            SseFrame::Retry(3000),
            SseFrame::Event {
                name: "hello".into(),
                data: "{\"a\":1}".into(),
            },
            SseFrame::Comment,
            SseFrame::Event {
                name: "holders".into(),
                data: "{\"b\":\n2}".into(),
            },
        ]
    }

    #[test]
    fn whole_sample_parses() {
        let mut p = SseParser::default();
        assert_eq!(p.push(SAMPLE.as_bytes()), expected());
    }

    #[test]
    fn every_split_point_gives_the_same_frames() {
        let bytes = SAMPLE.as_bytes();
        for cut in 0..=bytes.len() {
            let mut p = SseParser::default();
            let mut got = p.push(&bytes[..cut]);
            got.extend(p.push(&bytes[cut..]));
            assert_eq!(got, expected(), "split at {cut}");
        }
    }

    /// The same sample and expectations, but with every line ending
    /// rewritten to CRLF — the hub never sends this, but a proxy might.
    #[test]
    fn every_split_point_gives_the_same_frames_with_crlf() {
        let text = SAMPLE.replace('\n', "\r\n");
        let bytes = text.as_bytes();
        for cut in 0..=bytes.len() {
            let mut p = SseParser::default();
            let mut got = p.push(&bytes[..cut]);
            got.extend(p.push(&bytes[cut..]));
            assert_eq!(got, expected(), "CRLF split at {cut}");
        }
    }

    /// Same again with bare CR line endings (the spec's third accepted form).
    #[test]
    fn every_split_point_gives_the_same_frames_with_cr_only() {
        let text = SAMPLE.replace('\n', "\r");
        let bytes = text.as_bytes();
        for cut in 0..=bytes.len() {
            let mut p = SseParser::default();
            let mut got = p.push(&bytes[..cut]);
            got.extend(p.push(&bytes[cut..]));
            assert_eq!(got, expected(), "CR-only split at {cut}");
        }
    }

    #[test]
    fn a_line_past_the_cap_ends_the_parse_with_too_large() {
        let mut p = SseParser::default();
        let huge = vec![b'a'; MAX_LINE_BYTES + 1];
        assert_eq!(p.push(&huge), vec![SseFrame::TooLarge]);
        // Nothing further from this parser — including a well-formed event
        // right after the overflow — is trusted.
        assert_eq!(p.push(b"\nevent: hello\ndata: {}\n\n"), Vec::new());
    }

    #[test]
    fn event_data_past_the_cap_ends_the_parse_with_too_large() {
        let mut p = SseParser::default();
        // Many small `data:` lines whose sum crosses the cap — the per-line
        // cap alone would not catch this.
        let line = format!("data: {}\n", "a".repeat(1000));
        let mut buf = Vec::new();
        let reps = MAX_DATA_BYTES / 1000 + 2;
        for _ in 0..reps {
            buf.extend_from_slice(line.as_bytes());
        }
        let frames = p.push(&buf);
        assert!(frames.contains(&SseFrame::TooLarge), "{frames:?}");
    }

    #[test]
    fn crlf_and_utf8_split_inside_a_character() {
        let s = "event: project\r\ndata: {\"t\":\"Ω\"}\r\n\r\n".as_bytes();
        let omega = s.iter().position(|b| *b == 0xCE).unwrap();
        let mut p = SseParser::default();
        let mut got = p.push(&s[..omega + 1]);
        got.extend(p.push(&s[omega + 1..]));
        assert_eq!(
            got,
            vec![SseFrame::Event {
                name: "project".into(),
                data: "{\"t\":\"Ω\"}".into()
            }]
        );
    }
}
