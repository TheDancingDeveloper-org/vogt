//! `agent-activity` — what agents did, read from their own transcripts.
//!
//! Ports `src/vogt/collectors/agent_activity.py`. The application registers it
//! only when transcript roots are configured. Its scope is those roots, not
//! projects, so `project_scoped` is false and an unnamed project-narrowed sweep
//! leaves it out. `collect` returns nothing; the sweeper calls `scan`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use fancy_regex::Regex;
use serde_json::{json, Value};

use crate::core::{from_iso, py_repr, Moment, Project};
use crate::decisions::{
    dumps_secrets, is_error, py_str, python_json_dumps, result_excerpt, summarize_input,
    ServiceMatcher,
};
use crate::errors::VogtError;
use crate::storage::observed_types::{
    ActivityBatch, ActivityCall, ActivityResult, TranscriptCursor,
};

use super::base::{ActivityIndexer, Collection, Collector, CollectorContext};

pub const COLLECTOR_NAME: &str = "agent-activity";

const AGENTS: &[&str] = &["claude", "codex"];
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
const MAX_FILES_PER_ROOT: usize = 50_000;

struct Pending {
    tool: String,
    withheld: bool,
}

struct FileState {
    path: String,
    agent: String,
    agent_session_id: Option<String>,
    cwd: Option<String>,
    calls: Vec<ActivityCall>,
    results: Vec<ActivityResult>,
    pending: BTreeMap<String, Pending>,
}

/// Indexes agent transcripts under the configured roots.
pub struct AgentActivityCollector {
    roots: BTreeMap<String, PathBuf>,
    budget: i64,
    services: ServiceMatcher,
}

impl AgentActivityCollector {
    pub fn new(
        roots: BTreeMap<String, PathBuf>,
        budget_bytes: i64,
        services: &[(&str, &str)],
    ) -> Result<Self, String> {
        Ok(Self {
            roots: roots
                .into_iter()
                .map(|(agent, path)| (agent, expanduser(&path)))
                .collect(),
            budget: budget_bytes,
            services: ServiceMatcher::build(services)?,
        })
    }

    /// Read up to the byte budget of unread transcript, newest files first.
    fn read_batch(&self, cursors: &BTreeMap<String, TranscriptCursor>) -> ActivityBatch {
        let mut skipped = BTreeMap::new();
        let mut candidates: Vec<(i64, i64, String, PathBuf, i64)> = Vec::new();
        for (agent, root) in &self.roots {
            if !AGENTS.contains(&agent.as_str()) {
                skipped.insert(
                    root.display().to_string(),
                    format!("unknown transcript format {}", py_repr(agent)),
                );
                continue;
            }
            if !root.is_dir() {
                skipped.insert(root.display().to_string(), "not a directory".to_string());
                continue;
            }
            for path in transcripts(root) {
                let Ok(meta) = path.metadata() else {
                    continue;
                };
                let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
                let cursor = cursors.get(&path.display().to_string());
                if cursor.is_some_and(|cursor| cursor.offset == size) {
                    continue;
                }
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|time| time.as_secs() as i64)
                    .unwrap_or(0);
                let nanos = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|time| i64::from(time.subsec_nanos()))
                    .unwrap_or(0);
                candidates.push((mtime, nanos, agent.clone(), path, size));
            }
        }
        candidates.sort_by(|left, right| {
            right.0.cmp(&left.0).then(right.1.cmp(&left.1)).then(
                left.3
                    .display()
                    .to_string()
                    .cmp(&right.3.display().to_string()),
            )
        });

        let mut remaining = self.budget;
        let mut calls = Vec::new();
        let mut results = Vec::new();
        let mut new_cursors = Vec::new();
        let mut backlog: i64 = 0;
        let mut files: i64 = 0;
        for (_, _, agent, path, size) in candidates {
            let key = path.display().to_string();
            let cursor = cursors.get(&key);
            let offset = match cursor {
                Some(cursor) if cursor.offset <= size => cursor.offset,
                _ => 0,
            };
            if remaining <= 0 {
                backlog += size - offset;
                continue;
            }
            let reset = cursor.is_none_or(|cursor| cursor.offset > size);
            let mut state = FileState {
                path: key.clone(),
                agent: agent.clone(),
                agent_session_id: if reset {
                    None
                } else {
                    cursor.and_then(|cursor| cursor.agent_session_id.clone())
                },
                cwd: if reset {
                    None
                } else {
                    cursor.and_then(|cursor| cursor.cwd.clone())
                },
                calls: Vec::new(),
                results: Vec::new(),
                pending: BTreeMap::new(),
            };
            let end = match read_file(self, &mut state, &path, offset, remaining) {
                Ok(end) => end,
                Err(error) => {
                    skipped.insert(key, format!("OSError: {error}"));
                    continue;
                }
            };
            files += 1;
            let consumed = end - offset;
            remaining -= consumed;
            backlog += size - end;
            calls.append(&mut state.calls);
            results.append(&mut state.results);
            if end != offset || reset {
                new_cursors.push(TranscriptCursor {
                    path: key,
                    agent,
                    offset: end,
                    size,
                    agent_session_id: state.agent_session_id,
                    cwd: state.cwd,
                });
            }
        }
        ActivityBatch {
            calls,
            results,
            cursors: new_cursors,
            files,
            bytes_read: self.budget - remaining,
            backlog_bytes: backlog.max(0),
            skipped,
        }
    }
}

