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

use std::collections::{BTreeMap, HashMap};
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
use crate::application::brief::{self, ComposedBrief, LandingFields, WhyResult, WorkBrief};
use crate::application::context::{write_of, AppContext};
use crate::application::dispatch as dispatch_profiles;
use crate::application::policy_conflicts;
use crate::application::resolve;
use crate::application::session_commit;
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
const SESSION_RENAME: &str = "session.rename";
const SESSION_REMOVE: &str = "session.remove";
const SESSION_TOKEN: &str = "session.token";

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
    let parsed =
        StartSessionParams {
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
            purpose: optional_string(&params, "purpose")?,
            review_of: optional_string(&params, "review_of")?,
            from_handoff: optional_string(&params, "from_handoff")?,
            budget: match params.get("budget") {
                None | Some(Value::Null) => None,
                Some(raw) => Some(serde_json::from_value(raw.clone()).map_err(|error| {
                    VogtError::InvalidRequest(format!("invalid budget: {error}"))
                })?),
            },
            worktree: optional_bool(&params, "worktree")?,
            repo_map: optional_bool(&params, "repo_map")?,
            explicit_null: PROFILE_FIELDS
                .into_iter()
                .filter(|field| matches!(params.get(*field), Some(Value::Null)))
                .collect(),
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
        mode: optional_string(&params, "mode")?.unwrap_or_else(|| "digest".to_string()),
        fields: optional_string_list(&params, "fields")?.unwrap_or_default(),
        started_by_session: optional_string(&params, "started_by_session")?,
        states: optional_string_list(&params, "states")?,
        stall_after_minutes: optional_i64(&params, "stall_after_minutes", 10)?,
        include_hidden: optional_bool(&params, "include_hidden")?.unwrap_or(false),
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
        mode: optional_string(&params, "mode")?.unwrap_or_else(|| "digest".to_string()),
        fields: optional_string_list(&params, "fields")?.unwrap_or_default(),
        states: optional_string_list(&params, "states")?,
        attention: optional_string_list(&params, "attention")?,
        limit: optional_i64(&params, "limit", 50)?,
        // Python's default is 0: no screen tail unless asked (design C1).
        screen_lines: optional_i64(&params, "screen_lines", 0)?,
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
        screen: optional_bool(&params, "screen")?.unwrap_or(false),
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
    /// The dispatch purpose (WI-1323). Unset, the start takes no profile.
    pub purpose: Option<String>,
    /// The work item a review is of (Python `review_of`). A review's brief
    /// checks the item's diff rather than carrying the work on.
    pub review_of: Option<String>,
    /// Open on the work item this names, resumed from its latest hand-off
    /// (Python `from_handoff`). Sugar for `work_item`; refused when the item
    /// has no hand-off comment.
    pub from_handoff: Option<String>,
    /// The limits the start asks for. Unset, the default rule applies.
    pub budget: Option<SessionBudget>,
    /// Cut a worktree for the session. Unset, the profile decides.
    pub worktree: Option<bool>,
    /// Draw a repo map into the brief. Unset, the profile decides.
    pub repo_map: Option<bool>,
    /// Profile fields the caller sent as an explicit null. Python counts these
    /// in `model_fields_set`: they are overrides, and the profile leaves them.
    pub explicit_null: Vec<&'static str>,
}

/// The start fields a dispatch profile fills, in Python's override order.
const PROFILE_FIELDS: [&str; 7] = [
    "template",
    "model",
    "effort",
    "budget",
    "worktree",
    "repo_map",
    "autopilot",
];

/// The limits a start carries (Python `SessionBudgetParams`). Each field is one
/// limit; a field left out is not set.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionBudget {
    pub turns: Option<i64>,
    pub context_tokens: Option<i64>,
    pub wall_clock_minutes: Option<i64>,
    pub session_wall_clock_minutes: Option<i64>,
}

/// The engine's `budget` object: the fields that are set, and `{}` for a
/// budget that names no limit, which the engine reads as "no limits".
fn budget_wire(budget: &SessionBudget) -> Value {
    let mut wire = serde_json::Map::new();
    for (key, value) in [
        ("turns", budget.turns),
        ("context_tokens", budget.context_tokens),
        ("wall_clock_minutes", budget.wall_clock_minutes),
        (
            "session_wall_clock_minutes",
            budget.session_wall_clock_minutes,
        ),
    ] {
        if let Some(value) = value {
            wire.insert(key.to_string(), json!(value));
        }
    }
    Value::Object(wire)
}

/// Python's `_budget`: the limits a start carries. Asked explicitly, that
/// object is the answer, even an empty one. Asked nothing, a worker running an
/// agent template on a task gets the default, plus the project's whole-session
/// wall clock when its policy sets one. Any other start gets none.
fn effective_budget(
    params: &StartSessionParams,
    policy: &crate::core::AgentPolicy,
) -> Option<SessionBudget> {
    if let Some(budget) = &params.budget {
        return Some(budget.clone());
    }
    let has_task = params
        .task
        .as_deref()
        .is_some_and(|task| !task.trim().is_empty());
    if params.template.is_some() && has_task && params.role == "worker" {
        return Some(SessionBudget {
            turns: Some(120),
            context_tokens: None,
            wall_clock_minutes: Some(90),
            session_wall_clock_minutes: policy.session_wall_clock_minutes,
        });
    }
    None
}

/// Python's `_worktree_branch`: `<work item or project>-<session>`, with the
/// head flattened to one path component.
fn worktree_branch(subject: &Subject, session_id: &str) -> String {
    let head = subject
        .work_item_ref
        .as_deref()
        .unwrap_or(subject.project_slug.as_str());
    let mut flat = String::new();
    let mut in_gap = false;
    for character in head.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            flat.push(character);
            in_gap = false;
        } else if !in_gap {
            flat.push('-');
            in_gap = true;
        }
    }
    let flat = flat.trim_matches('-');
    let flat = if flat.is_empty() { "session" } else { flat };
    let tail = session_id.strip_prefix("ses_").unwrap_or(session_id);
    format!("{flat}-{tail}")
}

/// Python's `_caller_merges`: whether this start is oversight's own landing
/// work (WI-1841).
///
/// On a project where oversight merges, an oversight session that starts a
/// mechanical session to land a branch is doing what the policy says; its
/// "merge" is not overridden to "do not merge".
fn caller_merges(
    policy: &crate::core::AgentPolicy,
    params: &StartSessionParams,
    started_by_kind: Option<&str>,
) -> bool {
    LandingFields::of(policy).is_some_and(|landing| landing.merge_by == "oversight")
        && started_by_kind == Some("oversight")
        && params.purpose.as_deref() == Some("mechanical")
}

/// Python's `_effective_policy`: the landing rule a started worker runs
/// under, and what it overrode.
///
/// Returned on `session.start` so the caller sees, before the worker runs,
/// that its task said one thing and the policy another (WI-1832).
fn effective_policy(
    policy: &crate::core::AgentPolicy,
    task: Option<&str>,
    caller_merges: bool,
) -> Option<EffectivePolicy> {
    let landing = LandingFields::of(policy)?;
    let overrides =
        policy_conflicts::task_policy_conflicts(task.unwrap_or(""), Some(&landing), caller_merges);
    Some(EffectivePolicy {
        base_branch: landing.base_branch,
        lands_by: landing.worker_lands_by,
        merge_by: landing.merge_by,
        merge_mode: landing.merge_mode,
        deploy_by: landing.deploy_by,
        overrides,
    })
}

/// Python's `_with_item_base`: the project as this start sees it, its policy
/// on the item's base.
///
/// A project's landing rule names one base, but a long-lived integration
/// line — the Rust port on `rust-core` — has its own (WI-1841). The
/// initiative's base applies to its items, and a task that names a known
/// long-lived branch as its base is taken at its word. The brief, the
/// worktree and the conflict check all read the result, so they agree.
fn with_item_base(
    view: &impl ReadView,
    mut project: crate::core::Project,
    item: Option<&WorkItem>,
    task: Option<&str>,
) -> Result<crate::core::Project, VogtError> {
    let Some(landing) = LandingFields::of(&project.agent_policy) else {
        return Ok(project);
    };
    let mut base = landing.base_branch.clone();
    if let Some(initiative_id) = item.and_then(|item| item.initiative_id.as_deref()) {
        let mut keys = vec![initiative_id.to_string()];
        if let Some(initiative) = view.initiative_by_id(initiative_id)? {
            keys.push(initiative.slug);
        }
        if let Some(found) = keys
            .iter()
            .find_map(|key| landing.initiative_bases.get(key))
        {
            base = found.clone();
        }
    }
    if let Some(named) = policy_conflicts::task_named_base(task.unwrap_or(""), Some(&landing)) {
        base = named;
    }
    project.agent_policy = project.agent_policy.with_base(&base);
    Ok(project)
}

/// Python's `_worktree_base`: the project's integration branch, else `main`.
fn worktree_base(policy: &crate::core::AgentPolicy) -> String {
    let stated = policy.base_branch.trim();
    if stated.is_empty() {
        "main".to_string()
    } else {
        stated.to_string()
    }
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
    /// `digest` (default) or `full`.
    pub mode: String,
    /// Extra SessionSummary attribute names on each digest row.
    pub fields: Vec<String>,
    /// Only sessions this one started: its engine id or `ses_…` id.
    pub started_by_session: Option<String>,
    /// Activities to keep; `None` is the live states, empty is every state.
    pub states: Option<Vec<String>>,
    pub stall_after_minutes: i64,
    /// Also ask the engine for the sessions it hides (WI-1289).
    pub include_hidden: bool,
}

