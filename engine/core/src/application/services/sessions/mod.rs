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

use crate::adapters::engine::{
    CreateSession, EngineApproval, EngineBlocked, EngineClient, EngineHibernation, EngineResources,
    EngineScreen, EngineSession, EngineSweepEntry, EngineWait,
};
use crate::adapters::transcripts;
use crate::application::brief::{self, ComposedBrief, WhyResult, WorkBrief};
use crate::application::context::{write_of, AppContext};
use crate::application::dispatch as dispatch_profiles;
use crate::application::resolve;
use crate::application::session_blocked::{self, known_or, BOUNDARIES, NEEDS};
use crate::application::session_commit;
use crate::application::writes::{self, audited_action, audited_write, WriteOutcome};
use crate::auth::{self, TOKEN_ENTROPY_BYTES};
use crate::core::cold_wake;
use crate::core::{py_repr, Actor, ActorKind, CodingSession, Moment, Token, TokenKind, WorkItem};
use crate::core::{Clock, IdFactory};
use crate::decisions;
use crate::delivery::{self};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const SESSION_START: &str = "session.start";
/// The audit operation (and event) an oversight `override` writes beside the
/// start it allowed (WI-1329); the Inbox lists them.
pub const DISPATCH_OVERRIDE: &str = "dispatch.override";
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
const MAX_ENTERS: usize = 3;
const SETTLE: Duration = Duration::from_secs(10);
const READY_GATE: Duration = Duration::from_secs(30);
const TYPED_READS: usize = 10;

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

/// The columns a `starters_only` read may name, all held on the declared
/// session row: the GUI rail nests a session under the one that started it
/// (WI-4531), and a terminal reads the work item its session was opened for
/// (WI-4543).
const STARTER_FIELDS: [&str; 3] = ["engine_session_id", "started_by_session", "work_item"];

/// The starters path is asked for with `starters_only: true` (WI-4543), never
/// inferred from `fields`, so a caller that names starter columns alone still
/// gets the ordinary full rows. The flag needs `mode=full`, `fields` naming
/// only declared-row columns, and no filter that needs the engine's state.
fn starters_only(
    parsed: &ListSessionsParams,
    flag: bool,
    started_by_session_set: bool,
) -> Result<bool, VogtError> {
    if !flag {
        return Ok(false);
    }
    let fields = parsed.fields.as_deref().unwrap_or_default();
    let usable = parsed.mode == "full"
        && !fields.is_empty()
        && fields
            .iter()
            .all(|field| STARTER_FIELDS.contains(&field.as_str()))
        && parsed.order == "started"
        && parsed.states.is_none()
        && !parsed.include_hidden
        && !started_by_session_set;
    if usable {
        Ok(true)
    } else {
        Err(VogtError::InvalidRequest(
            "starters_only needs mode=full, fields naming only engine_session_id, started_by_session and work_item, order=started, and no states, include_hidden or started_by_session"
                .to_string(),
        ))
    }
}

/// Each Vogt session's `id` plus the named starter columns, read from the
/// declared table only: no engine call and no transcript read (WI-4531).
/// Unlinked engine sessions have no starter and are not listed. The page is
/// cut in the query; `total` is one past the page when more rows follow.
fn list_starters<C, I>(
    ctx: &AppContext<C, I>,
    params: &ListSessionsParams,
) -> Result<Value, VogtError>
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
    let limit = params.limit.max(0);
    let offset = params.offset.max(0);
    let mut rows = view.list_sessions(
        project_id.as_deref(),
        work_item_id.as_deref(),
        params.include_stopped,
        limit.saturating_add(1),
        offset,
    )?;
    let truncated = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let fields = params.fields.as_deref().unwrap_or_default();
    // The item's ref is read once per item, and only when it was asked for.
    let mut refs: std::collections::BTreeMap<String, Option<String>> = Default::default();
    if fields.iter().any(|wanted| wanted == "work_item") {
        let mut ids: Vec<String> = rows
            .iter()
            .filter_map(|row| row.work_item_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        for (id, reference, _, _) in view.work_briefs_by_ids(&ids)? {
            refs.insert(id, Some(reference));
        }
        for id in ids {
            refs.entry(id).or_insert(None);
        }
    }
    drop(view);
    let sessions: Vec<Value> = rows
        .iter()
        .map(|session| {
            let mut row = serde_json::Map::new();
            row.insert("id".to_string(), Value::String(session.id.clone()));
            for field in STARTER_FIELDS {
                if !fields.iter().any(|wanted| wanted == field) {
                    continue;
                }
                let value = match field {
                    "engine_session_id" => Value::String(session.engine_session_id.clone()),
                    "work_item" => session
                        .work_item_id
                        .as_deref()
                        .and_then(|id| refs.get(id).cloned().flatten())
                        .map_or(Value::Null, Value::String),
                    _ => session
                        .started_by_session
                        .clone()
                        .map_or(Value::Null, Value::String),
                };
                row.insert(field.to_string(), value);
            }
            Value::Object(row)
        })
        .collect();
    let total = offset + sessions.len() as i64 + i64::from(truncated);
    Ok(serde_json::json!({
        "sessions": sessions,
        "digests": [],
        "engine": null,
        "total": total,
        "truncated": truncated,
        "hidden_count": 0,
    }))
}

