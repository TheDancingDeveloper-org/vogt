//! Watches: a session waits on an event instead of polling (WI-2036).
//!
//! Ports `watch.create`, `watch.list` and `watch.cancel` from
//! `application/services/watches.py`. The watch loop (evaluate, deliver,
//! hibernate `wake_on`) is not part of this port.

use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::grants::own_engine_session;
use crate::application::models::watches::{WatchKind, WatchUntil};
use crate::application::services::now_of;
use crate::application::writes::{audited_write, validate_reason, WriteOutcome};
use crate::core::{py_repr, ActorKind, Clock, IdFactory, Moment, Watch, WatchState};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const WATCH_CREATED_EVENT: &str = "watch.created";
const WATCH_CANCELLED_EVENT: &str = "watch.cancelled";
const DEFAULT_TTL_SECONDS: i64 = 86_400;
const MIN_TTL_SECONDS: i64 = 60;
const MAX_TTL_SECONDS: i64 = 1_209_600;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
const MAX_NOTE: usize = 400;

pub fn create_watch_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| create(ctx, &params))
}

pub fn list_watches_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| list(ctx, &params))
}

pub fn cancel_watch_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| cancel(ctx, &params))
}

fn create<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = validate_reason(&required(params, "watch.create", "reason")?)?;
    let trigger_value = params
        .get("trigger")
        .cloned()
        .ok_or_else(|| VogtError::InvalidRequest("watch.create needs a trigger".to_string()))?;
    let trigger: crate::application::models::watches::WatchTriggerModel =
        serde_json::from_value(trigger_value)
            .map_err(|err| VogtError::InvalidRequest(format!("trigger: {err}")))?;
    let target = match text_opt(params, "session") {
        Some(named) => engine_id(ctx, &named)?,
        None => own_engine_session(ctx)?.ok_or_else(|| {
            VogtError::InvalidRequest(
                "name the session to wake: the caller is not a session".to_string(),
            )
        })?,
    };
    let note = text(params, "note")
        .trim()
        .chars()
        .take(MAX_NOTE)
        .collect::<String>();
    let ttl = params
        .get("ttl_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_TTL_SECONDS);
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl) {
        return Err(VogtError::InvalidRequest(format!(
            "ttl_seconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}"
        )));
    }
    let stored = resolved_trigger(ctx, &trigger, &target)?;
    let kind = stored
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let mut write = write_of(ctx);
    let clock = Arc::clone(write.clock());
    let ids = Arc::clone(write.ids());
    let watch = audited_write(&mut write, "watch.create", &reason, |txn, actor| {
        let now = now_of(&clock);
        let watch = Watch {
            id: ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .next("wat"),
            owner_actor_id: actor.id.clone(),
            target_session: target.clone(),
            kind: kind.clone(),
            trigger: stored.clone(),
            note: note.clone(),
            state: WatchState::Active,
            created_at: now,
            expires_at: plus_seconds(now, ttl),
            fired_at: None,
            outcome: None,
            attempts: 0,
            last_error: None,
            reason: reason.clone(),
        };
        txn.insert_watch(&watch)?;
        outcome(&*txn, &watch, WATCH_CREATED_EVENT, &clock)
    })?;
    let view = ctx.declared.read()?;
    Ok(json!({ "watch": view_of(&view, &watch, now_of(write_of(ctx).clock()))? }))
}

fn cancel<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = validate_reason(&required(params, "watch.cancel", "reason")?)?;
    let id = required(params, "watch.cancel", "id")?;
    let watch = load(ctx, &id)?;
    if ctx.principal.kind == ActorKind::Agent && !may_cancel(ctx, &watch)? {
        return Err(VogtError::GrantRefused(format!(
            "only a person, the watch's owner or the session it wakes cancels a watch ({} is none of them)",
            ctx.principal.identity_ref
        )));
    }
    let mut write = write_of(ctx);
    let clock = Arc::clone(write.clock());
    let cancelled = audited_write(&mut write, "watch.cancel", &reason, |txn, _actor| {
        let current = txn
            .watch(&watch.id)?
            .ok_or_else(|| VogtError::NotFound(format!("no watch {}", py_repr(&watch.id))))?;
        let state = current.effective_state(now_of(&clock));
        if state != "active" {
            return Err(VogtError::Conflict(format!(
                "watch {} is {state}, not active",
                watch.id
            )));
        }
        let mut cancelled = current;
        cancelled.state = WatchState::Cancelled;
        txn.update_watch(&cancelled)?;
        outcome(&*txn, &cancelled, WATCH_CANCELLED_EVENT, &clock)
    })?;
    let view = ctx.declared.read()?;
    Ok(json!({ "watch": view_of(&view, &cancelled, now_of(&clock))? }))
}

