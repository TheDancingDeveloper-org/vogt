//! A session's own agent conversation, read from the agent's transcript.
//!
//! A faithful port of `src/vogt/adapters/transcripts.py`. Finding the
//! conversation, in order, with the basis reported so a caller can weigh a
//! guess: an id the session's command names (`--session-id`, `--resume`,
//! `codex resume`, `--session`), then the engine session id itself, then —
//! only when `allow_cwd_guess` — the newest transcript in the session's
//! directory written since the session started. Only the tail of one file is
//! read, and every message is redacted with the observability redactor before
//! it leaves here. Transcript content is untrusted data.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::observability;

/// The most of a transcript's tail read looking for replies.
pub const TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// How much of a transcript's end is read for the model it runs.
pub const RUNTIME_TAIL_BYTES: u64 = 256 * 1024;
/// One message is cut here.
pub const MAX_MESSAGE_CHARS: usize = 20_000;
/// Excerpts for session lists.
pub const EXCERPT_CHARS: usize = 300;
/// Directory entries considered per root, as a guard against a wrong root.
pub const MAX_ENTRIES: usize = 20_000;

const EXCERPTS_MAX: usize = 512;

/// Agents whose transcripts are Claude Code's JSONL.
const CLAUDE_FORMAT: &[&str] = &["claude", "klaudia"];

/// Where a session's conversation is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub agent: String,
    pub path: PathBuf,
    pub conversation_id: String,
    /// `session-id`, `resume-id`, `engine-id` or `cwd`.
    pub basis: String,
}

/// One assistant message, redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    pub at: Option<String>,
}

type CacheKey = (String, u128, u64);

struct Cache {
    excerpts: HashMap<CacheKey, Option<String>>,
    runtimes: HashMap<CacheKey, (Option<String>, Option<String>)>,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
    Mutex::new(Cache {
        excerpts: HashMap::new(),
        runtimes: HashMap::new(),
    })
});

/// Which transcript formats to look in, most likely first.
pub fn agent_of(command: Option<&str>, template: Option<&str>) -> Vec<&'static str> {
    let hint = format!("{} {}", command.unwrap_or(""), template.unwrap_or("")).to_lowercase();
    if hint.contains("klaudia") {
        vec!["klaudia"]
    } else if hint.contains("codex") {
        vec!["codex"]
    } else if hint.contains("claude") {
        vec!["claude"]
    } else {
        vec!["claude", "codex"]
    }
}

/// Conversation ids the command line names, with what named them.
pub fn named_ids(command: Option<&str>) -> Vec<(String, &'static str)> {
    let Some(command) = command else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (flag, value) in flag_values(command) {
        let basis = if flag == "--session-id" {
            "session-id"
        } else {
            "resume-id"
        };
        if is_id(&value) {
            found.push((value, basis));
        }
    }
    for value in codex_resume_values(command) {
        if is_id(&value) {
            found.push((value, "resume-id"));
        }
    }
    found
}

/// The directory name Claude Code files a conversation under for `cwd`.
/// Everything outside `[A-Za-z0-9-]` becomes `-`.
pub fn claude_key(cwd: &str) -> String {
    cwd.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

/// The transcript a session's conversation is in, or `None`.
pub fn find(
    roots: &HashMap<&str, &Path>,
    engine_session_id: &str,
    command: Option<&str>,
    template: Option<&str>,
    cwd: Option<&str>,
    started_at: Option<SystemTime>,
    allow_cwd_guess: bool,
) -> Option<Transcript> {
    let agents = agent_of(command, template);
    let mut candidates = named_ids(command);
    if is_id(engine_session_id) {
        candidates.push((engine_session_id.to_string(), "engine-id"));
    }
    for agent in agents {
        let Some(root) = roots.get(agent) else {
            continue;
        };
        if !root.is_dir() {
            continue;
        }
        for (conversation_id, basis) in &candidates {
            if let Some(path) = by_id(agent, root, conversation_id) {
                return Some(Transcript {
                    agent: agent.to_string(),
                    path,
                    conversation_id: conversation_id.clone(),
                    basis: (*basis).to_string(),
                });
            }
        }
        if allow_cwd_guess {
            if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
                if let Some(guessed) = by_cwd(agent, root, cwd, started_at) {
                    return Some(guessed);
                }
            }
        }
    }
    None
}

