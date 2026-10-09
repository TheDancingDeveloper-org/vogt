//! Coding sessions — the work opening a terminal on its own project (WI-1073,
//! sessions part I).
//!
//! Ports the first half of `src/vogt/application/services/sessions.py`: start,
//! stop, list, sweep, log tail, input, screen, wait, wake, keep awake,
//! hibernate, set role and answer, plus the shared helpers both halves use.
//! Sessions part II (history, search, replies, bind, blocked reports, rename,
//! remove, grants, wait any) lands in the same module.
//!
//! The declared half is the Rust `CodingSession`: an engine id, a project, an
//! optional work item, the actor it runs as, its cwd, template, model, effort
//! and reason. The live half — name, activity, approval, role, hibernation —
//! belongs to the engine and is read, never stored.
//!
//! The terminal starts before the declared write. The engine and SQLite cannot
//! share a transaction, so the ordering is: start the terminal, then record it;
//! if the write fails, kill what was started and raise the original failure.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::preferences::{optional_i64, optional_string, require_string};
use super::{dispatch, Built};

use crate::adapters::engine::{
    CreateSession, EngineApproval, EngineBlocked, EngineClient, EngineHibernation, EngineResources,
    EngineScreen, EngineSession, EngineSweepEntry,
};
use crate::adapters::transcripts;
use crate::application::brief;
use crate::application::context::{write_of, AppContext};
use crate::application::resolve;
use crate::application::writes::{self, audited_action, audited_write, WriteOutcome};
use crate::auth::{self, TOKEN_ENTROPY_BYTES};
use crate::core::{py_repr, Actor, ActorKind, CodingSession, Moment, Token, TokenKind, WorkItem};
use crate::core::{Clock, IdFactory};
use crate::decisions::{self, Attention};
use crate::delivery::{self};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const SESSION_START: &str = "session.start";
const SESSION_STOP: &str = "session.stop";
const SESSION_INPUT: &str = "session.input";
const SESSION_ANSWER: &str = "session.answer";
const SESSION_HIBERNATE: &str = "session.hibernate";
const SESSION_WAKE: &str = "session.wake";
const SESSION_KEEP_AWAKE: &str = "session.keep_awake";
const SESSION_SET_ROLE: &str = "session.set_role";

/// The longest `session.input` text the engine accepts in one write.
const SESSION_INPUT_MAX_BYTES: usize = 64 * 1024;

/// How long `session.input` watches for what became of a submitted input: up to
/// this many screen reads, this far apart.
const CONFIRM_READS: usize = 10;
const CONFIRM_INTERVAL: Duration = Duration::from_millis(200);

/// The longest a session's own log tail may be. The engine caps it the same way.
const LOG_TAIL_MAX_BYTES: i64 = 262_144;

/// The keys `session.input` accepts.
const INPUT_KEYS: [&str; 10] = [
    "enter",
    "esc",
    "tab",
    "up",
    "down",
    "left",
    "right",
    "ctrl-c",
    "ctrl-d",
    "backspace",
];

// -- the registry's entry points ----------------------------------------------
//
// One wrapper per operation: parse the transport's JSON into the parameter
// model (defaults matching the Python models), then the service. The table in
// `registry::service_for` is the only place that names them.

pub fn start_session_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, start_session_json, params)
}
pub fn stop_session_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, stop_session_json, params)
}
pub fn list_sessions_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, list_sessions_json, params)
}
pub fn sweep_sessions_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, sweep_sessions_json, params)
}
pub fn log_tail_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, log_tail_json, params)
}
pub fn input_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, input_json, params)
}
pub fn screen_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, screen_json, params)
}
pub fn wait_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, wait_json, params)
}
pub fn wake_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, wake_json, params)
}
pub fn keep_awake_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, keep_awake_json, params)
}
pub fn set_role_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, set_role_json, params)
}
pub fn answer_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, answer_json, params)
}
pub fn hibernate_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, hibernate_json, params)
}

fn start_session_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = StartSessionParams {
        project: optional_string(&params, "project")?,
        work_item: optional_string(&params, "work_item")?,
        name: optional_string(&params, "name")?,
        template: optional_string(&params, "template")?,
        task: optional_string(&params, "task")?,
        reason: require_string(&params, "reason")?,
        model: optional_string(&params, "model")?,
        effort: optional_string(&params, "effort")?,
        resume: optional_string(&params, "resume")?,
        permission_mode: optional_string(&params, "permission_mode")?
            .unwrap_or_else(|| "default".to_string()),
        autopilot: optional_bool(&params, "autopilot")?,
        role: optional_string(&params, "role")?.unwrap_or_else(|| "worker".to_string()),
    };
    Ok(serde_json::to_value(start_session(ctx, &parsed)?).expect("a session result serialises"))
}

fn stop_session_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = StopSessionParams {
        id: require_string(&params, "id")?,
        reason: require_string(&params, "reason")?,
    };
    Ok(serde_json::to_value(stop_session(ctx, &parsed)?).expect("a session result serialises"))
}

fn list_sessions_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = ListSessionsParams {
        project: optional_string(&params, "project")?,
        work_item: optional_string(&params, "work_item")?,
        include_stopped: optional_bool(&params, "include_stopped")?.unwrap_or(false),
        limit: optional_i64(&params, "limit", 50)?,
        offset: optional_i64(&params, "offset", 0)?,
        order: optional_string(&params, "order")?.unwrap_or_else(|| "started".to_string()),
    };
    Ok(serde_json::to_value(list_sessions(ctx, &parsed)?).expect("a session result serialises"))
}

fn sweep_sessions_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = SweepSessionsParams {
        project: optional_string(&params, "project")?,
        screen_lines: optional_i64(&params, "screen_lines", 8)?,
        stall_after_minutes: optional_i64(&params, "stall_after_minutes", 10)?,
    };
    Ok(serde_json::to_value(sweep_sessions(ctx, &parsed)?).expect("a session result serialises"))
}

fn log_tail_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = LogTailParams {
        id: require_string(&params, "id")?,
        tail_bytes: optional_i64(&params, "tail_bytes", 64 * 1024)?,
        strip_ansi: optional_bool(&params, "strip_ansi")?.unwrap_or(true),
    };
    Ok(serde_json::to_value(log_tail(ctx, &parsed)?).expect("a session result serialises"))
}

fn input_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = InputParams {
        id: require_string(&params, "id")?,
        text: optional_string(&params, "text")?,
        keys: optional_string_list(&params, "keys")?,
        submit: optional_bool(&params, "submit")?.unwrap_or(false),
        reason: require_string(&params, "reason")?,
        confirm: optional_bool(&params, "confirm")?.unwrap_or(true),
        wake_timeout_s: optional_i64(&params, "wake_timeout_s", 120)?,
    };
    Ok(serde_json::to_value(input(ctx, &parsed)?).expect("a session result serialises"))
}

fn screen_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = ScreenParams {
        id: require_string(&params, "id")?,
        scrollback_lines: optional_i64(&params, "scrollback_lines", 0)?,
    };
    Ok(serde_json::to_value(screen(ctx, &parsed)?).expect("a session result serialises"))
}

fn wait_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = WaitParams {
        id: require_string(&params, "id")?,
        until: optional_string(&params, "until")?.unwrap_or_else(|| "ready".to_string()),
        timeout_s: optional_i64(&params, "timeout_s", 120)?,
    };
    Ok(serde_json::to_value(wait(ctx, &parsed)?).expect("a session result serialises"))
}

fn wake_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = WakeParams {
        id: require_string(&params, "id")?,
        reason: require_string(&params, "reason")?,
    };
    Ok(serde_json::to_value(wake(ctx, &parsed)?).expect("a session result serialises"))
}

fn keep_awake_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = KeepAwakeParams {
        id: require_string(&params, "id")?,
        keep_awake: require_bool(&params, "keep_awake")?,
        reason: require_string(&params, "reason")?,
    };
    Ok(serde_json::to_value(keep_awake(ctx, &parsed)?).expect("a session result serialises"))
}

fn set_role_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = SetRoleParams {
        id: require_string(&params, "id")?,
        role: require_string(&params, "role")?,
        reason: require_string(&params, "reason")?,
    };
    Ok(serde_json::to_value(set_role(ctx, &parsed)?).expect("a session result serialises"))
}

