//! `session.digest`: one session's digest from a single engine row plus a
//! transcript tail. Ports `digest` in `sessions.py`.
//!
//! Numbers come from the row's `economics` when the engine already computed
//! them and stay null when it did not: this read does not meter a transcript.
//! The excerpt is the list digest's tail read, clipped to 120 characters, and
//! null when the transcript cannot be found or read.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::adapters::engine::{EngineBlocked, EngineSession};
use crate::adapters::transcripts;
use crate::application::context::{AppContext, Built};
use crate::application::services::sessions::resolve_target;
use crate::core::session_economics::{context_pct, SessionEconomics};
use crate::core::{from_iso, py_repr, to_json_timestamp, Clock, IdFactory};
use crate::errors::VogtError;

const EXCERPT_LIMIT: usize = 120;

pub(crate) fn digest_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let window = params
        .get("context_window")
        .and_then(Value::as_i64)
        .filter(|window| *window > 0);
    crate::with_ctx!(ctx, |ctx| digest_in(ctx, &id, window))
}

fn digest_in<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    id: &str,
    window: Option<i64>,
) -> Result<Value, VogtError> {
    let target = resolve_target(ctx, id)?;
    let live = match ctx.engine.as_ref() {
        Some(engine) => match engine.get_session(&target.engine_session_id) {
            Ok(found) => found,
            Err(VogtError::EngineUnavailable(_)) => None,
            Err(error) => return Err(error),
        },
        None => None,
    };
    if live.is_none() && target.session.is_none() {
        return Err(VogtError::NotFound(format!("no session {}", py_repr(id))));
    }
    let Some(live) = live else {
        return Ok(result(id, None, &Numbers::default(), window, None));
    };
    let template = target
        .session
        .as_ref()
        .and_then(|row| row.template.as_deref());
    let excerpt = excerpt_of(ctx, &live, template);
    let numbers = live.economics.as_ref().map(numbers_of).unwrap_or_default();
    Ok(result(id, Some(&live), &numbers, window, excerpt))
}

/// The engine's counters. A value that is not an integer is absent rather
/// than zero: zero would say the session was measured and did nothing.
#[derive(Default)]
struct Numbers {
    reported: bool,
    turns: Option<i64>,
    context: Option<i64>,
    peak: Option<i64>,
    compactions: Option<i64>,
    output: Option<i64>,
    usage_measured: Option<bool>,
}

fn numbers_of(economics: &Map<String, Value>) -> Numbers {
    let int = |name: &str| economics.get(name).and_then(Value::as_i64);
    Numbers {
        reported: true,
        turns: int("turns"),
        context: int("context_tokens"),
        peak: int("peak_context_tokens"),
        compactions: int("compactions"),
        output: int("output_tokens"),
        usage_measured: economics.get("usage_measured").and_then(Value::as_bool),
    }
}

/// The list digest's tail read, clipped. Only the id-based lookups, as the
/// list uses: no start time and no cwd guess.
fn excerpt_of<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    live: &EngineSession,
    template: Option<&str>,
) -> Option<String> {
    let roots = &ctx.config.session_transcript_roots;
    if roots.is_empty() {
        return None;
    }
    let roots: HashMap<&str, &Path> = roots
        .iter()
        .map(|(agent, root)| (agent.as_str(), root.as_path()))
        .collect();
    let found = transcripts::find(
        &roots,
        &live.id,
        live.command.as_deref(),
        template,
        Some(live.cwd.as_str()),
        None,
        false,
    )?;
    transcripts::last_reply_excerpt(&found)
        .map(|excerpt| excerpt.chars().take(EXCERPT_LIMIT).collect())
}

fn blocked_of(blocked: &EngineBlocked) -> Value {
    json!({
        "blocker": blocked.reason,
        "items": blocked.items,
        "since": timestamp(blocked.since.as_deref()),
    })
}

/// The engine's RFC3339 string in the answer's form, or null if unreadable.
fn timestamp(text: Option<&str>) -> Option<String> {
    let text = text.filter(|text| !text.is_empty())?;
    from_iso(&text.replace('Z', "+00:00"))
        .ok()
        .map(to_json_timestamp)
}

