//! Ports `tests/test_scheduler.py`: the in-process collector schedule, and the
//! budget settlement each tick runs after the sweep (WI-1833).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::scheduler::{
    scheduled_pass, scheduler_principal, serve_tick, sweep_then_settle, CollectorSchedule, Tick,
    SCHEDULED_REASON,
};
use crate::application::context::{build_context, Built};
use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore};

fn counting(calls: &Arc<AtomicU64>) -> Tick {
    let calls = Arc::clone(calls);
    Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
}

fn scratch(name: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "vogt-scheduler-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn instance(name: &str) -> Built {
    let config = crate::config::VogtConfig {
        data_dir: scratch(name),
        ..crate::config::VogtConfig::default()
    };
    let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
    let built = build_context(
        config,
        Some(principal.clone()),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
        Some(SequentialIds::new(None).unwrap()),
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    ctx.declared.migrate().unwrap();
    ctx.declared.bootstrap(&principal).unwrap();
    ctx.observed.migrate().unwrap();
    built
}

// -- it runs, and keeps running -------------------------------------------

#[test]
fn the_schedule_sweeps_without_anyone_asking() {
    let built = instance("sweeps");
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    scheduled_pass(ctx).unwrap();
    assert!(
        !ctx.observed.coverage().unwrap().is_empty(),
        "a scheduled sweep must record coverage like any other"
    );
}

#[test]
fn the_serve_tick_sweeps_as_the_machine_over_the_shared_handles() {
    let built = instance("serve-tick");
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    let tick = serve_tick(
        ctx.config.clone(),
        Arc::clone(&ctx.clock),
        Arc::clone(&ctx.id_factory),
    );
    tick().unwrap();
    assert!(!ctx.observed.coverage().unwrap().is_empty());
}

#[test]
fn a_tick_settles_budget_stops_after_the_sweep_even_when_the_sweep_fails() {
    let order = Mutex::new(Vec::new());
    let result = sweep_then_settle(
        || {
            order.lock().unwrap().push("sweep");
            Err(VogtError::InvalidRequest("a collector fell over".into()))
        },
        || {
            order.lock().unwrap().push("settle");
            Ok(())
        },
    );
    assert_eq!(*order.lock().unwrap(), vec!["sweep", "settle"]);
    assert!(result.is_err(), "the sweep's failure is still the tick's");

    let ok = sweep_then_settle(|| Ok(()), || Ok(()));
    assert!(ok.is_ok());
}

#[test]
fn a_failing_settlement_fails_the_tick_as_a_finally_would() {
    let result = sweep_then_settle(
        || Err(VogtError::InvalidRequest("sweep".into())),
        || Err(VogtError::InvalidRequest("settle".into())),
    );
    assert!(result.unwrap_err().to_string().contains("settle"));
    assert!(sweep_then_settle(|| Ok(()), || Err(VogtError::InvalidRequest("x".into()))).is_err());
}

#[tokio::test]
async fn a_failing_sweep_does_not_stop_the_schedule() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let tick: Tick = Arc::new(move || {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(VogtError::InvalidRequest("a collector fell over".into()));
        }
        Ok(())
    });
    let schedule = CollectorSchedule::new(tick, 3600);
    schedule.run_once().await;
    schedule.run_once().await;
    assert_eq!(schedule.failed(), 1);
    assert_eq!(schedule.completed(), 1, "the loop survived and swept again");
}

#[tokio::test]
async fn a_panicking_tick_is_counted_and_the_loop_keeps_ticking() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let tick: Tick = Arc::new(move || {
        if seen.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("a collector panicked");
        }
        Ok(())
    });
    let mut schedule = CollectorSchedule::new(tick, 1);
    schedule.start_every_for_test(Duration::from_millis(10));
    tokio::time::sleep(Duration::from_millis(200)).await;
    schedule.stop().await;
    assert_eq!(schedule.failed(), 1);
    assert!(
        schedule.completed() >= 1,
        "the loop ticked again after the panic"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweeps_do_not_overlap() {
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (r, p) = (Arc::clone(&running), Arc::clone(&peak));
    let tick: Tick = Arc::new(move || {
        let now = r.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(50));
        r.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    });
    let mut schedule = CollectorSchedule::new(tick, 1);
    schedule.start_every_for_test(Duration::from_millis(10));
    tokio::time::sleep(Duration::from_millis(250)).await;
    schedule.stop().await;
    assert!(schedule.completed() >= 2, "the loop ticked more than once");
    assert_eq!(peak.load(Ordering::SeqCst), 1, "sweeps must not overlap");
}

#[tokio::test]
async fn stopping_waits_rather_than_leaving_a_sweep_half_done() {
    let finished = Arc::new(AtomicU64::new(0));
    let done = Arc::clone(&finished);
    let tick: Tick = Arc::new(move || {
        std::thread::sleep(Duration::from_millis(100));
        done.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    let mut schedule = CollectorSchedule::new(tick, 3600);
    schedule.start();
    tokio::time::sleep(Duration::from_millis(20)).await;
    schedule.stop().await;
    assert!(!schedule.running(), "the task was awaited, not abandoned");
    assert_eq!(
        finished.load(Ordering::SeqCst),
        1,
        "the sweep in flight finished before stop returned"
    );
}

#[tokio::test]
async fn shutdown_does_not_wait_out_the_interval() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut schedule = CollectorSchedule::new(counting(&calls), 900);
    schedule.start();
    assert!(schedule.running());
    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::time::timeout(Duration::from_secs(5), schedule.stop())
        .await
        .expect("stop returned without waiting out the interval");
    assert!(!schedule.running());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "it swept once at start");
}

// -- disabled ---------------------------------------------------------------

#[tokio::test]
async fn a_zero_interval_starts_nothing() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut schedule = CollectorSchedule::new(counting(&calls), 0);
    assert!(!schedule.enabled());
    schedule.start();
    assert!(!schedule.running(), "nothing runs when disabled");
    tokio::time::sleep(Duration::from_millis(30)).await;
    schedule.stop().await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn the_default_interval_keeps_looking() {
    let config = crate::config::VogtConfig::default();
    assert_eq!(config.sweep_interval_seconds, 900);
    let schedule = CollectorSchedule::new(Arc::new(|| Ok(())), config.sweep_interval_seconds);
    assert!(schedule.enabled(), "the default is to keep looking");
}

// -- provenance -------------------------------------------------------------

#[test]
fn a_scheduled_sweep_runs_as_the_machine() {
    let principal = scheduler_principal();
    assert_eq!(principal.kind, ActorKind::Agent);
    assert!(principal.identity_ref.contains("service:"));
}

#[test]
fn the_scheduled_reason_says_what_it_was() {
    assert!(!SCHEDULED_REASON.trim().is_empty());
    assert!(SCHEDULED_REASON.to_lowercase().contains("schedul"));
}
