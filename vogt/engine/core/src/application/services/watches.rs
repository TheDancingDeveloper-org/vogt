//! Watches a session arms so it can sleep until an event (WI-2036). Ports
//! `application/services/watches.py` including scheduled evaluation.
//!
//! `watch.list`, `watch.create` and `watch.cancel` are served. Evaluating
//! watches uses the optional engine for delivery.

use serde_json::{json, Map, Value};

use crate::application::context::{AppContext, Built};
use crate::application::services::now_of;
use crate::core::{Clock, IdFactory, Moment, Watch};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

pub fn watch_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| list(ctx, &params))
}

fn list<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let state = text_opt(params, "state");
    let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(50);
    // `expired` is stored once the loop has seen it, and reads so beforehand.
    let stored_state = match state.as_deref() {
        Some("expired") => Some("active"),
        other => other,
    };
    let view = ctx.declared.read()?;
    let target = match text_opt(params, "session") {
        Some(session) => Some(engine_id(&view, &session)?),
        None => None,
    };
    let mut rows = view.list_watches(stored_state, target.as_deref(), None, limit)?;
    if state.as_deref() == Some("expired") {
        rows.extend(view.list_watches(Some("expired"), target.as_deref(), None, limit)?);
    }
    let now = now_of(&ctx.clock);
    let mut views = Vec::new();
    for row in &rows {
        views.push(view_of(&view, row, now)?);
    }
    if let Some(wanted) = state.as_deref() {
        views.retain(|found| found["state"].as_str() == Some(wanted));
    }
    views.truncate(limit.max(0) as usize);
    Ok(json!({ "watches": views }))
}

/// A `ses_` id is the Vogt session; anything else is already an engine id.
fn engine_id(view: &impl ReadView, session_id: &str) -> Result<String, VogtError> {
    let wanted = session_id.trim();
    if wanted.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".to_string(),
        ));
    }
    if !wanted.starts_with("ses_") {
        return Ok(wanted.to_string());
    }
    match view.session_by_id(wanted)? {
        Some(session) => Ok(session.engine_session_id),
        None => Err(VogtError::NotFound(format!(
            "no session {}",
            crate::core::py_repr(wanted)
        ))),
    }
}

fn view_of(view: &impl ReadView, watch: &Watch, now: Moment) -> Result<Value, VogtError> {
    let owner = match view.actor_by_id(&watch.owner_actor_id)? {
        Some(actor) => actor.identity_ref,
        None => watch.owner_actor_id.clone(),
    };
    let trigger = watch.trigger.as_object().cloned().unwrap_or_else(Map::new);
    let delivered = watch
        .outcome
        .as_ref()
        .and_then(|outcome| outcome.get("delivered"))
        .cloned();
    Ok(json!({
        "id": watch.id,
        "owner": owner,
        "session": watch.target_session,
        "kind": watch.kind,
        "trigger": trigger,
        "waits_for": describe(&watch.kind, &watch.trigger),
        "note": watch.note,
        "state": watch.effective_state(now),
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

/// What a watch waits for, in a few words. Ports `core.watches.describe`.
fn describe(kind: &str, trigger: &Value) -> String {
    match kind {
        "pr" => format!(
            "PR #{} in {}",
            json_text(trigger, "number"),
            json_text(trigger, "project")
        ),
        "ci" => {
            let target = match trigger.get("pr").filter(|value| !value.is_null()) {
                Some(pr) => format!("PR #{pr}"),
                None => json_text(trigger, "sha").chars().take(12).collect(),
            };
            format!("CI on {target} in {}", json_text(trigger, "project"))
        }
        "work_item" => format!("{} state", json_text(trigger, "ref")),
        "session" => format!("session {}", json_text(trigger, "session")),
        other => other.to_string(),
    }
}

fn json_text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

fn text_opt(params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

use std::sync::Arc;

use crate::application::context::write_of;
use crate::application::grants::own_engine_session;
use crate::application::writes::{audited_write, validate_reason, WriteOutcome};
use crate::core::{WatchState, TERMINAL_STATES};
use crate::storage::interface::WriteTxn;

const WATCH_CREATED_EVENT: &str = "watch.created";
const WATCH_CANCELLED_EVENT: &str = "watch.cancelled";
const DEFAULT_TTL_SECONDS: i64 = 86_400;

pub fn watch_create_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| create(ctx, &params))
}

pub fn watch_cancel_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(built, |ctx| cancel(ctx, &params))
}

fn create<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = validate_reason(text(params, "reason").as_str())?;
    let target = match text_opt(params, "session") {
        Some(session) => {
            let view = ctx.declared.read()?;
            engine_id(&view, &session)?
        }
        None => own_engine_session(ctx)?.ok_or_else(|| {
            VogtError::InvalidRequest(
                "name the session to wake: the caller is not a session".to_string(),
            )
        })?,
    };
    let trigger = params.get("trigger").cloned().unwrap_or(Value::Null);
    let stored = resolved_trigger(ctx, &trigger, &target)?;
    let note = text(params, "note");
    let ttl = params
        .get("ttl_seconds")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_TTL_SECONDS);
    let kind = stored
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("")
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
            trigger: Value::Object(stored.clone()),
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
        outcome_of(&watch, WATCH_CREATED_EVENT)
    })?;
    let view = ctx.declared.read()?;
    Ok(json!({"watch": view_of(&view, &watch, now_of(&ctx.clock))?}))
}