/// Slim `mode=full` rows to `id` plus the named `fields` (WI-3130). A full page
/// of the whole roster is over a megabyte; a caller that reads a few columns
/// names them. No `fields` (the default) leaves every row whole, which is what
/// the Python core returns, and an unknown name is left out, as a digest's
/// `extra` leaves it.
pub(super) fn slim_full_rows(listed: &mut Value, fields: Option<&[String]>) {
    let Some(rows) = listed.get_mut("sessions").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(fields) = fields.filter(|fields| !fields.is_empty()) else {
        // The launch line is long and repeated on every row; it is left out
        // unless `fields` asks for it (WI-4725).
        for row in rows {
            if let Some(row) = row.as_object_mut() {
                row.remove("command");
            }
        }
        return;
    };
    for row in rows {
        let Some(whole) = row.as_object() else {
            continue;
        };
        let slim: serde_json::Map<String, Value> = whole
            .iter()
            .filter(|(name, _)| name.as_str() == "id" || fields.iter().any(|f| f == *name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        *row = Value::Object(slim);
    }
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
        escalated_to: None,
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
    let (live, detail, hidden_count) = match ctx.engine.as_ref() {
        None => (
            Vec::new(),
            Some("no session engine is configured (VOGT_ENGINE_URL is unset)".to_string()),
            0,
        ),
        Some(engine) => match engine.list_sessions_counted(params.include_hidden) {
            // Kept as a Vec: the engine's order is the order unlinked rows
            // append in, and a HashMap would shuffle it.
            Ok((rows, hidden)) => (rows, None, hidden as i64),
            Err(VogtError::EngineUnavailable(text)) => (Vec::new(), Some(text), 0),
            Err(error) => return Err(error),
        },
    };
    // Python's session list reads the clock once here, after the engine
    // answers or does not, and before the rows are assembled. The value is
    // the attention reference.
    let now = super::now_of(&ctx.clock);
    let full = params.mode == "full";
    // Full mode keeps every joined row, and rss order reads only the engine's
    // sample, so the page can be cut before any transcript is read (WI-4531).
    // Digest mode filters on `running`, which the transcript decides.
    let (mut joined, declared_overflow) = if full {
        joined_rows(ctx, params, &live, detail.as_deref())?
    } else {
        rows_of(ctx, params, &live, detail.as_deref())?
    };
    let stall_after = params.stall_after_minutes.saturating_mul(60);
    // The starter filter holds on every path: full or digest, with or without
    // `states`, linked or not (WI-4725).
    if let Some(starter) = params.started_by_session.as_deref() {
        let starter = resolve_target(ctx, starter)?.engine_session_id;
        joined.retain(|summary| summary.started_by_session.as_deref() == Some(starter.as_str()));
    }
    let mut kept = Vec::new();
    for summary in joined.drain(..) {
        if full
            || digest_kept(
                &summary,
                attention_name(&summary, now, stall_after),
                params.states.as_deref(),
            )
        {
            kept.push(summary);
        }
    }
    if params.order == "rss" {
        // Stable: sessions with no sample keep their order, after the
        // measured ones. Sweep shares rows_of and must not inherit this.
        kept.sort_by(|left, right| {
            let rss =
                |row: &SessionSummary| row.resources.as_ref().map(|r| r.rss_bytes).unwrap_or(0);
            match (left.resources.is_none(), right.resources.is_none()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => rss(right).cmp(&rss(left)),
            }
        });
    }
    let total = kept.len() as i64;
    let offset = params.offset.max(0) as usize;
    let limit = params.limit.max(0) as usize;
    let mut page: Vec<SessionSummary> = kept.into_iter().skip(offset).take(limit).collect();
    if full {
        read_transcripts(ctx, &mut page, &live);
    }
    // When the declared fetch hits its ceiling, more links match than were
    // joined, so `total` is a floor and the list stays truncated.
    let mut reported = total;
    let truncated = (offset as i64) + (page.len() as i64) < total || declared_overflow;
    if declared_overflow {
        reported += 1;
    }
    if full {
        return Ok(SessionListResult {
            sessions: page,
            digests: Vec::new(),
            engine: detail,
            total: reported,
            truncated,
            hidden_count,
        });
    }
    let digests = page
        .iter()
        .map(|summary| {
            digest_of(
                summary,
                attention_name(summary, now, stall_after),
                params.fields.as_deref(),
            )
        })
        .collect();
    Ok(SessionListResult {
        sessions: Vec::new(),
        digests,
        engine: detail,
        total: reported,
        truncated,
        hidden_count,
    })
}

/// One past the operation's own page ceiling, so a match beyond it is
/// overflow rather than a silently complete list. Python `_DECLARED_FETCH`.
const DECLARED_FETCH: i64 = 501;

const DIGEST_LIVE: &[&str] = &[
    "running", "idle", "waiting", "blocked", "approval", "stalled", "done", "unknown",
];
const EXCLUDED_STATES: &[&str] = &["hibernated", "stopped", "exited"];
const DIGEST_CLIP: usize = 120;

fn attention_name(
    summary: &SessionSummary,
    now: crate::core::Moment,
    stall_after_secs: i64,
) -> String {
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
        summary
            .last_output_at
            .as_deref()
            .and_then(|text| crate::core::from_iso(text).ok()),
        now,
        stall_after_secs,
        summary.done_at.as_deref(),
    )
    .attention
    .as_str()
    .to_string()
}

fn state_of(summary: &SessionSummary, attention: &str) -> Option<String> {
    if summary.activity.as_deref() == Some("stopped") || attention == "exited" {
        return Some(if attention == "exited" {
            "exited".to_string()
        } else {
            summary
                .activity
                .clone()
                .unwrap_or_else(|| attention.to_string())
        });
    }
    summary.activity.clone().or_else(|| {
        if attention.is_empty() {
            None
        } else {
            Some(attention.to_string())
        }
    })
}

/// Default listings drop hibernated, stopped and exited. An empty `states`
/// list means the caller asked for every state.
fn digest_kept(summary: &SessionSummary, attention: String, states: Option<&[String]>) -> bool {
    if states.is_some_and(|wanted| wanted.is_empty()) {
        return true;
    }
    let allowed: Vec<&str> = match states {
        None => DIGEST_LIVE.to_vec(),
        Some(wanted) => wanted.iter().map(String::as_str).collect(),
    };
    let state = state_of(summary, &attention);
    let Some(state) = state else {
        return allowed.contains(&"unknown");
    };
    if allowed.contains(&state.as_str()) || allowed.contains(&attention.as_str()) {
        return true;
    }
    !EXCLUDED_STATES.contains(&state.as_str()) && allowed.contains(&state.as_str())
}

fn clip(text: &str) -> String {
    text.chars().take(DIGEST_CLIP).collect()
}

fn digest_of(
    summary: &SessionSummary,
    attention: String,
    fields: Option<&[String]>,
) -> SessionDigestRow {
    let blocked = summary
        .blocked
        .as_ref()
        .map(|blocked| blocked.blocker.clone());
    let approval = summary.approval.as_ref();
    let report = summary.latest_report.clone();
    let excerpt = if report.is_some() {
        None
    } else {
        summary.last_reply_excerpt.as_ref().map(|text| clip(text))
    };
    SessionDigestRow {
        id: summary.id.clone(),
        name: summary.name.clone(),
        work_item: summary.work_item.clone(),
        activity: summary.activity.clone(),
        attention: Some(attention),
        ready: Some(summary.activity.as_deref() == Some("waiting-for-input")),
        blocked_reason: blocked.as_ref().map(|text| clip(text)),
        blocked_detail: blocked.filter(|text| text.chars().count() > DIGEST_CLIP),
        turn_started_at: summary.turn_started_at.clone(),
        done_at: summary.done_at.clone(),
        last_reply_excerpt: excerpt,
        report,
        template: summary.template.clone(),
        brief_file: None,
        question: approval.map(|approval| approval.question.clone()),
        question_kind: approval.map(|approval| approval.kind.clone()),
        options: approval.and_then(|approval| {
            if approval.options.is_empty() {
                None
            } else {
                Some(
                    approval
                        .options
                        .iter()
                        .map(|option| option.label.clone())
                        .collect(),
                )
            }
        }),
        budget: budget_of(summary.economics.as_ref()),
        supervisor: None,
        extra: digest_extra(summary, fields),
    }
}

fn budget_of(economics: Option<&serde_json::Map<String, Value>>) -> Option<String> {
    let payload = economics?;
    if payload.is_empty() {
        return None;
    }
    let int_of = |name: &str| -> i64 {
        match payload.get(name) {
            // Python `_int_of` accepts real ints and rejects bools. JSON has
            // no bool-as-number, so a number is enough; a float is not an int.
            Some(Value::Number(number)) if number.is_i64() => number.as_i64().unwrap_or(0),
            _ => 0,
        }
    };
    let reading = crate::core::session_economics::SessionEconomics {
        turns: int_of("turns"),
        context_tokens: int_of("context_tokens"),
        peak_context_tokens: int_of("peak_context_tokens"),
        compactions: int_of("compactions"),
        tool_result_bytes: match payload.get("tool_result_bytes") {
            Some(Value::Object(tools)) => tools
                .iter()
                .filter_map(|(name, value)| value.as_i64().map(|number| (name.clone(), number)))
                .collect(),
            _ => Default::default(),
        },
        reads_repeated: int_of("reads_repeated"),
        output_tokens: int_of("output_tokens"),
        usage_measured: payload.get("usage_measured") == Some(&Value::Bool(true)),
        ..Default::default()
    };
    Some(reading.budget_line(None, None))
}

fn digest_extra(
    summary: &SessionSummary,
    fields: Option<&[String]>,
) -> Option<serde_json::Map<String, Value>> {
    let wanted = fields?;
    if wanted.is_empty() {
        return None;
    }
    let dumped = serde_json::to_value(summary).unwrap_or(Value::Null);
    let object = dumped.as_object();
    let mut extra = serde_json::Map::new();
    for name in wanted {
        if name == "command" {
            continue;
        }
        if let Some(value) = object.and_then(|object| object.get(name)) {
            extra.insert(name.clone(), value.clone());
        }
    }
    if extra.is_empty() {
        None
    } else {
        Some(extra)
    }
}

/// Newest report per engine session, already formatted as the digest line.
/// A missing table or a locked database is no report, not a failed list.
fn latest_report_lines<C, I>(
    ctx: &AppContext<C, I>,
    ids: &[String],
) -> std::collections::HashMap<String, String>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    use std::collections::HashMap;
    let mut found = HashMap::new();
    let wanted: Vec<&str> = ids
        .iter()
        .filter(|id| !id.is_empty())
        .map(String::as_str)
        .collect();
    if wanted.is_empty() {
        return found;
    }
    let path = ctx.declared.path().to_path_buf();
    let rows = crate::storage::sqlite::connection::latest_report_rows(&path, &wanted);
    for (kind, summary, data, engine_id) in rows {
        let Some(engine_id) = engine_id else { continue };
        found.insert(engine_id, report_line(&kind, &summary, &data));
    }
    found
}