impl Default for ListSessionsParams {
    /// Python's `ListSessionsParams()` defaults.
    fn default() -> Self {
        Self {
            project: None,
            work_item: None,
            include_stopped: false,
            limit: 50,
            offset: 0,
            order: "started".to_string(),
            mode: "digest".to_string(),
            fields: Vec::new(),
            started_by_session: None,
            states: None,
            stall_after_minutes: 10,
            include_hidden: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SweepSessionsParams {
    pub project: Option<String>,
    /// `digest` (default) or `full`.
    pub mode: String,
    pub fields: Vec<String>,
    pub states: Option<Vec<String>>,
    /// Attention classes to keep; `needs_you` expands to its classes.
    pub attention: Option<Vec<String>>,
    /// Digest rows returned at most; counts cover every session.
    pub limit: i64,
    pub screen_lines: i64,
    pub stall_after_minutes: i64,
}

impl Default for SweepSessionsParams {
    /// Python's `SweepSessionsParams()` defaults.
    fn default() -> Self {
        Self {
            project: None,
            mode: "digest".to_string(),
            fields: Vec::new(),
            states: None,
            attention: None,
            limit: 50,
            screen_lines: 0,
            stall_after_minutes: 10,
        }
    }
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
    /// True returns the full screen; false (default) the digest row plus an
    /// 8-line screen tail.
    pub screen: bool,
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
    /// What the session is blocked on, in its words. Python's `SessionBlocked`
    /// names this `blocker`, so the wire does too.
    pub blocker: String,
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
    /// The dispatch purpose the start named (WI-1323), when it named one.
    pub purpose: Option<String>,
    /// The engine id of the session that started this one, when a session did.
    pub started_by_session: Option<String>,
    /// Whether the starter was a person, or a session in the oversight or
    /// worker role at the time.
    pub started_by_kind: Option<String>,
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
    /// When an autopilot session declared itself done (`AUTOPILOT: DONE`)
    /// this turn (WI-1518); cleared when the next turn starts.
    pub done_at: Option<String>,
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

/// Python's `SessionResult`. Start, hibernate and wake carry the digest row as
/// well as the summary (design C1); every other session write leaves it null.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionResult {
    pub session: SessionSummary,
    pub digest: Option<SessionDigest>,
}

impl SessionResult {
    /// A result with no digest row.
    fn plain(session: SessionSummary) -> Self {
        Self {
            session,
            digest: None,
        }
    }

    /// Python's `_as_digest`: the summary stays, and the digest row is what an
    /// MCP caller keeps.
    fn digested(session: SessionSummary) -> Self {
        let digest = Some(digest_of(&session, None, None, &[], None));
        Self { session, digest }
    }
}

/// One line of session state (design C1), Python's `SessionDigest`, fields in
/// its declaration order.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SessionDigest {
    pub id: String,
    pub name: Option<String>,
    pub work_item: Option<String>,
    pub activity: Option<String>,
    pub attention: Option<String>,
    pub ready: Option<bool>,
    /// First 120 characters of the blocked report.
    pub blocked_reason: Option<String>,
    /// The blocked report in full, when it is longer than 120 characters.
    pub blocked_detail: Option<String>,
    pub turn_started_at: Option<String>,
    pub done_at: Option<String>,
    /// First 120 characters of the last reply.
    pub last_reply_excerpt: Option<String>,
    pub template: Option<String>,
    /// Null until the engine reports a brief path.
    pub brief_file: Option<String>,
    /// The question or dialog the session is showing (WI-1871).
    pub question: Option<String>,
    pub question_kind: Option<String>,
    pub options: Option<Vec<String>>,
    /// The engine's economics line, when it has metered the session.
    pub budget: Option<String>,
    /// Reserved for WI-956.
    pub supervisor: Option<String>,
    /// SessionSummary attributes named in `fields`, when asked.
    pub extra: Option<serde_json::Map<String, Value>>,
}

/// `session.start`'s result: Python's `SessionResult` with its
/// `effective_policy` (WI-1832), which only a start fills.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StartSessionResult {
    pub session: SessionSummary,
    /// The digest row (design C1).
    pub digest: Option<SessionDigest>,
    /// On `session.start` for a project with a landing rule: the rule the
    /// worker runs under and any task lines it overrode.
    pub effective_policy: Option<EffectivePolicy>,
}

/// The landing rule a started session runs under (WI-1832).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EffectivePolicy {
    pub base_branch: String,
    pub lands_by: String,
    pub merge_by: String,
    pub merge_mode: String,
    pub deploy_by: String,
    /// Where the task contradicted this policy, one line per axis (PR vs
    /// push only, merge, base branch, deploy). The policy wins and the brief
    /// says so; empty when the task agreed or set none.
    pub overrides: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
    /// Digest rows (design C1). `mode` defaults to `digest`, so the ordinary
    /// list returns these and leaves `sessions` empty; `mode=full` does the
    /// reverse.
    #[serde(default)]
    pub digests: Vec<SessionDigest>,
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
    /// The full summary on `mode=full`; null on a digest row.
    pub session: Option<SessionSummary>,
    /// The digest row on `mode=digest` (the default); null on `mode=full`.
    pub digest: Option<SessionDigest>,
    pub screen_tail: Vec<String>,
    pub ready: Option<bool>,
    /// When the session's blocked report was made, if it has one (WI-1834).
    pub blocked_since: Option<String>,
    /// How old the blocked report is, in seconds.
    pub blocked_age_s: Option<i64>,
    /// The blocked report predates the session's current turn, so it is not
    /// counted in `needs_you`.
    pub blocked_stale: bool,
    /// On a `done` row bound to a work item: its AUTOPILOT: DONE claim checked
    /// against git and the forge (WI-1835), noted on the item once per DONE.
    pub claim_check: Option<crate::application::models::ClaimCheck>,
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
    /// The full screen, when `screen=true` asked for it.
    pub screen: Option<SessionScreenResult>,
    /// The digest row, unless `screen=true`; null when it cannot be read.
    pub digest: Option<SessionDigest>,
    /// The last 8 non-blank screen lines, unless `screen=true`.
    pub screen_tail: Vec<String>,
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
    /// The digest of the session just typed into, when it can be read.
    pub digest: Option<SessionDigest>,
}

// -- the operations -----------------------------------------------------------

/// Open a terminal for a work item or a project.
pub fn start_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<StartSessionResult, VogtError>
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
    // Resolved after the guards above: a profile carries neither a role nor a
    // permission posture, so naming a purpose cannot raise a start past what
    // the caller could ask for directly (WI-1323).
    let (resolved, read_only, overrides) = resolve_purpose(ctx, params)?;
    let params = &resolved;
    // After the profile fills the model, before anything reaches the engine.
    refuse_forbidden_model(params)?;
    let (started_by_session, started_by_kind) = started_by(ctx, engine)?;
    let mut writing = write_of(ctx);
    let session_id = fresh(&writing, "ses");
    let subject = subject_of(ctx, params, &session_id)?;
    let caller_merges = caller_merges(&subject.policy, params, started_by_kind.as_deref());
    let actor_ref = format!("agent:session:{session_id}");
    let scopes = session_scopes(ctx)?;
    let (secret, token_hash) = minted_token()?;
    // The project's policy on the item's base (WI-1841), as the subject
    // resolved it: the brief, the worktree and the conflict check agree.
    let policy = subject.policy.clone();
    let budget = effective_budget(params, &policy);
    // The audit records what the start asked for (Python `params.worktree`),
    // so a start with no purpose and no value shows null, not false.
    let audit_worktree = params.worktree;
    let audit_repo_map = params.repo_map;
    let worktree = params.worktree.unwrap_or(false);
    let mut spec = build_spec(ctx, params, &subject, &secret, read_only, caller_merges)?;
    spec.budget = budget.as_ref().map(budget_wire);
    if worktree {
        spec.worktree = Some(json!({
            "branch": worktree_branch(&subject, &session_id),
            "base_ref": worktree_base(&policy),
        }));
    }
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
    let cwd = match (params.resume.is_some() || worktree, started.cwd.as_str()) {
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
        // Python's `_review_subject` records the resolved item, not the input.
        let review_of = params
            .review_of
            .as_ref()
            .and_then(|_| subject.work_item_ref.clone());
        let project_slug = subject.project_slug.clone();
        let is_scratch = subject.is_scratch;
        let resume = params.resume.clone();
        let role = params.role.clone();
        let permission_mode = params.permission_mode.clone();
        let autopilot = spec.autopilot;
        let overlay_template = ctx.config.branch_binding_template.clone();
        let purpose = params.purpose.clone();
        let started_by = started_by_session.clone();
        let started_by_kind = started_by_kind.clone();
        let audit_overrides = overrides.clone();
        let audit_budget = budget.as_ref().map(budget_wire);
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
                    purpose: purpose.clone(),
                    started_by_session: started_by.clone(),
                    started_by_kind: started_by_kind.clone(),
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
                        "purpose": purpose,
                        "overrides": audit_overrides,
                        "budget": audit_budget,
                        "review_of": review_of,
                        "worktree": audit_worktree,
                        "repo_map": audit_repo_map,
                        "started_by_session": started_by_session,
                        "started_by_kind": started_by_kind,
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
    let session = summarize(ctx, &stored?, Some(&started))?;
    Ok(StartSessionResult {
        digest: Some(digest_of(&session, None, None, &[], None)),
        session,
        effective_policy: effective_policy(&policy, params.task.as_deref(), caller_merges),
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
    Ok(SessionResult::plain(summarize(ctx, &stopped, None)?))
}

/// List sessions, live activity merged in. The engine being down is a fact
/// about the rows, not a failure of the list.
///
/// Python's `list_sessions`: the joined set (Vogt's links plus live sessions
/// the engine holds that Vogt never linked) is filtered (digest mode; full
/// mode returns every state), then ordered, then sliced, and `total` /
/// `truncated` say whether the page is the whole answer (WI-1266).
pub fn list_sessions<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
) -> Result<SessionListResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    // The engine's count of rows it hid before this list (WI-1289). Zero when
    // no engine answered, or one older than the object response said nothing.
    let (live, detail, hidden_count) = match ctx.engine.as_ref() {
        None => (
            Vec::new(),
            Some("no session engine is configured (VOGT_ENGINE_URL is unset)".to_string()),
            0,
        ),
        Some(engine) => match engine.list_sessions_counted(params.include_hidden) {
            // Kept as a Vec: the engine's order is the order unlinked rows
            // append in, and a HashMap would shuffle it.
            Ok((rows, hidden)) => (rows, None, i64::try_from(hidden).unwrap_or(i64::MAX)),
            Err(VogtError::EngineUnavailable(text)) => (Vec::new(), Some(text), 0),
            Err(error) => return Err(error),
        },
    };
    let (joined, declared_overflow) = rows_of(ctx, params, &live, detail.as_deref())?;
    // Python reads the clock once, after the rows are joined: the attention
    // reference for the state filter and the digest rows.
    let now = super::now_of(&ctx.clock);
    let stall = params.stall_after_minutes.saturating_mul(60);
    let full = params.mode == "full";
    let mut kept: Vec<SessionSummary> = joined
        .into_iter()
        .filter(|summary| {
            full || kept_in(
                summary,
                Some(&attention_of(summary, now, stall)),
                params.states.as_deref(),
            )
        })
        .collect();
    if params.order == "rss" {
        // Stable: sessions with no sample keep their order, after the
        // measured ones.
        kept.sort_by(|left, right| {
            let rss =
                |row: &SessionSummary| row.resources.as_ref().map(|r| r.rss_bytes).unwrap_or(0);
            match (left.resources.is_none(), right.resources.is_none()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (true, true) => std::cmp::Ordering::Equal,
                (false, false) => rss(right).cmp(&rss(left)),
            }
        });
    }
    let mut total = kept.len() as i64;
    let offset = usize::try_from(params.offset).unwrap_or(0);
    let limit = usize::try_from(params.limit).unwrap_or(0);
    let page: Vec<SessionSummary> = kept.into_iter().skip(offset).take(limit).collect();
    // The declared half is fetched one past the operation's own maximum page.
    // When that ceiling is hit the counted total is a floor and the list is
    // partial even when this page holds everything fetched (WI-1266 review).
    let truncated = params.offset.saturating_add(page.len() as i64) < total || declared_overflow;
    if declared_overflow {
        total += 1;
    }
    if full {
        return Ok(SessionListResult {
            sessions: page,
            digests: Vec::new(),
            engine: detail,
            total,
            truncated,
            hidden_count,
        });
    }
    let digests = page
        .iter()
        .map(|summary| {
            let attention = attention_of(summary, now, stall);
            let economics = live
                .iter()
                .find(|row| row.id == summary.engine_session_id)
                .and_then(|row| row.economics.as_ref());
            digest_of(
                summary,
                Some(&attention),
                Some(summary.activity.as_deref() == Some("waiting-for-input")),
                &params.fields,
                budget_of(economics),
            )
        })
        .collect();
    Ok(SessionListResult {
        sessions: Vec::new(),
        digests,
        engine: detail,
        total,
        truncated,
        hidden_count,
    })
}

/// How many declared links `rows_of` reads: one past `session.list`'s own
/// maximum page (500), so a match beyond it shows as overflow.
const DECLARED_FETCH: i64 = 501;

/// Python's `_starter_engine_id`: the engine id a `started_by_session` filter
/// matches. The column stores the starter's engine session id; a caller may
/// name either that or the starter's `ses_…` id.
fn starter_engine_id(
    view: &dyn crate::storage::interface::ReadView,
    named: Option<&str>,
) -> Result<Option<String>, VogtError> {
    let Some(named) = named else {
        return Ok(None);
    };
    let session = match view.session_by_id(named)? {
        Some(session) => Some(session),
        None => view.session_by_engine_id(named)?,
    };
    Ok(Some(
        session
            .map(|session| session.engine_session_id)
            .unwrap_or_else(|| named.to_string()),
    ))
}

