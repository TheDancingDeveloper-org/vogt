//! The session engine's operational report, on the three surfaces.
//!
//! Ports `src/vogt/application/services/engine_status.py`. The engine answers
//! `GET /api/status` for the GUI's Settings panel: its build, how many
//! sessions, push subscriptions and GUI processes it holds, its history
//! archive and agent-task storage, and event subscribers that fell behind.
//! `engine.status` puts that same report on CLI, REST and MCP.
//!
//! No engine, or one that does not answer, is a stated answer rather than an
//! error, as `agent_cli.list` says it.
//!
//! The nested rows (`disk`, `history`, `agent_tasks`, `event_lag`) are
//! pydantic models in Python, validated in lax mode with `extra="forbid"`.
//! `Lax` below reproduces that validation: the same coercions (`"90"` and
//! `90.0` are an int, `"yes"` is a bool), the same refusals (`"x"`, `90.5`,
//! an unknown field), and the same defaults for a missing field.

use serde_json::{Map, Value};

use super::{dispatch, Built};

use crate::application::context::AppContext;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;

const NO_ENGINE: &str = "no session engine is configured (VOGT_ENGINE_URL is unset)";

pub fn engine_status_op(built: &Built, _params: Value) -> Result<Value, VogtError> {
    dispatch!(built, engine_status, ())
}

/// What the engine reports about itself right now.
fn engine_status<C, I>(ctx: &AppContext<C, I>, _: ()) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(unanswered(NO_ENGINE));
    };
    let payload = match engine.operational_status() {
        Ok(payload) => payload,
        Err(VogtError::EngineUnavailable(message)) => return Ok(unanswered(&message)),
        Err(other) => return Err(other),
    };
    report(&payload)
}

/// The result when the engine could not be asked: `engine` says why, and
/// every other field keeps its default.
fn unanswered(why: &str) -> Value {
    let mut out = empty();
    out.insert("engine".to_string(), Value::String(why.to_string()));
    Value::Object(out)
}

fn empty() -> Map<String, Value> {
    let mut out = Map::new();
    for field in [
        "engine",
        "version",
        "source_ref",
        "source_sha",
        "release_url",
        "session_count",
        "push_subscription_count",
        "gui_process_count",
        "gui_stream_configured",
        "fcm_enabled",
        "auto_agent_auth",
        "state_dir",
        "workspace_root",
        "history",
        "agent_tasks",
    ] {
        out.insert(field.to_string(), Value::Null);
    }
    out.insert("event_lag".to_string(), Value::Object(Map::new()));
    out.insert("disk".to_string(), Value::Null);
    out
}

/// The engine's answer, read the way the Python service reads it. A nested
/// row that pydantic would refuse (`history`, `agent_tasks`, an `event_lag`
/// entry) is a failure there, uncaught; here it is an `engine_unavailable`
/// naming the row, since the engine answered something Vogt cannot read.
/// A malformed `disk` is dropped, as in Python.
fn report(payload: &Value) -> Result<Value, VogtError> {
    let storage = object(payload.get("storage"));
    let broker = object(payload.get("auth_broker"));
    let version = match payload.get("product_version") {
        Some(value) if truthy(value) => text(Some(value)),
        _ => text(payload.get("version")),
    };
    let mut out = empty();
    out.insert("version".to_string(), version);
    for field in ["source_ref", "source_sha", "release_url"] {
        out.insert(field.to_string(), text(payload.get(field)));
    }
    for field in [
        "session_count",
        "push_subscription_count",
        "gui_process_count",
    ] {
        out.insert(field.to_string(), count(payload.get(field)));
    }
    for field in ["gui_stream_configured", "fcm_enabled"] {
        out.insert(field.to_string(), flag(payload.get(field)));
    }
    out.insert(
        "auto_agent_auth".to_string(),
        flag(broker.and_then(|broker| broker.get("auto_agent_auth"))),
    );
    for field in ["state_dir", "workspace_root"] {
        out.insert(
            field.to_string(),
            text(storage.and_then(|storage| storage.get(field))),
        );
    }
    if let Some(history) = payload.get("history").and_then(Value::as_object) {
        out.insert("history".to_string(), nested("history", HISTORY, history)?);
    }
    if let Some(tasks) = payload.get("agent_tasks").and_then(Value::as_object) {
        out.insert(
            "agent_tasks".to_string(),
            nested("agent_tasks", AGENT_TASKS, tasks)?,
        );
    }
    let mut lags = Map::new();
    if let Some(event_lag) = object(payload.get("event_lag")) {
        for (name, lag) in event_lag {
            if let Some(lag) = lag.as_object() {
                lags.insert(name.clone(), nested("event_lag", LAG, lag)?);
            }
        }
    }
    out.insert("event_lag".to_string(), Value::Object(lags));
    out.insert(
        "disk".to_string(),
        disk(payload.get("disk")).unwrap_or(Value::Null),
    );
    Ok(Value::Object(out))
}