fn answer_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = AnswerParams {
        id: require_string(&params, "id")?,
        option: optional_i64_some(&params, "option")?,
        label: optional_string(&params, "label")?,
        expect_question: optional_string(&params, "expect_question")?,
        reason: require_string(&params, "reason")?,
    };
    answer(ctx, &parsed)
}

fn hibernate_json<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let parsed = HibernateParams {
        id: require_string(&params, "id")?,
        reason: require_string(&params, "reason")?,
        allow_shell: optional_bool(&params, "allow_shell")?.unwrap_or(false),
    };
    Ok(serde_json::to_value(hibernate(ctx, &parsed)?).expect("a session result serialises"))
}

/// A bool that may be absent.
fn optional_bool(params: &Value, field: &str) -> Result<Option<bool>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a boolean"
        ))),
    }
}

/// A bool the caller must give.
fn require_bool(params: &Value, field: &str) -> Result<bool, VogtError> {
    optional_bool(params, field)?
        .ok_or_else(|| VogtError::InvalidRequest(format!("{field} is required")))
}

/// An integer that may be absent, unlike `optional_i64` which fills a default.
fn optional_i64_some(params: &Value, field: &str) -> Result<Option<i64>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| VogtError::InvalidRequest(format!("{field} must be an integer"))),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be an integer"
        ))),
    }
}

/// A list of strings that may be absent.
fn optional_string_list(params: &Value, field: &str) -> Result<Option<Vec<String>>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_string).ok_or_else(|| {
                    VogtError::InvalidRequest(format!("{field} must be a list of strings"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a list of strings"
        ))),
    }
}

// -- parameter models ---------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StartSessionParams {
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub name: Option<String>,
    pub template: Option<String>,
    pub task: Option<String>,
    pub reason: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub resume: Option<String>,
    pub permission_mode: String,
    pub autopilot: Option<bool>,
    pub role: String,
}

