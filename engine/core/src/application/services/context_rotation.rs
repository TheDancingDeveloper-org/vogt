//! Rotate an oversight session that has outgrown its context budget (WI-3132).
//!
//! A long oversight conversation re-reads its whole context every turn, so the
//! cost of a turn grows with the conversation. `dispatch.context_budget` names
//! a per-role ceiling (`{"oversight": {"tokens": 250000}}`); a role with no
//! entry is never rotated. Each scheduled pass, for every live session of such
//! a role whose context reading is above its ceiling:
//!
//! 1. If the session is idle and no hand-off file exists, ask it (typed into
//!    its terminal) to write one at `<data_dir>/context-rotation/<id>.md`:
//!    fleet ids, `wait_any` cursor, open decisions, batch state, pending
//!    reviews. The ask is repeated after `REASK_AFTER` up to `MAX_ASKS` times.
//! 2. Once the file exists and is not empty, start a fresh session of the same
//!    role in the same project, seeded with the file, then hibernate the old
//!    one (a failed hibernate is retried alone, never with a second replacement).
//!    The rotation is an audited `session.context_rotated` event.
//! 3. Once the old session is hibernated, its `.asked`, `.rotating` and
//!    hand-off files are removed (the replacement reads a copy, `.rotated.md`,
//!    pruned after `ARCHIVE_KEEP`). A woken session still over budget is then
//!    asked for a fresh hand-off and rotated again, rather than hibernated
//!    again with no replacement.
//!
//! The replacement is claimed in `<id>.rotating` before it is started and the
//! file is then given the new id. If the claim or the id cannot be persisted
//! the rotation stops there: a claim with no id is never overwritten, so a
//! later pass cannot start a second replacement.
//!
//! Idempotent: a hibernated session is not a candidate, and an unanswered ask
//! waits for its marker to age. The old session is not woken by this module,
//! but `session.input` to it would wake it: tell reviewers the new id.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use super::sessions::dispatch_preferences;
use super::sessions::{hibernate, start_session_tracked, HibernateParams, StartSessionParams};
use crate::adapters::engine::EngineSession;
use crate::application::context::{write_of, AppContext};
use crate::application::dispatch as dispatch_profiles;
use crate::application::writes::audited_action;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

pub const REASON: &str =
    "context budget crossed: hand off and continue in a fresh session (WI-3132)";
const DIR_NAME: &str = "context-rotation";
const REASK_AFTER: Duration = Duration::from_secs(15 * 60);
const MAX_ASKS: u32 = 3;
const HIBERNATE_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);
/// How long a replacement's copy of the hand-off is kept: it only has to
/// outlive the replacement's first read.
const ARCHIVE_KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const ARCHIVE_SUFFIX: &str = ".rotated.md";
const ROTATE_OP: &str = "session.context_rotate";

/// What one pass did, by engine session id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotationOutcome {
    pub asked: Vec<String>,
    /// `(old engine id, new engine id)`.
    pub rotated: Vec<(String, String)>,
}

/// The context a session's engine row reports, in tokens.
fn context_tokens(row: &EngineSession) -> Option<i64> {
    row.economics
        .as_ref()?
        .get("context_tokens")?
        .as_i64()
        .filter(|tokens| *tokens > 0)
}

fn handoff_path(dir: &Path, engine_id: &str) -> PathBuf {
    dir.join(format!("{}.md", safe_name(engine_id)))
}

fn marker_path(dir: &Path, engine_id: &str) -> PathBuf {
    dir.join(format!("{}.asked", safe_name(engine_id)))
}

fn rotating_path(dir: &Path, engine_id: &str) -> PathBuf {
    dir.join(format!("{}.rotating", safe_name(engine_id)))
}

/// The copy of the hand-off the replacement is told to read, so the original
/// can be removed once the rotation is done.
fn archive_path(dir: &Path, engine_id: &str) -> PathBuf {
    dir.join(format!("{}{ARCHIVE_SUFFIX}", safe_name(engine_id)))
}

