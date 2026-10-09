//! Per-actor preferences. Ports `application/services/preferences.py`.
//!
//! A preference is a small JSON value keyed per actor, kept under optimistic
//! concurrency. A read never auto-registers an actor: a principal who has never
//! written simply has no preferences.

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::application::services::{context_parts, dispatch, now_of, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{ActorPreference, Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};
use std::sync::Arc;

pub const PREFERENCE_SET_EVENT: &str = "preference.set";
pub const INBOX_FILTER_KEY: &str = "inbox.filter";

/// A preference is a setting, not a document. Sixteen kilobytes is past any
/// sane filter and short of a place to hide a payload.
const MAX_VALUE_BYTES: usize = 16 * 1024;

const KEY_PARTS: &str = r"^[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)*$";

pub fn preference_get_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, preference_get, params)
}

pub fn preference_set_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, preference_set, params)
}

fn preference_get<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let key = optional_string(&params, "key")?;
    let (declared, _, principal, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    let Some(actor) = view.actor_by_identity(&principal.identity_ref)? else {
        // A principal who has never written has no preferences, and a read does
        // not create the actor.
        return Ok(json!({"preferences": []}));
    };
    let mut rows = view.actor_preferences(&actor.id)?;
    if let Some(key) = key.as_deref() {
        rows.retain(|row| row.key == key);
    }
    Ok(json!({
        "preferences": rows.iter().map(preference_json).collect::<Vec<_>>(),
    }))
}

fn preference_set<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let key = require_string(&params, "key")?;
    validate_key(&key)?;
    let value = params.get("value").cloned().unwrap_or(Value::Null);
    let value = validate_value(&key, &value)?;
    let expected = match params.get("expected_version") {
        Some(Value::Number(number)) => number.as_i64(),
        _ => None,
    };
    let reason = require_string(&params, "reason")?;

    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let updated_at = now_of(&clock);
    let mut writing = write_context(declared, principal, Arc::clone(&clock), Arc::clone(&ids));
    let stored = set_preference(&mut writing, &key, &value, expected, &reason, updated_at)?;
    Ok(stored)
}

/// Split out so the closure is built against the concrete store. Against the
/// generic `DeclaredStore` bound the compiler treats it as needing a `'static`
/// clock and id factory, which is the same shape `contracts::record_check`
/// avoids.
fn set_preference<C: Clock + 'static, I: IdFactory + 'static>(
    writing: &mut crate::application::writes::WriteContext<
        '_,
        C,
        I,
        crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>,
    >,
    key: &str,
    value: &Value,
    expected: Option<i64>,
    reason: &str,
    updated_at: crate::core::Moment,
) -> Result<Value, VogtError> {
    let key = key.to_string();
    let value = canonical_value(&key, value)?;
    audited_write(writing, "preference.set", reason, |txn, actor| {
        let current = txn.actor_preference(&actor.id, &key)?;
        let current_version = current.as_ref().map(|row| row.version).unwrap_or(0);
        if let Some(expected) = expected {
            if expected != current_version {
                let message = format!(
                    "preference {} is at version {current_version}, not {expected}; re-read it and apply the change on top of the current value",
                    crate::core::py_repr(&key),
                );
                return Err(VogtError::PreferenceVersionConflict(message));
            }
        }
        let stored = ActorPreference {
            actor_id: actor.id.clone(),
            key: key.clone(),
            value: value.clone(),
            version: current_version + 1,
            updated_at,
        };
        let payload = preference_payload_json(&stored);
        txn.upsert_actor_preference(&stored)?;
        let summary = json!({
            "key": key,
            "version": stored.version,
            "cleared": stored.value == json!({}),
        });
        Ok(WriteOutcome::new(
            json!({"preference": preference_json(&stored)}),
            "preference",
            &format!("{actor_id}:{key}", actor_id = actor.id),
            payload,
            PREFERENCE_SET_EVENT,
            summary,
        ))
    })
}

