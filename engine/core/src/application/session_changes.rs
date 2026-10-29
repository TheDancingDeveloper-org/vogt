//! `session.changes` and `session.wait_any`: an overseer's delta reads
//! (WI-1074, S8b; WI-1246 on the engine).
//!
//! Ports `sessions.py`'s `session_changes` and `session_wait_any`. The engine
//! keeps a bounded ring of the session changes it published; `changes` reads a
//! page of it after a cursor, and `wait_any` blocks until the next match. Both
//! are reads: named sessions resolve to engine ids, and each change names
//! Vogt's `ses_…` id when the engine session is linked.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::adapters::engine::EngineChange;
use crate::application::context::{AppContext, Built};
use crate::application::services::resolve_session;
use crate::application::services::sessions::engine_of;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

const NO_CHANGES_ROUTE: &str = "the session engine does not support changes yet (it has no \
     GET /api/sessions/changes route); upgrade the engine, or poll session.sweep";
const NO_WAIT_ANY_ROUTE: &str = "the session engine does not support wait-any yet (it has no \
     GET /api/sessions/wait-any route); upgrade the engine, or poll session.sweep";

pub(crate) fn session_changes_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| session_changes(ctx, &params))
}

pub(crate) fn session_wait_any_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| session_wait_any(ctx, &params))
}

/// Read session deltas published after `since`. One call replaces
/// re-sweeping every session.
fn session_changes<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let engine = engine_of(ctx)?;
    let ids = engine_ids(ctx, &strings(params.get("ids")))?;
    let since = params.get("since").and_then(Value::as_i64).unwrap_or(0);
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(100);
    // A 409 is an engine that has the path but not the behaviour: the same
    // answer as no route at all.
    let page = match engine.session_changes(since, ids.as_deref(), limit) {
        Ok(page) => page,
        Err(VogtError::Conflict(_)) => None,
        Err(error) => return Err(error),
    };
    let page = page.ok_or_else(|| VogtError::EngineUnavailable(NO_CHANGES_ROUTE.to_string()))?;
    let engine_ids: Vec<&str> = page
        .changes
        .iter()
        .map(|one| one.session.as_str())
        .collect();
    let labels = session_labels(ctx, &engine_ids)?;
    Ok(json!({
        "changes": page.changes.iter().map(|one| change(one, &labels)).collect::<Vec<_>>(),
        "next_cursor": page.next_cursor,
        "truncated": page.truncated,
    }))
}

/// Block until one named session publishes a kind in `until`. The engine does
/// the waiting; this is a read held open for at most `timeout_s`.
fn session_wait_any<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let engine = engine_of(ctx)?;
    let ids = engine_ids(ctx, &strings(params.get("ids")))?;
    let kinds: Vec<String> = strings(params.get("until"))
        .into_iter()
        .map(|one| one.trim().to_string())
        .filter(|one| !one.is_empty())
        .collect();
    // The registry's validator has already made `cursor` a seq, `now` or null.
    let cursor = match params.get("cursor") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Number(number)) => Some(number.to_string()),
        // Python sends `str(int(cursor))`, and a bool is an int to it.
        Some(Value::Bool(flag)) => Some(i64::from(*flag).to_string()),
        _ => None,
    };
    let timeout_s = params
        .get("timeout_s")
        .and_then(Value::as_u64)
        .unwrap_or(120);
    let waited = match engine.wait_any(
        ids.as_deref(),
        (!kinds.is_empty()).then_some(kinds.as_slice()),
        cursor.as_deref(),
        timeout_s,
    ) {
        Ok(waited) => waited,
        Err(VogtError::Conflict(_)) => None,
        Err(error) => return Err(error),
    };
    let waited =
        waited.ok_or_else(|| VogtError::EngineUnavailable(NO_WAIT_ANY_ROUTE.to_string()))?;
    let labels = match &waited.change {
        Some(one) => session_labels(ctx, &[one.session.as_str()])?,
        None => HashMap::new(),
    };
    Ok(json!({
        "outcome": waited.outcome,
        "change": waited.change.as_ref().map(|one| change(one, &labels)),
        "next_cursor": waited.next_cursor,
    }))
}