fn cancel<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = validate_reason(text(params, "reason").as_str())?;
    let raw_id = params.get("id").and_then(Value::as_str).unwrap_or("");
    let watch = {
        let view = ctx.declared.read()?;
        view.watch(raw_id.trim())?.ok_or_else(|| {
            VogtError::NotFound(format!("no watch {}", crate::core::py_repr(raw_id)))
        })?
    };
    if ctx.principal.kind == crate::core::ActorKind::Agent && !may_cancel(ctx, &watch)? {
        return Err(VogtError::GrantRefused(format!(
            "only a person, the watch's owner or the session it wakes cancels a watch ({} is none of them)",
            ctx.principal.identity_ref
        )));
    }
    let mut write = write_of(ctx);
    let clock = Arc::clone(write.clock());
    let cancelled = audited_write(&mut write, "watch.cancel", &reason, |txn, _actor| {
        let current = txn.watch(&watch.id)?.ok_or_else(|| {
            VogtError::NotFound(format!("no watch {}", crate::core::py_repr(&watch.id)))
        })?;
        let state = current.effective_state(now_of(&clock));
        if state != "active" {
            return Err(VogtError::Conflict(format!(
                "watch {} is {state}, not active",
                current.id
            )));
        }
        let mut cancelled = current;
        cancelled.state = WatchState::Cancelled;
        txn.update_watch(&cancelled)?;
        outcome_of(&cancelled, WATCH_CANCELLED_EVENT)
    })?;
    let view = ctx.declared.read()?;
    Ok(json!({"watch": view_of(&view, &cancelled, now_of(&ctx.clock))?}))
}

