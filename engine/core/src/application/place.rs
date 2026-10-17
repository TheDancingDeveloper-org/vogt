//! Small aggregate reads owned by a product surface. Ports
//! `src/vogt/application/services/place.py`.
//!
//! `place.metrics` answers every shell badge in one bounded response. A badge
//! whose source fails is `null`, the same shape Python returns when a provider
//! raises: one unavailable answer never hides the others or fails the route.
//! Read order follows Python's, because the step clock is read along the way
//! and the answer's `generated_at` is the last of those reads.

use serde_json::{json, Value};

use super::context::{AppContext, Built};
use super::services::inbox::inbox_badge;
use super::services::now_of;
use super::services::preferences::{saved_inbox_filter, InboxSavedFilter};
use super::views::backlog_total_considered;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WorkFilter};

/// Return all shell badge values in one bounded response.
pub fn place_metrics_op(ctx: &Built, _params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| metrics(ctx))
}

fn metrics<C, I>(ctx: &AppContext<C, I>) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    // The saved filter and the badge are read first, as Python does. An
    // unreadable filter means "no filter", and an unreadable badge is null.
    let saved: Option<InboxSavedFilter> = saved_inbox_filter(ctx).ok().flatten();
    let badge = inbox_badge(ctx, saved.as_ref()).ok();
    // The store counts, in Python's order. The backlog badge is the whole
    // candidate population, which `backlog` ranks and reports.
    let projects_total = ctx.declared.read()?.counts()?.projects;
    let work_total = ctx.declared.read()?.count_work_items(&WorkFilter {
        exclude_terminal: true,
        limit: 1,
        ..WorkFilter::default()
    })?;
    let backlog_total = backlog_total_considered(ctx).ok();
    let drift_present = !ctx
        .declared
        .read()?
        .list_drift(Some("open"), None, None, 1)?
        .is_empty();
    let revision = ctx.declared.read()?.current_revision()?;
    let generated_at = now_of(&ctx.clock);
    Ok(json!({
        "inbox_active": badge.map(|(filtered, _)| filtered),
        "inbox_active_unfiltered": badge.map(|(_, active)| active),
        "inbox_filter": saved,
        "projects_total": projects_total,
        "work_total": work_total,
        "backlog_total_considered": backlog_total,
        "drift_present": drift_present,
        "revision": revision,
        "generated_at": generated_at,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ActorKind, Principal, SequentialIds, StepClock};
    use crate::storage::interface::ObservedStore;

    #[test]
    fn metrics_report_every_badge_as_a_counted_answer() {
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
        ctx.observed.migrate().unwrap();
        ctx.declared.bootstrap(&principal).unwrap();
        let result = place_metrics_op(&built, Value::Null).unwrap();
        assert_eq!(result["projects_total"], 0);
        // An empty Inbox and an empty backlog count as zero, not null: null
        // would mean the provider failed.
        assert_eq!(result["inbox_active"], 0);
        assert_eq!(result["inbox_active_unfiltered"], 0);
        assert!(result["inbox_filter"].is_null());
        assert_eq!(result["backlog_total_considered"], 0);
        // No work items exist, so the work badge answers zero.
        assert_eq!(result["work_total"], 0);
        // No proposals exist, so the ported drift badge answers false rather
        // than null.
        assert_eq!(result["drift_present"], false);
        assert_eq!(result["revision"], 0);
    }
}
