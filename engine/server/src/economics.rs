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
    /// Codex's own `token_count` readings. Once one is seen the session is
    /// measured and these replace the estimate.
    codex: Option<CodexUsage>,
    /// A Codex `compacted` line not yet followed by a `token_count`: an
    /// `item_completed` ContextCompaction in that gap is the same compaction.
    codex_compaction_open: bool,
}

/// What Codex's `token_count` events said, latest and largest.
#[derive(Default)]
struct CodexUsage {
    context: i64,
    peak: i64,
    window: i64,
    output: i64,
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
            codex: None,
            codex_compaction_open: false,
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

        let codex_compaction = match codex_compaction(entry) {
            Some(CodexCompaction::Rollout) => {
                self.codex_compaction_open = true;
                true
            }
            Some(CodexCompaction::Item) => {
                !std::mem::replace(&mut self.codex_compaction_open, false)
            }
            None => false,
        };
        if let Some(reading) = codex_token_count(entry) {
            self.codex_compaction_open = false;
            let codex = self.codex.get_or_insert_with(CodexUsage::default);
            codex.context = reading.context;
            codex.peak = codex.peak.max(reading.context);
            if reading.window > 0 {
                codex.window = reading.window;
            }
            match reading.total_output {
                Some(total) => codex.output = total,
                None => codex.output += reading.last_output,
            }
        }

