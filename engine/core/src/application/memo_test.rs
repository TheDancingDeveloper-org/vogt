//! The shell's badge counts are shared while the stores stand still (WI-1224).
//! Ports `tests/test_place_metrics_cache.py`.
//!
//! These pin what the shared answer is keyed on (every way the stores can move
//! must invalidate it) and that a burst of callers costs one computation.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::context::{build_context, Built};
use super::memo::{store_state, StoreMemo};
use super::place::{place_metrics_op, COUNTS_COMPUTED};
use super::services::inbox::STORED_COMPUTED;
use super::services::work::create_op;
use crate::adapters::engine::EngineClient;
use crate::core::{ActorKind, Moment, Principal};
use crate::storage::interface::{DeclaredStore, ObservedStore};

const WHY: &str = "badge cache test";

fn data_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "vogt-memo-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut clock = None;
    let mut ids = None;
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    dir
}

/// A new context over the same stores, as the HTTP adapter builds per request:
/// the case a cache keyed on the store objects never hit.
fn fresh(dir: &Path) -> Built {
    fresh_with(dir, None)
}

fn fresh_with(dir: &Path, engine: Option<EngineClient>) -> Built {
    build_context(
        crate::config::VogtConfig {
            data_dir: dir.to_path_buf(),
            ..crate::config::VogtConfig::default()
        },
        Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
        None,
        None,
        None,
        engine,
        None,
        None,
    )
    .unwrap()
}

fn metrics(built: &Built) -> Value {
    place_metrics_op(built, Value::Null).unwrap()
}

fn counted() -> usize {
    COUNTS_COMPUTED.with(|n| n.get())
}

fn create_bug(built: &Built, title: &str) {
    create_op(built, json!({"kind": "bug", "title": title, "reason": WHY})).unwrap();
}

#[test]
fn the_counts_are_shared_across_per_request_contexts() {
    let dir = data_dir("shared");
    let first = metrics(&fresh(&dir));
    let before = counted();
    let second = metrics(&fresh(&dir));
    assert_eq!(
        counted(),
        before,
        "the second context was answered from memory"
    );
    assert_eq!(
        first["backlog_total_considered"],
        second["backlog_total_considered"]
    );
}

#[test]
fn a_declared_write_invalidates_the_counts() {
    let dir = data_dir("declared");
    let before = metrics(&fresh(&dir));
    let start = counted();
    create_bug(&fresh(&dir), "a new bug");
    let after = metrics(&fresh(&dir));
    assert_eq!(counted(), start + 1);
    assert!(after["revision"].as_i64() > before["revision"].as_i64());
    assert_eq!(
        after["work_total"],
        before["work_total"].as_i64().unwrap() + 1
    );
    assert_eq!(
        after["backlog_total_considered"],
        before["backlog_total_considered"].as_i64().unwrap() + 1
    );
}

#[test]
fn a_rebuilt_observed_projection_invalidates_the_counts() {
    // A sweep writes no declared revision; the projection generation is what
    // tells the memo the observed half moved.
    let dir = data_dir("observed");
    let before = metrics(&fresh(&dir));
    let built = fresh(&dir);
    let generation = crate::with_ctx!(&built, |ctx| ctx.observed.latest_generation().unwrap());
    let start = counted();
    crate::with_ctx!(&built, |ctx| ctx.observed.rebuild_latest().unwrap());
    let moved = crate::with_ctx!(&built, |ctx| ctx.observed.latest_generation().unwrap());
    assert_eq!(moved, generation + 1);
    let after = metrics(&fresh(&dir));
    assert_eq!(
        after["revision"], before["revision"],
        "nothing declared moved"
    );
    assert_eq!(
        counted(),
        start + 1,
        "the observed projection moved, so it recomputed"
    );
}

#[test]
fn an_observed_side_event_invalidates_the_counts() {
    let dir = data_dir("event");
    metrics(&fresh(&dir));
    let start = counted();
    let built = fresh(&dir);
    crate::with_ctx!(&built, |ctx| {
        ctx.declared
            .publish_event(
                "sweep.completed",
                "sweep",
                "swp_test",
                &json!({}),
                Moment::from_unix(1_700_000_000, 0),
            )
            .unwrap()
    });
    metrics(&fresh(&dir));
    assert_eq!(counted(), start + 1);
}