/// The trigger as stored: slugs and refs resolved, unused fields dropped.
/// Key order is Python's insertion order.
fn resolved_trigger<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    trigger: &Value,
    target_session: &str,
) -> Result<Map<String, Value>, VogtError> {
    let kind = trigger.get("kind").and_then(Value::as_str).unwrap_or("");
    let mut stored = Map::new();
    stored.insert("kind".to_string(), json!(kind));
    let view = ctx.declared.read()?;
    if kind == "pr" || kind == "ci" {
        let slug = trigger.get("project").and_then(Value::as_str).unwrap_or("");
        let project = view.project_by_slug(slug)?.ok_or_else(|| {
            VogtError::NotFound(format!("no project {}", crate::core::py_repr(slug)))
        })?;
        stored.insert("project".to_string(), json!(project.slug));
        stored.insert("project_id".to_string(), json!(project.id));
    }
    match kind {
        "pr" => {
            stored.insert(
                "number".to_string(),
                trigger.get("number").cloned().unwrap_or(Value::Null),
            );
            stored.insert(
                "on".to_string(),
                Value::Array(sorted_unique_strings(trigger.get("on"))),
            );
        }
        "ci" => {
            stored.insert(
                "sha".to_string(),
                trigger.get("sha").cloned().unwrap_or(Value::Null),
            );
            stored.insert(
                "pr".to_string(),
                trigger.get("pr").cloned().unwrap_or(Value::Null),
            );
            stored.insert(
                "checks".to_string(),
                Value::Array(sorted_unique_trimmed(trigger.get("checks"))),
            );
        }
        "work_item" => {
            let reference = trigger
                .get("work_item")
                .and_then(Value::as_str)
                .unwrap_or("");
            let item = view.work_item_by_ref(reference)?.ok_or_else(|| {
                VogtError::NotFound(format!("no work item {}", crate::core::py_repr(reference)))
            })?;
            let mut states = sorted_unique_trimmed(trigger.get("states"));
            if states.is_empty() {
                states = TERMINAL_STATES.iter().map(|s| json!(s)).collect::<Vec<_>>();
                states.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
                states.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
            }
            stored.insert("ref".to_string(), json!(item.reference));
            stored.insert("work_item_id".to_string(), json!(item.id));
            stored.insert("states".to_string(), Value::Array(states));
        }
        "session" => {
            let session = trigger.get("session").and_then(Value::as_str).unwrap_or("");
            let watched = engine_id(&view, session)?;
            if watched == target_session {
                return Err(VogtError::InvalidRequest(
                    "a session cannot wait for itself to end".to_string(),
                ));
            }
            let until = trigger
                .get("until")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("ended");
            stored.insert("session".to_string(), json!(session));
            stored.insert("engine_session_id".to_string(), json!(watched));
            stored.insert("until".to_string(), json!(until));
        }
        _ => {}
    }
    Ok(stored)
}

fn may_cancel<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    watch: &Watch,
) -> Result<bool, VogtError> {
    let view = ctx.declared.read()?;
    if let Some(caller) = view.actor_by_identity(&ctx.principal.identity_ref)? {
        if caller.id == watch.owner_actor_id {
            return Ok(true);
        }
    }
    Ok(own_engine_session(ctx)?.as_deref() == Some(watch.target_session.as_str()))
}

fn outcome_of(watch: &Watch, event: &str) -> Result<WriteOutcome<Watch>, VogtError> {
    let outcome_state = watch
        .outcome
        .as_ref()
        .and_then(|outcome| outcome.get("state"))
        .cloned()
        .unwrap_or(Value::Null);
    let summary = json!({
        "watch_id": watch.id,
        "kind": watch.kind,
        "session": watch.target_session,
        "waits_for": describe(&watch.kind, &watch.trigger),
        "state": watch.state.to_string(),
        "outcome_state": outcome_state,
        "attempts": watch.attempts,
    });
    Ok(WriteOutcome::new(
        watch.clone(),
        "watch",
        &watch.id,
        watch_payload(watch),
        event,
        summary,
    ))
}

/// `Watch.model_dump(mode="json")`: declaration order, moments with `Z`.
fn watch_payload(watch: &Watch) -> Value {
    json!({
        "id": watch.id,
        "owner_actor_id": watch.owner_actor_id,
        "target_session": watch.target_session,
        "kind": watch.kind,
        "trigger": watch.trigger,
        "note": watch.note,
        "state": watch.state.to_string(),
        "created_at": watch.created_at.to_json(),
        "expires_at": watch.expires_at.to_json(),
        "fired_at": watch.fired_at.map(|moment| moment.to_json()),
        "outcome": watch.outcome,
        "attempts": watch.attempts,
        "last_error": watch.last_error,
        "reason": watch.reason,
    })
}