impl Collector for AgentActivityCollector {
    fn name(&self) -> &'static str {
        COLLECTOR_NAME
    }

    fn requires_network(&self) -> bool {
        false
    }

    fn project_scoped(&self) -> bool {
        false
    }

    fn collect(
        &mut self,
        _ctx: &mut CollectorContext<'_>,
        _project: &Project,
        _now: &mut dyn FnMut() -> Moment,
    ) -> Collection {
        Collection::ok(Vec::new())
    }
}

impl ActivityIndexer for AgentActivityCollector {
    fn scan(
        &mut self,
        cursors: &BTreeMap<String, TranscriptCursor>,
    ) -> Result<ActivityBatch, VogtError> {
        Ok(self.read_batch(cursors))
    }
}

fn read_file(
    collector: &AgentActivityCollector,
    state: &mut FileState,
    path: &Path,
    offset: i64,
    budget: i64,
) -> std::io::Result<i64> {
    let mut handle = BufReader::new(File::open(path)?);
    handle.seek(SeekFrom::Start(u64::try_from(offset).unwrap_or(0)))?;
    let mut position = offset;
    let mut line = Vec::new();
    while position - offset < budget {
        line.clear();
        let (got, ended) = read_capped(&mut handle, &mut line, MAX_LINE_BYTES + 1)?;
        if got == 0 {
            break;
        }
        if !ended {
            if line.len() <= MAX_LINE_BYTES {
                break;
            }
            let Some(skipped) = skip_line(&mut handle)? else {
                break;
            };
            position += i64::try_from(line.len() + skipped).unwrap_or(i64::MAX);
            continue;
        }
        position += i64::try_from(line.len()).unwrap_or(i64::MAX);
        parse_line(collector, state, &line);
    }
    Ok(position)
}