fn nested(
    row: &str,
    fields: &[(&str, Lax)],
    value: &Map<String, Value>,
) -> Result<Value, VogtError> {
    validate(fields, value).ok_or_else(|| {
        VogtError::EngineUnavailable(format!(
            "the session engine answered GET /api/status with a malformed `{row}`"
        ))
    })
}

/// The workspace volume, when the engine reports one (WI-1304): `None` when
/// `disk` is not an object, or is one pydantic's `EngineDiskStatus` refuses.
pub(crate) fn disk(value: Option<&Value>) -> Option<Value> {
    validate(DISK, value?.as_object()?)
}

fn object(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

/// `_text`: a non-empty string, else null.
fn text(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Value::String(text.clone()),
        _ => Value::Null,
    }
}

/// `_count`: an int (never a bool or a float), else null.
fn count(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => {
            Value::Number(number.clone())
        }
        _ => Value::Null,
    }
}

/// `_flag`: a bool, else null.
fn flag(value: Option<&Value>) -> Value {
    match value {
        Some(Value::Bool(flag)) => Value::Bool(*flag),
        _ => Value::Null,
    }
}

/// Python truthiness of a JSON value, for `product_version or version`.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// One pydantic field's type and default, in lax mode.
#[derive(Clone, Copy)]
enum Lax {
    /// `str | None = None`
    OptionalText,
    /// `int | None = None`
    OptionalInt,
    /// `int = 0`
    Int,
    /// `bool = False`
    Bool,
}

const DISK: &[(&str, Lax)] = &[
    ("path", Lax::OptionalText),
    ("total_bytes", Lax::OptionalInt),
    ("free_bytes", Lax::OptionalInt),
    ("used_percent", Lax::OptionalInt),
    ("threshold_percent", Lax::OptionalInt),
    ("needs_attention", Lax::Bool),
];

const HISTORY: &[(&str, Lax)] = &[
    ("enabled", Lax::Bool),
    ("archived_session_count", Lax::OptionalInt),
    ("log_file_count", Lax::OptionalInt),
    ("log_bytes", Lax::OptionalInt),
    ("db_bytes", Lax::OptionalInt),
];

const AGENT_TASKS: &[(&str, Lax)] = &[
    ("task_count", Lax::Int),
    ("prompt_task_dir_count", Lax::Int),
    ("prompt_file_count", Lax::Int),
    ("context_file_count", Lax::Int),
    ("session_prompt_file_count", Lax::Int),
    ("prompt_bytes", Lax::Int),
    ("orphan_task_dir_count", Lax::Int),
];

const LAG: &[(&str, Lax)] = &[("episodes", Lax::Int), ("events_skipped", Lax::Int)];