/// Named sessions resolved to engine ids; `None` means every session.
fn engine_ids<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    ids: &[String],
) -> Result<Option<Vec<String>>, VogtError> {
    let named: Vec<&str> = ids
        .iter()
        .map(|one| one.trim())
        .filter(|one| !one.is_empty())
        .collect();
    if named.is_empty() {
        return Ok(None);
    }
    named
        .into_iter()
        .map(|one| resolve_session(ctx, one))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// Vogt's `ses_…` id for each linked engine session, else the uuid itself.
/// One read for the page: opening the store per change is a query per row.
fn session_labels<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    engine_ids: &[&str],
) -> Result<HashMap<String, String>, VogtError> {
    let view = ctx.declared.read()?;
    let mut labels = HashMap::new();
    for engine_id in engine_ids {
        if labels.contains_key(*engine_id) {
            continue;
        }
        let label = match view.session_by_engine_id(engine_id)? {
            Some(linked) => linked.id,
            None => (*engine_id).to_string(),
        };
        labels.insert((*engine_id).to_string(), label);
    }
    Ok(labels)
}

fn change(one: &EngineChange, labels: &HashMap<String, String>) -> Value {
    json!({
        "seq": one.seq,
        "at": engine_timestamp(&one.at),
        "session": labels.get(&one.session).unwrap_or(&one.session),
        "kind": one.kind,
        "summary": one.summary,
    })
}

