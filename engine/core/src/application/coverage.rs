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
/// The core collectors are always present. Session-outcomes registers only when
/// an engine is configured, and agent-activity only when transcript roots are.
/// the forge sync pair and the five forge read collectors run when a forge is
/// configured. Deploy-lanes is not ported, but coverage still lists it: Python
/// registers it whenever a lane is configured, and an omitted name reads as
/// "not configured" rather than "never run".
fn collector_names<C, I>(ctx: &AppContext<C, I>) -> Result<Vec<&'static str>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let mut names = vec![
        "contract-checker",
        "dep-refs",
        "git-local",
        "source-markers",
        "mirrored-source",
    ];
    if ctx.engine.is_some() {
        names.push("session-outcomes");
    }
    if !ctx.config.agent_activity_roots.is_empty() {
        names.push("agent-activity");
    }
    if crate::adapters::forge::has_configured_forge(&ctx.config)? {
        names.extend([
            "forge-checks",
            "forge-issues",
            "forge-labels",
            "forge-notifications",
            "forge-posture",
            "forge-prs",
            "forge-releases",
        ]);
    }
    if !ctx.config.deploy_lanes.is_empty() {
        names.push("deploy-lanes");
    }
    names.sort_unstable();
    Ok(names)
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
    for name in collector_names(ctx)? {
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

#[cfg(test)]
mod tests {
    use super::coverage_op;
    use crate::application::services::services_test::{moment, opened, principal, unique};
    use crate::core::SweepOutcome;
    use crate::storage::interface::ObservedStore;
    use serde_json::json;

    /// A fresh instance has run no collector. Every one is `never_run`, and
    /// nothing is reported as swept.
    #[test]
    fn coverage_on_a_fresh_instance_says_nothing_has_run() {
        let built = opened();
        let answer = coverage_op(&built, json!({})).unwrap();
        let collectors = answer["collectors"].as_array().unwrap();
        assert!(!collectors.is_empty());
        assert!(collectors.iter().all(|row| row["status"] == "never_run"));
        assert_eq!(answer["swept_project_ids"].as_array().unwrap().len(), 0);
    }

    /// A finished sweep is counted, and a collector that has not run stays
    /// `never_run` beside it.
    #[test]
    fn coverage_counts_a_collector_that_has_finished() {
        let built = opened();
        let crate::application::context::Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        let sweep = ctx
            .observed
            .begin_sweep("git-local", &[], moment())
            .unwrap();
        ctx.observed
            .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], moment(), None)
            .unwrap();
        let answer = coverage_op(&built, json!({})).unwrap();
        let git = answer["collectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["collector"] == "git-local")
            .expect("git-local is a core collector");
        assert_eq!(git["status"], "ok");
        let contract = answer["collectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["collector"] == "contract-checker")
            .unwrap();
        assert_eq!(contract["status"], "never_run");
    }

    /// Before the evidence tables exist the answer is empty: there is nothing
    /// to have looked at, which is not a list of collectors that never ran.
    #[test]
    fn coverage_before_the_evidence_tables_exist_is_empty() {
        use crate::application::context::build_context;
        use crate::core::{SequentialIds, StepClock};
        let dir =
            std::env::temp_dir().join(format!("vogt-cov-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                ..crate::config::VogtConfig::default()
            },
            Some(principal()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let answer = coverage_op(&built, json!({})).unwrap();
        assert_eq!(answer["collectors"].as_array().unwrap().len(), 0);
        assert_eq!(answer["swept_project_ids"].as_array().unwrap().len(), 0);
    }
}