fn sorted_unique_strings(value: Option<&Value>) -> Vec<Value> {
    let mut texts: Vec<String> = match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    texts.sort();
    texts.dedup();
    texts.into_iter().map(Value::String).collect()
}

fn sorted_unique_trimmed(value: Option<&Value>) -> Vec<Value> {
    let mut texts: Vec<String> = match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    texts.sort();
    texts.dedup();
    texts.into_iter().map(Value::String).collect()
}

fn plus_seconds(moment: Moment, seconds: i64) -> Moment {
    Moment::from_unix(moment.unix_seconds() + seconds, moment.nanos())
}

fn text(params: &Value, name: &str) -> String {
    params
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Evaluate persisted watches once, caching forge evidence per project and kind.
pub fn evaluate_watches<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
) -> Result<(), VogtError> {
    use crate::storage::interface::ObservedStore;
    let now = now_of(&ctx.clock);
    let active = ctx
        .declared
        .read()?
        .list_watches(Some("active"), None, None, 500)?;
    let mut evidence =
        std::collections::HashMap::<(String, String), Vec<crate::core::Observation>>::new();
    for watch in active {
        if watch.expires_at <= now {
            settle(
                ctx,
                &watch,
                WatchState::Expired,
                "watch.expired",
                None,
                None,
                false,
            )?;
            continue;
        }
        if watch
            .outcome
            .as_ref()
            .and_then(|v| v["next_attempt_at"].as_str())
            .and_then(|s| crate::core::from_iso(s).ok())
            .is_some_and(|stamp| stamp > now)
        {
            continue;
        }
        let evaluated = (|| -> Result<Option<Value>, VogtError> {
            if let Some(outcome) = &watch.outcome {
                return Ok(Some(outcome.clone()));
            }
            let t = &watch.trigger;
            let mut observations =
                |kind: &str| -> Result<Vec<crate::core::Observation>, VogtError> {
                    let project = t["project_id"].as_str().unwrap_or("");
                    let key = (kind.to_owned(), project.to_owned());
                    if !evidence.contains_key(&key) {
                        evidence.insert(
                            key.clone(),
                            ctx.observed.latest(
                                &[kind.to_owned()],
                                Some(project),
                                false,
                                false,
                                2000,
                            )?,
                        );
                    }
                    Ok(evidence[&key].clone())
                };
            Ok(match watch.kind.as_str() {
                "work_item" => {
                    match ctx
                        .declared
                        .read()?
                        .work_item_by_id(t["work_item_id"].as_str().unwrap_or(""))?
                    {
                        None => Some(json!({"state":"gone"})),
                        Some(item) => t["states"]
                            .as_array()
                            .filter(|states| states.iter().any(|s| s == &item.state))
                            .map(|_| json!({"state":item.state})),
                    }
                }
                "session" => {
                    let Some(engine) = &ctx.engine else {
                        return Ok(None);
                    };
                    match engine.get_session(t["engine_session_id"].as_str().unwrap_or(""))? {
                        None => Some(json!({"state":"gone"})),
                        Some(s)
                            if !s.alive
                                || ["exited", "hibernated", "stopped"]
                                    .contains(&s.activity.as_str()) =>
                        {
                            Some(
                                json!({"state":if s.activity.is_empty() { "exited" } else { &s.activity }}),
                            )
                        }
                        Some(s)
                            if t["until"] == "done"
                                && s.done_at.as_ref().is_some_and(|s| !s.is_empty()) =>
                        {
                            Some(json!({"state":"done", "done_at":s.done_at}))
                        }
                        _ => None,
                    }
                }
                "pr" => observations("forge.pull_request")?
                    .iter()
                    .find(|p| p.payload["number"] == t["number"])
                    .and_then(|p| {
                        crate::core::watches::pr_outcome(&p.payload, &t["on"], &p.source_url)
                    }),
                "ci" => {
                    let revision = t["sha"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    let revision = match revision {
                        Some(r) => Some(r),
                        None => observations("forge.pull_request")?
                            .iter()
                            .find(|p| p.payload["number"] == t["pr"])
                            .and_then(|p| {
                                p.payload["head"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_owned)
                            }),
                    };
                    match revision {
                        Some(r) => crate::core::watches::ci_outcome(
                            &observations("ci.check")?,
                            &r,
                            t["checks"].as_array().map(Vec::as_slice).unwrap_or(&[]),
                        ),
                        None => None,
                    }
                }
                _ => None,
            })
        })();
        let mut outcome = match evaluated {
            Ok(Some(outcome)) => outcome,
            Ok(None) => continue,
            Err(error) => {
                tracing::info!(watch = %watch.id, %error, "watch not evaluated");
                continue;
            }
        };
        let error = if ctx.engine.is_none() {
            Some("no engine is configured".to_owned())
        } else {
            super::sessions::input(
                ctx,
                &super::sessions::InputParams {
                    id: watch.target_session.clone(),
                    text: Some(crate::core::watches::notice(
                        &watch.kind,
                        &describe(&watch.kind, &watch.trigger),
                        &outcome,
                        &watch.note,
                    )),
                    keys: None,
                    submit: true,
                    confirm: true,
                    wake_timeout_s: 30,
                    reason: format!("{FIRE_REASON}: {}", watch.id),
                },
            )
            .err()
            .map(|e| e.to_string().chars().take(500).collect())
        };
        let (state, event, fired) = if error.is_none() {
            outcome["delivered"] = json!(true);
            (WatchState::Fired, "watch.fired", true)
        } else if watch.attempts + 1 >= 5 {
            outcome["delivered"] = json!(false);
            (WatchState::Failed, "watch.failed", true)
        } else {
            outcome["next_attempt_at"] =
                json!(plus_seconds(now, 60 * 2_i64.pow(watch.attempts as u32)).to_iso());
            (WatchState::Active, "watch.failed", false)
        };
        settle(ctx, &watch, state, event, Some(outcome), error, fired)?;
    }
    Ok(())
}

const FIRE_REASON: &str = "watch fired: deliver its outcome (WI-2036)";
fn settle<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    watch: &Watch,
    state: WatchState,
    event: &str,
    outcome: Option<Value>,
    error: Option<String>,
    fired: bool,
) -> Result<(), VogtError> {
    let mut write = write_of(ctx);
    audited_write(&mut write, "watch.fire", FIRE_REASON, |txn, _actor| {
        let mut current = txn.watch(&watch.id)?.unwrap_or_else(|| watch.clone());
        if current.state != WatchState::Active {
            return outcome_of(&current, event);
        }
        current.state = state;
        if outcome.is_some() {
            current.attempts += 1;
            current.outcome = outcome;
        }
        current.last_error = error;
        if fired {
            current.fired_at = Some(now_of(&ctx.clock));
        }
        txn.update_watch(&current)?;
        outcome_of(&current, event)
    })
    .map(drop)
}

