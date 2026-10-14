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

use std::collections::HashMap;
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
use crate::decisions;
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
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
        id: require_session_id(&params)?,
        reason: require_string(&params, "reason")?,
        allow_shell: optional_bool(&params, "allow_shell")?.unwrap_or(false),
    };
    Ok(serde_json::to_value(hibernate(ctx, &parsed)?).expect("a session result serialises"))
}

/// A session id the caller must give. Blank or whitespace is the same refusal
/// `resolve_target` gives, so the JSON entry points and the typed functions
/// agree on the text.
fn require_session_id(params: &Value) -> Result<String, VogtError> {
    match params.get("id").and_then(Value::as_str) {
        Some(text) if !text.trim().is_empty() => Ok(text.to_string()),
        _ => Err(VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".to_string(),
        )),
    }
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
    pub kind: String,
    pub options: Vec<SessionApprovalOption>,
    pub deadline_seconds: Option<i64>,
    pub deadline_at: Option<String>,
    /// Absent when the engine did not say when it noticed, which serialises as
    /// null rather than an empty string.
    pub detected_at: Option<String>,
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
    pub reason: Option<String>,
    pub resumable: bool,
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
    #[serde(default = "linked_default")]
    pub linked: bool,
    pub id: String,
    pub engine_session_id: String,
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub work_item_title: Option<String>,
    pub work_item_state: Option<String>,
    pub actor: Option<String>,
    pub cwd: String,
    pub template: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reason: Option<String>,
    pub started_at: Option<Moment>,
    pub stopped_at: Option<Moment>,
    // The engine's own facts about the process, copied straight across. They
    // sit here because pydantic emits fields in declaration order and Python
    // declares them right after `stopped_at`.
    pub name: Option<String>,
    pub command: Option<String>,
    pub exit_code: Option<i64>,
    pub activity_changed_at: Option<String>,
    pub conversation_agent: Option<String>,
    pub activity: Option<String>,
    pub alive: Option<bool>,
    pub turn_started_at: Option<String>,
    pub last_output_at: Option<String>,
    pub approval: Option<SessionApproval>,
    pub blocked: Option<SessionBlocked>,
    pub last_reply_excerpt: Option<String>,
    pub hibernation: Option<SessionHibernation>,
    pub keep_awake: Option<bool>,
    pub autopilot: Option<bool>,
    pub autopilot_nudges: Option<i64>,
    pub role: Option<String>,
    pub stopped_by: Option<String>,
    pub stop_reason: Option<String>,
    pub permission_mode: Option<String>,
    pub running: Option<SessionRuntime>,
    pub resources: Option<SessionResources>,
    pub conversation_id: Option<String>,
}

