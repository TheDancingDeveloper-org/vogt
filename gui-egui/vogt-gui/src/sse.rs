//! An incremental `text/event-stream` parser.
//!
//! Bytes arrive in arbitrary chunks; [`SseParser::feed`] returns each complete
//! event's `data` (multi-line data joined with `\n`, as the spec says) and
//! counts comment lines, which the engine sends as heartbeats.

/// What one call to [`SseParser::feed`] produced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Fed {
    /// The data of each complete event, in order.
    pub events: Vec<String>,
    /// True when anything arrived at all: an event, a comment or a blank
    /// keep-alive. A stream that says nothing for too long is stale.
    pub heartbeat: bool,
}

/// Holds a partial line and a partial event between chunks.
#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    data: Option<String>,
}

impl SseParser {
    /// A parser at the start of a stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next chunk.
    pub fn feed(&mut self, chunk: &[u8]) -> Fed {
        let mut fed = Fed { heartbeat: !chunk.is_empty(), ..Fed::default() };
        for &byte in chunk {
            if byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.line_done(&String::from_utf8_lossy(&line), &mut fed);
            } else {
                self.line.push(byte);
            }
        }
        fed
    }

    fn line_done(&mut self, line: &str, fed: &mut Fed) {
        if line.is_empty() {
            if let Some(data) = self.data.take() {
                fed.events.push(data);
            }
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        if field == "data" {
            match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_owned()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"a\"").events.is_empty());
        let fed = p.feed(b":1}\n\ndata: two\n\n");
        assert_eq!(fed.events, vec!["{\"a\":1}".to_owned(), "two".to_owned()]);
    }

    #[test]
    fn crlf_comments_and_multiline() {
        let mut p = SseParser::new();
        let fed = p.feed(b": keep-alive\r\n\r\nevent: x\r\ndata: a\r\ndata:b\r\nid: 3\r\n\r\n");
        assert_eq!(fed.events, vec!["a\nb".to_owned()]);
        assert!(fed.heartbeat);
    }

    #[test]
    fn comment_only_chunk_is_a_heartbeat_without_events() {
        let mut p = SseParser::new();
        let fed = p.feed(b":\n\n");
        assert!(fed.events.is_empty());
        assert!(fed.heartbeat);
        assert!(!p.feed(b"").heartbeat);
    }
}