/// The last `n` assistant messages, oldest first, redacted.
pub fn last_replies(transcript: &Transcript, n: usize) -> Vec<Reply> {
    let lines = tail_lines(&transcript.path, TAIL_BYTES);
    let messages = if CLAUDE_FORMAT.contains(&transcript.agent.as_str()) {
        claude_messages(&lines)
    } else {
        codex_messages(&lines)
    };
    let picked = if n > 0 {
        messages.into_iter().rev().take(n).rev().collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    picked
        .into_iter()
        .map(|(text, at)| Reply {
            text: cut(&observability::redact(&text), MAX_MESSAGE_CHARS),
            at,
        })
        .collect()
}

/// A reply from somewhere other than a transcript file, redacted and cut the
/// same way.
pub fn reply(text: &str, at: Option<String>) -> Reply {
    Reply {
        text: cut(&observability::redact(text), MAX_MESSAGE_CHARS),
        at,
    }
}

/// The one-line list excerpt of a reply's (redacted) text.
pub fn excerpt(text: &str) -> String {
    cut(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        EXCERPT_CHARS,
    )
}

/// A redacted one-line excerpt of the latest reply, cached by the file's size
/// and modification time.
pub fn last_reply_excerpt(transcript: &Transcript) -> Option<String> {
    let (mtime_ns, size) = stat_key(&transcript.path)?;
    let key = (transcript.path.display().to_string(), mtime_ns, size);
    if let Some(cached) = CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.excerpts.get(&key).cloned())
    {
        return cached;
    }
    let replies = last_replies(transcript, 1);
    let value = replies.last().map(|reply| excerpt(&reply.text));
    if let Ok(mut cache) = CACHE.lock() {
        if cache.excerpts.len() >= EXCERPTS_MAX {
            cache.excerpts.clear();
        }
        cache.excerpts.insert(key, value.clone());
    }
    value
}

/// The model (and, for Codex, the reasoning effort) the conversation's latest
/// turn ran on. Cached like the excerpt.
pub fn runtime(transcript: &Transcript) -> (Option<String>, Option<String>) {
    let Some((mtime_ns, size)) = stat_key(&transcript.path) else {
        return (None, None);
    };
    let key = (transcript.path.display().to_string(), mtime_ns, size);
    if let Some(cached) = CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.runtimes.get(&key).cloned())
    {
        return cached;
    }
    let lines = tail_lines(&transcript.path, RUNTIME_TAIL_BYTES);
    let mut model: Option<String> = None;
    let mut effort: Option<String> = None;
    for entry in parsed(&lines) {
        if CLAUDE_FORMAT.contains(&transcript.agent.as_str()) {
            if entry.get("type").and_then(Value::as_str) == Some("assistant") {
                if let Some(message) = entry.get("message").and_then(Value::as_object) {
                    if let Some(found) = message.get("model").and_then(Value::as_str) {
                        // `<synthetic>` is a message Claude Code made up itself.
                        if !found.is_empty() && !found.starts_with('<') {
                            model = Some(found.to_string());
                        }
                    }
                }
            }
        } else if entry.get("type").and_then(Value::as_str) == Some("turn_context") {
            if let Some(payload) = entry.get("payload").and_then(Value::as_object) {
                if let Some(found) = payload.get("model").and_then(Value::as_str) {
                    model = Some(found.to_string());
                }
                for name in ["effort", "reasoning_effort", "model_reasoning_effort"] {
                    if let Some(found) = payload.get(name).and_then(Value::as_str) {
                        effort = Some(found.to_string());
                    }
                }
            }
        }
    }
    let found_runtime = (model, effort);
    if let Ok(mut cache) = CACHE.lock() {
        if cache.runtimes.len() >= EXCERPTS_MAX {
            cache.runtimes.clear();
        }
        cache.runtimes.insert(key, found_runtime.clone());
    }
    found_runtime
}

