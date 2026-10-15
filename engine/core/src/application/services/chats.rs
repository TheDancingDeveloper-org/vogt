//! Quick chats (WI-1097), reached through the session engine.
//!
//! Ports `src/vogt/application/services/chats.py`. A chat is a conversation the
//! engine keeps with one agent: its transcript, its live process and its
//! approvals all live there, and this layer relays. The declared store records
//! only the audit row, after the engine has answered, so a failed call leaves
//! no row.
//!
//! The message text is never the audit payload. A chat exists to carry
//! untrusted text, and the audit log is read by more people than the chat is.

use serde_json::{json, Map, Value};

use super::preferences::{optional_i64, optional_string, require_string};
use super::{context_parts, dispatch, write_context, Built};
use crate::application::context::AppContext;
use crate::application::services::sessions::answers_as_person;
use crate::application::writes::audited_action;
use crate::core::{py_repr, Clock, IdFactory};
use crate::errors::VogtError;

const CHAT: &str = "chat";
const DEFAULT_WAIT_S: i64 = 60;
const WAIT_MAX: i64 = 300;
const LIST_DEFAULT: i64 = 50;
const LIST_MAX: i64 = 500;
const TAIL_DEFAULT: i64 = 200;
const TAIL_MAX: i64 = 5000;
const TEXT_MAX: usize = 65_536;

pub fn list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, list, params)
}

pub fn get_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, get, params)
}

pub fn create_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, create, params)
}

pub fn send_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, send, params)
}

pub fn decide_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, decide, params)
}

pub fn set_model_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, set_model, params)
}

pub fn interrupt_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, interrupt, params)
}

pub fn archive_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, archive, params)
}

pub fn promote_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, promote, params)
}

fn list<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let q = optional_string(&params, "q")?;
    let archived = match params.get("archived") {
        None | Some(Value::Null) => "false".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "archived must be a string".to_string(),
            ))
        }
    };
    let limit = bounded(&params, "limit", LIST_DEFAULT, 1, LIST_MAX)?;
    let chats = engine(ctx)?.chat_list(q.as_deref(), &archived, limit)?;
    let Some(chats) = chats else {
        return Ok(json!({"chats": [], "available": false}));
    };
    Ok(json!({
        "chats": chats.into_iter().map(summary_view).collect::<Vec<_>>(),
        "available": true,
    }))
}

fn get<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let chat_id = chat_id_of(&params)?;
    let tail = bounded(&params, "tail", TAIL_DEFAULT, 1, TAIL_MAX)?;
    let detail = engine(ctx)?.chat_get(&chat_id, tail)?;
    let Some(detail) = detail else {
        return Err(missing_chat(&chat_id));
    };
    Ok(detail_view(&detail))
}

fn create<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = require_string(&params, "reason")?;
    let message = optional_string(&params, "message")?;
    let title = optional_string(&params, "title")?;
    let model = optional_string(&params, "model")?;
    let work_item = optional_string(&params, "work_item")?;
    let wait_s = bounded(&params, "wait_s", 0, 0, WAIT_MAX)?;
    // The engine's create returns at once (ChatCreateRequest has no wait).
    // Waiting for the first reply is a later read of the same turn.
    let mut body = json!({"creator": ctx.principal.identity_ref});
    if let Some(text) = message.clone().filter(|text| !text.is_empty()) {
        body["message"] = Value::String(text);
    }
    if let Some(title) = title.filter(|text| !text.is_empty()) {
        body["title"] = Value::String(title);
    }
    if let Some(model) = model.filter(|text| !text.is_empty()) {
        body["model"] = Value::String(model);
    }
    if let Some(work_item) = work_item.filter(|text| !text.is_empty()) {
        body["work_item"] = Value::String(work_item);
    }
    let client = engine(ctx)?;
    let outcome = client.chat_create(&body, 0)?;
    let Some(outcome) = outcome else {
        return Err(missing_chat("(new)"));
    };
    let mut result = send_view(&outcome);
    let message_bytes = message.as_deref().unwrap_or("").len();
    if message.as_deref().is_some_and(|text| !text.is_empty()) && wait_s > 0 {
        result = wait_for_turn(client, result, wait_s);
    }
    let chat = &result["chat"];
    let chat_id = chat
        .get("id")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            VogtError::EngineUnavailable(
                "the session engine answered without a chat id".to_string(),
            )
        })?
        .to_string();
    audit(
        ctx,
        "chat.create",
        &reason,
        &chat_id,
        &json!({
            "chat": chat_id,
            "model": chat.get("model").cloned().unwrap_or(Value::Null),
            "work_item": chat.get("work_item").cloned().unwrap_or(Value::Null),
            "message_bytes": message_bytes,
        }),
        "chat.created",
    )?;
    Ok(result)
}