/// The engine's RFC3339 string as pydantic renders a datetime, or `None` when
/// it does not parse (Python's `_parse_engine_timestamp`).
fn engine_timestamp(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    crate::core::from_iso(&value.replace('Z', "+00:00"))
        .ok()
        .map(|moment| moment.to_json())
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use crate::adapters::engine::EngineClient;
    use crate::application::context::{build_context, Built};
    use crate::application::writes::audited_write;
    use crate::core::{ActorKind, CodingSession, Moment, Principal, Project, StepClock};
    use crate::errors::VogtError;
    use crate::registry::default_registry;
    use crate::storage::interface::WriteTxn;

    const UNLINKED: &str = "9f0c0000-0000-4000-8000-000000000002";

    fn built(name: &str, engine: EngineClient) -> Built {
        let dir = std::env::temp_dir().join(format!("vogt-changes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap();
        seed(&built);
        built
    }

    /// An engine that records each query and answers `status` with `body`.
    fn engine(status: u16, body: &'static str) -> (Arc<Mutex<Vec<String>>>, EngineClient) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let client = EngineClient::new(
            "http://engine.test",
            None,
            Some(Arc::new(move |url, _, _, _| {
                record.lock().unwrap().push(url.to_string());
                (status, body.as_bytes().to_vec())
            })),
        );
        (seen, client)
    }

    fn seed(built: &Built) {
        crate::with_ctx!(built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            audited_write(&mut write, "seed", "test", |txn, actor| {
                txn.insert_project(&Project::new(
                    "prj_1",
                    "proj",
                    "Proj",
                    "/work",
                    Moment::from_unix(1_700_000_000, 0),
                ))?;
                txn.insert_session(&CodingSession {
                    id: "ses_0001".to_string(),
                    engine_session_id: "eng-1".to_string(),
                    project_id: "prj_1".to_string(),
                    work_item_id: None,
                    actor_id: actor.id.clone(),
                    cwd: "/work".to_string(),
                    template: Some("claude".to_string()),
                    model: None,
                    effort: None,
                    reason: "test".to_string(),
                    purpose: None,
                    started_by_session: None,
                    started_by_kind: None,
                    started_at: Moment::from_unix(1_700_000_000, 0),
                    stopped_at: None,
                })?;
                Ok(crate::application::writes::WriteOutcome {
                    result: json!({}),
                    entity_kind: "session".to_string(),
                    entity_id: "ses_0001".to_string(),
                    payload: json!({}),
                    event_kind: "session.started".to_string(),
                    summary: json!({}),
                })
            })
        })
        .unwrap();
    }

    fn run(built: &Built, op: &str, params: Value) -> Result<Value, VogtError> {
        default_registry()
            .get(op)
            .expect("the operation is registered")
            .run(Some(built), params)
    }

    #[test]
    fn changes_maps_a_linked_session_and_keeps_an_unlinked_uuid() {
        let (seen, client) = engine(
            200,
            r#"{"changes":[
                {"seq":7,"at":"2026-01-01T00:00:00Z","session":"eng-1","kind":"ready","summary":"ready"},
                {"seq":8,"at":"2026-01-01T00:00:01Z","session":"9f0c0000-0000-4000-8000-000000000002","kind":"blocked","summary":"blocked: needs a token"}
            ],"next_cursor":8,"truncated":false}"#,
        );
        let built = built("map", client);
        let page = run(
            &built,
            "session.changes",
            json!({"since": 3, "ids": ["ses_0001"], "limit": 10}),
        )
        .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0],
            "http://engine.test/api/sessions/changes?since=3&limit=10&ids=eng-1"
        );
        assert_eq!(page["next_cursor"], 8);
        assert_eq!(page["truncated"], false);
        let changes = page["changes"].as_array().unwrap();
        assert_eq!(changes[0]["session"], "ses_0001");
        assert_eq!(changes[0]["kind"], "ready");
        assert_eq!(changes[0]["seq"], 7);
        assert!(changes[0]["at"].is_string());
        assert_eq!(changes[1]["session"], UNLINKED);
        assert_eq!(changes[1]["summary"], "blocked: needs a token");
    }

    #[test]
    fn changes_on_an_engine_without_the_route_says_so() {
        for status in [404, 409] {
            let (_, client) = engine(status, "");
            let built = built(&format!("noroute-{status}"), client);
            let error = run(&built, "session.changes", json!({})).unwrap_err();
            assert!(matches!(&error, VogtError::EngineUnavailable(text)
                if text.contains("does not support changes yet")));
        }
    }

    #[test]
    fn changes_rejects_an_unknown_session() {
        let (seen, client) = engine(200, "{}");
        let built = built("unknown", client);
        let error = run(&built, "session.changes", json!({"ids": ["ses_nope"]})).unwrap_err();
        assert!(matches!(error, VogtError::NotFound(_)));
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn wait_any_passes_its_query_and_returns_the_mapped_change() {
        let (seen, client) = engine(
            200,
            r#"{"outcome":"changed","change":{"seq":8,"at":"2026-01-01T00:00:01Z","session":"9f0c0000-0000-4000-8000-000000000002","kind":"blocked","summary":"blocked"},"next_cursor":8}"#,
        );
        let built = built("waitany", client);
        let waited = run(
            &built,
            "session.wait_any",
            json!({"ids": ["ses_0001", UNLINKED], "until": ["blocked"], "cursor": 4, "timeout_s": 30}),
        )
        .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0],
            format!(
                "http://engine.test/api/sessions/wait-any?timeout_s=30&ids=eng-1%2C{UNLINKED}&until=blocked&cursor=4"
            )
        );
        assert_eq!(waited["outcome"], "changed");
        assert_eq!(waited["change"]["session"], UNLINKED);
        assert_eq!(waited["next_cursor"], 8);
    }

    #[test]
    fn wait_any_timeout_has_no_change() {
        let (seen, client) = engine(
            200,
            r#"{"outcome":"timeout","change":null,"next_cursor":3}"#,
        );
        let built = built("timeout", client);
        let waited = run(
            &built,
            "session.wait_any",
            json!({"until": ["exited"], "timeout_s": 5}),
        )
        .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0],
            "http://engine.test/api/sessions/wait-any?timeout_s=5&until=exited"
        );
        assert_eq!(waited["outcome"], "timeout");
        assert!(waited["change"].is_null());
        assert_eq!(waited["next_cursor"], 3);
    }

    #[test]
    fn wait_any_on_an_engine_without_the_route_says_so() {
        for status in [404, 409] {
            let (_, client) = engine(status, "");
            let built = built(&format!("waitnoroute-{status}"), client);
            let error = run(&built, "session.wait_any", json!({})).unwrap_err();
            assert!(matches!(&error, VogtError::EngineUnavailable(text)
                if text.contains("does not support wait-any yet")));
        }
    }

    #[test]
    fn wait_any_cursor_now_reaches_the_engine_as_now() {
        // Mirrors test_session_drive.py: `now` skips what is already published.
        let (seen, client) = engine(
            200,
            r#"{"outcome":"timeout","change":null,"next_cursor":12}"#,
        );
        let built = built("cursornow", client);
        let waited = run(
            &built,
            "session.wait_any",
            json!({"cursor": "NOW", "timeout_s": 5}),
        )
        .unwrap();
        assert_eq!(
            seen.lock().unwrap()[0],
            "http://engine.test/api/sessions/wait-any?timeout_s=5&cursor=now"
        );
        assert_eq!(waited["next_cursor"], 12);
    }

    #[test]
    fn wait_any_bounds_its_timeout_and_its_cursor() {
        let (seen, client) = engine(200, "{}");
        let built = built("bounds", client);
        assert!(run(&built, "session.wait_any", json!({"timeout_s": 601})).is_err());
        let error = run(&built, "session.wait_any", json!({"cursor": "yesterday"})).unwrap_err();
        assert!(error.to_string().contains("cursor"));
        assert!(seen.lock().unwrap().is_empty());
    }
}