/// Forget every cached excerpt (tests).
pub fn clear_excerpt_cache() {
    if let Ok(mut cache) = CACHE.lock() {
        cache.excerpts.clear();
        cache.runtimes.clear();
    }
}

pub fn excerpt_cache_size() -> usize {
    CACHE.lock().map(|cache| cache.excerpts.len()).unwrap_or(0)
}

// -- lookup --------------------------------------------------------------------

fn by_id(agent: &str, root: &Path, conversation_id: &str) -> Option<PathBuf> {
    if CLAUDE_FORMAT.contains(&agent) {
        let name = format!("{conversation_id}.jsonl");
        for entry in entries(root) {
            let candidate = entry.join(&name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        return None;
    }
    let suffix = format!("-{conversation_id}.jsonl");
    codex_files(root).find(|path| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(&suffix))
    })
}

fn by_cwd(
    agent: &str,
    root: &Path,
    cwd: &str,
    started_at: Option<SystemTime>,
) -> Option<Transcript> {
    let since = started_at
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or(Duration::ZERO);
    let files: Vec<PathBuf> = if CLAUDE_FORMAT.contains(&agent) {
        let directory = root.join(claude_key(cwd));
        if !directory.is_dir() {
            return None;
        }
        fs::read_dir(&directory)
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().and_then(|e| e.to_str()) == Some("jsonl") && mtime(path) >= since
            })
            .collect()
    } else {
        codex_files(root)
            .filter(|path| mtime(path) >= since && codex_cwd(path).as_deref() == Some(cwd))
            .collect()
    };
    let newest = files.into_iter().max_by_key(|path| mtime(path))?;
    let stem = newest
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    let conversation = if agent == "codex" {
        uuid_tail(&stem).unwrap_or(stem)
    } else {
        stem
    };
    Some(Transcript {
        agent: agent.to_string(),
        path: newest,
        conversation_id: conversation,
        basis: "cwd".to_string(),
    })
}

fn entries(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(read) = fs::read_dir(root) else {
        return out;
    };
    for (count, entry) in read.filter_map(|e| e.ok()).enumerate() {
        if count >= MAX_ENTRIES {
            break;
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            out.push(entry.path());
        }
    }
    out
}

/// Rollouts under `root/YYYY/MM/DD/`, newest dates first.
fn codex_files(root: &Path) -> impl Iterator<Item = PathBuf> {
    let mut out = Vec::new();
    let mut seen = 0usize;
    let mut years = entries(root);
    years.sort();
    years.reverse();
    'outer: for year in years {
        let mut months = entries(&year);
        months.sort();
        months.reverse();
        for month in months {
            let mut days = entries(&month);
            days.sort();
            days.reverse();
            for day in days {
                let mut names: Vec<String> = fs::read_dir(&day)
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| entry.ok())
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .collect();
                names.sort();
                names.reverse();
                for name in names {
                    seen += 1;
                    if seen > MAX_ENTRIES {
                        break 'outer;
                    }
                    if name.ends_with(".jsonl") {
                        out.push(day.join(name));
                    }
                }
            }
        }
    }
    out.into_iter()
}

fn codex_cwd(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; 256 * 1024];
    let read = file.read(&mut buf).ok()?;
    let line = buf[..read]
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or(&[]);
    let entry: Value = serde_json::from_slice(line).ok()?;
    entry
        .get("payload")
        .and_then(|p| p.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn mtime(path: &Path) -> Duration {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .unwrap_or(Duration::ZERO)
}

fn stat_key(path: &Path) -> Option<(u128, u64)> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some((modified.as_nanos(), meta.len()))
}

// -- reading -------------------------------------------------------------------