/// `Model.model_validate(value).model_dump()`, or `None` where pydantic
/// raises: an unknown field (`extra="forbid"`) or a value the field's type
/// refuses. A missing field takes its default.
fn validate(fields: &[(&str, Lax)], value: &Map<String, Value>) -> Option<Value> {
    if value
        .keys()
        .any(|key| !fields.iter().any(|(name, _)| name == key))
    {
        return None;
    }
    let mut out = Map::new();
    for (name, kind) in fields {
        let read = match (value.get(*name), kind) {
            (None, Lax::OptionalText | Lax::OptionalInt) => Value::Null,
            (None, Lax::Int) => Value::from(0),
            (None, Lax::Bool) => Value::Bool(false),
            (Some(Value::Null), Lax::OptionalText | Lax::OptionalInt) => Value::Null,
            (Some(Value::String(text)), Lax::OptionalText) => Value::String(text.clone()),
            (Some(_), Lax::OptionalText) => return None,
            (Some(given), Lax::OptionalInt | Lax::Int) => lax_int(given)?,
            (Some(given), Lax::Bool) => Value::Bool(lax_bool(given)?),
        };
        out.insert((*name).to_string(), read);
    }
    Some(Value::Object(out))
}

/// pydantic's lax `int`: a bool, an integral float, or a string of an int
/// (surrounding whitespace, `_` between digits and a trailing `.0…` allowed).
///
/// Python's int is unbounded, so an int past `i64` is kept, not refused: the
/// whole `u64` range round-trips exactly, as a JSON int or a string of one
/// (WI-1081). The engine is Rust and reports its counts as `u64`, so that is
/// every value it can send. A magnitude past `u64` cannot be held exactly by
/// `serde_json` without `arbitrary_precision`; see
/// `docs/rust-port/accepted-differences.md`.
fn lax_int(value: &Value) -> Option<Value> {
    match value {
        Value::Bool(flag) => Some(Value::from(i64::from(*flag))),
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                return Some(Value::Number(number.clone()));
            }
            let float = number.as_f64()?;
            // pydantic refuses a float past i64 (1e20) and any fraction.
            if float.is_finite()
                && float.fract() == 0.0
                && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&float)
            {
                #[allow(clippy::cast_possible_truncation)]
                Some(Value::from(float as i64))
            } else {
                None
            }
        }
        Value::String(text) => int_text(text),
        _ => None,
    }
}

fn int_text(text: &str) -> Option<Value> {
    let trimmed = text.trim_matches(|c: char| c.is_ascii_whitespace());
    // A trailing `.0`, `.00`, …: "90.0" is 90, "90." and "90.5" are refused.
    let trimmed = match trimmed.split_once('.') {
        Some((whole, zeros)) if !zeros.is_empty() && zeros.bytes().all(|b| b == b'0') => whole,
        Some(_) => return None,
        None => trimmed,
    };
    let (negative, digits) = match trimmed.as_bytes().first()? {
        b'+' => (false, &trimmed[1..]),
        b'-' => (true, &trimmed[1..]),
        _ => (false, trimmed),
    };
    // Python's `int()`: digits, with single underscores between them.
    let bytes = digits.as_bytes();
    if bytes.is_empty()
        || !bytes.iter().all(|b| b.is_ascii_digit() || *b == b'_')
        || bytes[0] == b'_'
        || bytes[bytes.len() - 1] == b'_'
        || digits.contains("__")
    {
        return None;
    }
    let digits = digits.replace('_', "");
    if negative {
        format!("-{digits}").parse::<i64>().ok().map(Value::from)
    } else {
        digits.parse::<u64>().ok().map(Value::from)
    }
}

