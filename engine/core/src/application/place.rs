//! Small aggregate reads owned by a product surface. Ports
//! `src/vogt/application/services/place.py`.
//!
//! `place.metrics` answers every shell badge in one bounded response. A badge
//! whose source is not ported yet is `null`, the same shape Python returns
//! when a provider raises: one unavailable answer never hides the others or
//! fails the route. The saved Inbox filter, the backlog count and the inbox
//! badge itself are those sources. `work.list` and `drift.list` are ported, so
//! their badges are real answers.

use serde_json::{json, Value};

use super::context::Built;
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

/// Return all shell badge values in one bounded response.
pub fn place_metrics_op(ctx: &Built, _params: Value) -> Result<Value, VogtError> {
    match ctx {
        Built::SystemRandom(ctx) => metrics(ctx),
        Built::SystemSequential(ctx) => metrics(ctx),
        Built::StepRandom(ctx) => metrics(ctx),
        Built::StepSequential(ctx) => metrics(ctx),
    }
}

fn metrics<C: crate::core::Clock, I: crate::core::IdFactory>(
    ctx: &super::context::AppContext<C, I>,
) -> Result<Value, VogtError> {
    let projects_total = ctx.declared.read()?.counts()?.projects;
    let revision = ctx.declared.read()?.current_revision()?;
    let generated_at = crate::application::services::now_of(&ctx.clock);
    // `work.list` and `list_drift` are ported, so both badges are real answers.
    // The inbox and backlog badges read services that are not, and a null there
    // means "not available", which is distinct from a counted zero.
    // The badge counts every open item, the same total `work.list` reports with
    // no project scope and the default filter. The upstream join adds nothing
    // when no project is linked, which is the only state the declared store can
    // answer on its own.
    let work_total =
        ctx.declared
            .read()?
            .count_work_items(&crate::storage::interface::WorkFilter {
                exclude_terminal: true,
                limit: 1,
                ..crate::storage::interface::WorkFilter::default()
            })?;
    let drift_present = !ctx
        .declared
        .read()?
        .list_drift(Some("open"), None, None, 1)?
        .is_empty();
    Ok(json!({
        "inbox_active": Value::Null,
        "inbox_active_unfiltered": Value::Null,
        "inbox_filter": Value::Null,
        "projects_total": projects_total,
        "work_total": work_total,
        "backlog_total_considered": Value::Null,
        "drift_present": drift_present,
        "revision": revision,
        "generated_at": generated_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ActorKind, Principal, SequentialIds, StepClock};

    #[test]
    fn metrics_report_the_project_count_and_leave_unported_badges_absent() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vogt-place-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            ..crate::config::VogtConfig::default()
        };
        let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
        let built = crate::application::context::build_context(
            config,
            Some(principal.clone()),
            Some(StepClock::new(crate::core::Moment::from_unix(
                1_700_000_000,
                0,
            ))),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            unreachable!()
        };
        ctx.declared.migrate().unwrap();
        ctx.declared.bootstrap(&principal).unwrap();
        let result = place_metrics_op(&built, Value::Null).unwrap();
        assert_eq!(result["projects_total"], 0);
        assert!(result["inbox_active"].is_null());
        // No work items exist, so the ported work badge answers zero.
        assert_eq!(result["work_total"], 0);
        // No proposals exist, so the ported drift badge answers false rather
        // than null.
        assert_eq!(result["drift_present"], false);
        assert_eq!(result["revision"], 0);
    }
}