/// Python `reports.one_line`, clipped at 200 rather than the excerpt's 120.
fn report_line(kind: &str, summary: &str, data: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(data).unwrap_or(Value::Null);
    let mut extras = Vec::new();
    if let Some(branch) = parsed
        .get("branch")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        extras.push(format!("branch={branch}"));
    }
    if let Some(sha) = parsed
        .get("sha")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        let short: String = sha.chars().take(12).collect();
        extras.push(format!("sha={short}"));
    }
    if let Some(pr) = parsed.get("pr").filter(|value| !value.is_null()) {
        let text = match pr {
            Value::String(text) if !text.is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        };
        if let Some(text) = text {
            extras.push(format!("pr={text}"));
        }
    }
    let mut line = format!("{kind}: {summary}");
    if !extras.is_empty() {
        line = format!("{line} [{}]", extras.join(" "));
    }
    let count = line.chars().count();
    if count <= 200 {
        line
    } else {
        let head: String = line.chars().take(199).collect();
        format!("{head}…")
    }
}

/// The rows `session.list` and `session.sweep` share, before any transcript
/// is read.
///
/// The second value is true when more declared links match than
/// `DECLARED_FETCH` read, so the caller's `total` is a floor.
fn joined_rows<C, I>(
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
    // The page is cut after the state filter, so the declared half is the
    // joined set, not this caller's page. One past the operation ceiling.
    let recorded = view.list_sessions(
        project_id.as_deref(),
        work_item_id.as_deref(),
        params.include_stopped,
        DECLARED_FETCH,
        0,
    )?;
    let declared_overflow = recorded.len() as i64 == DECLARED_FETCH;
    let recorded: Vec<_> = if declared_overflow {
        recorded
            .into_iter()
            .take((DECLARED_FETCH - 1) as usize)
            .collect()
    } else {
        recorded
    };
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
    // Joined before the page is cut, matching Python: an offset applied here
    // would drop unlinked sessions from every page but the first.
    if unfiltered {
        let view = ctx.declared.read()?;
        for engine_session in live {
            // A hibernated session is asleep, not stopped: listed always. An
            // exited one is the engine's leftover, listed only when stopped
            // sessions were asked for. Linked rows are not filtered here —
            // the declared query already applied include_stopped.
            // An empty states list asks for every state, including an
            // unlinked session whose process has exited. Otherwise that
            // leftover is listed only when stopped sessions were asked for.
            let every_state = params.states.as_ref().is_some_and(Vec::is_empty);
            if !engine_session.alive
                && !engine_session.hibernated()
                && !params.include_stopped
                && !every_state
            {
                continue;
            }
            if view.session_by_engine_id(&engine_session.id)?.is_none() {
                summaries.push(unlinked_summary(&view, engine_session)?);
            }
        }
    }
    // The latest typed report per engine session, attached before the
    // transcript loop so a report can replace the reply excerpt (WI-2039).
    let ids: Vec<String> = summaries
        .iter()
        .map(|summary| summary.engine_session_id.clone())
        .collect();
    let reports = latest_report_lines(ctx, &ids);
    for summary in &mut summaries {
        if let Some(line) = reports.get(&summary.engine_session_id) {
            summary.latest_report = Some(line.clone());
        }
    }
    Ok((summaries, declared_overflow))
}

#[cfg(test)]
thread_local! {
    /// The engine sessions `read_transcripts` looked up, in order (WI-4531).
    pub(super) static TRANSCRIPT_READS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The joined rows with their transcript-derived fields filled in.
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
    let (mut summaries, declared_overflow) = joined_rows(ctx, params, live, detail)?;
    read_transcripts(ctx, &mut summaries, live);
    Ok((summaries, declared_overflow))
}