/// `readline(limit)`: at most `limit` bytes, `ended` when a newline was included.
fn read_capped<R: Read>(
    handle: &mut R,
    line: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<(usize, bool)> {
    let mut byte = [0_u8; 1];
    let mut ended = false;
    while line.len() < limit {
        let got = handle.read(&mut byte)?;
        if got == 0 {
            break;
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            ended = true;
            break;
        }
    }
    Ok((line.len(), ended))
}

fn skip_line<R: Read>(handle: &mut R) -> std::io::Result<Option<usize>> {
    let mut skipped = 0;
    let mut chunk = Vec::new();
    loop {
        chunk.clear();
        let (got, ended) = read_capped(handle, &mut chunk, MAX_LINE_BYTES)?;
        if got == 0 {
            return Ok(None);
        }
        skipped += got;
        if ended {
            return Ok(Some(skipped));
        }
    }
}

fn parse_line(collector: &AgentActivityCollector, state: &mut FileState, raw: &[u8]) {
    let Ok(entry) = serde_json::from_slice::<Value>(raw) else {
        return;
    };
    let Value::Object(_) = &entry else {
        return;
    };
    if state.agent == "claude" {
        claude(collector, state, &entry);
    } else {
        codex(collector, state, &entry);
    }
}

fn claude(collector: &AgentActivityCollector, state: &mut FileState, entry: &Value) {
    if let Some(session) = entry.get("sessionId").and_then(Value::as_str) {
        if !session.is_empty() {
            state.agent_session_id = Some(session.to_string());
        }
    } else if state.agent_session_id.is_none() {
        state.agent_session_id = Some(Path::new(&state.path).file_stem_string());
    }
    if let Some(cwd) = entry.get("cwd").and_then(Value::as_str) {
        if !cwd.is_empty() {
            state.cwd = Some(cwd.to_string());
        }
    }
    let Some(content) = entry.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    let at = timestamp(entry.get("timestamp"));
    for block in content {
        let Some(kind) = block.get("type").and_then(Value::as_str) else {
            continue;
        };
        if kind == "tool_use" {
            let (Some(call_id), Some(tool), Some(at)) = (
                block.get("id").and_then(Value::as_str),
                block.get("name").and_then(Value::as_str),
                at,
            ) else {
                continue;
            };
            record_call(
                collector,
                state,
                call_id,
                tool,
                block.get("input").unwrap_or(&Value::Null),
                at,
                None,
            );
        } else if kind == "tool_result" {
            let Some(call_id) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            let flagged = match block.get("is_error") {
                Some(Value::Bool(flag)) => Some(*flag),
                _ => None,
            };
            record_result(
                state,
                call_id,
                &text_of(block.get("content").unwrap_or(&Value::Null)),
                flagged,
                at,
            );
        }
    }
}

fn codex(collector: &AgentActivityCollector, state: &mut FileState, entry: &Value) {
    let Some(payload) = entry.get("payload").filter(|value| value.is_object()) else {
        return;
    };
    match entry.get("type").and_then(Value::as_str) {
        Some("session_meta") => {
            let session = payload
                .get("id")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .or_else(|| {
                    payload
                        .get("session_id")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                });
            if let Some(session) = session {
                state.agent_session_id = Some(session.to_string());
            }
            if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    state.cwd = Some(cwd.to_string());
                }
            }
        }
        Some("turn_context") => {
            if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                if !cwd.is_empty() {
                    state.cwd = Some(cwd.to_string());
                }
            }
        }
        Some("response_item") => codex_item(collector, state, entry, payload),
        _ => {}
    }
}

fn codex_item(
    collector: &AgentActivityCollector,
    state: &mut FileState,
    entry: &Value,
    payload: &Value,
) {
    if state.agent_session_id.is_none() {
        let stem = Path::new(&state.path).file_stem_string();
        state.agent_session_id = Some(uuid_tail(&stem).unwrap_or(stem));
    }
    let at = timestamp(entry.get("timestamp"));
    let (Some(item), Some(call_id)) = (
        payload.get("type").and_then(Value::as_str),
        payload.get("call_id").and_then(Value::as_str),
    ) else {
        return;
    };
    if item.ends_with("_call_output") {
        record_result(
            state,
            call_id,
            &text_of(payload.get("output").unwrap_or(&Value::Null)),
            None,
            at,
        );
        return;
    }
    let Some(at) = at else {
        return;
    };
    match item {
        "function_call" => {
            let Some(name) = payload.get("name").and_then(Value::as_str) else {
                return;
            };
            let arguments = payload.get("arguments").cloned().unwrap_or(Value::Null);
            let parsed = match &arguments {
                Value::String(text) => serde_json::from_str(text).unwrap_or(arguments),
                other => other.clone(),
            };
            record_call(collector, state, call_id, name, &parsed, at, None);
        }
        "custom_tool_call" => {
            let Some(name) = payload.get("name").and_then(Value::as_str) else {
                return;
            };
            match payload.get("input") {
                Some(Value::String(script)) => {
                    let commands = codex_commands(script);
                    if !commands.is_empty() {
                        record_call(
                            collector,
                            state,
                            call_id,
                            &format!("{name}_command"),
                            &json!({"command": commands.join(" ; ")}),
                            at,
                            Some(Value::String(script.clone())),
                        );
                        return;
                    }
                    record_call(
                        collector,
                        state,
                        call_id,
                        name,
                        &Value::String(script.clone()),
                        at,
                        None,
                    );
                }
                other => record_call(
                    collector,
                    state,
                    call_id,
                    name,
                    other.unwrap_or(&Value::Null),
                    at,
                    None,
                ),
            }
        }
        "local_shell_call" => {
            let command = payload
                .get("action")
                .and_then(|action| action.get("command"))
                .cloned()
                .unwrap_or(Value::Null);
            record_call(
                collector,
                state,
                call_id,
                "shell",
                &json!({"command": command}),
                at,
                None,
            );
        }
        _ => {}
    }
}

