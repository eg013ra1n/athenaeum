//! A minimal Server-Sent Events parser for the hub's event channel
//! (spec §4.1; hub § Wire contract framing). Pure: bytes in, frames out.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    Event { name: String, data: String },
    Comment,
    Retry(u64),
}

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    name: Option<String>,
    data: Option<String>,
    saw_cr: bool,
}

impl SseParser {
    /// Feed a chunk of bytes; returns every frame the chunk completed. Bytes
    /// are buffered until a full line, so a UTF-8 character split across two
    /// chunks (or a CRLF split across two chunks) is safe.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        let mut out = Vec::new();
        for &b in chunk {
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
                _ => self.buf.push(b),
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
            "data" => match &mut self.data {
                Some(d) => {
                    d.push('\n');
                    d.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
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