#[derive(Debug, Clone)]
pub struct StopSessionParams {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ListSessionsParams {
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub include_stopped: bool,
    pub limit: i64,
    pub offset: i64,
    pub order: String,
}

#[derive(Debug, Clone)]
pub struct SweepSessionsParams {
    pub project: Option<String>,
    pub screen_lines: i64,
    pub stall_after_minutes: i64,
}

#[derive(Debug, Clone)]
pub struct LogTailParams {
    pub id: String,
    pub tail_bytes: i64,
    pub strip_ansi: bool,
}

#[derive(Debug, Clone)]
pub struct InputParams {
    pub id: String,
    pub text: Option<String>,
    pub keys: Option<Vec<String>>,
    pub submit: bool,
    pub reason: String,
    pub confirm: bool,
    pub wake_timeout_s: i64,
}

#[derive(Debug, Clone)]
pub struct ScreenParams {
    pub id: String,
    pub scrollback_lines: i64,
}

#[derive(Debug, Clone)]
pub struct WaitParams {
    pub id: String,
    pub until: String,
    pub timeout_s: i64,
}

#[derive(Debug, Clone)]
pub struct WakeParams {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct KeepAwakeParams {
    pub id: String,
    pub keep_awake: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct SetRoleParams {
    pub id: String,
    pub role: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct AnswerParams {
    pub id: String,
    pub option: Option<i64>,
    pub label: Option<String>,
    pub expect_question: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct HibernateParams {
    pub id: String,
    pub reason: String,
    pub allow_shell: bool,
}

// -- result models, field order matching application/models.py ---------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionApprovalOption {
    pub number: i64,
    pub label: String,
    pub selected: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionApproval {
    pub question: String,
    pub command_excerpt: String,
    pub detected_at: String,
    pub deadline_seconds: Option<i64>,
    pub deadline_at: Option<String>,
    pub kind: String,
    pub options: Vec<SessionApprovalOption>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionBlocked {
    pub reason: String,
    pub items: Vec<String>,
    pub since: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionResources {
    pub rss_bytes: i64,
    pub cpu_pct: f64,
    pub processes: i64,
    pub sampled_at: String,
    pub over_threshold: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionHibernation {
    pub at: String,
    pub trigger: String,
    pub resumable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionRuntime {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub model_basis: Option<String>,
    pub effort: Option<String>,
    pub effort_basis: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub engine_session_id: String,
    pub name: String,
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub state: String,
    pub activity: Option<String>,
    pub alive: Option<bool>,
    pub exit_code: Option<i64>,
    pub cwd: String,
    pub started_at: Moment,
    pub stopped_at: Option<Moment>,
    pub stop_reason: Option<String>,
    pub activity_changed_at: Option<String>,
    pub created_at: Option<String>,
    pub turn_started_at: Option<String>,
    pub last_output_at: Option<String>,
    pub approval: Option<SessionApproval>,
    pub command: Option<String>,
    pub blocked: Option<SessionBlocked>,
    pub conversation_agent: Option<String>,
    pub hibernation: Option<SessionHibernation>,
    pub keep_awake: bool,
    pub autopilot: bool,
    pub autopilot_nudges: i64,
    pub role: String,
    pub resources: Option<SessionResources>,
    pub template: Option<String>,
    pub permission_mode: Option<String>,
    pub stopped_by: Option<String>,
    pub runtime: SessionRuntime,
    pub conversation_id: Option<String>,
    pub last_reply_excerpt: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionResult {
    pub session: SessionSummary,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
    pub next_offset: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSweepRow {
    pub session: SessionSummary,
    pub attention: String,
    pub reason: String,
    pub needs_you: bool,
    pub screen_tail: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSweepResult {
    pub rows: Vec<SessionSweepRow>,
    pub total: i64,
    pub needs_you: i64,
    pub engine_available: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogTailResult {
    pub session_id: String,
    pub text: String,
    pub bytes: i64,
    pub total_bytes: i64,
    pub truncated: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionScreenResult {
    pub id: String,
    pub cols: i64,
    pub rows: i64,
    pub lines: Vec<String>,
    pub cursor_row: Option<i64>,
    pub cursor_col: Option<i64>,
    pub title: Option<String>,
    pub activity: Option<String>,
    pub alive: Option<bool>,
    pub ready: Option<bool>,
    pub scrollback: Vec<String>,
    pub turn_started_at: Option<String>,
    pub last_output_at: Option<String>,
    pub approval: Option<SessionApproval>,
    pub blocked: Option<SessionBlocked>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionWaitResult {
    pub outcome: String,
    pub matched: bool,
    pub waited_ms: i64,
    pub screen: SessionScreenResult,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInputResult {
    pub id: String,
    pub engine_session_id: String,
    pub linked: bool,
    pub bytes: i64,
    pub keys: Vec<String>,
    pub submitted: bool,
    pub delivery: String,
    pub delivery_evidence: String,
    pub woke: bool,
}

// -- the operations -----------------------------------------------------------

/// Open a terminal for a work item or a project.
pub fn start_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    if params.permission_mode == "bypass" && ctx.principal.kind == ActorKind::Agent {
        return Err(VogtError::BypassRefused(format!(
            "permission_mode=bypass can only be granted by a person, not by an agent ({}); \
             start the session from the GUI or the CLI, or use the default posture and report \
             the denied action as blocked",
            ctx.principal.identity_ref
        )));
    }
    let engine = engine_of(ctx)?;
    let mut writing = write_of(ctx);
    let session_id = fresh(&writing, "ses");
    let subject = subject_of(ctx, params, &session_id)?;
    let actor_ref = format!("agent:session:{session_id}");
    let scopes = session_scopes(ctx)?;
    let (secret, token_hash) = minted_token()?;
    let now = now(&writing);
    let actor = Actor {
        id: fresh(&writing, "act"),
        kind: ActorKind::Agent,
        display_name: format!("Session {session_id}"),
        identity_ref: actor_ref.clone(),
        disabled: false,
        created_at: now,
    };
    let token = Token {
        id: fresh(&writing, "tok"),
        actor_id: actor.id.clone(),
        actor_identity_ref: Some(actor_ref.clone()),
        name: format!("session {session_id}"),
        scopes: scopes.clone(),
        kind: TokenKind::Agent,
        created_at: now,
        expires_at: None,
        last_used_at: None,
        revoked_at: None,
        revoked_reason: None,
    };
    let spec = build_spec(ctx, params, &subject, &secret, &actor_ref, &session_id)?;
    let started = engine.create_session(&spec)?;
    let recorded = CodingSession {
        id: session_id.clone(),
        engine_session_id: started.id.clone(),
        project_id: subject.project_id.clone(),
        work_item_id: subject.work_item_id.clone(),
        actor_id: actor.id.clone(),
        cwd: subject.cwd.clone(),
        template: params.template.clone(),
        model: params.model.clone(),
        effort: params.effort.clone(),
        reason: reason.clone(),
        started_at: now,
        stopped_at: None,
    };
    let stored = {
        let actor = actor.clone();
        let token = token.clone();
        let token_hash = token_hash.clone();
        let recorded = recorded.clone();
        audited_write(&mut writing, SESSION_START, &reason, move |txn, _actor| {
            txn.insert_actor(&actor)?;
            txn.insert_token(&token, &token_hash)?;
            txn.insert_session(&recorded)?;
            let payload = serde_json::to_value(&recorded).unwrap_or(Value::Null);
            Ok(WriteOutcome::new(
                recorded.id.clone(),
                "session",
                &recorded.id,
                payload,
                "session.started",
                json!({"engine_session_id": recorded.engine_session_id}),
            ))
        })
    };
    if let Err(error) = stored {
        // The terminal is already running and Vogt holds no record of it.
        // Stop it so a failed write never leaves an orphan process.
        let _ = engine.kill_session(
            &started.id,
            Some(&reason),
            Some(&ctx.principal.identity_ref),
        );
        return Err(error);
    }
    Ok(SessionResult {
        session: summarize(ctx, &recorded, Some(&started))?,
    })
}

/// Stop a session and revoke the token it ran with.
pub fn stop_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &StopSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let Some(session) = found.session else {
        return stop_unlinked(ctx, engine, &found.engine_session_id, &reason);
    };
    if session.stopped_at.is_some() {
        return Err(VogtError::Conflict(format!(
            "session {} is already stopped",
            py_repr(&session.id)
        )));
    }
    let _ = engine.kill_session(
        &session.engine_session_id,
        Some(&reason),
        Some(&ctx.principal.identity_ref),
    );
    let mut writing = write_of(ctx);
    let stopped_at = now(&writing);
    let actor_id = session.actor_id.clone();
    let session_id = session.id.clone();
    let stop_reason = reason.clone();
    audited_write(&mut writing, SESSION_STOP, &reason, move |txn, _actor| {
        txn.mark_session_stopped(&session_id, stopped_at)?;
        for token in txn.tokens_for_actor(&actor_id, false)? {
            if token.revoked_at.is_none() {
                txn.revoke_token(&token.id, &stop_reason, stopped_at)?;
            }
        }
        Ok(WriteOutcome::new(
            session_id.clone(),
            "session",
            &session_id,
            json!({"stopped_at": stopped_at.to_iso()}),
            "session.stopped",
            json!({}),
        ))
    })?;
    let stopped = ctx
        .declared
        .read()?
        .session_by_id(&session.id)?
        .unwrap_or(session);
    Ok(SessionResult {
        session: summarize(ctx, &stopped, None)?,
    })
}

/// List sessions, live activity merged in. The engine being down is a fact
/// about the rows, not a failure of the list.
pub fn list_sessions<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
) -> Result<SessionListResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let project = match &params.project {
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?),
        None => None,
    };
    let work_item = match &params.work_item {
        Some(reference) => Some(resolve::work_item(&ctx.declared.read()?, reference)?),
        None => None,
    };
    let live = live_sessions(ctx.engine.as_ref());
    let recorded = ctx.declared.read()?.list_sessions(
        project.as_ref().map(|project| project.id.as_str()),
        work_item.as_ref().map(|item| item.id.as_str()),
        params.include_stopped,
        i64::MAX,
        0,
    )?;
    let mut rows = recorded;
    if params.order == "rss" {
        rows.sort_by(|left, right| {
            rss_of(live.get(&left.engine_session_id))
                .cmp(&rss_of(live.get(&right.engine_session_id)))
                .reverse()
                .then_with(|| right.started_at.cmp(&left.started_at))
        });
    }
    let total = rows.len() as i64;
    let offset = params.offset.max(0) as usize;
    let page: Vec<CodingSession> = rows
        .into_iter()
        .skip(offset)
        .take(params.limit.max(0) as usize)
        .collect();
    let mut sessions = Vec::with_capacity(page.len());
    for session in &page {
        sessions.push(summarize(
            ctx,
            session,
            live.get(&session.engine_session_id),
        )?);
    }
    let all_recorded = ctx
        .declared
        .read()?
        .list_sessions(None, None, true, i64::MAX, 0)?;
    let unlinked = unlinked_rows(&live, &all_recorded, params);
    let unlinked_total = unlinked.len() as i64;
    let room = (params.limit - sessions.len() as i64).max(0) as usize;
    sessions.extend(unlinked.into_iter().take(room));
    Ok(SessionListResult {
        total: total + unlinked_total,
        limit: params.limit,
        offset: params.offset,
        next_offset: (params.offset + page_len(sessions.len(), unlinked_total)
            < total + unlinked_total)
            .then_some(params.offset + params.limit),
        sessions,
    })
}

fn page_len(shown: usize, _unlinked: i64) -> i64 {
    shown as i64
}

/// One pass over every session the engine has, ordered by who needs a person.
pub fn sweep_sessions<C, I>(
    ctx: &AppContext<C, I>,
    params: &SweepSessionsParams,
) -> Result<SessionSweepResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let project = match &params.project {
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?),
        None => None,
    };
    let engine = match engine_of(ctx) {
        Ok(engine) => engine,
        Err(VogtError::EngineUnavailable(_)) => {
            return Ok(SessionSweepResult {
                rows: Vec::new(),
                total: 0,
                needs_you: 0,
                engine_available: false,
            });
        }
        Err(error) => return Err(error),
    };
    let swept = match engine.sweep_sessions(params.screen_lines) {
        Ok(Some(rows)) => rows,
        Ok(None) | Err(VogtError::EngineUnavailable(_)) => {
            return Ok(SessionSweepResult {
                rows: Vec::new(),
                total: 0,
                needs_you: 0,
                engine_available: false,
            });
        }
        Err(error) => return Err(error),
    };
    let recorded = ctx
        .declared
        .read()?
        .list_sessions(None, None, true, i64::MAX, 0)?;
    let now = crate::application::services::now_of(&ctx.clock);
    let stall = params.stall_after_minutes.saturating_mul(60);
    let mut rows = Vec::new();
    for entry in &swept {
        let declared = recorded
            .iter()
            .find(|session| session.engine_session_id == entry.session.id);
        if let Some(project) = &project {
            if declared.map(|session| session.project_id.as_str()) != Some(project.id.as_str()) {
                continue;
            }
        }
        let summary = match declared {
            Some(session) => summarize(ctx, session, Some(&entry.session))?,
            None => unlinked_summary(&entry.session),
        };
        let (attention, reason) = attention_of(&entry.session, entry.ready, now, stall);
        rows.push(SessionSweepRow {
            needs_you: attention.needs_you(),
            attention: attention_name(attention).to_string(),
            reason,
            screen_tail: entry.screen_tail.clone(),
            session: summary,
        });
    }
    rows.sort_by(|left, right| {
        attention_rank(&left.attention)
            .cmp(&attention_rank(&right.attention))
            .then_with(|| right.session.started_at.cmp(&left.session.started_at))
    });
    let needs_you = rows.iter().filter(|row| row.needs_you).count() as i64;
    let total = rows.len() as i64;
    Ok(SessionSweepResult {
        rows,
        total,
        needs_you,
        engine_available: true,
    })
}

/// The tail of a session's own raw output log.
pub fn log_tail<C, I>(
    ctx: &AppContext<C, I>,
    params: &LogTailParams,
) -> Result<LogTailResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let bytes = params.tail_bytes.clamp(1, LOG_TAIL_MAX_BYTES);
    let log = engine
        .history_log(&engine_id, bytes, params.strip_ansi)?
        .ok_or_else(|| {
            VogtError::NotFound(format!("no session with id {}", py_repr(&params.id)))
        })?;
    Ok(LogTailResult {
        session_id: params.id.clone(),
        text: log.text,
        bytes: log.bytes,
        total_bytes: log.total_bytes,
        truncated: log.truncated,
    })
}

/// Write to a session's terminal.
pub fn input<C, I>(
    ctx: &AppContext<C, I>,
    params: &InputParams,
) -> Result<SessionInputResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let text = params.text.clone().unwrap_or_default();
    let keys = params.keys.clone().unwrap_or_default();
    for key in &keys {
        if !INPUT_KEYS.contains(&key.as_str()) {
            return Err(VogtError::InvalidRequest(format!(
                "unknown input key {} (known: {})",
                py_repr(key),
                INPUT_KEYS.join(", ")
            )));
        }
    }
    // Enter by either route is a submit: `keys: ["enter"]` used to report
    // `submitted: false` (WI-918).
    let submitted = params.submit || keys.iter().any(|key| key == "enter");
    if text.is_empty() && keys.is_empty() && !params.submit {
        return Err(VogtError::InvalidRequest(
            "nothing to send: give text, keys, or submit".to_string(),
        ));
    }
    let size = text.len();
    if size > SESSION_INPUT_MAX_BYTES {
        return Err(VogtError::InvalidRequest(format!(
            "text is {size} bytes; the engine accepts at most {SESSION_INPUT_MAX_BYTES} bytes per write"
        )));
    }
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    let linked = found.session.is_some();

    // Text, then each named key, then Enter — each its own engine write, so a
    // terminal reading an Esc does not take the bytes after it as an Alt-chord.
    let mut writes_in_order: Vec<(String, bool)> = Vec::new();
    if !text.is_empty() {
        writes_in_order.push((text.clone(), false));
    }
    for key in &keys {
        writes_in_order.push((key_bytes(key), false));
    }
    if params.submit {
        writes_in_order.push((String::new(), true));
    }
    let person = answers_as_person(ctx)?;
    let confirming = submitted && params.confirm;
    let mut before = if confirming {
        engine
            .get_session(&engine_id)
            .ok()
            .flatten()
            .map(|seen| seen.activity)
    } else {
        None
    };
    let mut woke = false;
    if let Err(refused) = send_all(engine, &engine_id, &params.id, &writes_in_order, person) {
        // The engine refuses input to a hibernated session. Typing into one is
        // asking for it back: wake it, wait until it is at its prompt, then
        // type. Anything else the engine refused stays refused.
        let VogtError::Conflict(_) = refused else {
            return Err(refused);
        };
        let current = engine.get_session(&engine_id)?;
        if current.as_ref().is_none_or(|live| !live.hibernated()) {
            return Err(refused);
        }
        wake_resolved(
            ctx,
            engine,
            &found,
            &format!("woken to deliver input: {reason}"),
        )?;
        woke = true;
        let waited = engine.wait_session(
            &engine_id,
            "ready",
            Duration::from_secs(params.wake_timeout_s.max(0) as u64),
        )?;
        let ready = waited
            .as_ref()
            .is_some_and(|waited| waited.outcome == "ready");
        if !ready {
            let outcome = waited
                .map(|waited| waited.outcome)
                .unwrap_or_else(|| "no answer".to_string());
            return Err(VogtError::Conflict(format!(
                "woke session {}, but it was not ready for input within {}s ({outcome}); nothing was typed. Read session_screen — it may be showing a dialog — then retry",
                py_repr(&params.id),
                params.wake_timeout_s
            )));
        }
        // Freshly woken and at its prompt: nothing was running.
        before = Some("waiting-for-input".to_string());
        send_all(engine, &engine_id, &params.id, &writes_in_order, person)?;
    }
    let verdict = if confirming {
        confirm_delivery(engine, &engine_id, before.as_deref())
    } else {
        delivery::judge(submitted, None, &[])
    };
    let entity = found
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let detail = json!({
        "engine_session_id": engine_id,
        "linked": linked,
        "bytes": size,
        "keys": keys,
        "submit": params.submit,
        "submitted": submitted,
        "delivery": verdict.delivery.as_str(),
        "woke": woke,
        "person": person,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_INPUT,
        &reason,
        "session",
        &entity,
        &detail,
        "session.input",
        Some(&detail),
    )?;
    Ok(SessionInputResult {
        id: params.id.clone(),
        engine_session_id: engine_id,
        linked,
        bytes: size as i64,
        keys,
        submitted,
        delivery: verdict.delivery.as_str().to_string(),
        delivery_evidence: verdict.evidence,
        woke,
    })
}

/// Read a session's screen.
pub fn screen<C, I>(
    ctx: &AppContext<C, I>,
    params: &ScreenParams,
) -> Result<SessionScreenResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let found = engine.session_screen(&engine_id, params.scrollback_lines)?;
    Ok(screen_result(&require_screen(found, &params.id)?))
}

/// Block until a session reaches a state, or the timeout passes.
pub fn wait<C, I>(
    ctx: &AppContext<C, I>,
    params: &WaitParams,
) -> Result<SessionWaitResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let found = engine.wait_session(
        &engine_id,
        &params.until,
        Duration::from_secs(params.timeout_s.max(0) as u64),
    )?;
    let waited = found.ok_or_else(|| {
        VogtError::NotFound(format!("no session with id {}", py_repr(&params.id)))
    })?;
    Ok(SessionWaitResult {
        outcome: waited.outcome,
        matched: waited.matched,
        waited_ms: waited.waited_ms,
        screen: screen_result(&waited.screen),
    })
}

/// Wake a hibernated session. A live session is returned as it is — waking is
/// idempotent, so a caller that is not sure need not check first.
pub fn wake<C, I>(ctx: &AppContext<C, I>, params: &WakeParams) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    wake_resolved(ctx, engine, &found, &reason)
}

/// Keep a session awake, or let it hibernate again.
pub fn keep_awake<C, I>(
    ctx: &AppContext<C, I>,
    params: &KeepAwakeParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let updated = engine
        .keep_awake(&engine_id, params.keep_awake)?
        .ok_or_else(|| {
            VogtError::NotFound(format!("no session with id {}", py_repr(&params.id)))
        })?;
    let recorded = resolve_target(ctx, &params.id)?;
    let why = recorded
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or(engine_id);
    let mut writing = write_of(ctx);
    let detail = json!({"keep_awake": params.keep_awake});
    audited_action(
        &mut writing,
        SESSION_KEEP_AWAKE,
        &reason,
        "session",
        &why,
        &detail,
        "session.keep_awake",
        Some(&detail),
    )?;
    let summary = match &recorded.session {
        Some(session) => summarize(ctx, session, Some(&updated))?,
        None => unlinked_summary(&updated),
    };
    Ok(SessionResult { session: summary })
}

/// Change a session's role. Nominating oversight is a person's act.
pub fn set_role<C, I>(
    ctx: &AppContext<C, I>,
    params: &SetRoleParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    if params.role == "oversight" && ctx.principal.kind == ActorKind::Agent {
        return Err(VogtError::PersonRequired(
            "nominating an oversight session can only be done by a person, not by an agent"
                .to_string(),
        ));
    }
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let updated = engine.set_role(&engine_id, &params.role)?.ok_or_else(|| {
        VogtError::NotFound(format!("no session with id {}", py_repr(&params.id)))
    })?;
    let recorded = resolve_target(ctx, &params.id)?;
    let why = recorded
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or(engine_id);
    let mut writing = write_of(ctx);
    let detail = json!({"role": params.role});
    audited_action(
        &mut writing,
        SESSION_SET_ROLE,
        &reason,
        "session",
        &why,
        &detail,
        "session.role_set",
        Some(&detail),
    )?;
    let summary = match &recorded.session {
        Some(session) => summarize(ctx, session, Some(&updated))?,
        None => unlinked_summary(&updated),
    };
    Ok(SessionResult { session: summary })
}

/// Answer the dialog a session shows — a permission request or a startup gate —
/// by option, not by keystrokes. Exactly one of `option` or `label`. Whether the
/// caller counts as a person is the engine's decision: the flag is sent, and a
/// permission prompt an agent answers is refused there.
pub fn answer<C, I>(ctx: &AppContext<C, I>, params: &AnswerParams) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let label = params
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty());
    if params.option.is_some() == label.is_some() {
        return Err(VogtError::InvalidRequest(
            "give exactly one of option (a number) or label".to_string(),
        ));
    }
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    let person = answers_as_person(ctx)?;
    let answered = engine
        .answer_session(&engine_id, params.option, label, params.expect_question.as_deref(), person)?
        .ok_or_else(|| {
            VogtError::NotFound(format!(
                "the engine has no session {}, or predates answering (no POST /api/sessions/{{id}}/answer)",
                py_repr(&params.id)
            ))
        })?;
    let chosen = answered.get("chosen").cloned().unwrap_or(Value::Null);
    let number = chosen.get("number").and_then(Value::as_i64).unwrap_or(0);
    let chosen_label = chosen
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let question = answered
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = answered
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("permission")
        .to_string();
    let dismissed = answered.get("dismissed") == Some(&Value::Bool(true));
    let entity = found
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or(engine_id.clone());
    let detail = json!({
        "engine_session_id": engine_id,
        "linked": found.session.is_some(),
        "question": question.chars().take(300).collect::<String>(),
        "kind": kind,
        "option": number,
        "label": chosen_label.chars().take(200).collect::<String>(),
        "dismissed": dismissed,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_ANSWER,
        &reason,
        "session",
        &entity,
        &detail,
        "session.answered",
        Some(&detail),
    )?;
    Ok(json!({
        "id": params.id,
        "engine_session_id": engine_id,
        "question": question,
        "kind": kind,
        "chosen": {"number": number, "label": chosen_label, "selected": true},
        "dismissed": dismissed,
    }))
}

/// Hibernate a session to free its memory, keeping it listed. The session's
/// token is revoked: nothing runs to hold it, and a wake mints a new one. A
/// stopped session has nothing to hibernate.
pub fn hibernate<C, I>(
    ctx: &AppContext<C, I>,
    params: &HibernateParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    if found
        .session
        .as_ref()
        .is_some_and(|session| session.stopped_at.is_some())
    {
        return Err(VogtError::Conflict(format!(
            "session {} was stopped; there is nothing to hibernate",
            py_repr(&params.id)
        )));
    }
    let hibernated = engine
        .hibernate_session(&engine_id, Some(&reason), params.allow_shell)?
        .ok_or_else(|| {
            VogtError::NotFound(format!(
                "the engine has no session {}, or predates hibernation (no POST /api/sessions/{{id}}/hibernate)",
                py_repr(&params.id)
            ))
        })?;
    let resumable = hibernated
        .hibernation
        .as_ref()
        .map(|hibernation| hibernation.resumable);
    let outcome = json!({
        "engine_session_id": engine_id,
        "linked": found.session.is_some(),
        "allow_shell": params.allow_shell,
        "resumable": resumable,
    });
    let Some(session) = found.session.clone() else {
        let mut writing = write_of(ctx);
        audited_action(
            &mut writing,
            SESSION_HIBERNATE,
            &reason,
            "session",
            &engine_id,
            &outcome,
            "session.hibernated",
            Some(&outcome),
        )?;
        return Ok(SessionResult {
            session: unlinked_summary(&hibernated),
        });
    };
    let mut writing = write_of(ctx);
    let actor_id = session.actor_id.clone();
    let session_id = session.id.clone();
    let why = reason.clone();
    let recorded = outcome.clone();
    let revoked_at = now(&writing);
    audited_write(
        &mut writing,
        SESSION_HIBERNATE,
        &reason,
        move |txn, _actor| {
            let current = txn.session_by_id(&session_id)?.ok_or_else(|| {
                VogtError::NotFound(format!("no session {}", py_repr(&session_id)))
            })?;
            let mut revoked = 0;
            for token in txn.tokens_for_actor(&actor_id, false)? {
                if txn.revoke_token(&token.id, &format!("hibernated: {why}"), revoked_at)? {
                    revoked += 1;
                }
            }
            let mut summary = recorded.clone();
            summary["tokens_revoked"] = json!(revoked);
            Ok(WriteOutcome::new(
                (),
                "session",
                &current.id,
                audited_payload(&current),
                "session.hibernated",
                summary,
            ))
        },
    )?;
    Ok(SessionResult {
        session: summarize(ctx, &session, Some(&hibernated))?,
    })
}

// -- shared helpers -----------------------------------------------------------

struct Subject {
    project_id: String,
    work_item_id: Option<String>,
    cwd: String,
    name: String,
}

/// The project and work item a session opens on, and the directory it runs in.
/// Every session has a project: a work item without one, and no project named,
/// falls back to the deployment's scratch project.
fn subject_of<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    session_id: &str,
) -> Result<Subject, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let work_item = match &params.work_item {
        Some(reference) => Some(resolve::work_item(&ctx.declared.read()?, reference)?),
        None => None,
    };
    let named = match &params.project {
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?),
        None => None,
    };
    if let (Some(item), Some(project)) = (&work_item, &named) {
        if item.project_id.as_deref() != Some(project.id.as_str()) {
            return Err(VogtError::InvalidRequest(format!(
                "work item {} is not in project {}",
                py_repr(&item.reference),
                py_repr(&project.slug)
            )));
        }
    }
    let project = match named {
        Some(project) => project,
        None => match work_item
            .as_ref()
            .and_then(|item| item.project_id.as_deref())
        {
            Some(project_id) => {
                ctx.declared
                    .read()?
                    .project_by_id(project_id)?
                    .ok_or_else(|| {
                        VogtError::InvalidRequest(format!(
                            "work item {} names a project that does not exist",
                            py_repr(
                                work_item
                                    .as_ref()
                                    .map(|item| item.reference.as_str())
                                    .unwrap_or("")
                            )
                        ))
                    })?
            }
            None => {
                return Err(VogtError::InvalidRequest(
                    "a session needs a project or a work item to open in".to_string(),
                ));
            }
        },
    };
    let cwd = if project.root_path.is_empty() {
        return Err(VogtError::InvalidRequest(format!(
            "project {} has no checkout to open a session in",
            py_repr(&project.slug)
        )));
    } else {
        project.root_path.clone()
    };
    let name = params
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .map(str::trim)
        .map(str::to_string)
        .unwrap_or_else(|| match &work_item {
            Some(item) => item.title.clone(),
            None => project.slug.clone(),
        });
    let _ = session_id;
    Ok(Subject {
        project_id: project.id,
        work_item_id: work_item.map(|item| item.id),
        cwd,
        name,
    })
}

/// The scopes a session's own token carries, from the deployment's config.
fn session_scopes<C, I>(ctx: &AppContext<C, I>) -> Result<Vec<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    auth::parse_scopes(&ctx.config.agent_session_scopes)
        .map(|scopes| scopes.into_iter().map(str::to_string).collect())
        .map_err(VogtError::InvalidRequest)
}

