//! The in-process collector schedule. Ports `src/vogt/adapters/http/scheduler.py`.
//!
//! `sweep` is triggerable on demand from all three transports. This is the
//! other half: while `serve` is running, collectors also run on their own, so
//! an instance nobody has poked at is not an instance with no evidence.
//!
//! The same three properties as Python's:
//!
//! **One sweep at a time.** The loop sleeps *after* the work finishes, so the
//! interval is a gap between sweeps rather than a fixed drumbeat, and a slow
//! estate degrades to "continuously sweeping" rather than to overlapping
//! writers.
//!
//! **A failed sweep never stops the schedule.** A tick that returns an error
//! (or panics) is counted, logged as a warning and the loop carries on: one bad
//! collector must not turn into permanent silence.
//!
//! **It runs as the machine, not as a person.** `serve` hands the schedule a
//! service principal (`service:vogt-scheduler`), never whoever started the
//! process.
//!
//! Each tick runs the collector sweep with reason [`SCHEDULED_REASON`] and then,
//! whether or not the sweep succeeded, `settle_budget_stops` (WI-1833).
//! [`sweep_then_settle`] catches a panicking sweep and still settles, so one
//! bad collector does not skip the hand-off. The loop counts that failure and
//! carries on. The watch pass (`evaluate_watches`, WI-2036) is not ported:
//! `watch_interval_seconds` stays unread, and no watch loop is started.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::application::collect::sweep_built;
use crate::application::context::build_context;
use crate::application::services::budget_handoff;
use crate::config::VogtConfig;
use crate::core::{ActorKind, Principal};
use crate::errors::VogtError;
use crate::with_ctx;

/// What a scheduled sweep records as its reason. Every write carries one, and
/// "the schedule ran" is the honest answer: there is no person to name.
pub const SCHEDULED_REASON: &str = "scheduled sweep";

/// The log target, Python's `logging.getLogger("vogt.scheduler")`.
const TARGET: &str = "vogt.scheduler";

/// One tick's work. Runs on a blocking thread, off the async runtime, so a
/// sweep's file, network and SQLite I/O cannot stall the listener.
pub type Tick = Arc<dyn Fn() -> Result<(), VogtError> + Send + Sync>;

/// The principal a scheduled sweep runs as. A scheduled sweep has no human
/// behind it; attributing it to whoever started the process would put their
/// name on evidence they never asked for.
pub fn scheduler_principal() -> Principal {
    Principal::new(
        "service:vogt-scheduler",
        ActorKind::Agent,
        "collector schedule",
    )
    .expect("a fixed, non-empty identity")
}

/// Sweep, then settle budget stops. A panicking sweep is caught here and
/// turned into the same failure a raised error is, and the settlement still
/// runs. An error from the settlement replaces the sweep's, the way an
/// exception raised in a `finally` block does.
pub fn sweep_then_settle(
    sweep: impl FnOnce() -> Result<(), VogtError>,
    settle: impl FnOnce() -> Result<(), VogtError>,
) -> Result<(), VogtError> {
    // Closures that capture the engine transport are not UnwindSafe (the
    // transport is interior-mutable). Assert it here: a sweep panic is caught
    // and turned into an error, and the settlement still runs.
    let swept = std::panic::catch_unwind(std::panic::AssertUnwindSafe(sweep));
    let settled = settle();
    let swept = match swept {
        Ok(result) => result,
        Err(_) => Err(VogtError::InvalidRequest("scheduled sweep panicked".into())),
    };
    settled?;
    swept
}

/// The tick `serve` runs: a fresh context per tick, as the service principal,
/// over the same config the listener loaded. The clock and ids are the hook
/// pair `build_context` already resolves, not a second clock the routes share.
/// The sweep, then the budget settlement, as Python's `try: sweep finally:
/// settle_budget_stops`, via [`sweep_then_settle`].
pub fn serve_tick(config: VogtConfig) -> Tick {
    Arc::new(move || {
        let built = build_context(
            config.clone(),
            Some(scheduler_principal()),
            None,
            None,
            None,
            None,
            None,
            None,
        )?;
        sweep_then_settle(
            || sweep_built(&built, &json!({ "reason": SCHEDULED_REASON })).map(drop),
            || {
                with_ctx!(&built, |ctx| {
                    budget_handoff::settle_budget_stops(ctx).map(drop)
                })
            },
        )
    })
}

/// Runs sweeps in the background for as long as the server is up.
pub struct CollectorSchedule {
    tick: Tick,
    interval_seconds: i64,
    stop: Option<tokio::sync::watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
    counts: Arc<Counts>,
}

