//! Small aggregate reads owned by a product surface. Ports
//! `src/vogt/application/services/place.py`.
//!
//! `place.metrics` answers every shell badge in one bounded response. A badge
//! whose source fails is `null`, the same shape Python returns when a provider
//! raises: one unavailable answer never hides the others or fails the route.
//! Read order follows Python's, because the step clock is read along the way
//! and the answer's `generated_at` is the last of those reads.

use std::sync::LazyLock;

use serde_json::{json, Value};

use super::context::{AppContext, Built};
use super::memo::StoreMemo;
use super::services::inbox::inbox_badge;
use super::services::now_of;
use super::services::preferences::{saved_inbox_filter, InboxSavedFilter};
use super::services::work::list_work_total;
use super::views::backlog_total_considered;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

/// The badge counts that are pure functions of the two stores.
#[derive(Debug, Clone)]
struct StoreCounts {
    projects_total: i64,
    work_total: i64,
    backlog_total: Option<i64>,
    drift_present: bool,
}

/// Shared between every badge read while the stores have not moved
/// (WI-1224). `backlog_total_considered` alone ranks the whole backlog, and
/// every tab asked for it on every change.
static COUNTS: LazyLock<StoreMemo<StoreCounts>> = LazyLock::new(StoreMemo::new);

#[cfg(test)]
thread_local! {
    /// How many times this thread computed the store counts.
    pub(crate) static COUNTS_COMPUTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Drop the shared badge counts over `data_dir` (a restore).
pub fn clear_metrics_cache(data_dir: &std::path::Path) {
    COUNTS.forget(data_dir);
}

/// The store counts, in Python's order. The backlog badge is the whole
/// candidate population, which `backlog` ranks and reports.
fn store_counts<C, I>(ctx: &AppContext<C, I>) -> Result<StoreCounts, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    #[cfg(test)]
    COUNTS_COMPUTED.with(|n| n.set(n.get() + 1));
    let projects_total = ctx.declared.read()?.counts()?.projects;
    // Python asks `list_work(limit=1).total`: declared non-terminal rows plus
    // upstream-truth items that no `work_link` has adopted. A raw declared
    // count under-reports every linked project.
    let work_total = list_work_total(ctx)?;
    let backlog_total = backlog_total_considered(ctx).ok();
    let drift_present = !ctx
        .declared
        .read()?
        .list_drift(Some("open"), None, None, 1)?
        .is_empty();
    Ok(StoreCounts {
        projects_total,
        work_total,
        backlog_total,
        drift_present,
    })
}

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
    // The store counts, shared between every badge read while the stores have
    // not moved (WI-1224). An answer with the backlog missing is not
    // remembered: it is asked for again next time.
    let counts = COUNTS.get(
        ctx,
        || store_counts(ctx),
        |counts| counts.backlog_total.is_some(),
    )?;
    let revision = ctx.declared.read()?.current_revision()?;
    let generated_at = now_of(&ctx.clock);
    Ok(json!({
        "inbox_active": badge.map(|(filtered, _)| filtered),
        "inbox_active_unfiltered": badge.map(|(_, active)| active),
        "inbox_filter": saved,
        "projects_total": counts.projects_total,
        "work_total": counts.work_total,
        "backlog_total_considered": counts.backlog_total,
        "drift_present": counts.drift_present,
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

/// Timing harness for the badge counts (WI-700). Ignored in CI: run with
/// `cargo test -p vogt-core place_bench -- --ignored --nocapture`.
#[cfg(test)]
mod place_bench {
    use super::*;
    use crate::application::context::build_context;
    use crate::application::context::AppContext;
    use crate::application::services::work::create_op;
    use crate::core::{ActorKind, Principal};
    use std::time::Instant;

    const ITEMS: usize = 850;

    fn seed_observed<C: crate::core::Clock, I: crate::core::IdFactory>(ctx: &AppContext<C, I>) {
        use crate::core::{Moment, SweepOutcome};
        use crate::storage::interface::ObservedStore;
        use crate::storage::observed_types::PendingObservation;
        let at = Moment::from_unix(1_700_000_000, 0);
        let sweep = ctx
            .observed
            .begin_sweep("bench-forge", &["vogt".to_string()], at)
            .unwrap();
        let mut findings = Vec::new();
        for n in 0..1000 {
            findings.push(PendingObservation {
                kind: "forge.issue".into(),
                subject_key: format!("vogt/issues/{n}"),
                payload: json!({"state": "open", "title": format!("issue {n}"), "number": n, "labels": ["bug"]}),
                content_digest: format!("issue-{n}"),
                project_id: None,
                source_url: None,
                promoted: false,
            });
        }
        for n in 0..300 {
            findings.push(PendingObservation {
                kind: "forge.pull_request".into(),
                subject_key: format!("vogt/pulls/{n}"),
                payload: json!({"state": "open", "title": format!("pr {n}"), "number": n, "head": format!("wi-{n}")}),
                content_digest: format!("pr-{n}"),
                project_id: None,
                source_url: None,
                promoted: false,
            });
        }
        ctx.observed.append(&sweep.id, &findings, at).unwrap();
        ctx.observed
            .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], at, None)
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
    }

    /// The count-only path must answer what the ranked list holds (WI-700).
    #[test]
    fn backlog_count_matches_the_ranked_list() {
        let dir = std::env::temp_dir().join(format!("vogt-place-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(Principal::new("local:count", ActorKind::Human, "Count").unwrap()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        for n in 0..20 {
            create_op(
                &built,
                json!({"kind": "bug", "title": format!("count item {n}"), "reason": "count"}),
            )
            .unwrap();
        }
        crate::with_ctx!(&built, |ctx| seed_observed(ctx));
        let counted = crate::with_ctx!(&built, |ctx| {
            crate::application::views::backlog_total_considered(ctx)
        })
        .unwrap();
        let full = crate::application::views::backlog_op(&built, json!({"limit": 1})).unwrap();
        assert_eq!(counted, full["total_considered"].as_i64().unwrap());
        assert_eq!(counted, 20 + 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "timing harness, run on demand"]
    fn place_bench_seeded_store() {
        let dir = std::env::temp_dir().join(format!("vogt-place-bench-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let fresh = || {
            build_context(
                crate::config::VogtConfig {
                    data_dir: dir.clone(),
                    ..crate::config::VogtConfig::default()
                },
                Some(Principal::new("local:bench", ActorKind::Human, "Bench").unwrap()),
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let built = fresh();
        for n in 0..ITEMS {
            create_op(
                &built,
                json!({"kind": "bug", "title": format!("bench item {n}"), "reason": "bench"}),
            )
            .unwrap();
        }
        crate::with_ctx!(&built, |ctx| seed_observed(ctx));
        let median = |mut v: Vec<u128>| {
            v.sort_unstable();
            v[v.len() / 2]
        };
        let mut cold = Vec::new();
        let mut backlog = Vec::new();
        let mut full_backlog = Vec::new();
        for _ in 0..5 {
            clear_metrics_cache(&dir);
            let built = fresh();
            let t = Instant::now();
            let _ = place_metrics_op(&built, Value::Null).unwrap();
            cold.push(t.elapsed().as_micros());
            let t = Instant::now();
            let total = crate::with_ctx!(&built, |ctx| {
                crate::application::views::backlog_total_considered(ctx)
            })
            .unwrap();
            backlog.push(t.elapsed().as_micros());
            let t = Instant::now();
            let full = crate::application::views::backlog_op(&built, json!({"limit": 1})).unwrap();
            full_backlog.push(t.elapsed().as_micros());
            assert_eq!(total, full["total_considered"].as_i64().unwrap());
            println!("BENCH backlog_total_considered={total}");
        }
        println!(
            "BENCH items={ITEMS} place_metrics_cold_median_us={}",
            median(cold)
        );
        println!(
            "BENCH backlog_total_considered_median_us={}",
            median(backlog)
        );
        println!(
            "BENCH backlog_op_limit1_full_median_us={}",
            median(full_backlog)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
