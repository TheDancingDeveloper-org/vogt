//! What a session cost, read from the transcript the agent already wrote.
//! Ports `src/vogt/core/session_economics.py`.
//!
//! Claude Code records `message.usage` on every assistant entry; the sum of
//! its input, cache-read and cache-creation tokens is the live context at that
//! turn. Klaudia writes the same envelope with no usage and marks compaction
//! with a `compact_boundary` line. Codex carries tool results under `payload`.
//! Without usage, context is estimated at four characters per token over the
//! history since the latest compaction. This takes text and returns numbers:
//! it opens no file, asks no clock, and stores nothing.

#![allow(dead_code)] // Domain vocabulary the bin re-exports; tests and later services use what the bin does not.
use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value};

use crate::core::from_iso;
use crate::decisions::python_json_dumps;

/// Klaudia's own figure: four characters of transcript per token when the
/// agent records no usage.
pub const CHARS_PER_TOKEN: i64 = 4;

/// Tool names whose input names a file. A repeated value is a repeated read.
const READ_TOOLS: &[&str] = &["Read", "read_file", "cat"];
const PATH_KEYS: &[&str] = &["file_path", "path", "filePath"];

/// Per-turn wall clock, in milliseconds. Zero when fewer than two turns
/// carried a timestamp, because one turn has no gap to measure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnTiming {
    pub count: i64,
    pub max_ms: i64,
    pub mean_ms: i64,
}

/// The economics of one transcript. Every field is present even when the
/// transcript said nothing: zero and an empty map, not absence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionEconomics {
    pub turns: i64,
    pub context_tokens: i64,
    pub peak_context_tokens: i64,
    pub compactions: i64,
    pub tool_result_bytes: BTreeMap<String, i64>,
    pub reads_repeated: i64,
    pub wall_clock: TurnTiming,
    pub output_tokens: i64,
    /// True when context came from recorded usage, false when estimated.
    pub usage_measured: bool,
}

impl SessionEconomics {
    /// The one line a digest row carries: `96/120 turns, 61% ctx, 2 compactions`.
    /// A missing (or zero) budget or window is left out rather than shown as
    /// zero, because zero would say a budget exists and is exhausted.
    pub fn budget_line(&self, turn_budget: Option<i64>, window_tokens: Option<i64>) -> String {
        let mut parts = Vec::new();
        match turn_budget.filter(|budget| *budget != 0) {
            Some(budget) => parts.push(format!("{}/{budget} turns", self.turns)),
            None => parts.push(format!("{} turns", self.turns)),
        }
        if let Some(window) = window_tokens.filter(|window| *window != 0) {
            parts.push(format!("{}% ctx", context_pct(self.context_tokens, window)));
        }
        if self.compactions != 0 {
            let noun = if self.compactions == 1 {
                "compaction"
            } else {
                "compactions"
            };
            parts.push(format!("{} {noun}", self.compactions));
        }
        parts.join(", ")
    }
}

/// `min(100, round(100 * context / window))`, with Python's half-even round.
pub fn context_pct(context: i64, window: i64) -> i64 {
    round_half_even(100.0 * context as f64 / window as f64).min(100)
}

fn round_half_even(value: f64) -> i64 {
    let floor = value.floor();
    let diff = value - floor;
    let rounded = if diff > 0.5 || (diff == 0.5 && floor % 2.0 != 0.0) {
        floor + 1.0
    } else {
        floor
    };
    rounded as i64
}

