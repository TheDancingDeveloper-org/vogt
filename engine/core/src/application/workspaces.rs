//! `workspace.list` (WI-1340): every session's working tree, joined to Vogt,
//! and `workspace.clean` (WI-1325). Ports
//! `src/vogt/application/services/workspaces.py`.
//!
//! The engine owns the trees: `GET /api/workspaces` gives each session's
//! workspace block with its size and whether it is running. Vogt adds what the
//! engine cannot know — the project and work item the session was started for —
//! and whether the branch has landed, read from the `git-local` collector's
//! `git.branch` observations: a branch with nothing ahead of the project's
//! default branch has its tip in that branch, so it is merged. A branch the
//! collector has not observed is `merged: null`, never a guess.

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::models::workspace::{
    WorkspaceCleanParams, WorkspaceCleanResult, WorkspaceListParams, WorkspaceListResult,
    WorkspaceRow,
};
use crate::application::services::sessions::engine_of;
use crate::application::writes::{audited_action, validate_reason};
use crate::core::Project;
use crate::core::{py_repr, ActorKind, Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

pub(crate) fn workspace_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let parsed: WorkspaceListParams = serde_json::from_value(params)
        .map_err(|error| VogtError::InvalidRequest(format!("invalid parameters: {error}")))?;
    crate::with_ctx!(built, |ctx| workspace_list(ctx, &parsed))
        .map(|result| serde_json::to_value(result).expect("a workspace list serialises"))
}

/// Every live or hibernated session's workspace, with Vogt's join.
pub(crate) fn workspace_list<C, I>(
    ctx: &AppContext<C, I>,
    params: &WorkspaceListParams,
) -> Result<WorkspaceListResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let Some(rows) = engine_of(ctx)?.list_workspaces()? else {
        return Err(VogtError::EngineUnavailable(
            "the session engine does not list workspaces yet (it has no \
             GET /api/workspaces route); upgrade the engine"
                .to_string(),
        ));
    };
    let mut out = Vec::new();
    let mut projects: HashMap<String, Option<Project>> = HashMap::new();
    let mut merged_by_project: HashMap<String, HashMap<String, Option<bool>>> = HashMap::new();
    let view = ctx.declared.read()?;
    let wanted = match params.project.as_deref() {
        None => None,
        Some(slug) => match view.project_by_slug(slug)? {
            Some(project) => Some(project),
            None => return Err(VogtError::NotFound(format!("no project {}", py_repr(slug)))),
        },
    };
    for row in rows {
        if params.alive.is_some_and(|alive| alive != row.alive) {
            continue;
        }
        let linked = view.session_by_engine_id(&row.session_id)?;
        let mut project: Option<Project> = None;
        let mut work_item: Option<String> = None;
        if let Some(linked) = &linked {
            if !projects.contains_key(&linked.project_id) {
                let found = view.project_by_id(&linked.project_id)?;
                projects.insert(linked.project_id.clone(), found);
            }
            project = projects[&linked.project_id].clone();
            if let Some(item_id) = &linked.work_item_id {
                work_item = view.work_item_by_id(item_id)?.map(|item| item.reference);
            }
        }
        if let Some(wanted) = &wanted {
            if project
                .as_ref()
                .is_none_or(|project| project.id != wanted.id)
            {
                continue;
            }
        }
        let mut merged = None;
        if let Some(project) = &project {
            if !merged_by_project.contains_key(&project.id) {
                let branches = merged_branches(ctx, &project.id)?;
                merged_by_project.insert(project.id.clone(), branches);
            }
            merged = merged_by_project[&project.id]
                .get(&row.branch)
                .copied()
                .flatten();
        }
        out.push(WorkspaceRow {
            path: row.path,
            kind: row.kind,
            branch: row.branch,
            base_sha: row.base_sha,
            size_bytes: row.size_bytes,
            cleaned_at: row.cleaned_at,
            alive: row.alive,
            merged,
            work_item,
            session: linked.map_or(row.session_id, |linked| linked.id),
            project: project.map(|project| project.slug),
        });
    }
    Ok(WorkspaceListResult { workspaces: out })
}

/// Branch name -> merged, from the collector's latest observations.
fn merged_branches<C, I>(
    ctx: &AppContext<C, I>,
    project_id: &str,
) -> Result<HashMap<String, Option<bool>>, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut merged = HashMap::new();
    if !ctx.observed.has_evidence_tables()? {
        return Ok(merged);
    }
    let kinds = ["git.branch".to_string()];
    for obs in ctx
        .observed
        .latest(&kinds, Some(project_id), false, false, 1000)?
    {
        let Some(name) = obs.payload.get("name").and_then(Value::as_str) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        // Python's `isinstance(ahead, int)`: a bool is an int there, so
        // `false` reads as 0 (merged) and `true` as 1.
        let ahead = match obs.payload.get("ahead") {
            Some(Value::Bool(flag)) => Some(i64::from(*flag)),
            Some(other) => other.as_i64(),
            None => None,
        };
        merged.insert(name.to_string(), ahead.map(|ahead| ahead == 0));
    }
    Ok(merged)
}

