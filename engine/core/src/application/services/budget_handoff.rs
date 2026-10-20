//! What the core does after the engine stops a session at its budget (WI-1833).
//! Ports `src/vogt/application/services/budget_handoff.py`.
//!
//! The engine's soft warning asks the agent to commit, push and write a
//! structured hand-off (`work_comment kind=handoff`) before the hard limit
//! hibernates it with trigger `budget`. An agent that ran out mid-loop often
//! never wrote one, and the item then had to be re-discovered by hand.
//!
//! `settle_budget_stops` closes that gap, once per budget-stopped session:
//!
//! 1. If the item has no hand-off written since the session started, it writes
//!    a minimal one from the session's git state — branch, head and the dirty
//!    files — so `session_start(from_handoff=…)` always has something to start
//!    from.
//! 2. If the deployment's `dispatch.budget_restarts_per_item` cap (default 1)
//!    allows it, and nothing has picked the item up since, it starts a fresh
//!    worker from that hand-off with the stopped session's purpose, template,
//!    model, effort and engine budget. Each restart leaves a note on the item;
//!    the notes are the count the cap is read against.
//!
//! Idempotent by construction: a second pass finds the hand-off, and either the
//! restart note or the newer session, and does nothing. It runs after every
//! scheduled sweep (`adapters/http/scheduler.rs`), as in Python.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::sessions::{dispatch_preferences, start_session, SessionBudget, StartSessionParams};
use super::work::comment_work;
use crate::adapters::engine::EngineSession;
use crate::application::context::AppContext;
use crate::application::dispatch as dispatch_profiles;
use crate::core::{Clock, CodingSession, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

/// Every automatic restart's note starts with this; the cap counts them.
pub const RESTART_NOTE: &str = "[vogt] budget restart:";
pub const REASON: &str = "budget stop: keep the item's hand-off and continue it (WI-1833)";
/// Dirty files listed on a git-state hand-off, at most.
const DIRTY_LIMIT: usize = 50;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// What one pass did, by work item ref.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BudgetSettlement {
    pub handoffs_written: Vec<String>,
    pub restarted: Vec<String>,
}

/// A checkout's branch, head and dirty paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    pub dirty: Vec<String>,
}

/// Python `_git`: stdout, or `None` when git could not run, overran its
/// timeout or exited non-zero. Untrimmed: `status --porcelain` lines start
/// with their two status columns.
fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drained on its own thread, so a long status cannot fill the pipe while
    // the wait is polled.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > GIT_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    };
    let out = reader.join().unwrap_or_default();
    status
        .success()
        .then(|| String::from_utf8_lossy(&out).into_owned())
}

