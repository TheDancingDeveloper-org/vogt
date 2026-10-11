//! The economics of one session's transcript, read from the JSONL the agent
//! already wrote. A line-for-line port of `vogt.core.session_economics`: the
//! engine and the core must give the same numbers for the same lines.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;
use time::OffsetDateTime;
use vogt_engine_contract::{SessionEconomics, TurnTiming};

/// Klaudia's own figure: four characters of transcript per token when the
/// agent records no usage.
const CHARS_PER_TOKEN: i64 = 4;

/// Tool names whose input names a file. A repeated value is a repeated read.
const READ_TOOLS: [&str; 3] = ["Read", "read_file", "cat"];
const PATH_KEYS: [&str; 3] = ["file_path", "path", "filePath"];

/// Accumulates the economics of one transcript, one JSONL line at a time.
pub struct Meter {
    /// `claude` records usage; anything else is estimated from transcript size.
    measured: bool,
    turn_ids: HashMap<String, ()>,
    unkeyed_turns: i64,
    compactions: i64,
    context: i64,
    peak: i64,
    /// Output tokens per turn. Claude repeats one turn's usage on each of its
    /// content-block entries, so the last reading per message id is the turn's.
    output_by_turn: HashMap<String, i64>,
    unkeyed_output: i64,
    live_chars: i64,
    tools: HashMap<String, i64>,
    reads: HashMap<String, i64>,
    tool_names: HashMap<String, String>,
    stamps: Vec<String>,
}

impl Meter {
    pub fn new(agent: &str) -> Self {
        Self {
            measured: agent == "claude",
            turn_ids: HashMap::new(),
            unkeyed_turns: 0,
            compactions: 0,
            context: 0,
            peak: 0,
            output_by_turn: HashMap::new(),
            unkeyed_output: 0,
            live_chars: 0,
            tools: HashMap::new(),
            reads: HashMap::new(),
            tool_names: HashMap::new(),
            stamps: Vec::new(),
        }
    }

    /// Fold one JSONL line in. A line that is not JSON, or not an object, is
    /// skipped, as is a blank line.
    pub fn feed_line(&mut self, line: &str) {
        let text = line.trim();
        if text.is_empty() {
            return;
        }
        let Ok(entry) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let Some(entry) = entry.as_object() else {
            return;
        };
        let kind = entry.get("type").and_then(Value::as_str);

        if is_compaction(entry) {
            self.compactions += 1;
            // The compacted history is no longer in the window.
            self.live_chars = 0;
        }
        // Characters, as Python's len(str) counts them, not UTF-8 bytes.
        self.live_chars += text.chars().count() as i64;

        if kind == Some("assistant") {
            let message = entry.get("message").and_then(Value::as_object);
            let message_id = message
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty());
            match message_id {
                Some(id) => {
                    if let std::collections::hash_map::Entry::Vacant(slot) =
                        self.turn_ids.entry(id.to_string())
                    {
                        if let Some(stamp) = stamp_of(entry) {
                            self.stamps.push(stamp);
                        }
                        slot.insert(());
                    }
                }
                None => {
                    self.unkeyed_turns += 1;
                    if let Some(stamp) = stamp_of(entry) {
                        self.stamps.push(stamp);
                    }
                }
            }
            remember_tools(entry, &mut self.tool_names);
            if let Some((turn_context, output)) = usage(entry) {
                match message_id {
                    Some(id) => {
                        self.output_by_turn.insert(id.to_string(), output);
                    }
                    None => self.unkeyed_output += output,
                }
                self.context = turn_context;
                self.peak = self.peak.max(turn_context);
            } else if !self.measured {
                self.context = self.live_chars / CHARS_PER_TOKEN;
                self.peak = self.peak.max(self.context);
            }
        } else if !self.measured && matches!(kind, Some("user" | "tool_result" | "turn_context")) {
            self.context = self.live_chars / CHARS_PER_TOKEN;
            self.peak = self.peak.max(self.context);
        }