/// A fresh token secret and the hash that is all the store keeps.
fn minted_token() -> Result<(String, String), VogtError> {
    let mut entropy = [0u8; TOKEN_ENTROPY_BYTES];
    getrandom::getrandom(&mut entropy)
        .map_err(|_| VogtError::InvalidRequest("the system RNG is unavailable".to_string()))?;
    Ok(auth::issue(&entropy))
}

/// The engine client, or the unconfigured error.
pub(super) fn engine_of<C, I>(ctx: &AppContext<C, I>) -> Result<&EngineClient, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    ctx.engine.as_ref().ok_or_else(|| {
        VogtError::EngineUnavailable(
            "no session engine is configured (set session_engine_url)".to_string(),
        )
    })
}

/// A session named by either id form, resolved to the engine's id. `session` is
/// Vogt's record when there is one; `None` means the engine id names a session
/// Vogt never linked, which the caller may still read or drive.
pub(super) struct Target {
    pub engine_session_id: String,
    pub session: Option<CodingSession>,
}

/// Resolve a `ses_…` id or an engine UUID to the engine's session id.
///
/// A `ses_…` id must be one Vogt recorded — otherwise it is a wrong id and says
/// so. Anything else is the engine's own id, passed through as given and linked
/// to Vogt's record when one exists. Shared with the grants half (WI-1074).
pub(super) fn resolve_target<C, I>(
    ctx: &AppContext<C, I>,
    session_id: &str,
) -> Result<Target, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let wanted = session_id.trim();
    if wanted.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".to_string(),
        ));
    }
    let view = ctx.declared.read()?;
    if let Some(id) = wanted.strip_prefix("ses_") {
        let _ = id;
        let session = view
            .session_by_id(wanted)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(wanted))))?;
        return Ok(Target {
            engine_session_id: session.engine_session_id.clone(),
            session: Some(session),
        });
    }
    Ok(Target {
        engine_session_id: wanted.to_string(),
        session: view.session_by_engine_id(wanted)?,
    })
}

