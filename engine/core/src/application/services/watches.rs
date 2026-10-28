//! Watches a session arms so it can sleep until an event (WI-2036). Ports the
//! read half of `application/services/watches.py`.
//!
//! `watch.list` is served. Creating, cancelling and evaluating watches stay
//! unported: those write and they need the engine.

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
        "created_at": crate::core::to_iso(watch.created_at),
        "expires_at": crate::core::to_iso(watch.expires_at),
        "fired_at": watch.fired_at.map(crate::core::to_iso),
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