fn record_call(
    collector: &AgentActivityCollector,
    state: &mut FileState,
    call_id: &str,
    tool: &str,
    call_input: &Value,
    at: Moment,
    raw: Option<Value>,
) {
    let source = raw.as_ref().unwrap_or(call_input);
    let flat = match source {
        Value::String(text) => text.clone(),
        other => python_json_dumps(other, true),
    };
    let withheld = dumps_secrets(&flat);
    state.pending.insert(
        call_id.to_string(),
        Pending {
            tool: tool.to_string(),
            withheld,
        },
    );
    state.calls.push(ActivityCall {
        source_path: state.path.clone(),
        call_id: call_id.to_string(),
        agent: state.agent.clone(),
        agent_session_id: state
            .agent_session_id
            .clone()
            .unwrap_or_else(|| Path::new(&state.path).file_stem_string()),
        cwd: state.cwd.clone(),
        tool: tool.to_string(),
        summary: summarize_input(call_input),
        services: collector.services.tags(tool, source),
        withheld,
        at,
    });
}

fn record_result(
    state: &mut FileState,
    call_id: &str,
    output: &str,
    flagged: Option<bool>,
    at: Option<Moment>,
) {
    let pending = state.pending.remove(call_id);
    let tool = pending
        .as_ref()
        .map(|pending| pending.tool.as_str())
        .unwrap_or("");
    let error = is_error(output, flagged, tool);
    state.results.push(ActivityResult {
        source_path: state.path.clone(),
        call_id: call_id.to_string(),
        error,
        excerpt: result_excerpt(
            output,
            error,
            pending.is_some_and(|pending| pending.withheld),
        ),
        at,
    });
}

fn transcripts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut names: Vec<_> = entries.flatten().collect();
        names.sort_by_key(|entry| entry.file_name());
        let mut subdirs = Vec::new();
        for entry in names {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                subdirs.push(path);
            } else if file_type.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".jsonl"))
            {
                found.push(path);
                if found.len() >= MAX_FILES_PER_ROOT {
                    return found;
                }
            }
        }
        for subdir in subdirs.into_iter().rev() {
            stack.push(subdir);
        }
    }
    found
}

fn timestamp(value: Option<&Value>) -> Option<Moment> {
    let text = value?.as_str().filter(|text| !text.is_empty())?;
    from_iso(&text.replace('Z', "+00:00")).ok()
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let kind = block.get("type").and_then(Value::as_str)?;
                if kind == "text" || kind == "input_text" {
                    Some(
                        block
                            .get("text")
                            .map(|text| match text {
                                Value::String(text) => text.clone(),
                                other => py_str(other),
                            })
                            .unwrap_or_default(),
                    )
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => {
            let dumped = python_json_dumps(other, false);
            dumped.chars().take(65_536).collect()
        }
    }
}

fn uuid_tail(stem: &str) -> Option<String> {
    static UUID: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let pattern = UUID.get_or_init(|| {
        Regex::new(r"(?i)([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$").unwrap()
    });
    pattern
        .captures(stem)
        .ok()
        .flatten()
        .and_then(|caps| caps.get(1).map(|group| group.as_str().to_string()))
}