fn result(
    id: &str,
    live: Option<&EngineSession>,
    numbers: &Numbers,
    window: Option<i64>,
    excerpt: Option<String>,
) -> Value {
    let budget_line = numbers.reported.then(|| {
        SessionEconomics {
            turns: numbers.turns.unwrap_or(0),
            context_tokens: numbers.context.unwrap_or(0),
            peak_context_tokens: numbers.peak.unwrap_or(0),
            compactions: numbers.compactions.unwrap_or(0),
            output_tokens: numbers.output.unwrap_or(0),
            usage_measured: numbers.usage_measured.unwrap_or(false),
            ..SessionEconomics::default()
        }
        .budget_line(None, window)
    });
    let percent = match (window, numbers.context) {
        (Some(window), Some(context)) => Some(context_pct(context, window)),
        _ => None,
    };
    json!({
        "id": id,
        "name": live.map(|session| session.name.as_str()).filter(|name| !name.is_empty()),
        "work_item": live.and_then(|session| session.work_item.as_deref()),
        "activity": live.map(|session| session.activity.as_str()),
        "blocked": live.and_then(|session| session.blocked.as_ref()).map(blocked_of),
        "last_reply_excerpt": excerpt,
        "turns": numbers.turns,
        "context_tokens": numbers.context,
        "peak_context_tokens": numbers.peak,
        "compactions": numbers.compactions,
        "output_tokens": numbers.output,
        "usage_measured": numbers.usage_measured,
        "context_pct": percent,
        "budget_line": budget_line,
        "updated_at": timestamp(live.and_then(|session| session.activity_changed_at.as_deref())),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{json, Value};

    use crate::adapters::transcripts::claude_key;
    use crate::application::session_economics::tests::{built, engine_row, run};

    const ENGINE_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    fn row(extra: Value) -> Value {
        let mut summary = json!({
            "id": ENGINE_ID,
            "name": "agent",
            "activity": "idle",
            "cwd": "/srv/vogt",
            "alive": true,
            "created_at": "2026-10-05T00:00:00Z",
            "activity_changed_at": "2026-10-05T00:02:00Z",
            "work_item": "WI-7",
        });
        for (key, value) in extra.as_object().unwrap() {
            summary[key] = value.clone();
        }
        summary
    }

    fn digest(name: &str, summary: Value, params: Value) -> Value {
        let built = built(name, Vec::new(), Some(engine_row(summary)));
        run(&built, "session.digest", params).unwrap()
    }

    #[test]
    fn digest_reads_economics_the_row_already_carries() {
        let result = digest(
            "digest-econ",
            row(json!({"economics": {
                "turns": 96, "context_tokens": 122_000, "peak_context_tokens": 180_000,
                "compactions": 2, "output_tokens": 4000, "usage_measured": true
            }})),
            json!({"id": ENGINE_ID, "context_window": 200_000}),
        );
        assert_eq!(result["id"], ENGINE_ID);
        assert_eq!(result["name"], "agent");
        assert_eq!(result["work_item"], "WI-7");
        assert_eq!(result["activity"], "idle");
        assert_eq!(result["turns"], 96);
        assert_eq!(result["context_tokens"], 122_000);
        assert_eq!(result["peak_context_tokens"], 180_000);
        assert_eq!(result["compactions"], 2);
        assert_eq!(result["output_tokens"], 4000);
        assert_eq!(result["usage_measured"], true);
        assert_eq!(result["context_pct"], 61);
        assert_eq!(result["budget_line"], "96 turns, 61% ctx, 2 compactions");
        assert_eq!(result["updated_at"], "2026-10-05T00:02:00Z");
        assert!(result["blocked"].is_null());
        assert!(result["last_reply_excerpt"].is_null());
    }

    #[test]
    fn absent_economics_stays_null() {
        let result = digest("digest-none", row(json!({})), json!({"id": ENGINE_ID}));
        for field in [
            "turns",
            "context_tokens",
            "peak_context_tokens",
            "compactions",
            "output_tokens",
            "usage_measured",
            "context_pct",
            "budget_line",
        ] {
            assert!(result[field].is_null(), "{field}: {}", result[field]);
        }
    }

    #[test]
    fn no_window_omits_the_percentage() {
        let result = digest(
            "digest-nowindow",
            row(json!({"economics": {"turns": 3, "context_tokens": 120, "compactions": 1}})),
            json!({"id": ENGINE_ID}),
        );
        assert!(result["context_pct"].is_null());
        assert_eq!(result["budget_line"], "3 turns, 1 compaction");
        assert!(result["usage_measured"].is_null());
    }

    #[test]
    fn blocked_report_carries_the_reason_and_the_items() {
        let result = digest(
            "digest-blocked",
            row(json!({"blocked": {
                "reason": "needs the bot token",
                "items": ["create the bot"],
                "since": "2026-10-05T00:03:00Z"
            }})),
            json!({"id": ENGINE_ID}),
        );
        assert_eq!(result["blocked"]["blocker"], "needs the bot token");
        assert_eq!(result["blocked"]["items"], json!(["create the bot"]));
        assert_eq!(result["blocked"]["since"], "2026-10-05T00:03:00Z");
        assert!(result["last_reply_excerpt"].is_null());
    }

    #[test]
    fn a_transcript_fills_the_excerpt() {
        let dir =
            std::env::temp_dir().join(format!("vogt-digest-transcripts-{}", std::process::id()));
        let root = dir.join("claude");
        let directory = root.join(claude_key("/srv/vogt"));
        fs::create_dir_all(&directory).unwrap();
        let long = "x".repeat(200);
        fs::write(
            directory.join(format!("{ENGINE_ID}.jsonl")),
            format!(
                "{}\n",
                json!({
                    "type": "assistant",
                    "timestamp": "2026-10-05T00:04:00Z",
                    "cwd": "/srv/vogt",
                    "message": {"id": "m1", "role": "assistant",
                        "content": [{"type": "text", "text": format!("The bot is created. {long}")}]}
                })
            ),
        )
        .unwrap();
        let summary = row(json!({"command": format!("claude --session-id {ENGINE_ID}")}));
        let built = built(
            "digest-file",
            vec![("claude".to_string(), root)],
            Some(engine_row(summary)),
        );
        let result = run(&built, "session.digest", json!({"id": ENGINE_ID})).unwrap();
        let excerpt = result["last_reply_excerpt"].as_str().unwrap();
        assert!(excerpt.starts_with("The bot is created."), "{excerpt}");
        assert_eq!(excerpt.chars().count(), 120);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_session_is_not_found() {
        let built = built(
            "digest-missing",
            Vec::new(),
            Some(engine_row(row(json!({})))),
        );
        let error = run(&built, "session.digest", json!({"id": "ses_nope"})).unwrap_err();
        assert_eq!(error.to_string(), "no session 'ses_nope'");
    }
}