/// Drop replacement copies old enough that no replacement still needs them.
fn prune_archives(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .file_name()
            .to_string_lossy()
            .ends_with(ARCHIVE_SUFFIX)
            && entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| SystemTime::now().duration_since(at).ok())
                .is_some_and(|age| age >= ARCHIVE_KEEP);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// An engine id is a UUID or `eng-…`; anything else cannot climb out of `dir`.
fn safe_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn ask_text(path: &Path, tokens: i64, budget: i64) -> String {
    format!(
        "[vogt] Your context is {tokens} tokens, over the {budget} budget for this role. \
         Write a hand-off file at {} so a fresh session can take over: fleet session ids \
         and what each is doing, your wait_any cursor, open decisions, batch state, pending \
         reviews, and anything you promised. Write it to a temp name and rename it into \
         place when complete. Then stop; Vogt starts your replacement and hibernates you.",
        path.display()
    )
}

fn seed_task(old_id: &str, path: &Path) -> String {
    format!(
        "You replace oversight session {old_id}, rotated because its context grew past \
         budget. Read the hand-off file {} first and continue its work. Tell reviewers and \
         workers your new session id; the old session is hibernated and a message to it \
         would wake it.",
        path.display()
    )
}

/// How many asks the marker records and whether it is old enough to repeat.
fn marker_state(marker: &Path) -> (u32, bool) {
    let count = std::fs::read_to_string(marker)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    let Some(count) = count else {
        return (0, true);
    };
    let aged = std::fs::metadata(marker)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age >= REASK_AFTER);
    (count, aged)
}

/// Writes the audited rotation event for `(old engine id, outcome)`.
pub(super) type Audit<C, I> = fn(&AppContext<C, I>, &str, &Value) -> Result<(), VogtError>;

/// Ask and rotate every live session over its role's context budget.
pub fn rotate_over_budget<C, I>(ctx: &AppContext<C, I>) -> Result<RotationOutcome, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    rotate_with(ctx, record_rotation)
}

pub(super) fn rotate_with<C, I>(
    ctx: &AppContext<C, I>,
    audit: Audit<C, I>,
) -> Result<RotationOutcome, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut done = RotationOutcome::default();
    let Some(engine) = ctx.engine.as_ref() else {
        return Ok(done);
    };
    let budgets = dispatch_profiles::resolve(&dispatch_preferences(ctx)?).context_budget;
    if budgets.is_empty() {
        return Ok(done);
    }
    let rows = match engine.list_sessions() {
        Ok(rows) => rows,
        Err(VogtError::EngineUnavailable(message)) => {
            tracing::info!("context rotation pass skipped: {message}");
            return Ok(done);
        }
        Err(error) => return Err(error),
    };
    let dir = ctx.config.resolved_data_dir().join(DIR_NAME);
    prune_archives(&dir);
    for row in rows.iter().filter(|row| row.alive && !row.hibernated()) {
        let Some(budget) = budgets.get(&row.role).copied() else {
            continue;
        };
        let Some(tokens) = context_tokens(row).filter(|tokens| *tokens > budget) else {
            continue;
        };
        let path = handoff_path(&dir, &row.id);
        let written = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 0);
        if row.activity != "idle" {
            continue;
        }
        if !written {
            let marker = marker_path(&dir, &row.id);
            let (asks, aged) = marker_state(&marker);
            if asks >= MAX_ASKS || !aged {
                continue;
            }
            if std::fs::create_dir_all(&dir).is_err() {
                tracing::warn!("context rotation: cannot create {}", dir.display());
                continue;
            }
            match engine.send_input(&row.id, &ask_text(&path, tokens, budget), true, false, None) {
                Ok(true) => {
                    let _ = std::fs::write(&marker, (asks + 1).to_string());
                    done.asked.push(row.id.clone());
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    "context rotation ask of {} refused: {}",
                    row.id,
                    error.message()
                ),
            }
            continue;
        }
        match rotate_one(ctx, row, &dir, &path, tokens, budget, audit) {
            Ok(Some(new_id)) => done.rotated.push((row.id.clone(), new_id)),
            Ok(None) => {}
            // One session's failure never ends the pass for the others.
            Err(error) => {
                tracing::warn!("context rotation of {} failed: {}", row.id, error.message())
            }
        }
    }
    Ok(done)
}

