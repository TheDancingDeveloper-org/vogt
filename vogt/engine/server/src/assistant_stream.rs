//! Streamed chat completions, for the live call (WI-960, WI-965).
//!
//! A typed turn waits for the whole reply because nothing reads it before it
//! is finished. A call cannot afford that: the first sentence should be on
//! its way to the speaker while the model is still writing the second. This
//! module turns an OpenAI-compatible `stream: true` response — Server-Sent
//! Events carrying `choices[0].delta` chunks — back into the *same* message
//! the non-streamed loop already consumes, reporting each text delta as it
//! arrives. Everything after the message (the tool gate, the log, the
//! transcript) is therefore one code path, streamed or not.
//!
//! The parsing is kept free of I/O so it can be tested on recorded byte
//! sequences, including the awkward ones: an event split across network
//! reads, CRLF line endings, keep-alive comments, and tool-call arguments
//! that arrive a few characters at a time.

use serde_json::{json, Map, Value};

use crate::error::{ApiError, Result};

/// Splits a Server-Sent Events byte stream into the payloads of its `data:`
/// lines. Bytes arrive in arbitrary pieces, so an incomplete trailing line is
/// held until the rest of it comes.
#[derive(Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
}

impl SseDecoder {
    /// Feed one network read; returns every complete `data:` payload in it.
    /// Comments (`: keep-alive`), `event:`/`id:` lines and blank separators
    /// carry nothing the loop needs and are skipped.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut payloads = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(data) = line.strip_prefix("data:") {
                payloads.push(data.strip_prefix(' ').unwrap_or(data).to_string());
            }
        }
        payloads
    }

    /// A last line the server sent without a newline before closing.
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.pending);
        let line = String::from_utf8_lossy(&rest);
        let line = line.trim_end_matches(['\n', '\r']);
        line.strip_prefix("data:")
            .map(|data| data.strip_prefix(' ').unwrap_or(data).to_string())
    }
}

/// One tool call as its pieces arrive. Providers send the id and name in the
/// first chunk for an index and the JSON arguments spread over the rest.
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// Rebuilds the assistant message from streamed deltas.
#[derive(Default)]
pub struct DeltaAccumulator {
    content: String,
    calls: Vec<PartialCall>,
}