fn list<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let state = params
        .get("state")
        .and_then(Value::as_str)
        .map(str::to_string);
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);
    let stored_state = match state.as_deref() {
        Some("expired") => Some("active"),
        other => other,
    };
    let target = match text_opt(params, "session") {
        Some(named) => Some(engine_id(ctx, &named)?),
        None => None,
    };
    let view = ctx.declared.read()?;
    let mut rows = view.list_watches(stored_state, target.as_deref(), None, limit)?;
    if state.as_deref() == Some("expired") {
        rows.extend(view.list_watches(Some("expired"), target.as_deref(), None, limit)?);
    }
    let now = now_of(&Arc::clone(&ctx.clock));
    let mut views: Vec<Value> = rows
        .iter()
        .map(|row| view_of(&view, row, now))
        .collect::<Result<_, _>>()?;
    if let Some(wanted) = state.as_deref() {
        views.retain(|row| row.get("state").and_then(Value::as_str) == Some(wanted));
    }
    views.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(json!({ "watches": views }))
}

fn resolved_trigger<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    trigger: &crate::application::models::watches::WatchTriggerModel,
    target_session: &str,
) -> Result<Value, VogtError> {
    let mut stored = Map::new();
    let kind = match trigger.kind {
        WatchKind::Pr => "pr",
        WatchKind::Ci => "ci",
        WatchKind::WorkItem => "work_item",
        WatchKind::Session => "session",
    };
    stored.insert("kind".to_string(), json!(kind));
    let view = ctx.declared.read()?;
    match trigger.kind {
        WatchKind::Pr | WatchKind::Ci => {
            let slug = trigger.project.as_deref().unwrap_or("").trim();
            let project = view
                .project_by_slug(slug)?
                .ok_or_else(|| VogtError::NotFound(format!("no project {}", py_repr(slug))))?;
            stored.insert("project".to_string(), json!(project.slug));
            stored.insert("project_id".to_string(), json!(project.id));
        }
        WatchKind::WorkItem | WatchKind::Session => {}
    }
    match trigger.kind {
        WatchKind::Pr => {
            let number = trigger.number.ok_or_else(|| {
                VogtError::InvalidRequest("a pull request watch needs a number".to_string())
            })?;
            stored.insert("number".to_string(), json!(number));
            let mut on: Vec<String> = if trigger.on.is_empty() {
                vec!["closed".to_string(), "merged".to_string()]
            } else {
                trigger
                    .on
                    .iter()
                    .map(|event| match event {
                        crate::application::models::watches::PrWatchEvent::Merged => {
                            "merged".to_string()
                        }
                        crate::application::models::watches::PrWatchEvent::Closed => {
                            "closed".to_string()
                        }
                    })
                    .collect()
            };
            on.sort();
            on.dedup();
            stored.insert("on".to_string(), json!(on));
        }
        WatchKind::Ci => {
            let sha = trigger.sha.as_ref().map(|s| s.trim().to_string());
            let pr = trigger.pr;
            let has_sha = sha.as_ref().is_some_and(|s| !s.is_empty());
            if has_sha == pr.is_some() {
                return Err(VogtError::InvalidRequest(
                    "a CI watch names exactly one of sha or pr".to_string(),
                ));
            }
            stored.insert(
                "sha".to_string(),
                match sha.as_deref() {
                    Some(value) if !value.is_empty() => json!(value),
                    _ => Value::Null,
                },
            );
            stored.insert(
                "pr".to_string(),
                pr.map_or(Value::Null, |number| json!(number)),
            );
            let mut checks: Vec<String> = trigger
                .checks
                .iter()
                .map(|check| check.trim().to_string())
                .filter(|check| !check.is_empty())
                .collect();
            if checks.is_empty() {
                return Err(VogtError::InvalidRequest(
                    "a CI watch names at least one check".to_string(),
                ));
            }
            checks.sort();
            checks.dedup();
            stored.insert("checks".to_string(), json!(checks));
        }
        WatchKind::WorkItem => {
            let reference = trigger.work_item.as_deref().unwrap_or("").trim();
            let item = view.work_item_by_ref(reference)?.ok_or_else(|| {
                VogtError::NotFound(format!("no work item {}", py_repr(reference)))
            })?;
            let mut states: Vec<String> = trigger
                .states
                .iter()
                .map(|state| state.trim().to_string())
                .filter(|state| !state.is_empty())
                .collect();
            if states.is_empty() {
                states = crate::core::TERMINAL_STATES
                    .iter()
                    .map(|state| (*state).to_string())
                    .collect();
                states.sort();
            }
            stored.insert("ref".to_string(), json!(item.reference));
            stored.insert("work_item_id".to_string(), json!(item.id));
            stored.insert("states".to_string(), json!(states));
        }
        WatchKind::Session => {
            let named = trigger.session.as_deref().unwrap_or("");
            let watched = engine_id(ctx, named)?;
            if watched == target_session {
                return Err(VogtError::InvalidRequest(
                    "a session cannot wait for itself to end".to_string(),
                ));
            }
            let until = match trigger.until {
                WatchUntil::Ended => "ended",
                WatchUntil::Done => "done",
            };
            stored.insert("session".to_string(), json!(named));
            stored.insert("engine_session_id".to_string(), json!(watched));
            stored.insert("until".to_string(), json!(until));
        }
    }
    drop(view);
    Ok(Value::Object(stored))
}