fn codex_commands(script: &str) -> Vec<String> {
    static CMD: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let pattern = CMD.get_or_init(|| {
        Regex::new(r#"(?x)\bcmd\s*:\s*(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|`([^`]*)`)"#)
            .unwrap()
    });
    let mut found = Vec::new();
    let mut start = 0;
    while let Ok(Some(caps)) = pattern.captures_from_pos(script, start) {
        let whole = caps.get(0).unwrap();
        if let Some(command) = caps.get(1).or_else(|| caps.get(2)).or_else(|| caps.get(3)) {
            found.push(command.as_str().to_string());
        }
        start = whole.end();
    }
    found
}

fn expanduser(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

trait FileStemString {
    fn file_stem_string(&self) -> String;
}

impl FileStemString for Path {
    fn file_stem_string(&self) -> String {
        self.file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t0() -> Moment {
        from_iso("2026-10-04T06:00:00+00:00").unwrap()
    }

    fn plus(seconds: i64) -> Moment {
        Moment::from_unix(t0().unix_seconds() + seconds, 0)
    }

    fn iso(moment: Moment) -> String {
        moment.to_iso()
    }

    fn write_jsonl(path: &Path, lines: &[Value]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut text = String::new();
        for line in lines {
            text.push_str(&serde_json::to_string(line).unwrap());
            text.push('\n');
        }
        std::fs::write(path, text).unwrap();
    }

    fn claude_use(
        session: &str,
        cwd: &str,
        id: &str,
        tool: &str,
        input: Value,
        at: Moment,
    ) -> Value {
        json!({
            "type": "assistant",
            "sessionId": session,
            "cwd": cwd,
            "timestamp": iso(at),
            "message": {"role": "assistant", "content": [{
                "type": "tool_use", "id": id, "name": tool, "input": input
            }]}
        })
    }

    fn claude_result(
        session: &str,
        cwd: &str,
        id: &str,
        output: Value,
        at: Moment,
        is_error: Option<bool>,
    ) -> Value {
        let mut block = json!({"type": "tool_result", "tool_use_id": id, "content": output});
        if let Some(flag) = is_error {
            block["is_error"] = json!(flag);
        }
        json!({
            "type": "user",
            "sessionId": session,
            "cwd": cwd,
            "timestamp": iso(at),
            "message": {"role": "user", "content": [block]}
        })
    }

    fn temp(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("vogt-activity-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn collector(roots: BTreeMap<String, PathBuf>, budget: i64) -> AgentActivityCollector {
        AgentActivityCollector::new(roots, budget, &[]).unwrap()
    }

    fn b64_secret() -> String {
        format!(
            "{}{}{}",
            "Qm9ndXNTZWNyZXQ", "Rm9yVGVzdHM0Mz", "IxMjM0NTY3ODkw"
        )
    }

    fn gh_classic() -> String {
        format!("{}{}_{}", "gh", "p", "Zq3".repeat(12))
    }

    #[test]
    fn python_dumps_matches_json_dumps_defaults() {
        let value = json!({"b": [1, "é😀"], "a": {"d": null, "c": true}});
        // json.dumps(value)
        assert_eq!(
            python_json_dumps(&value, false),
            r#"{"b": [1, "\u00e9\ud83d\ude00"], "a": {"d": null, "c": true}}"#
        );
        // json.dumps(value, sort_keys=True)
        assert_eq!(
            python_json_dumps(&value, true),
            r#"{"a": {"c": true, "d": null}, "b": [1, "\u00e9\ud83d\ude00"]}"#
        );
        // Separators inside strings are data, not structure.
        assert_eq!(
            python_json_dumps(&json!({"k": "a,b:c"}), false),
            r#"{"k": "a,b:c"}"#
        );
    }

    #[test]
    fn claude_calls_and_results_are_read_and_redacted() {
        let root = temp("claude");
        let file = root.join("-work-estate").join("conv-1.jsonl");
        let secret = b64_secret();
        write_jsonl(
            &file,
            &[
                json!({"type": "summary", "summary": "not a message"}),
                claude_use(
                    "conv-1",
                    "/work/estate",
                    "t1",
                    "Bash",
                    json!({"command": "gh pr view 7"}),
                    t0(),
                ),
                claude_result(
                    "conv-1",
                    "/work/estate",
                    "t1",
                    json!("title: fix"),
                    plus(2),
                    None,
                ),
                claude_use(
                    "conv-1",
                    "/work/estate",
                    "t2",
                    "Bash",
                    json!({"command": format!("curl -H 'Authorization: Bearer {secret}' https://x")}),
                    plus(5),
                ),
                claude_result(
                    "conv-1",
                    "/work/estate",
                    "t2",
                    json!([{"type": "text", "text": format!("HTTP 403 Forbidden token={}", gh_classic())}]),
                    plus(6),
                    Some(true),
                ),
            ],
        );
        let batch = collector(
            BTreeMap::from([("claude".to_string(), root.clone())]),
            1 << 20,
        )
        .read_batch(&BTreeMap::new());
        assert_eq!(
            batch
                .calls
                .iter()
                .map(|call| call.call_id.as_str())
                .collect::<Vec<_>>(),
            ["t1", "t2"]
        );
        assert_eq!(batch.calls[0].agent_session_id, "conv-1");
        assert_eq!(batch.calls[0].cwd.as_deref(), Some("/work/estate"));
        assert_eq!(batch.calls[0].services, ["github"]);
        assert!(!batch.calls[1].summary.contains(&secret));
        let results: BTreeMap<_, _> = batch
            .results
            .iter()
            .map(|result| (result.call_id.as_str(), result))
            .collect();
        assert!(!results["t1"].error);
        assert!(results["t1"].excerpt.is_none());
        assert!(results["t2"].error);
        let excerpt = results["t2"].excerpt.as_deref().unwrap();
        assert!(!excerpt.contains(&gh_classic()));
        assert!(excerpt.contains("403"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reading_waits_for_whole_lines() {
        let root = temp("partial");
        let file = root.join("p").join("c.jsonl");
        write_jsonl(
            &file,
            &[claude_use(
                "c",
                "/w",
                "t1",
                "Read",
                json!({"file_path": "/w/a"}),
                t0(),
            )],
        );
        let collector = collector(
            BTreeMap::from([("claude".to_string(), root.clone())]),
            1 << 20,
        );
        let first = collector.read_batch(&BTreeMap::new());
        assert_eq!(first.calls.len(), 1);
        std::fs::write(
            &file,
            format!(
                "{}\n{{\"type\": \"user\", \"sessionId\": \"c\", \"mess",
                std::fs::read_to_string(&file).unwrap().trim_end()
            ),
        )
        .unwrap();
        let cursors = first
            .cursors
            .iter()
            .map(|cursor| (cursor.path.clone(), cursor.clone()))
            .collect();
        // The stored cursor is at the complete line. Re-scan from a cursor that
        // has not seen the half line yet: the half line is left unread.
        let second = collector.read_batch(&cursors);
        assert!(second.calls.is_empty());
        assert!(second.results.is_empty());
        let cursor = second
            .cursors
            .iter()
            .find(|cursor| cursor.path == file.display().to_string());
        let size = i64::try_from(std::fs::metadata(&file).unwrap().len()).unwrap();
        if let Some(cursor) = cursor {
            assert!(cursor.offset < size);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_replaced_file_is_read_again_from_the_start() {
        let root = temp("replaced");
        let file = root.join("-work-estate").join("conv-1.jsonl");
        write_jsonl(
            &file,
            &[
                claude_use(
                    "conv-1",
                    "/work/estate",
                    "t1",
                    "Bash",
                    json!({"command": "ls"}),
                    t0(),
                ),
                claude_use(
                    "conv-1",
                    "/work/estate",
                    "t2",
                    "Bash",
                    json!({"command": "pwd"}),
                    t0(),
                ),
            ],
        );
        let collector = collector(
            BTreeMap::from([("claude".to_string(), root.clone())]),
            1 << 20,
        );
        let first = collector.read_batch(&BTreeMap::new());
        write_jsonl(
            &file,
            &[claude_use(
                "conv-2",
                "/w",
                "n1",
                "Bash",
                json!({"command": "ls"}),
                t0(),
            )],
        );
        let cursors = first
            .cursors
            .iter()
            .map(|cursor| (cursor.path.clone(), cursor.clone()))
            .collect();
        let batch = collector.read_batch(&cursors);
        assert_eq!(
            batch
                .calls
                .iter()
                .map(|call| call.call_id.as_str())
                .collect::<Vec<_>>(),
            ["n1"]
        );
        assert_eq!(batch.calls[0].agent_session_id, "conv-2");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_sweep_reads_no_more_than_its_budget() {
        let root = temp("budget");
        for n in 0..5 {
            let mut lines = Vec::new();
            for i in 0..20 {
                lines.push(claude_use(
                    &format!("c{n}"),
                    "/w",
                    &format!("x{i}"),
                    "Bash",
                    json!({"command": "ls ".repeat(50)}),
                    t0(),
                ));
            }
            write_jsonl(&root.join("p").join(format!("c{n}.jsonl")), &lines);
        }
        let total: i64 = std::fs::read_dir(root.join("p"))
            .unwrap()
            .flatten()
            .map(|entry| i64::try_from(entry.metadata().unwrap().len()).unwrap())
            .sum();
        let budget = total / 3;
        let collector = collector(
            BTreeMap::from([("claude".to_string(), root.clone())]),
            budget,
        );
        let batch = collector.read_batch(&BTreeMap::new());
        assert!(batch.bytes_read <= budget + 2_000);
        assert_eq!(batch.backlog_bytes, total - batch.bytes_read);
        let cursors = batch
            .cursors
            .iter()
            .cloned()
            .map(|cursor| (cursor.path.clone(), cursor))
            .collect();
        let second = collector.read_batch(&cursors);
        assert!(second.bytes_read > 0);
        let first_ids: std::collections::BTreeSet<_> = batch
            .calls
            .iter()
            .map(|call| (call.source_path.clone(), call.call_id.clone()))
            .collect();
        assert!(second
            .calls
            .iter()
            .all(|call| !first_ids.contains(&(call.source_path.clone(), call.call_id.clone()))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn codex_rollouts_are_read() {
        let root = temp("codex");
        let session = "01a08fac-ae53-7f70-952d-855ee60f0a64";
        let file = root
            .join("2026")
            .join("10")
            .join("04")
            .join(format!("rollout-2026-10-04T06-00-00-{session}.jsonl"));
        let at = iso(t0());
        write_jsonl(
            &file,
            &[
                json!({"timestamp": at, "type": "session_meta", "payload": {"id": session, "cwd": "/work/estate"}}),
                json!({"timestamp": at, "type": "response_item", "payload": {
                    "type": "custom_tool_call", "call_id": "c1", "name": "exec",
                    "input": "const r = await tools.exec_command({cmd:\"docker compose ps\"});"
                }}),
                json!({"timestamp": at, "type": "response_item", "payload": {
                    "type": "custom_tool_call_output", "call_id": "c1",
                    "output": [{"type": "input_text", "text": "Script failed\nboom"}]
                }}),
                json!({"timestamp": at, "type": "response_item", "payload": {
                    "type": "function_call", "call_id": "c2", "name": "spawn_agent",
                    "arguments": serde_json::to_string(&json!({"task_name": "review", "message": "go"})).unwrap()
                }}),
                json!({"timestamp": at, "type": "response_item", "payload": {
                    "type": "function_call_output", "call_id": "c2", "output": "started"
                }}),
            ],
        );
        let batch = collector(
            BTreeMap::from([("codex".to_string(), root.clone())]),
            1 << 20,
        )
        .read_batch(&BTreeMap::new());
        assert_eq!(batch.calls[0].tool, "exec_command");
        assert_eq!(batch.calls[0].summary, "docker compose ps");
        assert_eq!(batch.calls[1].tool, "spawn_agent");
        assert!(batch
            .calls
            .iter()
            .all(|call| call.agent_session_id == session));
        assert!(batch
            .calls
            .iter()
            .all(|call| call.cwd.as_deref() == Some("/work/estate")));
        assert_eq!(batch.calls[0].services, ["docker"]);
        let errors: BTreeMap<_, _> = batch
            .results
            .iter()
            .map(|result| (result.call_id.as_str(), result.error))
            .collect();
        assert_eq!(errors.get("c1"), Some(&true));
        assert_eq!(errors.get("c2"), Some(&false));
        let _ = std::fs::remove_dir_all(&root);
    }
}
