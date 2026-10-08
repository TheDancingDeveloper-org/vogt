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

std::thread_local! {
    static REQUEST_ID: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static ACTOR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
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

pub fn bind_request_id(request_id: &str) {
    REQUEST_ID.with(|slot| *slot.borrow_mut() = Some(request_id.to_string()));
}

pub fn reset_request_id() {
    REQUEST_ID.with(|slot| *slot.borrow_mut() = None);
}

pub fn current_request_id() -> Option<String> {
    REQUEST_ID.with(|slot| slot.borrow().clone())
}

pub fn set_request_actor(identity_ref: Option<&str>) {
    ACTOR.with(|slot| *slot.borrow_mut() = identity_ref.map(str::to_string));
}

pub fn current_actor() -> Option<String> {
    ACTOR.with(|slot| slot.borrow().clone())
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

pub fn configure_logging() {
    RECENT.lock().expect("recent problems lock").capturing = true;
    let _ = tracing::subscriber::set_global_default(tracing_subscriber::registry().with(VogtLayer));
}

struct VogtLayer;

struct FieldVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        let text = rendered.trim_matches('"');
        if field.name() == "message" {
            self.message = text.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), text.to_string()));
        }
    }
}

impl<S> Layer<S> for VogtLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        if !meta.target().starts_with(LOGGER_NAMESPACE) {
            return;
        }
        let mut visitor = FieldVisitor {
            message: String::new(),
            fields: Vec::new(),
        };
        event.record(&mut visitor);
        let mut parts = Vec::new();
        if let Some(id) = current_request_id() {
            parts.push(format!("request_id={id}"));
        }
        parts.push(visitor.message);
        for (key, value) in &visitor.fields {
            parts.push(format!("{key}={value}"));
        }
        if let Some(actor) = current_actor() {
            parts.push(format!("actor={actor}"));
        }
        let redacted = redact(&parts.join(" "));
        if *meta.level() <= Level::WARN {
            let mut recent = RECENT.lock().expect("recent problems lock");
            if recent.lines.len() == RECENT_PROBLEMS_CAPACITY {
                recent.lines.pop_front();
            }
            recent
                .lines
                .push_back(redacted.chars().take(RECENT_LINE_LIMIT).collect());
        }
        let _ = writeln!(std::io::stderr(), "{redacted}");
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