/// Python's `str.splitlines`: the transcript reader splits on every line
/// boundary Python knows, not only `\n`, so a line holding a raw U+2028 is
/// two lines here as it is there.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, ch)) = chars.next() {
        let boundary = matches!(
            ch,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&text[start..at]);
        let mut end = at + ch.len_utf8();
        if ch == '\r' {
            if let Some((next, '\n')) = chars.peek().copied() {
                chars.next();
                end = next + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Reduce one transcript's JSONL lines to its economics.
///
/// A line that is not a JSON object is skipped. `agent` names the shape to
/// expect; an unknown agent is read as Claude's shape, which Klaudia also
/// writes, but only `claude` counts as measured usage.
pub fn meter<'a>(lines: impl IntoIterator<Item = &'a str>, agent: &str) -> SessionEconomics {
    let measured = agent == "claude";
    let mut turn_ids: HashSet<String> = HashSet::new();
    let mut unkeyed_turns = 0_i64;
    let mut compactions = 0_i64;
    let mut context = 0_i64;
    let mut peak = 0_i64;
    // Claude repeats one turn's usage on each of its content-block entries, so
    // the last reading per message id is the turn's.
    let mut output_by_turn: HashMap<String, i64> = HashMap::new();
    let mut unkeyed_output = 0_i64;
    let mut live_chars = 0_i64;
    let mut tools: BTreeMap<String, i64> = BTreeMap::new();
    let mut reads: HashMap<String, i64> = HashMap::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut stamps: Vec<String> = Vec::new();

    for raw in lines {
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(text) else {
            continue;
        };
        let kind = entry.get("type").and_then(Value::as_str);

        if is_compaction(&entry) {
            compactions += 1;
            // The compacted history is no longer in the window.
            live_chars = 0;
        }
        live_chars += text.chars().count() as i64;

        if kind == Some("assistant") {
            let message_id = entry
                .get("message")
                .and_then(Value::as_object)
                .and_then(|message| message.get("id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty());
            let stamp = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .filter(|stamp| !stamp.is_empty());
            match message_id {
                Some(id) => {
                    if turn_ids.insert(id.to_string()) {
                        stamps.extend(stamp.map(str::to_string));
                    }
                }
                None => {
                    unkeyed_turns += 1;
                    stamps.extend(stamp.map(str::to_string));
                }
            }
            remember_tools(&entry, &mut tool_names);
            if let Some((turn_context, output)) = usage(&entry) {
                match message_id {
                    Some(id) => {
                        output_by_turn.insert(id.to_string(), output);
                    }
                    None => unkeyed_output += output,
                }
                context = turn_context;
                peak = peak.max(turn_context);
            } else if !measured {
                context = live_chars / CHARS_PER_TOKEN;
                peak = peak.max(context);
            }
        } else if !measured && matches!(kind, Some("user" | "tool_result" | "turn_context")) {
            context = live_chars / CHARS_PER_TOKEN;
            peak = peak.max(context);
        }

        count_tools(&entry, &mut tools, &mut reads, &tool_names);
    }

    if !measured && context == 0 && live_chars != 0 {
        context = live_chars / CHARS_PER_TOKEN;
        peak = context;
    }

    SessionEconomics {
        turns: turn_ids.len() as i64 + unkeyed_turns,
        context_tokens: context,
        peak_context_tokens: peak,
        compactions,
        tool_result_bytes: tools,
        reads_repeated: reads.values().filter(|count| **count > 1).count() as i64,
        wall_clock: timing(&stamps),
        output_tokens: output_by_turn.values().sum::<i64>() + unkeyed_output,
        usage_measured: measured && peak > 0,
    }
}

/// Claude sets `isCompactSummary`; Klaudia writes a line whose subtype is
/// `compact_boundary`.
fn is_compaction(entry: &Map<String, Value>) -> bool {
    if entry.get("isCompactSummary") == Some(&Value::Bool(true)) {
        return true;
    }
    if entry.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
        return true;
    }
    entry
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("isCompactSummary"))
        == Some(&Value::Bool(true))
}

/// `(context_tokens, output_tokens)` from an assistant entry. Context is the
/// input the model was given this turn: fresh input plus whatever was read
/// from or written to the cache.
fn usage(entry: &Map<String, Value>) -> Option<(i64, i64)> {
    let usage = entry
        .get("message")?
        .as_object()?
        .get("usage")?
        .as_object()?;
    let context = [
        "input_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ]
    .iter()
    .map(|key| int_of(usage.get(*key)))
    .sum();
    Some((context, int_of(usage.get("output_tokens"))))
}

/// Python's `int(value or 0)` for the shapes a usage counter takes. A value
/// `int()` would refuse counts as zero rather than failing the reading.
fn int_of(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|float| float.trunc() as i64))
            .unwrap_or(0),
        Some(Value::Bool(flag)) => i64::from(*flag),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Python truthiness for a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

/// Map a `tool_use` block's id to its tool name. A later `tool_result`
/// carries only `tool_use_id`, so the name has to come from the call.
fn remember_tools(entry: &Map<String, Value>, tool_names: &mut HashMap<String, String>) {
    for block in blocks(entry) {
        let Some(block) = block.as_object() else {
            continue;
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let id = block.get("id").and_then(Value::as_str);
        let name = block.get("name").and_then(Value::as_str);
        if let (Some(id), Some(name)) = (id, name) {
            if !id.is_empty() && !name.is_empty() {
                tool_names.insert(id.to_string(), name.to_string());
            }
        }
    }
}

/// Add one entry's tool results to the byte map and the read counts. A result
/// names its tool on the block or only by `tool_use_id`; one that names
/// nothing is ignored rather than lumped under a made-up name.
fn count_tools(
    entry: &Map<String, Value>,
    tools: &mut BTreeMap<String, i64>,
    reads: &mut HashMap<String, i64>,
    tool_names: &HashMap<String, String>,
) {
    for block in blocks(entry) {
        let Some(block) = block.as_object() else {
            continue;
        };
        let named = ["name", "tool_name", "tool"]
            .iter()
            .find_map(|key| block.get(*key).filter(|value| truthy(value)));
        let name = match named.and_then(Value::as_str) {
            Some(name) => Some(name),
            None => block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .and_then(|id| tool_names.get(id))
                .map(String::as_str),
        };
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            continue;
        };
        let size = block_bytes(block);
        let kind = block.get("type").and_then(Value::as_str);
        if matches!(kind, Some("tool_result" | "tool-result")) || block.contains_key("content") {
            *tools.entry(name.to_string()).or_insert(0) += size;
        }
        if READ_TOOLS.contains(&name) {
            if let Some(path) = read_path(block.get("input")) {
                *reads.entry(path.to_string()).or_insert(0) += 1;
            }
        }
    }
}

fn blocks(entry: &Map<String, Value>) -> Vec<&Value> {
    let mut found = Vec::new();
    for key in ["message", "payload"] {
        if let Some(Value::Array(content)) = entry
            .get(key)
            .and_then(Value::as_object)
            .and_then(|container| container.get("content"))
        {
            found.extend(content);
        }
    }
    if let Some(Value::Array(content)) = entry.get("content") {
        found.extend(content);
    }
    found
}

fn block_bytes(block: &Map<String, Value>) -> i64 {
    match block.get("content") {
        Some(Value::String(text)) => text.chars().count() as i64,
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| match part {
                Value::String(text) => text.chars().count() as i64,
                Value::Object(part) => part
                    .get("text")
                    .and_then(Value::as_str)
                    .map_or(0, |text| text.chars().count() as i64),
                _ => 0,
            })
            .sum(),
        _ => python_json_dumps(&Value::Object(block.clone()), false).len() as i64,
    }
}

