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
    Ok(json!({
        "chats": chats.clone().unwrap_or_default(),
        "available": chats.is_some(),
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
    detail_of(detail)
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
    let mut body = json!({"creator": ctx.principal.identity_ref, "wait_secs": wait_s});
    if let Some(message) = message.filter(|text| !text.is_empty()) {
        body["message"] = Value::String(message);
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
    let outcome = engine(ctx)?.chat_create(&body, wait_s)?;
    let Some(outcome) = outcome else {
        return Err(chats_off());
    };
    let chat = object_field(&outcome, "chat")?;
    let chat_id = string_field(&chat, "id")?;
    audit(
        ctx,
        "chat.create",
        &reason,
        &chat_id,
        &json!({"title": chat.get("title").cloned().unwrap_or(Value::Null)}),
        "chat.created",
    )?;
    Ok(outcome)
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
        return Err(unknown_chat(&chat_id));
    };
    let entries = outcome
        .get("entries")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    audit(
        ctx,
        "chat.send",
        &reason,
        &chat_id,
        &json!({"bytes": text.len(), "entries": entries}),
        "chat.message_sent",
    )?;
    Ok(outcome)
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
        return Err(unknown_chat(&chat_id));
    };
    let mut payload = json!({
        "approval_id": approval_id,
        "allow": allow,
        "status": outcome.get("status").cloned().unwrap_or(Value::Null),
    });
    if let Some(message) = message.filter(|text| !text.is_empty()) {
        payload["message"] = Value::String(message);
    }
    audit(
        ctx,
        "chat.decide",
        &reason,
        &chat_id,
        &payload,
        "chat.approval_decided",
    )?;
    Ok(outcome)
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
        return Err(unknown_chat(&chat_id));
    };
    audit(
        ctx,
        "chat.set_model",
        &reason,
        &chat_id,
        &json!({"model": outcome.get("model").cloned().unwrap_or(Value::Null)}),
        "chat.model_set",
    )?;
    Ok(outcome)
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
        return Err(unknown_chat(&chat_id));
    };
    audit(
        ctx,
        "chat.interrupt",
        &reason,
        &chat_id,
        &json!({"state": outcome.get("state").cloned().unwrap_or(Value::Null)}),
        "chat.interrupted",
    )?;
    Ok(outcome)
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
        return Err(chats_off());
    };
    audit(
        ctx,
        "chat.archive",
        &reason,
        &chat_id,
        &json!({"archived": outcome.get("archived").cloned().unwrap_or(Value::Bool(archived))}),
        "chat.archived",
    )?;
    Ok(outcome)
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
        return Err(unknown_chat(&chat_id));
    };
    let engine_session_id = outcome
        .get("engine_session_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    audit(
        ctx,
        "chat.promote",
        &reason,
        &chat_id,
        &json!({"engine_session_id": engine_session_id}),
        "chat.promoted",
    )?;
    Ok(outcome)
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
    let ok = chat_id.len() == 36
        && chat_id
            .bytes()
            .enumerate()
            .all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            });
    if ok {
        Ok(chat_id)
    } else {
        Err(VogtError::InvalidRequest(format!(
            "{} is not a chat id",
            py_repr(&chat_id)
        )))
    }
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

fn detail_of(payload: Option<Value>) -> Result<Value, VogtError> {
    let Some(payload) = payload else {
        return Err(chats_off());
    };
    let mut detail = Map::new();
    detail.insert("chat".to_string(), object_field(&payload, "chat")?);
    detail.insert(
        "entries".to_string(),
        payload
            .get("entries")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    );
    detail.insert(
        "approvals".to_string(),
        payload
            .get("approvals")
            .cloned()
            .unwrap_or(Value::Array(Vec::new())),
    );
    Ok(Value::Object(detail))
}

fn object_field(payload: &Value, field: &str) -> Result<Value, VogtError> {
    match payload.get(field) {
        Some(Value::Object(object)) => Ok(Value::Object(object.clone())),
        _ => Err(VogtError::EngineUnavailable(format!(
            "the session engine answered without a {field}"
        ))),
    }
}

fn string_field(payload: &Value, field: &str) -> Result<String, VogtError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            VogtError::EngineUnavailable(format!("the session engine answered without a {field}"))
        })
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

fn chats_off() -> VogtError {
    VogtError::NotFound("the session engine has chats turned off".to_string())
}

fn unknown_chat(chat_id: &str) -> VogtError {
    VogtError::NotFound(format!("no chat with id {}", py_repr(chat_id)))
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
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
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
                    json!({"chat": summary(), "entries": entries, "approvals": []})
                } else if path.ends_with("/messages") || path == "/api/chats" {
                    json!({"chat": summary(), "entries": entries, "finished": true})
                } else if path.ends_with("/promote") {
                    json!({"chat": summary(), "engine_session_id": "ses-1", "session_name": "Sydney"})
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
        assert_eq!(result["finished"], true);
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
}