/// Fills `running`, the engine's template and `last_reply_excerpt` from each
/// live session's transcript. Callers run it on as few rows as they can: it
/// is file I/O per live session (WI-4531).
fn read_transcripts<C, I>(
    ctx: &AppContext<C, I>,
    summaries: &mut [SessionSummary],
    live: &[EngineSession],
) where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    // Outside the read transaction: this is file I/O, and an excerpt that
    // cannot be had is a null, never an error. Only a session the engine
    // still has alive or asleep gets a transcript read.
    for summary in summaries.iter_mut() {
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
        #[cfg(test)]
        TRANSCRIPT_READS.with(|reads| reads.borrow_mut().push(summary.engine_session_id.clone()));
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
    let (listed, _declared_overflow) = rows_of(
        ctx,
        &ListSessionsParams {
            project: params.project.clone(),
            work_item: None,
            include_stopped: false,
            limit: 500,
            offset: 0,
            order: "started".to_string(),
            mode: "full".to_string(),
            states: None,
            stall_after_minutes: params.stall_after_minutes,
            include_hidden: false,
            fields: None,
            started_by_session: None,
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
                .map(|blocked| blocked.blocker.as_str()),
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
            summary.done_at.as_deref(),
        );
        // The overseer's recorded pick, shown on a dialog waiting for an
        // answer (WI-996 E0b); never chosen for the person.
        let recommendation = match (verdict.attention.as_str(), entry.session.approval.as_ref()) {
            ("approval", Some(approval)) if !approval.options.is_empty() => {
                let view = ctx.declared.read()?;
                session_blocked::dialog_recommendation(&view, &entry.session.id, approval)?
            }
            _ => None,
        };
        rows.push(SessionSweepRow {
            attention: verdict.attention.as_str().to_string(),
            attention_reason: verdict.reason.clone(),
            screen_tail: entry.screen_tail.clone(),
            ready: Some(entry.ready),
            recommendation,
            session: summary,
        });
    }
    narrow_sweep(ctx, params, &mut rows)?;
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

/// Apply `exclude_roles`, `supervised_by` and `since` (WI-1525). Counts are
/// taken from what is left, so a sweep that hides the person's own terminals
/// does not report them as sessions needing attention.
fn narrow_sweep<C, I>(
    ctx: &AppContext<C, I>,
    params: &SweepSessionsParams,
    rows: &mut Vec<SessionSweepRow>,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if !params.exclude_roles.is_empty() {
        rows.retain(|row| {
            !row.session
                .role
                .as_deref()
                .is_some_and(|role| params.exclude_roles.iter().any(|wanted| wanted == role))
        });
    }
    if let Some(supervisor) = params.supervised_by.as_deref() {
        let supervisor = resolve_target(ctx, supervisor)?.engine_session_id;
        rows.retain(|row| row.session.started_by_session.as_deref() == Some(supervisor.as_str()));
    }
    if let Some(since) = params.since.as_deref() {
        let since = crate::core::from_iso(since).map_err(|_| {
            VogtError::InvalidRequest(format!(
                "since must be an ISO-8601 time, got {}",
                crate::core::py_repr(since)
            ))
        })?;
        let after = |text: Option<&str>| {
            text.and_then(|text| crate::core::from_iso(text).ok())
                .is_some_and(|moment| moment > since)
        };
        rows.retain(|row| {
            let session = &row.session;
            after(session.last_output_at.as_deref())
                || after(session.done_at.as_deref())
                || after(session.turn_started_at.as_deref())
                || after(
                    session
                        .blocked
                        .as_ref()
                        .and_then(|blocked| blocked.since.as_deref()),
                )
        });
    }
    Ok(())
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
    let seen = if confirming {
        engine.get_session(&engine_id).ok().flatten()
    } else {
        None
    };
    let mut before = seen.as_ref().map(|seen| seen.activity.clone());
    let stepwise = confirming && params.submit && !text.is_empty() && keys.is_empty();
    let mut woke = false;
    let mut stepwise_verdict = None;
    let sent = if stepwise && seen.as_ref().is_some_and(has_input_box) {
        if seen.as_ref().is_some_and(EngineSession::hibernated) {
            Err(VogtError::Conflict("hibernated".to_string()))
        } else {
            type_then_submit(
                engine,
                &engine_id,
                &params.id,
                &text,
                before.as_deref(),
                person,
            )
            .map(|verdict| stepwise_verdict = Some(verdict))
        }
    } else {
        send_all(
            engine,
            &engine_id,
            &params.id,
            &writes_in_order,
            person,
            overseer.as_deref(),
        )
    };
    if let Err(refused) = sent {
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
            false,
        )?;
        woke = true;
        let waited = engine.wait_session(
            &engine_id,
            "ready",
            Duration::from_secs(params.wake_timeout_s.max(0) as u64),
        )?;
        let from_relay = overseer.is_some() || person;
        let ready = waited
            .as_ref()
            .is_some_and(|waited| at_prompt_for_input(waited, from_relay));
        if !ready {
            let outcome = waited
                .as_ref()
                .map(|waited| waited.outcome.clone())
                .unwrap_or_else(|| "no answer".to_string());
            return Err(VogtError::Conflict(format!(
                "woke session {}, but it was not ready for input within {}s ({outcome}); nothing was typed. {}",
                py_repr(&params.id),
                params.wake_timeout_s,
                not_ready_next_step(&outcome)
            )));
        }
        // Freshly woken and at its prompt: nothing was running.
        before = Some("waiting-for-input".to_string());
        if stepwise && current.as_ref().is_some_and(has_input_box) {
            stepwise_verdict = Some(type_then_submit(
                engine,
                &engine_id,
                &params.id,
                &text,
                before.as_deref(),
                person,
            )?);
        } else {
            send_all(
                engine,
                &engine_id,
                &params.id,
                &writes_in_order,
                person,
                overseer.as_deref(),
            )?;
        }
    }
    let verdict = if let Some(verdict) = stepwise_verdict {
        verdict
    } else if confirming {
        confirm_delivery(
            engine,
            &engine_id,
            before.as_deref(),
            (!text.is_empty()).then_some(text.as_str()),
        )
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
    let digest = input_digest(ctx, engine, &found, submitted, &reason);
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

/// Audit a write the core itself typed into a session's terminal: a CI
/// conclusion notice, the context-rotation ask. These are system actions, not
/// a caller's `session.input`, but they put text on a PTY all the same, so each
/// leaves the same `session.input` row: the session (the `ses_` id when linked,
/// otherwise the engine UUID), the actor, the byte count and the key names
/// (none here), never the text. `source` names the system path.
pub(crate) fn audit_system_input<C, I>(
    ctx: &AppContext<C, I>,
    engine_id: &str,
    source: &str,
    reason: &str,
    bytes: usize,
    submitted: bool,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let linked = ctx.declared.read()?.session_by_engine_id(engine_id)?;
    let entity = linked
        .as_ref()
        .map(|session| session.id.clone())
        .unwrap_or_else(|| engine_id.to_string());
    let detail = json!({
        "engine_session_id": engine_id,
        "linked": linked.is_some(),
        "bytes": bytes,
        "keys": Vec::<String>::new(),
        "submit": submitted,
        "submitted": submitted,
        "person": false,
        "oversight": false,
        "system": true,
        "source": source,
    });
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        SESSION_INPUT,
        reason,
        "session",
        &entity,
        &detail,
        "session.input",
        Some(&detail),
    )
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
            let Some(screen) = screen else {
                // The activity check above only reaches here when a screen was
                // returned and it says hibernated. A missing screen is the
                // engine disagreeing with itself, not a reason to panic.
                return Err(VogtError::EngineUnavailable(
                    "the session engine reported a hibernated session with no screen".to_string(),
                ));
            };
            let shown = screen_result(&params.id, &engine_id, &screen);
            return Ok(SessionWaitResult {
                id: params.id.clone(),
                engine_session_id: engine_id.clone(),
                outcome: "hibernated".to_string(),
                matched: false,
                waited_ms: 0,
                screen: params.screen.then(|| shown.clone()),
                digest: if params.screen {
                    None
                } else {
                    wait_digest(ctx, &params.id, &engine_id)
                },
                screen_tail: if params.screen {
                    Vec::new()
                } else {
                    tail_of(&shown)
                },
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
    let shown = screen_result(&params.id, &engine_id, &waited.screen);
    Ok(SessionWaitResult {
        id: params.id.clone(),
        engine_session_id: engine_id.clone(),
        outcome: waited.outcome,
        matched: waited.matched,
        waited_ms: waited.waited_ms,
        screen: params.screen.then(|| shown.clone()),
        digest: if params.screen {
            None
        } else {
            wait_digest(ctx, &params.id, &engine_id)
        },
        screen_tail: if params.screen {
            Vec::new()
        } else {
            tail_of(&shown)
        },
    })
}

/// The last non-blank screen lines, for a wait that did not ask for the
/// whole screen.
fn tail_of(screen: &SessionScreenResult) -> Vec<String> {
    let kept: Vec<&String> = screen
        .lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    kept[kept.len().saturating_sub(WAIT_TAIL_LINES)..]
        .iter()
        .map(|line| (*line).clone())
        .collect()
}

/// The digest of a session as an input or wait leaves it: no attention and no
/// ready flag, which only a sweep computes.
fn plain_digest(summary: &SessionSummary) -> SessionDigestRow {
    let mut row = digest_of(summary, String::new(), None);
    row.attention = None;
    row.ready = None;
    row
}

fn digest_of_target<C, I>(
    ctx: &AppContext<C, I>,
    found: &Target,
    live: Option<&EngineSession>,
) -> Option<SessionDigestRow>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    match (&found.session, live) {
        (Some(session), live) => summarize(ctx, session, live).ok(),
        (None, Some(live)) => ctx
            .declared
            .read()
            .ok()
            .and_then(|view| unlinked_summary(&view, live).ok()),
        (None, None) => None,
    }
    .map(|summary| plain_digest(&summary))
}

fn wait_digest<C, I>(
    ctx: &AppContext<C, I>,
    named: &str,
    engine_id: &str,
) -> Option<SessionDigestRow>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine = engine_of(ctx).ok()?;
    let live = engine.get_session(engine_id).ok().flatten()?;
    let found = resolve_target(ctx, named).ok()?;
    digest_of_target(ctx, &found, Some(&live))
}

/// The digest of the session just typed into, when it can be read. A submitted
/// input also clears the session's blocked report (WI-1834): a new instruction
/// answers what it was blocked on, or supersedes it.
fn input_digest<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    found: &Target,
    submitted: bool,
    reason: &str,
) -> Option<SessionDigestRow>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut live = engine.get_session(&found.engine_session_id).ok().flatten();
    if submitted {
        if let Some(current) = live.take() {
            live = Some(
                clear_block_on(ctx, engine, found, "input", reason, &current).unwrap_or(current),
            );
        }
    }
    digest_of_target(ctx, found, live.as_ref())
}