#[test]
fn two_data_directories_never_share_an_answer() {
    let here = data_dir("here");
    let there = data_dir("there");
    create_bug(&fresh(&here), "only here");
    assert_eq!(metrics(&fresh(&here))["work_total"], 1);
    assert_eq!(metrics(&fresh(&there))["work_total"], 0);
}

#[test]
fn the_key_moves_with_each_kind_of_change() {
    let dir = data_dir("key");
    let built = fresh(&dir);
    let start = crate::with_ctx!(&built, |ctx| store_state(ctx).unwrap());
    create_bug(&built, "x");
    let declared = crate::with_ctx!(&built, |ctx| store_state(ctx).unwrap());
    assert!(declared.revision > start.revision);
    crate::with_ctx!(&built, |ctx| ctx.observed.rebuild_latest().unwrap());
    let observed = crate::with_ctx!(&built, |ctx| store_state(ctx).unwrap());
    assert_eq!(observed.latest_generation, declared.latest_generation + 1);
    assert_eq!(observed.revision, declared.revision);
}

#[test]
fn a_burst_of_callers_costs_one_computation() {
    let dir = data_dir("burst");
    let memo: Arc<StoreMemo<i64>> = Arc::new(StoreMemo::new());
    let computed = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(Barrier::new(9));
    let callers: Vec<_> = (0..8)
        .map(|_| {
            let (dir, memo, computed, gate) = (
                dir.clone(),
                Arc::clone(&memo),
                Arc::clone(&computed),
                Arc::clone(&gate),
            );
            std::thread::spawn(move || {
                let built = fresh(&dir);
                gate.wait();
                crate::with_ctx!(&built, |ctx| memo
                    .get(
                        ctx,
                        || {
                            computed.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(200));
                            Ok(42)
                        },
                        |_| true,
                    )
                    .unwrap())
            })
        })
        .collect();
    gate.wait();
    let answers: Vec<i64> = callers.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(answers, vec![42; 8]);
    assert_eq!(computed.load(Ordering::SeqCst), 1);
}

#[test]
fn waiters_take_only_a_computation_that_started_after_they_arrived() {
    // The follower arrives while the leader computes and the store then moves:
    // it must not take the leader's answer, which read the stores before it
    // arrived, so it computes again against the moved store.
    let dir = data_dir("waiters");
    let memo: Arc<StoreMemo<usize>> = Arc::new(StoreMemo::new());
    let computed = Arc::new(Mutex::new(0usize));
    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let compute = |computed: Arc<Mutex<usize>>,
                   started: Option<Arc<Barrier>>,
                   release: Option<Arc<Barrier>>| {
        move || {
            let n = {
                let mut n = computed.lock().unwrap();
                *n += 1;
                *n
            };
            if let (Some(started), Some(release)) = (started, release) {
                started.wait();
                release.wait();
            }
            Ok(n)
        }
    };
    let leader = {
        let (dir, memo, computed, started, release) = (
            dir.clone(),
            Arc::clone(&memo),
            Arc::clone(&computed),
            Arc::clone(&started),
            Arc::clone(&release),
        );
        std::thread::spawn(move || {
            let built = fresh(&dir);
            let f = compute(computed, Some(started), Some(release));
            crate::with_ctx!(&built, |ctx| memo.get(ctx, f, |_| true).unwrap())
        })
    };
    started.wait();
    let follower = {
        let (dir, memo, computed) = (dir.clone(), Arc::clone(&memo), Arc::clone(&computed));
        std::thread::spawn(move || {
            let built = fresh(&dir);
            let f = compute(computed, None, None);
            crate::with_ctx!(&built, |ctx| memo.get(ctx, f, |_| true).unwrap())
        })
    };
    std::thread::sleep(Duration::from_millis(100));
    create_bug(&fresh(&dir), "moves");
    release.wait();
    assert_eq!(leader.join().unwrap(), 1);
    assert_eq!(follower.join().unwrap(), 2);

    // A caller arriving now, with nothing moved since, is answered from memory.
    let built = fresh(&dir);
    let again = crate::with_ctx!(&built, |ctx| memo
        .get(ctx, compute(Arc::clone(&computed), None, None), |_| true)
        .unwrap());
    assert_eq!(again, 2);
    assert_eq!(*computed.lock().unwrap(), 2);
}