/// Start the replacement, hibernate the old session, then record the event.
/// `Ok(None)` means nothing was rotated this pass (not an error).
///
/// The replacement's id is persisted before the old session is hibernated, so
/// a hibernate that fails is retried on its own after `HIBERNATE_RETRY_AFTER`
/// and never starts a second replacement. The audit comes last: its failure
/// is logged and cannot orphan the replacement.
fn rotate_one<C, I>(
    ctx: &AppContext<C, I>,
    row: &EngineSession,
    dir: &Path,
    path: &Path,
    tokens: i64,
    budget: i64,
    audit: Audit<C, I>,
) -> Result<Option<String>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let pending = rotating_path(dir, &row.id);
    let new_id = match std::fs::read_to_string(&pending) {
        Ok(text) if !text.trim().is_empty() => {
            let aged = std::fs::metadata(&pending)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| SystemTime::now().duration_since(at).ok())
                .is_some_and(|age| age >= HIBERNATE_RETRY_AFTER);
            if !aged {
                return Ok(None);
            }
            text.trim().to_string()
        }
        // A claim with no id: a replacement may have been started and its id
        // lost. Never start another; an operator clears the file.
        Ok(_) => {
            tracing::warn!(
                "context rotation: {} has a replacement claim with no id ({}); \
                 not starting another",
                row.id,
                pending.display()
            );
            return Ok(None);
        }
        Err(_) => {
            // Claim the rotation before starting anything, so a failure to
            // persist it stops the rotation instead of leaving a replacement
            // nothing remembers.
            if let Err(error) = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&pending)
            {
                tracing::warn!(
                    "context rotation: cannot claim the rotation of {}: {error}",
                    row.id
                );
                return Ok(None);
            }
            let archive = archive_path(dir, &row.id);
            if let Err(error) = std::fs::copy(path, &archive) {
                tracing::warn!(
                    "context rotation: cannot copy the hand-off of {}: {error}",
                    row.id
                );
                let _ = std::fs::remove_file(&pending);
                return Ok(None);
            }
            let new_id = match start_replacement(ctx, row, &archive) {
                Ok(Replacement::Started(id)) => id,
                // The engine may be running a replacement Vogt has no id for.
                // Keep the empty claim: a later pass never starts a second.
                Ok(Replacement::Unknown) => {
                    tracing::warn!(
                        "context rotation: starting the replacement of {} failed after the \
                         engine accepted it; keeping the claim ({}), not starting another",
                        row.id,
                        pending.display()
                    );
                    return Ok(None);
                }
                // Nothing was started: release the claim for a later pass.
                Ok(Replacement::NotStarted) => {
                    let _ = std::fs::remove_file(&pending);
                    let _ = std::fs::remove_file(&archive);
                    return Ok(None);
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&pending);
                    let _ = std::fs::remove_file(&archive);
                    return Err(error);
                }
            };
            if let Err(error) = std::fs::write(&pending, &new_id) {
                tracing::warn!(
                    "context rotation: cannot record replacement {new_id} of {}: {error}; \
                     not hibernating it",
                    row.id
                );
                return Ok(None);
            }
            new_id
        }
    };
    if let Err(error) = hibernate(
        ctx,
        &HibernateParams {
            id: row.id.clone(),
            reason: format!("rotated to {new_id}: {REASON}"),
            allow_shell: false,
            cold_wake_opt_out: None,
        },
    ) {
        // Not rotated: keep the hand-off and the replacement id, back off.
        let _ = std::fs::write(&pending, &new_id);
        tracing::warn!(
            "context rotation: hibernating {} failed, retrying later: {}",
            row.id,
            error.message()
        );
        return Ok(None);
    }
    // Rotated: forget it, so a wake of this session starts a new rotation.
    // `.rotating` goes first; left behind, it would hibernate the woken
    // session again with no replacement.
    for stale in [pending, path.to_path_buf(), marker_path(dir, &row.id)] {
        if let Err(error) = std::fs::remove_file(&stale) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "context rotation: cannot remove {}: {error}",
                    stale.display()
                );
            }
        }
    }
    let outcome = json!({
        "from": row.id,
        "to": new_id,
        "role": row.role,
        "context_tokens": tokens,
        "budget_tokens": budget,
        "handoff_file": path.display().to_string(),
    });
    if let Err(error) = audit(ctx, &row.id, &outcome) {
        tracing::warn!(
            "context rotation: audit of {} -> {new_id} failed: {}",
            row.id,
            error.message()
        );
    }
    Ok(Some(new_id))
}