/// Clear `found`'s blocked report because someone acted on the session. The
/// report it replaces stays in the audit as an unblock naming the cause and
/// what it said. A session acting on itself clears nothing, and a report that
/// cannot be read or cleared is left for the sweep's staleness rule. The
/// session as cleared when one was.
fn clear_block_on<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    found: &Target,
    cause: &str,
    reason: &str,
    current: &EngineSession,
) -> Option<EngineSession>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let engine_id = &found.engine_session_id;
    let caller = ctx.principal.identity_ref.as_str();
    if caller == format!("agent:session:{engine_id}")
        || found
            .session
            .as_ref()
            .is_some_and(|session| caller == format!("agent:session:{}", session.id))
    {
        return None;
    }
    let was = current.blocked.as_ref()?;
    let cleared = engine
        .set_blocked(engine_id, false, None, &[], "person", "other")
        .ok()??;
    let entity = found
        .session
        .as_ref()
        .map_or_else(|| engine_id.clone(), |session| session.id.clone());
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        "session.report_unblocked",
        &format!("cleared by {cause}: {reason}"),
        "session",
        &entity,
        &json!({
            "engine_session_id": engine_id,
            "linked": found.session.is_some(),
            "blocker": Value::Null,
            "items": [],
            "cleared_by": cause,
            "was_blocker": was.reason,
            "was_items": was.items,
            "was_since": was.since,
        }),
        "session.unblocked",
        None,
    )
    .ok()?;
    Some(cleared)
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
    if params.force && !may_force_wake(ctx, engine)? {
        return Err(VogtError::RoleRefused(
            "only a person or an oversight session may force a wake".to_string(),
        ));
    }
    let found = resolve_target(ctx, &params.id)?;
    let result = wake_resolved(ctx, engine, &found, &reason, params.force)?;
    // Woken on purpose: whoever woke it is acting on it, so the report it slept
    // with no longer stands (WI-1834). A session that is still blocked reports
    // again.
    let current = engine.get_session(&found.engine_session_id).ok().flatten();
    if current
        .and_then(|current| clear_block_on(ctx, engine, &found, "wake", &reason, &current))
        .is_some()
    {
        return wake_resolved(ctx, engine, &found, &reason, params.force);
    }
    Ok(result)
}

/// `force` overrides the handoff guard, so only a person, or a session the
/// engine has marked `oversight`, may use it.
fn may_force_wake<C, I>(ctx: &AppContext<C, I>, engine: &EngineClient) -> Result<bool, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    if answers_as_person(ctx)? {
        return Ok(true);
    }
    Ok(relayed_overseer(ctx, engine, false)?.is_some())
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
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
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
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
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
    // A person answers anything; an oversight session is relayed by its
    // engine id and may answer a permission prompt (WI-2028).
    let overseer = relayed_overseer(ctx, engine, person)?;
    let answered = match engine.answer_session(
        &engine_id,
        params.option,
        label,
        params.expect_question.as_deref(),
        person,
        overseer.as_deref(),
    ) {
        // The engine refuses an agent's answer to a permission prompt: the
        // prompt is a person's, so the refusal comes back as an escalation.
        Err(VogtError::PersonRequired(refused)) => {
            return Err(escalated(
                ctx,
                engine,
                &engine_id,
                &refused,
                (params.option, label),
                &reason,
            )?)
        }
        answered => answered?,
    }
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