        count_tools(entry, &mut self.tools, &mut self.reads, &self.tool_names);
    }

    pub fn snapshot(&self) -> SessionEconomics {
        let mut context = self.context;
        let mut peak = self.peak;
        if !self.measured && context == 0 && self.live_chars > 0 {
            context = self.live_chars / CHARS_PER_TOKEN;
            peak = context;
        }
        let tool_result_bytes: BTreeMap<String, i64> = self
            .tools
            .iter()
            .map(|(name, bytes)| (name.clone(), *bytes))
            .collect();
        SessionEconomics {
            turns: self.turn_ids.len() as i64 + self.unkeyed_turns,
            context_tokens: context,
            peak_context_tokens: peak,
            compactions: self.compactions,
            tool_result_bytes,
            reads_repeated: self.reads.values().filter(|n| **n > 1).count() as i64,
            wall_clock: timing(&self.stamps),
            output_tokens: self.output_by_turn.values().sum::<i64>() + self.unkeyed_output,
            usage_measured: self.measured && peak > 0,
        }
    }
}

fn stamp_of(entry: &serde_json::Map<String, Value>) -> Option<String> {
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|stamp| !stamp.is_empty())
        .map(str::to_string)
}

/// Claude sets `isCompactSummary`; Klaudia writes a system line whose subtype
/// is `compact_boundary`.
fn is_compaction(entry: &serde_json::Map<String, Value>) -> bool {
    if entry.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    if entry.get("subtype").and_then(Value::as_str) == Some("compact_boundary") {
        return true;
    }
    entry
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("isCompactSummary"))
        .and_then(Value::as_bool)
        == Some(true)
}

/// `(context_tokens, output_tokens)` from an assistant entry.
///
/// Context is the input the model was given this turn: fresh input plus
/// whatever was read from or written to the cache. The caller keeps only the
/// latest, so history is not counted once per turn.
fn usage(entry: &serde_json::Map<String, Value>) -> Option<(i64, i64)> {
    let usage = entry
        .get("message")
        .and_then(Value::as_object)?
        .get("usage")
        .and_then(Value::as_object)?;
    let context = [
        "input_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ]
    .iter()
    .map(|key| json_int(usage.get(*key)))
    .sum();
    Some((context, json_int(usage.get("output_tokens"))))
}

/// `int(value or 0)`: JSON null, a missing value and a non-number all read as 0.
fn json_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0),
        _ => 0,
    }
}

