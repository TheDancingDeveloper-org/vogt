//! Logging. Ports `src/vogt/observability.py`.
//!
//! Every logger is `vogt.<area>`. Structured fields travel as `key=value` on
//! the text line and as top-level keys in JSON (`ts`, `level`, `logger`,
//! `message`, `request_id`, `actor`) — the `JsonFormatter` field names. A
//! request id is accepted only when it matches `^[A-Za-z0-9._-]{1,64}$`.
//! Output is stderr. Warning-or-worse lines are retained, redacted, the newest
//! 200, each cut at 2000 characters.
//!
//! `tracing` is the subscriber. `configure_logging` installs it once; a second
//! call only marks problem capture on, because a process has one global
//! subscriber. Nothing in this chunk emits yet, so the surface stays public
//! for the HTTP adapter.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{LazyLock, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
pub const LOGGER_NAMESPACE: &str = "vogt";
pub const RECENT_PROBLEMS_CAPACITY: usize = 200;
const RECENT_LINE_LIMIT: usize = 2000;

tokio::task_local! {
    static REQUEST_ID: String;
    // A cell, not the scope value itself: auth resolves the actor after the
    // request scope has opened, and Python's `set_request_actor` writes it then.
    static ACTOR: std::cell::RefCell<Option<String>>;
}

struct Recent {
    lines: VecDeque<String>,
    capturing: bool,
}

static RECENT: Mutex<Recent> = Mutex::new(Recent {
    lines: VecDeque::new(),
    capturing: false,
});

/// `logger("http")` is the `tracing` target modules should use.
pub fn logger(area: &str) -> String {
    format!("{LOGGER_NAMESPACE}.{area}")
}

pub fn accepted_request_id(raw: Option<&str>) -> Option<String> {
    let candidate = raw?.trim();
    let ok = !candidate.is_empty()
        && candidate.len() <= 64
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    ok.then(|| candidate.to_string())
}

/// Run `body` with the correlation id bound to the current task. The scope is
/// the binding, the way Python's `ContextVar` token is: it cannot leak onto
/// the next request a worker picks up. Unlike a `ContextVar`, a `tokio::spawn`
/// inside the scope does not inherit it, so a task that should carry the id
/// wraps itself in `with_request_id`.
pub async fn with_request_id<F, T>(request_id: String, body: F) -> T
where
    F: std::future::Future<Output = T>,
{
    REQUEST_ID
        .scope(request_id, ACTOR.scope(std::cell::RefCell::new(None), body))
        .await
}

pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(Clone::clone).ok()
}

/// Open the request scope with no actor. `set_request_actor` fills it once auth
/// has resolved who is calling.
pub async fn with_actor<F, T>(identity_ref: String, body: F) -> T
where
    F: std::future::Future<Output = T>,
{
    ACTOR
        .scope(std::cell::RefCell::new(Some(identity_ref)), body)
        .await
}

/// `set_request_actor`: record who is calling, inside an already-open scope.
/// Outside one it does nothing, because there is no request to attribute.
pub fn set_request_actor(identity_ref: Option<String>) {
    let _ = ACTOR.try_with(|actor| *actor.borrow_mut() = identity_ref);
}

pub fn current_actor() -> Option<String> {
    ACTOR
        .try_with(|actor| actor.borrow().clone())
        .ok()
        .flatten()
}

/// Best-effort removal of credential-shaped substrings. Mirrors the five
/// `_REDACTIONS` patterns: URL userinfo, bearer/basic, `vogt_` tokens, GitHub
/// token shapes, and `key=value` for secret-shaped keys.
pub fn redact(text: &str) -> String {
    let mut current = text.to_string();
    for (pattern, replacement) in REDACTIONS.iter() {
        current = pattern.replace_all(&current, *replacement).into_owned();
    }
    current
}

// The five `_REDACTIONS` in observability.py, in the same order. Compiled once:
// building them per log line dominated nothing yet, but the layer calls this
// on every event.
static REDACTIONS: LazyLock<[(regex::Regex, &'static str); 5]> = LazyLock::new(|| {
    let pairs = [
        (r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s@]+@", "$1[redacted]@"),
        (
            r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]+",
            "$1 [redacted]",
        ),
        (r"\bvogt_[A-Za-z0-9_-]{8,}", "[redacted]"),
        (
            r"\b(gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})",
            "[redacted]",
        ),
        (
            r#"(?i)\b([a-z_]*(?:token|secret|password|passwd|api[_-]?key|authorization|cookie)[a-z_]*)("?\s*[:=]\s*"?)[^\s",;&]+"#,
            "$1$2[redacted]",
        ),
    ];
    pairs.map(|(pattern, replacement)| {
        (
            regex::Regex::new(pattern).expect("redaction pattern"),
            replacement,
        )
    })
});