fn send<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let chat_id = chat_id_of(&params)?;
    let text = require_string(&params, "text")?;
    if text.chars().count() > TEXT_MAX {
        return Err(VogtError::InvalidRequest(format!(
            "text is longer than {TEXT_MAX} characters"
        )));
    }
    let wait_s = bounded(&params, "wait_s", DEFAULT_WAIT_S, 0, WAIT_MAX)?;
    let reason = require_string(&params, "reason")?;
    let outcome = engine(ctx)?.chat_send(
        &chat_id,
        &json!({"text": text, "by": ctx.principal.identity_ref, "wait_secs": wait_s}),
    )?;
    let Some(outcome) = outcome else {
        return Err(missing_chat(&chat_id));
    };
    let result = send_view(&outcome);
    audit(
        ctx,
        "chat.send",
        &reason,
        &chat_id,
        &json!({
            "chat": chat_id,
            "bytes": text.len(),
            "finished": result.get("finished").cloned().unwrap_or(Value::Bool(false)),
        }),
        "chat.message_sent",
    )?;
    Ok(result)
}

fn decide<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let chat_id = chat_id_of(&params)?;
    let approval_id = require_string(&params, "approval_id")?;
    let allow = match params.get("allow") {
        Some(Value::Bool(allow)) => *allow,
        _ => return Err(VogtError::InvalidRequest("allow is required".to_string())),
    };
    let message = optional_string(&params, "message")?;
    let reason = require_string(&params, "reason")?;
    // Asked before the call: the engine answers the same question, and a
    // refusal must not be audited as a decision that did not happen.
    let person = answers_as_person(ctx)?;
    let outcome =
        engine(ctx)?.chat_decide(&chat_id, &approval_id, allow, message.as_deref(), person)?;
    let Some(outcome) = outcome else {
        return Err(missing_chat(&chat_id));
    };
    let approval = approval_view(&outcome);
    let summary = approval
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(300)
        .collect::<String>();
    audit(
        ctx,
        "chat.decide",
        &reason,
        &chat_id,
        &json!({
            "chat": chat_id,
            "approval": approval.get("id").cloned().unwrap_or(Value::Null),
            "tool": approval.get("tool_name").cloned().unwrap_or(Value::Null),
            "summary": summary,
            "allowed": allow,
            "person": person,
        }),
        "chat.approval_decided",
    )?;
    Ok(approval)
}

fn set_model<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let chat_id = chat_id_of(&params)?;
    let model = require_string(&params, "model")?;
    let reason = require_string(&params, "reason")?;
    let outcome = engine(ctx)?.chat_set_model(&chat_id, &model)?;
    let Some(outcome) = outcome else {
        return Err(missing_chat(&chat_id));
    };
    let view = summary_view(outcome);
    audit(
        ctx,
        "chat.set_model",
        &reason,
        &chat_id,
        &json!({
            "chat": chat_id,
            "model": view.get("model").cloned().unwrap_or(Value::Null),
        }),
        "chat.model_set",
    )?;
    Ok(view)
}

fn interrupt<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let chat_id = chat_id_of(&params)?;
    let reason = require_string(&params, "reason")?;
    let outcome = engine(ctx)?.chat_interrupt(&chat_id)?;
    let Some(outcome) = outcome else {
        return Err(missing_chat(&chat_id));
    };
    audit(
        ctx,
        "chat.interrupt",
        &reason,
        &chat_id,
        &json!({"chat": chat_id}),
        "chat.interrupted",
    )?;
    Ok(summary_view(outcome))
}