impl DeltaAccumulator {
    /// Apply one parsed chunk. Returns the text it added, if any, so the
    /// caller can forward it the moment it exists.
    ///
    /// A chunk carrying a top-level `error` — how OpenRouter reports a failure
    /// after the stream has already started — is an error, not an empty delta.
    pub fn push(&mut self, chunk: &Value) -> Result<Option<String>> {
        if let Some(error) = chunk.get("error") {
            return Err(ApiError::Internal(format!(
                "assistant backend stream error: {}",
                truncate(&error.to_string(), 300)
            )));
        }
        let Some(delta) = chunk.pointer("/choices/0/delta") else {
            // Usage-only and keep-alive chunks carry no choice.
            return Ok(None);
        };
        let text = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string);
        if let Some(text) = &text {
            self.content.push_str(text);
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .map(|i| i as usize)
                    .unwrap_or(self.calls.len().saturating_sub(1));
                // Bounded: a provider cannot make us allocate an index table
                // of arbitrary size with one large number.
                if index > 64 {
                    return Err(ApiError::Internal(
                        "assistant backend stream: tool-call index out of range".into(),
                    ));
                }
                while self.calls.len() <= index {
                    self.calls.push(PartialCall::default());
                }
                let slot = &mut self.calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    if slot.id.is_empty() {
                        slot.id = id.to_string();
                    }
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    // Some providers repeat the whole name on every chunk;
                    // others send it once. Neither sends it in pieces.
                    if slot.name.is_empty() {
                        slot.name = name.to_string();
                    }
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    slot.arguments.push_str(args);
                }
            }
        }
        Ok(text)
    }

    /// Text received so far — what was said before an interruption.
    pub fn text(&self) -> &str {
        &self.content
    }

    /// The finished message, shaped exactly like a non-streamed
    /// `choices[0].message`.
    pub fn message(&self) -> Value {
        let mut message = Map::new();
        message.insert("role".into(), json!("assistant"));
        message.insert(
            "content".into(),
            if self.content.is_empty() && !self.calls.is_empty() {
                Value::Null
            } else {
                json!(self.content)
            },
        );
        let calls: Vec<Value> = self
            .calls
            .iter()
            .filter(|call| !call.name.is_empty())
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.name, "arguments": call.arguments},
                })
            })
            .collect();
        if !calls.is_empty() {
            message.insert("tool_calls".into(), Value::Array(calls));
        }
        Value::Object(message)
    }

    /// The finished message wrapped as a whole non-streamed response, so the
    /// loop reads both from `/choices/0/message`.
    pub fn response(&self) -> Value {
        json!({"choices": [{"message": self.message()}]})
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(chunks: &[&[u8]]) -> (DeltaAccumulator, Vec<String>) {
        let mut decoder = SseDecoder::default();
        let mut acc = DeltaAccumulator::default();
        let mut deltas = Vec::new();
        for chunk in chunks {
            for payload in decoder.feed(chunk) {
                if payload == "[DONE]" {
                    continue;
                }
                let value: Value = serde_json::from_str(&payload).unwrap();
                if let Some(text) = acc.push(&value).unwrap() {
                    deltas.push(text);
                }
            }
        }
        (acc, deltas)
    }

    #[test]
    fn text_deltas_arrive_in_order_and_rebuild_the_message() {
        let (acc, deltas) = feed_all(&[
            b"data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
            b"data: {\"choices\":[{\"delta\":{\"content\":\"Two sessions \"}}]}\n\n",
            b"data: {\"choices\":[{\"delta\":{\"content\":\"are idle.\"}}]}\n\ndata: [DONE]\n\n",
        ]);
        assert_eq!(deltas, vec!["Two sessions ", "are idle."]);
        assert_eq!(
            acc.message(),
            json!({"role": "assistant", "content": "Two sessions are idle."})
        );
    }

    #[test]
    fn an_event_split_across_reads_and_crlf_endings_still_parse() {
        let (acc, deltas) = feed_all(&[
            b": OPENROUTER PROCESSING\r\n\r\ndata: {\"choices\":[{\"del",
            b"ta\":{\"content\":\"Hel\"}}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}",
            b"\r\n\r\n",
        ]);
        assert_eq!(deltas, vec!["Hel", "lo"]);
        assert_eq!(acc.text(), "Hello");
    }

    #[test]
    fn tool_call_arguments_are_concatenated_per_index() {
        let (acc, deltas) = feed_all(&[
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_session_tail","arguments":""}}]}}]}"#,
            b"\n\n",
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"session_"}}]}}]}"#,
            b"\n\n",
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"list_sessions","arguments":"{}"}}]}}]}"#,
            b"\n\n",
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"id\":\"x\"}"}}]}}]}"#,
            b"\n\ndata: [DONE]\n\n",
        ]);
        assert!(deltas.is_empty());
        let message = acc.message();
        assert_eq!(message["content"], Value::Null);
        let calls = message["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["id"], "call_a");
        assert_eq!(calls[0]["function"]["name"], "read_session_tail");
        assert_eq!(calls[0]["function"]["arguments"], "{\"session_id\":\"x\"}");
        assert_eq!(calls[1]["function"]["name"], "list_sessions");
    }

    #[test]
    fn a_mid_stream_error_is_an_error_not_silence() {
        let mut acc = DeltaAccumulator::default();
        let err = acc
            .push(&json!({"error": {"message": "upstream overloaded", "code": 502}}))
            .unwrap_err();
        assert!(err.to_string().contains("upstream overloaded"));
    }

    #[test]
    fn a_huge_tool_call_index_is_refused_rather_than_allocated() {
        let mut acc = DeltaAccumulator::default();
        let chunk = json!({"choices":[{"delta":{"tool_calls":[{"index": 4_000_000_000u64, "function": {"name": "x"}}]}}]});
        assert!(acc.push(&chunk).is_err());
    }

    #[test]
    fn a_final_line_without_a_newline_is_not_lost() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.feed(b"data: [DONE]").is_empty());
        assert_eq!(decoder.finish().as_deref(), Some("[DONE]"));
    }
}