/// The rows `session.list` and `session.sweep` share (Python's `_rows`):
/// Vogt's links joined with what the engine reported, unlinked engine sessions
/// appended, and reply excerpts read from the agents' transcripts. The second
/// element is true when more declared links match than were fetched.
fn rows_of<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
    live: &[EngineSession],
    detail: Option<&str>,
) -> Result<(Vec<SessionSummary>, bool), VogtError>
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
    let starter = starter_engine_id(&view, params.started_by_session.as_deref())?;
    // The page is cut after the state filter, so the declared half has to be
    // the whole filtered set.
    let mut recorded = view.list_sessions_started_by(
        project_id.as_deref(),
        work_item_id.as_deref(),
        starter.as_deref(),
        params.include_stopped,
        DECLARED_FETCH,
        0,
    )?;
    let declared_overflow = recorded.len() as i64 == DECLARED_FETCH;
    if declared_overflow {
        recorded.truncate((DECLARED_FETCH - 1) as usize);
    }
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
    // Engine sessions Vogt never linked surface only when no filter is set,
    // and are not sliced here: slicing before the state filter drops a live
    // unlinked session behind a page of hibernated links (WI-1266).
    let unfiltered = project_id.is_none() && work_item_id.is_none() && starter.is_none();
    if unfiltered {
        let view = ctx.declared.read()?;
        for engine_session in live {
            // A hibernated session is asleep, not stopped: listed always. An
            // exited one is the engine's leftover, listed only when stopped
            // sessions were asked for.
            if !engine_session.alive && !engine_session.hibernated() && !params.include_stopped {
                continue;
            }
            if view.session_by_engine_id(&engine_session.id)?.is_none() {
                summaries.push(unlinked_summary(&view, engine_session)?);
            }
        }
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
            // A linked session with no recorded template: the engine's.
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
    Ok((summaries, declared_overflow))
}

// -- digest rows (design C1) --------------------------------------------------

/// Session states a digest listing keeps unless the caller names others.
/// Hibernated, stopped and exited stay in the sweep's counts, not its rows.
const DIGEST_LIVE_STATES: [&str; 8] = [
    "running", "idle", "waiting", "blocked", "approval", "stalled", "done", "unknown",
];

/// States a default listing drops.
const EXCLUDED_STATES: [&str; 3] = ["hibernated", "stopped", "exited"];

/// How much of a reply or a blocked report a digest row carries.
const DIGEST_CLIP: usize = 120;

/// Python's `_clip`: the first `DIGEST_CLIP` characters (code points).
fn clip(text: Option<&str>) -> Option<String> {
    text.map(|text| text.chars().take(DIGEST_CLIP).collect())
}

/// Python's truthiness of an optional string: `None` and `""` are both false.
fn truthy(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.is_empty())
}

/// Python's `_digest_of`: one line of state from a full summary.
///
/// `command` stays off the digest; `template` is the replacement, and
/// `brief_file` is null until the engine reports a path. `budget` is the
/// engine's economics line when the caller has one. `supervisor` stays null
/// until WI-956 exists.
pub(crate) fn digest_of(
    summary: &SessionSummary,
    attention: Option<&str>,
    ready: Option<bool>,
    fields: &[String],
    budget: Option<String>,
) -> SessionDigest {
    let mut extra = serde_json::Map::new();
    if !fields.is_empty() {
        let dumped = serde_json::to_value(summary).expect("a session summary serialises");
        for name in fields {
            if name == "command" {
                continue;
            }
            if let Some(value) = dumped.get(name) {
                extra.insert(name.clone(), value.clone());
            }
        }
    }
    let blocked = summary
        .blocked
        .as_ref()
        .map(|blocked| blocked.blocker.as_str());
    let approval = summary.approval.as_ref();
    SessionDigest {
        id: summary.id.clone(),
        name: summary.name.clone(),
        work_item: summary.work_item.clone(),
        activity: summary.activity.clone(),
        attention: attention.map(str::to_string),
        ready,
        blocked_reason: clip(blocked),
        blocked_detail: blocked
            .filter(|blocked| blocked.chars().count() > DIGEST_CLIP)
            .map(str::to_string),
        turn_started_at: summary.turn_started_at.clone(),
        done_at: summary.done_at.clone(),
        last_reply_excerpt: clip(summary.last_reply_excerpt.as_deref()),
        template: summary.template.clone(),
        brief_file: None,
        question: approval.map(|approval| approval.question.clone()),
        question_kind: approval.map(|approval| approval.kind.clone()),
        options: approval
            .filter(|approval| !approval.options.is_empty())
            .map(|approval| {
                approval
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect()
            }),
        budget,
        supervisor: None,
        extra: (!extra.is_empty()).then_some(extra),
    }
}

/// Python's `_budget_of`: the digest line from an engine economics reading,
/// or `None` when the engine has not measured the session (no reading, or an
/// empty one). Only turns and compactions reach the line.
fn budget_of(economics: Option<&serde_json::Map<String, Value>>) -> Option<String> {
    let economics = economics.filter(|economics| !economics.is_empty())?;
    // `_int_of`: an int that is not a bool, else 0.
    let int = |name: &str| economics.get(name).and_then(Value::as_i64).unwrap_or(0);
    let reading = crate::core::session_economics::SessionEconomics {
        turns: int("turns"),
        compactions: int("compactions"),
        ..Default::default()
    };
    Some(reading.budget_line(None, None))
}

/// Python's `_state_of`: the state a row is filtered on.
fn state_of<'a>(summary: &'a SessionSummary, attention: Option<&'a str>) -> Option<&'a str> {
    let activity = truthy(summary.activity.as_deref());
    if summary.activity.as_deref() == Some("stopped") || attention == Some("exited") {
        return if attention == Some("exited") {
            Some("exited")
        } else {
            activity.or(truthy(attention))
        };
    }
    activity.or(truthy(attention))
}

/// Python's `_kept`: default listings drop hibernated, stopped and exited.
/// An empty list means the caller asked for every state.
fn kept_in(summary: &SessionSummary, attention: Option<&str>, states: Option<&[String]>) -> bool {
    if states.is_some_and(<[String]>::is_empty) {
        return true;
    }
    let allowed = |name: &str| match states {
        None => DIGEST_LIVE_STATES.contains(&name),
        Some(states) => states.iter().any(|state| state == name),
    };
    let Some(state) = state_of(summary, attention) else {
        return allowed("unknown");
    };
    if allowed(state) || attention.is_some_and(allowed) {
        return true;
    }
    !EXCLUDED_STATES.contains(&state) && allowed(state)
}

/// Python's `_attention_of`: the attention class a sweep row would carry,
/// without a screen.
fn attention_of(summary: &SessionSummary, now: Moment, stall_after_secs: i64) -> String {
    decisions::classify(
        summary.activity.as_deref(),
        summary.alive,
        Some(summary.activity.as_deref() == Some("waiting-for-input")),
        summary
            .approval
            .as_ref()
            .map(|approval| approval.question.as_str()),
        summary
            .blocked
            .as_ref()
            .map(|blocked| blocked.blocker.as_str()),
        summary
            .approval
            .as_ref()
            .map(|approval| approval.kind.as_str()),
        moment_of(summary.last_output_at.as_deref()),
        now,
        stall_after_secs,
        moment_of(summary.done_at.as_deref()),
        None,
    )
    .attention
    .as_str()
    .to_string()
}