fn linked_default() -> bool {
    true
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionResult {
    pub session: SessionSummary,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
    /// Digest rows (design C1). `mode` defaults to `digest`, so the ordinary
    /// list returns these and leaves `sessions` empty; `mode=full` does the
    /// reverse. The Rust list does not build digests yet, so this is empty
    /// either way.
    #[serde(default)]
    pub digests: Vec<serde_json::Value>,
    /// Why the engine could not be asked, or null when it answered.
    pub engine: Option<String>,
    /// How many sessions match the filters, before `limit`/`offset`.
    #[serde(default)]
    pub total: i64,
    /// More sessions match than this page returns.
    #[serde(default)]
    pub truncated: bool,
    /// How many sessions the engine left out of its default list (WI-1289).
    /// 0 from an engine that does not report it, and when no engine answered.
    #[serde(default)]
    pub hidden_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSweepRow {
    pub attention: String,
    pub attention_reason: String,
    pub session: SessionSummary,
    pub screen_tail: Vec<String>,
    pub ready: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSweepResult {
    pub rows: Vec<SessionSweepRow>,
    pub counts: serde_json::Map<String, Value>,
    pub swept_at: Moment,
    pub engine: Option<String>,
}

/// What `log_tail` says when no session engine is configured at all.
const NO_ENGINE: &str = "no session engine is configured (VOGT_ENGINE_URL is unset)";

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogTailResult {
    pub session_id: Option<String>,
    pub text: String,
    pub bytes: i64,
    pub total_bytes: i64,
    pub truncated: bool,
    pub engine: Option<String>,
}

impl LogTailResult {
    /// No output to show: an unknown id or history that is off.
    fn empty() -> Self {
        Self {
            session_id: None,
            text: String::new(),
            bytes: 0,
            total_bytes: 0,
            truncated: false,
            engine: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionScreenResult {
    pub id: String,
    pub engine_session_id: String,
    pub cols: i64,
    pub rows: i64,
    pub lines: Vec<String>,
    pub cursor: Option<SessionCursor>,
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
pub struct SessionCursor {
    pub row: i64,
    pub col: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionWaitResult {
    pub id: String,
    pub engine_session_id: String,
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
    let spec = build_spec(ctx, params, &subject, &secret)?;
    // How long the engine took to accept the session: on the audit row, and
    // logged, so a slow or refused start is attributable afterwards.
    let spawn_started = std::time::Instant::now();
    let started = match engine.create_session(&spec) {
        Ok(started) => started,
        Err(error) => {
            tracing::warn!(
                target: "vogt.sessions",
                "session.start failed session_id={session_id} template={} engine_spawn_ms={} error={}",
                params.template.as_deref().unwrap_or(""),
                spawn_started.elapsed().as_millis(),
                error.code(),
            );
            return Err(error);
        }
    };
    let engine_spawn_ms = spawn_started.elapsed().as_millis() as i64;
    tracing::info!(
        target: "vogt.sessions",
        "session.start session_id={session_id} engine_session_id={} template={} engine_spawn_ms={engine_spawn_ms}",
        started.id,
        params.template.as_deref().unwrap_or(""),
    );
    // A resumed conversation starts in the directory its transcript records,
    // which may be below or beside the project root. Record where it actually
    // runs; every other session runs at the registry's root.
    let cwd = match (params.resume.is_some(), started.cwd.as_str()) {
        (true, resumed) if !resumed.is_empty() => resumed.to_string(),
        _ => subject.cwd.clone(),
    };
    let actor_id = fresh(&writing, "act");
    let token_id = fresh(&writing, "tok");
    // Read after the ranking tick: `why` consumes one, and every timestamp on
    // this path — actor, token, session, overlay — shares the next one.
    let now = now(&writing);
    let stored = {
        let actor_ref = actor_ref.clone();
        let scopes = scopes.clone();
        let token_hash = token_hash.clone();
        let session_id = session_id.clone();
        let engine_session_id = started.id.clone();
        let project_id = subject.project_id.clone();
        let work_item_id = subject.work_item_id.clone();
        let template = params.template.clone();
        let model = params.model.clone();
        let effort = params.effort.clone();
        let reason = reason.clone();
        let work_item_ref = subject.work_item_ref.clone();
        let project_slug = subject.project_slug.clone();
        let is_scratch = subject.is_scratch;
        let resume = params.resume.clone();
        let role = params.role.clone();
        let permission_mode = params.permission_mode.clone();
        let autopilot = spec.autopilot;
        let overlay_template = ctx.config.branch_binding_template.clone();
        audited_write(
            &mut writing,
            SESSION_START,
            &reason.clone(),
            move |txn, _actor| {
                let actor = Actor {
                    id: actor_id.clone(),
                    kind: ActorKind::Agent,
                    display_name: format!("Session {session_id}"),
                    identity_ref: actor_ref.clone(),
                    disabled: false,
                    created_at: now,
                };
                let token = Token {
                    id: token_id.clone(),
                    actor_id: actor.id.clone(),
                    actor_identity_ref: Some(actor_ref),
                    name: format!("session {session_id}"),
                    scopes,
                    kind: TokenKind::Api,
                    created_at: now,
                    expires_at: None,
                    last_used_at: None,
                    revoked_at: None,
                    revoked_reason: None,
                };
                let recorded = CodingSession {
                    id: session_id.clone(),
                    engine_session_id,
                    project_id: project_id.clone(),
                    work_item_id,
                    actor_id: actor.id.clone(),
                    cwd: cwd.clone(),
                    template,
                    model: model.clone(),
                    effort: effort.clone(),
                    reason,
                    started_at: now,
                    stopped_at: None,
                };
                txn.insert_actor(&actor)?;
                txn.insert_token(&token, &token_hash)?;
                txn.insert_session(&recorded)?;
                let payload = audited_payload(&recorded);
                // The declared half of the branch binding: a session opened for a
                // work item records the branch it will use. The summary's branch
                // is what was recorded, which is nothing when there is no item.
                let declared_branch = match &work_item_ref {
                    Some(work_ref) => Some(record_declared_branch(
                        txn,
                        work_ref,
                        &project_id,
                        &overlay_template,
                        now,
                    )?),
                    None => None,
                };
                Ok(WriteOutcome::new(
                    recorded,
                    "session",
                    &session_id,
                    payload,
                    "session.started",
                    json!({
                        "work_item": work_item_ref,
                        "branch": declared_branch,
                        "project": project_slug,
                        "cwd": cwd,
                        "scratch": is_scratch,
                        "model": model,
                        "effort": effort,
                        "resume": resume,
                        "autopilot": autopilot,
                        "role": role,
                        "permission_mode": permission_mode,
                        "engine_spawn_ms": engine_spawn_ms,
                    }),
                ))
            },
        )
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
        session: summarize(ctx, &stored?, Some(&started))?,
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
    let found = resolve_target(ctx, &params.id)?;
    let Some(session) = found.session else {
        let engine = engine_of(ctx)?;
        return stop_unlinked(ctx, engine, &found.engine_session_id, &reason);
    };
    // The kill goes first and its failure is not fatal: an engine that has
    // already forgotten the session, or one that is down, must not leave Vogt
    // unable to close its own record. Only an unconfigured engine yields no
    // answer; any other failure is the caller's to see.
    let engine_killed = match ctx.engine.as_ref() {
        None => None,
        Some(engine) => match engine.kill_session(
            &session.engine_session_id,
            Some(&reason),
            Some(&ctx.principal.identity_ref),
        ) {
            Ok(killed) => Some(killed),
            // An engine that is down answers nothing; any other failure is
            // the caller's to see.
            Err(VogtError::EngineUnavailable(_)) => None,
            Err(other) => return Err(other),
        },
    };
    let mut writing = write_of(ctx);
    let stopped_at = now(&writing);
    let actor_id = session.actor_id.clone();
    let session_id = session.id.clone();
    let named = params.id.clone();
    let stop_reason = reason.clone();
    audited_write(&mut writing, SESSION_STOP, &reason, move |txn, _actor| {
        let current = txn
            .session_by_id(&session_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(&named))))?;
        if current.stopped_at.is_some() {
            return Err(VogtError::Conflict(format!(
                "session {} was already stopped",
                py_repr(&named)
            )));
        }
        txn.mark_session_stopped(&session_id, stopped_at)?;
        for token in txn.tokens_for_actor(&actor_id, false)? {
            if token.revoked_at.is_none() {
                txn.revoke_token(&token.id, &stop_reason, stopped_at)?;
            }
        }
        let mut stopped = current;
        stopped.stopped_at = Some(stopped_at);
        Ok(WriteOutcome::new(
            session_id.clone(),
            "session",
            &session_id,
            audited_payload(&stopped),
            "session.stopped",
            json!({"engine_killed": engine_killed}),
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
    let (live, detail) = match ctx.engine.as_ref() {
        None => (
            Vec::new(),
            Some("no session engine is configured (VOGT_ENGINE_URL is unset)".to_string()),
        ),
        Some(engine) => match engine.list_sessions() {
            // Kept as a Vec: the engine's order is the order unlinked rows
            // append in, and a HashMap would shuffle it.
            Ok(rows) => (rows, None),
            Err(VogtError::EngineUnavailable(text)) => (Vec::new(), Some(text)),
            Err(error) => return Err(error),
        },
    };
    let mut sessions = rows_of(ctx, params, &live, detail.as_deref())?;
    if params.order == "rss" {
        // Stable: sessions with no sample keep their order, after the
        // measured ones. Sweep shares rows_of and must not inherit this.
        sessions.sort_by(|left, right| {
            let rss =
                |row: &SessionSummary| row.resources.as_ref().map(|r| r.rss_bytes).unwrap_or(0);
            match (left.resources.is_none(), right.resources.is_none()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => rss(right).cmp(&rss(left)),
            }
        });
    }
    Ok(SessionListResult {
        sessions,
        // `mode` defaults to `digest` on the Python side, which returns its
        // rows under `digests` and leaves `sessions` empty. The digest rows
        // are not built here yet, so both are empty for the same empty list.
        digests: Vec::new(),
        engine: detail,
        total: 0,
        truncated: false,
        hidden_count: 0,
    })
}

/// The rows `session.list` and `session.sweep` share.
fn rows_of<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
    live: &[EngineSession],
    detail: Option<&str>,
) -> Result<Vec<SessionSummary>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let view = ctx.declared.read()?;
    let project_id = match &params.project {
        Some(slug) => Some(resolve::project(&view, slug)?.id),
        None => None,
    };
    let work_item_id = match &params.work_item {
        Some(reference) => Some(resolve::work_item(&view, reference)?.id),
        None => None,
    };
    let recorded = view.list_sessions(
        project_id.as_deref(),
        work_item_id.as_deref(),
        params.include_stopped,
        params.limit,
        params.offset,
    )?;
    drop(view);
    let mut summaries = Vec::with_capacity(recorded.len());
    for session in &recorded {
        let engine_session = live.iter().find(|row| row.id == session.engine_session_id);
        summaries.push(summarize_asked(
            ctx,
            session,
            engine_session,
            detail.is_none(),
        )?);
    }
    let unfiltered = project_id.is_none() && work_item_id.is_none();
    if unfiltered && params.offset == 0 {
        let view = ctx.declared.read()?;
        for engine_session in live {
            // A hibernated session is asleep, not stopped: listed always. An
            // exited one is the engine's leftover, listed only when stopped
            // sessions were asked for. Linked rows are not filtered here —
            // the declared query already applied include_stopped.
            if !engine_session.alive && !engine_session.hibernated() && !params.include_stopped {
                continue;
            }
            if view.session_by_engine_id(&engine_session.id)?.is_none() {
                summaries.push(unlinked_summary(&view, engine_session)?);
            }
        }
        summaries.truncate(params.limit.max(0) as usize);
    }
    // Outside the read transaction: this is file I/O, and an excerpt that
    // cannot be had is a null, never an error. Only a session the engine
    // still has alive or asleep gets a transcript read.
    for summary in &mut summaries {
        let Some(live_session) = live.iter().find(|row| row.id == summary.engine_session_id) else {
            continue;
        };
        if !(live_session.alive || live_session.hibernated()) {
            continue;
        }
        let template = summary
            .template
            .clone()
            .filter(|template| !template.is_empty())
            .or_else(|| live_session.template.clone());
        let found = transcript_for(ctx, live_session, template.as_deref());
        summary.running = running_of(summary, live_session, found.as_ref());
        if summary.template.is_none()
            && live_session
                .template
                .as_deref()
                .is_some_and(|template| !template.is_empty())
        {
            // An unlinked session's template is the engine's to report.
            summary.template = live_session.template.clone();
        }
        if let Some(found) = found.as_ref() {
            if let Some(excerpt) = transcripts::last_reply_excerpt(found) {
                summary.last_reply_excerpt = Some(excerpt);
            }
        } else if live_session.conversation_agent.as_deref() == Some("opencode") {
            if let Some(excerpt) = opencode_excerpt(ctx, &live_session.id) {
                summary.last_reply_excerpt = Some(excerpt);
            }
        }
    }
    Ok(summaries)
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
    let swept_at = crate::application::services::now_of(&ctx.clock);
    let empty = |engine: Option<String>| SessionSweepResult {
        rows: Vec::new(),
        counts: serde_json::Map::from_iter([
            ("total".to_string(), json!(0)),
            ("needs_you".to_string(), json!(0)),
        ]),
        swept_at,
        engine,
    };
    let engine = match ctx.engine.as_ref() {
        None => {
            return Ok(empty(Some(
                "no session engine is configured (VOGT_ENGINE_URL is unset)".to_string(),
            )))
        }
        Some(engine) => engine,
    };
    let entries = match engine.sweep_sessions(params.screen_lines) {
        Ok(Some(rows)) => rows,
        Ok(None) => engine
            .list_sessions()?
            .into_iter()
            .filter(|row| row.alive || row.hibernated())
            .map(|session| EngineSweepEntry {
                session,
                screen_tail: Vec::new(),
                ready: false,
            })
            .collect(),
        Err(VogtError::EngineUnavailable(text)) => return Ok(empty(Some(text))),
        Err(error) => return Err(error),
    };
    let live: Vec<EngineSession> = entries.iter().map(|entry| entry.session.clone()).collect();
    let listed = rows_of(
        ctx,
        &ListSessionsParams {
            project: params.project.clone(),
            work_item: None,
            include_stopped: false,
            limit: 500,
            offset: 0,
            order: "started".to_string(),
        },
        &live,
        None,
    )?;
    let stall = params.stall_after_minutes.saturating_mul(60);
    let mut rows = Vec::new();
    for summary in listed {
        let Some(entry) = entries
            .iter()
            .find(|entry| entry.session.id == summary.engine_session_id)
        else {
            continue;
        };
        let verdict = decisions::classify(
            summary.activity.as_deref(),
            summary.alive,
            Some(entry.ready),
            summary
                .approval
                .as_ref()
                .map(|approval| approval.question.as_str()),
            summary
                .blocked
                .as_ref()
                .map(|blocked| blocked.reason.as_str()),
            summary
                .approval
                .as_ref()
                .map(|approval| approval.kind.as_str()),
            summary
                .last_output_at
                .as_deref()
                .and_then(|text| crate::core::from_iso(text).ok()),
            swept_at,
            stall,
        );
        rows.push(SessionSweepRow {
            attention: verdict.attention.as_str().to_string(),
            attention_reason: verdict.reason.clone(),
            screen_tail: entry.screen_tail.clone(),
            ready: Some(entry.ready),
            session: summary,
        });
    }
    rows.sort_by(|left, right| {
        decisions::Attention::order_of(&left.attention)
            .cmp(&decisions::Attention::order_of(&right.attention))
            .then_with(|| {
                right
                    .session
                    .last_output_at
                    .cmp(&left.session.last_output_at)
            })
    });
    let mut counts = serde_json::Map::from_iter([
        ("total".to_string(), json!(rows.len() as i64)),
        ("needs_you".to_string(), json!(0)),
    ]);
    let mut needs_you = 0;
    for row in &rows {
        let entry = counts.entry(row.attention.clone()).or_insert(json!(0));
        *entry = json!(entry.as_i64().unwrap_or(0) + 1);
        if decisions::Attention::needs_you_name(&row.attention) {
            needs_you += 1;
        }
    }
    counts.insert("needs_you".to_string(), json!(needs_you));
    Ok(SessionSweepResult {
        rows,
        counts,
        swept_at,
        engine: None,
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
    // No engine is an ordinary answer here, not an error: there is no log to
    // read. A `ses_…` id Vogt has no record of is still a wrong id.
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(LogTailResult {
            engine: Some(NO_ENGINE.to_string()),
            ..LogTailResult::empty()
        });
    };
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    // `tail_bytes` is bounded by the parameter schema (1..=256 KiB), exactly as
    // Python's pydantic model bounds it, and the service passes it through.
    let log = match engine.history_log(&engine_id, params.tail_bytes, params.strip_ansi) {
        // A missing log — unknown id, or history off — is an empty answer, not
        // an error: there is simply no output to show.
        Ok(None) => return Ok(LogTailResult::empty()),
        Ok(Some(log)) => log,
        Err(VogtError::EngineUnavailable(message)) => {
            return Ok(LogTailResult {
                engine: Some(message),
                ..LogTailResult::empty()
            });
        }
        Err(error) => return Err(error),
    };
    Ok(LogTailResult {
        session_id: Some(log.session_id),
        text: log.text,
        bytes: log.bytes,
        total_bytes: log.total_bytes,
        truncated: log.truncated,
        engine: None,
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
    Ok(screen_result(
        &params.id,
        &engine_id,
        &require_screen(engine, found, &engine_id, &params.id)?,
    ))
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
    let until = params.until.replace('_', "-");
    let found = match engine.wait_session(
        &engine_id,
        &until,
        Duration::from_secs(params.timeout_s.max(0) as u64),
    ) {
        // A hibernated session does nothing until somebody wakes it, and a wait
        // is a read: it does not wake it. Say so at once, with the screen it
        // kept, rather than holding the caller to a timeout.
        Err(refused @ VogtError::Conflict(_)) => {
            let screen = engine.session_screen(&engine_id, 0)?;
            if screen
                .as_ref()
                .is_none_or(|screen| screen.activity.as_deref() != Some("hibernated"))
            {
                return Err(refused);
            }
            return Ok(SessionWaitResult {
                id: params.id.clone(),
                engine_session_id: engine_id.clone(),
                outcome: "hibernated".to_string(),
                matched: false,
                waited_ms: 0,
                screen: screen_result(&params.id, &engine_id, &screen.expect("checked")),
            });
        }
        other => other?,
    };
    let waited = found.ok_or_else(|| {
        if engine.get_session(&engine_id).ok().flatten().is_some() {
            VogtError::EngineUnavailable(
                "the session engine does not support wait yet (it has no GET \
                 /api/sessions/{id}/wait route); upgrade the engine, or poll \
                 session.screen"
                    .to_string(),
            )
        } else {
            VogtError::NotFound(format!(
                "the engine has no live session {}",
                py_repr(&params.id)
            ))
        }
    })?;
    Ok(SessionWaitResult {
        id: params.id.clone(),
        engine_session_id: engine_id.clone(),
        outcome: waited.outcome,
        matched: waited.matched,
        waited_ms: waited.waited_ms,
        screen: screen_result(&params.id, &engine_id, &waited.screen),
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
            VogtError::NotFound(format!(
                "the engine has no session {}, or predates hibernation",
                py_repr(&params.id)
            ))
        })?;
    let recorded = resolve_target(ctx, &params.id)?;
    let why = recorded
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let mut writing = write_of(ctx);
    let detail = json!({
        "engine_session_id": engine_id.clone(),
        "linked": recorded.session.is_some(),
        "keep_awake": params.keep_awake,
    });
    audited_action(
        &mut writing,
        SESSION_KEEP_AWAKE,
        &reason,
        "session",
        &why,
        &detail,
        "session.keep_awake",
        None,
    )?;
    let summary = match &recorded.session {
        Some(session) => summarize(ctx, session, Some(&updated))?,
        None => unlinked_summary(&ctx.declared.read()?, &updated)?,
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
    // The role is a person's nomination. An agent that could nominate itself
    // would hand itself an overseer's standing, and a worker demoting its
    // overseer is no better. Refused by who is asking, like `bypass`.
    if ctx.principal.kind == ActorKind::Agent {
        return Err(VogtError::RoleRefused(format!(
            "only a person nominates an oversight session, or makes one a worker              again ({} is an agent); ask a person to set the role from the GUI,              or start the session with role=oversight",
            ctx.principal.identity_ref
        )));
    }
    let engine = engine_of(ctx)?;
    let engine_id = resolve_target(ctx, &params.id)?.engine_session_id;
    let updated = engine.set_role(&engine_id, &params.role)?.ok_or_else(|| {
        VogtError::NotFound(format!(
            "the engine has no session {}, or predates session roles",
            py_repr(&params.id)
        ))
    })?;
    let recorded = resolve_target(ctx, &params.id)?;
    let why = recorded
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let mut writing = write_of(ctx);
    let detail = json!({
        "engine_session_id": engine_id.clone(),
        "linked": recorded.session.is_some(),
        "role": params.role,
        "keep_awake": updated.keep_awake,
    });
    audited_action(
        &mut writing,
        SESSION_SET_ROLE,
        &reason,
        "session",
        &why,
        &detail,
        "session.role_set",
        None,
    )?;
    let summary = match &recorded.session {
        Some(session) => summarize(ctx, session, Some(&updated))?,
        None => unlinked_summary(&ctx.declared.read()?, &updated)?,
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
        "person": person,
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
            session: unlinked_summary(&ctx.declared.read()?, &hibernated)?,
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
    session_id: String,
    project_id: String,
    project_slug: String,
    work_item_id: Option<String>,
    work_item_ref: Option<String>,
    cwd: String,
    name: String,
    brief: String,
    is_scratch: bool,
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
    // Both is a bad request, and it is reported before either is resolved so a
    // missing work item does not hide it.
    if params.work_item.is_some() && params.project.is_some() {
        return Err(VogtError::InvalidRequest(
            "give at most one of --work-item or --project".to_string(),
        ));
    }
    let work_item = match &params.work_item {
        Some(reference) => Some(resolve::work_item(&ctx.declared.read()?, reference)?),
        None => None,
    };
    let named = match &params.project {
        Some(slug) => Some(resolve::project(&ctx.declared.read()?, slug)?),
        None => None,
    };
    if work_item.is_some() && named.is_some() {
        return Err(VogtError::InvalidRequest(
            "give at most one of --work-item or --project".to_string(),
        ));
    }
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
    let brief = match &work_item {
        // The score explanation is optional on purpose: a brief that refused
        // to be written because a score could not be computed would make the
        // ranking a precondition for starting work.
        Some(item) => {
            let ranking = crate::application::views::why(ctx, &item.reference)
                .ok()
                .and_then(|value| serde_json::from_value(value).ok());
            crate::application::brief::brief_for_work_item(
                &ctx.declared.read()?,
                item,
                session_id,
                ranking.as_ref(),
            )?
        }
        None => crate::application::brief::brief_for_project(&project.slug, session_id),
    };
    Ok(Subject {
        session_id: session_id.to_string(),
        project_id: project.id,
        project_slug: project.slug,
        work_item_id: work_item.as_ref().map(|item| item.id.clone()),
        work_item_ref: work_item.map(|item| item.reference),
        cwd,
        name,
        brief,
        is_scratch: false,
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
pub(crate) fn engine_of<C, I>(ctx: &AppContext<C, I>) -> Result<&EngineClient, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    ctx.engine.as_ref().ok_or_else(|| {
        VogtError::EngineUnavailable(
            "no session engine is configured, so there is nothing to open a \
             terminal on (set VOGT_ENGINE_URL)"
                .to_string(),
        )
    })
}

/// A session named by either id form, resolved to the engine's id. `session` is
/// Vogt's record when there is one; `None` means the engine id names a session
/// Vogt never linked, which the caller may still read or drive.
pub(crate) struct Target {
    pub engine_session_id: String,
    pub session: Option<CodingSession>,
}

/// Resolve a `ses_…` id or an engine UUID to the engine's session id.
///
/// A `ses_…` id must be one Vogt recorded — otherwise it is a wrong id and says
/// so. Anything else is the engine's own id, passed through as given and linked
/// to Vogt's record when one exists. Shared with the grants half (WI-1074).
pub(crate) fn resolve_target<C, I>(
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
    // Read first: the kill removes the session, and the summary is built from
    // what the engine said it was, with the stop applied on top.
    let session = engine
        .get_session(engine_id)?
        .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(engine_id))))?;
    let killed = engine.kill_session(engine_id, Some(reason), Some(&ctx.principal.identity_ref))?;
    // The audit records its own moment, so the summary's stopped_at is the read
    // after it, one tick later than the moment the stop was asked.
    let outcome = json!({"engine_session_id": engine_id, "linked": false});
    let summary_row = json!({"engine_killed": killed, "linked": false});
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_STOP,
        reason,
        "session",
        engine_id,
        &outcome,
        "session.stopped",
        Some(&summary_row),
    )?;
    let stopped_at = now(&writing);
    let mut summary = unlinked_summary(&ctx.declared.read()?, &session)?;
    summary.alive = killed.then_some(false);
    summary.stopped_at = Some(stopped_at);
    Ok(SessionResult { session: summary })
}

/// What the engine was asked to run.
fn build_spec<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    subject: &Subject,
    token: &str,
) -> Result<CreateSession, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let autopilot = params.autopilot.unwrap_or_else(|| {
        params.template.is_some()
            && params
                .task
                .as_deref()
                .is_some_and(|task| !task.trim().is_empty())
    });
    let mut spec = CreateSession::new(subject.name.clone(), subject.cwd.clone());
    spec.template = params.template.clone();
    spec.model = params.model.clone();
    spec.effort = params.effort.clone();
    spec.resume = params.resume.clone();
    spec.permission_mode = Some(params.permission_mode.clone());
    spec.role = params.role.clone();
    spec.work_item = subject.work_item_ref.clone();
    spec.autopilot = autopilot;
    spec.prompt = Some(brief_with_task(
        &subject.brief,
        params.task.as_deref(),
        autopilot,
    ));
    spec.env = Some(session_env(
        ctx,
        &subject.session_id,
        token,
        subject.work_item_ref.as_deref(),
    ));
    Ok(spec)
}

/// The brief plus the task and the autopilot note, matching `_brief_with_task`.
fn brief_with_task(brief: &str, task: Option<&str>, autopilot: bool) -> String {
    let mut text = brief.to_string();
    if autopilot {
        text = format!(
            "{}\n\n{}",
            text.trim_end(),
            crate::application::brief::AUTOPILOT
        );
    }
    let task = task.unwrap_or("").trim();
    if task.is_empty() {
        return text;
    }
    format!("{}\n\n## Task\n\n{task}\n", text.trim_end())
}

/// Add the branch a session will use to the item's overlay, idempotently.
fn record_declared_branch(
    txn: &mut impl crate::storage::interface::WriteTxn,
    work_ref: &str,
    project_id: &str,
    template: &str,
    at: crate::core::Moment,
) -> Result<String, VogtError> {
    let branch = crate::branches::default_branch_name(work_ref, template);
    let existing = txn.work_overlay(work_ref)?;
    let mut branches = existing
        .as_ref()
        .map(|row| row.branches.clone())
        .unwrap_or_default();
    if branches.iter().any(|have| have == &branch) {
        return Ok(branch);
    }
    branches.push(branch.clone());
    let overlay = match existing {
        Some(mut row) => {
            row.branches = branches;
            row.updated_at = at;
            row
        }
        None => crate::core::WorkOverlay {
            subject_key: work_ref.to_string(),
            project_id: project_id.to_string(),
            rank: None,
            workflow_state: None,
            priority: None,
            effort: None,
            assignee_actor_id: None,
            initiative_id: None,
            branches,
            created_at: at,
            updated_at: at,
        },
    };
    txn.upsert_work_overlay(&overlay)?;
    Ok(branch)
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
    summarize_asked(ctx, session, live, true)
}

/// `asked` is whether the engine was reached: `alive` is its answer then, and
/// null when it could not be asked, which is a different fact from "not running".
fn summarize_asked<C, I>(
    ctx: &AppContext<C, I>,
    session: &CodingSession,
    live: Option<&EngineSession>,
    asked: bool,
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
    let actor = ctx.declared.read()?.actor_by_id(&session.actor_id)?;
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.engine_session_id.clone(),
        linked: true,
        project: project.map(|project| project.slug),
        work_item: work_item.as_ref().map(|item| item.reference.clone()),
        work_item_title: work_item.as_ref().map(|item| item.title.clone()),
        work_item_state: work_item.map(|item| item.state.to_string()),
        actor: Some(
            actor
                .map(|actor| actor.identity_ref)
                .unwrap_or(session.actor_id.clone()),
        ),
        cwd: session.cwd.clone(),
        template: session.template.clone(),
        model: session.model.clone(),
        effort: session.effort.clone(),
        reason: Some(session.reason.clone()),
        started_at: Some(session.started_at),
        stopped_at: session.stopped_at,
        name: None,
        command: None,
        exit_code: None,
        activity_changed_at: None,
        conversation_agent: None,
        activity: None,
        alive: asked.then(|| live.is_some_and(|live| live.alive)),
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        blocked: None,
        last_reply_excerpt: None,
        hibernation: None,
        // None until the engine was asked and answered: an engine that was
        // not asked has nothing to report, not a default of false/"worker".
        keep_awake: None,
        autopilot: None,
        autopilot_nudges: None,
        role: None,
        stopped_by: None,
        stop_reason: None,
        permission_mode: None,
        running: None,
        resources: None,
        conversation_id: None,
    };
    if let Some(live) = live {
        apply_live(&mut summary, live);
    }
    Ok(summary)
}

/// Copy the engine's live fields onto a summary.
fn apply_live(summary: &mut SessionSummary, live: &EngineSession) {
    summary.activity = Some(live.activity.clone());
    summary.name = Some(live.name.clone());
    summary.command = live.command.clone();
    summary.exit_code = live.exit_code;
    summary.activity_changed_at = live.activity_changed_at.clone();
    summary.conversation_agent = live.conversation_agent.clone();
    summary.turn_started_at = live.turn_started_at.clone();
    summary.last_output_at = live.last_output_at.clone();
    summary.approval = live.approval.as_ref().map(approval_of);
    summary.blocked = live.blocked.as_ref().map(blocked_of);
    summary.hibernation = live.hibernation.as_ref().map(hibernation_of);
    summary.keep_awake = Some(live.keep_awake);
    summary.autopilot = Some(live.autopilot);
    summary.autopilot_nudges = Some(live.autopilot_nudges);
    summary.role = Some(live.role.clone());
    summary.conversation_id = live.conversation_id.clone();
    summary.resources = live.resources.as_ref().map(resources_of);
    summary.permission_mode = live.permission_mode.clone();
    summary.stopped_by = live.stopped_by.clone();
    summary.stop_reason = live.stop_reason.clone();
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

/// The transcript a listed session's conversation is in. Only the id-based
/// lookups: a list must stay cheap and must not guess which of two agents in
/// one directory a session is, so no start time and no cwd guess.
fn transcript_for<C, I>(
    ctx: &AppContext<C, I>,
    live: &EngineSession,
    template: Option<&str>,
) -> Option<transcripts::Transcript>
where
    C: Clock,
    I: IdFactory,
{
    if ctx.config.session_transcript_roots.is_empty() {
        return None;
    }
    let roots: HashMap<&str, &Path> = ctx
        .config
        .session_transcript_roots
        .iter()
        .map(|(agent, path)| (agent.as_str(), path.as_path()))
        .collect();
    transcripts::find(
        &roots,
        &live.id,
        live.command.as_deref(),
        template,
        Some(live.cwd.as_str()),
        None,
        false,
    )
}

/// What the session is actually running: the transcript's latest turn, else
/// the command's flags, else what was asked. Null when none of the three
/// says anything.
fn running_of(
    summary: &SessionSummary,
    live: &EngineSession,
    found: Option<&transcripts::Transcript>,
) -> Option<SessionRuntime> {
    let (model, effort) = found.map(transcripts::runtime).unwrap_or((None, None));
    let resolved = decisions::resolve_runtime(
        live.command.as_deref(),
        live.conversation_agent.as_deref(),
        model.as_deref(),
        effort.as_deref(),
        summary.model.as_deref(),
        summary.effort.as_deref(),
    );
    if resolved.agent.is_none() && resolved.model.is_none() && resolved.effort.is_none() {
        return None;
    }
    Some(SessionRuntime {
        agent: resolved.agent,
        model: resolved.model,
        model_basis: resolved.model_basis.map(str::to_string),
        effort: resolved.effort,
        effort_basis: resolved.effort_basis.map(str::to_string),
    })
}

/// An opencode session's last reply, from the engine: opencode keeps no
/// transcript file. An engine that cannot be asked, or one that has no such
/// session, is a null.
fn opencode_excerpt<C, I>(ctx: &AppContext<C, I>, engine_session_id: &str) -> Option<String>
where
    C: Clock,
    I: IdFactory,
{
    let engine = ctx.engine.as_ref()?;
    let found = engine.session_replies(engine_session_id, 1).ok()??;
    found.1.last().map(|(text, _at)| transcripts::excerpt(text))
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

fn approval_of(approval: &EngineApproval) -> SessionApproval {
    SessionApproval {
        question: approval.question.clone(),
        command_excerpt: approval.command_excerpt.clone(),
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
        deadline_seconds: approval.deadline_seconds,
        deadline_at: approval.deadline_at.clone(),
        detected_at: (!approval.detected_at.is_empty()).then(|| approval.detected_at.clone()),
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
/// in both id fields and null for every declared field. Its work item, when
/// the engine named one Vogt knows, contributes its title and state.
fn unlinked_summary(
    view: &dyn crate::storage::interface::ReadView,
    session: &EngineSession,
) -> Result<SessionSummary, VogtError> {
    let work_item = match session.work_item.as_deref() {
        Some(reference) => view.work_item_by_ref(reference)?,
        None => None,
    };
    let mut summary = SessionSummary {
        id: session.id.clone(),
        engine_session_id: session.id.clone(),
        linked: false,
        project: None,
        work_item: session.work_item.clone(),
        work_item_title: work_item.as_ref().map(|item| item.title.clone()),
        work_item_state: work_item.map(|item| item.state.to_string()),
        actor: None,
        cwd: session.cwd.clone(),
        template: None,
        model: None,
        effort: None,
        reason: None,
        started_at: session
            .created_at
            .as_deref()
            .and_then(|text| crate::core::from_iso(text).ok()),
        stopped_at: None,
        name: None,
        command: None,
        exit_code: None,
        activity_changed_at: None,
        conversation_agent: None,
        activity: None,
        alive: Some(session.alive),
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        blocked: None,
        last_reply_excerpt: None,
        hibernation: None,
        keep_awake: None,
        autopilot: None,
        autopilot_nudges: None,
        role: None,
        stopped_by: None,
        stop_reason: None,
        permission_mode: None,
        running: None,
        resources: None,
        conversation_id: None,
    };
    apply_live(&mut summary, session);
    Ok(summary)
}

/// The screen the caller asked about. `named` is the id they used, kept as
/// given; the engine's own id is reported beside it.
fn screen_result(named: &str, engine_id: &str, screen: &EngineScreen) -> SessionScreenResult {
    let cursor = match (screen.cursor_row, screen.cursor_col) {
        (Some(row), Some(col)) => Some(SessionCursor { row, col }),
        _ => None,
    };
    SessionScreenResult {
        id: named.to_string(),
        engine_session_id: engine_id.to_string(),
        cols: screen.cols,
        rows: screen.rows,
        lines: screen.lines.clone(),
        cursor,
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

/// A missing screen is either an engine without the route, or a session the
/// engine no longer holds. The second lookup tells them apart.
fn require_screen(
    engine: &EngineClient,
    screen: Option<EngineScreen>,
    engine_id: &str,
    named: &str,
) -> Result<EngineScreen, VogtError> {
    if let Some(screen) = screen {
        return Ok(screen);
    }
    if engine.get_session(engine_id)?.is_some() {
        return Err(VogtError::EngineUnavailable(
            "the session engine does not support screen yet (it has no GET              /api/sessions/{id}/screen route); upgrade the engine, or read the              output with session.log_tail"
                .to_string(),
        ));
    }
    Err(VogtError::NotFound(format!(
        "the engine has no live session {}",
        py_repr(named)
    )))
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
            None => unlinked_summary(&ctx.declared.read()?, &current)?,
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
            session: unlinked_summary(&ctx.declared.read()?, &woken)?,
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
    let env = session_env(ctx, &session.id, &secret, None);
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
    work_item: Option<&str>,
) -> Vec<(String, String)>
where
    C: Clock,
    I: IdFactory,
{
    let mut env = vec![
        ("VOGT_HTTP_TOKEN".to_string(), secret.to_string()),
        ("VOGT_SESSION_ID".to_string(), session_id.to_string()),
    ];
    if let Some(work_item) = work_item.filter(|item| !item.is_empty()) {
        env.push(("VOGT_WORK_ITEM".to_string(), work_item.to_string()));
    }
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use crate::adapters::engine::EngineClient;
    use crate::application::context::{build_context, write_of};
    use crate::application::writes::{audited_write, WriteOutcome};
    use crate::auth;
    use crate::core::{
        ActorKind, CodingSession, Moment, Principal, Project, StepClock, Token, TokenKind,
    };
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};
    use crate::with_ctx;

    use super::{
        answers_as_person, hibernate, input, wake, HibernateParams, InputParams, WakeParams,
    };

    /// An engine that records every request and answers from `script`: each GET
    /// of a session pops the next payload, and a write echoes its own body so
    /// the client reads it back as the session's new state.
    fn engine(
        script: Vec<Vec<u8>>,
        seen: Arc<Mutex<Vec<(String, String, String)>>>,
    ) -> EngineClient {
        let remaining = Arc::new(Mutex::new(script));
        EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |path, _, body, method| {
                seen.lock().unwrap().push((
                    method.to_string(),
                    path.to_string(),
                    String::from_utf8_lossy(body).to_string(),
                ));
                let answer = if method == "GET" {
                    remaining
                        .lock()
                        .unwrap()
                        .pop()
                        .unwrap_or_else(|| b"null".to_vec())
                } else {
                    body.to_vec()
                };
                (200, answer)
            })),
        )
    }

    #[allow(clippy::type_complexity)]
    fn context(
        name: &str,
        principal: Principal,
        token: Option<Token>,
        config_extra: impl FnOnce(&mut crate::config::VogtConfig),
        script: Vec<Vec<u8>>,
    ) -> (
        std::path::PathBuf,
        crate::application::context::Built,
        Arc<Mutex<Vec<(String, String, String)>>>,
    ) {
        let dir = std::env::temp_dir().join(format!("vogt-ses-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut config = crate::config::VogtConfig {
            data_dir: dir.clone(),
            ..crate::config::VogtConfig::default()
        };
        config_extra(&mut config);
        let built = build_context(
            config,
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
            None,
            token,
            Some(engine(script, Arc::clone(&seen))),
            None,
            None,
        )
        .unwrap();
        (dir, built, seen)
    }

    fn person() -> Principal {
        Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()
    }

    fn agent(identity: &str) -> Principal {
        Principal::new(identity, ActorKind::Agent, "agent").unwrap()
    }

    fn live(id: &str) -> Vec<u8> {
        format!(r#"{{"id":"{id}","alive":true,"activity":"idle","role":"worker"}}"#).into_bytes()
    }

    fn hibernated(id: &str) -> Vec<u8> {
        format!(
            r#"{{"id":"{id}","alive":false,"activity":"hibernated","role":"worker","conversation":{{"id":"conv-1","agent":"claude"}}}}"#
        )
        .into_bytes()
    }

    /// Record a Vogt session and one token for its actor, and return the token's id.
    fn record_session(
        built: &crate::application::context::Built,
        id: &str,
        engine_id: &str,
    ) -> String {
        with_ctx!(built, |ctx| {
            let mut write = write_of(ctx);
            let id = id.to_string();
            let engine_id = engine_id.to_string();
            let token_id = "tok_old".to_string();
            let kept = token_id.clone();
            audited_write(&mut write, "session.start", "test", move |txn, actor| {
                txn.insert_project(&Project::new(
                    "prj_1",
                    "proj",
                    "Proj",
                    "/work",
                    Moment::from_unix(1_700_000_000, 0),
                ))?;
                txn.insert_session(&CodingSession {
                    id: id.clone(),
                    engine_session_id: engine_id,
                    project_id: "prj_1".to_string(),
                    work_item_id: None,
                    actor_id: actor.id.clone(),
                    cwd: "/work".to_string(),
                    template: None,
                    model: None,
                    effort: None,
                    reason: "test".to_string(),
                    started_at: Moment::from_unix(1_700_000_000, 0),
                    stopped_at: None,
                })?;
                txn.insert_token(
                    &Token {
                        id: token_id,
                        actor_id: actor.id.clone(),
                        actor_identity_ref: Some(actor.identity_ref.clone()),
                        name: "session token".to_string(),
                        scopes: vec!["read".to_string()],
                        kind: TokenKind::Api,
                        created_at: Moment::from_unix(1_700_000_000, 0),
                        expires_at: None,
                        last_used_at: None,
                        revoked_at: None,
                        revoked_reason: None,
                    },
                    "hash-old",
                )?;
                Ok(WriteOutcome::new(
                    (),
                    "session",
                    &id,
                    json!({}),
                    "session.started",
                    json!({}),
                ))
            })
            .unwrap();
            kept
        })
    }

    fn tokens_of(built: &crate::application::context::Built, actor_ref: &str) -> Vec<Token> {
        with_ctx!(built, |ctx| {
            let view = ctx.declared.read().unwrap();
            let actor = view.actor_by_identity(actor_ref).unwrap().unwrap();
            view.tokens_for_actor(&actor.id, true).unwrap()
        })
    }

    #[test]
    fn a_person_answers_as_a_person_and_an_agent_does_not() {
        let (_dir, built, seen) = context("person", person(), None, |_| {}, vec![live("eng-1")]);
        record_session(&built, "ses_1", "eng-1");
        with_ctx!(&built, |ctx| {
            assert!(answers_as_person(ctx).unwrap());
            input(
                ctx,
                &InputParams {
                    id: "ses_1".to_string(),
                    text: Some("hello".to_string()),
                    keys: None,
                    submit: true,
                    reason: "typing".to_string(),
                    confirm: false,
                    wake_timeout_s: 120,
                },
            )
            .unwrap();
        });
        let calls = seen.lock().unwrap().clone();
        let writes: Vec<_> = calls
            .iter()
            .filter(|(_, _, body)| body.contains("person"))
            .collect();
        assert!(
            writes
                .iter()
                .all(|(_, _, body)| body.contains(r#""person":true"#)),
            "{calls:?}"
        );
        drop(built);

        let (_dir, built, seen) = context(
            "agent",
            agent("agent:worker"),
            None,
            |_| {},
            vec![live("eng-1")],
        );
        record_session(&built, "ses_1", "eng-1");
        with_ctx!(&built, |ctx| {
            assert!(!answers_as_person(ctx).unwrap());
        });
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn the_engine_credential_is_not_a_person_even_when_human() {
        let (_dir, built, _) = context(
            "engine-actor",
            Principal::new("agent:vogt-engine", ActorKind::Human, "engine").unwrap(),
            None,
            |_| {},
            vec![],
        );
        with_ctx!(&built, |ctx| assert!(!answers_as_person(ctx).unwrap()));

        let secret_path = std::env::temp_dir().join(format!("vogt-secret-{}", std::process::id()));
        std::fs::write(&secret_path, "the-stack-secret\n").unwrap();
        let (_dir, built, _) = context(
            "stack",
            person(),
            None,
            |config| {
                config.bootstrap_core_token_file = Some(secret_path.clone());
            },
            vec![],
        );
        // The presented token is the one the stack secret hashes to.
        with_ctx!(&built, |ctx| {
            let mut write = write_of(ctx);
            audited_write(&mut write, "token.issue", "test", |txn, actor| {
                txn.insert_token(
                    &Token {
                        id: "tok_stack".to_string(),
                        actor_id: actor.id.clone(),
                        actor_identity_ref: Some(actor.identity_ref.clone()),
                        name: "stack".to_string(),
                        scopes: vec!["admin".to_string()],
                        kind: TokenKind::Api,
                        created_at: Moment::from_unix(1_700_000_000, 0),
                        expires_at: None,
                        last_used_at: None,
                        revoked_at: None,
                        revoked_reason: None,
                    },
                    &auth::hash_token("the-stack-secret"),
                )?;
                Ok(WriteOutcome::new(
                    (),
                    "token",
                    "tok_stack",
                    json!({}),
                    "token.issued",
                    json!({}),
                ))
            })
            .unwrap();
        });
        // Rebuilt over the same store, presenting that token.
        let presented = with_ctx!(&built, |ctx| ctx
            .declared
            .read()
            .unwrap()
            .token_by_hash(&auth::hash_token("the-stack-secret"))
            .unwrap()
            .unwrap());
        let dir = with_ctx!(&built, |ctx| ctx.config.data_dir.clone());
        drop(built);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let rebuilt = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                bootstrap_core_token_file: Some(secret_path),
                ..crate::config::VogtConfig::default()
            },
            Some(person()),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
            None,
            Some(presented),
            Some(engine(vec![], Arc::clone(&seen))),
            None,
            None,
        )
        .unwrap();
        with_ctx!(&rebuilt, |ctx| assert!(!answers_as_person(ctx).unwrap()));
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("vogt-secret-{}", std::process::id())),
        );
    }

    #[test]
    fn hibernate_revokes_the_sessions_tokens() {
        let (_dir, built, seen) = context("hibernate", person(), None, |_| {}, vec![live("eng-1")]);
        let old = record_session(&built, "ses_1", "eng-1");
        with_ctx!(&built, |ctx| {
            hibernate(
                ctx,
                &HibernateParams {
                    id: "ses_1".to_string(),
                    reason: "freeing memory".to_string(),
                    allow_shell: false,
                },
            )
            .unwrap();
        });
        let tokens = tokens_of(&built, "local:ada");
        let revoked = tokens.iter().find(|token| token.id == old).unwrap();
        assert_eq!(
            revoked.revoked_reason.as_deref(),
            Some("hibernated: freeing memory")
        );
        assert!(revoked.revoked_at.is_some());
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, path, _)| path.ends_with("/hibernate")));
    }

    #[test]
    fn wake_mints_a_token_and_revokes_the_older_ones() {
        let (_dir, built, seen) = context(
            "wake",
            person(),
            None,
            |_| {},
            vec![hibernated("eng-1"), hibernated("eng-1")],
        );
        let old = record_session(&built, "ses_1", "eng-1");
        with_ctx!(&built, |ctx| {
            wake(
                ctx,
                &WakeParams {
                    id: "ses_1".to_string(),
                    reason: "back to work".to_string(),
                },
            )
            .unwrap();
        });
        let tokens = tokens_of(&built, "local:ada");
        let revoked = tokens.iter().find(|token| token.id == old).unwrap();
        assert_eq!(
            revoked.revoked_reason.as_deref(),
            Some("superseded on wake: back to work")
        );
        let minted: Vec<_> = tokens
            .iter()
            .filter(|token| token.revoked_at.is_none())
            .collect();
        assert_eq!(minted.len(), 1, "{tokens:?}");
        assert!(minted[0].name.contains("ses_1"));
        let calls = seen.lock().unwrap().clone();
        let wake_call = calls
            .iter()
            .find(|(_, path, _)| path.ends_with("/wake"))
            .unwrap();
        assert!(wake_call.2.contains("VOGT_HTTP_TOKEN"), "{}", wake_call.2);
        assert!(wake_call.2.contains("VOGT_SESSION_ID"), "{}", wake_call.2);
    }

    #[test]
    fn waking_a_stopped_session_is_refused_and_revokes_nothing() {
        let (_dir, built, _) =
            context("stopped", person(), None, |_| {}, vec![hibernated("eng-1")]);
        let old = record_session(&built, "ses_1", "eng-1");
        // Marked stopped directly, not through session.stop: stopping revokes
        // the token itself, and this test is about wake refusing before it
        // touches tokens.
        with_ctx!(&built, |ctx| {
            let mut write = write_of(ctx);
            audited_write(&mut write, "session.stop", "test", |txn, _actor| {
                txn.set_session_stopped("ses_1", Moment::from_unix(1_700_000_001, 0))?;
                Ok(WriteOutcome::new(
                    (),
                    "session",
                    "ses_1",
                    json!({}),
                    "session.stopped",
                    json!({}),
                ))
            })
            .unwrap();
            let error = wake(
                ctx,
                &WakeParams {
                    id: "ses_1".to_string(),
                    reason: "no".to_string(),
                },
            )
            .unwrap_err();
            assert!(matches!(error, VogtError::Conflict(_)), "{error:?}");
        });
        let tokens = tokens_of(&built, "local:ada");
        assert!(tokens
            .iter()
            .find(|token| token.id == old)
            .unwrap()
            .revoked_at
            .is_none());
    }
}