#[cfg(test)]
mod evaluation_tests {
    use super::*;
    use crate::application::context::build_context;
    use crate::core::{ActorKind, Principal, StepClock};

    #[test]
    fn persisted_watch_fires_and_missing_engine_retries_then_expires() {
        let dir =
            std::env::temp_dir().join(format!("vogt-watch-evaluation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::application::instance::init(&dir, &mut None, &mut None).unwrap();
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = Arc::clone(&writes);
        let engine = crate::adapters::engine::EngineClient::new(
            "http://engine",
            None,
            Some(Arc::new(move |path, _, _, method| {
                if method == "GET" && path.contains("watched") {
                    return (404, b"{}".to_vec());
                }
                if method == "GET" {
                    return (
                        200,
                        br#"{"id":"target","alive":true,"activity":"running","role":"worker"}"#
                            .to_vec(),
                    );
                }
                recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (200, b"{}".to_vec())
            })),
        );
        let config = crate::config::VogtConfig {
            data_dir: dir.clone(),
            ..Default::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("service:watch-test", ActorKind::Agent, "watch test").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap();
        crate::with_ctx!(&built, |ctx| {
            let made = create(ctx, &json!({"session":"target", "trigger":{"kind":"session", "session":"watched", "until":"done"}, "reason":"test", "ttl_seconds":3600})).unwrap();
            let id = made["watch"]["id"].as_str().unwrap();
            evaluate_watches(ctx).unwrap();
            let fired = ctx.declared.read().unwrap().watch(id).unwrap().unwrap();
            assert_eq!(fired.state, WatchState::Fired);
            assert_eq!(fired.outcome.unwrap()["delivered"], true);
            assert_eq!(fired.attempts, 1);
            let count = writes.load(std::sync::atomic::Ordering::SeqCst);
            assert!(count > 0);
            evaluate_watches(ctx).unwrap();
            assert_eq!(writes.load(std::sync::atomic::Ordering::SeqCst), count);
            // Seed a delivery retry, which must survive a fresh pass without
            // re-evaluating a now-changed trigger.
            let mut write = write_of(ctx);
            audited_write(&mut write, "watch.create", "test retry", |txn, actor| {
                let watch = Watch {
                    id: "wat_retry".into(),
                    owner_actor_id: actor.id.clone(),
                    target_session: "target".into(),
                    kind: "work_item".into(),
                    trigger: json!({"ref":"WI-1"}),
                    note: String::new(),
                    state: WatchState::Active,
                    created_at: Moment::from_unix(1_700_000_000, 0),
                    expires_at: Moment::from_unix(1_700_003_600, 0),
                    fired_at: None,
                    outcome: Some(json!({"state":"done"})),
                    attempts: 0,
                    last_error: None,
                    reason: "test".into(),
                };
                txn.insert_watch(&watch)?;
                outcome_of(&watch, WATCH_CREATED_EVENT)
            })
            .unwrap();
        });
        // Rebuild without an engine, as a restarted standalone core would.
        let absent = build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..Default::default()
            },
            Some(Principal::new("service:watch-test", ActorKind::Agent, "watch test").unwrap()),
            Some(StepClock::new(Moment::from_unix(1_700_000_100, 0))),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        crate::with_ctx!(&absent, |ctx| {
            evaluate_watches(ctx).unwrap();
            let retry = ctx
                .declared
                .read()
                .unwrap()
                .watch("wat_retry")
                .unwrap()
                .unwrap();
            assert_eq!(retry.state, WatchState::Active);
            assert_eq!(retry.attempts, 1);
            assert_eq!(retry.last_error.as_deref(), Some("no engine is configured"));
            evaluate_watches(ctx).unwrap();
            assert_eq!(
                ctx.declared
                    .read()
                    .unwrap()
                    .watch("wat_retry")
                    .unwrap()
                    .unwrap()
                    .attempts,
                1
            );
            let mut write = write_of(ctx);
            audited_write(&mut write, "watch.fire", "expire test", |txn, _| {
                let mut watch = txn.watch("wat_retry")?.unwrap();
                watch.expires_at = Moment::from_unix(1, 0);
                txn.update_watch(&watch)?;
                outcome_of(&watch, "watch.failed")
            })
            .unwrap();
            evaluate_watches(ctx).unwrap();
            assert_eq!(
                ctx.declared
                    .read()
                    .unwrap()
                    .watch("wat_retry")
                    .unwrap()
                    .unwrap()
                    .state,
                WatchState::Expired
            );
        });
        drop(absent);
        drop(built);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