/// The saved Inbox filter, or `None` when the principal has none — or one this
/// build no longer understands, which is read as "no filter" rather than an
/// error. A read must not fail because an old client saved a shape a new one
/// dropped.
pub fn saved_inbox_filter<C, I>(
    ctx: &AppContext<C, I>,
) -> Result<Option<InboxSavedFilter>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (declared, _, principal, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    let Some(actor) = view.actor_by_identity(&principal.identity_ref)? else {
        return Ok(None);
    };
    let Some(row) = view.actor_preference(&actor.id, INBOX_FILTER_KEY)? else {
        return Ok(None);
    };
    match serde_json::from_value::<InboxSavedFilter>(row.value.clone()) {
        Ok(filter) if filter != InboxSavedFilter::default() => Ok(Some(filter)),
        _ => Ok(None),
    }
}

/// `inbox.filter` is the one key with a known shape. Everything else is an
/// opaque object, which is what lets a new setting ship without a migration.
fn validate_value(key: &str, value: &Value) -> Result<Value, VogtError> {
    if !value.is_object() {
        return Err(VogtError::InvalidPreference(
            "a preference value must be a JSON object".to_string(),
        ));
    }
    let encoded = serde_json::to_vec(value).map_err(|error| {
        VogtError::InvalidPreference(format!("value is not JSON-serialisable: {error}"))
    })?;
    if encoded.len() > MAX_VALUE_BYTES {
        return Err(VogtError::InvalidPreference(format!(
            "preference value is {len} bytes; the limit is {MAX_VALUE_BYTES}",
            len = encoded.len(),
        )));
    }
    if key == INBOX_FILTER_KEY {
        // An empty object is always valid: it is how a filter is cleared.
        if value.as_object().is_some_and(|object| !object.is_empty()) {
            serde_json::from_value::<InboxSavedFilter>(value.clone()).map_err(|error| {
                VogtError::InvalidPreference(format!("invalid {key} value — {error}"))
            })?;
        }
    }
    Ok(value.clone())
}

fn validate_key(key: &str) -> Result<(), VogtError> {
    let pattern = regex::Regex::new(KEY_PARTS).expect("the key pattern is static");
    if pattern.is_match(key) {
        Ok(())
    } else {
        Err(VogtError::InvalidRequest(format!(
            "preference key {key:?} does not match {KEY_PARTS}"
        )))
    }
}

/// The Inbox filter a client can pin. Ports `InboxSavedFilter`, `extra="forbid"`.
/// Omitted fields render as their defaults, which is how a stored filter reads
/// back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct InboxSavedFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,
    #[serde(default = "default_actor")]
    pub actor: String,
    #[serde(default = "default_triage_states")]
    pub triage_states: Vec<String>,
}

fn default_actor() -> String {
    "any".to_string()
}

fn default_triage_states() -> Vec<String> {
    vec!["active".to_string()]
}

/// `inbox.filter` is stored as its model renders it, so omitted fields come back
/// as their defaults. An empty object clears the setting and is stored as given.
/// Any other key is stored verbatim.
fn canonical_value(key: &str, value: &Value) -> Result<Value, VogtError> {
    if key != INBOX_FILTER_KEY || value.as_object().is_some_and(|object| object.is_empty()) {
        return Ok(value.clone());
    }
    let parsed: InboxSavedFilter = serde_json::from_value(value.clone()).map_err(|error| {
        VogtError::InvalidPreference(format!("invalid {INBOX_FILTER_KEY} value — {error}"))
    })?;
    Ok(serde_json::to_value(parsed).expect("a parsed filter serialises"))
}

fn preference_json(row: &ActorPreference) -> Value {
    json!({
        "key": row.key,
        "value": row.value,
        "version": row.version,
        "updated_at": row.updated_at.to_json(),
    })
}

fn preference_payload_json(row: &ActorPreference) -> Value {
    json!({
        "actor_id": row.actor_id,
        "key": row.key,
        "value": row.value,
        "version": row.version,
        "updated_at": row.updated_at.to_json(),
    })
}

pub(super) fn require_string(params: &Value, field: &str) -> Result<String, VogtError> {
    match params.get(field).and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(VogtError::InvalidRequest(format!("{field} is required"))),
    }
}

pub(super) fn optional_string(params: &Value, field: &str) -> Result<Option<String>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}

pub(super) fn optional_i64(params: &Value, field: &str, default: i64) -> Result<i64, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| VogtError::InvalidRequest(format!("{field} must be an integer"))),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be an integer"
        ))),
    }
}