/// Python's `_attention_kept`: no filter keeps every row; `needs_you` stands
/// for its classes.
fn attention_kept(attention: &str, wanted: Option<&[String]>) -> bool {
    let Some(wanted) = wanted.filter(|wanted| !wanted.is_empty()) else {
        return true;
    };
    wanted.iter().any(|name| {
        if name == "needs_you" {
            decisions::Attention::needs_you_name(attention)
        } else {
            name == attention
        }
    })
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
    // Enough of every screen to see an input box holding unsent text
    // (WI-1870), whatever the caller asked to be shown.
    let entries = match engine.sweep_sessions(params.screen_lines.max(SWEEP_INPUT_LINES)) {
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
    // Unstopped links only: a history of stopped links would fill the
    // declared cap with rows the sweep then discards (WI-1266 review).
    let (listed, _declared_overflow) = rows_of(
        ctx,
        &ListSessionsParams {
            project: params.project.clone(),
            limit: 500,
            ..ListSessionsParams::default()
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
        let stale = block_is_stale(&summary);
        let mut verdict = decisions::classify(
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
                .filter(|_| !stale)
                .map(|blocked| blocked.blocker.as_str()),
            summary
                .approval
                .as_ref()
                .map(|approval| approval.kind.as_str()),
            moment_of(summary.last_output_at.as_deref()),
            swept_at,
            stall,
            moment_of(summary.done_at.as_deref()),
            turns_of(&entry.session),
        );
        if matches!(
            verdict.attention,
            decisions::Attention::Waiting | decisions::Attention::Done | decisions::Attention::Idle
        ) && delivery::input_pending(&entry.screen_tail).is_some()
        {
            verdict = decisions::Verdict {
                attention: decisions::Attention::Waiting,
                reason: decisions::INPUT_PENDING.to_string(),
            };
        }
        let shown = usize::try_from(params.screen_lines).unwrap_or(0);
        let screen_tail = if shown == 0 {
            Vec::new()
        } else {
            let tail = &entry.screen_tail;
            tail[tail.len().saturating_sub(shown)..].to_vec()
        };
        let (blocked_since, blocked_age_s) = block_age(&summary, swept_at);
        let mut reason = verdict.reason.clone();
        let mut claim_check = None;
        if matches!(verdict.attention, decisions::Attention::Done) && summary.work_item.is_some() {
            // WI-1835: a DONE claim is checked against git and the forge, and a
            // mismatch is said in the row's reason so the digest carries it.
            let text = std::iter::once(summary.last_reply_excerpt.clone().unwrap_or_default())
                .chain(entry.screen_tail.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n");
            let check = crate::application::claims::check_claim(
                ctx,
                &summary,
                crate::application::models::ClaimCheckClaim::AutopilotDone,
                Some(&text),
            )?;
            // Once per DONE: the key is the session and the moment it said so.
            if let Some(done_at) = moment_of(summary.done_at.as_deref()) {
                crate::application::claims::record_claim_check(
                    ctx,
                    &check,
                    Some(&format!("{}@{}", summary.id, done_at.to_iso())),
                )?;
            }
            if !check.mismatches.is_empty() {
                reason = format!("{reason}; claim check: {}", check.mismatches.join("; "));
            }
            claim_check = Some(check);
        }
        rows.push(SessionSweepRow {
            attention: verdict.attention.as_str().to_string(),
            attention_reason: reason,
            screen_tail,
            ready: Some(entry.ready),
            session: Some(summary),
            digest: None,
            blocked_since,
            blocked_age_s,
            blocked_stale: stale,
            claim_check,
        });
    }
    // Most urgent class first, then the most recent output; a row with no
    // readable output time counts as now, as Python's `or now` does.
    let output_at = |row: &SessionSweepRow| {
        row.session
            .as_ref()
            .and_then(|session| moment_of(session.last_output_at.as_deref()))
            .unwrap_or(swept_at)
    };
    rows.sort_by(|left, right| {
        decisions::Attention::order_of(&left.attention)
            .cmp(&decisions::Attention::order_of(&right.attention))
            .then_with(|| output_at(right).cmp(&output_at(left)))
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
    if params.mode == "digest" {
        // Digest rows: the state and attention filters and `limit` apply to
        // the rows only; the counts above cover every session.
        let mut kept = Vec::new();
        for row in rows {
            let Some(session) = row.session.as_ref() else {
                continue;
            };
            if !kept_in(session, Some(&row.attention), params.states.as_deref()) {
                continue;
            }
            if !attention_kept(&row.attention, params.attention.as_deref()) {
                continue;
            }
            let economics = live
                .iter()
                .find(|live_row| live_row.id == session.engine_session_id)
                .and_then(|live_row| live_row.economics.as_ref());
            let digest = digest_of(
                session,
                Some(&row.attention),
                row.ready,
                &params.fields,
                budget_of(economics),
            );
            kept.push(SessionSweepRow {
                session: None,
                digest: Some(digest),
                ..row
            });
        }
        kept.truncate(usize::try_from(params.limit).unwrap_or(0));
        rows = kept;
    }
    Ok(SessionSweepResult {
        rows,
        counts,
        swept_at,
        engine: None,
    })
}

/// Screen lines the sweep always reads, to find an input box (WI-1870).
const SWEEP_INPUT_LINES: i64 = 8;

/// An engine timestamp as a moment, or `None` when absent or unreadable.
fn moment_of(text: Option<&str>) -> Option<Moment> {
    text.and_then(|text| crate::core::from_iso(text).ok())
}

/// The engine's metered turn count, or `None` when it has not metered one
/// (Python's `_turns_of`). A bool or a float is not a count.
fn turns_of(engine_session: &EngineSession) -> Option<i64> {
    engine_session
        .economics
        .as_ref()
        .and_then(|economics| economics.get("turns"))
        .and_then(Value::as_i64)
}

/// Whether the session's blocked report predates its current turn.
///
/// A report is made mid-turn, just before the agent stops. A turn that began
/// after it means the session went back to work — it was typed into, woken,
/// or nudged on — so the report no longer says what it waits on (WI-1834).
/// So does a done declaration (`AUTOPILOT: DONE`) made after the report, even
/// in the same turn: the agent says nothing is left for it. The report stays
/// on the row, flagged stale, so it is not lost.
fn block_is_stale(summary: &SessionSummary) -> bool {
    let Some(since) = summary
        .blocked
        .as_ref()
        .and_then(|blocked| moment_of(blocked.since.as_deref()))
    else {
        return false;
    };
    [&summary.turn_started_at, &summary.done_at]
        .into_iter()
        .filter_map(|at| moment_of(at.as_deref()))
        .any(|at| at > since)
}

/// The sweep row's `blocked_since` and `blocked_age_s` (Python's
/// `_block_age`); both null when the session has no dated report.
fn block_age(summary: &SessionSummary, now: Moment) -> (Option<String>, Option<i64>) {
    let Some(blocked) = summary.blocked.as_ref() else {
        return (None, None);
    };
    let Some(since) = moment_of(blocked.since.as_deref()) else {
        return (None, None);
    };
    let age = now.seconds_since(since).trunc() as i64;
    (blocked.since.clone(), Some(age.max(0)))
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
    // An oversight session is relayed by its engine id: the engine lets it
    // answer a worker's permission prompt and audits it (WI-2028).
    let overseer = relayed_overseer(ctx, engine, person)?;
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
    if let Err(refused) = send_all(
        engine,
        &engine_id,
        &params.id,
        &writes_in_order,
        person,
        overseer.as_deref(),
    ) {
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
        send_all(
            engine,
            &engine_id,
            &params.id,
            &writes_in_order,
            person,
            overseer.as_deref(),
        )?;
    }
    let verdict = if confirming {
        let pending = (!text.is_empty()).then_some(text.as_str());
        let verdict = confirm_delivery(engine, &engine_id, before.as_deref(), pending);
        if verdict.delivery == delivery::Delivery::Queued && !text.is_empty() && params.submit {
            // Behind a running turn. The CLI should take it when the turn
            // ends, but a Claude Code turn can end with the text left in the
            // box, unsent (WI-1870): watch for that and press Enter.
            follow_queued(
                engine,
                &engine_id,
                &params.id,
                &text,
                person,
                overseer.as_deref(),
            );
        }
        verdict
    } else {
        delivery::judge(submitted, None, &[], None)
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
        "oversight": overseer.is_some(),
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
    let digest = input_digest(ctx, engine, &found, submitted, &reason)?;
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
        digest,
    })
}

/// Python's `_input_digest`: the digest of the session just typed into, when
/// it can be read.
///
/// A submitted input also clears the session's blocked report (WI-1834), from
/// the same read: a new instruction answers what it was blocked on, or
/// supersedes it. A draft is not sent yet.
fn input_digest<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    target: &Target,
    submitted: bool,
    reason: &str,
) -> Result<Option<SessionDigest>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut live = match engine.get_session(&target.engine_session_id) {
        Ok(live) => live,
        Err(VogtError::EngineUnavailable(_) | VogtError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if submitted {
        if let Some(current) = live.clone() {
            if let Some(cleared) =
                clear_block_on(ctx, engine, target, "input", reason, Some(current))?
            {
                live = Some(cleared);
            }
        }
    }
    target_digest(ctx, target, live.as_ref())
}

/// The digest row for a resolved target from one engine read: Python's
/// `_digest_of(_summarize(...))` for a linked session, of
/// `_summarize_engine_only(live)` for an unlinked one (null without a row).
fn target_digest<C, I>(
    ctx: &AppContext<C, I>,
    target: &Target,
    live: Option<&EngineSession>,
) -> Result<Option<SessionDigest>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let summary = match (&target.session, live) {
        (Some(session), live) => summarize(ctx, session, live)?,
        (None, Some(live)) => unlinked_summary(&ctx.declared.read()?, live)?,
        (None, None) => return Ok(None),
    };
    Ok(Some(digest_of(&summary, None, None, &[], None)))
}

/// Python's `_wait_digest`: one more engine read after the wait, then the
/// target resolved again. Null when the session cannot be read.
fn wait_digest<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    named: &str,
    engine_id: &str,
) -> Result<Option<SessionDigest>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let live = match engine.get_session(engine_id) {
        Ok(Some(live)) => live,
        Ok(None) | Err(VogtError::EngineUnavailable(_) | VogtError::NotFound(_)) => {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let target = resolve_target(ctx, named)?;
    target_digest(ctx, &target, Some(&live))
}

/// Python's `_tail_of`: the last 8 non-blank lines of a screen.
fn tail_of(screen: &SessionScreenResult) -> Vec<String> {
    let kept: Vec<&String> = screen
        .lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    kept[kept.len().saturating_sub(8)..]
        .iter()
        .map(|line| (*line).clone())
        .collect()
}

/// A wait's result: the full screen when `screen=true` asked for it, else the
/// digest row plus an 8-line tail (design C1). `ended` is the outcome, whether
/// it matched, and how long the wait took.
fn wait_result<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    params: &WaitParams,
    engine_id: &str,
    ended: (String, bool, i64),
    shown: SessionScreenResult,
) -> Result<SessionWaitResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (screen, digest, screen_tail) = if params.screen {
        (Some(shown), None, Vec::new())
    } else {
        let digest = wait_digest(ctx, engine, &params.id, engine_id)?;
        (None, digest, tail_of(&shown))
    };
    let (outcome, matched, waited_ms) = ended;
    Ok(SessionWaitResult {
        id: params.id.clone(),
        engine_session_id: engine_id.to_string(),
        outcome,
        matched,
        waited_ms,
        screen,
        digest,
        screen_tail,
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
            let shown = screen_result(&params.id, &engine_id, &screen.expect("checked"));
            return wait_result(
                ctx,
                engine,
                params,
                &engine_id,
                ("hibernated".to_string(), false, 0),
                shown,
            );
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
    let shown = screen_result(&params.id, &engine_id, &waited.screen);
    wait_result(
        ctx,
        engine,
        params,
        &engine_id,
        (waited.outcome, waited.matched, waited.waited_ms),
        shown,
    )
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
    let result = wake_resolved(ctx, engine, &found, &reason)?;
    // Woken on purpose: whoever woke it is acting on it, so the report it
    // slept with no longer stands (WI-1834). A session that is still blocked
    // reports again.
    if clear_block_on(ctx, engine, &found, "wake", &reason, None)?.is_some() {
        return wake_resolved(ctx, engine, &found, &reason);
    }
    Ok(result)
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
    Ok(SessionResult::plain(summary))
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
    Ok(SessionResult::plain(summary))
}

/// Answer the dialog a session shows — a permission request or a startup gate —
/// by option, not by keystrokes. Exactly one of `option` or `label`. Whether the
/// caller counts as a person is the engine's decision: the flag is sent, and a
/// permission prompt an agent answers is refused there. The one exception is
/// an oversight session choosing "Yes" on an allow-listed command in the
/// worker's own worktree, which the engine decides and audits (WI-1831).
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
    // A person answers anything. An oversight session is relayed by its
    // engine id, not as a person: the engine lets it choose "Yes" only on an
    // allow-listed command in the worker's own worktree and refuses the rest
    // with `PersonRequired` (WI-1831, WI-2028). The audit line records which
    // answered.
    let overseer = relayed_overseer(ctx, engine, person)?;
    let answered = engine
        .answer_session(
            &engine_id,
            params.option,
            label,
            params.expect_question.as_deref(),
            person,
            overseer.as_deref(),
        )?
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
        "oversight": overseer.is_some(),
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
        .hibernate_session(&engine_id, Some(&reason), params.allow_shell, None)?
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
        return Ok(SessionResult::digested(unlinked_summary(
            &ctx.declared.read()?,
            &hibernated,
        )?));
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
    Ok(SessionResult::digested(summarize(
        ctx,
        &session,
        Some(&hibernated),
    )?))
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
    brief: ComposedBrief,
    is_scratch: bool,
    /// The project's agent policy on this start's base (WI-1841).
    policy: crate::core::AgentPolicy,
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
    // A review is of one work item: `review_of` names it, and a different
    // `work_item` or `from_handoff` beside it is a contradiction, refused
    // before anything starts. Naming it is enough: it becomes the work item.
    let mut work_ref = params.work_item.clone();
    if let Some(review) = &params.review_of {
        if let Some(work) = params.work_item.as_ref().filter(|work| *work != review) {
            return Err(VogtError::InvalidRequest(format!(
                "review_of names {review}, but work_item names {work}"
            )));
        }
        if let Some(from) = params.from_handoff.as_ref().filter(|from| *from != review) {
            return Err(VogtError::InvalidRequest(format!(
                "review_of names {review}, but from_handoff names {from}"
            )));
        }
        work_ref = Some(resolve::work_item(&ctx.declared.read()?, review)?.reference);
    }
    // `from_handoff` is sugar for "a fresh session on the item the hand-off
    // belongs to". The brief carries the hand-off because it always does once
    // the item is the subject; an item with no hand-off is refused.
    if let Some(from) = &params.from_handoff {
        if let Some(work) = work_ref.as_ref().filter(|work| *work != from) {
            return Err(VogtError::InvalidRequest(format!(
                "from_handoff names {from}, but work_item names {work}"
            )));
        }
        let view = ctx.declared.read()?;
        let item = resolve::work_item(&view, from)?;
        let comments = view.comments_for(&item.id, 200, true)?;
        if !comments.iter().any(|one| one.kind == "handoff") {
            return Err(VogtError::InvalidRequest(format!(
                "{} has no hand-off comment to start a session from",
                item.reference
            )));
        }
        work_ref = Some(item.reference);
    }
    // Both is a bad request, and it is reported before either is resolved
    // (beyond the review and hand-off checks above) so a missing work item or
    // project does not hide it.
    if work_ref.is_some() && params.project.is_some() {
        return Err(VogtError::InvalidRequest(
            "give at most one of --work-item or --project".to_string(),
        ));
    }
    let work_item = match &work_ref {
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
    // The policy on the item's base, before the brief states it (WI-1841).
    let project = with_item_base(
        &ctx.declared.read()?,
        project,
        work_item.as_ref(),
        params.task.as_deref(),
    )?;
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
        Some(item) => {
            let review = params.review_of.is_some();
            // The score explanation is optional on purpose: a brief that
            // refused to be written because a score could not be computed
            // would make the ranking a precondition for starting work. A
            // review does not rank the item it checks.
            let ranking: Option<WhyResult> = if review {
                None
            } else {
                crate::application::views::why(ctx, &item.reference)
                    .ok()
                    .and_then(|value| serde_json::from_value(value).ok())
            };
            let view = ctx.declared.read()?;
            let comments = view.comments_for(&item.id, 200, true)?;
            let mut related = BTreeMap::new();
            for relation in &item.relations {
                if let Some(found) = view.work_item_by_id(&relation.related_id)? {
                    related.insert(relation.related_id.clone(), found);
                }
            }
            let input = WorkBrief {
                item,
                session_id,
                ranking: ranking.as_ref(),
                project: Some(&project),
                comments: &comments,
                related: &related,
            };
            if review {
                brief::review_brief_for_work_item(&input)
            } else {
                brief::brief_for_work_item(&input)?
            }
        }
        None => brief::brief_for_project(&project.slug, session_id, Some(&project)),
    };
    Ok(Subject {
        session_id: session_id.to_string(),
        policy: project.agent_policy.clone(),
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
    Ok(SessionResult::plain(summary))
}

/// Python's `_resolve_purpose`: fill a start's unset fields from its purpose's
/// dispatch profile. Returns the filled params, whether the profile asks for a
/// read-only session, and the profile fields the caller set itself (the
/// overrides). A start with no purpose is returned unchanged. The profile is
/// the deployment's, written by a person, over the code defaults.
fn resolve_purpose<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
) -> Result<(StartSessionParams, bool, Vec<&'static str>), VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let Some(purpose) = params.purpose.as_deref() else {
        return Ok((params.clone(), false, Vec::new()));
    };
    if !dispatch_profiles::PURPOSES.contains(&purpose) {
        return Err(VogtError::InvalidRequest(format!(
            "unknown purpose {}; the purposes are {}",
            py_repr(purpose),
            dispatch_profiles::PURPOSES.join(", ")
        )));
    }
    let settings = dispatch_profiles::resolve(&dispatch_preferences(ctx)?);
    let profile = settings.profile(purpose).ok_or_else(|| {
        VogtError::InvalidRequest(format!("no dispatch profile for {}", py_repr(purpose)))
    })?;
    // A field is the caller's when it carries a value or an explicit null.
    let set = |name: &str, given: bool| given || params.explicit_null.contains(&name);
    let given = [
        params.template.is_some(),
        params.model.is_some(),
        params.effort.is_some(),
        params.budget.is_some(),
        params.worktree.is_some(),
        params.repo_map.is_some(),
        params.autopilot.is_some(),
    ];
    let overrides: Vec<&'static str> = PROFILE_FIELDS
        .into_iter()
        .zip(given)
        .filter_map(|(name, given)| set(name, given).then_some(name))
        .collect();
    let caller = |name: &str| overrides.contains(&name);
    let mut filled = params.clone();
    if !caller("template") {
        filled.template = Some(profile.template.clone());
        if caller("model")
            && params
                .model
                .as_deref()
                .is_some_and(|model| CLAUDE_MODEL.is_match(model))
            && resolved_template_tag(Some(&profile.template)).as_deref() != Some("claude")
        {
            // A Claude model on the profile's other agent never runs: klaudia
            // answers `opus` with 400 "Invalid model name" on every turn
            // (WI-1852). The caller named the model, so the agent follows it.
            filled.template = Some("claude".to_string());
        }
    }
    if !caller("model") {
        filled.model = Some(profile.model.clone());
    }
    if !caller("effort") {
        filled.effort = profile.effort.clone();
    }
    if !caller("budget") {
        // Sized by the model the start will run, the caller's or the
        // profile's (WI-1833).
        let turns = if caller("model") {
            profile.turns_for(params.model.as_deref())
        } else {
            profile.turns_for(Some(&profile.model))
        };
        filled.budget =
            (turns.is_some() || profile.budget_wall_clock_minutes.is_some()).then(|| {
                SessionBudget {
                    turns,
                    wall_clock_minutes: profile.budget_wall_clock_minutes,
                    ..SessionBudget::default()
                }
            });
    }
    if !caller("worktree") {
        filled.worktree = Some(profile.worktree);
    }
    if !caller("repo_map") {
        filled.repo_map = Some(profile.repo_map);
    }
    if !caller("autopilot") {
        filled.autopilot = Some(profile.autopilot);
    }
    Ok((filled, profile.read_only, overrides))
}

/// `opus`, `sonnet`, `haiku` and `fable` are the aliases Claude Code resolves
/// itself; a full id is `claude-…`.
static CLAUDE_MODEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^(claude-.+|opus|sonnet|haiku|fable)$").expect("a fixed pattern")
});
/// The bare aliases among those: names only Claude Code resolves.
static CLAUDE_ALIAS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^(opus|sonnet|haiku|fable)$").expect("a fixed pattern")
});
/// Agent templates that are not Claude Code.
const NON_CLAUDE_AGENTS: [&str; 3] = ["codex", "opencode", "klaudia"];