const WORKSPACE_CLEAN: &str = "workspace.clean";
const WORKSPACE_CLEANED_EVENT: &str = "workspace.cleaned";

pub(crate) fn workspace_clean_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    let parsed: WorkspaceCleanParams = serde_json::from_value(params)
        .map_err(|error| VogtError::InvalidRequest(format!("invalid parameters: {error}")))?;
    crate::with_ctx!(built, |ctx| workspace_clean(ctx, &parsed))
        .map(|result| serde_json::to_value(result).expect("a workspace clean serialises"))
}

/// Drop a workspace's build output, or remove its worktree (WI-1325).
///
/// The engine makes every safety check — no session in the tree, a clean
/// tree, a merged branch unless forced — and refuses with a 409 that names
/// the reason. Here: who may force, and the audit row.
pub(crate) fn workspace_clean<C, I>(
    ctx: &AppContext<C, I>,
    params: &WorkspaceCleanParams,
) -> Result<WorkspaceCleanResult, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let reason = validate_reason(&params.reason)?;
    if params.force && ctx.principal.kind == ActorKind::Agent {
        // A forced removal throws away a worktree whose branch has not
        // landed, so it is a person's call, as a grant decision is.
        return Err(VogtError::CleanRefused(format!(
            "only a person forces a workspace removal, not an agent ({}); ask a person to do it",
            ctx.principal.identity_ref
        )));
    }
    if params.id.is_none() == params.path.is_none() {
        return Err(VogtError::InvalidRequest(
            "give exactly one of `id` (a session) or `path` (a workspace)".to_string(),
        ));
    }
    let engine = engine_of(ctx)?;
    let path = match &params.path {
        Some(path) => path.clone(),
        None => path_of_session(ctx, params.id.as_deref().unwrap_or(""))?,
    };
    let mode = params.mode.as_str();
    let Some(done) = engine.clean_workspace(&path, mode, params.force, &reason)? else {
        return Err(VogtError::EngineUnavailable(
            "the session engine cannot clean workspaces yet (it has no \
             POST /api/workspaces/clean route); upgrade the engine"
                .to_string(),
        ));
    };
    let optional_text = |name: &str| match done.get(name) {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    let result = WorkspaceCleanResult {
        // Python's `str(done.get("path", path))`.
        path: match done.get("path") {
            None => path.clone(),
            Some(Value::String(text)) => text.clone(),
            Some(Value::Null) => "None".to_string(),
            Some(other) => other.to_string(),
        },
        mode: mode.to_string(),
        bytes_freed: python_int(done.get("bytes_freed")),
        removed: done.get("removed").is_some_and(python_truthy),
        branch_deleted: optional_text("branch_deleted"),
        cleaned_at: optional_text("cleaned_at"),
    };
    let mut writing = write_of(ctx);
    audited_action(
        &mut writing,
        WORKSPACE_CLEAN,
        &reason,
        "workspace",
        &result.path,
        &json!({
            "mode": result.mode,
            "force": params.force,
            "bytes_freed": result.bytes_freed,
            "removed": result.removed,
            "branch_deleted": result.branch_deleted,
        }),
        WORKSPACE_CLEANED_EVENT,
        None,
    )?;
    Ok(result)
}

/// The workspace path of session `reference` (a `ses_…` id or engine UUID).
fn path_of_session<C, I>(ctx: &AppContext<C, I>, reference: &str) -> Result<String, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let mut engine_id = reference.to_string();
    if reference.starts_with("ses_") {
        if let Some(linked) = ctx.declared.read()?.session_by_id(reference)? {
            if !linked.engine_session_id.is_empty() {
                engine_id = linked.engine_session_id;
            }
        }
    }
    let rows = engine_of(ctx)?.list_workspaces()?.unwrap_or_default();
    match rows.into_iter().find(|row| row.session_id == engine_id) {
        Some(row) => Ok(row.path),
        None => Err(VogtError::NotFound(format!(
            "session {} has no workspace",
            py_repr(reference)
        ))),
    }
}

/// Python's `int(value or 0)` for a decoded JSON number or bool; anything
/// else reads as 0.
fn python_int(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Bool(flag)) => i64::from(*flag),
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|n| n.trunc() as i64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// Python's `bool(value)` for a decoded JSON value.
fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

#[cfg(test)]
#[path = "workspaces_test.rs"]
mod workspaces_test;
