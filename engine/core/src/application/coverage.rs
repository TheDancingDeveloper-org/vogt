//! What has looked at what, and how long ago.
//!
//! Ports `coverage` from `services/collect.py`. The answer to "has anything
//! even looked at this repo lately", which is the question the observation
//! layer exists to make answerable. `projects` is cumulative — how many
//! projects a collector has ever swept — because the most recent sweep's scope
//! cannot answer that.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

/// The collectors this instance can run, in the order Python's registry yields
/// them once sorted by name.
///
/// The core collectors are always present. The forge collectors register only
/// when a forge is configured, deploy lanes only when a lane is, and the
/// session and agent collectors only when their source is configured. An
/// absent collector is "not collected", which is a different answer from
/// "there are none", so it must not appear here at all.
fn collector_names<C, I>(ctx: &AppContext<C, I>) -> Vec<&'static str>
where
    C: Clock,
    I: IdFactory,
{
    let mut names = vec![
        "contract-checker",
        "dep-refs",
        "git-local",
        "source-markers",
    ];
    names.push("mirrored-source");
    if crate::adapters::forge::has_configured_forge(&ctx.config) {
        names.extend([
            "forge-issues",
            "forge-prs",
            "forge-checks",
            "forge-releases",
            "forge-labels",
            "forge-posture",
            "forge-notifications",
        ]);
    }
    if !ctx.config.deploy_lanes.is_empty() {
        names.push("deploy-lanes");
    }
    if ctx.engine.is_some() {
        names.push("session-outcomes");
    }
    if !ctx.config.agent_activity_roots.is_empty() {
        names.push("agent-activity");
    }
    names.sort_unstable();
    names
}

/// What has looked at what.
pub fn coverage_op(ctx: &Built, _params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| coverage(ctx))
}

fn coverage<C: Clock, I: IdFactory>(ctx: &AppContext<C, I>) -> Result<Value, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Ok(json!({ "collectors": [], "swept_project_ids": [] }));
    }
    let newest = ctx.observed.coverage()?;
    // Cumulative rather than last-sweep: with eight projects registered and
    // the last sweep scoped to one, a last-sweep count reads as seven unswept
    // projects when they were in fact swept earlier.
    let ever = ctx.observed.coverage_by_project()?;
    let registered: BTreeSet<String> = ctx
        .declared
        .read()?
        .list_projects(10_000, 0)?
        .into_iter()
        .map(|project| project.id)
        .collect();
    let now = clock_now(&ctx.clock);

    let mut swept: BTreeSet<String> = BTreeSet::new();
    let mut entries = Vec::new();
    for name in collector_names(ctx) {
        let seen = ever.get(name).cloned().unwrap_or_default();
        swept.extend(seen.keys().cloned());
        let Some(sweep) = newest.get(name) else {
            entries.push(json!({
                "collector": name,
                "status": "never_run",
                "last_swept_at": Value::Null,
                "age_seconds": Value::Null,
                "projects": 0,
                "registered": registered.len(),
                "last_sweep_scope": 0,
                "never_swept": 0,
                "detail": "this collector has not completed a sweep",
            }));
            continue;
        };
        let finished = sweep.finished_at.unwrap_or(sweep.started_at);
        let never_swept = registered
            .difference(&seen.keys().cloned().collect())
            .count();
        entries.push(json!({
            "collector": name,
            "status": sweep.outcome.to_string(),
            "last_swept_at": finished.to_json(),
            "age_seconds": now.seconds_since(finished) as i64,
            "projects": seen.len(),
            "registered": registered.len(),
            "last_sweep_scope": sweep.scope.len(),
            "never_swept": never_swept,
            "detail": sweep.detail,
        }));
    }
    let unswept: Vec<&String> = registered.difference(&swept).collect();
    let mut swept_ids: Vec<&String> = swept.iter().collect();
    swept_ids.sort();
    Ok(json!({
        "collectors": entries,
        "swept_project_ids": swept_ids,
        "unswept_project_ids": unswept,
    }))
}

fn clock_now<C: Clock>(clock: &std::sync::Arc<std::sync::Mutex<C>>) -> crate::core::Moment {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now()
}