/// The engine's refusal of an agent's answer to a permission prompt (WI-983),
/// handed back as an escalation (WI-996): the prompt already stands in the
/// Inbox for a person, so the refusal names that entry and the dialog's
/// content key, with `outcome: escalated`. The caller waits on it or reports
/// blocked; it does not retry. The agent's pick is recorded under the dialog's
/// key as a recommendation (E0b): shown beside the person's buttons, never
/// chosen for them.
///
/// Built from the engine's read of the session. When that read fails the
/// refusal still carries the outcome; only the keys are missing.
fn escalated<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    engine_id: &str,
    refused: &str,
    pick: (Option<i64>, Option<&str>),
    reason: &str,
) -> Result<VogtError, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut details = json!({
        "outcome": "escalated",
        "needs": "person",
        "boundary": "permission_prompt",
    });
    // A failed read still leaves the refusal an escalation: only the keys are
    // missing (the refusal's own outcome is what the caller acts on).
    let seen = engine.get_session(engine_id).ok().flatten();
    let mut inbox_key = None;
    let mut recommended = false;
    if let Some(seen) = seen.as_ref() {
        details["session_id"] = json!(seen.id);
        let key = match seen.approval.as_ref() {
            None => session_blocked::approval_key(&seen.id, ""),
            Some(approval) => {
                session_blocked::dialog_key(&seen.id, &approval.key, &approval.question)
            }
        };
        details["escalation_key"] = json!(key);
        if seen.activity == "awaiting-approval" {
            let inbox = format!(
                "agent:session:{}:awaiting-approval:{}",
                seen.id,
                seen.activity_changed_at.as_deref().unwrap_or("unknown")
            );
            details["inbox_entry_key"] = json!(inbox);
            inbox_key = Some(inbox);
        }
        if let Some(approval) = seen.approval.as_ref() {
            recommended =
                session_blocked::record_recommendation(ctx, &key, approval, pick, reason)?;
            if recommended {
                details["recommendation"] = json!("recorded");
            }
        }
    }
    let mut message = format!("{refused} The prompt is escalated to a person in the Inbox");
    if let Some(inbox) = &inbox_key {
        message.push_str(&format!(" ({inbox})"));
    }
    message.push_str("; wait for it, do not retry.");
    if recommended {
        message.push_str(" Your pick is shown to them as a recommendation.");
    }
    Ok(VogtError::Escalated { message, details })
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
    let successor = match params.superseded_by.as_deref() {
        Some(text) if !text.is_empty() => Some(resolve_target(ctx, text)?.engine_session_id),
        _ => None,
    };
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
        .hibernate_session(&engine_id, Some(&reason), params.allow_shell, successor.as_deref())?
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
        "cold_wake_opt_out": params.cold_wake_opt_out,
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
            escalated_to: None,
        });
    };
    let mut writing = write_of(ctx);
    let actor_id = session.actor_id.clone();
    let session_id = session.id.clone();
    let why = reason.clone();
    let recorded = outcome.clone();
    let opt_out = params.cold_wake_opt_out;
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
            if let Some(opt_out) = opt_out {
                if opt_out != txn.session_cold_wake_opt_out(&current.id)? {
                    txn.set_session_cold_wake_opt_out(&current.id, opt_out)?;
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
        escalated_to: None,
    })
}

/// Whose notes a worker brief carries (WI-1320): a person's, or oversight's.
///
/// A session author counts when the engine says its role is `oversight` now.
/// A session that no longer exists, or an engine that cannot say (unreachable,
/// refused, or rejecting the lookup), counts as a worker and its notes are
/// dropped: the brief fails closed and the start goes ahead. One engine read
/// per author; once the engine is unreachable, no later author is asked for.
fn decision_author<'a, C, I>(
    ctx: &'a AppContext<C, I>,
    view: &'a impl ReadView,
) -> impl Fn(&crate::core::Comment) -> bool + 'a
where
    C: Clock,
    I: IdFactory,
{
    let roles: std::cell::RefCell<std::collections::HashMap<String, bool>> = Default::default();
    let engine_down = std::cell::Cell::new(false);
    let oversight = move |actor_id: &str| -> bool {
        let Ok(Some(actor)) = view.actor_by_id(actor_id) else {
            return false;
        };
        let Some(engine) = ctx.engine.as_ref() else {
            return false;
        };
        if engine_down.get() {
            return false;
        }
        let reference = actor.identity_ref;
        let engine_id = if let Some(id) = reference.strip_prefix("agent:session:") {
            match view.session_by_id(id) {
                Ok(Some(row)) => row.engine_session_id,
                _ => return false,
            }
        } else if let Some(id) = reference.strip_prefix(ENGINE_SESSION_ACTOR_PREFIX) {
            id.to_string()
        } else {
            return false;
        };
        if engine_id.is_empty() {
            return false;
        }
        match engine.get_session(&engine_id) {
            Ok(live) => live.is_some_and(|session| session.role == "oversight"),
            Err(VogtError::EngineUnavailable(error)) => {
                engine_down.set(true);
                tracing::warn!(
                    target: "vogt.sessions",
                    "brief decision notes: engine unavailable, session notes dropped actor_id={actor_id} error={error}"
                );
                false
            }
            Err(error) => {
                tracing::warn!(
                    target: "vogt.sessions",
                    "brief decision notes: engine refused the role lookup, note author counted as a worker actor_id={actor_id} error={}",
                    error.code()
                );
                false
            }
        }
    };
    move |comment: &crate::core::Comment| {
        let person = matches!(
            view.actor_by_id(&comment.actor_id),
            Ok(Some(actor)) if actor.kind == ActorKind::Human
        );
        if person {
            return true;
        }
        if let Some(known) = roles.borrow().get(&comment.actor_id) {
            return *known;
        }
        let judged = oversight(&comment.actor_id);
        roles.borrow_mut().insert(comment.actor_id.clone(), judged);
        judged
    }
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
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
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
        approval: None,
        blocked: None,
        reported_status: None,
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
        posture_lost_on_resume: false,
        account: None,
        budget: None,
        running: None,
        resources: None,
        conversation_id: None,
        done_at: None,
        latest_report: None,
        economics: None,
    };
    if let Some(live) = live {
        apply_live(&mut summary, live);
    }
    Ok(summary)
}