/// Stop a session Vogt never recorded.
fn stop_unlinked<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    engine_id: &str,
    reason: &str,
) -> Result<SessionResult, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let found = engine.kill_session(engine_id, Some(reason), Some(&ctx.principal.identity_ref))?;
    if !found {
        return Err(VogtError::NotFound(format!(
            "no session with id {}",
            py_repr(engine_id)
        )));
    }
    let session = engine
        .get_session(engine_id)?
        .ok_or_else(|| VogtError::NotFound(format!("no session with id {}", py_repr(engine_id))))?;
    let detail = json!({"engine_session_id": engine_id, "linked": false});
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_STOP,
        reason,
        "session",
        engine_id,
        &detail,
        "session.stopped",
        Some(&detail),
    )?;
    Ok(SessionResult {
        session: unlinked_summary(&session),
    })
}

/// What the engine was asked to run.
fn build_spec<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    subject: &Subject,
    token: &str,
    actor_ref: &str,
    session_id: &str,
) -> Result<CreateSession, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let mut spec = CreateSession::new(subject.name.clone(), subject.cwd.clone());
    spec.template = params.template.clone();
    spec.model = params.model.clone();
    spec.effort = params.effort.clone();
    spec.resume = params.resume.clone();
    spec.permission_mode = Some(params.permission_mode.clone());
    spec.role = params.role.clone();
    spec.work_item = params.work_item.clone();
    spec.autopilot = params.autopilot.unwrap_or(false);
    let mut env = vec![
        ("VOGT_TOKEN".to_string(), token.to_string()),
        ("VOGT_ACTOR".to_string(), actor_ref.to_string()),
        ("VOGT_ENGINE_SESSION_ID".to_string(), session_id.to_string()),
    ];
    if let Some(task) = params
        .task
        .as_deref()
        .filter(|task| !task.trim().is_empty())
    {
        env.push((
            "VOGT_ENGINE_AGENT_TASK_PROMPT_FILE".to_string(),
            task.to_string(),
        ));
    }
    if let Some(url) = ctx
        .config
        .public_url
        .as_deref()
        .filter(|url| !url.is_empty())
    {
        env.push(("VOGT_URL".to_string(), url.to_string()));
    }
    spec.env = Some(env);
    Ok(spec)
}

