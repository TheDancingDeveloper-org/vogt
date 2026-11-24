//! An agent reporting itself blocked on a person, and clearing that. Ports
//! `report_blocked`, `report_unblocked` and `_set_blocked` from
//! `src/vogt/application/services/sessions.py`.
//!
//! The report lives on the engine's session (live state, gone when the session
//! is), where `session.list`, the screen, `session.wait`, the Inbox and a push
//! all see it. Audited here, after the effect, with the text: the report is
//! meant to be read.

use serde_json::{json, Value};
use sha2::Digest;

use crate::adapters::engine::EngineApproval;
use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::sessions::{engine_of, resolve_target};
use crate::application::writes::{audited_action, validate_reason};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::ReadView;

const REPORT_BLOCKED: &str = "session.report_blocked";
const REPORT_UNBLOCKED: &str = "session.report_unblocked";
const BLOCKED_EVENT: &str = "session.blocked";
const UNBLOCKED_EVENT: &str = "session.unblocked";

/// Audited when an agent's answer to a permission prompt is refused: its pick
/// becomes a recommendation shown beside the person's buttons, never chosen
/// (WI-996 E0b). Its entity is the dialog's escalation key.
pub(crate) const SESSION_ANSWER_RECOMMENDED: &str = "session.answer_recommended";
/// The entity kind a recommendation is audited under.
pub(crate) const RECOMMENDATION_ENTITY: &str = "escalation";
/// The most recommendation events read for one dialog.
const RECOMMENDATION_SCAN: i64 = 50;

/// Who can resolve a blocked report (WI-996). A report that names none needs a
/// person.
pub(crate) const NEEDS: [&str; 3] = ["person", "overseer", "either"];
const NEEDS_DEFAULT: &str = "person";
/// What kind of boundary a report is (WI-996). A report that names none is
/// `other`.
pub(crate) const BOUNDARIES: [&str; 5] = [
    "permission_prompt",
    "denied_action",
    "credential",
    "decision",
    "other",
];
const BOUNDARY_DEFAULT: &str = "other";

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
    // Who can resolve it and what kind of boundary it is (WI-996). Both are
    // checked before the engine is asked, as pydantic checks them before
    // Python's service runs.
    let (needs, boundary) = escalation_of(&params)?;
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
        &items,
        needs,
        boundary,
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
        &[],
        NEEDS_DEFAULT,
        BOUNDARY_DEFAULT,
    ))
}

/// `needs` and `boundary` from a report. Both are optional: a report that
/// names neither needs a person, and its boundary is `other`.
fn escalation_of(params: &Value) -> Result<(&'static str, &'static str), VogtError> {
    let needs = match params.get("needs").and_then(Value::as_str) {
        None => NEEDS_DEFAULT,
        Some(text) => NEEDS
            .iter()
            .copied()
            .find(|known| *known == text)
            .ok_or_else(|| {
                VogtError::InvalidRequest(format!(
                    "needs must be person, overseer or either, not {}",
                    crate::core::py_repr(text)
                ))
            })?,
    };
    let boundary = match params.get("boundary").and_then(Value::as_str) {
        None => BOUNDARY_DEFAULT,
        Some(text) => BOUNDARIES
            .iter()
            .copied()
            .find(|known| *known == text)
            .ok_or_else(|| {
                VogtError::InvalidRequest(format!(
                    "boundary must be permission_prompt, denied_action, credential, decision or other, not {}",
                    crate::core::py_repr(text)
                ))
            })?,
    };
    Ok((needs, boundary))
}

fn set_blocked<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    id: Option<&str>,
    reason: &str,
    blocker: Option<&str>,
    items: &[String],
    needs: &str,
    boundary: &str,
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
        .set_blocked(
            &engine_id,
            blocker.is_some(),
            blocker,
            items,
            needs,
            boundary,
        )?
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
    let mut outcome = json!({
        "engine_session_id": engine_id,
        "linked": target.session.is_some(),
        "blocker": blocker,
        "items": items,
    });
    if blocker.is_some() {
        outcome["needs"] = json!(needs);
        outcome["boundary"] = json!(boundary);
    }
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
/// name; that one belongs to the other half. `needs` and `boundary` fall back
/// as the engine's value does (an unknown one reads as a person and `other`).
fn blocked_of(session: &crate::adapters::engine::EngineSession) -> Value {
    match &session.blocked {
        None => Value::Null,
        Some(blocked) => json!({
            "blocker": blocked.reason,
            "items": blocked.items,
            "since": blocked.since.as_deref().and_then(engine_timestamp),
            "needs": known_or(&blocked.needs, &NEEDS, NEEDS_DEFAULT),
            "boundary": known_or(&blocked.boundary, &BOUNDARIES, BOUNDARY_DEFAULT),
            "key": blocked.key,
        }),
    }
}