/// Counted so tests can assert the loop ran without waiting on a wall clock.
#[derive(Default)]
struct Counts {
    completed: AtomicU64,
    failed: AtomicU64,
}

impl CollectorSchedule {
    /// `interval_seconds` of zero (or less) is a disabled schedule.
    pub fn new(tick: Tick, interval_seconds: i64) -> Self {
        Self {
            tick,
            interval_seconds,
            stop: None,
            task: None,
            counts: Arc::new(Counts::default()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.interval_seconds > 0
    }

    pub fn completed(&self) -> u64 {
        self.counts.completed.load(Ordering::SeqCst)
    }

    pub fn failed(&self) -> u64 {
        self.counts.failed.load(Ordering::SeqCst)
    }

    /// Whether a background task is running.
    pub fn running(&self) -> bool {
        self.task.is_some()
    }

    /// Start the loop on the current runtime. A disabled schedule starts
    /// nothing. `notify` is the same stop the listener waits on: the first
    /// change ends the gap between sweeps without waiting out the interval.
    pub fn start(&mut self, mut notify: tokio::sync::watch::Receiver<bool>) {
        if !self.enabled() {
            tracing::info!(
                target: TARGET,
                "collector schedule disabled (sweep_interval_seconds=0)"
            );
            return;
        }
        let interval = Duration::from_secs(u64::try_from(self.interval_seconds).unwrap_or(0));
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let tick = Arc::clone(&self.tick);
        let counts = Arc::clone(&self.counts);
        self.stop = Some(sender.clone());
        self.task = Some(tokio::spawn(async move {
            // Either the listener's signal or `stop()` ends the wait. The
            // forwarder is joined (or aborted if the loop ends first) so stop
            // does not leave a detached task.
            let forward = tokio::spawn(async move {
                let _ = notify.changed().await;
                let _ = sender.send(true);
            });
            run_loop(tick, counts, interval, receiver).await;
            forward.abort();
            let _ = forward.await;
        }));
        tracing::info!(
            target: TARGET,
            "collector schedule started; every {}s",
            self.interval_seconds
        );
    }

    /// The loop on a sub-second interval, for the tests that need it to tick
    /// more than once. Python sets `_interval` to a float for the same reason.
    #[cfg(test)]
    pub(crate) fn start_every_for_test(&mut self, interval: Duration) {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let tick = Arc::clone(&self.tick);
        let counts = Arc::clone(&self.counts);
        self.stop = Some(sender);
        self.task = Some(tokio::spawn(run_loop(tick, counts, interval, receiver)));
    }

    /// Ask the loop to finish, and wait for the sweep in flight. A sweep is a
    /// writer, and abandoning one mid-write would leave a started sweep with no
    /// outcome. The wait between sweeps is on the stop signal, not the clock,
    /// so shutdown never waits out the interval.
    pub async fn stop(&mut self) {
        if let Some(sender) = self.stop.take() {
            let _ = sender.send(true);
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    /// One tick, counted. Python's `run_once`.
    pub async fn run_once(&self) {
        run_once(&self.tick, &self.counts).await;
    }
}

async fn run_loop(
    tick: Tick,
    counts: Arc<Counts>,
    interval: Duration,
    mut stopping: tokio::sync::watch::Receiver<bool>,
) {
    while !*stopping.borrow() {
        run_once(&tick, &counts).await;
        // Sleeping on the stop signal rather than on the clock, so shutdown
        // does not wait out a fifteen-minute interval. A dropped sender is a
        // stop too.
        let _ = tokio::time::timeout(interval, stopping.changed()).await;
    }
}

async fn run_once(tick: &Tick, counts: &Counts) {
    let tick = Arc::clone(tick);
    match tokio::task::spawn_blocking(move || tick()).await {
        Ok(Ok(())) => {
            counts.completed.fetch_add(1, Ordering::SeqCst);
        }
        Ok(Err(error)) => {
            counts.failed.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(target: TARGET, "scheduled sweep failed: {error}");
        }
        Err(join) => {
            counts.failed.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(target: TARGET, "scheduled sweep failed: {join}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    fn counting(gate: Arc<Mutex<()>>) -> (Tick, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = Arc::clone(&hits);
        let tick: Tick = Arc::new(move || {
            let _held = gate.lock().unwrap();
            hits2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        (tick, hits)
    }

    #[tokio::test]
    async fn a_disabled_interval_starts_nothing() {
        let (tick, _) = counting(Arc::new(Mutex::new(())));
        let mut schedule = CollectorSchedule::new(tick, 0);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        schedule.start(rx);
        assert!(!schedule.running());
        assert_eq!(schedule.completed(), 0);
    }

    #[tokio::test]
    async fn a_failed_tick_is_counted_and_the_loop_continues() {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = Arc::clone(&hits);
        let tick: Tick = Arc::new(move || {
            let n = hits2.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(VogtError::InvalidRequest("boom".into()))
            } else {
                Ok(())
            }
        });
        let mut schedule = CollectorSchedule::new(tick, 900);
        schedule.start_every_for_test(Duration::from_millis(20));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if schedule.completed() >= 1 && schedule.failed() >= 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the loop kept going after a failure");
        schedule.stop().await;
        assert!(hits.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn a_panic_is_counted_as_a_failure() {
        let tick: Tick = Arc::new(|| panic!("collector"));
        let mut schedule = CollectorSchedule::new(tick, 900);
        schedule.start_every_for_test(Duration::from_millis(5));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if schedule.failed() >= 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a panic was counted");
        schedule.stop().await;
    }

    #[tokio::test]
    async fn sweeps_do_not_overlap() {
        let current = Arc::new(AtomicUsize::new(0));
        let max = Arc::new(AtomicUsize::new(0));
        let current2 = Arc::clone(&current);
        let max2 = Arc::clone(&max);
        let tick: Tick = Arc::new(move || {
            let now = current2.fetch_add(1, Ordering::SeqCst) + 1;
            max2.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            current2.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        });
        let mut schedule = CollectorSchedule::new(tick, 900);
        schedule.start_every_for_test(Duration::from_millis(1));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if schedule.completed() >= 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("two sweeps finished");
        schedule.stop().await;
        assert_eq!(max.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stop_does_not_return_until_the_in_flight_tick_finishes() {
        let entered = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(AtomicUsize::new(0));
        let entered2 = Arc::clone(&entered);
        let release2 = Arc::clone(&release);
        let tick: Tick = Arc::new(move || {
            entered2.store(1, Ordering::SeqCst);
            while release2.load(Ordering::SeqCst) == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        });
        let mut schedule = CollectorSchedule::new(tick, 900);
        schedule.start_every_for_test(Duration::from_secs(30));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if entered.load(Ordering::SeqCst) == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("tick entered");
        let stop = tokio::spawn(async move {
            schedule.stop().await;
            schedule.completed()
        });
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(!stop.is_finished(), "stop returned while the tick was held");
        release.store(1, Ordering::SeqCst);
        let completed = tokio::time::timeout(Duration::from_secs(2), stop)
            .await
            .expect("stop finished")
            .expect("stop task");
        assert_eq!(completed, 1);
    }

    #[tokio::test]
    async fn shutdown_does_not_wait_out_the_interval() {
        let (tick, _) = counting(Arc::new(Mutex::new(())));
        let mut schedule = CollectorSchedule::new(tick, 900);
        schedule.start_every_for_test(Duration::from_secs(900));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if schedule.completed() >= 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the first sweep finished");
        let started = std::time::Instant::now();
        schedule.stop().await;
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_tick_settles_after_the_sweep_even_when_the_sweep_fails() {
        let order = std::sync::Mutex::new(Vec::new());
        let swept = sweep_then_settle(
            || {
                order.lock().unwrap().push("sweep");
                Err(VogtError::InvalidRequest("sweep".into()))
            },
            || {
                order.lock().unwrap().push("settle");
                Ok(())
            },
        );
        assert!(swept.is_err());
        assert_eq!(order.lock().unwrap().as_slice(), ["sweep", "settle"]);
    }

    #[test]
    fn a_failing_settlement_fails_the_tick_as_a_finally_would() {
        let result = sweep_then_settle(
            || Ok(()),
            || Err(VogtError::InvalidRequest("settle".into())),
        );
        assert!(matches!(result, Err(VogtError::InvalidRequest(message)) if message == "settle"));
    }

    #[test]
    fn the_scheduled_principal_is_the_machine() {
        let principal = scheduler_principal();
        assert!(principal.identity_ref.contains("service:"));
        assert_eq!(principal.kind, ActorKind::Agent);
        assert_eq!(SCHEDULED_REASON, "scheduled sweep");
    }

    #[test]
    fn the_default_interval_is_fifteen_minutes() {
        let config = VogtConfig::default();
        assert_eq!(config.sweep_interval_seconds, 900);
    }

    #[test]
    fn a_panicking_sweep_still_settles() {
        let order = std::sync::Mutex::new(Vec::new());
        let swept = sweep_then_settle(
            || {
                order.lock().unwrap().push("sweep");
                panic!("collector");
            },
            || {
                order.lock().unwrap().push("settle");
                Ok(())
            },
        );
        assert!(swept.is_err());
        assert_eq!(order.lock().unwrap().as_slice(), ["sweep", "settle"]);
    }
}