/// The agent a template name means, after the engine's alias and tag rules.
///
/// The engine matches a template by name first and, failing that, by tag, so
/// `claude` reaches the template tagged `claude` and `Claude Code
/// (protected)` is the same template by name. The core never sees the
/// deployment's template list, so the resolution mirrored here is the
/// built-in set those names come from: a name or tag of `claude`, `codex`,
/// `opencode` or `klaudia`, compared case-insensitively the way the engine
/// compares them. A name that names none of them is returned as given,
/// lower-cased. Ports `_resolved_template_tag`.
fn resolved_template_tag(template: Option<&str>) -> Option<String> {
    let folded = template?.trim().to_lowercase();
    for agent in ["claude", "codex", "opencode", "klaudia"] {
        if folded == agent || folded.ends_with(&format!("({agent})")) {
            return Some(agent.to_string());
        }
    }
    // The built-in template's own name, "Claude Code (protected)": the engine
    // matches a template by its full name before it tries a tag.
    if folded.starts_with("claude code") {
        return Some("claude".to_string());
    }
    Some(folded)
}

/// The WI-1852 half of Python's `_refuse_forbidden_model`: a bare Claude Code
/// alias on an agent template that is not Claude Code is refused before the
/// engine spawns anything, rather than failing every turn.
///
/// The WI-1590 rules in the same Python function (a Claude Code template with
/// a model it does not speak, and the project's `never_models` and
/// `forbidden_text` matched against the model) are not ported yet; none of
/// them can refuse the start this one refuses, since a template is either
/// Claude Code or not.
fn refuse_forbidden_model(params: &StartSessionParams) -> Result<(), VogtError> {
    let template = resolved_template_tag(params.template.as_deref());
    if let (Some(agent), Some(model)) = (template.as_deref(), params.model.as_deref()) {
        if NON_CLAUDE_AGENTS.contains(&agent) && CLAUDE_ALIAS.is_match(model) {
            // The mirror image (WI-1852): `opus` is a name only Claude Code
            // resolves, and the other agents refuse it on every turn.
            return Err(VogtError::PolicyRefused(format!(
                "template {} runs {agent}, which does not resolve the Claude Code alias {}; \
                 use template 'claude', or name a model that agent takes",
                py_repr(params.template.as_deref().unwrap_or_default()),
                py_repr(model),
            )));
        }
    }
    Ok(())
}

/// The `dispatch.*` values a person last wrote, by key. A key nobody wrote is
/// absent, which the resolver reads as its default.
pub(crate) fn dispatch_preferences<C, I>(
    ctx: &AppContext<C, I>,
) -> Result<std::collections::BTreeMap<String, Value>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let view = ctx.declared.read()?;
    let mut stored = std::collections::BTreeMap::new();
    for key in dispatch_profiles::KEYS {
        if let Some(row) = view.latest_human_preference(key)? {
            if row
                .value
                .as_object()
                .is_some_and(|object| !object.is_empty())
            {
                stored.insert(key.to_string(), row.value);
            }
        }
    }
    Ok(stored)
}

/// Python's `_started_by`: who is starting this session, from the caller's
/// verified identity. A person is `person` with no session. A session is named
/// by its engine id, and its kind is its engine role right now, or none when
/// the engine cannot say. Nothing here is read from the start's parameters.
fn started_by<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
) -> Result<(Option<String>, Option<String>), VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if ctx.principal.kind != ActorKind::Agent {
        return Ok((None, Some("person".to_string())));
    }
    let Some(caller) = session_commit::caller_engine_session_id(ctx)? else {
        return claimed_oversight(ctx, engine);
    };
    let live = match engine.get_session(&caller) {
        Ok(live) => live,
        Err(VogtError::EngineUnavailable(_)) => None,
        Err(error) => return Err(error),
    };
    let kind = match live.map(|session| session.role) {
        Some(role) if role == "oversight" => Some("oversight".to_string()),
        Some(role) if role == "worker" => Some("worker".to_string()),
        _ => None,
    };
    Ok((Some(caller), kind))
}

/// The shared pod token's actor (`agent:pod:<stack>`).
const POD_ACTOR_PREFIX: &str = "agent:pod:";

/// A pod-token caller's starter, from the session its bridge says it runs in.
///
/// An oversight session resumed by hand (`claude --resume` in a shell) runs
/// with the pod's token, not its own, so it has no session identity and its
/// starts were recorded with no starter — and its ownership filter lost them
/// (WI-1852). The MCP bridge names the engine session it runs in; that claim
/// counts only from the pod token, which already outranks any session, and
/// only when the engine says that session is live and oversight. It sets
/// attribution and nothing else.
fn claimed_oversight<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
) -> Result<(Option<String>, Option<String>), VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let Some(claimed) = ctx.claimed_engine_session.clone() else {
        return Ok((None, None));
    };
    if !ctx.principal.identity_ref.starts_with(POD_ACTOR_PREFIX) {
        return Ok((None, None));
    }
    let live = match engine.get_session(&claimed) {
        Ok(live) => live,
        Err(VogtError::EngineUnavailable(_)) => return Ok((None, None)),
        Err(error) => return Err(error),
    };
    match live {
        Some(live) if live.alive && live.role == "oversight" => {
            Ok((Some(claimed), Some("oversight".to_string())))
        }
        _ => Ok((None, None)),
    }
}

/// What the engine was asked to run.
fn build_spec<C, I>(
    ctx: &AppContext<C, I>,
    params: &StartSessionParams,
    subject: &Subject,
    token: &str,
    read_only: bool,
    caller_merges: bool,
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
    // A start that names no task takes the one it implies (WI-1854); a
    // review's Task section is written by `brief_with_task` instead.
    let mut task = params.task.as_deref().unwrap_or("").trim().to_string();
    if params.review_of.is_none() && task.is_empty() {
        task = default_task(params);
    }
    spec.prompt = Some(brief_with_task(
        &subject.brief.text,
        Some(&task),
        autopilot,
        read_only,
        params.review_of.is_some(),
        LandingFields::of(&subject.policy).as_ref(),
        caller_merges,
    ));
    spec.companion = (!subject.brief.companion.is_empty()).then(|| subject.brief.companion.clone());
    spec.env = Some(session_env(
        ctx,
        &subject.session_id,
        token,
        subject.work_item_ref.as_deref(),
    ));
    Ok(spec)
}