pub fn recent_problems(limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let recent = RECENT.lock().expect("recent problems lock");
    recent
        .lines
        .iter()
        .rev()
        .take(limit)
        .rev()
        .cloned()
        .collect()
}

pub fn capturing_problems() -> bool {
    RECENT.lock().expect("recent problems lock").capturing
}

pub fn configure_logging(level: &str, format: &str) {
    RECENT.lock().expect("recent problems lock").capturing = true;
    let _ =
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(VogtLayer {
            level: level.to_string(),
            json: format == "json",
        }));
}

struct VogtLayer {
    level: String,
    json: bool,
}

struct FieldVisitor {
    message: String,
    fields: Vec<(String, serde_json::Value)>,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.store(field, value.into());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.store(field, value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.store(field, value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.store(field, value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.store(
            field,
            serde_json::Number::from_f64(value).map_or(serde_json::Value::Null, Into::into),
        );
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        // The message is a string whose Debug form is a quoted JSON string.
        // Parsing it back keeps an embedded quote intact; trimming the ends
        // would turn `"quoted" message` into `quoted" message`.
        let value = if field.name() == "message" {
            serde_json::from_str::<String>(&rendered)
                .map_or(serde_json::Value::String(rendered), Into::into)
        } else {
            serde_json::Value::String(rendered)
        };
        self.store(field, value);
    }
}

impl FieldVisitor {
    fn store(&mut self, field: &Field, value: serde_json::Value) {
        if field.name() == "message" {
            self.message = match &value {
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            };
        } else {
            self.fields.push((field.name().to_string(), value));
        }
    }
}

impl<S> Layer<S> for VogtLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Python sets the root logger to WARNING and only `vogt.*` to the
        // configured level, so a dependency's warning is kept and its chatter
        // is not.
        let ours = meta.target().starts_with(LOGGER_NAMESPACE);
        if !((ours && self.emits(meta.level())) || *meta.level() <= Level::WARN) {
            return;
        }
        let mut visitor = FieldVisitor {
            message: String::new(),
            fields: Vec::new(),
        };
        event.record(&mut visitor);
        let request_id = current_request_id();
        let actor = current_actor();
        let text = render_text(meta, &visitor, request_id.as_deref(), actor.as_deref());
        let line = if self.json {
            render_json(meta, &visitor, request_id.as_deref(), actor.as_deref())
        } else {
            text.clone()
        };
        let redacted = redact(&line);
        if *meta.level() <= Level::WARN {
            // Python always keeps the text line, even when stderr is JSON.
            let kept = redact(&text);
            let mut recent = RECENT
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if recent.lines.len() == RECENT_PROBLEMS_CAPACITY {
                recent.lines.pop_front();
            }
            recent
                .lines
                .push_back(kept.chars().take(RECENT_LINE_LIMIT).collect());
        }
        let _ = writeln!(std::io::stderr(), "{redacted}");
    }
}

impl VogtLayer {
    fn emits(&self, level: &Level) -> bool {
        let floor = match self.level.to_ascii_lowercase().as_str() {
            "debug" => Level::DEBUG,
            "warning" => Level::WARN,
            "error" => Level::ERROR,
            _ => Level::INFO,
        };
        *level <= floor
    }
}

/// Python's `TextFormatter`: `2026-08-19T11:02:03.123+00:00 INFO    vogt.http message`,
/// then `key=value` pairs quoted only when the value is empty or has a space.
/// A bool renders `True`/`False`, the way `str(True)` does. `request_id` leads
/// the fields and `actor` trails them.
fn render_text(
    meta: &'static tracing::Metadata<'static>,
    visitor: &FieldVisitor,
    request_id: Option<&str>,
    actor: Option<&str>,
) -> String {
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3f+00:00");
    let level = format!("{:<7}", level_name(meta.level()));
    let mut line = format!("{stamp} {level} {} {}", meta.target(), visitor.message);
    let mut fields: Vec<(&str, String)> = Vec::new();
    if let Some(id) = request_id {
        fields.push(("request_id", id.to_string()));
    }
    for (key, value) in &visitor.fields {
        fields.push((key, text_value(value)));
    }
    if let Some(actor) = actor {
        fields.push(("actor", actor.to_string()));
    }
    let rendered = fields
        .iter()
        .map(|(key, value)| format!("{key}={}", terse(value)))
        .collect::<Vec<_>>()
        .join(" ");
    if !rendered.is_empty() {
        line.push(' ');
        line.push_str(&rendered);
    }
    line
}