/// How starting a replacement ended.
enum Replacement {
    Started(String),
    /// Refused before the engine accepted anything.
    NotStarted,
    /// The engine accepted a session but the start then failed: it may be
    /// running, so no second one may be started.
    Unknown,
}

fn start_replacement<C, I>(
    ctx: &AppContext<C, I>,
    row: &EngineSession,
    path: &Path,
) -> Result<Replacement, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let link = ctx.declared.read()?.session_by_engine_id(&row.id)?;
    let Some(link) = link else {
        tracing::warn!(
            "context rotation: {} has no linked project to restart in",
            row.id
        );
        return Ok(Replacement::NotStarted);
    };
    let slug = ctx
        .declared
        .read()?
        .project_by_id(&link.project_id)?
        .map(|project| project.slug);
    let Some(slug) = slug else {
        return Ok(Replacement::NotStarted);
    };
    let params = StartSessionParams {
        project: Some(slug),
        work_item: None,
        name: (!row.name.is_empty()).then(|| row.name.clone()),
        template: link.template.clone(),
        task: Some(seed_task(&row.id, path)),
        reason: REASON.to_string(),
        model: link.model.clone(),
        effort: link.effort.clone(),
        resume: None,
        permission_mode: row
            .permission_mode
            .clone()
            .filter(|mode| !mode.is_empty())
            .unwrap_or_else(|| "default".to_string()),
        autopilot: Some(row.autopilot),
        role: row.role.clone(),
        purpose: link.purpose.clone(),
        review_of: None,
        from_handoff: None,
        budget: row
            .budget
            .clone()
            .and_then(|map| serde_json::from_value(Value::Object(map)).ok()),
        worktree: None,
        repo_map: None,
        cold_wake_opt_out: false,
        explicit_null: Vec::new(),
    };
    let mut engine_started = false;
    match start_session_tracked(ctx, &params, &mut engine_started) {
        Ok(started) => Ok(Replacement::Started(
            started.session.engine_session_id.clone(),
        )),
        Err(error) if engine_started => {
            tracing::warn!(
                "context rotation of {}: the replacement may be running: {}",
                row.id,
                error.message()
            );
            Ok(Replacement::Unknown)
        }
        Err(error) => {
            tracing::warn!(
                "context rotation of {} refused: {}",
                row.id,
                error.message()
            );
            Ok(Replacement::NotStarted)
        }
    }
}

fn record_rotation<C, I>(
    ctx: &AppContext<C, I>,
    old_id: &str,
    outcome: &Value,
) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        ROTATE_OP,
        REASON,
        "session",
        old_id,
        outcome,
        "session.context_rotated",
        Some(outcome),
    )
}