fn engine_id<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    session_id: &str,
) -> Result<String, VogtError> {
    let wanted = session_id.trim();
    if wanted.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".to_string(),
        ));
    }
    if !wanted.starts_with("ses_") {
        return Ok(wanted.to_string());
    }
    let session = ctx
        .declared
        .read()?
        .session_by_id(wanted)?
        .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(wanted))))?;
    Ok(session.engine_session_id)
}

fn may_cancel<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    watch: &Watch,
) -> Result<bool, VogtError> {
    let caller = ctx
        .declared
        .read()?
        .actor_by_identity(&ctx.principal.identity_ref)?;
    if caller.is_some_and(|actor| actor.id == watch.owner_actor_id) {
        return Ok(true);
    }
    Ok(own_engine_session(ctx)?.as_deref() == Some(watch.target_session.as_str()))
}

fn load<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    watch_id: &str,
) -> Result<Watch, VogtError> {
    ctx.declared
        .read()?
        .watch(watch_id.trim())?
        .ok_or_else(|| VogtError::NotFound(format!("no watch {}", py_repr(watch_id))))
}

fn outcome<C: Clock>(
    txn: &impl ReadView,
    watch: &Watch,
    event: &str,
    clock: &Arc<Mutex<C>>,
) -> Result<WriteOutcome<Watch>, VogtError> {
    let _ = view_of(txn, watch, now_of(clock))?;
    Ok(WriteOutcome {
        result: watch.clone(),
        entity_kind: "watch".to_string(),
        entity_id: watch.id.clone(),
        payload: serde_json::to_value(watch).unwrap_or(Value::Null),
        event_kind: event.to_string(),
        summary: json!({
            "watch_id": watch.id,
            "kind": watch.kind,
            "session": watch.target_session,
            "waits_for": describe(&watch.kind, &watch.trigger),
            "state": watch.state.to_string(),
            "outcome_state": watch.outcome.as_ref().and_then(|outcome| outcome.get("state").cloned()),
            "attempts": watch.attempts,
        }),
    })
}

fn view_of(view: &impl ReadView, watch: &Watch, now: Moment) -> Result<Value, VogtError> {
    let owner = view.actor_by_id(&watch.owner_actor_id)?;
    let delivered = watch
        .outcome
        .as_ref()
        .and_then(|outcome| outcome.get("delivered").cloned());
    let state = watch.effective_state(now);
    Ok(json!({
        "id": watch.id,
        "owner": owner.map_or_else(|| watch.owner_actor_id.clone(), |actor| actor.identity_ref),
        "session": watch.target_session,
        "kind": watch.kind,
        "trigger": watch.trigger,
        "waits_for": describe(&watch.kind, &watch.trigger),
        "note": watch.note,
        "state": state,
        "created_at": watch.created_at.to_json(),
        "expires_at": watch.expires_at.to_json(),
        "fired_at": watch.fired_at.map(|moment| moment.to_json()),
        "outcome": watch.outcome,
        "delivered": delivered,
        "attempts": watch.attempts,
        "last_error": watch.last_error,
        "reason": watch.reason,
    }))
}

/// The trigger in a few words. Ports `core.watches.describe`.
fn describe(kind: &str, trigger: &Value) -> String {
    match kind {
        "pr" => format!(
            "PR #{} in {}",
            trigger.get("number").and_then(Value::as_i64).unwrap_or(0),
            trigger.get("project").and_then(Value::as_str).unwrap_or("")
        ),
        "ci" => {
            let subject = match trigger.get("pr").and_then(Value::as_i64) {
                Some(number) => format!("CI on PR #{number}"),
                None => {
                    let sha = trigger.get("sha").and_then(Value::as_str).unwrap_or("");
                    let short: String = sha.chars().take(12).collect();
                    format!("CI on {short}")
                }
            };
            format!(
                "{subject} in {}",
                trigger.get("project").and_then(Value::as_str).unwrap_or("")
            )
        }
        "work_item" => format!(
            "{} state",
            trigger.get("ref").and_then(Value::as_str).unwrap_or("")
        ),
        "session" => format!(
            "session {}",
            trigger.get("session").and_then(Value::as_str).unwrap_or("")
        ),
        other => other.to_string(),
    }
}

fn plus_seconds(moment: Moment, seconds: i64) -> Moment {
    Moment::from_unix(
        moment.unix_seconds().saturating_add(seconds),
        moment.nanos(),
    )
}

fn required(params: &Value, operation: &str, name: &str) -> Result<String, VogtError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a {name}")))
}

fn text(params: &Value, name: &str) -> String {
    text_opt(params, name).unwrap_or_default()
}

fn text_opt(params: &Value, name: &str) -> Option<String> {
    params.get(name).and_then(Value::as_str).map(str::to_string)
}