/// pydantic's lax `bool`: a bool, 0/1 (int or float), or one of its words,
/// case-insensitively and with no surrounding whitespace.
fn lax_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => {
            let n = number.as_f64()?;
            if n == 0.0 {
                Some(false)
            } else if n == 1.0 {
                Some(true)
            } else {
                None
            }
        }
        Value::String(text) => match text.to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn used(value: Value) -> Option<Value> {
        disk(Some(&json!({ "used_percent": value }))).map(|row| row["used_percent"].clone())
    }

    /// Python keeps an int past `i64` (its int is unbounded), and so must a
    /// nested row: the row is kept and the value round-trips (WI-1081).
    #[test]
    fn an_int_past_i64_round_trips_in_a_nested_row() {
        let big = u64::MAX;
        let past = 9_223_372_036_854_775_808_u64; // i64::MAX + 1
        let payload = json!({
            "history": { "enabled": true, "log_bytes": big, "db_bytes": past },
            "agent_tasks": { "prompt_bytes": big, "task_count": "18446744073709551615" },
            "event_lag": { "sse": { "episodes": past, "events_skipped": big } },
            "disk": { "total_bytes": big, "free_bytes": past },
        });
        let out = report(&payload).expect("no row is refused");
        assert_eq!(out["history"]["log_bytes"], json!(big));
        assert_eq!(out["history"]["db_bytes"], json!(past));
        assert_eq!(out["agent_tasks"]["prompt_bytes"], json!(big));
        assert_eq!(out["agent_tasks"]["task_count"], json!(big));
        assert_eq!(out["event_lag"]["sse"]["episodes"], json!(past));
        assert_eq!(out["event_lag"]["sse"]["events_skipped"], json!(big));
        assert_eq!(out["disk"]["total_bytes"], json!(big));
        assert_eq!(out["disk"]["free_bytes"], json!(past));
        // Through the wire format too: the digits survive a parse and a dump.
        let wire = serde_json::to_string(&out["history"]).unwrap();
        assert!(
            wire.contains("\"log_bytes\":18446744073709551615"),
            "{wire}"
        );
        assert!(wire.contains("\"db_bytes\":9223372036854775808"), "{wire}");
        let reparsed: Value = serde_json::from_str(
            r#"{"history":{"log_bytes":18446744073709551615},"agent_tasks":{},"event_lag":{}}"#,
        )
        .unwrap();
        assert_eq!(
            report(&reparsed).unwrap()["history"]["log_bytes"],
            json!(18_446_744_073_709_551_615_u64)
        );
        // Below i64::MIN stays exact where it fits; a negative past i64 is
        // past what serde_json holds as an int.
        assert_eq!(used(json!(i64::MIN)), Some(json!(i64::MIN)));
    }

    /// Each case was read off pydantic 2 (`EngineDiskStatus.model_validate`).
    #[test]
    fn the_disk_row_coerces_and_refuses_as_pydantic_does() {
        for (given, want) in [
            (json!(true), Some(json!(1))),
            (json!(90.0), Some(json!(90))),
            (json!(-1.0), Some(json!(-1))),
            (json!("90"), Some(json!(90))),
            (json!(" 90 "), Some(json!(90))),
            (json!("\t7\n"), Some(json!(7))),
            (json!("9_0"), Some(json!(90))),
            (json!("1_0.0"), Some(json!(10))),
            (json!("90.00"), Some(json!(90))),
            (json!("+5"), Some(json!(5))),
            (json!("  -5.0  "), Some(json!(-5))),
            (json!("007"), Some(json!(7))),
            (json!(null), Some(json!(null))),
            (json!("x"), None),
            (json!(90.5), None),
            (json!(1e20), None),
            (json!("90."), None),
            (json!(".0"), None),
            (json!("90.5"), None),
            (json!("1e2"), None),
            (json!("0x10"), None),
            (json!("_1"), None),
            (json!("1__0"), None),
            (json!(""), None),
            (json!(" "), None),
            (json!([]), None),
        ] {
            assert_eq!(used(given.clone()), want, "{given}");
        }
        for (given, want) in [
            (json!(1), Some(true)),
            (json!(0.0), Some(false)),
            (json!("YES"), Some(true)),
            (json!("On"), Some(true)),
            (json!("f"), Some(false)),
            (json!("No"), Some(false)),
            (json!(2), None),
            (json!(" true"), None),
            (json!(null), None),
        ] {
            let row = disk(Some(&json!({ "needs_attention": given })));
            assert_eq!(
                row.map(|row| row["needs_attention"].as_bool().unwrap()),
                want,
                "{given}"
            );
        }
        for given in [json!(1), json!(true)] {
            assert_eq!(disk(Some(&json!({ "path": given }))), None);
        }
        assert_eq!(disk(Some(&json!({ "used_percent": 1, "extra": 1 }))), None);
        assert_eq!(
            disk(Some(&json!({}))),
            Some(json!({
                "path": null, "total_bytes": null, "free_bytes": null,
                "used_percent": null, "threshold_percent": null,
                "needs_attention": false,
            }))
        );
    }
}
