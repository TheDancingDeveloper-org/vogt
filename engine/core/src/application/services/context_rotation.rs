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
//!    one. The rotation is an audited `session.context_rotated` event.
//!
//! Idempotent: a hibernated session is not a candidate, and an unanswered ask
//! waits for its marker to age. The old session is not woken by this module,
//! but `session.input` to it would wake it: tell reviewers the new id.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::json;

use super::sessions::dispatch_preferences;
use super::sessions::{hibernate, start_session, HibernateParams, StartSessionParams};
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

/// Ask and rotate every live session over its role's context budget.
pub fn rotate_over_budget<C, I>(ctx: &AppContext<C, I>) -> Result<RotationOutcome, VogtError>
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
    for row in rows.iter().filter(|row| row.alive && !row.hibernated()) {
        let Some(budget) = budgets.get(&row.role).copied() else {
            continue;
        };
        let Some(tokens) = context_tokens(row).filter(|tokens| *tokens > budget) else {
            continue;
        };
        let path = handoff_path(&dir, &row.id);
        let written = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 0);
        if !written {
            // Never interrupt a turn in progress.
            if row.activity != "idle" {
                continue;
            }
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
        let link = ctx.declared.read()?.session_by_engine_id(&row.id)?;
        let Some(link) = link else {
            tracing::warn!(
                "context rotation: {} has no linked project to restart in",
                row.id
            );
            continue;
        };
        let slug = ctx
            .declared
            .read()?
            .project_by_id(&link.project_id)?
            .map(|project| project.slug);
        let Some(slug) = slug else { continue };
        let params = StartSessionParams {
            project: Some(slug),
            work_item: None,
            name: (!row.name.is_empty()).then(|| row.name.clone()),
            template: link.template.clone(),
            task: Some(seed_task(&row.id, &path)),
            reason: REASON.to_string(),
            model: link.model.clone(),
            effort: link.effort.clone(),
            resume: None,
            permission_mode: "default".to_string(),
            autopilot: None,
            role: row.role.clone(),
            purpose: link.purpose.clone(),
            review_of: None,
            from_handoff: None,
            budget: None,
            worktree: None,
            repo_map: None,
            explicit_null: Vec::new(),
        };
        let started = match start_session(ctx, &params) {
            Ok(started) => started,
            Err(error) => {
                tracing::warn!(
                    "context rotation of {} refused: {}",
                    row.id,
                    error.message()
                );
                continue;
            }
        };
        let new_id = started.session.engine_session_id.clone();
        let outcome = json!({
            "from": row.id,
            "to": new_id,
            "role": row.role,
            "context_tokens": tokens,
            "budget_tokens": budget,
            "handoff_file": path.display().to_string(),
        });
        let mut writing = write_of(ctx);
        audited_action(
            &mut writing,
            ROTATE_OP,
            REASON,
            "session",
            &row.id,
            &outcome,
            "session.context_rotated",
            Some(&outcome),
        )?;
        if let Err(error) = hibernate(
            ctx,
            &HibernateParams {
                id: row.id.clone(),
                reason: format!("rotated to {new_id}: {REASON}"),
                allow_shell: false,
            },
        ) {
            tracing::warn!(
                "context rotation: hibernating {} failed: {}",
                row.id,
                error.message()
            );
        }
        done.rotated.push((row.id.clone(), new_id));
    }
    Ok(done)
}