/// The brief plus the autopilot and read-only notes and the task, matching
/// Python's `_brief_with_task`. A review's task is recorded as what the review
/// was asked to look at, not as new work. Touch points (files, symbols, a repo
/// map drawn from them) are not carried by the Rust start parameters yet.
fn brief_with_task(
    brief: &str,
    task: Option<&str>,
    autopilot: bool,
    read_only: bool,
    review: bool,
    landing: Option<&LandingFields>,
    caller_merges: bool,
) -> String {
    let mut text = brief.to_string();
    if autopilot {
        text = format!("{}\n\n{}", text.trim_end(), brief::AUTOPILOT);
    }
    if read_only {
        text = format!("{}\n\n{}", text.trim_end(), brief::READ_ONLY);
    }
    let task = task.unwrap_or("").trim();
    if review {
        // The agent's launch prompt looks for a Task section and otherwise
        // waits, so a reviewer gets one even when the caller named no task
        // (WI-1854). The caller's own task stays under "Also asked".
        text = format!("{}\n\n## Task\n\n{REVIEW_TASK}\n", text.trim_end());
    }
    if !task.is_empty() {
        let heading = if review { "## Also asked" } else { "## Task" };
        // Where the task contradicts the landing rule the policy wins, and
        // the brief says so above the task rather than leaving the worker
        // two instructions to choose between (WI-1832).
        let overrides = policy_conflicts::overrides_section(
            &policy_conflicts::task_policy_conflicts(task, landing, caller_merges),
        );
        if !overrides.is_empty() {
            text = format!("{}\n\n{overrides}", text.trim_end());
        }
        text = format!("{}\n\n{heading}\n\n{task}\n", text.trim_end());
    }
    text
}

/// The Task section a review start gets (WI-1854): what the review brief
/// above it already asks, said under the heading the launch prompt looks for.
const REVIEW_TASK: &str = "Review the branch this item's latest hand-off names, as the Review \
     section above says, and post the verdict as one `work_comment`: \
     `no blockers (head <sha>)` or `blockers (head <sha>):` with the list.";

/// How a reason says it is the task (WI-1854): `Task: do the thing`.
static TASK_PREFIX: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)^\s*task\s*:\s*").expect("a fixed pattern")
});

/// The task a start implies when it names none (WI-1854).
///
/// An agent is launched to "carry out the Task section, or summarise and
/// wait", so a start that leaves `task` out opens an agent that idles.
/// Callers often put the task in `reason` instead; an agent start takes it
/// from there. A purpose of `review` gets the review task. A plain shell gets
/// nothing: it has no agent to read a task.
fn default_task(params: &StartSessionParams) -> String {
    if params.purpose.as_deref() == Some("review") {
        let target = params.work_item.as_deref().unwrap_or("the bound work item");
        return format!(
            "Review the pull request or branch bound to {target} and \
             report blockers: post one `work_comment` whose first line is \
             `no blockers (head <sha>)` or `blockers (head <sha>):` followed \
             by a numbered list. Make no edits, commits or pushes."
        );
    }
    if params.template.is_none() {
        return String::new();
    }
    TASK_PREFIX.replace(&params.reason, "").trim().to_string()
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
        purpose: session.purpose.clone(),
        started_by_session: session.started_by_session.clone(),
        started_by_kind: session.started_by_kind.clone(),
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
        done_at: None,
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
    summary.done_at = live.done_at.clone();
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
        blocker: blocked.reason.clone(),
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
        // The engine's own record of the template, not an audited claim: the
        // name it was started from, as the GUI's rail shows it.
        template: session.template.clone(),
        model: None,
        effort: None,
        reason: None,
        purpose: None,
        started_by_session: None,
        started_by_kind: None,
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
        done_at: None,
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

/// Read the session a few times after a submit and judge, stopping at the
/// first decisive read. An engine that cannot be read leaves the judgement
/// to what was seen before. `pending` is the submitted text when the input
/// box should be checked for it.
fn confirm_delivery(
    engine: &EngineClient,
    engine_id: &str,
    before: Option<&str>,
    pending: Option<&str>,
) -> delivery::Verdict {
    let mut observed = Vec::new();
    let mut verdict = delivery::judge(true, before, &observed, pending);
    if verdict.delivery != delivery::Delivery::Unconfirmed {
        return verdict;
    }
    for _ in 0..CONFIRM_READS {
        pause(CONFIRM_INTERVAL);
        let Ok(Some(screen)) = engine.session_screen(engine_id, 0) else {
            break;
        };
        observed.push(delivery::Observation {
            activity: screen.activity.clone(),
            lines: screen.lines.clone(),
        });
        verdict = delivery::judge(true, before, &observed, pending);
        // A held box can be the CLI not yet having read the Enter: `stuck`
        // stands only if it is still held at the last read.
        if !matches!(
            verdict.delivery,
            delivery::Delivery::Unconfirmed | delivery::Delivery::Stuck
        ) {
            break;
        }
    }
    verdict
}

/// The pause between delivery reads. Instant under test, as Python's
/// conftest makes `_pause` instant.
fn pause(interval: Duration) {
    if !cfg!(test) {
        std::thread::sleep(interval);
    }
}

/// How long a queued input is followed for its turn to end, and how many
/// Enters it gets once the agent is at rest with the text still in its box.
const QUEUED_FOLLOW_S: u64 = 3600;
const QUEUED_MAX_ENTERS: usize = 2;

/// The longest single wait the engine accepts (its `/wait` caps at 600 s and
/// refuses more); a longer follow-up waits in chunks of this.
const ENGINE_WAIT_MAX_S: u64 = 600;

/// Follow a queued input in the background (WI-1870). Under test it runs
/// inline, as Python's conftest swaps `_follow_queued` for `_submit_queued`
/// so a test sees what it did. A client whose transport was injected (a
/// stand-in) cannot cross a thread, so it is followed inline too.
fn follow_queued(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    text: &str,
    person: bool,
    overseer: Option<&str>,
) {
    let detached = if cfg!(test) { None } else { engine.detached() };
    let Some(detached) = detached else {
        submit_queued_logged(engine, engine_id, named, text, person, overseer);
        return;
    };
    let (engine_id, named, text, overseer) = (
        engine_id.to_string(),
        named.to_string(),
        text.to_string(),
        overseer.map(str::to_string),
    );
    let spawned = std::thread::Builder::new()
        .name(format!("vogt-queued-input-{engine_id}"))
        .spawn(move || {
            let engine = detached.client();
            submit_queued_logged(
                &engine,
                &engine_id,
                &named,
                &text,
                person,
                overseer.as_deref(),
            );
        });
    if let Err(error) = spawned {
        tracing::error!(
            target: "vogt.sessions",
            "following queued input failed to start: {error}"
        );
    }
}

/// The follow-up thread's body: nothing it raises may die silently.
fn submit_queued_logged(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    text: &str,
    person: bool,
    overseer: Option<&str>,
) {
    if let Err(error) = submit_queued(engine, engine_id, named, text, person, overseer) {
        tracing::error!(
            target: "vogt.sessions",
            "following queued input to {named} failed: {error}"
        );
    }
}

/// Wait up to `QUEUED_FOLLOW_S` for the session to be ready, in waits of at
/// most `ENGINE_WAIT_MAX_S` each. False when it never was, or the engine no
/// longer holds it.
fn wait_turn_end(engine: &EngineClient, engine_id: &str) -> Result<bool, VogtError> {
    let chunks = QUEUED_FOLLOW_S.div_ceil(ENGINE_WAIT_MAX_S);
    for _ in 0..chunks {
        let Some(waited) =
            engine.wait_session(engine_id, "ready", Duration::from_secs(ENGINE_WAIT_MAX_S))?
        else {
            return Ok(false);
        };
        if waited.outcome == "ready" {
            return Ok(true);
        }
        if waited.outcome != "timeout" {
            // Exited, hibernated, refused: nothing left to submit to.
            return Ok(false);
        }
    }
    Ok(false)
}

/// Once the running turn ends, make sure the queued text was taken (WI-1870).
///
/// Waits for the agent to be ready, then reads its screen: a box that no
/// longer holds the text means the CLI took it. A box still holding it, with
/// no dialog or approval showing, gets an Enter, at most `QUEUED_MAX_ENTERS`
/// times. Still held after that is `stuck`; the sweep shows such a session
/// as "input pending" from its screen.
fn submit_queued(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    text: &str,
    person: bool,
    overseer: Option<&str>,
) -> Result<delivery::Delivery, VogtError> {
    let attempt = || -> Result<Option<delivery::Delivery>, VogtError> {
        if !wait_turn_end(engine, engine_id)? {
            return Ok(Some(delivery::Delivery::Queued));
        }
        for _ in 0..QUEUED_MAX_ENTERS {
            let Some(screen) = engine.session_screen(engine_id, 0)? else {
                return Ok(Some(delivery::Delivery::Unconfirmed));
            };
            let seen = delivery::Observation {
                activity: screen.activity.clone(),
                lines: screen.lines.clone(),
            };
            if !delivery::safe_to_resubmit(&seen, text) {
                return Ok(Some(delivery::Delivery::Delivered));
            }
            send_all(
                engine,
                engine_id,
                named,
                &[(String::new(), true)],
                person,
                overseer,
            )?;
            let verdict =
                confirm_delivery(engine, engine_id, Some("waiting-for-input"), Some(text));
            if verdict.delivery == delivery::Delivery::Delivered {
                tracing::info!(
                    target: "vogt.sessions",
                    "queued input to {named} submitted by a follow-up Enter"
                );
                return Ok(Some(delivery::Delivery::Delivered));
            }
        }
        Ok(None)
    };
    match attempt() {
        Ok(Some(found)) => return Ok(found),
        Ok(None) => {}
        Err(VogtError::EngineUnavailable(_) | VogtError::NotFound(_)) => {
            return Ok(delivery::Delivery::Unconfirmed)
        }
        Err(error) => return Err(error),
    }
    tracing::warn!(
        target: "vogt.sessions",
        "queued input to {named} is stuck in its input box"
    );
    Ok(delivery::Delivery::Stuck)
}

/// `submit_queued` for the service tests, which drive it directly as
/// `tests/test_delivery.py` drives `_submit_queued`.
#[cfg(test)]
pub(crate) fn tests_submit_queued(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    text: &str,
) -> Result<delivery::Delivery, VogtError> {
    submit_queued(engine, engine_id, named, text, false, None)
}

/// Clear `target`'s blocked report because someone acted on the session
/// (WI-1834): it was given new input, or woken. The report it replaces stays
/// in the audit, as an unblock naming the cause and what it said.
///
/// A session typing into or waking itself clears nothing, and a report that
/// cannot be read or cleared is left for the sweep's staleness rule: this
/// never fails the input or wake it rides on. `current` is a read of the
/// session the caller already holds, to spare a second one. The session as
/// cleared when one was, else `None`.
fn clear_block_on<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    target: &Target,
    cause: &str,
    reason: &str,
    current: Option<EngineSession>,
) -> Result<Option<EngineSession>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine_id = &target.engine_session_id;
    let own = ctx.principal.identity_ref == format!("agent:session:{engine_id}")
        || target.session.as_ref().is_some_and(|session| {
            ctx.principal.identity_ref == format!("agent:session:{}", session.id)
        });
    if own {
        return Ok(None);
    }
    let read = || -> Result<Option<(EngineBlocked, EngineSession)>, VogtError> {
        let current = match current {
            Some(current) => current,
            None => match engine.get_session(engine_id)? {
                Some(current) => current,
                None => return Ok(None),
            },
        };
        let Some(was) = current.blocked else {
            return Ok(None);
        };
        Ok(engine
            .set_blocked(engine_id, false, None, &[])?
            .map(|cleared| (was, cleared)))
    };
    let (was, cleared) = match read() {
        Ok(Some(found)) => found,
        Ok(None) => return Ok(None),
        Err(VogtError::EngineUnavailable(_) | VogtError::Conflict(_) | VogtError::NotFound(_)) => {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let entity = target
        .session
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.clone());
    let outcome = json!({
        "engine_session_id": engine_id,
        "linked": target.session.is_some(),
        "blocker": null,
        "items": [],
        "cleared_by": cause,
        "was_blocker": was.reason,
        "was_items": was.items,
        "was_since": was.since,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        "session.report_unblocked",
        &format!("cleared by {cause}: {reason}"),
        "session",
        &entity,
        &outcome,
        "session.unblocked",
        None,
    )?;
    Ok(Some(cleared))
}

/// The engine id to relay as `overseer` when the caller is a session the
/// engine marks `oversight` (WI-2028), so the engine lets it answer a worker's
/// permission prompt. `None` for a person, who needs no relay, and for every
/// other caller, which the engine still refuses with `PersonRequired`.
pub(crate) fn relayed_overseer<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    person: bool,
) -> Result<Option<String>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if person {
        return Ok(None);
    }
    let Some(caller) = crate::application::session_commit::caller_engine_session_id(ctx)? else {
        return Ok(None);
    };
    Ok(engine
        .get_session(&caller)?
        .filter(|found| found.role == "oversight")
        .map(|_| caller))
}