fn read_path(input: Option<&Value>) -> Option<&str> {
    match input? {
        Value::String(text) if !text.is_empty() => Some(text),
        Value::Object(object) => PATH_KEYS.iter().find_map(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
        }),
        _ => None,
    }
}

/// Gaps between consecutive assistant timestamps, in milliseconds. Stamps
/// that do not parse are dropped. An out-of-order pair gives a zero gap: a
/// negative turn length is not a length.
fn timing(stamps: &[String]) -> TurnTiming {
    let micros: Vec<i64> = stamps
        .iter()
        .filter_map(|stamp| from_iso(&stamp.replace('Z', "+00:00")).ok())
        .map(|moment| moment.unix_seconds() * 1_000_000 + i64::from(moment.nanos() / 1_000))
        .collect();
    if micros.len() < 2 {
        return TurnTiming::default();
    }
    let gaps: Vec<i64> = micros
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).div_euclid(1_000).max(0))
        .collect();
    let total: i64 = gaps.iter().sum();
    TurnTiming {
        count: gaps.len() as i64,
        max_ms: gaps.iter().copied().max().unwrap_or(0),
        mean_ms: round_half_even(total as f64 / gaps.len() as f64),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn lines(entries: &[Value]) -> Vec<String> {
        entries.iter().map(Value::to_string).collect()
    }

    fn metered(entries: &[Value], agent: &str) -> SessionEconomics {
        let text = lines(entries);
        meter(text.iter().map(String::as_str), agent)
    }

    fn assistant(id: &str, usage: Value, stamp: &str) -> Value {
        json!({
            "type": "assistant",
            "timestamp": stamp,
            "message": {"id": id, "usage": usage, "content": []},
        })
    }

    #[test]
    fn claude_context_is_the_latest_turn_not_a_sum() {
        let reading = metered(
            &[
                assistant(
                    "m1",
                    json!({"input_tokens": 10, "cache_read_input_tokens": 90, "output_tokens": 5}),
                    "2026-01-01T00:00:00Z",
                ),
                assistant(
                    "m2",
                    json!({"input_tokens": 20, "cache_creation_input_tokens": 30, "output_tokens": 7}),
                    "2026-01-01T00:00:01.001Z",
                ),
            ],
            "claude",
        );
        assert_eq!(reading.turns, 2);
        assert_eq!(reading.context_tokens, 50);
        assert_eq!(reading.peak_context_tokens, 100);
        assert_eq!(reading.output_tokens, 12);
        assert!(reading.usage_measured);
        assert_eq!(
            reading.wall_clock,
            TurnTiming {
                count: 1,
                max_ms: 1001,
                mean_ms: 1001
            }
        );
    }

    #[test]
    fn one_turn_across_content_blocks_counts_once() {
        let usage = json!({"input_tokens": 100, "output_tokens": 9});
        let reading = metered(
            &[
                assistant("m1", usage.clone(), "2026-01-01T00:00:00Z"),
                assistant("m1", usage, "2026-01-01T00:00:05Z"),
            ],
            "claude",
        );
        assert_eq!(reading.turns, 1);
        assert_eq!(reading.output_tokens, 9);
        assert_eq!(reading.wall_clock, TurnTiming::default());
    }

    #[test]
    fn compactions_reset_the_estimate_for_klaudia() {
        let before = json!({"type": "user", "message": {"content": "x".repeat(400)}});
        let boundary = json!({"type": "system", "subtype": "compact_boundary"});
        let after = json!({"type": "user", "message": {"content": "hi"}});
        let reading = metered(
            &[before.clone(), boundary.clone(), after.clone()],
            "klaudia",
        );
        assert_eq!(reading.compactions, 1);
        let live = boundary.to_string().len() + after.to_string().len();
        assert_eq!(reading.context_tokens, live as i64 / CHARS_PER_TOKEN);
        assert_eq!(
            reading.peak_context_tokens,
            before.to_string().len() as i64 / CHARS_PER_TOKEN
        );
        assert!(!reading.usage_measured);
    }

    #[test]
    fn claude_compact_summary_is_a_compaction() {
        let reading = metered(
            &[json!({"type": "user", "isCompactSummary": true, "message": {}})],
            "claude",
        );
        assert_eq!(reading.compactions, 1);
        assert_eq!(reading.context_tokens, 0);
        assert!(!reading.usage_measured);
    }

    #[test]
    fn tool_results_are_named_by_their_call_and_repeat_reads_count() {
        let call = |id: &str, path: &str| {
            json!({"type": "assistant", "message": {"id": id, "content": [
                {"type": "tool_use", "id": format!("t-{id}"), "name": "Read",
                 "input": {"file_path": path}}
            ]}})
        };
        let result = |id: &str, body: Value| {
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": format!("t-{id}"), "content": body}
            ]}})
        };
        let reading = metered(
            &[
                call("a", "/x.rs"),
                result("a", json!("hello")),
                call("b", "/x.rs"),
                result("b", json!([{"type": "text", "text": "abc"}, "de"])),
                json!({"type": "user", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "unknown", "content": "zzz"}
                ]}}),
            ],
            "claude",
        );
        assert_eq!(reading.tool_result_bytes.get("Read"), Some(&10));
        assert_eq!(reading.tool_result_bytes.len(), 1);
        assert_eq!(reading.reads_repeated, 1);
        assert_eq!(reading.turns, 2);
    }

    #[test]
    fn codex_payload_results_count_and_fall_back_to_json_size() {
        let block = json!({"type": "function_call_output", "name": "shell", "output": "ok"});
        let reading = metered(
            &[json!({"type": "response_item", "payload": {"content": [block.clone()]}})],
            "codex",
        );
        // No `content` and not a tool_result type: not counted as bytes.
        assert!(reading.tool_result_bytes.is_empty());
        let counted = json!({"type": "tool_result", "name": "shell", "output": "ok"});
        let reading = metered(
            &[json!({"type": "response_item", "payload": {"content": [counted.clone()]}})],
            "codex",
        );
        let expected = python_json_dumps(&counted, false).len() as i64;
        assert_eq!(reading.tool_result_bytes.get("shell"), Some(&expected));
        // An estimate with no turn still reports the characters it saw.
        assert!(reading.context_tokens > 0);
        assert_eq!(reading.peak_context_tokens, reading.context_tokens);
    }

    #[test]
    fn junk_lines_and_untyped_entries_are_skipped() {
        let reading = meter(["", "not json", "[1, 2]", "   "], "claude");
        assert_eq!(reading, SessionEconomics::default());
    }

    #[test]
    fn out_of_order_stamps_give_a_zero_gap() {
        let reading = metered(
            &[
                assistant("a", json!({}), "2026-01-01T00:00:10Z"),
                assistant("b", json!({}), "2026-01-01T00:00:00Z"),
                assistant("c", json!({}), "2026-01-01T00:00:03Z"),
                assistant("d", json!({}), "not a time"),
            ],
            "claude",
        );
        assert_eq!(
            reading.wall_clock,
            TurnTiming {
                count: 2,
                max_ms: 3000,
                mean_ms: 1500
            }
        );
        assert!(!reading.usage_measured);
    }

    #[test]
    fn budget_line_leaves_out_what_is_missing() {
        let reading = SessionEconomics {
            turns: 96,
            context_tokens: 122_000,
            compactions: 2,
            ..SessionEconomics::default()
        };
        assert_eq!(
            reading.budget_line(Some(120), Some(200_000)),
            "96/120 turns, 61% ctx, 2 compactions"
        );
        assert_eq!(reading.budget_line(None, None), "96 turns, 2 compactions");
        assert_eq!(
            reading.budget_line(Some(0), Some(0)),
            "96 turns, 2 compactions"
        );
        let one = SessionEconomics {
            turns: 1,
            compactions: 1,
            context_tokens: 500,
            ..SessionEconomics::default()
        };
        assert_eq!(
            one.budget_line(None, Some(100)),
            "1 turns, 100% ctx, 1 compaction"
        );
    }

    #[test]
    fn percent_rounds_half_to_even() {
        assert_eq!(context_pct(1, 8), 12);
        assert_eq!(context_pct(3, 8), 38);
        assert_eq!(context_pct(5, 2), 100);
    }

    #[test]
    fn split_lines_matches_python() {
        assert_eq!(
            split_lines("a\nb\r\nc\rd\u{2028}e\n"),
            vec!["a", "b", "c", "d", "e"]
        );
        assert_eq!(split_lines(""), Vec::<&str>::new());
        assert_eq!(split_lines("a\n\nb"), vec!["a", "", "b"]);
    }

    #[test]
    fn claude_peak_is_the_largest_turn_and_results_name_their_tool() {
        let reading = metered(
            &[
                json!({"type": "user", "message": {"role": "user", "content": "go"}}),
                json!({"type": "assistant", "timestamp": "2026-10-11T01:00:00Z", "message": {
                    "model": "claude",
                    "usage": {"input_tokens": 100, "cache_read_input_tokens": 50,
                              "cache_creation_input_tokens": 25, "output_tokens": 10},
                    "content": [{"type": "tool_result", "name": "Bash", "content": "x".repeat(40)}],
                }}),
                json!({"type": "assistant", "timestamp": "2026-10-11T01:00:30Z", "message": {
                    "usage": {"input_tokens": 80, "output_tokens": 5}, "content": [],
                }}),
            ],
            "claude",
        );
        assert_eq!(reading.turns, 2);
        assert_eq!(reading.context_tokens, 80);
        assert_eq!(reading.peak_context_tokens, 175);
        assert_eq!(reading.output_tokens, 15);
        assert!(reading.usage_measured);
        assert_eq!(reading.compactions, 0);
        assert_eq!(
            reading.tool_result_bytes,
            BTreeMap::from([("Bash".to_string(), 40)])
        );
        assert_eq!(
            reading.wall_clock,
            TurnTiming {
                count: 1,
                max_ms: 30_000,
                mean_ms: 30_000
            }
        );
    }

    #[test]
    fn claude_compaction_marker_counts_wherever_it_sits() {
        let reading = metered(
            &[
                json!({"type": "system", "isCompactSummary": true}),
                json!({"type": "assistant", "message": {"isCompactSummary": true, "usage": {"input_tokens": 1}}}),
            ],
            "claude",
        );
        assert_eq!(reading.compactions, 2);
        assert_eq!(reading.turns, 1);
    }

    #[test]
    fn claude_output_tokens_count_once_per_turn_across_its_blocks() {
        let usage = json!({"input_tokens": 10, "cache_read_input_tokens": 0,
                           "cache_creation_input_tokens": 0, "output_tokens": 7});
        let entries: Vec<Value> = [
            json!({"type": "thinking", "thinking": "hm"}),
            json!({"type": "text", "text": "hello"}),
            json!({"type": "tool_use", "id": "t1", "name": "Read", "input": {}}),
        ]
        .into_iter()
        .map(|block| json!({"type": "assistant", "message": {"id": "msg_1", "usage": usage, "content": [block]}}))
        .collect();
        let reading = metered(&entries, "claude");
        assert_eq!(reading.turns, 1);
        assert_eq!(reading.output_tokens, 7);
    }

    fn fixture(name: &str, agent: &str) -> SessionEconomics {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../server/tests/fixtures/economics")
            .join(format!("{name}.jsonl"));
        let text = std::fs::read_to_string(&path).expect("shared economics fixture");
        meter(split_lines(&text), agent)
    }

    fn bytes(pairs: &[(&str, i64)]) -> BTreeMap<String, i64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn shared_fixtures_match_the_python_meter() {
        assert_eq!(
            fixture("claude", "claude"),
            SessionEconomics {
                turns: 2,
                context_tokens: 80,
                peak_context_tokens: 175,
                compactions: 1,
                tool_result_bytes: bytes(&[("Bash", 40), ("Read", 16)]),
                reads_repeated: 1,
                wall_clock: TurnTiming {
                    count: 1,
                    max_ms: 30_000,
                    mean_ms: 30_000
                },
                output_tokens: 16,
                usage_measured: true,
            }
        );
        assert_eq!(
            fixture("klaudia", "klaudia"),
            SessionEconomics {
                turns: 2,
                context_tokens: 67,
                peak_context_tokens: 117,
                compactions: 1,
                tool_result_bytes: bytes(&[("Read", 16), ("shell", 6)]),
                reads_repeated: 1,
                wall_clock: TurnTiming {
                    count: 1,
                    max_ms: 60_000,
                    mean_ms: 60_000
                },
                output_tokens: 0,
                usage_measured: false,
            }
        );
        assert_eq!(
            fixture("codex", "codex"),
            SessionEconomics {
                turns: 3,
                context_tokens: 191,
                peak_context_tokens: 191,
                compactions: 0,
                tool_result_bytes: bytes(&[("cat", 10), ("grep", 9), ("shell", 11)]),
                reads_repeated: 1,
                wall_clock: TurnTiming {
                    count: 2,
                    max_ms: 45_000,
                    mean_ms: 22_500
                },
                output_tokens: 0,
                usage_measured: false,
            }
        );
    }

    #[test]
    fn shared_unicode_fixture_counts_characters_as_python_does() {
        let reading = fixture("klaudia_unicode", "klaudia");
        assert_eq!(reading.turns, 3);
        assert_eq!(reading.context_tokens, 127);
        assert_eq!(
            reading.tool_result_bytes,
            bytes(&[("Grep", 90), ("Read", 10)])
        );
        assert_eq!(
            reading.wall_clock,
            TurnTiming {
                count: 2,
                max_ms: 3,
                mean_ms: 2
            }
        );
    }

    #[test]
    fn shared_gap_fixture_counts_milliseconds_exactly() {
        let reading = fixture("claude_gap", "claude");
        assert_eq!(reading.wall_clock.max_ms, 1001);
        assert_eq!(reading.wall_clock.mean_ms, 1001);
        assert_eq!(reading.tool_result_bytes, bytes(&[("Grep", 71)]));
    }
}