/// Map a `tool_use` block's id to its tool name. A later `tool_result` carries
/// only `tool_use_id`, so the name has to come from the call that produced it.
fn remember_tools(
    entry: &serde_json::Map<String, Value>,
    tool_names: &mut HashMap<String, String>,
) {
    for block in blocks(entry) {
        let Some(block) = block.as_object() else {
            continue;
        };
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let (Some(tool_id), Some(name)) = (
            block
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
            block
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        tool_names.insert(tool_id.to_string(), name.to_string());
    }
}

/// Add one entry's tool results to the byte map and the read counts.
///
/// Claude and Klaudia nest results in `message.content`; Codex nests them in
/// `payload`. A result names its tool on the block or, in the real Claude
/// shape, only by `tool_use_id` pointing at an earlier `tool_use`. A result
/// that names nothing is ignored rather than lumped under a made-up name.
fn count_tools(
    entry: &serde_json::Map<String, Value>,
    tools: &mut HashMap<String, i64>,
    reads: &mut HashMap<String, i64>,
    tool_names: &HashMap<String, String>,
) {
    for block in blocks(entry) {
        let Some(block) = block.as_object() else {
            continue;
        };
        let named = block
            .get("name")
            .or_else(|| block.get("tool_name"))
            .or_else(|| block.get("tool"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let name = named.or_else(|| {
            block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .and_then(|tool_id| tool_names.get(tool_id))
                .cloned()
        });
        let Some(name) = name else { continue };
        let size = block_bytes(block);
        let kind = block.get("type").and_then(Value::as_str);
        if matches!(kind, Some("tool_result" | "tool-result")) || block.contains_key("content") {
            *tools.entry(name.clone()).or_insert(0) += size;
        }
        if READ_TOOLS.contains(&name.as_str()) {
            if let Some(path) = read_path(block.get("input")) {
                *reads.entry(path).or_insert(0) += 1;
            }
        }
    }
}

fn blocks(entry: &serde_json::Map<String, Value>) -> Vec<&Value> {
    let mut found = Vec::new();
    for container_key in ["message", "payload"] {
        if let Some(content) = entry
            .get(container_key)
            .and_then(Value::as_object)
            .and_then(|container| container.get("content"))
            .and_then(Value::as_array)
        {
            found.extend(content);
        }
    }
    if let Some(content) = entry.get("content").and_then(Value::as_array) {
        found.extend(content);
    }
    found
}

fn block_bytes(block: &serde_json::Map<String, Value>) -> i64 {
    // Python's len(str) counts characters, so these do too.
    match block.get("content") {
        Some(Value::String(content)) => content.chars().count() as i64,
        Some(Value::Array(parts)) => parts
            .iter()
            .map(|part| match part {
                Value::String(text) => text.chars().count(),
                Value::Object(object) => object
                    .get("text")
                    .and_then(Value::as_str)
                    .map(|text| text.chars().count())
                    .unwrap_or(0),
                _ => 0,
            })
            .sum::<usize>() as i64,
        _ => py_json_len(&Value::Object(block.clone())) as i64,
    }
}

/// The length of `json.dumps(value)` with Python's defaults: `", "` and
/// `": "` separators and `ensure_ascii`, so every non-ASCII character is a
/// `\uXXXX` escape (two of them outside the BMP). serde_json writes compact,
/// unescaped JSON, so its length is not the same number.
fn py_json_len(value: &Value) -> usize {
    match value {
        Value::Null | Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(n) => n.to_string().len(),
        Value::String(text) => py_str_len(text),
        Value::Array(items) => {
            2 + items.iter().map(py_json_len).sum::<usize>() + 2 * items.len().saturating_sub(1)
        }
        Value::Object(map) => {
            2 + map
                .iter()
                .map(|(key, item)| py_str_len(key) + 2 + py_json_len(item))
                .sum::<usize>()
                + 2 * map.len().saturating_sub(1)
        }
    }
}

fn py_str_len(text: &str) -> usize {
    2 + text
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if (c as u32) < 0x20 => 6,
            c if c.is_ascii() => 1,
            c if (c as u32) > 0xFFFF => 12,
            _ => 6,
        })
        .sum::<usize>()
}

fn read_path(tool_input: Option<&Value>) -> Option<String> {
    match tool_input {
        Some(Value::String(path)) if !path.is_empty() => Some(path.clone()),
        Some(Value::Object(input)) => PATH_KEYS.iter().find_map(|key| {
            input
                .get(*key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }),
        _ => None,
    }
}

/// Gaps between consecutive assistant timestamps, in milliseconds.
///
/// Stamps that do not parse are dropped. Out-of-order stamps give a zero gap
/// rather than a negative one: a transcript rewritten by a compaction can
/// repeat an earlier time, and a negative turn length is not a length.
fn timing(stamps: &[String]) -> TurnTiming {
    let times: Vec<OffsetDateTime> = stamps
        .iter()
        .filter_map(|stamp| {
            OffsetDateTime::parse(stamp, &time::format_description::well_known::Rfc3339).ok()
        })
        .collect();
    if times.len() < 2 {
        return TurnTiming::default();
    }
    let gaps: Vec<i64> = times
        .windows(2)
        .map(|pair| {
            i64::try_from((pair[1] - pair[0]).whole_milliseconds())
                .unwrap_or(0)
                .max(0)
        })
        .collect();
    let count = gaps.len() as i64;
    TurnTiming {
        count,
        max_ms: gaps.iter().copied().max().unwrap_or(0),
        mean_ms: round_half_even(gaps.iter().sum::<i64>(), count),
    }
}

/// `round(total / count)` as Python rounds it: halves go to the even
/// neighbour. Both arguments are non-negative and `count` is positive.
fn round_half_even(total: i64, count: i64) -> i64 {
    let (quotient, remainder) = (total / count, total % count);
    match (2 * remainder).cmp(&count) {
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Equal if quotient % 2 == 1 => quotient + 1,
        _ => quotient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(agent: &str, transcript: &str) -> SessionEconomics {
        let mut meter = Meter::new(agent);
        for line in transcript.lines() {
            meter.feed_line(line);
        }
        meter.snapshot()
    }

    fn expected_claude() -> SessionEconomics {
        SessionEconomics {
            turns: 2,
            context_tokens: 80,
            peak_context_tokens: 175,
            compactions: 1,
            tool_result_bytes: BTreeMap::from([("Bash".to_string(), 40), ("Read".to_string(), 16)]),
            reads_repeated: 1,
            wall_clock: TurnTiming {
                count: 1,
                max_ms: 30_000,
                mean_ms: 30_000,
            },
            output_tokens: 16,
            usage_measured: true,
        }
    }

    fn expected_klaudia() -> SessionEconomics {
        SessionEconomics {
            turns: 2,
            context_tokens: 67,
            peak_context_tokens: 117,
            compactions: 1,
            tool_result_bytes: BTreeMap::from([("Read".to_string(), 16), ("shell".to_string(), 6)]),
            reads_repeated: 1,
            wall_clock: TurnTiming {
                count: 1,
                max_ms: 60_000,
                mean_ms: 60_000,
            },
            output_tokens: 0,
            usage_measured: false,
        }
    }

    fn expected_codex() -> SessionEconomics {
        SessionEconomics {
            turns: 3,
            context_tokens: 191,
            peak_context_tokens: 191,
            compactions: 0,
            tool_result_bytes: BTreeMap::from([
                ("cat".to_string(), 10),
                ("grep".to_string(), 9),
                ("shell".to_string(), 11),
            ]),
            reads_repeated: 1,
            wall_clock: TurnTiming {
                count: 2,
                max_ms: 45_000,
                mean_ms: 22_500,
            },
            output_tokens: 0,
            usage_measured: false,
        }
    }

    #[test]
    fn claude_fixture_matches_python() {
        let transcript = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/economics/claude.jsonl"
        ));
        assert_eq!(feed("claude", transcript), expected_claude());
    }

    #[test]
    fn klaudia_fixture_matches_python() {
        let transcript = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/economics/klaudia.jsonl"
        ));
        assert_eq!(feed("klaudia", transcript), expected_klaudia());
    }

    #[test]
    fn codex_fixture_matches_python() {
        let transcript = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/economics/codex.jsonl"
        ));
        assert_eq!(feed("codex", transcript), expected_codex());
    }

    #[test]
    fn feeding_in_two_halves_equals_feeding_at_once() {
        let transcript = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/economics/claude.jsonl"
        ));
        let lines: Vec<&str> = transcript.lines().collect();
        let mid = lines.len() / 2;
        let mut meter = Meter::new("claude");
        for line in &lines[..mid] {
            meter.feed_line(line);
        }
        for line in &lines[mid..] {
            meter.feed_line(line);
        }
        assert_eq!(meter.snapshot(), feed("claude", transcript));
    }
    /// Non-ASCII text, a tool result with no content and a half-millisecond
    /// mean, asserted with the numbers Python's `meter` gives for the same
    /// file (tests/test_session_economics.py).
    #[test]
    fn unicode_fixture_matches_python() {
        let reading = feed(
            "klaudia",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/economics/klaudia_unicode.jsonl"
            )),
        );
        assert_eq!(reading.turns, 3);
        assert_eq!(reading.context_tokens, 127);
        assert_eq!(reading.tool_result_bytes.get("Grep"), Some(&90));
        assert_eq!(reading.tool_result_bytes.get("Read"), Some(&10));
        assert_eq!(reading.wall_clock.count, 2);
        assert_eq!(reading.wall_clock.max_ms, 3);
        assert_eq!(reading.wall_clock.mean_ms, 2);
    }

    #[test]
    fn halves_round_to_even_like_python() {
        assert_eq!(round_half_even(5, 2), 2);
        assert_eq!(round_half_even(7, 2), 4);
        assert_eq!(round_half_even(10, 4), 2);
        assert_eq!(round_half_even(11, 4), 3);
    }
}