/// One session as the list shows it, live fields merged over the declared row.
pub(super) fn summarize<C, I>(
    ctx: &AppContext<C, I>,
    session: &CodingSession,
    live: Option<&EngineSession>,
) -> Result<SessionSummary, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let project = ctx.declared.read()?.project_by_id(&session.project_id)?;
    let work_item = match session.work_item_id.as_deref() {
        Some(id) => ctx.declared.read()?.work_item_by_id(id)?,
        None => None,
    };
    let transcript = transcript_of(ctx, session, live);
    let resolved = runtime_of(session, live, transcript.as_ref());
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.engine_session_id.clone(),
        name: live
            .map(|live| live.name.clone())
            .unwrap_or_else(|| session.id.clone()),
        project: project.map(|project| project.slug),
        work_item: work_item.map(|item| item.reference),
        state: if session.stopped_at.is_some() {
            "stopped"
        } else {
            "running"
        }
        .to_string(),
        activity: None,
        alive: None,
        exit_code: None,
        cwd: session.cwd.clone(),
        started_at: session.started_at,
        stopped_at: session.stopped_at,
        stop_reason: None,
        activity_changed_at: None,
        created_at: None,
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        command: None,
        blocked: None,
        conversation_agent: None,
        hibernation: None,
        keep_awake: false,
        autopilot: false,
        autopilot_nudges: 0,
        role: "worker".to_string(),
        resources: None,
        template: session.template.clone(),
        permission_mode: None,
        stopped_by: None,
        runtime: SessionRuntime {
            agent: resolved.agent.clone(),
            model: resolved.model.clone(),
            model_basis: resolved.model_basis.map(str::to_string),
            effort: resolved.effort.clone(),
            effort_basis: resolved.effort_basis.map(str::to_string),
        },
        conversation_id: None,
        last_reply_excerpt: transcript
            .as_ref()
            .and_then(transcripts::last_reply_excerpt),
    };
    if let Some(live) = live {
        apply_live(&mut summary, live, &resolved);
    }
    Ok(summary)
}

fn transcript_of<C, I>(
    ctx: &AppContext<C, I>,
    session: &CodingSession,
    live: Option<&EngineSession>,
) -> Option<transcripts::Transcript>
where
    C: Clock,
    I: IdFactory,
{
    let roots: HashMap<&str, &Path> = ctx
        .config
        .session_transcript_roots
        .iter()
        .map(|(agent, path)| (agent.as_str(), path.as_path()))
        .collect();
    let started = UNIX_EPOCH + Duration::from_secs(session.started_at.unix_seconds().max(0) as u64);
    transcripts::find(
        &roots,
        &session.engine_session_id,
        live.and_then(|session| session.command.as_deref()),
        session.template.as_deref(),
        live.map(|session| session.cwd.as_str()),
        Some(started),
        false,
    )
}

fn runtime_of(
    session: &CodingSession,
    live: Option<&EngineSession>,
    transcript: Option<&transcripts::Transcript>,
) -> decisions::ResolvedRuntime {
    let (model, effort) = transcript.map(transcripts::runtime).unwrap_or((None, None));
    decisions::resolve_runtime(
        live.and_then(|session| session.command.as_deref()),
        live.and_then(|session| session.conversation_agent.as_deref()),
        model.as_deref(),
        effort.as_deref(),
        session.model.as_deref(),
        session.effort.as_deref(),
    )
}

/// Copy the engine's live fields onto a summary.
fn apply_live(
    summary: &mut SessionSummary,
    live: &EngineSession,
    resolved: &decisions::ResolvedRuntime,
) {
    summary.name = live.name.clone();
    summary.activity = Some(live.activity.clone());
    summary.alive = Some(live.alive);
    summary.exit_code = live.exit_code;
    summary.cwd = live.cwd.clone();
    summary.activity_changed_at = live.activity_changed_at.clone();
    summary.created_at = live.created_at.clone();
    summary.turn_started_at = live.turn_started_at.clone();
    summary.last_output_at = live.last_output_at.clone();
    summary.approval = live.approval.as_ref().map(approval_of);
    summary.command = live.command.clone();
    summary.blocked = live.blocked.as_ref().map(blocked_of);
    summary.conversation_agent = live.conversation_agent.clone();
    summary.hibernation = live.hibernation.as_ref().map(hibernation_of);
    summary.keep_awake = live.keep_awake;
    summary.autopilot = live.autopilot;
    summary.autopilot_nudges = live.autopilot_nudges;
    summary.role = live.role.clone();
    summary.resources = live.resources.as_ref().map(resources_of);
    summary.template = live.template.clone().or_else(|| summary.template.clone());
    summary.permission_mode = live.permission_mode.clone();
    summary.stopped_by = live.stopped_by.clone();
    summary.stop_reason = live.stop_reason.clone();
    summary.conversation_id = live.conversation_id.clone();
    summary.runtime.agent = resolved
        .agent
        .clone()
        .or_else(|| summary.runtime.agent.clone());
}

fn approval_of(approval: &EngineApproval) -> SessionApproval {
    SessionApproval {
        question: approval.question.clone(),
        command_excerpt: approval.command_excerpt.clone(),
        detected_at: approval.detected_at.clone(),
        deadline_seconds: approval.deadline_seconds,
        deadline_at: approval.deadline_at.clone(),
        kind: approval.kind.clone(),
        options: approval
            .options
            .iter()
            .map(|(number, label, selected)| SessionApprovalOption {
                number: *number,
                label: label.clone(),
                selected: *selected,
            })
            .collect(),
    }
}

