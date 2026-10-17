//! An agent reporting itself blocked on a person, and clearing that. Ports
//! `report_blocked`, `report_unblocked` and `_set_blocked` from
//! `src/vogt/application/services/sessions.py`.
//!
//! The report lives on the engine's session (live state, gone when the session
//! is), where `session.list`, the screen, `session.wait`, the Inbox and a push
//! all see it. Audited here, after the effect, with the text: the report is
//! meant to be read.

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::sessions::{engine_of, resolve_target};
use crate::application::writes::{audited_action, validate_reason};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;

const REPORT_BLOCKED: &str = "session.report_blocked";
const REPORT_UNBLOCKED: &str = "session.report_unblocked";
const BLOCKED_EVENT: &str = "session.blocked";
const UNBLOCKED_EVENT: &str = "session.unblocked";

const OWN_SESSION_PREFIX: &str = "agent:session:";
const NOT_A_SESSION: &str = "give `id`: this caller is not a session Vogt \
    started, so it does not name one (inside a session, pass \
    $VOGT_ENGINE_SESSION_ID)";

pub fn report_blocked_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = required_text(&params, "reason", REPORT_BLOCKED)?;
    let blocker = params
        .get("blocker")
        .and_then(Value::as_str)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{REPORT_BLOCKED} needs a blocker")))?
        .trim()
        .to_string();
    let items = params
        .get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // Python treats an exactly-empty id as omitted (`params.id or
    // _own_session`), so it takes the own-session path. Whitespace is given:
    // the shared resolver refuses it, and trimming first would fall through.
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    crate::with_ctx!(ctx, |ctx| set_blocked(
        ctx,
        id.as_deref(),
        &reason,
        Some(&blocker),
        &items
    ))
}

pub fn report_unblocked_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = required_text(&params, "reason", REPORT_UNBLOCKED)?;
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    crate::with_ctx!(ctx, |ctx| set_blocked(
        ctx,
        id.as_deref(),
        &reason,
        None,
        &[]
    ))
}