fn archive<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let chat_id = chat_id_of(&params)?;
    let archived = match params.get("archived") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(archived)) => *archived,
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "archived must be a boolean".to_string(),
            ))
        }
    };
    let reason = require_string(&params, "reason")?;
    let outcome = engine(ctx)?.chat_archive(&chat_id, archived)?;
    let Some(outcome) = outcome else {
        return Err(missing_chat(&chat_id));
    };
    audit(
        ctx,
        "chat.archive",
        &reason,
        &chat_id,
        &json!({"chat": chat_id, "archived": archived}),
        if archived {
            "chat.archived"
        } else {
            "chat.unarchived"
        },
    )?;
    Ok(summary_view(outcome))
}

fn promote<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let chat_id = chat_id_of(&params)?;
    let cwd = optional_string(&params, "cwd")?;
    let name = optional_string(&params, "name")?;
    let work_item = optional_string(&params, "work_item")?;
    let reason = require_string(&params, "reason")?;
    let mut body = json!({});
    if let Some(cwd) = cwd.filter(|text| !text.is_empty()) {
        body["cwd"] = Value::String(cwd);
    }
    if let Some(name) = name.filter(|text| !text.is_empty()) {
        body["name"] = Value::String(name);
    }
    if let Some(work_item) = work_item.filter(|text| !text.is_empty()) {
        body["work_item"] = Value::String(work_item);
    }
    let outcome = engine(ctx)?.chat_promote(&chat_id, &body)?;
    let Some(outcome) = outcome else {
        return Err(missing_chat(&chat_id));
    };
    // The contract is {chat, session}. session.id and session.name are the
    // fields Python returns; a top-level engine_session_id is not the reply.
    let session = match outcome.get("session") {
        Some(session) if session.is_object() => session,
        _ => &Value::Null,
    };
    let engine_session_id = session.get("id").and_then(Value::as_str).unwrap_or("");
    let session_name = session.get("name").and_then(Value::as_str).unwrap_or("");
    audit(
        ctx,
        "chat.promote",
        &reason,
        &chat_id,
        &json!({"chat": chat_id, "engine_session_id": engine_session_id}),
        "chat.promoted",
    )?;
    Ok(json!({
        "chat": summary_view(outcome.get("chat").cloned().unwrap_or(Value::Null)),
        "engine_session_id": engine_session_id,
        "session_name": session_name,
    }))
}

/// The engine, or the honest "not configured". A missing engine is not an empty
/// list of chats.
fn engine<C, I>(ctx: &AppContext<C, I>) -> Result<&crate::adapters::engine::EngineClient, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    ctx.engine.as_ref().ok_or_else(|| {
        VogtError::EngineUnavailable(
            "no session engine is configured (set VOGT_ENGINE_URL)".to_string(),
        )
    })
}

/// A chat id before it is interpolated into a URL path.
fn chat_id_of(params: &Value) -> Result<String, VogtError> {
    let chat_id = require_string(params, "id")?;
    // `uuid.UUID(raw.strip())`: surrounding whitespace and either hex case
    // are accepted, and the canonical form is lowercase with hyphens.
    // `uuid.UUID(raw.strip())` accepts surrounding whitespace, either hex
    // case and a missing hyphen set, and canonicalises to lowercase.
    let hex: String = chat_id
        .trim()
        .chars()
        .filter(|char| *char != '-')
        .flat_map(|char| char.to_lowercase())
        .collect();
    let ok = hex.len() == 32 && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !ok {
        return Err(VogtError::InvalidRequest(format!(
            "{} is not a chat id (a UUID, from chat.list or chat.create)",
            py_repr(&chat_id)
        )));
    }
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// An integer field inside `[low, high]`, or its default when absent.
fn bounded(
    params: &Value,
    field: &str,
    default: i64,
    low: i64,
    high: i64,
) -> Result<i64, VogtError> {
    let value = optional_i64(params, field, default)?;
    if (low..=high).contains(&value) {
        Ok(value)
    } else {
        Err(VogtError::InvalidRequest(format!(
            "{field} must be between {low} and {high}"
        )))
    }
}

fn audit<C, I>(
    ctx: &AppContext<C, I>,
    operation: &str,
    reason: &str,
    chat_id: &str,
    outcome: &Value,
    event_kind: &str,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let mut writing = write_context(declared, principal, clock, ids);
    audited_action(
        &mut writing,
        operation,
        reason,
        CHAT,
        chat_id,
        outcome,
        event_kind,
        None,
    )
}

fn missing_chat(chat_id: &str) -> VogtError {
    VogtError::NotFound(format!(
        "the engine has no chat {chat_id}, or has chats turned off (ENGINE_CHAT_ENABLED, or no Klaudia launch configured)"
    ))
}

/// Poll chat_get until the chat is idle or the wait runs out. The create
/// reply is what comes back when the engine never reports idle.
fn wait_for_turn(
    client: &crate::adapters::engine::EngineClient,
    result: Value,
    wait_s: i64,
) -> Value {
    let chat_id = result
        .pointer("/chat/id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let first_seq = result
        .get("entries")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("seq").and_then(Value::as_i64))
                .min()
        })
        .unwrap_or(1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_s as u64);
    while std::time::Instant::now() < deadline {
        let detail = match client.chat_get(&chat_id, 500) {
            Ok(Some(detail)) => detail,
            _ => break,
        };
        let summary = summary_view(detail.clone());
        // Absent state is not idle: the create reply carries no state,
        // and Python's view then has state None, which also keeps waiting.
        if summary.get("state").and_then(Value::as_str) == Some("idle") {
            let entries = detail
                .get("entries")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|row| {
                            row.get("seq").and_then(Value::as_i64).unwrap_or(0) >= first_seq
                        })
                        .cloned()
                        .map(entry_view)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            return json!({
                "chat": summary,
                "entries": entries,
                "finished": true,
            });
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(500)));
    }
    result
}