fn tail_lines(path: &Path, limit: u64) -> Vec<Vec<u8>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let start = size.saturating_sub(limit);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        return Vec::new();
    }
    let mut lines: Vec<&[u8]> = data.split(|byte| *byte == b'\n').collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // the first line is cut mid-way
    }
    lines
        .into_iter()
        .filter(|line| !line.iter().all(|byte| byte.is_ascii_whitespace()))
        .map(|line| line.to_vec())
        .collect()
}

fn parsed(lines: &[Vec<u8>]) -> Vec<Value> {
    lines
        .iter()
        .filter_map(|raw| serde_json::from_slice::<Value>(raw).ok())
        .filter(|entry| entry.is_object())
        .collect()
}

/// Assistant text, one item per API message (its streamed entries joined).
fn claude_messages(lines: &[Vec<u8>]) -> Vec<(String, Option<String>)> {
    let mut order: Vec<String> = Vec::new();
    let mut texts: HashMap<String, Vec<String>> = HashMap::new();
    let mut times: HashMap<String, Option<String>> = HashMap::new();
    for (index, entry) in parsed(lines).into_iter().enumerate() {
        if entry.get("type").and_then(Value::as_str) != Some("assistant")
            || entry.get("isSidechain").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let Some(message) = entry.get("message").and_then(Value::as_object) else {
            continue;
        };
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        let parts: Vec<String> = content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string)
            .collect();
        if parts.is_empty() {
            continue;
        }
        let key = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("line-{index}"));
        if !texts.contains_key(&key) {
            order.push(key.clone());
            times.insert(key.clone(), when(entry.get("timestamp")));
        }
        texts.entry(key).or_default().extend(parts);
    }
    order
        .into_iter()
        .map(|key| (texts[&key].join("\n\n"), times[&key].clone()))
        .collect()
}

fn codex_messages(lines: &[Vec<u8>]) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    for entry in parsed(lines) {
        if entry.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = entry.get("payload").and_then(Value::as_object) else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("message")
            || payload.get("role").and_then(Value::as_str) != Some("assistant")
        {
            continue;
        }
        let Some(content) = payload.get("content").and_then(Value::as_array) else {
            continue;
        };
        let text = content
            .iter()
            .filter(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("output_text" | "text")
                )
            })
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if !text.is_empty() {
            found.push((text, when(entry.get("timestamp"))));
        }
    }
    found
}

fn when(value: Option<&Value>) -> Option<String> {
    let text = value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())?;
    // Accept the ISO-8601 instants the agents write; anything else is no time.
    if chrono_like(text) {
        Some(text.to_string())
    } else {
        None
    }
}

fn chrono_like(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes
            .iter()
            .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'T' | b':' | b'.' | b'Z' | b'+'))
}

fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        let head: String = text.chars().take(limit - 1).collect();
        format!("{head}…")
    }
}

// -- command parsing -----------------------------------------------------------