fn blocked_of(blocked: &EngineBlocked) -> SessionBlocked {
    SessionBlocked {
        reason: blocked.reason.clone(),
        items: blocked.items.clone(),
        since: blocked.since.clone(),
    }
}

fn resources_of(resources: &EngineResources) -> SessionResources {
    SessionResources {
        rss_bytes: resources.rss_bytes,
        cpu_pct: resources.cpu_pct,
        processes: resources.processes,
        sampled_at: resources.sampled_at.clone(),
        over_threshold: resources.over_threshold,
    }
}

fn hibernation_of(hibernation: &EngineHibernation) -> SessionHibernation {
    SessionHibernation {
        at: hibernation.at.clone(),
        trigger: hibernation.trigger.clone(),
        resumable: hibernation.resumable,
        reason: hibernation.reason.clone(),
    }
}

/// A session the engine has and Vogt never recorded, carrying the engine's id
/// in both id fields.
fn unlinked_summary(session: &EngineSession) -> SessionSummary {
    let resolved = decisions::resolve_runtime(
        session.command.as_deref(),
        session.conversation_agent.as_deref(),
        None,
        None,
        None,
        None,
    );
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.id.clone(),
        name: session.name.clone(),
        project: None,
        work_item: None,
        state: if session.activity == "stopped" {
            "stopped"
        } else {
            "running"
        }
        .to_string(),
        activity: None,
        alive: None,
        exit_code: None,
        cwd: session.cwd.clone(),
        started_at: session
            .created_at
            .as_deref()
            .and_then(|text| crate::core::from_iso(text).ok())
            .unwrap_or_else(|| Moment::from_unix(0, 0)),
        stopped_at: None,
        stop_reason: None,
        activity_changed_at: None,
        created_at: None,
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        command: None,
        blocked: None,
        conversation_agent: None,
        hibernation: None,
        keep_awake: false,
        autopilot: false,
        autopilot_nudges: 0,
        role: "worker".to_string(),
        resources: None,
        template: None,
        permission_mode: None,
        stopped_by: None,
        runtime: SessionRuntime {
            agent: resolved.agent.clone(),
            model: resolved.model.clone(),
            model_basis: resolved.model_basis.map(str::to_string),
            effort: resolved.effort.clone(),
            effort_basis: resolved.effort_basis.map(str::to_string),
        },
        conversation_id: None,
        last_reply_excerpt: None,
    };
    apply_live(&mut summary, session, &resolved);
    summary
}

fn unlinked_rows(
    live: &HashMap<String, EngineSession>,
    recorded: &[CodingSession],
    params: &ListSessionsParams,
) -> Vec<SessionSummary> {
    if params.project.is_some() || params.work_item.is_some() || params.offset != 0 {
        return Vec::new();
    }
    let known: HashSet<&str> = recorded
        .iter()
        .map(|session| session.engine_session_id.as_str())
        .collect();
    let mut rows: Vec<&EngineSession> = live
        .values()
        .filter(|session| !known.contains(session.id.as_str()))
        .filter(|session| params.include_stopped || session.activity != "stopped")
        .collect();
    rows.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    rows.into_iter().map(unlinked_summary).collect()
}

fn live_sessions(engine: Option<&EngineClient>) -> HashMap<String, EngineSession> {
    let Some(engine) = engine else {
        return HashMap::new();
    };
    match engine.list_sessions() {
        Ok(sessions) => sessions
            .into_iter()
            .map(|session| (session.id.clone(), session))
            .collect(),
        Err(VogtError::EngineUnavailable(_)) => HashMap::new(),
        Err(_) => HashMap::new(),
    }
}

fn rss_of(live: Option<&EngineSession>) -> i64 {
    live.and_then(|session| session.resources.as_ref())
        .map(|resources| resources.rss_bytes)
        .unwrap_or(0)
}

fn attention_of(
    session: &EngineSession,
    ready: bool,
    now: Moment,
    stall_after_seconds: i64,
) -> (Attention, String) {
    let last_output = session
        .last_output_at
        .as_deref()
        .and_then(|text| crate::core::from_iso(text).ok());
    let verdict = decisions::classify(
        Some(&session.activity),
        Some(session.alive),
        Some(ready),
        session
            .approval
            .as_ref()
            .map(|approval| approval.question.as_str()),
        session
            .blocked
            .as_ref()
            .map(|blocked| blocked.reason.as_str()),
        session
            .approval
            .as_ref()
            .map(|approval| approval.kind.as_str()),
        last_output,
        now,
        stall_after_seconds,
    );
    (verdict.attention, verdict.reason)
}

fn attention_name(attention: Attention) -> &'static str {
    match attention {
        Attention::Approval => "approval",
        Attention::Blocked => "blocked",
        Attention::Waiting => "waiting",
        Attention::Stalled => "stalled",
        Attention::Running => "running",
        Attention::Idle => "idle",
        Attention::Hibernated => "hibernated",
        Attention::Exited => "exited",
        Attention::Unknown => "unknown",
    }
}

fn attention_rank(name: &str) -> u8 {
    match name {
        "approval" => Attention::Approval.order(),
        "blocked" => Attention::Blocked.order(),
        "waiting" => Attention::Waiting.order(),
        "stalled" => Attention::Stalled.order(),
        "running" => Attention::Running.order(),
        "idle" => Attention::Idle.order(),
        "hibernated" => Attention::Hibernated.order(),
        "exited" => Attention::Exited.order(),
        _ => Attention::Unknown.order(),
    }
}

fn screen_result(screen: &EngineScreen) -> SessionScreenResult {
    SessionScreenResult {
        id: screen.id.clone(),
        cols: screen.cols,
        rows: screen.rows,
        lines: screen.lines.clone(),
        cursor_row: screen.cursor_row,
        cursor_col: screen.cursor_col,
        title: screen.title.clone(),
        activity: screen.activity.clone(),
        alive: screen.alive,
        ready: screen.ready,
        scrollback: screen.scrollback.clone(),
        turn_started_at: screen.turn_started_at.clone(),
        last_output_at: screen.last_output_at.clone(),
        approval: screen.approval.as_ref().map(approval_of),
        blocked: screen.blocked.as_ref().map(blocked_of),
    }
}

fn require_screen(screen: Option<EngineScreen>, id: &str) -> Result<EngineScreen, VogtError> {
    screen.ok_or_else(|| VogtError::NotFound(format!("no session with id {}", py_repr(id))))
}

fn bare_acknowledgement(text: &str, keys: &[String], submit: bool) -> bool {
    text.trim().is_empty() && (submit || keys.iter().any(|key| key == "enter"))
}

fn key_bytes(key: &str) -> String {
    match key {
        "enter" => "\r".to_string(),
        "esc" => "\u{1b}".to_string(),
        "tab" => "\t".to_string(),
        "backspace" => "\u{7f}".to_string(),
        "ctrl-c" => "\u{3}".to_string(),
        "ctrl-d" => "\u{4}".to_string(),
        "up" => "\u{1b}[A".to_string(),
        "down" => "\u{1b}[B".to_string(),
        "right" => "\u{1b}[C".to_string(),
        "left" => "\u{1b}[D".to_string(),
        other => other.to_string(),
    }
}

fn input_bytes(text: &str, keys: &[String], submit: bool) -> i64 {
    let keys: usize = keys.iter().map(|key| key_bytes(key).len()).sum();
    (text.len() + keys + usize::from(submit)) as i64
}

fn confirm_delivery(
    engine: &EngineClient,
    engine_id: &str,
    before: Option<&str>,
) -> delivery::Verdict {
    let mut seen = Vec::new();
    for _ in 0..CONFIRM_READS {
        std::thread::sleep(CONFIRM_INTERVAL);
        let Ok(Some(screen)) = engine.session_screen(engine_id, 0) else {
            break;
        };
        seen.push(delivery::Observation {
            activity: screen.activity.clone(),
            lines: screen.lines.clone(),
        });
        if matches!(
            screen.activity.as_deref(),
            Some("running") | Some("awaiting-approval")
        ) {
            break;
        }
    }
    delivery::judge(true, before, &seen)
}