/// One engine write per chunk. `Ok(false)` is the engine saying it has no live
/// session by that id — said with the name the caller used.
fn send_all(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    chunks: &[(String, bool)],
    person: bool,
    overseer: Option<&str>,
) -> Result<(), VogtError> {
    for (text, submit) in chunks {
        let sent = engine.send_input(engine_id, text, *submit, person, overseer)?;
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
        return Ok(SessionResult::digested(summary));
    }
    let Some(session) = session else {
        let woken = engine
            .wake_session(engine_id, None, false)?
            .ok_or_else(|| {
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
        return Ok(SessionResult::digested(unlinked_summary(
            &ctx.declared.read()?,
            &woken,
        )?));
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
    let woken = engine
        .wake_session(engine_id, Some(&env), false)?
        .ok_or_else(|| {
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
    Ok(SessionResult::digested(summarize(
        ctx,
        &session,
        Some(&woken),
    )?))
}

/// Whether the caller counts as a person for the engine's person gate: a human
/// caller who is not the engine's own credential.
pub(crate) fn answers_as_person<C, I>(ctx: &AppContext<C, I>) -> Result<bool, VogtError>
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
        "purpose": session.purpose,
        "started_by_session": session.started_by_session,
        "started_by_kind": session.started_by_kind,
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

// -- sessions part II: rename, remove, token (WI-1074) -------------------------

/// Rename a session, live or hibernated. The name lives on the engine; Vogt
/// records that it happened and returns the session as it is afterwards.
pub fn rename_session_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let parsed = RenameSessionParams {
        id: require_string(&params, "id")?,
        name: require_string(&params, "name")?,
        reason: require_string(&params, "reason")?,
    };
    dispatch!(built, rename_session, &parsed)
        .and_then(|result| serde_json::to_value(result).map_err(json_error))
}

/// Remove a session: kill it if it still runs and have the engine forget it.
/// A linked session's record is closed and its token revoked, as a stop would.
pub fn remove_session_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let parsed = RemoveSessionParams {
        id: require_string(&params, "id")?,
        reason: require_string(&params, "reason")?,
    };
    dispatch!(built, remove_session, &parsed)
        .and_then(|result| serde_json::to_value(result).map_err(json_error))
}

/// The session engine's call: mint `agent:engine:<uuid>`, or revoke it.
/// Refused to every other caller (403 engine_only).
pub fn session_token_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let parsed = SessionTokenParams {
        engine_session_id: require_string(&params, "engine_session_id")?,
        revoke: params
            .get("revoke")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        reason: require_string(&params, "reason")?,
    };
    if !ENGINE_SESSION_ID.is_match(&parsed.engine_session_id) {
        return Err(VogtError::InvalidRequest(
            "engine_session_id must be a UUID".to_string(),
        ));
    }
    dispatch!(built, session_token, &parsed)
        .and_then(|result| serde_json::to_value(result).map_err(json_error))
}

#[derive(Debug, Clone)]
pub struct RenameSessionParams {
    pub id: String,
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct RemoveSessionParams {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct SessionTokenParams {
    pub engine_session_id: String,
    pub revoke: bool,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionTokenResult {
    pub engine_session_id: String,
    pub actor: String,
    /// `null` when the call revoked rather than minted, matching Python's
    /// `EngineSessionTokenResult.token`.
    pub token: Option<String>,
    pub revoked: i64,
}

/// The engine's session id: a UUID, exactly as the catalog's pattern.
static ENGINE_SESSION_ID: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap()
});

const ENGINE_SESSION_ACTOR_PREFIX: &str = "agent:engine:";

/// The name the engine shows. Trimmed, and refused when empty or past the
/// engine's own byte limit, so a name it would reject is rejected here first.
const SESSION_NAME_MAX_BYTES: usize = 256;

pub fn rename_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &RenameSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let name = params.name.trim();
    if name.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a session name cannot be empty".to_string(),
        ));
    }
    if name.len() > SESSION_NAME_MAX_BYTES {
        return Err(VogtError::InvalidRequest(format!(
            "a session name is at most {SESSION_NAME_MAX_BYTES} bytes"
        )));
    }
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let renamed = engine.rename_session(&found.engine_session_id, name)?;
    if !renamed {
        return Err(VogtError::NotFound(format!(
            "no session {}",
            py_repr(&params.id)
        )));
    }
    let mut writing = write_of(ctx);
    let engine_id = found.engine_session_id.clone();
    let named = name.to_string();
    audited_action(
        &mut writing,
        SESSION_RENAME,
        &reason,
        "session",
        &engine_id,
        &json!({"name": named, "linked": found.session.is_some()}),
        "session.renamed",
        Some(&json!({"engine_session_id": engine_id})),
    )?;
    // The rename itself only says whether the engine had the session.
    let current = engine
        .get_session(&found.engine_session_id)?
        .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(&params.id))))?;
    let summary = match &found.session {
        Some(session) => summarize(ctx, session, Some(&current))?,
        None => unlinked_summary(&ctx.declared.read()?, &current)?,
    };
    Ok(SessionResult::plain(summary))
}

/// Kill a session if it still runs and have the engine forget it.
///
/// `session.stop` closes Vogt's record and leaves the engine's; this also
/// drops the engine's record, screen and brief. A session Vogt linked is
/// closed and has its token revoked, exactly as a stop would, so a removed
/// session cannot keep calling back.
pub fn remove_session<C, I>(
    ctx: &AppContext<C, I>,
    params: &RemoveSessionParams,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    let engine = engine_of(ctx)?;
    let found = resolve_target(ctx, &params.id)?;
    let engine_id = found.engine_session_id.clone();
    // A live session is killed with who and why first, so the exit reads
    // `stopped` rather than `errored` (WI-913). DELETE alone kills without them.
    let before = engine.get_session(&engine_id)?;
    if before.as_ref().is_some_and(|session| session.alive) {
        engine.kill_session(&engine_id, Some(&reason), Some(&ctx.principal.identity_ref))?;
    }
    if found.session.is_none() {
        let Some(before) = before else {
            return Err(VogtError::NotFound(format!(
                "no session {}",
                py_repr(&engine_id)
            )));
        };
        if !engine.remove_session(&engine_id)? {
            return Err(VogtError::NotFound(format!(
                "no session {}",
                py_repr(&engine_id)
            )));
        }
        let mut writing = write_of(ctx);
        audited_action(
            &mut writing,
            SESSION_REMOVE,
            &reason,
            "session",
            &engine_id,
            &json!({"engine_session_id": engine_id, "linked": false}),
            "session.removed",
            Some(&json!({"engine_removed": true, "linked": false})),
        )?;
        let mut summary = unlinked_summary(&ctx.declared.read()?, &before)?;
        summary.alive = Some(false);
        summary.stopped_at = Some(now(&writing));
        return Ok(SessionResult::plain(summary));
    }

    let session = found.session.unwrap();
    let removed = engine.remove_session(&engine_id)?;
    let mut writing = write_of(ctx);
    let session_id = session.id.clone();
    let actor_id = session.actor_id.clone();
    let why = reason.clone();
    let stopped_at = now(&writing);
    let result = audited_write(&mut writing, SESSION_REMOVE, &reason, move |txn, _actor| {
        let current = txn
            .session_by_id(&session_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(&session_id))))?;
        let closed = current.stopped_at.is_none();
        if closed {
            txn.mark_session_stopped(&session_id, stopped_at)?;
            for token in txn.tokens_for_actor(&actor_id, false)? {
                if token.revoked_at.is_none() {
                    txn.revoke_token(&token.id, &why, stopped_at)?;
                }
            }
        }
        let after = txn
            .session_by_id(&session_id)?
            .ok_or_else(|| VogtError::NotFound(format!("no session {}", py_repr(&session_id))))?;
        let summary = summarize(ctx, &after, None)?;
        Ok(WriteOutcome::new(
            SessionResult::plain(summary),
            "session",
            &session_id,
            audited_payload(&after),
            "session.removed",
            json!({"engine_removed": removed, "record_closed": closed}),
        ))
    })?;
    Ok(result)
}