fn is_id(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphanumeric() || matches!(first, '.' | '_')) || value.chars().count() > 128
    {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// `(--session-id|--resume|--session)[= ]'?value`.
fn flag_values(command: &str) -> Vec<(&str, String)> {
    let mut out = Vec::new();
    let bytes = command.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(flag) = ["--session-id", "--resume", "--session"]
            .into_iter()
            .find(|flag| command[index..].starts_with(flag) && boundary(bytes, index))
        {
            let mut cursor = index + flag.len();
            // `--session` must not swallow the prefix of `--session-id`.
            if cursor < bytes.len() && is_id_byte(bytes[cursor]) {
                index += 1;
                continue;
            }
            if cursor < bytes.len() && (bytes[cursor] == b'=' || bytes[cursor] == b' ') {
                cursor += 1;
                if cursor < bytes.len() && bytes[cursor] == b'\'' {
                    cursor += 1;
                }
                let start = cursor;
                while cursor < bytes.len() && is_id_byte(bytes[cursor]) {
                    cursor += 1;
                }
                if cursor > start {
                    out.push((flag, command[start..cursor].to_string()));
                }
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
    out
}

fn codex_resume_values(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = command.as_bytes();
    let mut index = 0;
    while index + "codex".len() <= bytes.len() {
        if command[index..].starts_with("codex") && boundary(bytes, index) {
            let mut cursor = index + "codex".len();
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if command[cursor..].starts_with("resume") {
                cursor += "resume".len();
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if cursor < bytes.len() && bytes[cursor] == b'\'' {
                    cursor += 1;
                }
                let start = cursor;
                while cursor < bytes.len() && is_id_byte(bytes[cursor]) {
                    cursor += 1;
                }
                if cursor > start {
                    out.push(command[start..cursor].to_string());
                }
                index = cursor;
                continue;
            }
        }
        index += 1;
    }
    out
}

fn boundary(bytes: &[u8], index: usize) -> bool {
    index == 0 || !is_id_byte(bytes[index - 1])
}

fn is_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn uuid_tail(stem: &str) -> Option<String> {
    if stem.len() < 36 {
        return None;
    }
    let tail = &stem[stem.len() - 36..];
    let bytes = tail.as_bytes();
    let groups = [8, 4, 4, 4, 12];
    let mut cursor = 0;
    for (index, length) in groups.iter().enumerate() {
        if index > 0 {
            if bytes.get(cursor) != Some(&b'-') {
                return None;
            }
            cursor += 1;
        }
        if !bytes[cursor..cursor + length]
            .iter()
            .all(|b| b.is_ascii_hexdigit())
        {
            return None;
        }
        cursor += length;
    }
    Some(tail.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const ROOT_CWD: &str = "/srv/vogt";
    const ENGINE_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    fn secret() -> String {
        format!("ghp_{}", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8")
    }

    fn claude_line(message_id: &str, text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-10-05T00:01:00Z",
            "cwd": ROOT_CWD,
            "message": {"id": message_id, "role": "assistant", "content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    fn write_claude(root: &Path, conversation: &str, lines: &[String]) -> PathBuf {
        let directory = root.join(claude_key(ROOT_CWD));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{conversation}.jsonl"));
        fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        path
    }

    fn temp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vogt-transcripts-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn claude_key_replaces_everything_outside_the_alphabet() {
        assert_eq!(claude_key("/srv/vogt"), "-srv-vogt");
        assert_eq!(claude_key("a b_c.d"), "a-b-c-d");
    }

    #[test]
    fn claude_replies_are_grouped_by_message_and_redacted() {
        let base = temp();
        let root = base.join("claude");
        write_claude(
            &root,
            ENGINE_ID,
            &[
                serde_json::json!({"type": "user", "message": {"content": "hi"}}).to_string(),
                claude_line("m1", "First reply."),
                claude_line("m2", "Part one,"),
                serde_json::json!({"type": "assistant", "message": {"id": "m2", "content": [{"type": "tool_use", "name": "Bash"}]}}).to_string(),
                claude_line("m2", &format!("part two with {}.", secret())),
                serde_json::json!({"type": "assistant", "isSidechain": true, "message": {"id": "sub", "content": [{"type": "text", "text": "a subagent's reply"}]}}).to_string(),
                "not json".to_string(),
            ],
        );
        let mut roots = HashMap::new();
        roots.insert("claude", root.as_path());
        let found = find(
            &roots,
            ENGINE_ID,
            Some(&format!(
                "vogt-agent-auth run -- claude --session-id {ENGINE_ID}"
            )),
            None,
            Some(ROOT_CWD),
            None,
            false,
        )
        .unwrap();
        assert_eq!(found.basis, "session-id");
        assert_eq!(found.agent, "claude");
        let replies = last_replies(&found, 5);
        assert_eq!(
            replies
                .iter()
                .map(|r| r.text.split_whitespace().next().unwrap())
                .collect::<Vec<_>>(),
            ["First", "Part"]
        );
        assert!(replies.last().unwrap().text.contains("part two"));
        assert!(!replies.last().unwrap().text.contains(&secret()));
        assert!(replies.last().unwrap().at.is_some());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_resumed_or_engine_started_conversation_is_found_by_its_id() {
        let base = temp();
        let root = base.join("claude");
        write_claude(&root, "resumed-1", &[claude_line("m", "resumed reply")]);
        let mut roots = HashMap::new();
        roots.insert("claude", root.as_path());
        let by_resume = find(
            &roots,
            "11111111-2222-4333-8444-555555555555",
            Some("claude --resume resumed-1"),
            None,
            Some(ROOT_CWD),
            None,
            false,
        )
        .unwrap();
        assert_eq!(by_resume.basis, "resume-id");
        write_claude(&root, ENGINE_ID, &[claude_line("m", "pinned")]);
        let by_engine = find(
            &roots,
            ENGINE_ID,
            Some("claude"),
            Some("claude"),
            Some(ROOT_CWD),
            None,
            false,
        )
        .unwrap();
        assert_eq!(by_engine.basis, "engine-id");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_directory_guess_is_only_made_when_allowed() {
        let base = temp();
        let root = base.join("claude");
        write_claude(&root, "some-other-id", &[claude_line("m", "guessed")]);
        let mut roots = HashMap::new();
        roots.insert("claude", root.as_path());
        assert!(find(
            &roots,
            ENGINE_ID,
            Some("bash"),
            None,
            Some(ROOT_CWD),
            None,
            false
        )
        .is_none());
        let guessed = find(
            &roots,
            ENGINE_ID,
            Some("bash"),
            None,
            Some(ROOT_CWD),
            None,
            true,
        )
        .unwrap();
        assert_eq!(guessed.basis, "cwd");
        assert_eq!(guessed.conversation_id, "some-other-id");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn codex_rollouts_by_id_and_by_directory() {
        let base = temp();
        let root = base.join("codex");
        let day = root.join("2026").join("10").join("05");
        fs::create_dir_all(&day).unwrap();
        let conversation = "0199aaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let rollout = day.join(format!("rollout-2026-10-05T00-00-00-{conversation}.jsonl"));
        let body = [
            serde_json::json!({"type": "session_meta", "payload": {"id": conversation, "cwd": ROOT_CWD}}),
            serde_json::json!({"type": "response_item", "timestamp": "2026-10-05T00:02:00Z", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Codex says hi"}]}}),
            serde_json::json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "not a reply"}]}}),
        ]
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        fs::write(&rollout, format!("{body}\n")).unwrap();
        let mut roots = HashMap::new();
        roots.insert("codex", root.as_path());
        let by_id = find(
            &roots,
            ENGINE_ID,
            Some(&format!("codex resume {conversation}")),
            None,
            Some(ROOT_CWD),
            None,
            false,
        )
        .unwrap();
        assert_eq!(by_id.agent, "codex");
        assert_eq!(
            last_replies(&by_id, 3)
                .iter()
                .map(|r| r.text.clone())
                .collect::<Vec<_>>(),
            ["Codex says hi"]
        );
        let by_cwd = find(
            &roots,
            ENGINE_ID,
            Some("codex"),
            None,
            Some(ROOT_CWD),
            None,
            true,
        )
        .unwrap();
        assert_eq!(by_cwd.conversation_id, conversation);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_excerpt_is_cached_by_the_files_size_and_time() {
        clear_excerpt_cache();
        let base = temp();
        let path = write_claude(
            &base.join("claude"),
            ENGINE_ID,
            &[claude_line("m", &"word ".repeat(200))],
        );
        let found = Transcript {
            agent: "claude".to_string(),
            path: path.clone(),
            conversation_id: ENGINE_ID.to_string(),
            basis: "engine-id".to_string(),
        };
        let first = last_reply_excerpt(&found).unwrap();
        assert!(first.chars().count() <= EXCERPT_CHARS);
        assert_eq!(last_reply_excerpt(&found).as_deref(), Some(first.as_str()));
        // A new reply changes the size, so the cache misses and it is re-read.
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", claude_line("m9", "the newest reply")).unwrap();
        drop(file);
        assert_eq!(
            last_reply_excerpt(&found).as_deref(),
            Some("the newest reply")
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn named_ids_ignore_anything_that_is_not_an_id() {
        assert_eq!(
            named_ids(Some("claude --resume abc-1 --model x")),
            vec![("abc-1".to_string(), "resume-id")]
        );
        assert_eq!(
            named_ids(Some("claude --session-id 'u-1'")),
            vec![("u-1".to_string(), "session-id")]
        );
        assert!(named_ids(None).is_empty());
        // A flag value that is not an id never counts.
        assert!(named_ids(Some("claude --resume --model")).is_empty());
    }

    #[test]
    fn codex_records_model_and_effort_per_turn() {
        let base = temp();
        let conversation = "0199aaaa-bbbb-4ccc-8ddd-eeeeeeeeeeef";
        let path = base.join(format!("rollout-{conversation}.jsonl"));
        let body = [
            serde_json::json!({"type": "turn_context", "payload": {"model": "gpt-5.5", "effort": "low"}}),
            serde_json::json!({"type": "turn_context", "payload": {"model": "gpt-5.6", "effort": "high"}}),
        ]
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        fs::write(&path, format!("{body}\n")).unwrap();
        let found = Transcript {
            agent: "codex".to_string(),
            path,
            conversation_id: conversation.to_string(),
            basis: "resume-id".to_string(),
        };
        assert_eq!(
            runtime(&found),
            (Some("gpt-5.6".to_string()), Some("high".to_string()))
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_klaudia_session_is_read_from_its_own_root() {
        let base = temp();
        let claude_root = base.join("claude");
        let klaudia_root = base.join("klaudia");
        write_claude(
            &claude_root,
            ENGINE_ID,
            &[claude_line("m", "the wrong agent")],
        );
        let directory = klaudia_root.join(claude_key(ROOT_CWD));
        fs::create_dir_all(&directory).unwrap();
        let body = [
            serde_json::json!({"type": "user", "message": {"content": [{"type": "text", "text": "go"}]}}),
            serde_json::json!({"type": "assistant", "timestamp": "2026-10-06T09:52:00Z", "message": {"role": "assistant", "content": [{"type": "text", "text": "Iteration one done."}]}}),
            serde_json::json!({"type": "assistant", "timestamp": "2026-10-06T09:53:00Z", "message": {"role": "assistant", "content": [{"type": "text", "text": format!("Pushed with {}.", secret())}]}}),
        ]
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n");
        fs::write(
            directory.join(format!("{ENGINE_ID}.jsonl")),
            format!("{body}\n"),
        )
        .unwrap();
        let mut roots = HashMap::new();
        roots.insert("claude", claude_root.as_path());
        roots.insert("klaudia", klaudia_root.as_path());
        let found = find(
            &roots,
            ENGINE_ID,
            Some(&format!(
                "vogt-agent-auth run -- klaudia --model grok-4.7 --session-id {ENGINE_ID}"
            )),
            Some("klaudia"),
            Some(ROOT_CWD),
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            (found.agent.as_str(), found.basis.as_str()),
            ("klaudia", "session-id")
        );
        let replies = last_replies(&found, 5);
        assert_eq!(
            replies
                .iter()
                .map(|r| r.text.split_whitespace().next().unwrap())
                .collect::<Vec<_>>(),
            ["Iteration", "Pushed"]
        );
        assert!(!replies.last().unwrap().text.contains(&secret()));
        assert_eq!(runtime(&found), (None, None));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_synthetic_model_is_no_model() {
        let base = temp();
        let path = write_claude(
            &base.join("claude"),
            ENGINE_ID,
            &[serde_json::json!({"type": "assistant", "message": {"id": "c", "model": "<synthetic>", "content": [{"type": "text", "text": "x"}]}}).to_string()],
        );
        let found = Transcript {
            agent: "claude".to_string(),
            path,
            conversation_id: ENGINE_ID.to_string(),
            basis: "engine-id".to_string(),
        };
        assert_eq!(runtime(&found), (None, None));
        let _ = fs::remove_dir_all(&base);
    }
}