const SUMMARY_FIELDS: &[&str] = &[
    "id",
    "title",
    "driver",
    "model",
    "creator",
    "created_at",
    "updated_at",
    "archived",
    "work_item",
    "promoted_session",
    "state",
    "live",
    "message_count",
    "preview",
];
const ENTRY_FIELDS: &[&str] = &[
    "seq",
    "at",
    "kind",
    "text",
    "tool_name",
    "tool_use_id",
    "is_error",
    "retryable",
    "by",
];
const APPROVAL_FIELDS: &[&str] = &[
    "id",
    "tool_name",
    "summary",
    "source",
    "status",
    "requested_at",
    "expires_at",
    "decided_by",
];

fn project(payload: &Value, fields: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(object) = payload.as_object() else {
        return out;
    };
    for field in fields {
        if let Some(value) = object.get(*field).filter(|value| !value.is_null()) {
            out.insert((*field).to_string(), value.clone());
        }
    }
    out
}

fn summary_view(payload: Value) -> Value {
    let mut view = project(&payload, SUMMARY_FIELDS);
    view.entry("archived".to_string())
        .or_insert(Value::Bool(false));
    view.entry("message_count".to_string())
        .or_insert(Value::from(0));
    Value::Object(view)
}

fn entry_view(payload: Value) -> Value {
    let mut view = project(&payload, ENTRY_FIELDS);
    view.entry("is_error".to_string())
        .or_insert(Value::Bool(false));
    view.entry("retryable".to_string())
        .or_insert(Value::Bool(false));
    Value::Object(view)
}

fn approval_view(payload: &Value) -> Value {
    Value::Object(project(payload, APPROVAL_FIELDS))
}

/// ChatDetail flattens the summary beside `entries` and `approvals`.
fn detail_view(payload: &Value) -> Value {
    let entries = payload
        .get("entries")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().cloned().map(entry_view).collect::<Vec<_>>())
        .unwrap_or_default();
    let approvals = payload
        .get("approvals")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().map(approval_view).collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "chat": summary_view(payload.clone()),
        "entries": entries,
        "approvals": approvals,
    })
}

