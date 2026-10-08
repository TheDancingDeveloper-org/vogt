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
use std::sync::Mutex;

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
    redact_assignments(&redact_github(&redact_prefixed(
        &redact_authorization(&redact_url_userinfo(text)),
        "vogt_",
    )))
}

fn redact_url_userinfo(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(scheme) = url_scheme_at(text, index) {
            if let Some(at) = text[index + scheme..].find('@') {
                let userinfo = &text[index + scheme..index + scheme + at];
                if !userinfo.contains('/') && !userinfo.contains(' ') {
                    out.push_str(&text[index..index + scheme]);
                    out.push_str("[redacted]@");
                    index += scheme + at + 1;
                    continue;
                }
            }
        }
        out.push(bytes[index] as char);
        index += 1;
    }
    out
}

fn url_scheme_at(text: &str, index: usize) -> Option<usize> {
    let rest = text.as_bytes().get(index..)?;
    if !rest.first()?.is_ascii_alphabetic() {
        return None;
    }
    let mut end = 1;
    while end < rest.len()
        && (rest[end].is_ascii_alphanumeric() || matches!(rest[end], b'+' | b'-' | b'.'))
    {
        end += 1;
    }
    text[index + end..].starts_with("://").then_some(end + 3)
}

fn redact_authorization(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::new();
    let mut index = 0;
    while index < text.len() {
        let needle = ["bearer ", "basic "]
            .iter()
            .find(|needle| lower[index..].starts_with(*needle));
        if let Some(needle) = needle {
            out.push_str(&text[index..index + needle.len()]);
            out.push_str("[redacted]");
            index += needle.len();
            while text
                .as_bytes()
                .get(index)
                .is_some_and(|b| is_token_char(*b))
            {
                index += 1;
            }
        } else {
            let ch = text[index..].chars().next().unwrap();
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

fn is_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
}

fn redact_prefixed(text: &str, prefix: &str) -> String {
    let mut out = String::new();
    let mut index = 0;
    while let Some(found) = text[index..].find(prefix) {
        let start = index + found;
        let after = start + prefix.len();
        let tail: String = text[after..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        out.push_str(&text[index..start]);
        if tail.len() >= 8 {
            out.push_str("[redacted]");
            index = after + tail.len();
        } else {
            out.push_str(prefix);
            index = after;
        }
    }
    out.push_str(&text[index..]);
    out
}

fn redact_github(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(skip) = github_token_len(&text[index..]) {
            out.push_str("[redacted]");
            index += skip;
        } else {
            out.push(bytes[index] as char);
            index += 1;
        }
    }
    out
}

fn github_token_len(text: &str) -> Option<usize> {
    for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let body = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .count();
            let bytes: usize = rest.chars().take(body).map(char::len_utf8).sum();
            if body >= 20 {
                return Some(prefix.len() + bytes);
            }
        }
    }
    None
}

fn redact_assignments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(value_end) = secret_assignment(&text[index..]) {
            let eq = text[index..index + value_end]
                .find(['=', ':'])
                .map(|at| index + at + 1)
                .unwrap_or(index);
            let mut value_at = eq;
            while text.as_bytes().get(value_at) == Some(&b' ')
                || text.as_bytes().get(value_at) == Some(&b'"')
            {
                value_at += 1;
            }
            out.push_str(&text[index..value_at]);
            out.push_str("[redacted]");
            index += value_end;
        } else {
            out.push(bytes[index] as char);
            index += 1;
        }
    }
    out
}

fn secret_assignment(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if !first.is_ascii_alphabetic() && first != b'_' {
        return None;
    }
    let mut key_end = 1;
    while bytes
        .get(key_end)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        key_end += 1;
    }
    let key = text[..key_end].to_ascii_lowercase();
    let secret = [
        "token",
        "secret",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "authorization",
        "cookie",
    ]
    .iter()
    .any(|needle| key.contains(needle));
    if !secret {
        return None;
    }
    let mut cursor = key_end;
    if bytes.get(cursor) == Some(&b'"') {
        cursor += 1;
    }
    while bytes.get(cursor) == Some(&b' ') {
        cursor += 1;
    }
    if !matches!(bytes.get(cursor), Some(b'=') | Some(b':')) {
        return None;
    }
    cursor += 1;
    while bytes.get(cursor) == Some(&b' ') || bytes.get(cursor) == Some(&b'"') {
        cursor += 1;
    }
    let value_start = cursor;
    while bytes
        .get(cursor)
        .is_some_and(|b| !matches!(b, b' ' | b'"' | b',' | b';' | b'&'))
    {
        cursor += 1;
    }
    (cursor > value_start).then_some(cursor)
}

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
    fn redaction_removes_credential_shapes() {
        let line = redact(
            "saw vogt_abcdefghijk and bearer abc.def and url https://user:secret@host/x token=hunter2 ghp_abcdefghijklmnopqrst",
        );
        assert!(!line.contains("abcdefghijk"), "{line}");
        assert!(!line.contains("abc.def"), "{line}");
        assert!(!line.contains("user:secret"), "{line}");
        assert!(!line.contains("hunter2"), "{line}");
        assert!(!line.contains("ghp_abc"), "{line}");
        assert!(line.contains("[redacted]"), "{line}");
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