/// One engine write per chunk. `Ok(false)` is the engine saying it has no live
/// session by that id — said with the name the caller used.
fn send_all(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    chunks: &[(String, bool)],
    person: bool,
) -> Result<(), VogtError> {
    for (text, submit) in chunks {
        let sent = engine.send_input(engine_id, text, *submit, person)?;
        if !sent {
            return Err(VogtError::NotFound(format!(
                "the engine has no live session {}",
                py_repr(named)
            )));
        }
    }
    Ok(())
}

/// Wake `target` if it is hibernated; its summary either way.
///
/// For a session Vogt started, a new token is minted for the session's own
/// actor and handed to the woken process, and every older token of that actor
/// is revoked in the same write. The process starts before the declared write.
fn wake_resolved<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    target: &Target,
    reason: &str,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine_id = &target.engine_session_id;
    let current = engine.get_session(engine_id)?.ok_or_else(|| {
        VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
    })?;
    let session = target.session.clone();
    if !current.hibernated() {
        if !current.alive {
            return Err(VogtError::Conflict(format!(
                "session {} has exited, not hibernated; start a new one (session_start, with resume to continue its conversation)",
                py_repr(engine_id)
            )));
        }
        let summary = match &session {
            Some(session) => summarize(ctx, session, Some(&current))?,
            None => unlinked_summary(&current),
        };
        return Ok(SessionResult { session: summary });
    }
    let Some(session) = session else {
        let woken = engine.wake_session(engine_id, None)?.ok_or_else(|| {
            VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
        })?;
        let detail = json!({"engine_session_id": engine_id, "linked": false});
        let mut writing = write_of(ctx);
        audited_action(
            &mut writing,
            SESSION_WAKE,
            reason,
            "session",
            engine_id,
            &detail,
            "session.woken",
            Some(&detail),
        )?;
        return Ok(SessionResult {
            session: unlinked_summary(&woken),
        });
    };
    if session.stopped_at.is_some() {
        return Err(VogtError::Conflict(format!(
            "session {} was stopped; start a new one instead",
            py_repr(&session.id)
        )));
    }
    let scopes = session_scopes(ctx)?;
    let (secret, token_hash) = minted_token()?;
    let env = session_env(ctx, &session.id, &secret);
    let woken = engine.wake_session(engine_id, Some(&env))?.ok_or_else(|| {
        VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
    })?;
    let conversation_id = current.conversation_id.clone();
    let engine_session_id = engine_id.clone();
    let session_id = session.id.clone();
    let why = reason.to_string();
    let mut writing = write_of(ctx);
    let token_id = fresh(&writing, "tok");
    let minted_at = now(&writing);
    audited_write(&mut writing, SESSION_WAKE, reason, move |txn, _actor| {
        let row = txn.session_by_id(&session_id)?;
        let holder = row
            .as_ref()
            .and_then(|row| txn.actor_by_id(&row.actor_id).ok().flatten());
        let (Some(row), Some(holder)) = (row, holder) else {
            return Err(VogtError::NotFound(format!(
                "no session {}",
                py_repr(&session_id)
            )));
        };
        let mut revoked = 0;
        for token in txn.tokens_for_actor(&row.actor_id, false)? {
            if txn.revoke_token(&token.id, &format!("superseded on wake: {why}"), minted_at)? {
                revoked += 1;
            }
        }
        txn.insert_token(
            &Token {
                id: token_id,
                actor_id: holder.id.clone(),
                actor_identity_ref: Some(holder.identity_ref.clone()),
                name: format!("session {}", row.id),
                scopes,
                kind: TokenKind::Api,
                created_at: minted_at,
                expires_at: None,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            },
            &token_hash,
        )?;
        Ok(WriteOutcome::new(
            (),
            "session",
            &row.id,
            audited_payload(&row),
            "session.woken",
            json!({
                "engine_session_id": engine_session_id,
                "conversation_id": conversation_id,
                "tokens_revoked": revoked,
            }),
        ))
    })?;
    Ok(SessionResult {
        session: summarize(ctx, &session, Some(&woken))?,
    })
}

/// Whether the caller counts as a person for the engine's person gate: a human
/// caller who is not the engine's own credential.
fn answers_as_person<C, I>(ctx: &AppContext<C, I>) -> Result<bool, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    Ok(ctx.principal.kind != ActorKind::Agent && !is_engine_credential(ctx)?)
}

/// Whether the caller is the session engine: the actor its credential is bound
/// to by configuration, or the very token it shares with this core.
fn is_engine_credential<C, I>(ctx: &AppContext<C, I>) -> Result<bool, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if ctx.principal.identity_ref == ctx.config.bootstrap_core_token_actor {
        return Ok(true);
    }
    let Some(configured) = &ctx.config.bootstrap_core_token_file else {
        return Ok(false);
    };
    let Some(presented) = &ctx.token else {
        return Ok(false);
    };
    let secret = match std::fs::read_to_string(configured) {
        Ok(text) => text.trim().to_string(),
        Err(_) => return Ok(false),
    };
    if secret.is_empty() {
        return Ok(false);
    }
    let row = ctx
        .declared
        .read()?
        .token_by_hash(&auth::hash_token(&secret))?;
    Ok(row.is_some_and(|row| row.revoked_at.is_none() && row.id == presented.id))
}

/// What an agent inside the session needs to reach Vogt.
fn session_env<C, I>(
    ctx: &AppContext<C, I>,
    session_id: &str,
    secret: &str,
) -> Vec<(String, String)>
where
    C: Clock,
    I: IdFactory,
{
    let mut env = vec![
        ("VOGT_HTTP_TOKEN".to_string(), secret.to_string()),
        ("VOGT_SESSION_ID".to_string(), session_id.to_string()),
    ];
    if let Some(url) = ctx.config.public_url.as_ref().filter(|url| !url.is_empty()) {
        env.push(("VOGT_URL".to_string(), url.clone()));
    }
    if let Some(engine) = &ctx.engine {
        env.push(("VOGT_ENGINE_URL".to_string(), engine.base_url().to_string()));
    }
    env
}

/// What the audit row records about a session. Everything except the credential.
fn audited_payload(session: &CodingSession) -> Value {
    json!({
        "id": session.id,
        "engine_session_id": session.engine_session_id,
        "project_id": session.project_id,
        "work_item_id": session.work_item_id,
        "actor_id": session.actor_id,
        "cwd": session.cwd,
        "template": session.template,
        "model": session.model,
        "effort": session.effort,
        "started_at": session.started_at.to_iso(),
        "stopped_at": session.stopped_at.map(|moment| moment.to_iso()),
    })
}

fn now<C, I>(
    writing: &writes::WriteContext<'_, C, I, impl crate::storage::interface::DeclaredStore>,
) -> Moment
where
    C: Clock,
    I: IdFactory,
{
    writing.clock().lock().expect("the shared clock").now()
}

fn fresh<C, I>(
    writing: &writes::WriteContext<'_, C, I, impl crate::storage::interface::DeclaredStore>,
    prefix: &str,
) -> String
where
    C: Clock,
    I: IdFactory,
{
    writing
        .ids()
        .lock()
        .expect("the shared id factory")
        .next(prefix)
}

/// The work item a branch name mentions, for a session credited to one.
pub(super) fn branch_work_item(view: &impl ReadView, text: &str) -> Option<WorkItem> {
    let references = crate::decisions::issue_references(text);
    let reference = references.first()?;
    resolve::work_item(view, reference).ok()
}

/// A person is required and the caller is an agent.
pub(super) fn person_required(kind: ActorKind, action: &str) -> Result<(), VogtError> {
    if kind == ActorKind::Agent {
        Err(VogtError::PersonRequired(format!(
            "{action} can only be done by a person, not by an agent"
        )))
    } else {
        Ok(())
    }
}

/// Silence the unused-import warning for the sweep entry type alias path.
#[allow(dead_code)]
fn _sweep_entry(entry: &EngineSweepEntry) -> &EngineSession {
    &entry.session
}

/// The brief a session's task becomes, when the work item is known.
#[allow(dead_code)]
fn task_brief(view: &dyn ReadView, item: &WorkItem, session_id: &str) -> Result<String, VogtError> {
    brief::brief_for_work_item(view, item, session_id, None)
}

/// A started session's clock read, so a test can pin it.
#[allow(dead_code)]
fn started_at(session: &CodingSession) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(session.started_at.unix_seconds().max(0) as u64)
}
