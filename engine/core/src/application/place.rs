//! Small aggregate reads owned by a product surface. Ports
//! `src/vogt/application/services/place.py`.
//!
//! `place.metrics` answers every shell badge in one bounded response. A badge
//! whose source is not ported yet is `null`, the same shape Python returns
//! when a provider raises: one unavailable answer never hides the others or
//! fails the route. The saved Inbox filter, the work, backlog and drift
//! counts, and the inbox badge itself are those sources (S3–S5).

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
    let generated_at = ctx
        .clock
        .lock()
        .expect("the clock lock is not poisoned")
        .now();
    Ok(json!({
        // The badge honours the caller's saved Inbox filter (WI-840). Search
        // text is not part of a saved filter, so it never moves the badge.
        // Neither the badge nor the filter is ported yet.
        "inbox_active": Value::Null,
        "inbox_active_unfiltered": Value::Null,
        "inbox_filter": Value::Null,
        "projects_total": projects_total,
        // list_work, backlog and list_drift are not ported yet, so these stay
        // absent rather than reporting a zero that would read as "none".
        "work_total": Value::Null,
        "backlog_total_considered": Value::Null,
        "drift_present": Value::Null,
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
        assert!(result["work_total"].is_null());
        assert!(result["drift_present"].is_null());
        assert_eq!(result["revision"], 0);
    }
}