        if codex_compaction || is_compaction(entry) {
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
        let mut output = self.output_by_turn.values().sum::<i64>() + self.unkeyed_output;
        let mut window = 0;
        if let Some(codex) = &self.codex {
            context = codex.context;
            peak = codex.peak;
            output = codex.output;
            window = codex.window;
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
            output_tokens: output,
            usage_measured: (self.measured && peak > 0) || self.codex.is_some(),
            context_window_tokens: window,
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

enum CodexCompaction {
    /// The rollout's own `compacted` item.
    Rollout,
    /// An `item_completed` event whose item is a ContextCompaction.
    Item,
}

fn codex_compaction(entry: &serde_json::Map<String, Value>) -> Option<CodexCompaction> {
    let kind = entry.get("type").and_then(Value::as_str);
    if kind == Some("compacted") {
        return Some(CodexCompaction::Rollout);
    }
    let payload = entry.get("payload").and_then(Value::as_object)?;
    if kind == Some("event_msg")
        && payload.get("type").and_then(Value::as_str) == Some("item_completed")
        && payload
            .get("item")
            .and_then(Value::as_object)
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            == Some("ContextCompaction")
    {
        return Some(CodexCompaction::Item);
    }
    None
}

struct CodexReading {
    context: i64,
    window: i64,
    total_output: Option<i64>,
    last_output: i64,
}

/// A Codex `event_msg` `token_count` with usage info. `last_token_usage` is
/// the latest request, so its `total_tokens` is what the window holds;
/// `total_token_usage` is cumulative and only its output count is used.
fn codex_token_count(entry: &serde_json::Map<String, Value>) -> Option<CodexReading> {
    if entry.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = entry.get("payload").and_then(Value::as_object)?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let info = payload.get("info").and_then(Value::as_object)?;
    let last = info.get("last_token_usage").and_then(Value::as_object)?;
    Some(CodexReading {
        context: json_int(last.get("total_tokens")),
        window: json_int(info.get("model_context_window")),
        total_output: info
            .get("total_token_usage")
            .and_then(Value::as_object)
            .map(|total| json_int(total.get("output_tokens"))),
        last_output: json_int(last.get("output_tokens")),
    })
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
            c if (c as u32) < 0x20 || c == '\u{7f}' => 6,
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

// --- live tailer (phase A2) ---------------------------------------------------

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vogt_engine_contract::AgentConversation;

use crate::app::AppState;

/// One session's place in its transcript: the file being tailed, how far it
/// has been read, the line a previous read left unfinished, and the meter
/// those lines feed.
pub(crate) struct Tail {
    agent: String,
    /// The transcript resolved for this conversation. Kept so a tick does not
    /// scan the agent's directory again; cleared when a read fails, so the
    /// next tick looks once more.
    path: Option<PathBuf>,
    offset: u64,
    /// The bytes of a line a read ended in the middle of. Bytes, not text: a
    /// character split across two reads must be decoded whole.
    partial: Vec<u8>,
    meter: Meter,
    /// When the snapshot was last written to the archive row. The tailer
    /// persists at most once a minute per session.
    persisted: Option<Instant>,
    /// The snapshot JSON last written. An unchanged snapshot is not written
    /// again, and does not restart the timer.
    persisted_json: Option<String>,
    /// Codex turn boundaries read since the tailer last took them (WI-1576).
    events: Vec<crate::codex_rollout::RolloutEvent>,
    /// A read of the current path has succeeded, so later events are live
    /// rather than the history a first read replays.
    caught_up: bool,
    /// The events held were read by a path's first read.
    replay: bool,
}

impl Tail {
    pub(crate) fn start(agent: &str) -> Self {
        Self {
            agent: agent.to_string(),
            path: None,
            offset: 0,
            partial: Vec::new(),
            meter: Meter::new(agent),
            persisted: None,
            persisted_json: None,
            events: Vec::new(),
            caught_up: false,
            replay: false,
        }
    }
}

/// Bytes one tick will read of one transcript. A transcript that races ahead
/// of this is caught up over later ticks rather than read in one go.
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// How often a session's snapshot is written back to its archive row.
const PERSIST_EVERY: Duration = Duration::from_secs(60);

/// How long one session's read may take before the tick moves on. An open
/// that blocks despite the non-blocking flag costs the tick this much and no
/// more, and only that session.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Read `path` from `offset`, at most [`MAX_READ_BYTES`]. `None` when the
/// file shrank past the offset or can no longer be read: the caller resets
/// and re-reads from the start.
///
/// The open refuses to follow a symlink (`O_NOFOLLOW`) and never blocks on a
/// file an agent swapped for a FIFO (`O_NONBLOCK`). Both checks are repeated
/// on the descriptor actually opened, so a swap between the check and the
/// open is caught too.
fn read_tail(path: &Path, offset: u64) -> Option<(u64, Vec<u8>)> {
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    if meta.len() < offset {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0; (meta.len() - offset).min(MAX_READ_BYTES) as usize];
    if buf.is_empty() {
        return Some((offset, buf));
    }
    let read = file.read(&mut buf).ok()?;
    buf.truncate(read);
    Some((offset + read as u64, buf))
}

/// Where a reported conversation keeps its transcript, under `home`. A
/// klaudia conversation is a session file under `~/.klaudia/sessions`, which
/// the claude layout already names.
pub(crate) fn transcript_path(
    home: &Path,
    claude_dirs: &[PathBuf],
    conversation: &AgentConversation,
) -> Option<PathBuf> {
    // The id is the only part of the path the agent chose. Anything that is
    // not a conversation id could walk out of the transcript root.
    if !crate::agent_cli::is_conversation_id(&conversation.id) {
        return None;
    }
    // A Claude conversation lives under whichever account's config directory
    // it ran in, so each root is tried until one holds it.
    let roots = match conversation.agent.as_str() {
        "claude" => crate::transcripts::claude_roots(home, claude_dirs),
        // Klaudia files Claude Code's layout under its own root (WI-950).
        "klaudia" => vec![home.join(".klaudia/sessions")],
        "codex" => vec![home.join(".codex/sessions")],
        _ => return None,
    };
    let (root, found) = roots.into_iter().find_map(|root| {
        let root = root.canonicalize().ok()?;
        let found = if conversation.agent == "codex" {
            crate::transcripts::codex_transcript(&root, &conversation.id)?
        } else {
            crate::transcripts::claude_transcript(&root, &conversation.id)?
        };
        Some((root, found))
    })?;
    // `is_file` follows links. A symlink planted in the transcript tree must
    // not be opened, so the entry itself has to be a regular file, and its
    // canonical path has to stay inside the canonical root.
    if !found.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let canonical = found.canonicalize().ok()?;
    if !canonical.starts_with(&root) {
        return None;
    }
    Some(canonical)
}

/// Advance one tail by whatever the transcript grew, or start it over when
/// the file moved or shrank. Returns the snapshot once the meter has seen a
/// turn, whether it is due to be written to the archive row, and the `now`
/// the caller passed so it can stamp the write it actually makes.
pub(crate) fn advance(
    tail: &mut Tail,
    path: &Path,
    now: Instant,
) -> Option<(SessionEconomics, bool, Instant)> {
    if tail.path.as_deref() != Some(path) {
        let agent = tail.agent.clone();
        let persisted_json = tail.persisted_json.clone();
        let persisted = tail.persisted;
        *tail = Tail::start(&agent);
        tail.path = Some(path.to_path_buf());
        tail.persisted_json = persisted_json;
        tail.persisted = persisted;
    }
    let (offset, bytes) = match read_tail(path, tail.offset) {
        Some(read) => read,
        None => {
            // The file moved, shrank, or was replaced. Forget it so the next
            // tick resolves the conversation again instead of re-reading a
            // path that no longer leads to the transcript.
            tail.path = None;
            tail.offset = 0;
            tail.partial.clear();
            tail.meter = Meter::new(&tail.agent);
            tail.caught_up = false;
            return None;
        }
    };
    tail.replay = !tail.caught_up;
    tail.caught_up = true;
    tail.offset = offset;
    feed_bytes(tail, &bytes);

    let snapshot = tail.meter.snapshot();
    if snapshot.turns == 0 {
        return None;
    }
    let due = match tail.persisted {
        Some(at) => now.duration_since(at) >= PERSIST_EVERY,
        None => true,
    };
    Some((snapshot, due, now))
}

/// Fold transcript bytes into the meter, line by line. A line that runs past
/// [`MAX_READ_BYTES`] without a newline is dropped rather than carried, so a
/// transcript with no newline cannot grow the tail without bound. Bytes are
/// decoded only once a line is complete, so a character split between two
/// reads survives.
fn feed_bytes(tail: &mut Tail, bytes: &[u8]) {
    let mut rest = bytes;
    while let Some(at) = rest.iter().position(|b| *b == b'\n') {
        if tail.partial.len() + at > MAX_READ_BYTES as usize {
            tail.partial.clear();
        } else {
            tail.partial.extend_from_slice(&rest[..at]);
            let line = String::from_utf8_lossy(&tail.partial);
            let line = line.trim_end_matches('\r');
            tail.meter.feed_line(line);
            if tail.agent == "codex" {
                tail.events.extend(crate::codex_rollout::parse_line(line));
            }
            tail.partial.clear();
        }
        rest = &rest[at + 1..];
    }
    if tail.partial.len() + rest.len() > MAX_READ_BYTES as usize {
        tail.partial.clear();
    } else {
        tail.partial.extend_from_slice(rest);
    }
}

/// Tail every live session's reported transcript and keep its economics
/// snapshot current. `tick` is `ENGINE_ECONOMICS_TICK_SECS`; the caller does
/// not spawn this when that is zero.
pub fn spawn_economics_tailer(state: Arc<AppState>, home: PathBuf, tick: Duration) {
    let claude_dirs = state.config.agent_onboarding.claude_dirs();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tick);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            for session in state.sessions.live_sessions() {
                let Some(conversation) = session.conversation() else {
                    continue;
                };
                let agent = conversation.agent.clone();
                let mut tail = session
                    .take_economics_tail()
                    .unwrap_or_else(|| Tail::start(&agent));
                let home = home.clone();
                let claude_dirs = claude_dirs.clone();
                // One bad transcript must not stall the rest of the tick, so
                // the blocking work is bounded and a timeout skips the session
                // until the next tick.
                let worked = tokio::time::timeout(
                    READ_TIMEOUT,
                    tokio::task::spawn_blocking(move || {
                        let now = Instant::now();
                        let path = match tail.path.clone() {
                            Some(path) => path,
                            None => match transcript_path(&home, &claude_dirs, &conversation) {
                                Some(path) => path,
                                None => return (tail, None),
                            },
                        };
                        let outcome = advance(&mut tail, &path, now);
                        (tail, outcome)
                    }),
                )
                .await;
                let Ok(Ok((mut tail, outcome))) = worked else {
                    continue;
                };
                // A Codex rollout's turn boundaries become ready and done
                // (WI-1576). Applied even with no events once the rollout is
                // read, so the screen path stands down for it.
                if tail.agent == "codex" && tail.caught_up {
                    let events = std::mem::take(&mut tail.events);
                    crate::codex_rollout::apply(
                        &session,
                        &state.sessions,
                        &state.bus,
                        &events,
                        tail.replay,
                    );
                }
                let (snapshot, mut persist, now) = match &outcome {
                    Some((snapshot, due, now)) => (Some(snapshot.clone()), *due, Some(*now)),
                    None => (None, false, None),
                };
                let json = snapshot
                    .as_ref()
                    .and_then(|s| serde_json::to_string(s).ok());
                // A snapshot identical to the one last written is not written
                // again, and its timer is not restarted.
                if let (Some(json), Some(previous)) = (&json, &tail.persisted_json) {
                    if json == previous {
                        persist = false;
                    }
                }
                if persist {
                    tail.persisted = now;
                    tail.persisted_json = json.clone();
                }
                session.store_economics_tail(tail, snapshot.clone());
                // The hibernation record is what a later removal counts, so it
                // has to carry the snapshot the session ends with.
                if let Some(json) = json.clone() {
                    state.sessions.set_record_economics(session.id, json);
                }
                if persist {
                    if let Some(json) = json {
                        persist_snapshot(&state, session.id, json).await;
                    }
                }
            }
        }
    });
}

