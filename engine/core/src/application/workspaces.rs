//! `workspace.list` (WI-1340): every session's working tree, joined to Vogt.
//! Ports `src/vogt/application/services/workspaces.py`.
//!
//! The engine owns the trees: `GET /api/workspaces` gives each session's
//! workspace block with its size and whether it is running. Vogt adds what the
//! engine cannot know — the project and work item the session was started for —
//! and whether the branch has landed, read from the `git-local` collector's
//! `git.branch` observations: a branch with nothing ahead of the project's
//! default branch has its tip in that branch, so it is merged. A branch the
//! collector has not observed is `merged: null`, never a guess.

use std::collections::HashMap;

use serde_json::Value;

use crate::application::context::{AppContext, Built};
use crate::application::models::workspace::{
    WorkspaceListParams, WorkspaceListResult, WorkspaceRow,
};
use crate::application::services::sessions::engine_of;
use crate::core::Project;
use crate::core::{py_repr, Clock, IdFactory};
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

#[cfg(test)]
#[path = "workspaces_test.rs"]
mod workspaces_test;