fn set_blocked<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    id: Option<&str>,
    reason: &str,
    blocker: Option<&str>,
    items: &[String],
) -> Result<Value, VogtError> {
    let reason = validate_reason(reason)?;
    // Python asks for the engine before resolving the own session, so a caller
    // with no engine and no id hears that no engine is configured.
    let engine = engine_of(ctx)?;
    let reference = match id {
        Some(id) => id.to_string(),
        None => own_session(ctx)?,
    };
    let target = resolve_target(ctx, &reference)?;
    let engine_id = target.engine_session_id.clone();
    let updated = engine
        .set_blocked(&engine_id, blocker.is_some(), blocker, items)?
        .ok_or_else(|| {
            VogtError::NotFound(format!(
                "the engine has no live session {}",
                crate::core::py_repr(&engine_id)
            ))
        })?;
    let entity_id = target
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let outcome = json!({
        "engine_session_id": engine_id,
        "linked": target.session.is_some(),
        "blocker": blocker,
        "items": items,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        if blocker.is_some() {
            REPORT_BLOCKED
        } else {
            REPORT_UNBLOCKED
        },
        &reason,
        "session",
        &entity_id,
        &outcome,
        if blocker.is_some() {
            BLOCKED_EVENT
        } else {
            UNBLOCKED_EVENT
        },
        None,
    )?;
    Ok(json!({
        "id": entity_id,
        "engine_session_id": engine_id,
        "blocked": blocked_of(&updated),
    }))
}

/// The engine's blocked report as the operation result writes it: `reason` on
/// the wire becomes `blocker`, and an unreadable `since` is null rather than
/// an error. The live-summary struct in `services::sessions` keeps the wire
/// name; that one belongs to the other half.
fn blocked_of(session: &crate::adapters::engine::EngineSession) -> Value {
    match &session.blocked {
        None => Value::Null,
        Some(blocked) => json!({
            "blocker": blocked.reason,
            "items": blocked.items,
            "since": blocked.since.as_deref().and_then(engine_timestamp),
        }),
    }
}

/// The engine's RFC3339 string in the form pydantic writes for
/// `model_dump(mode="json")`, or `None` when it does not parse. Python's
/// `datetime.fromisoformat` accepts a trailing `Z` only after it is rewritten
/// to `+00:00`; the result is then re-rendered with the `Z` suffix.
fn engine_timestamp(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    crate::core::from_iso(&value.replace('Z', "+00:00"))
        .ok()
        .map(|moment| moment.to_json())
}

/// The session a session's own token belongs to, for an omitted `id`.
///
/// A session Vogt started holds a token bound to the actor
/// `agent:session:<ses_…>`, so that actor names it. Any other caller must say
/// which session it means. An empty remainder is still a name (Python returns
/// `removeprefix` unfiltered); the shared resolver refuses it afterwards.
fn own_session<C: Clock, I: IdFactory>(ctx: &AppContext<C, I>) -> Result<String, VogtError> {
    match ctx.principal.identity_ref.strip_prefix(OWN_SESSION_PREFIX) {
        Some(id) => Ok(id.to_string()),
        None => Err(VogtError::InvalidRequest(NOT_A_SESSION.to_string())),
    }
}

fn required_text(params: &Value, name: &str, operation: &str) -> Result<String, VogtError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a {name}")))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use crate::adapters::engine::EngineClient;
    use crate::application::context::build_context;
    use crate::application::writes::audited_write;
    use crate::core::{ActorKind, CodingSession, Moment, Principal, StepClock};
    use crate::errors::VogtError;
    use crate::storage::interface::{AuditQuery, DeclaredStore, ReadView, WriteTxn};

    struct Seen(Arc<Mutex<Vec<(String, String, Value)>>>);

    fn fresh(name: &str, principal: Principal, seen: &Seen) -> crate::application::context::Built {
        let dir = std::env::temp_dir().join(format!("vogt-blocked-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let calls = Arc::clone(&seen.0);
        let engine = EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |path, _, body, method| {
                let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                calls
                    .lock()
                    .unwrap()
                    .push((method.to_string(), path.to_string(), payload.clone()));
                if path.contains("missing") {
                    return (404, b"{}".to_vec());
                }
                let mut answer = json!({
                    "id": "eng-1",
                    "alive": true,
                    "activity": "idle",
                });
                if payload.get("blocked") == Some(&Value::Bool(true)) {
                    answer["blocked"] = json!({
                        "reason": payload["reason"],
                        "items": payload["items"],
                        "since": "2026-10-11T00:00:00Z",
                    });
                }
                (200, answer.to_string().into_bytes())
            })),
        );
        build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap()
    }

    fn seed(built: &crate::application::context::Built) {
        crate::with_ctx!(built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            audited_write(&mut write, "seed", "test", |txn, actor| {
                txn.insert_project(&crate::core::Project::new(
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
                    template: None,
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

    fn session_principal() -> Principal {
        Principal::new("agent:session:ses_0001", ActorKind::Agent, "Session").unwrap()
    }

    #[test]
    fn a_session_reports_itself_blocked_without_naming_itself() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let built = fresh("self", session_principal(), &seen);
        seed(&built);
        let reported = super::report_blocked_op(
            &built,
            json!({
                "blocker": "  need a decision  ",
                "items": [" choose a colour ", "  ", "ship it"],
                "reason": "stuck",
            }),
        )
        .unwrap();
        assert_eq!(reported["id"], json!("ses_0001"));
        assert_eq!(reported["engine_session_id"], json!("eng-1"));
        assert_eq!(
            reported["blocked"],
            json!({
                "blocker": "need a decision",
                "items": ["choose a colour", "ship it"],
                "since": "2026-10-11T00:00:00Z",
            })
        );
        let calls = seen.0.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "POST");
        assert!(calls[0].1.ends_with("/eng-1/blocked"), "{}", calls[0].1);
        assert_eq!(
            calls[0].2,
            json!({
                "blocked": true,
                "reason": "need a decision",
                "items": ["choose a colour", "ship it"],
            })
        );
        drop(calls);

        let cleared = super::report_unblocked_op(&built, json!({"reason": "decided"})).unwrap();
        assert_eq!(cleared["blocked"], json!(null));

        let (audits, events) = crate::with_ctx!(built, |ctx| {
            let view = ctx.declared.read().unwrap();
            let audits = view
                .list_audit(&AuditQuery {
                    limit: 20,
                    offset: 0,
                    actor_id: None,
                    operation: None,
                    entity_id: Some("ses_0001".to_string()),
                    project_id: None,
                    since: None,
                    until: None,
                })
                .unwrap();
            let events = view.list_events(0, 20, Some("ses_0001")).unwrap();
            (audits, events)
        });
        let operations: Vec<&str> = audits.iter().map(|row| row.operation.as_str()).collect();
        assert!(
            operations.contains(&"session.report_blocked"),
            "{operations:?}"
        );
        assert!(
            operations.contains(&"session.report_unblocked"),
            "{operations:?}"
        );
        let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
        assert!(kinds.contains(&"session.blocked"), "{kinds:?}");
        assert!(kinds.contains(&"session.unblocked"), "{kinds:?}");
        let blocked = events
            .iter()
            .find(|event| event.kind == "session.blocked")
            .unwrap();
        assert_eq!(blocked.summary["blocker"], json!("need a decision"));
        assert_eq!(
            blocked.summary["items"],
            json!(["choose a colour", "ship it"])
        );
    }

    #[test]
    fn a_caller_that_is_not_a_session_must_name_one() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let built = fresh(
            "human",
            Principal::new("local:ada", ActorKind::Human, "Ada").unwrap(),
            &seen,
        );
        let err = super::report_blocked_op(&built, json!({"blocker": "x", "reason": "because"}))
            .unwrap_err();
        assert!(
            matches!(&err, VogtError::InvalidRequest(message) if message.contains("give `id`")),
            "{err:?}"
        );
        assert!(seen.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_whitespace_id_does_not_fall_through_to_the_own_session() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let built = fresh("blank", session_principal(), &seen);
        let err = super::report_blocked_op(
            &built,
            json!({"id": "   ", "blocker": "x", "reason": "because"}),
        )
        .unwrap_err();
        assert!(
            matches!(&err, VogtError::InvalidRequest(message) if message.contains("a session id is required")),
            "{err:?}"
        );
        assert!(seen.0.lock().unwrap().is_empty());
    }

    #[test]
    fn an_empty_id_is_the_own_session() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let built = fresh("empty", session_principal(), &seen);
        seed(&built);
        let reported = super::report_blocked_op(
            &built,
            json!({"id": "", "blocker": "need a decision", "reason": "because"}),
        )
        .unwrap();
        assert_eq!(reported["id"], "ses_0001");
        assert_eq!(seen.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn no_engine_is_reported_before_the_own_session() {
        let dir =
            std::env::temp_dir().join(format!("vogt-blocked-noengine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let err = super::report_unblocked_op(&built, json!({"reason": "because"})).unwrap_err();
        assert!(
            matches!(&err, VogtError::EngineUnavailable(message) if message.contains("no session engine is configured")),
            "{err:?}"
        );
    }

    #[test]
    fn a_session_the_engine_has_lost_is_not_found() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let built = fresh(
            "gone",
            Principal::new("local:ada", ActorKind::Human, "Ada").unwrap(),
            &seen,
        );
        let err = super::report_blocked_op(
            &built,
            json!({"id": "missing", "blocker": "x", "reason": "because"}),
        )
        .unwrap_err();
        assert!(
            matches!(&err, VogtError::NotFound(message) if message.contains("the engine has no live session")),
            "{err:?}"
        );
    }

    #[test]
    fn an_unreadable_since_is_null() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let dir = std::env::temp_dir().join(format!("vogt-blocked-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let calls = Arc::clone(&seen.0);
        let engine = EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |path, _, body, method| {
                calls.lock().unwrap().push((
                    method.to_string(),
                    path.to_string(),
                    serde_json::from_slice(body).unwrap_or(Value::Null),
                ));
                (
                    200,
                    br#"{"id":"eng-1","blocked":{"reason":"x","items":[],"since":"not-a-time"}}"#
                        .to_vec(),
                )
            })),
        );
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap();
        let reported =
            super::report_unblocked_op(&built, json!({"id": "eng-1", "reason": "cleared"}))
                .unwrap();
        assert_eq!(reported["blocked"]["since"], json!(null));
        assert_eq!(reported["id"], json!("eng-1"));
    }
}