/// Copy the engine's live fields onto a summary.
fn apply_live(summary: &mut SessionSummary, live: &EngineSession) {
    summary.activity = Some(live.activity.clone());
    summary.done_at = live.done_at.clone();
    summary.economics = live.economics.clone();
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
    summary.account = live.account.clone();
    summary.budget = budget_of_engine(live.budget.as_ref());
    summary.conversation_id = live.conversation_id.clone();
    summary.resources = live.resources.as_ref().map(resources_of);
    summary.permission_mode = live.permission_mode.clone();
    summary.posture_lost_on_resume = live.posture_lost_on_resume;
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
    transcripts::find_in(
        &roots,
        &ctx.config.claude_account_dirs,
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
    transcripts::find_in(
        &roots,
        &ctx.config.claude_account_dirs,
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
        needs: known_or(&blocked.needs, &NEEDS, "person"),
        boundary: known_or(&blocked.boundary, &BOUNDARIES, "other"),
        key: blocked.key.clone(),
    }
}

fn resources_of(resources: &EngineResources) -> SessionResources {
    SessionResources {
        rss_bytes: resources.rss_bytes,
        cpu_pct: resources.cpu_pct,
        processes: resources.processes,
        sampled_at: resources.sampled_at.clone(),
        over_threshold: resources.over_threshold,
        memory_current: resources.memory_current,
        memory_max: resources.memory_max,
        memory_peak: resources.memory_peak,
        oom_kill: resources.oom_kill,
        observed_at: resources.observed_at.clone(),
        memory_warning: resources.memory_warning,
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
        approval: None,
        blocked: None,
        reported_status: None,
        last_reply_excerpt: None,
        hibernation: None,
        keep_awake: None,
        autopilot: None,
        autopilot_nudges: None,
        role: None,
        stopped_by: None,
        stop_reason: None,
        permission_mode: None,
        posture_lost_on_resume: false,
        account: None,
        budget: None,
        running: None,
        resources: None,
        conversation_id: None,
        done_at: None,
        latest_report: None,
        economics: None,
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
    pending: Option<&str>,
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
    delivery::judge(true, before, &seen, pending)
}

fn has_input_box(session: &EngineSession) -> bool {
    matches!(
        session.conversation_agent.as_deref(),
        Some("claude" | "codex")
    )
}

fn read_screen(engine: &EngineClient, engine_id: &str) -> Option<delivery::Observation> {
    let screen = engine.session_screen(engine_id, 0).ok()??;
    Some(delivery::Observation {
        activity: screen.activity,
        lines: screen.lines,
    })
}

fn type_then_submit(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    text: &str,
    before: Option<&str>,
    person: bool,
) -> Result<delivery::Verdict, VogtError> {
    let mut boxed = read_screen(engine, engine_id)
        .is_some_and(|seen| delivery::composer_text(&seen.lines).is_some());
    if !boxed && !matches!(before, Some("running" | "awaiting-approval")) {
        let _ = engine.wait_session(engine_id, "ready", READY_GATE);
        boxed = read_screen(engine, engine_id)
            .is_some_and(|seen| delivery::composer_text(&seen.lines).is_some());
    }
    send_all(
        engine,
        engine_id,
        named,
        &[(text.to_string(), false)],
        person,
        None,
    )?;
    if boxed {
        // WI-4459: the Enter goes out only once the text is in the composer.
        // A text that never shows is reported unsent, not submitted.
        let mut shown = false;
        for _ in 0..TYPED_READS {
            if let Some(seen) = read_screen(engine, engine_id) {
                if delivery::composer_holds(&seen.lines, text) {
                    shown = true;
                    break;
                }
            }
            std::thread::sleep(CONFIRM_INTERVAL);
        }
        if !shown {
            return Ok(delivery::Verdict {
                delivery: delivery::Delivery::Unconfirmed,
                evidence: "the text never appeared in the input box, so no Enter was sent"
                    .to_string(),
            });
        }
    }
    send_all(
        engine,
        engine_id,
        named,
        &[(String::new(), true)],
        person,
        None,
    )?;
    submit_until_taken(engine, engine_id, named, before, text, person)
}

fn submit_until_taken(
    engine: &EngineClient,
    engine_id: &str,
    named: &str,
    before: Option<&str>,
    text: &str,
    person: bool,
) -> Result<delivery::Verdict, VogtError> {
    let mut verdict = settled(
        engine,
        engine_id,
        text,
        confirm_delivery(engine, engine_id, before, Some(text)),
    );
    for _ in 0..MAX_ENTERS - 1 {
        if !matches!(
            verdict.delivery,
            delivery::Delivery::Unconfirmed | delivery::Delivery::Stuck
        ) {
            break;
        }
        if engine.wait_session(engine_id, "ready", SETTLE).is_err() {
            break;
        }
        let Some(seen) = read_screen(engine, engine_id) else {
            break;
        };
        if !delivery::safe_to_resubmit(&seen, text) {
            if delivery::composer_text(&seen.lines).is_some() {
                verdict = delivery::Verdict {
                    delivery: delivery::Delivery::Delivered,
                    evidence: "the input box cleared after the Enter".to_string(),
                };
            }
            break;
        }
        send_all(
            engine,
            engine_id,
            named,
            &[(String::new(), true)],
            person,
            None,
        )?;
        verdict = settled(
            engine,
            engine_id,
            text,
            confirm_delivery(engine, engine_id, before, Some(text)),
        );
    }
    Ok(verdict)
}

fn settled(
    engine: &EngineClient,
    engine_id: &str,
    text: &str,
    verdict: delivery::Verdict,
) -> delivery::Verdict {
    if verdict.delivery != delivery::Delivery::Delivered {
        return verdict;
    }
    std::thread::sleep(CONFIRM_INTERVAL);
    let Some(seen) = read_screen(engine, engine_id) else {
        return verdict;
    };
    if seen.activity.as_deref() == Some("running") {
        return verdict;
    }
    delivery::judge(true, None, &[seen], Some(text))
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

/// Whether a wait that ended for a woken session leaves it at its prompt, able
/// to take input (WI-2232). A reported block is a waiting state, not a busy
/// one: a session that reported itself blocked and sits idle at its prompt is
/// ready for the overseer or person who is answering that block. A permission
/// dialog is never ready here; that answer belongs to a person or an overseer's
/// `session_answer`.
fn at_prompt_for_input(waited: &EngineWait, from_relay: bool) -> bool {
    if waited.outcome == "ready" {
        return true;
    }
    waited.outcome == "blocked"
        && from_relay
        && waited.screen.ready == Some(true)
        && matches!(
            waited.screen.activity.as_deref(),
            Some("idle" | "waiting-for-input")
        )
}

/// What the caller should do next when a woken session is not ready for input,
/// by why the wait ended (WI-2232).
fn not_ready_next_step(outcome: &str) -> &'static str {
    match outcome {
        "blocked" => {
            "It reported itself blocked and is not at an input prompt. Read session_screen, and wait with session_wait (until ready) for it to reach its prompt before sending input again; do not repeat this call until it has."
        }
        "awaiting-approval" => {
            "It is showing a permission prompt. Answer it with session_answer, then send any steering text with session_input once the prompt is gone."
        }
        _ => "Read session_screen — it may be showing a dialog — then retry",
    }
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
    force: bool,
) -> Result<SessionResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    match wake_inner(ctx, engine, target, reason, force) {
        Err(VogtError::Conflict(message)) => {
            Err(VogtError::Conflict(in_session_terms(ctx, &message)))
        }
        other => other,
    }
}

/// Say a refused wake in ses_… terms when the successor is a linked session.
/// An unlinked successor stays a uuid, and so does any other refusal.
fn in_session_terms<C, I>(ctx: &AppContext<C, I>, message: &str) -> String
where
    C: Clock,
    I: IdFactory,
{
    static SUCCESSOR: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = SUCCESSOR.get_or_init(|| {
        regex::Regex::new(r"session [0-9a-fA-F-]{36} was superseded by (?P<uuid>[0-9a-fA-F-]{36})")
            .expect("the successor pattern compiles")
    });
    let Some(uuid) = pattern
        .captures(message)
        .and_then(|found| found.name("uuid"))
        .map(|found| found.as_str().to_string())
    else {
        return message.to_string();
    };
    let named = ctx
        .declared
        .read()
        .ok()
        .and_then(|view| view.session_by_engine_id(&uuid).ok().flatten())
        .map(|row| row.id);
    match named {
        Some(named) if named != uuid => message.replacen(&uuid, &named, 1),
        _ => message.to_string(),
    }
}

/// Whether this wake compacts (WI-2037), and the summary it starts from. The
/// clock is read once here, as Python's `_cold_wake` reads it, whether or not
/// the feature is on. A summary that cannot be read falls back to a plain
/// resume: compaction is a saving, never a gate.
fn cold_wake_of<C, I>(
    ctx: &AppContext<C, I>,
    current: &EngineSession,
    session: Option<&CodingSession>,
) -> Result<(cold_wake::Decision, Option<String>), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let config = &ctx.config;
    let tokens = current
        .economics
        .as_ref()
        .and_then(|economics| economics.get("context_tokens"))
        .and_then(Value::as_i64)
        .filter(|tokens| *tokens > 0);
    let opted_out = match session {
        Some(session) => ctx
            .declared
            .read()?
            .session_cold_wake_opt_out(&session.id)?,
        None => false,
    };
    let hibernated_at = current.hibernation.as_ref().and_then(|hibernation| {
        crate::application::session_changes::engine_moment(&hibernation.at)
    });
    let moment = now(&write_of(ctx));
    let decision = cold_wake::decide(&cold_wake::Inputs {
        enabled: config.cold_wake_compaction,
        opted_out,
        agent: current.conversation_agent.as_deref(),
        model: session.and_then(|session| session.model.as_deref()),
        hibernated_at,
        now: moment,
        context_tokens: tokens,
        ttl_seconds: &config.cold_wake_ttl_seconds,
        threshold_tokens: config.cold_wake_threshold_tokens,
    });
    if !decision.compact() {
        return Ok((decision, None));
    }
    let reply = crate::application::session_reads::last_reply_in(ctx, &current.id, 1)
        .ok()
        .and_then(|body| {
            body.get("messages")
                .and_then(Value::as_array)
                .and_then(|messages| messages.last().cloned())
        })
        .and_then(|message| {
            message
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let (work_item, handoff) = match session.and_then(|session| session.work_item_id.as_deref()) {
        Some(item_id) => {
            let view = ctx.declared.read()?;
            let item = view.work_item_by_id(item_id)?;
            let comments = view.comments_for(item_id, 20, true)?;
            (
                item,
                comments
                    .into_iter()
                    .find(|comment| comment.kind == "handoff"),
            )
        }
        None => (None, None),
    };
    let summary = cold_wake::render_summary(&cold_wake::SummaryInputs {
        session_id: session.map_or(current.id.as_str(), |session| session.id.as_str()),
        work_item: work_item.as_ref().map(|item| item.reference.as_str()),
        work_item_title: work_item.as_ref().map(|item| item.title.as_str()),
        reason: session.map(|session| session.reason.as_str()),
        last_reply: reply.as_deref(),
        handoff: handoff.as_ref(),
        decision: &decision,
    });
    Ok((decision, Some(summary)))
}

/// A storage read error while deciding on compaction is logged and the wake
/// resumes the transcript: compaction is a saving, never a gate.
fn plain_resume_on_read_error(
    result: Result<(cold_wake::Decision, Option<String>), VogtError>,
    engine_session: &str,
) -> (cold_wake::Decision, Option<String>) {
    result.unwrap_or_else(|error| {
        tracing::warn!(
            session = %engine_session, error = %error,
            "could not read the cold-wake inputs; resuming the transcript"
        );
        (cold_wake::Decision::read_failed(), None)
    })
}

fn wake_inner<C, I>(
    ctx: &AppContext<C, I>,
    engine: &EngineClient,
    target: &Target,
    reason: &str,
    force: bool,
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
        return Ok(SessionResult {
            session: summary,
            escalated_to: None,
        });
    }
    let (decision, fresh_prompt) =
        plain_resume_on_read_error(cold_wake_of(ctx, &current, session.as_ref()), &current.id);
    let Some(session) = session else {
        let woken = engine
            .wake_session(engine_id, None, force, fresh_prompt.as_deref())?
            .ok_or_else(|| {
                VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
            })?;
        let detail = json!({
            "engine_session_id": engine_id,
            "linked": false,
            "forced": force,
            "cold_wake": decision.audit(),
        });
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
            escalated_to: None,
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
    let woken = engine
        .wake_session(engine_id, Some(&env), force, fresh_prompt.as_deref())?
        .ok_or_else(|| {
            VogtError::NotFound(format!("the engine has no session {}", py_repr(engine_id)))
        })?;
    let conversation_id = current.conversation_id.clone();
    let cold_audit = decision.audit();
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
                "forced": force,
                "cold_wake": cold_audit,
            }),
        ))
    })?;
    Ok(SessionResult {
        session: summarize(ctx, &session, Some(&woken))?,
        escalated_to: None,
    })
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