#[test]
fn an_answer_that_is_not_kept_is_asked_for_again() {
    let dir = data_dir("keep");
    let memo: StoreMemo<Option<i64>> = StoreMemo::new();
    let built = fresh(&dir);
    let missing = crate::with_ctx!(&built, |ctx| memo
        .get(ctx, || Ok(None), |answer| answer.is_some())
        .unwrap());
    assert_eq!(missing, None);
    let healed = crate::with_ctx!(&built, |ctx| memo
        .get(ctx, || Ok(Some(7)), |answer| answer.is_some())
        .unwrap());
    assert_eq!(healed, Some(7));
}

#[test]
fn memo_forgets_the_least_recent_stores() {
    let memo: StoreMemo<&'static str> = StoreMemo::with_limit(2);
    let dirs: Vec<PathBuf> = (0..3).map(|i| data_dir(&format!("lru{i}"))).collect();
    for dir in &dirs {
        let built = fresh(dir);
        crate::with_ctx!(&built, |ctx| memo
            .get(ctx, || Ok("answer"), |_| true)
            .unwrap());
    }
    let built = fresh(&dirs[0]);
    let again = crate::with_ctx!(&built, |ctx| memo
        .get(ctx, || Ok("again"), |_| true)
        .unwrap());
    assert_eq!(again, "again");
    let built = fresh(&dirs[2]);
    let kept = crate::with_ctx!(&built, |ctx| memo
        .get(ctx, || Ok("again"), |_| true)
        .unwrap());
    assert_eq!(kept, "answer");
}

// -- the Inbox half ------------------------------------------------------------

#[test]
fn the_live_inbox_half_is_read_on_every_badge() {
    // A session that starts waiting for a person moves no store; the badge still
    // counts it on the next read, because only the stored half is shared.
    let dir = data_dir("live");
    let rows: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let listed = Arc::new(AtomicUsize::new(0));
    let engine = {
        let (rows, listed) = (Arc::clone(&rows), Arc::clone(&listed));
        EngineClient::new(
            "http://engine",
            None,
            Some(Arc::new(move |path: &str, _, _, method: &str| {
                if method == "GET" && path.contains("/api/sessions") {
                    listed.fetch_add(1, Ordering::SeqCst);
                    let body = Value::Array(rows.lock().unwrap().clone());
                    return (200, body.to_string().into_bytes());
                }
                (404, b"{}".to_vec())
            })),
        )
    };
    let built = fresh_with(&dir, Some(engine));
    let quiet = metrics(&built);
    let built_before = STORED_COMPUTED.with(|n| n.get());
    rows.lock().unwrap().push(json!({
        "id": "00000000-0000-4000-8000-000000000001",
        "name": "worker",
        "activity": "waiting-for-input",
        "alive": true,
        "cwd": "/srv",
        "created_at": "2026-10-05T00:00:00Z",
        "last_output_at": "2026-10-05T00:00:00Z",
        "activity_changed_at": "2026-10-05T00:00:00Z",
    }));
    let asking = metrics(&built);
    assert_eq!(
        asking["inbox_active"],
        quiet["inbox_active"].as_i64().unwrap() + 1
    );
    assert_eq!(
        STORED_COMPUTED.with(|n| n.get()),
        built_before,
        "the stored half was not rebuilt"
    );
    assert_eq!(listed.load(Ordering::SeqCst), 2);
}

#[test]
fn forgetting_one_data_directory_keeps_the_others() {
    // A restore forgets only the stores it replaces: a restore of another
    // instance in the same process must not wipe this one's answers.
    let memo: StoreMemo<&'static str> = StoreMemo::new();
    let (kept, replaced) = (data_dir("kept"), data_dir("replaced"));
    for dir in [&kept, &replaced] {
        let built = fresh(dir);
        crate::with_ctx!(&built, |ctx| memo
            .get(ctx, || Ok("answer"), |_| true)
            .unwrap());
    }
    let resolved = crate::with_ctx!(&fresh(&replaced), |ctx| ctx
        .declared
        .path()
        .parent()
        .unwrap()
        .to_path_buf());
    memo.forget(&resolved);
    let built = fresh(&kept);
    let answer = crate::with_ctx!(&built, |ctx| memo
        .get(ctx, || Ok("again"), |_| true)
        .unwrap());
    assert_eq!(answer, "answer");
    let built = fresh(&replaced);
    let answer = crate::with_ctx!(&built, |ctx| memo
        .get(ctx, || Ok("again"), |_| true)
        .unwrap());
    assert_eq!(answer, "again");
}