/// `value` when it is one of `known`, else `fallback`: a newer engine's value
/// reads as the default rather than failing the read.
pub(crate) fn known_or(
    value: &str,
    known: &[&'static str],
    fallback: &'static str,
) -> &'static str {
    known
        .iter()
        .copied()
        .find(|item| *item == value)
        .unwrap_or(fallback)
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

// -- escalation keys and the overseer's recommendation (WI-996) -------------
//
// A permission dialog's escalation key is `<engine id>:permission_prompt:` and
// the first 16 hex characters of sha256 of its question. The engine sends its
// own key on the dialog; the hash is only the fallback for an older engine.

/// The fallback escalation key of the dialog `question` on that engine session.
pub(crate) fn approval_key(engine_session_id: &str, question: &str) -> String {
    let digest = hex::encode(sha2::Sha256::digest(question.trim().as_bytes()));
    format!("{engine_session_id}:permission_prompt:{}", &digest[..16])
}

/// A dialog's escalation key: the engine's own when it sent one.
pub(crate) fn dialog_key(engine_session_id: &str, engine_key: &str, question: &str) -> String {
    if engine_key.is_empty() {
        approval_key(engine_session_id, question)
    } else {
        engine_key.to_string()
    }
}

/// Which showing of a dialog a recommendation was made on. Nearly every dialog
/// asks the same question, so the key alone would carry a pick onto a later
/// dialog running a different command.
pub(crate) fn dialog_instance(detected_at: &str, command_excerpt: &str) -> String {
    let text = format!("{}\0{}", detected_at.trim(), command_excerpt.trim());
    hex::encode(sha2::Sha256::digest(text.as_bytes()))[..16].to_string()
}

/// The newest recommendation recorded on this dialog, when it still names an
/// option the dialog shows with the same label and is the same showing of the
/// dialog. Never chosen for the person: the Inbox and the oversight board show
/// it beside the buttons.
pub(crate) fn dialog_recommendation(
    view: &impl ReadView,
    session_id: &str,
    approval: &EngineApproval,
) -> Result<Option<Value>, VogtError> {
    let key = dialog_key(session_id, &approval.key, &approval.question);
    let events = view.list_entity_events_newest(&key, RECOMMENDATION_SCAN)?;
    let instance = dialog_instance(&approval.detected_at, &approval.command_excerpt);
    for event in &events {
        if event.kind != SESSION_ANSWER_RECOMMENDED
            || event.summary.get("dialog").and_then(Value::as_str) != Some(instance.as_str())
        {
            continue;
        }
        let option = event.summary.get("option").and_then(Value::as_i64);
        let label = event.summary.get("label").and_then(Value::as_str);
        let (Some(option), Some(label)) = (option, label) else {
            continue;
        };
        // The record holds the label cut to 200 characters (`record_recommendation`),
        // so the shown one is cut the same way before the comparison.
        let shown = approval
            .options
            .iter()
            .find(|(number, _, _)| *number == option)
            .map(|(_, text, _)| truncate_chars(text, 200));
        if shown.as_deref() != Some(label) {
            continue;
        }
        return Ok(Some(json!({
            "option": option,
            "label": label,
            "by": event.summary.get("by").and_then(Value::as_str).unwrap_or("an agent"),
            "at": event.at.to_json(),
            "escalation_key": key,
        })));
    }
    Ok(None)
}

/// Record a refused answer as the recommendation on the dialog `key`. Only an
/// option the dialog shows is recorded, by its number and the engine's label,
/// so the Inbox never repeats the caller's own text. An answer that matches no
/// option, or more than one, records nothing. Returns whether it recorded.
pub(crate) fn record_recommendation<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    key: &str,
    approval: &EngineApproval,
    pick: (Option<i64>, Option<&str>),
    reason: &str,
) -> Result<bool, VogtError> {
    let (option, label) = pick;
    let wanted = label.map(|text| text.trim().to_lowercase());
    let matches: Vec<&(i64, String, bool)> = approval
        .options
        .iter()
        .filter(|(number, text, _)| {
            option == Some(*number)
                || wanted
                    .as_deref()
                    .is_some_and(|wanted| text.trim().to_lowercase() == wanted)
        })
        .collect();
    let [(number, text, _)] = matches.as_slice() else {
        return Ok(false);
    };
    let outcome = json!({
        "escalation_key": key,
        "question": truncate_chars(approval.question.trim(), 300),
        "option": number,
        "label": truncate_chars(text, 200),
        "dialog": dialog_instance(&approval.detected_at, &approval.command_excerpt),
        "by": ctx.principal.identity_ref,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_ANSWER_RECOMMENDED,
        reason,
        RECOMMENDATION_ENTITY,
        key,
        &outcome,
        SESSION_ANSWER_RECOMMENDED,
        None,
    )?;
    Ok(true)
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
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
                        "needs": payload["needs"],
                        "boundary": payload["boundary"],
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
                    id: "ses_00000000000000000000000001".to_string(),
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
                    entity_id: "ses_00000000000000000000000001".to_string(),
                    payload: json!({}),
                    event_kind: "session.started".to_string(),
                    summary: json!({}),
                })
            })
        })
        .unwrap();
    }

    fn session_principal() -> Principal {
        Principal::new(
            "agent:session:ses_00000000000000000000000001",
            ActorKind::Agent,
            "Session",
        )
        .unwrap()
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
        assert_eq!(reported["id"], json!("ses_00000000000000000000000001"));
        assert_eq!(reported["engine_session_id"], json!("eng-1"));
        assert_eq!(
            reported["blocked"],
            json!({
                "blocker": "need a decision",
                "items": ["choose a colour", "ship it"],
                "since": "2026-10-11T00:00:00Z",
                "needs": "person",
                "boundary": "other",
                "key": null,
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
                "needs": "person",
                "boundary": "other",
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
                    entity_id: Some("ses_00000000000000000000000001".to_string()),
                    project_id: None,
                    since: None,
                    until: None,
                })
                .unwrap();
            let events = view
                .list_events(0, 20, Some("ses_00000000000000000000000001"))
                .unwrap();
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
    fn a_report_names_who_resolves_it_and_its_boundary() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let built = fresh("needs", session_principal(), &seen);
        seed(&built);
        let reported = super::report_blocked_op(
            &built,
            json!({
                "blocker": "the token is not held",
                "reason": "stuck",
                "needs": "overseer",
                "boundary": "credential",
            }),
        )
        .unwrap();
        assert_eq!(reported["blocked"]["needs"], json!("overseer"));
        assert_eq!(reported["blocked"]["boundary"], json!("credential"));
        let calls = seen.0.lock().unwrap();
        assert_eq!(calls[0].2["needs"], json!("overseer"));
        assert_eq!(calls[0].2["boundary"], json!("credential"));
        drop(calls);

        let events = crate::with_ctx!(&built, |ctx| {
            let view = ctx.declared.read().unwrap();
            view.list_events(0, 20, Some("ses_00000000000000000000000001"))
                .unwrap()
        });
        let blocked = events
            .iter()
            .find(|event| event.kind == "session.blocked")
            .unwrap();
        assert_eq!(blocked.summary["needs"], json!("overseer"));
        assert_eq!(blocked.summary["boundary"], json!("credential"));
    }

    #[test]
    fn an_unknown_needs_or_boundary_is_refused_before_the_engine_is_asked() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let built = fresh("badneeds", session_principal(), &seen);
        let err = super::report_blocked_op(
            &built,
            json!({"blocker": "x", "reason": "because", "needs": "boss"}),
        )
        .unwrap_err();
        assert!(
            matches!(&err, VogtError::InvalidRequest(message) if message == "needs must be person, overseer or either, not 'boss'"),
            "{err:?}"
        );
        let err = super::report_blocked_op(
            &built,
            json!({"blocker": "x", "reason": "because", "boundary": "whim"}),
        )
        .unwrap_err();
        assert!(
            matches!(&err, VogtError::InvalidRequest(message) if message.starts_with("boundary must be")),
            "{err:?}"
        );
        assert!(seen.0.lock().unwrap().is_empty());
    }

    #[test]
    fn the_engines_key_is_echoed_and_an_unknown_boundary_reads_as_other() {
        let seen = Seen(Arc::new(Mutex::new(Vec::new())));
        let dir = std::env::temp_dir().join(format!("vogt-blocked-key-{}", std::process::id()));
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
                    br#"{"id":"eng-1","blocked":{"reason":"x","items":[],"since":"2026-10-11T00:00:00Z","needs":"either","boundary":"whim","key":"eng-1:other:0123456789abcdef"}}"#
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
        let reported = super::report_blocked_op(
            &built,
            json!({"id": "eng-1", "blocker": "x", "reason": "because"}),
        )
        .unwrap();
        assert_eq!(reported["blocked"]["needs"], json!("either"));
        assert_eq!(reported["blocked"]["boundary"], json!("other"));
        assert_eq!(
            reported["blocked"]["key"],
            json!("eng-1:other:0123456789abcdef")
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
        assert_eq!(reported["id"], "ses_00000000000000000000000001");
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
