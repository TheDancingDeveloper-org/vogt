//! Lean write results (WI-1525), applied to MCP tool results.
//!
//! A write confirms itself in a few fields by default. The full record is a
//! `mode=full` away. This runs on the finished result, after the write has
//! committed, so it changes what is returned and never what is written.

use serde_json::{json, Map, Value};

/// Session fields a compact result keeps. `id`, `engine_session_id` and `cwd`
/// are what `SessionSummary` requires, so a compact result still reads as one.
const SESSION_KEEP: &[&str] = &[
    "linked",
    "id",
    "engine_session_id",
    "cwd",
    "name",
    "project",
    "work_item",
    "activity",
    "alive",
];

/// The operations whose result is compacted, and how.
fn shape_of(operation: &str) -> Option<Shape> {
    match operation {
        "session.start" | "session.hibernate" | "session.wake" | "session.bind_work" => {
            Some(Shape::Session)
        }
        "work.create" | "work.update" | "work.transition" => Some(Shape::Item),
        "work.comment" => Some(Shape::Comment),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Session,
    Item,
    Comment,
}

/// True when `params` asked for the complete result.
fn wants_full(params: &Value) -> bool {
    params.get("mode").and_then(Value::as_str) == Some("full")
}

/// Compact `result` for `operation` unless `params` asked for `mode=full`.
pub fn apply(operation: &str, params: &Value, result: Value) -> Value {
    let Some(shape) = shape_of(operation) else {
        return result;
    };
    if wants_full(params) {
        return result;
    }
    match shape {
        Shape::Session => session(result),
        Shape::Item => item(result),
        Shape::Comment => comment(params, result),
    }
}

fn session(mut result: Value) -> Value {
    let mut out = Map::new();
    if let Some(summary) = result.get("session").and_then(Value::as_object) {
        let mut slim = Map::new();
        for key in SESSION_KEEP {
            match summary.get(*key) {
                None | Some(Value::Null) => {}
                Some(value) => {
                    slim.insert((*key).to_string(), value.clone());
                }
            }
        }
        out.insert("session".to_string(), Value::Object(slim));
    }
    // Everything beside the session that says something: a bind's previous
    // item and label outcome, a start's landing rule. Nulls, empty values and
    // the defaults are dropped; the heavy `digest` is `mode=full` only.
    if let Some(object) = result.as_object_mut() {
        for (key, value) in std::mem::take(object) {
            if key == "session" || key == "digest" || is_empty(&value) {
                continue;
            }
            if key == "project_mismatch" && value == json!(false) {
                continue;
            }
            if key == "woke" && value == json!(false) {
                continue;
            }
            out.insert(key, value);
        }
    }
    Value::Object(out)
}

fn item(result: Value) -> Value {
    let Some(item) = result.get("item").and_then(Value::as_object) else {
        return result;
    };
    let mut out = Map::new();
    for key in ["ref", "state", "title", "updated_at"] {
        if let Some(value) = item.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(out)
}

fn comment(params: &Value, result: Value) -> Value {
    let mut out = Map::new();
    if let Some(reference) = params.get("ref") {
        out.insert("ref".to_string(), reference.clone());
    }
    if let Some(comment) = result.get("comment").and_then(Value::as_object) {
        for key in ["id", "kind", "created_at"] {
            if let Some(value) = comment.get(key) {
                out.insert(
                    if key == "id" { "comment_id" } else { key }.to_string(),
                    value.clone(),
                );
            }
        }
    }
    // `skipped` is the ordinary answer; only a real upstream outcome is news.
    if let Some(write_back) = result.get("write_back") {
        if write_back != "skipped" {
            out.insert("write_back".to_string(), write_back.clone());
        }
    }
    Value::Object(out)
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_session() -> Value {
        let mut session = json!({
            "linked": true, "id": "ses_01ABC", "engine_session_id": "3f2a9c1e-0000-4000-8000-000000000000",
            "project": "vogt", "work_item": "WI-1525", "work_item_title": "t".repeat(120),
            "cwd": "/home/sprooty/Working/Active/wi1525", "reason": "r".repeat(400),
            "name": "WI-1525", "activity": "hibernated", "alive": false, "model": null,
        });
        for index in 0..40 {
            session[format!("extra_{index}")] = json!("x".repeat(60));
        }
        session
    }

    #[test]
    fn a_compact_session_write_stays_under_600_bytes() {
        let full = json!({"session": big_session(), "digest": {"id": "ses_01ABC", "report": "p".repeat(900)},
            "effective_policy": null, "woke": false});
        let size_full = full.to_string().len();
        let lean = apply(
            "session.hibernate",
            &json!({"mode": "compact"}),
            full.clone(),
        );
        assert!(size_full > 3000, "fixture is not big: {size_full}");
        assert!(lean.to_string().len() <= 600, "{lean}");
        assert_eq!(lean["session"]["id"], "ses_01ABC");
        assert!(lean.get("digest").is_none() && lean.get("woke").is_none());
        let kept = apply("session.hibernate", &json!({"mode": "full"}), full.clone());
        assert_eq!(kept, full);
    }

    #[test]
    fn a_compact_bind_keeps_the_news_only() {
        let full = json!({"session": big_session(), "previous_work_item": "WI-9",
            "project_mismatch": false, "engine_label": "written", "engine": null});
        let lean = apply("session.bind_work", &json!({}), full);
        assert!(lean.to_string().len() <= 600, "{lean}");
        assert_eq!(lean["previous_work_item"], "WI-9");
        assert_eq!(lean["engine_label"], "written");
        assert!(lean.get("project_mismatch").is_none() && lean.get("engine").is_none());
    }

    #[test]
    fn a_compact_work_write_is_ref_state_title_updated_at() {
        let item = json!({"id": "x", "ref": "WI-7", "kind": "feature", "title": "Do it",
            "body": "b".repeat(5000), "state": "open", "updated_at": "2026-11-04T00:00:00Z"});
        let full = json!({"item": item, "comments": [], "sessions": [], "branches": []});
        for operation in ["work.create", "work.update", "work.transition"] {
            let lean = apply(operation, &json!({"mode": "compact"}), full.clone());
            assert!(lean.to_string().len() <= 300, "{operation}: {lean}");
            assert_eq!(
                lean,
                json!({"ref": "WI-7", "state": "open", "title": "Do it", "updated_at": "2026-11-04T00:00:00Z"})
            );
            assert_eq!(
                apply(operation, &json!({"mode": "full"}), full.clone()),
                full
            );
        }
    }

    #[test]
    fn a_compact_comment_names_the_item_and_the_comment() {
        let full = json!({"comment": {"id": "c1", "work_item_id": "w", "actor_id": "a",
            "actor_display_name": "n", "body": "b".repeat(4000), "created_at": "2026-11-04T00:00:00Z",
            "kind": "note", "handoff": null}, "write_back": "skipped"});
        let lean = apply("work.comment", &json!({"ref": "WI-7"}), full);
        assert!(lean.to_string().len() <= 300, "{lean}");
        assert_eq!(lean["ref"], "WI-7");
        assert_eq!(lean["comment_id"], "c1");
        assert!(lean.get("write_back").is_none());
    }

    #[test]
    fn other_operations_are_untouched() {
        let result = json!({"session": big_session()});
        assert_eq!(apply("session.list", &json!({}), result.clone()), result);
    }
}