/// The checkout's branch, head and dirty paths; `None` when it is not one.
pub fn read_git_state(cwd: &str) -> Option<GitState> {
    let path = Path::new(cwd);
    if cwd.is_empty() || !path.is_dir() {
        return None;
    }
    let head = git(path, &["rev-parse", "HEAD"])?;
    let branch = git(path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let branch = branch.trim();
    let status = git(path, &["status", "--porcelain"]).unwrap_or_default();
    // Python `str.splitlines` and `line[3:]`, by characters.
    let dirty = status
        .lines()
        .filter(|line| line.chars().count() > 3)
        .map(|line| line.chars().skip(3).collect())
        .collect();
    let head = head.trim();
    Some(GitState {
        branch: (!branch.is_empty() && branch != "HEAD").then(|| branch.to_string()),
        head_sha: (!head.is_empty()).then(|| head.to_string()),
        dirty,
    })
}

/// Python `HandoffFields(...).model_dump(mode="json")`: every field, in the
/// model's order, a field left unset at its default.
fn handoff_fields(
    session: &str,
    branch: Option<&str>,
    head_sha: Option<&str>,
    touch_points: Vec<String>,
    remaining: Vec<String>,
) -> Value {
    json!({
        "branch": branch,
        "head_sha": head_sha,
        "base": null,
        "done": [],
        "remaining": remaining,
        "touch_points": touch_points,
        "gates_run": [],
        "blockers": [],
        "session": session,
    })
}

/// The minimal hand-off a budget stop leaves when the agent wrote none: its
/// body and its structured fields.
pub fn git_state_handoff(
    session_id: &str,
    reason: Option<&str>,
    state: Option<&GitState>,
) -> (String, Value) {
    let stopped = reason
        .filter(|text| !text.is_empty())
        .unwrap_or("the session reached its budget");
    let Some(state) = state else {
        let body = format!(
            "Budget stop ({stopped}). The session wrote no hand-off and its \
             working tree could not be read, so nothing about its progress is \
             known here."
        );
        return (
            body,
            handoff_fields(
                session_id,
                None,
                None,
                Vec::new(),
                vec!["unknown: re-read the item and its branch".to_string()],
            ),
        );
    };
    let mut dirty: Vec<String> = state.dirty.iter().take(DIRTY_LIMIT).cloned().collect();
    let more = state.dirty.len() - dirty.len();
    let tail = if state.dirty.is_empty() {
        "The tree was clean.".to_string()
    } else {
        format!(
            "{} uncommitted path(s) were left in the tree.",
            state.dirty.len()
        )
    };
    let body = format!(
        "Budget stop ({stopped}). The session wrote no hand-off; this one is \
         from its git state. {tail}"
    );
    let mut remaining = vec!["not stated by the session: read the branch's commits".to_string()];
    if !state.dirty.is_empty() {
        remaining.push("commit or discard the uncommitted paths in touch_points".to_string());
    }
    if more > 0 {
        dirty.push(format!("... and {more} more"));
    }
    (
        body,
        handoff_fields(
            session_id,
            state.branch.as_deref(),
            state.head_sha.as_deref(),
            dirty,
            remaining,
        ),
    )
}

/// The start that continues `link`'s item: its own purpose, template, model,
/// effort and budget, so a restart runs on what the stopped session ran rather
/// than on the profile's defaults. `None` when the session named neither a
/// purpose nor a template.
fn restart_params(
    link: &CodingSession,
    reference: &str,
    n: i64,
    cap: i64,
    budget: Option<&Map<String, Value>>,
) -> Option<StartSessionParams> {
    if link.purpose.is_none() && link.template.is_none() {
        return None;
    }
    let task = format!(
        "Continue {reference} from its latest hand-off: the previous session \
         stopped at its turn budget. Read the hand-off, check out its branch, \
         carry on, and hand off again when the gates are green."
    );
    // The limits a restart carries over from the stopped session's engine
    // row: whole numbers above zero only.
    let limit = |key: &str| {
        budget
            .and_then(|budget| budget.get(key))
            .filter(|value| !value.is_boolean())
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
    };
    let limits = SessionBudget {
        turns: limit("turns"),
        context_tokens: limit("context_tokens"),
        wall_clock_minutes: limit("wall_clock_minutes"),
        session_wall_clock_minutes: limit("session_wall_clock_minutes"),
    };
    Some(StartSessionParams {
        project: None,
        work_item: None,
        name: None,
        template: link.template.clone(),
        task: Some(task),
        reason: format!("{REASON} ({n} of {cap})"),
        model: link.model.clone(),
        effort: link.effort.clone(),
        resume: None,
        permission_mode: "default".to_string(),
        autopilot: None,
        role: "worker".to_string(),
        purpose: link.purpose.clone(),
        review_of: None,
        from_handoff: Some(reference.to_string()),
        budget: (limits != SessionBudget::default()).then_some(limits),
        worktree: None,
        repo_map: None,
        explicit_null: Vec::new(),
    })
}

/// Hand off, and restart within the cap, every budget-stopped session.
pub fn settle_budget_stops<C, I>(ctx: &AppContext<C, I>) -> Result<BudgetSettlement, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut done = BudgetSettlement::default();
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(done);
    };
    let rows = match engine.list_sessions() {
        Ok(rows) => rows,
        Err(VogtError::EngineUnavailable(message)) => {
            tracing::info!("budget hand-off pass skipped: {message}");
            return Ok(done);
        }
        Err(error) => return Err(error),
    };
    let stopped: Vec<&EngineSession> = rows
        .iter()
        .filter(|row| {
            row.hibernated()
                && row
                    .hibernation
                    .as_ref()
                    .is_some_and(|hibernation| hibernation.trigger == "budget")
        })
        .collect();
    if stopped.is_empty() {
        return Ok(done);
    }
    let cap = dispatch_profiles::resolve(&dispatch_preferences(ctx)?).budget_restarts_per_item;
    for row in stopped {
        let (link, item, comments, later) = {
            let view = ctx.declared.read()?;
            let Some(link) = view.session_by_engine_id(&row.id)? else {
                continue;
            };
            let Some(work_item_id) = link.work_item_id.clone() else {
                continue;
            };
            let Some(item) = view.work_item_by_id(&work_item_id)? else {
                continue;
            };
            let comments = view.comments_for(&item.id, 200, true)?;
            let later = view
                .list_sessions(None, Some(&item.id), true, 50, 0)?
                .into_iter()
                .any(|one| one.id != link.id && one.started_at > link.started_at);
            (link, item, comments, later)
        };
        let handed_off = comments
            .iter()
            .any(|one| one.created_at >= link.started_at && one.kind == "handoff");
        if !handed_off {
            let reason = row
                .hibernation
                .as_ref()
                .and_then(|hibernation| hibernation.reason.as_deref());
            let cwd = if row.cwd.is_empty() {
                &link.cwd
            } else {
                &row.cwd
            };
            let state = read_git_state(cwd);
            let (body, fields) = git_state_handoff(&row.id, reason, state.as_ref());
            let written = comment_work(
                ctx,
                json!({
                    "ref": item.reference,
                    "body": body,
                    "kind": "handoff",
                    "handoff": fields,
                    "reason": REASON,
                }),
            );
            if let Err(error) = written {
                // An upstream item has no local thread to carry a hand-off.
                tracing::warn!(
                    "budget hand-off on {} refused: {}",
                    item.reference,
                    error.message()
                );
                continue;
            }
            done.handoffs_written.push(item.reference.clone());
        }
        // Anything started on the item since is the continuation, a person's
        // or this pass's own: never a second one.
        if later {
            continue;
        }
        let used = comments
            .iter()
            .filter(|one| one.kind == "note" && one.body.starts_with(RESTART_NOTE))
            .count() as i64;
        if used >= cap {
            continue;
        }
        let Some(params) =
            restart_params(&link, &item.reference, used + 1, cap, row.budget.as_ref())
        else {
            continue;
        };
        let started = match start_session(ctx, &params) {
            Ok(started) => started,
            Err(error) => {
                tracing::warn!(
                    "budget restart of {} refused: {}",
                    item.reference,
                    error.message()
                );
                continue;
            }
        };
        comment_work(
            ctx,
            json!({
                "ref": item.reference,
                "body": format!(
                    "{RESTART_NOTE} {} continues from the hand-off of {} ({} of {cap}).",
                    started.session.engine_session_id,
                    row.id,
                    used + 1
                ),
                "reason": REASON,
            }),
        )?;
        done.restarted.push(item.reference.clone());
    }
    Ok(done)
}