/// Mint (or revoke) the credential of an agent session the engine started.
///
/// Only the engine's own credential may ask. The token is bound to
/// `agent:engine:<uuid>`, created on first use, and every earlier token of
/// that actor is revoked in the same write. `revoke` ends them and returns no
/// secret.
pub fn session_token<C, I>(
    ctx: &AppContext<C, I>,
    params: &SessionTokenParams,
) -> Result<SessionTokenResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = writes::validate_reason(&params.reason)?;
    if !is_engine_credential(ctx)? {
        return Err(VogtError::EngineOnly(format!(
            "session.token is called by the session engine for the agents it starts, not by {}",
            ctx.principal.identity_ref
        )));
    }
    let identity_ref = format!("{ENGINE_SESSION_ACTOR_PREFIX}{}", params.engine_session_id);
    let scopes = session_scopes(ctx)?;
    let minted = if params.revoke {
        None
    } else {
        Some(minted_token()?)
    };
    let mut writing = write_of(ctx);
    let engine_session_id = params.engine_session_id.clone();
    let actor_ref = identity_ref.clone();
    let why = reason.clone();
    let revoke = params.revoke;
    let minted_scopes = scopes;
    let token_id = fresh(&writing, "tok");
    let minted_at = now(&writing);
    let actor_id_new = fresh(&writing, "act");
    let revoked = audited_write(&mut writing, SESSION_TOKEN, &reason, move |txn, _actor| {
        let existing = txn.actor_by_identity(&actor_ref)?;
        let holder_id = match &existing {
            Some(actor) => actor.id.clone(),
            None => {
                let short = engine_session_id.get(..8).unwrap_or(&engine_session_id);
                txn.insert_actor(&Actor {
                    id: actor_id_new.clone(),
                    kind: ActorKind::Agent,
                    display_name: format!("Engine session {short}"),
                    identity_ref: actor_ref.clone(),
                    disabled: false,
                    created_at: minted_at,
                })?;
                actor_id_new
            }
        };
        let mut revoked = 0i64;
        // Python: "session ended" on a revoke, "superseded: <reason>" on a mint.
        let revoke_why = if revoke {
            "session ended".to_string()
        } else {
            format!("superseded: {why}")
        };
        for token in txn.tokens_for_actor(&holder_id, false)? {
            if token.revoked_at.is_none() && txn.revoke_token(&token.id, &revoke_why, minted_at)? {
                revoked += 1;
            }
        }
        let secret = if let Some((secret, token_hash)) = minted {
            txn.insert_token(
                &Token {
                    id: token_id,
                    actor_id: holder_id.clone(),
                    actor_identity_ref: Some(actor_ref.clone()),
                    name: format!("engine session {engine_session_id}"),
                    scopes: minted_scopes,
                    kind: TokenKind::Api,
                    created_at: minted_at,
                    expires_at: None,
                    last_used_at: None,
                    revoked_at: None,
                    revoked_reason: None,
                },
                &token_hash,
            )?;
            Some(secret)
        } else {
            None
        };
        let event = if secret.is_none() && revoke {
            "session.token_revoked"
        } else {
            "session.token_issued"
        };
        let minted = secret.is_some();
        Ok(WriteOutcome::new(
            (revoked, secret),
            "actor",
            &holder_id,
            json!({"identity_ref": actor_ref, "kind": "agent"}),
            event,
            json!({
                "engine_session_id": engine_session_id,
                "minted": minted,
                "revoked": revoked,
            }),
        ))
    })?;
    let (revoked, secret) = revoked;
    Ok(SessionTokenResult {
        engine_session_id: params.engine_session_id.clone(),
        actor: identity_ref,
        token: secret,
        revoked,
    })
}

/// A serde failure while rendering a result the service just built.
fn json_error(error: serde_json::Error) -> VogtError {
    VogtError::InvalidRequest(format!("could not render the result: {error}"))
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
        answers_as_person, hibernate, input, relayed_overseer, remove_session_op,
        rename_session_op, session_token_op, wake, HibernateParams, InputParams, WakeParams,
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
                    purpose: None,
                    started_by_session: None,
                    started_by_kind: None,
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
    fn only_an_oversight_session_is_relayed_as_overseer() {
        let oversight = br#"{"id":"eng-boss","alive":true,"activity":"idle","role":"oversight"}"#;
        let (_dir, built, _) = context(
            "overseer",
            agent("agent:engine:eng-boss"),
            None,
            |_| {},
            vec![oversight.to_vec()],
        );
        with_ctx!(&built, |ctx| {
            let engine = ctx.engine.as_ref().unwrap();
            assert_eq!(
                relayed_overseer(ctx, engine, false).unwrap().as_deref(),
                Some("eng-boss")
            );
        });
        drop(built);
        // A plain worker is not relayed, so the engine still refuses it.
        let (_dir, built, _) = context(
            "not-overseer",
            agent("agent:engine:eng-1"),
            None,
            |_| {},
            vec![live("eng-1")],
        );
        with_ctx!(&built, |ctx| {
            let engine = ctx.engine.as_ref().unwrap();
            assert_eq!(relayed_overseer(ctx, engine, false).unwrap(), None);
            // A person needs no relay and asks the engine nothing.
            assert_eq!(relayed_overseer(ctx, engine, true).unwrap(), None);
        });
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

    fn renamed(id: &str, name: &str) -> Vec<u8> {
        format!(r#"{{"id":"{id}","alive":true,"activity":"idle","role":"worker","name":"{name}"}}"#)
            .into_bytes()
    }

    #[test]
    fn renaming_a_session_asks_the_engine_and_returns_the_new_name() {
        let (_dir, built, seen) = context(
            "rename",
            person(),
            None,
            |_| {},
            vec![renamed("eng-1", "after"), b"{}".to_vec()],
        );
        record_session(&built, "ses_1", "eng-1");
        let result = rename_session_op(
            &built,
            json!({"id": "ses_1", "name": "after", "reason": "clearer"}),
        )
        .unwrap();
        let calls = seen.lock().unwrap();
        assert!(calls.iter().any(|(method, path, body)| {
            method == "PATCH" && path.ends_with("/eng-1") && body.contains("\"name\":\"after\"")
        }));
        assert_eq!(result["session"]["id"], "ses_1");
    }

    #[test]
    fn renaming_with_an_empty_name_is_refused_before_the_engine() {
        let (_dir, built, seen) = context("rename-empty", person(), None, |_| {}, vec![]);
        record_session(&built, "ses_1", "eng-1");
        let error = rename_session_op(
            &built,
            json!({"id": "ses_1", "name": "   ", "reason": "no"}),
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
        assert!(seen.lock().unwrap().is_empty());
    }

    fn live_session(id: &str) -> Vec<u8> {
        format!(r#"{{"id":"{id}","alive":true,"activity":"idle","role":"worker"}}"#).into_bytes()
    }

    #[test]
    fn removing_a_live_session_kills_it_with_reason_before_forgetting_it() {
        let (_dir, built, seen) = context(
            "remove",
            person(),
            None,
            |_| {},
            vec![live_session("eng-1"), b"{}".to_vec()],
        );
        let old = record_session(&built, "ses_1", "eng-1");
        let result = remove_session_op(&built, json!({"id": "ses_1", "reason": "done"})).unwrap();
        assert_eq!(result["session"]["id"], "ses_1");
        assert!(result["session"]["stopped_at"].is_string());
        let calls = seen.lock().unwrap().clone();
        let kill = calls
            .iter()
            .position(|(method, path, body)| {
                method == "POST"
                    && path.ends_with("/eng-1/kill")
                    && body.contains("\"reason\":\"done\"")
                    && body.contains("\"by\":\"local:ada\"")
            })
            .expect("kill with reason");
        let delete = calls
            .iter()
            .position(|(method, path, _)| method == "DELETE" && path.ends_with("/eng-1"))
            .expect("delete");
        assert!(kill < delete, "killed before forgotten: {calls:?}");
        let tokens = tokens_of(&built, "local:ada");
        assert!(tokens
            .iter()
            .find(|token| token.id == old)
            .unwrap()
            .revoked_at
            .is_some());
    }

    #[test]
    fn removing_an_unknown_unlinked_session_is_not_found() {
        let (_dir, built, _) = context(
            "remove-missing",
            person(),
            None,
            |_| {},
            vec![b"null".to_vec()],
        );
        let error =
            remove_session_op(&built, json!({"id": "eng-9", "reason": "gone"})).unwrap_err();
        assert!(matches!(error, VogtError::NotFound(_)), "{error:?}");
    }

    #[test]
    fn removing_an_unlinked_live_session_kills_then_forgets_it() {
        let (_dir, built, seen) = context(
            "remove-unlinked",
            person(),
            None,
            |_| {},
            vec![live_session("eng-9"), b"{}".to_vec()],
        );
        let result = remove_session_op(&built, json!({"id": "eng-9", "reason": "gone"})).unwrap();
        assert_eq!(result["session"]["linked"], false);
        assert_eq!(result["session"]["alive"], false);
        let calls = seen.lock().unwrap().clone();
        let kill = calls
            .iter()
            .position(|(method, path, _)| method == "POST" && path.ends_with("/eng-9/kill"))
            .unwrap();
        let delete = calls
            .iter()
            .position(|(method, path, _)| method == "DELETE" && path.ends_with("/eng-9"))
            .unwrap();
        assert!(kill < delete);
    }

    const ENGINE_UUID: &str = "0cc46582-ed57-4327-9a28-624a7ae2474a";

    #[test]
    fn only_the_engine_may_mint_a_session_token() {
        let (_dir, built, _) = context("token-refused", person(), None, |_| {}, vec![]);
        let error = session_token_op(
            &built,
            json!({"engine_session_id": ENGINE_UUID, "reason": "rotated"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::EngineOnly(_)),
            "a person is not the engine: {error:?}"
        );
        let (_dir, built, _) = context(
            "token-agent",
            agent(&format!("agent:engine:{ENGINE_UUID}")),
            None,
            |_| {},
            vec![],
        );
        let error = session_token_op(
            &built,
            json!({"engine_session_id": ENGINE_UUID, "reason": "rotated"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::EngineOnly(_)),
            "the session's own actor is not the engine: {error:?}"
        );
    }

    #[test]
    fn the_engine_mints_an_agent_token_and_a_second_mint_supersedes_it() {
        let (_dir, built, _) = context(
            "token-engine",
            agent("agent:vogt-engine"),
            None,
            |_| {},
            vec![],
        );
        let first = session_token_op(
            &built,
            json!({"engine_session_id": ENGINE_UUID, "reason": "launch"}),
        )
        .unwrap();
        assert_eq!(first["actor"], format!("agent:engine:{ENGINE_UUID}"));
        assert_eq!(first["revoked"], 0);
        assert!(first["token"].as_str().unwrap().starts_with("vogt_"));
        let second = session_token_op(
            &built,
            json!({"engine_session_id": ENGINE_UUID, "reason": "wake"}),
        )
        .unwrap();
        assert_eq!(second["revoked"], 1);
        assert_ne!(second["token"], first["token"]);
        let gone = session_token_op(
            &built,
            json!({"engine_session_id": ENGINE_UUID, "revoke": true, "reason": "stop"}),
        )
        .unwrap();
        assert!(
            gone["token"].is_null(),
            "a revoke returns token: null, {gone}"
        );
        assert_eq!(gone["revoked"], 1);
        with_ctx!(&built, |ctx| {
            let view = ctx.declared.read().unwrap();
            let holder = view
                .actor_by_identity(&format!("agent:engine:{ENGINE_UUID}"))
                .unwrap()
                .unwrap();
            assert_eq!(
                holder.display_name,
                format!("Engine session {}", &ENGINE_UUID[..8])
            );
            let records = view
                .list_audit(&crate::storage::interface::AuditQuery {
                    limit: 20,
                    offset: 0,
                    actor_id: None,
                    operation: Some("session.token".to_string()),
                    entity_id: Some(holder.id.clone()),
                    project_id: None,
                    since: None,
                    until: None,
                })
                .unwrap();
            assert!(
                records.iter().any(|row| row.entity_kind == "actor"),
                "the audit row is the actor, not the session: {records:?}"
            );
            let events = view.list_events(0, 50, Some(&holder.id)).unwrap();
            let revoked = events
                .iter()
                .find(|event| event.kind == "session.token_revoked")
                .expect("a revoke publishes session.token_revoked");
            assert_eq!(revoked.summary["minted"], false);
            assert_eq!(revoked.summary["revoked"], 1);
            let issued = events
                .iter()
                .find(|event| event.kind == "session.token_issued")
                .expect("a mint publishes session.token_issued");
            assert_eq!(issued.summary["minted"], true);
            assert_eq!(issued.summary["engine_session_id"], ENGINE_UUID);
        });
        let tokens = tokens_of(&built, &format!("agent:engine:{ENGINE_UUID}"));
        assert!(tokens.iter().all(|token| token.revoked_at.is_some()));
    }

    #[test]
    fn a_session_id_that_is_not_a_uuid_is_refused() {
        let (_dir, built, _) = context(
            "token-shape",
            agent("agent:vogt-engine"),
            None,
            |_| {},
            vec![],
        );
        let error = session_token_op(
            &built,
            json!({"engine_session_id": "../../etc", "reason": "no"}),
        )
        .unwrap_err();
        assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    }
}