/// `str(value)` for the text line. Python spells a bool `True`/`False` and
/// drops the quotes around everything else.
fn text_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Bool(true) => "True".to_string(),
        serde_json::Value::Bool(false) => "False".to_string(),
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn terse(value: &str) -> String {
    if value.is_empty() || value.contains(' ') {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

/// Python's `JsonFormatter`: one object per line, level lower-case, the same
/// fields the text line carries.
fn render_json(
    meta: &'static tracing::Metadata<'static>,
    visitor: &FieldVisitor,
    request_id: Option<&str>,
    actor: Option<&str>,
) -> String {
    let mut payload = serde_json::json!({
        "ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3f+00:00").to_string(),
        "level": level_name(meta.level()).to_ascii_lowercase(),
        "logger": meta.target(),
        "message": visitor.message,
    });
    let object = payload.as_object_mut().expect("object");
    if let Some(id) = request_id {
        object.insert("request_id".to_string(), id.into());
    }
    if let Some(actor) = actor {
        object.insert("actor".to_string(), actor.into());
    }
    for (key, value) in &visitor.fields {
        object.insert(key.clone(), value.clone());
    }
    // Python's json.dumps defaults: a space after ':' and ',', non-ASCII kept.
    py_dumps(&payload)
}

/// `json.dumps` with its default separators, so the line is `{"a": 1, "b": 2}`.
fn py_dumps(value: &serde_json::Value) -> String {
    let compact = serde_json::to_string(value).expect("log payload is json");
    let mut out = String::with_capacity(compact.len() + 8);
    let mut in_string = false;
    let mut escaped = false;
    for ch in compact.chars() {
        if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        out.push(ch);
        if ch == '"' {
            in_string = true;
        } else if ch == ':' || ch == ',' {
            out.push(' ');
        }
    }
    out
}

fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "ERROR",
        Level::WARN => "WARNING",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_constrained() {
        assert_eq!(
            accepted_request_id(Some("abc-123")).as_deref(),
            Some("abc-123")
        );
        assert_eq!(accepted_request_id(Some("  has space")), None);
        assert_eq!(accepted_request_id(Some("has\nline")), None);
        assert_eq!(accepted_request_id(Some(&"a".repeat(65))), None);
        assert_eq!(accepted_request_id(None), None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_request_id_stays_with_its_task() {
        let first = tokio::spawn(with_request_id("req-1".to_string(), async {
            tokio::task::yield_now().await;
            current_request_id()
        }));
        let second = tokio::spawn(with_request_id("req-2".to_string(), async {
            current_request_id()
        }));
        assert_eq!(first.await.unwrap().as_deref(), Some("req-1"));
        assert_eq!(second.await.unwrap().as_deref(), Some("req-2"));
        assert_eq!(current_request_id(), None);
    }

    #[tokio::test]
    async fn the_actor_is_set_after_the_scope_opens() {
        // Auth resolves the caller once the handler is already running, so the
        // actor has to be writable inside the scope rather than fixed at entry.
        let seen = with_request_id("req".to_string(), async {
            assert_eq!(current_actor(), None);
            set_request_actor(Some("alice".to_string()));
            current_actor()
        })
        .await;
        assert_eq!(seen.as_deref(), Some("alice"));
        assert_eq!(current_actor(), None);
    }

    #[test]
    fn the_text_line_quotes_only_when_it_must() {
        assert_eq!(terse("GET"), "GET");
        assert_eq!(terse("GET /health"), "\"GET /health\"");
        assert_eq!(terse(""), "\"\"");
        assert_eq!(level_name(&Level::WARN), "WARNING");
    }

    #[test]
    fn the_configured_level_drops_quieter_events() {
        let layer = VogtLayer {
            level: "warning".to_string(),
            json: false,
        };
        assert!(layer.emits(&Level::WARN));
        assert!(!layer.emits(&Level::INFO));
    }

    #[test]
    fn redaction_matches_the_python_shapes() {
        assert_eq!(redact(&format!("ghp_{}", "a".repeat(30))), "[redacted]");
        assert_eq!(
            redact(r#"{"password": "hunter2"}"#),
            r#"{"password": "[redacted]"}"#
        );
        assert_eq!(redact("https://u:p@host/x"), "https://[redacted]@host/x");
        assert_eq!(redact("nothing secret here"), "nothing secret here");
        // A multibyte character must not panic: the old walker indexed bytes.
        assert!(redact("café token=s3cret").contains("[redacted]"));
        assert!(!redact("café token=s3cret").contains("s3cret"));
    }

    #[test]
    fn recent_problems_are_bounded_and_ordered() {
        {
            let mut recent = RECENT.lock().unwrap();
            recent.lines.clear();
            recent.capturing = true;
            for n in 0..3 {
                recent.lines.push_back(format!("warn {n}"));
            }
        }
        assert!(capturing_problems());
        assert_eq!(
            recent_problems(2),
            vec!["warn 1".to_string(), "warn 2".to_string()]
        );
        assert!(recent_problems(0).is_empty());
    }
}