// -- sessions part II: rename, remove, token (WI-1074) -------------------------

crate::typed_op!(rename_session_op, RenameSessionParams, rename_session_typed);
crate::typed_op!(remove_session_op, RemoveSessionParams, remove_session_typed);
crate::typed_op!(session_token_op, SessionTokenParams, session_token_typed);

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
            "the engine has no session {}",
            py_repr(&params.id)
        )));
    }
    let mut writing = write_of(ctx);
    let engine_id = found.engine_session_id.clone();
    let named = name.to_string();
    // A linked session is audited under Vogt's id; an unlinked one under the
    // engine's.
    let entity = found
        .session
        .as_ref()
        .map_or_else(|| engine_id.clone(), |session| session.id.clone());
    audited_action(
        &mut writing,
        SESSION_RENAME,
        &reason,
        "session",
        &entity,
        &json!({"name": named, "linked": found.session.is_some()}),
        "session.renamed",
        Some(&json!({"engine_session_id": engine_id})),
    )?;
    // The rename itself only says whether the engine had the session.
    let current = engine
        .get_session(&found.engine_session_id)?
        .ok_or_else(|| {
            VogtError::NotFound(format!("the engine has no session {}", py_repr(&params.id)))
        })?;
    let summary = match &found.session {
        Some(session) => summarize(ctx, session, Some(&current))?,
        None => unlinked_summary(&ctx.declared.read()?, &current)?,
    };
    Ok(SessionResult {
        session: summary,
        escalated_to: None,
    })
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
        return Ok(SessionResult {
            session: summary,
            escalated_to: None,
        });
    }

    let Some(session) = found.session else {
        // The branch above returns when there is no declared session. Getting
        // here without one means the resolution changed under us.
        return Err(VogtError::NotFound(format!(
            "no session {}",
            py_repr(&engine_id)
        )));
    };
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
            SessionResult {
                session: summary,
                escalated_to: None,
            },
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

#[cfg(test)]
mod tests;

mod budget;
mod models;
mod start;
mod subject;
mod support;
mod wire;

pub use self::budget::*;
pub use self::models::*;
pub use self::start::*;
use self::subject::*;
pub(crate) use self::support::*;
pub use self::wire::*;