async fn persist_snapshot(state: &AppState, id: uuid::Uuid, json: String) {
    let Some(history) = &state.history else {
        return;
    };
    if let Err(e) = history.set_economics(id, &json).await {
        tracing::warn!(session = %id, error = %e, "failed to persist session economics");
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
            context_window_tokens: 0,
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
            context_window_tokens: 0,
        }
    }

    fn expected_codex() -> SessionEconomics {
        SessionEconomics {
            turns: 3,
            context_tokens: 625,
            peak_context_tokens: 940,
            compactions: 1,
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
            output_tokens: 65,
            usage_measured: true,
            context_window_tokens: 272_000,
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

    /// A character cut at a chunk boundary must decode whole. Feeding every
    /// fixture in chunks of these sizes has to give the same reading as
    /// feeding it in one go, including the non-ASCII fixture.
    #[test]
    fn chunked_feed_matches_a_whole_feed() {
        let fixtures = [
            ("claude", "claude.jsonl"),
            ("claude", "claude_gap.jsonl"),
            ("codex", "codex.jsonl"),
            ("klaudia", "klaudia.jsonl"),
            ("klaudia", "klaudia_unicode.jsonl"),
        ];
        for (agent, name) in fixtures {
            let bytes = std::fs::read(format!(
                "{}/tests/fixtures/economics/{name}",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            let whole = feed(agent, std::str::from_utf8(&bytes).unwrap());
            for size in [1usize, 2, 3, 7, 64] {
                let mut tail = Tail::start(agent);
                for chunk in bytes.chunks(size) {
                    feed_bytes(&mut tail, chunk);
                }
                assert_eq!(tail.meter.snapshot(), whole, "{name} in chunks of {size}");
            }
        }
    }

    /// A transcript with no newline must not grow the carried line without
    /// bound: past the read cap it is dropped, and the next real line is read.
    #[test]
    fn a_line_past_the_cap_is_dropped() {
        let mut tail = Tail::start("claude");
        // Exactly the cap is still carried; one byte past it is dropped, and
        // the newline that would have ended it starts nothing.
        feed_bytes(&mut tail, &vec![b'x'; MAX_READ_BYTES as usize + 1]);
        assert!(tail.partial.is_empty());
        // The dropped run would have been a turn had it been kept. The line
        // after the newline that ends it is one, and it is counted.
        feed_bytes(&mut tail, b"\n");
        feed_bytes(
            &mut tail,
            b"{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"content\":[]}}\n",
        );
        assert_eq!(tail.meter.snapshot().turns, 1);
        assert!(tail.partial.is_empty());
    }

    #[test]
    fn halves_round_to_even_like_python() {
        assert_eq!(round_half_even(5, 2), 2);
        assert_eq!(round_half_even(7, 2), 4);
        assert_eq!(round_half_even(10, 4), 2);
        assert_eq!(round_half_even(11, 4), 3);
    }
    /// A 1.001 s gap and a DEL in a tool input (tests/test_session_economics.py
    /// asserts the same numbers).
    #[test]
    fn gap_fixture_matches_python() {
        let reading = feed(
            "claude",
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/economics/claude_gap.jsonl"
            )),
        );
        assert_eq!(reading.wall_clock.max_ms, 1001);
        assert_eq!(reading.wall_clock.mean_ms, 1001);
        assert_eq!(reading.tool_result_bytes.get("Grep"), Some(&71));
    }

    /// Feed `lines` to a fresh meter for `agent`, one line at a time.
    fn feed_all(agent: &str, lines: &[&str]) -> SessionEconomics {
        let mut meter = Meter::new(agent);
        for line in lines {
            meter.feed_line(line);
        }
        meter.snapshot()
    }

    /// Python: test_claude_usage_is_the_latest_turn_and_peak_is_the_largest
    #[test]
    fn claude_usage_is_the_latest_turn_and_peak_is_the_largest() {
        let bash_result = format!(
            r#"{{"type":"assistant","timestamp":"2026-10-11T01:00:00Z","message":{{"model":"claude","usage":{{"input_tokens":100,"cache_read_input_tokens":50,"cache_creation_input_tokens":25,"output_tokens":10}},"content":[{{"type":"tool_result","name":"Bash","content":"{}"}}]}}}}"#,
            "x".repeat(40)
        );
        let reading = feed_all(
            "claude",
            &[
                r#"{"type":"user","message":{"role":"user","content":"go"}}"#,
                &bash_result,
                r#"{"type":"assistant","timestamp":"2026-10-11T01:00:30Z","message":{"usage":{"input_tokens":80,"output_tokens":5},"content":[]}}"#,
            ],
        );
        assert_eq!(reading.turns, 2);
        // The latest turn, not the sum: 100+50+25 would double-count history.
        assert_eq!(reading.context_tokens, 80);
        assert_eq!(reading.peak_context_tokens, 175);
        assert_eq!(reading.output_tokens, 15);
        assert!(reading.usage_measured);
        assert_eq!(reading.compactions, 0);
        assert_eq!(reading.tool_result_bytes.get("Bash"), Some(&40));
        assert_eq!(reading.wall_clock.count, 1);
        assert_eq!(reading.wall_clock.max_ms, 30_000);
        assert_eq!(reading.wall_clock.mean_ms, 30_000);
    }

    /// Python: test_claude_compaction_marker_counts_wherever_it_sits
    #[test]
    fn claude_compaction_marker_counts_wherever_it_sits() {
        let reading = feed_all(
            "claude",
            &[
                r#"{"type":"system","isCompactSummary":true}"#,
                r#"{"type":"assistant","message":{"isCompactSummary":true,"usage":{"input_tokens":1}}}"#,
            ],
        );
        assert_eq!(reading.compactions, 2);
        assert_eq!(reading.turns, 1);
    }

    /// Python: test_klaudia_estimates_and_counts_its_own_compaction_marker
    #[test]
    fn klaudia_estimates_and_counts_its_own_compaction_marker() {
        let boundary = r#"{"type":"system","subtype":"compact_boundary"}"#;
        let body = format!(
            r#"{{"type":"assistant","message":{{"content":"{}"}}}}"#,
            "y".repeat(CHARS_PER_TOKEN as usize * 10)
        );
        let reading = feed_all("klaudia", &[boundary, &body]);
        let total = (boundary.len() + body.len()) as i64;
        assert!(!reading.usage_measured);
        assert_eq!(reading.compactions, 1);
        assert_eq!(reading.turns, 1);
        assert_eq!(reading.context_tokens, total / CHARS_PER_TOKEN);
        assert_eq!(reading.peak_context_tokens, reading.context_tokens);
    }

    /// Python: test_a_repeated_read_counts_once_per_path
    #[test]
    fn a_repeated_read_counts_once_per_path() {
        let read = |path: &str| {
            format!(
                r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","name":"Read","input":{{"file_path":"{path}"}},"content":"contents"}}]}}}}"#
            )
        };
        let (a, b) = (read("/a.py"), read("/b.py"));
        let reading = feed_all("klaudia", &[&a, &a, &b, &a]);
        assert_eq!(reading.reads_repeated, 1);
        assert_eq!(reading.tool_result_bytes.get("Read"), Some(&(8 * 4)));
    }

    /// Python: test_codex_results_under_payload_are_counted
    #[test]
    fn codex_results_under_payload_are_counted() {
        let line = r#"{"type":"tool_result","payload":{"content":[{"type":"tool_result","tool":"shell","content":"zzzzzzzz"}]}}"#;
        let reading = feed_all("codex", &[line]);
        assert_eq!(reading.turns, 0);
        assert_eq!(reading.tool_result_bytes.get("shell"), Some(&8));
        assert_eq!(reading.tool_result_bytes.len(), 1);
        assert_eq!(reading.context_tokens, line.len() as i64 / CHARS_PER_TOKEN);
    }

    /// Python: test_garbage_lines_are_skipped_and_an_empty_transcript_is_zero
    #[test]
    fn garbage_lines_are_skipped_and_an_empty_transcript_is_zero() {
        let reading = feed_all("claude", &["not json", "", "[]"]);
        assert_eq!(reading.turns, 0);
        assert_eq!(reading.context_tokens, 0);
        assert!(!reading.usage_measured);
        assert_eq!(reading.wall_clock.count, 0);
    }

    /// Python: test_one_timestamped_turn_has_no_wall_clock
    #[test]
    fn one_timestamped_turn_has_no_wall_clock() {
        let reading = feed_all(
            "klaudia",
            &[r#"{"type":"assistant","timestamp":"2026-10-11T01:00:00Z","message":{}}"#],
        );
        assert_eq!(reading.wall_clock.count, 0);
    }

    /// Python: test_claude_counts_a_turn_once_across_its_content_blocks
    #[test]
    fn claude_counts_a_turn_once_across_its_content_blocks() {
        let block = |index: usize, tokens: i64| {
            format!(
                r#"{{"type":"assistant","timestamp":"2026-10-11T01:00:{index:02}Z","message":{{"id":"msg_1","usage":{{"input_tokens":{tokens},"output_tokens":1}},"content":[{{"type":"text","text":"part"}}]}}}}"#
            )
        };
        let other = r#"{"type":"assistant","timestamp":"2026-10-11T01:01:00Z","message":{"id":"msg_2","usage":{"input_tokens":9,"output_tokens":1}}}"#;
        let reading = feed_all("claude", &[&block(1, 40), &block(2, 40), other]);
        assert_eq!(reading.turns, 2);
        // One output reading per turn, not one per content-block entry.
        assert_eq!(reading.output_tokens, 2);
        assert_eq!(reading.wall_clock.count, 1);
    }

    /// Python: test_klaudia_compaction_is_a_system_subtype_and_resets_the_estimate
    #[test]
    fn klaudia_compaction_is_a_system_subtype_and_resets_the_estimate() {
        let archived = format!(
            r#"{{"type":"assistant","message":{{"content":"{}"}}}}"#,
            "y".repeat(CHARS_PER_TOKEN as usize * 100)
        );
        let live = format!(
            r#"{{"type":"assistant","message":{{"content":"{}"}}}}"#,
            "z".repeat(CHARS_PER_TOKEN as usize * 10)
        );
        let boundary = r#"{"type":"system","subtype":"compact_boundary"}"#;
        let reading = feed_all("klaudia", &[&archived, boundary, &live]);
        assert_eq!(reading.compactions, 1);
        assert_eq!(reading.turns, 2);
        let live_only = (boundary.len() + live.len()) as i64 / CHARS_PER_TOKEN;
        assert_eq!(reading.context_tokens, live_only);
        assert!(reading.peak_context_tokens > reading.context_tokens);
    }

    /// Python: test_claude_tool_results_are_named_from_the_tool_use_id
    #[test]
    fn claude_tool_results_are_named_from_the_tool_use_id() {
        let reading = feed_all(
            "claude",
            &[
                r#"{"type":"assistant","message":{"id":"msg_1","usage":{"input_tokens":10,"output_tokens":1},"content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"/a.py"}}]}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"contents"}]}}"#,
            ],
        );
        assert_eq!(reading.tool_result_bytes.get("Read"), Some(&8));
        assert_eq!(reading.tool_result_bytes.len(), 1);
        assert_eq!(reading.reads_repeated, 0);
    }

    /// Python: test_claude_output_tokens_count_once_per_turn_across_its_blocks
    #[test]
    fn claude_output_tokens_count_once_per_turn_across_its_blocks() {
        let blocks = [
            r#"{"type":"thinking","thinking":"hm"}"#,
            r#"{"type":"text","text":"hello"}"#,
            r#"{"type":"tool_use","id":"t1","name":"Read","input":{}}"#,
        ];
        let lines: Vec<String> = blocks
            .iter()
            .map(|block| {
                format!(
                    r#"{{"type":"assistant","message":{{"id":"msg_1","usage":{{"input_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":7}},"content":[{block}]}}}}"#
                )
            })
            .collect();
        let reading = feed_all(
            "claude",
            &lines.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(reading.turns, 1);
        assert_eq!(reading.output_tokens, 7);
    }
}
