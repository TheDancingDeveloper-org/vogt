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
///
/// The projects and drift reads touch no clock, so they run on a scoped thread
/// beside the work and backlog reads (WI-700). Those two read the stepping
/// clock, so they stay on this thread in Python's order.
fn store_counts<C, I>(ctx: &AppContext<C, I>) -> Result<StoreCounts, VogtError>
where
    C: Clock + Send + 'static,
    I: IdFactory + Send + 'static,
{
    #[cfg(test)]
    COUNTS_COMPUTED.with(|n| n.set(n.get() + 1));
    let (side, work_total, backlog_total) = std::thread::scope(|scope| {
        let side = scope.spawn(|| -> Result<(i64, bool), VogtError> {
            let view = ctx.declared.read()?;
            Ok((view.counts()?.projects, view.has_open_drift()?))
        });
        // Python asks `list_work(limit=1).total`: declared non-terminal rows plus
        // upstream-truth items that no `work_link` has adopted. A raw declared
        // count under-reports every linked project.
        let work_total = list_work_total(ctx);
        let backlog_total = backlog_total_considered(ctx).ok();
        (side.join(), work_total, backlog_total)
    });
    let (projects_total, drift_present) =
        side.unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
    Ok(StoreCounts {
        projects_total,
        work_total: work_total?,
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
    C: Clock + Send + 'static,
    I: IdFactory + Send + 'static,
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
    use crate::storage::interface::WriteTxn;
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

    /// A later sweep whose subjects sort first in `latest`: a PR that implements
    /// a subject already present (dropped), a PR that implements nothing
    /// present (kept), and an unpromoted marker (not worklike, dropped).
    fn seed_newer_observed<C: crate::core::Clock, I: crate::core::IdFactory>(
        ctx: &AppContext<C, I>,
    ) {
        use crate::core::{Moment, SweepOutcome};
        use crate::storage::interface::ObservedStore;
        use crate::storage::observed_types::PendingObservation;
        let at = Moment::from_unix(1_700_000_100, 0);
        let sweep = ctx
            .observed
            .begin_sweep("bench-forge", &["vogt".to_string()], at)
            .unwrap();
        let findings = vec![
            PendingObservation {
                kind: "forge.pull_request".into(),
                subject_key: "vogt/pulls/newer-implements-present".into(),
                payload: json!({"state": "open", "title": "pr implements a present issue", "number": 900, "implements": [{"subject": "vogt/issues/5"}]}),
                content_digest: "pr-newer-present".into(),
                project_id: None,
                source_url: None,
                promoted: false,
            },
            PendingObservation {
                kind: "forge.pull_request".into(),
                subject_key: "vogt/pulls/newer-implements-absent".into(),
                payload: json!({"state": "open", "title": "pr implements nothing present", "number": 901, "implements": [{"subject": "vogt/issues/unknown"}]}),
                content_digest: "pr-newer-absent".into(),
                project_id: None,
                source_url: None,
                promoted: false,
            },
            PendingObservation {
                kind: "marker".into(),
                subject_key: "vogt/markers/newer-unpromoted".into(),
                payload: json!({"tag": "TODO", "path": "x.rs", "line": 1}),
                content_digest: "marker-newer".into(),
                project_id: None,
                source_url: None,
                promoted: false,
            },
        ];
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
        // Newer subjects displace the oldest issues in `latest(1000)`: the PR
        // implementing a present issue drops, the other PR and the issues count.
        crate::with_ctx!(&built, |ctx| seed_newer_observed(ctx));
        let counted_newer = crate::with_ctx!(&built, |ctx| {
            crate::application::views::backlog_total_considered(ctx)
        })
        .unwrap();
        let full_newer =
            crate::application::views::backlog_op(&built, json!({"limit": 1})).unwrap();
        assert_eq!(
            counted_newer,
            full_newer["total_considered"].as_i64().unwrap()
        );
        assert_eq!(counted_newer, 20 + 998);
        // The concurrent badge reads answer what the sequential reads answer.
        let metrics = place_metrics_op(&built, Value::Null).unwrap();
        crate::with_ctx!(&built, |ctx| {
            let view = ctx.declared.read().unwrap();
            assert_eq!(metrics["projects_total"], view.counts().unwrap().projects);
            assert_eq!(metrics["work_total"], list_work_total(ctx).unwrap());
            assert_eq!(metrics["backlog_total_considered"], counted_newer);
            let drift = !view
                .list_drift(Some("open"), None, None, 1)
                .unwrap()
                .is_empty();
            assert_eq!(view.has_open_drift().unwrap(), drift);
            assert_eq!(metrics["drift_present"], drift);
        });
        // An open drift proposal flips the badge and the EXISTS check to true,
        // and the same answer comes out of a sequential read.
        assert_eq!(metrics["drift_present"], false);
        crate::with_ctx!(&built, |ctx| {
            let mut txn = ctx.declared.write().unwrap();
            txn.insert_drift(&crate::core::DriftProposal {
                id: "drift-open-count".to_string(),
                kind: "work_item.unlinked".to_string(),
                subject_kind: "project".to_string(),
                subject_id: "prj".to_string(),
                project_id: None,
                project_slug: None,
                summary: "open drift for the badge".to_string(),
                evidence_observation_id: None,
                evidence_snapshot: json!({}),
                proposed_change: json!({}),
                status: crate::core::DriftStatus::Open,
                opened_at: crate::core::Moment::from_unix(1_700_000_000, 0),
                superseded_at: None,
                superseded_detail: None,
                resolved_by_actor_id: None,
                resolved_by_identity_ref: None,
                resolved_at: None,
                resolution_reason: None,
            })
            .unwrap();
            txn.commit().unwrap();
        });
        let with_drift = place_metrics_op(&built, Value::Null).unwrap();
        assert_eq!(with_drift["drift_present"], true);
        // The open-drift case: the count still equals the live ranked list.
        crate::with_ctx!(&built, |ctx| {
            let live = crate::application::views::backlog_total_considered(ctx).unwrap();
            assert_eq!(live, counted_newer);
        });
        let full_with_drift =
            crate::application::views::backlog_op(&built, json!({"limit": 1})).unwrap();
        assert_eq!(full_with_drift["total_considered"], counted_newer);
        crate::with_ctx!(&built, |ctx| {
            let view = ctx.declared.read().unwrap();
            assert!(view.has_open_drift().unwrap());
            assert!(!view
                .list_drift(Some("open"), None, None, 1)
                .unwrap()
                .is_empty());
        });
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
        let mut parts: Vec<(&str, Vec<u128>)> = vec![
            ("inbox_badge", vec![]),
            ("projects_count", vec![]),
            ("work_total", vec![]),
            ("drift_present", vec![]),
        ];
        for _ in 0..9 {
            let built = fresh();
            crate::with_ctx!(&built, |ctx| {
                let saved = saved_inbox_filter(ctx).ok().flatten();
                let t = Instant::now();
                let _ = inbox_badge(ctx, saved.as_ref());
                parts[0].1.push(t.elapsed().as_micros());
                let t = Instant::now();
                let _ = ctx.declared.read().unwrap().counts().unwrap().projects;
                parts[1].1.push(t.elapsed().as_micros());
                let t = Instant::now();
                let _ = list_work_total(ctx).unwrap();
                parts[2].1.push(t.elapsed().as_micros());
                let t = Instant::now();
                let _ = !ctx
                    .declared
                    .read()
                    .unwrap()
                    .list_drift(Some("open"), None, None, 1)
                    .unwrap()
                    .is_empty();
                parts[3].1.push(t.elapsed().as_micros());
            });
        }
        for (name, v) in parts {
            println!("BENCH part_{name}_median_us={}", median(v));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A prod-sized store: 20k declared rows, 50k observed subjects (ranking
    /// reads the newest 1000), 200 suppressions, 5k PRs implementing issues.
    /// Run on demand:
    /// `cargo test -p vogt-core --bin vogt-core place_bench_prod_sized -- --ignored --nocapture`.
    #[test]
    #[ignore = "prod-sized timing harness, run on demand"]
    fn place_bench_prod_sized() {
        const DECLARED: usize = 20_000;
        const ISSUES: usize = 45_000;
        const PULLS: usize = 5_000;
        const SUPPRESSIONS: usize = 200;
        // `VOGT_PROD_BENCH_DIR` names a store to keep: it is seeded once if empty,
        // then every later run reads that same seed, so before and after match.
        let reuse = std::env::var_os("VOGT_PROD_BENCH_DIR").map(std::path::PathBuf::from);
        let dir = reuse.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("vogt-place-prod-bench-{}", std::process::id()))
        });
        let already_seeded = dir.join("declared.sqlite3").exists();
        if !already_seeded {
            if reuse.is_none() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            std::fs::create_dir_all(&dir).unwrap();
            let mut clock = None;
            let mut ids = None;
            crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        }
        let fresh = || {
            build_context(
                crate::config::VogtConfig {
                    data_dir: dir.clone(),
                    ..crate::config::VogtConfig::default()
                },
                Some(Principal::new("local:prod", ActorKind::Human, "Prod").unwrap()),
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
        // A boot migrates the observed store; a kept store from before the index needs it too.
        crate::with_ctx!(&built, |ctx| {
            use crate::storage::interface::ObservedStore;
            ctx.observed.migrate().unwrap();
        });
        if !already_seeded {
            let seeded = Instant::now();
            for n in 0..DECLARED {
                create_op(
                    &built,
                    json!({"kind": "bug", "title": format!("prod item {n}"), "reason": "bench"}),
                )
                .unwrap();
            }
            println!("PROD seeded declared={DECLARED} in {:?}", seeded.elapsed());
            let seeded = Instant::now();
            crate::with_ctx!(&built, |ctx| {
                use crate::core::{Moment, SweepOutcome};
                use crate::storage::interface::ObservedStore;
                use crate::storage::observed_types::PendingObservation;
                let at = Moment::from_unix(1_700_000_000, 0);
                let sweep = ctx
                    .observed
                    .begin_sweep("bench-forge", &["vogt".to_string()], at)
                    .unwrap();
                let mut findings = Vec::with_capacity(ISSUES + PULLS);
                for n in 0..ISSUES {
                    let labels = if n % 2 == 0 {
                        json!(["bug"])
                    } else {
                        json!([])
                    };
                    findings.push(PendingObservation {
                        kind: "forge.issue".into(),
                        subject_key: format!("vogt/issues/{n}"),
                        payload: json!({"state": "open", "title": format!("issue {n}"), "number": n, "labels": labels}),
                        content_digest: format!("issue-{n}"),
                        project_id: None,
                        source_url: None,
                        promoted: false,
                    });
                }
                for n in 0..PULLS {
                    findings.push(PendingObservation {
                        kind: "forge.pull_request".into(),
                        subject_key: format!("vogt/pulls/{n}"),
                        payload: json!({"state": "open", "title": format!("pr {n}"), "number": n, "head": format!("wi-{n}"), "implements": [{"subject": format!("vogt/issues/{}", n * 9 % ISSUES)}]}),
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
            });
            println!(
                "PROD seeded observed={} in {:?}",
                ISSUES + PULLS,
                seeded.elapsed()
            );
            for n in 0..SUPPRESSIONS {
                let subject = if n % 20 == 0 {
                    json!({"subject": format!("vogt/pulls/old-{n}-*"), "reason": "bench", "pattern": true})
                } else {
                    json!({"subject": format!("vogt/issues/{}", n * 211 % ISSUES), "reason": "bench"})
                };
                crate::application::suppressions::suppress_op(&built, subject).unwrap();
            }
        }
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
            println!("PROD backlog_total_considered={total}");
        }
        println!("PROD place_metrics_cold_median_us={}", median(cold));
        println!(
            "PROD backlog_total_considered_median_us={}",
            median(backlog)
        );
        println!(
            "PROD backlog_op_limit1_full_median_us={}",
            median(full_backlog)
        );
        if reuse.is_none() {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// The count's drops equal the ranked list's (WI-4448): a closed issue and an
    /// overlay-terminal issue on a linked project, an exact and a glob suppression
    /// hit, a PR implementing a present subject, and an unpromoted marker.
    #[test]
    fn backlog_count_drops_match_the_ranked_list() {
        let dir = std::env::temp_dir().join(format!("vogt-place-drops-{}", std::process::id()));
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
            Some(Principal::new("local:drops", ActorKind::Human, "Drops").unwrap()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let at = crate::core::Moment::from_unix(1_700_000_200, 0);
        crate::with_ctx!(&built, |ctx| {
            use crate::storage::interface::{DeclaredStore, WriteTxn};
            let mut txn = ctx.declared.write().unwrap();
            let mut project =
                crate::core::Project::new("prj_linked", "linked", "Linked", "/srv/linked", at);
            project.link_state = crate::core::LinkState::Linked;
            txn.insert_project(&project).unwrap();
            txn.upsert_work_overlay(&crate::core::WorkOverlay {
                subject_key: "vogt/linked/overlay-done".into(),
                project_id: "prj_linked".into(),
                rank: None,
                workflow_state: Some("done".into()),
                priority: None,
                effort: None,
                assignee_actor_id: None,
                initiative_id: None,
                branches: Vec::new(),
                created_at: at,
                updated_at: at,
            })
            .unwrap();
            txn.commit().unwrap();
        });
        crate::with_ctx!(&built, |ctx| {
            use crate::core::SweepOutcome;
            use crate::storage::interface::ObservedStore;
            use crate::storage::observed_types::PendingObservation;
            let sweep = ctx
                .observed
                .begin_sweep("bench-forge", &["vogt".to_string()], at)
                .unwrap();
            let issue = |key: &str, project: Option<&str>, payload: Value| PendingObservation {
                kind: "forge.issue".into(),
                subject_key: key.into(),
                payload,
                content_digest: key.into(),
                project_id: project.map(str::to_string),
                source_url: None,
                promoted: false,
            };
            let findings = vec![
                issue(
                    "vogt/linked/closed-1",
                    Some("prj_linked"),
                    json!({"state": "closed", "title": "closed upstream", "labels": []}),
                ),
                issue(
                    "vogt/linked/overlay-done",
                    Some("prj_linked"),
                    json!({"state": "open", "title": "done in vogt", "labels": []}),
                ),
                issue(
                    "vogt/linked/open-kept",
                    Some("prj_linked"),
                    json!({"state": "open", "title": "open linked", "labels": []}),
                ),
                issue(
                    "vogt/linked/bug-kept",
                    Some("prj_linked"),
                    json!({"state": "open", "title": "bug linked", "labels": ["bug"]}),
                ),
                issue(
                    "vogt/linked/exact-suppressed",
                    None,
                    json!({"state": "open", "title": "exact hit", "labels": []}),
                ),
                issue(
                    "vogt/linked/glob-suppressed",
                    None,
                    json!({"state": "open", "title": "glob hit", "labels": []}),
                ),
                PendingObservation {
                    kind: "forge.pull_request".into(),
                    subject_key: "vogt/pulls/implements-present".into(),
                    payload: json!({"state": "open", "title": "implements a present subject", "implements": [{"subject": "vogt/linked/open-kept"}]}),
                    content_digest: "pr-implements-present".into(),
                    project_id: None,
                    source_url: None,
                    promoted: false,
                },
                PendingObservation {
                    kind: "forge.pull_request".into(),
                    subject_key: "vogt/pulls/implements-absent".into(),
                    payload: json!({"state": "open", "title": "implements nothing present", "implements": [{"subject": "vogt/nowhere/9"}]}),
                    content_digest: "pr-implements-absent".into(),
                    project_id: None,
                    source_url: None,
                    promoted: false,
                },
                PendingObservation {
                    kind: "marker".into(),
                    subject_key: "vogt/markers/unpromoted".into(),
                    payload: json!({"tag": "TODO", "path": "x.rs", "line": 1}),
                    content_digest: "marker-unpromoted".into(),
                    project_id: None,
                    source_url: None,
                    promoted: false,
                },
                issue(
                    "vogt/linked/adopted",
                    None,
                    json!({"state": "open", "title": "adopted by a work item", "labels": []}),
                ),
                issue(
                    "vogt/linked/scoped-hit",
                    Some("prj_linked"),
                    json!({"state": "open", "title": "scoped exact hit", "labels": []}),
                ),
                issue(
                    "vogt/linked/scoped-miss",
                    None,
                    json!({"state": "open", "title": "scoped exact, other scope", "labels": []}),
                ),
                PendingObservation {
                    kind: "marker".into(),
                    subject_key: "vogt/markers/promoted".into(),
                    payload: json!({"tag": "TODO", "path": "y.rs", "line": 2}),
                    content_digest: "marker-promoted".into(),
                    project_id: None,
                    source_url: None,
                    promoted: true,
                },
            ];
            ctx.observed.append(&sweep.id, &findings, at).unwrap();
            ctx.observed
                .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], at, None)
                .unwrap();
            ctx.observed.rebuild_latest().unwrap();
        });
        crate::application::suppressions::suppress_op(
            &built,
            json!({"subject": "vogt/linked/exact-suppressed", "reason": "drops"}),
        )
        .unwrap();
        crate::application::suppressions::suppress_op(
            &built,
            json!({"subject": "vogt/linked/glob-*", "reason": "drops", "pattern": true}),
        )
        .unwrap();
        let adopted = create_op(
            &built,
            json!({"kind": "feature", "title": "adopts a subject", "reason": "drops"}),
        )
        .unwrap();
        let adopted_id = adopted["item"]["id"].as_str().unwrap().to_string();
        crate::with_ctx!(&built, |ctx| {
            use crate::storage::interface::{DeclaredStore, WriteTxn};
            let mut txn = ctx.declared.write().unwrap();
            txn.insert_work_link(&crate::core::WorkLink {
                work_item_id: adopted_id.clone(),
                subject_key: "vogt/linked/adopted".into(),
                origin_kind: "drops".into(),
                source_url: None,
                relation: crate::core::LinkRelation::Completion,
                created_at: at,
            })
            .unwrap();
            txn.commit().unwrap();
        });
        for subject in ["vogt/linked/scoped-hit", "vogt/linked/scoped-miss"] {
            crate::application::suppressions::suppress_op(
                &built,
                json!({"subject": subject, "reason": "drops", "project": "linked"}),
            )
            .unwrap();
        }
        let counted = crate::with_ctx!(&built, |ctx| {
            crate::application::views::backlog_total_considered(ctx)
        })
        .unwrap();
        let full = crate::application::views::backlog_op(&built, json!({"limit": 1000})).unwrap();
        assert_eq!(counted, full["total_considered"].as_i64().unwrap());
        // Kept: the open linked issue, the bug-labelled linked issue, the PR whose
        // implements target is absent, the out-of-scope suppression, and the
        // promoted marker, plus the declared WI-1 that adopts the adopted subject.
        // Everything else is dropped.
        assert_eq!(counted, 6);
        let listed = full["items"].to_string();
        for kept in [
            "vogt/linked/open-kept",
            "vogt/linked/bug-kept",
            "vogt/pulls/implements-absent",
            "vogt/linked/scoped-miss",
            "vogt/markers/promoted",
        ] {
            assert!(listed.contains(kept), "{kept} should be ranked");
        }
        for dropped in [
            "vogt/linked/closed-1",
            "vogt/linked/overlay-done",
            "vogt/linked/exact-suppressed",
            "vogt/linked/glob-suppressed",
            "vogt/pulls/implements-present",
            "vogt/markers/unpromoted",
            "vogt/linked/adopted",
            "vogt/linked/scoped-hit",
        ] {
            assert!(!listed.contains(dropped), "{dropped} should be dropped");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The count reads the newest observations through `idx_latest_observed`
    /// (WI-4448), so the plan must name it and must not sort the whole set.
    #[test]
    fn backlog_count_plan_uses_the_observed_order_index() {
        let dir = std::env::temp_dir().join(format!("vogt-place-plan-{}", std::process::id()));
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
            Some(Principal::new("local:plan", ActorKind::Human, "Plan").unwrap()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        crate::with_ctx!(&built, |ctx| seed_observed(ctx));
        let (observed_db, declared_db) = crate::with_ctx!(&built, |ctx| (
            ctx.config.observed_db_path(),
            ctx.config.declared_db_path()
        ));
        let conn = rusqlite::Connection::open(&observed_db).unwrap();
        crate::storage::sqlite::observed::open_backlog_count(&conn, &declared_db).unwrap();
        let sql = crate::storage::sqlite::observed::backlog_count_sql(&conn).unwrap();
        let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let plan: Vec<String> = statement
            .query_map(rusqlite::params!["bug", "open"], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            plan.iter()
                .any(|detail| detail.contains("idx_latest_observed")),
            "the count should read idx_latest_observed, got {plan:?}"
        );
        drop(statement);
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A store still at observed schema 7 has no `idx_latest_observed`. The
    /// count must not name it: it answers from the planner's own choice and
    /// still equals the ranked total (WI-4448).
    #[test]
    fn backlog_count_runs_on_a_schema_7_store() {
        let dir = std::env::temp_dir().join(format!("vogt-place-schema7-{}", std::process::id()));
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
            Some(Principal::new("local:schema7", ActorKind::Human, "Schema7").unwrap()),
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
                json!({"kind": "bug", "title": format!("schema7 item {n}"), "reason": "schema7"}),
            )
            .unwrap();
        }
        crate::with_ctx!(&built, |ctx| seed_observed(ctx));
        // Take the index away, as a schema-7 store has it.
        let (observed_db, _) = crate::with_ctx!(&built, |ctx| (
            ctx.config.observed_db_path(),
            ctx.config.declared_db_path()
        ));
        {
            let conn = rusqlite::Connection::open(&observed_db).unwrap();
            conn.execute_batch("DROP INDEX idx_latest_observed;")
                .unwrap();
        }
        let counted = crate::with_ctx!(&built, |ctx| {
            crate::application::views::backlog_total_considered(ctx)
        })
        .unwrap();
        let full = crate::application::views::backlog_op(&built, json!({"limit": 1})).unwrap();
        assert_eq!(counted, full["total_considered"].as_i64().unwrap());
        assert_eq!(counted, 20 + 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Closed subjects leave the set before the 1000-row cap, as the ranked read
    /// does, so a newer closed run cannot push open work out of the count.
    #[test]
    fn backlog_count_filters_closed_before_the_cap() {
        let dir = std::env::temp_dir().join(format!("vogt-place-closed-{}", std::process::id()));
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
            Some(Principal::new("local:closed", ActorKind::Human, "Closed").unwrap()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        crate::with_ctx!(&built, |ctx| {
            seed_observed(ctx);
            use crate::core::{Moment, SweepOutcome};
            use crate::storage::interface::ObservedStore;
            use crate::storage::observed_types::PendingObservation;
            let at = Moment::from_unix(1_700_000_300, 0);
            let sweep = ctx
                .observed
                .begin_sweep("bench-forge", &["vogt".to_string()], at)
                .unwrap();
            let findings: Vec<PendingObservation> = (0..50)
                .map(|n| PendingObservation {
                    kind: "forge.issue".into(),
                    subject_key: format!("vogt/closed/{n}"),
                    payload: json!({"state": "closed", "title": format!("closed {n}"), "labels": []}),
                    content_digest: format!("closed-{n}"),
                    project_id: None,
                    source_url: None,
                    promoted: false,
                })
                .collect();
            ctx.observed.append(&sweep.id, &findings, at).unwrap();
            ctx.observed
                .finish_sweep(&sweep.id, SweepOutcome::Ok, &[], at, None)
                .unwrap();
            ctx.observed.rebuild_latest().unwrap();
        });
        let counted = crate::with_ctx!(&built, |ctx| {
            crate::application::views::backlog_total_considered(ctx)
        })
        .unwrap();
        let full = crate::application::views::backlog_op(&built, json!({"limit": 1})).unwrap();
        assert_eq!(counted, full["total_considered"].as_i64().unwrap());
        // No declared rows here: the first 1000 open issues after the closed run.
        assert_eq!(counted, 1000);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
