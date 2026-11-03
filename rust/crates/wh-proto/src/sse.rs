//! SSE parser, line-for-line the behaviour of `SseParser` in `daemon/src/connection.mjs`.
//!
//! Like the JS one, a lone `\r` at the end of a chunk counts as a line end, so a `\r\n` split across two chunks yields an extra
//! blank line. Kept identical on purpose so both parsers agree on every input; the server sends `\n`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseMessage {
    pub id: Option<String>,
    pub event: String,
    pub data: String,
}

#[derive(Debug)]
pub struct SseParser {
    buffer: String,
    event: String,
    data: Vec<String>,
    id: Option<String>,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        SseParser { buffer: String::new(), event: "message".into(), data: vec![], id: None }
    }

    fn reset(&mut self) {
        self.event = "message".into();
        self.data.clear();
        self.id = None;
    }

    pub fn push(&mut self, text: &str) -> Vec<SseMessage> {
        self.buffer.push_str(text);
        let mut out = vec![];
        while let Some(nl) = self.buffer.find(['\r', '\n']) {
            let line: String = self.buffer[..nl].to_string();
            let skip = if self.buffer[nl..].starts_with("\r\n") { 2 } else { 1 };
            self.buffer.drain(..nl + skip);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseMessage { id: self.id.clone(), event: self.event.clone(), data: self.data.join("\n") });
                }
                self.reset();
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.find(':') {
                None => (line.as_str(), ""),
                Some(i) => {
                    let v = &line[i + 1..];
                    (&line[..i], v.strip_prefix(' ').unwrap_or(v))
                }
            };
            match field {
                "event" => self.event = value.to_string(),
                "data" => self.data.push(value.to_string()),
                "id" => self.id = Some(value.to_string()),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: Option<&str>, event: &str, data: &str) -> SseMessage {
        SseMessage { id: id.map(String::from), event: event.into(), data: data.into() }
    }

    #[test]
    fn messages_comments_and_any_cut() {
        let text = "retry: 5000\n\n: keep-alive\n\nid: 7\nevent: job.offer\ndata: {\"a\":1}\n\nid: 8\nevent: settings.update\ndata: line1\ndata: line2\n\n";
        let chars: Vec<char> = text.chars().collect();
        for step in 1..8 {
            let mut p = SseParser::new();
            let mut out = vec![];
            for c in chars.chunks(step) {
                out.extend(p.push(&c.iter().collect::<String>()));
            }
            assert_eq!(out, vec![m(Some("7"), "job.offer", "{\"a\":1}"), m(Some("8"), "settings.update", "line1\nline2")]);
        }
    }

    #[test]
    fn crlf_and_plain_message() {
        let mut p = SseParser::new();
        assert_eq!(p.push("data: x\r\n\r\n"), vec![m(None, "message", "x")]);
    }
}