fn send_view(payload: &Value) -> Value {
    let entries = payload
        .get("entries")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().cloned().map(entry_view).collect::<Vec<_>>())
        .unwrap_or_default();
    json!({
        "chat": summary_view(payload.get("chat").cloned().unwrap_or(Value::Null)),
        "entries": entries,
        "finished": payload.get("finished").and_then(Value::as_bool).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use crate::adapters::engine::EngineClient;
    use crate::application::context::build_context;
    use crate::core::{ActorKind, Moment, Principal, StepClock};
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ReadView};
    use crate::with_ctx;

    const CHAT: &str = "0c0c0c0c-0000-4000-8000-000000000001";

    fn summary() -> serde_json::Value {
        json!({
            "id": CHAT,
            "title": "Sydney",
            "driver": "klaudia",
            "model": "grok-4.7",
            "creator": "human:ada",
            "created_at": "2026-10-08T00:00:00Z",
            "updated_at": "2026-10-08T00:00:00Z",
            "archived": false,
            "state": "idle",
            "live": true,
            "message_count": 2,
        })
    }

    /// An engine that records each request and answers the way the Python chat
    /// tests' stand-in does: a 404 while chats are off, a person-gate refusal,
    /// and otherwise the canned chat.
    #[allow(clippy::type_complexity)]
    fn context(
        name: &str,
        principal: Principal,
        chats_on: bool,
    ) -> (
        std::path::PathBuf,
        crate::application::context::Built,
        Arc<Mutex<Vec<(String, String, String)>>>,
    ) {
        let dir = std::env::temp_dir().join(format!("vogt-chat-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let seen = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
        let record = Arc::clone(&seen);
        let polls = Arc::new(Mutex::new(0_u32));
        let poll_count = Arc::clone(&polls);
        let engine = EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |url, _, body, method| {
                let rest = url.trim_start_matches("http://engine");
                let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
                record.lock().unwrap().push((
                    method.to_string(),
                    if query.is_empty() {
                        path.to_string()
                    } else {
                        format!("{path}?{query}")
                    },
                    String::from_utf8_lossy(body).to_string(),
                ));
                if !chats_on {
                    return (404, Vec::new());
                }
                if path.contains("/approvals/") {
                    let payload: serde_json::Value =
                        serde_json::from_slice(body).unwrap_or_default();
                    if payload.get("person") != Some(&serde_json::Value::Bool(true)) {
                        return (
                            403,
                            br#"{"error":"forbidden: person required: only a person answers"}"#
                                .to_vec(),
                        );
                    }
                    return (
                        200,
                        br#"{"id":"a1","tool_name":"Bash","summary":"make test","source":"gate","status":"allowed","requested_at":"2026-10-08T00:00:00Z","expires_at":"2026-10-08T00:10:00Z"}"#.to_vec(),
                    );
                }
                let entries = json!([
                    {"seq": 1, "at": "t", "kind": "user", "text": "is bedrock in sydney?"},
                    {"seq": 2, "at": "t", "kind": "assistant", "text": "yes"},
                ]);
                let answer = if method == "GET" && path == "/api/chats" {
                    json!([summary()])
                } else if method == "GET" {
                    // The first poll after create is still running; later ones idle.
                    let mut chat = summary();
                    let seen_polls = {
                        let mut count = poll_count.lock().unwrap();
                        let current = *count;
                        *count += 1;
                        current
                    };
                    if seen_polls == 0 {
                        chat["state"] = json!("running");
                    }
                    {
                        chat["entries"] = entries;
                        chat["approvals"] = json!([{
                            "id": "a-old",
                            "tool_name": "Bash",
                            "summary": "stale",
                            "source": "gate",
                            "status": "pending",
                            "requested_at": "t",
                            "expires_at": "t",
                            "extra": "dropped",
                        }]);
                        chat
                    }
                } else if path.ends_with("/messages") {
                    json!({"chat": summary(), "entries": entries, "finished": true})
                } else if path == "/api/chats" && method == "POST" {
                    let mut running = summary();
                    running["state"] = serde_json::json!("running");
                    serde_json::json!({"chat": running, "entries": entries})
                } else if path.ends_with("/promote") {
                    json!({
                        "chat": summary(),
                        "session": {"id": "ses-1", "name": "Sydney"},
                    })
                } else {
                    let mut merged = summary();
                    if let Ok(serde_json::Value::Object(payload)) =
                        serde_json::from_slice::<serde_json::Value>(body)
                    {
                        for (key, value) in payload {
                            merged[key] = value;
                        }
                    }
                    merged
                };
                (200, serde_json::to_vec(&answer).unwrap())
            })),
        );
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap();
        (dir, built, seen)
    }

    fn person() -> Principal {
        Principal::new("human:ada", ActorKind::Human, "Ada").unwrap()
    }

    fn agent() -> Principal {
        Principal::new("agent:session:ses_1", ActorKind::Agent, "agent").unwrap()
    }

    fn audited(built: &crate::application::context::Built, operation: &str) -> bool {
        with_ctx!(built, |ctx| {
            let view = ctx.declared.read().unwrap();
            view.list_audit(&crate::storage::interface::AuditQuery {
                limit: 50,
                offset: 0,
                actor_id: None,
                operation: None,
                entity_id: None,
                project_id: None,
                since: None,
                until: None,
            })
            .unwrap()
            .iter()
            .any(|row| row.operation == operation)
        })
    }

    #[test]
    fn create_relays_the_caller_and_is_audited() {
        let (dir, built, seen) = context("create", person(), true);
        let result = super::create_op(
            &built,
            json!({"message": "is bedrock in sydney?", "reason": "chat test"}),
        )
        .unwrap();
        assert_eq!(result["chat"]["id"], CHAT);
        let sent = seen.lock().unwrap();
        let body: serde_json::Value = serde_json::from_str(&sent[0].2).unwrap();
        assert_eq!(body["creator"], "human:ada");
        assert_eq!(body["message"], "is bedrock in sydney?");
        drop(sent);
        assert!(audited(&built, "chat.create"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn send_waits_and_audits_bytes_not_text() {
        let (dir, built, seen) = context("send", person(), true);
        let result = super::send_op(
            &built,
            json!({"id": CHAT, "text": "secret-ish", "reason": "chat test"}),
        )
        .unwrap();
        assert_eq!(result["finished"], true, "{result}");
        let sent = seen.lock().unwrap();
        let body: serde_json::Value = serde_json::from_str(&sent.last().unwrap().2).unwrap();
        assert_eq!(body["wait_secs"], 60);
        drop(sent);
        with_ctx!(&built, |ctx| {
            let view = ctx.declared.read().unwrap();
            let row = view
                .list_audit(&crate::storage::interface::AuditQuery {
                    limit: 50,
                    offset: 0,
                    actor_id: None,
                    operation: Some("chat.send".to_string()),
                    entity_id: None,
                    project_id: None,
                    since: None,
                    until: None,
                })
                .unwrap()
                .into_iter()
                .next()
                .unwrap();
            assert!(!format!("{row:?}").contains("secret-ish"));
        });
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_agent_cannot_answer_and_is_not_audited() {
        let (dir, built, seen) = context("agent", agent(), true);
        let error = super::decide_op(
            &built,
            json!({"id": CHAT, "approval_id": "a1", "allow": true, "reason": "chat test"}),
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::PersonRequired(_)), "{error:?}");
        let sent = seen.lock().unwrap();
        let body: serde_json::Value = serde_json::from_str(&sent.last().unwrap().2).unwrap();
        assert_eq!(body["person"], false);
        drop(sent);
        assert!(!audited(&built, "chat.decide"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_person_answers_and_is_audited() {
        let (dir, built, _) = context("person", person(), true);
        let result = super::decide_op(
            &built,
            json!({"id": CHAT, "approval_id": "a1", "allow": true, "reason": "chat test"}),
        )
        .unwrap();
        assert_eq!(result["status"], "allowed");
        assert!(audited(&built, "chat.decide"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_chat_id_is_a_uuid_before_it_becomes_a_path() {
        let (dir, built, _) = context("uuid", person(), true);
        let error = super::get_op(&built, json!({"id": "../sessions"})).unwrap_err();
        assert!(
            error.message().contains("not a chat id"),
            "{}",
            error.message()
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn chats_off_is_said_rather_than_an_empty_success() {
        let (dir, built, _) = context("off", person(), false);
        let listed = super::list_op(&built, json!({})).unwrap();
        assert_eq!(listed["available"], false);
        assert_eq!(listed["chats"], json!([]));
        let error =
            super::archive_op(&built, json!({"id": CHAT, "reason": "chat test"})).unwrap_err();
        assert!(matches!(error, VogtError::NotFound(_)), "{error:?}");
        assert!(error.message().contains("chats turned off"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn list_passes_the_search_through() {
        let (dir, built, seen) = context("search", person(), true);
        let listed = super::list_op(&built, json!({"q": "bedrock", "archived": "all"})).unwrap();
        assert_eq!(listed["available"], true);
        assert_eq!(listed["chats"][0]["title"], "Sydney");
        let sent = seen.lock().unwrap();
        let path = sent.last().unwrap().1.clone();
        drop(sent);
        assert!(path.contains("q=bedrock"), "{path}");
        assert!(path.contains("archived=all"), "{path}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_engine_is_unavailable() {
        let dir = std::env::temp_dir().join(format!("vogt-chat-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(person()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let error = super::list_op(&built, json!({})).unwrap_err();
        assert!(
            matches!(error, VogtError::EngineUnavailable(_)),
            "{error:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn event_summary(built: &crate::application::context::Built) -> String {
        let mut found = String::new();
        with_ctx!(built, |ctx| {
            let view = ctx.declared.read().unwrap();
            let events = view.list_events(0, 20, None).unwrap();
            found.push_str(
                &events
                    .iter()
                    .map(|event| format!("{} {}", event.kind, event.summary))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        });
        found
    }

    #[test]
    fn promote_reads_the_session_object() {
        let (dir, built, _) = context("promote", person(), true);
        let result = super::promote_op(
            &built,
            json!({"id": CHAT, "reason": "chat test", "name": "Sydney"}),
        )
        .unwrap();
        assert_eq!(result["engine_session_id"], "ses-1");
        assert_eq!(result["session_name"], "Sydney");
        assert_eq!(result["chat"]["id"], CHAT);
        let row = event_summary(&built);
        assert!(row.contains("chat.promoted"), "{row}");
        assert!(row.contains("ses-1"), "{row}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn create_waits_by_polling_until_idle() {
        let (dir, built, seen) = context("wait", person(), true);
        let result = super::create_op(
            &built,
            json!({
                "message": "is bedrock in sydney?",
                "wait_s": 2,
                "reason": "chat test",
                "title": "kept out of the audit",
            }),
        )
        .unwrap();
        let sent = seen.lock().unwrap().clone();
        assert_eq!(result["finished"], true, "result={result} sent={sent:?}");
        assert_eq!(result["chat"]["state"], "idle");
        drop(seen);
        let create = sent.iter().find(|row| row.0 == "POST").unwrap();
        let body: serde_json::Value = serde_json::from_str(&create.2).unwrap();
        assert!(body.get("wait_secs").is_none(), "{body}");
        assert!(sent.iter().any(|row| row.0 == "GET"), "{sent:?}");
        let row = event_summary(&built);
        assert!(row.contains("chat.created"), "{row}");
        assert!(!row.contains("kept out"), "{row}");
        assert!(row.contains("message_bytes"), "{row}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn set_model_interrupt_and_unarchive() {
        let (dir, built, _) = context("more", person(), true);
        let model = super::set_model_op(
            &built,
            json!({"id": CHAT, "model": "grok-4.7", "reason": "chat test"}),
        )
        .unwrap();
        assert_eq!(model["model"], "grok-4.7");
        assert!(event_summary(&built).contains("grok-4.7"));
        let stopped =
            super::interrupt_op(&built, json!({"id": CHAT, "reason": "chat test"})).unwrap();
        assert_eq!(stopped["id"], CHAT);
        let back = super::archive_op(
            &built,
            json!({"id": CHAT, "archived": false, "reason": "chat test"}),
        )
        .unwrap();
        assert_eq!(back["archived"], false);
        let row = event_summary(&built);
        assert!(row.contains("chat.unarchived"), "{row}");
        assert!(!row.contains("chat.archived"), "{row}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_chat_names_both_causes_and_a_uuid_may_have_space() {
        let (dir, built, _) = context("missing", person(), false);
        let error = super::get_op(&built, json!({"id": format!(" {CHAT} ")})).unwrap_err();
        assert!(
            error.message().contains("chats turned off"),
            "{}",
            error.message()
        );
        assert!(error.message().contains(CHAT), "{}", error.message());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_decision_audit_keeps_the_denial_message_out() {
        let (dir, built, _) = context("deny", person(), true);
        let result = super::decide_op(
            &built,
            json!({
                "id": CHAT,
                "approval_id": "a1",
                "allow": false,
                "message": "not this time",
                "reason": "chat test",
            }),
        )
        .unwrap();
        assert_eq!(result["tool_name"], "Bash");
        assert!(result.get("extra").is_none());
        let row = event_summary(&built);
        assert!(row.contains("chat.approval_decided"), "{row}");
        assert!(!row.contains("not this time"), "{row}");
        assert!(row.contains("make test"), "{row}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
