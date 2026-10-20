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
//!
//! Only `vogt-core serve` builds one. The CLI one-shots and a router built on
//! its own get no background writer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::application::context::{context_on, AppContext};
use crate::config::VogtConfig;
use crate::core::{ActorKind, Clock, IdFactory, Principal};
use crate::errors::VogtError;

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

/// The sweep, then the budget settlement, as Python's `try: sweep finally:
/// settle_budget_stops`. The settlement runs whether or not the sweep failed;
/// an error from the settlement replaces the sweep's, the way an exception
/// raised in a `finally` block does.
pub fn sweep_then_settle(
    sweep: impl FnOnce() -> Result<(), VogtError>,
    settle: impl FnOnce() -> Result<(), VogtError>,
) -> Result<(), VogtError> {
    let swept = sweep();
    settle()?;
    swept
}

/// One scheduled pass over a context: Python's `CollectorSchedule._sweep`.
pub fn scheduled_pass<C, I>(ctx: &AppContext<C, I>) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    sweep_then_settle(
        || {
            crate::application::collect::sweep(ctx, &json!({ "reason": SCHEDULED_REASON }))
                .map(drop)
        },
        || crate::application::services::budget_handoff::settle_budget_stops(ctx).map(drop),
    )
}

/// The tick `serve` runs: a fresh context per tick, as the service principal,
/// over the clock and id factory the routes already share.
pub fn serve_tick<C, I>(config: VogtConfig, clock: Arc<Mutex<C>>, ids: Arc<Mutex<I>>) -> Tick
where
    C: Clock + Send + 'static,
    I: IdFactory + Send + 'static,
{
    Arc::new(move || {
        let ctx = context_on(
            config.clone(),
            Some(scheduler_principal()),
            Arc::clone(&clock),
            Arc::clone(&ids),
            None,
        );
        scheduled_pass(&ctx)
    })
}

/// Runs sweeps in the background for as long as the server is up.
pub struct CollectorSchedule {
    tick: Tick,
    interval_seconds: i64,
    stopping: Option<watch::Sender<bool>>,
    task: Option<JoinHandle<()>>,
    counts: Arc<Counts>,
}

/// Counted so tests can assert the loop ran without waiting on a wall clock.
#[derive(Default)]
struct Counts {
    completed: AtomicU64,
    failed: AtomicU64,
}

impl CollectorSchedule {
    /// `interval_seconds` of zero (or less) is a disabled schedule: `serve`
    /// passes zero for `--no-schedule`, as Python does.
    pub fn new(tick: Tick, interval_seconds: i64) -> Self {
        Self {
            tick,
            interval_seconds,
            stopping: None,
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
    /// nothing.
    pub fn start(&mut self) {
        if !self.enabled() {
            tracing::info!(
                target: TARGET,
                "collector schedule disabled (sweep_interval_seconds=0)"
            );
            return;
        }
        let interval = Duration::from_secs(u64::try_from(self.interval_seconds).unwrap_or(0));
        self.start_every(interval);
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
        self.start_every(interval);
    }

    fn start_every(&mut self, interval: Duration) {
        let (sender, receiver) = watch::channel(false);
        let tick = Arc::clone(&self.tick);
        let counts = Arc::clone(&self.counts);
        self.stopping = Some(sender);
        self.task = Some(tokio::spawn(run_loop(tick, counts, interval, receiver)));
    }

    /// Ask the loop to finish, and wait for the sweep in flight. A sweep is a
    /// writer, and abandoning one mid-write would leave a started sweep with no
    /// outcome. The wait between sweeps is on the stop signal, not the clock,
    /// so shutdown never waits out the interval.
    pub async fn stop(&mut self) {
        if let Some(sender) = self.stopping.take() {
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
    mut stopping: watch::Receiver<bool>,
) {
    while !*stopping.borrow() {
        run_once(&tick, &counts).await;
        // Sleeping on the stop signal rather than on the clock, so shutdown
        // does not wait out a fifteen-minute interval. A dropped sender is a
        // stop too.
        let _ = tokio::time::timeout(interval, stopping.wait_for(|stop| *stop)).await;
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
            // Logged, not returned: the sweep's own failure is already
            // recorded as evidence.
            tracing::warn!(target: TARGET, "scheduled sweep failed: {error}");
        }
        Err(join) => {
            // A panic is Rust's uncaught exception. It must not end the
            // schedule either.
            counts.failed.fetch_add(1, Ordering::SeqCst);
            tracing::warn!(target: TARGET, "scheduled sweep failed: {join}");
        }
    }
}
