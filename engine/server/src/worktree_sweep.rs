//! Nightly sweep for orphaned worktree build output (WI-1773).
//!
//! Cleaning on stop and removal (WI-1750, WI-1304) only reaches a worktree
//! whose session stops after that code shipped, so the checkouts left behind
//! before it keep their `target/` and `node_modules` for good. Once a day the
//! sweep walks the workspace for linked worktrees and runs the same
//! `worktree_clean` pass on each one that no live or hibernated session works
//! in. A tree a session uses is never touched, and nothing is removed that the
//! cleanup's own fail-closed checks would refuse (a tracked file, a symlink, a
//! main checkout, a directory with no `Cargo.toml` beside its `target`).
//!
//! The schedule survives restarts. The time of the last sweep is kept in
//! `<state_dir>/worktree-sweep-last` (unix seconds). After boot the first
//! sweep runs once a short settle delay has passed (`ENGINE_WORKTREE_SWEEP_
//! DELAY_SECONDS`, default 10 minutes, so it does not compete with startup and
//! adoption of pod sessions) when the last sweep is missing or at least an
//! interval old (`ENGINE_WORKTREE_SWEEP_INTERVAL_SECONDS`, default 24 hours);
//! a recent one waits out the rest of the interval. Then it runs every
//! interval. An engine redeployed more than once a day therefore still sweeps.
//!
//! `ENGINE_WORKTREE_SWEEP` picks the mode. `dry-run` (the default) walks and
//! sizes everything and deletes nothing, so the first deploy only reports what
//! would be reclaimed. `on` removes it. `off` does not run the sweep. An
//! unreadable value turns it off. Every sweep writes one audit line
//! (`event=worktree.sweep`) and keeps its report for `GET /api/status`.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;

use crate::{app::AppState, worktree_clean};

/// The environment variable that picks the sweep mode.
pub const SWEEP_ENV: &str = "ENGINE_WORKTREE_SWEEP";

/// How often the sweep runs, counted from the last sweep.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Environment variable overriding [`SWEEP_INTERVAL`], in seconds.
pub const INTERVAL_ENV: &str = "ENGINE_WORKTREE_SWEEP_INTERVAL_SECONDS";

/// How long after boot the first due sweep waits, so it does not compete with
/// startup and the adoption of pod sessions.
pub const SETTLE_DELAY: Duration = Duration::from_secs(10 * 60);

/// Environment variable overriding [`SETTLE_DELAY`], in seconds.
pub const DELAY_ENV: &str = "ENGINE_WORKTREE_SWEEP_DELAY_SECONDS";

/// The shortest interval accepted, so a typo cannot make the sweep spin.
const MIN_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// File in the state dir holding the unix time of the last sweep.
const LAST_RUN_FILE: &str = "worktree-sweep-last";

/// When the sweep runs: a settle delay after boot, then every interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub interval: Duration,
    pub settle: Duration,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            interval: SWEEP_INTERVAL,
            settle: SETTLE_DELAY,
        }
    }
}

impl Schedule {
    /// Read the two knobs. A value that is not a whole number of seconds, or
    /// an interval under an hour, is logged and replaced by the default.
    pub fn from_env() -> Self {
        let mut schedule = Self::default();
        if let Some(v) = std::env::var(INTERVAL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            match v.trim().parse::<u64>().map(Duration::from_secs) {
                Ok(d) if d >= MIN_INTERVAL => schedule.interval = d,
                _ => tracing::warn!(
                    "{INTERVAL_ENV} must be at least 3600 seconds, not {v:?}; using the default"
                ),
            }
        }
        if let Some(v) = std::env::var(DELAY_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            match v.trim().parse::<u64>() {
                Ok(secs) => schedule.settle = Duration::from_secs(secs),
                Err(_) => {
                    tracing::warn!(
                        "{DELAY_ENV} must be whole seconds, not {v:?}; using the default"
                    )
                }
            }
        }
        schedule
    }

    /// How long to wait after boot before the first sweep. A missing last run,
    /// one a full interval old, or one stamped in the future (a clock that
    /// went back) is due after the settle delay. A recent one waits out the
    /// rest of its interval, and never less than the settle delay.
    pub fn first_wait(&self, last_run: Option<i64>, now: i64) -> Duration {
        let Some(last) = last_run.filter(|last| *last <= now) else {
            return self.settle;
        };
        let age = Duration::from_secs((now - last) as u64);
        if age >= self.interval {
            self.settle
        } else {
            (self.interval - age).max(self.settle)
        }
    }
}

fn last_run_path(state_dir: &Path) -> PathBuf {
    state_dir.join(LAST_RUN_FILE)
}

/// The unix time the last sweep finished, if one was recorded and readable.
pub fn load_last_run(state_dir: &Path) -> Option<i64> {
    std::fs::read_to_string(last_run_path(state_dir))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Record that a sweep finished at `now` (unix seconds). Written through a
/// temporary file and a rename so a crash leaves the old value, not half of one.
pub fn record_last_run(state_dir: &Path, now: i64) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let path = last_run_path(state_dir);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, now.to_string())?;
    std::fs::rename(&tmp, &path)
}

/// Drive the schedule: wait the first interval worked out from the recorded
/// last run, then run, record and wait a full interval, for ever (or for
/// `max_runs` runs, which tests use). `now` is wall-clock unix seconds and
/// `sleep` the wait, both injected so tests use a fake clock.
pub async fn drive_schedule<S, Fut, R, RFut>(
    schedule: Schedule,
    state_dir: &Path,
    now: impl Fn() -> i64,
    sleep: S,
    mut run: R,
    max_runs: Option<usize>,
) where
    S: Fn(Duration) -> Fut,
    Fut: Future<Output = ()>,
    R: FnMut() -> RFut,
    RFut: Future<Output = ()>,
{
    let mut wait = schedule.first_wait(load_last_run(state_dir), now());
    let mut runs = 0usize;
    loop {
        sleep(wait).await;
        run().await;
        if let Err(e) = record_last_run(state_dir, now()) {
            tracing::warn!(error = %e, "could not record the worktree sweep time");
        }
        runs += 1;
        if max_runs.is_some_and(|max| runs >= max) {
            return;
        }
        wait = schedule.interval;
    }
}

/// How many directories below the workspace root a session checkout may sit.
const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SweepMode {
    /// No sweep runs.
    #[default]
    Off,
    /// The sweep sizes what it would remove and removes nothing.
    DryRun,
    /// The sweep removes regenerable build output.
    On,
}

impl SweepMode {
    /// Parse `ENGINE_WORKTREE_SWEEP`. Unset or blank is `dry-run`, the safe
    /// default that reports without deleting.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(Self::DryRun),
            Some("off" | "0" | "false") => Ok(Self::Off),
            Some("dry-run" | "dry_run") => Ok(Self::DryRun),
            Some("on" | "1" | "true") => Ok(Self::On),
            Some(other) => Err(format!(
                "{SWEEP_ENV} must be off, dry-run or on, not {other:?}"
            )),
        }
    }

    /// The mode from the environment. An invalid value is logged and turns
    /// the sweep off rather than guessing.
    pub fn from_env() -> Self {
        match Self::parse(std::env::var(SWEEP_ENV).ok().as_deref()) {
            Ok(mode) => mode,
            Err(e) => {
                tracing::warn!("{e}; the worktree sweep is off");
                Self::Off
            }
        }
    }
}

/// What one sweep found and did. Sizes are bytes of regenerable output.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SweepReport {
    /// When the sweep ran, RFC 3339.
    pub at: String,
    /// Linked worktrees found under the workspace.
    pub worktrees: usize,
    /// Worktrees skipped because a live or hibernated session works in them.
    pub in_use: usize,
    /// Worktrees that held regenerable output.
    pub trees: usize,
    /// Bytes the sweep found, whether or not it removed them.
    pub bytes_reclaimable: u64,
    /// Bytes actually removed; zero in dry-run.
    pub bytes_reclaimed: u64,
    /// Worktrees the sweep removed output from, for stamping `cleaned_at`.
    #[serde(skip)]
    pub cleaned: Vec<String>,
}

/// Sweep `worktrees`, removing their regenerable output when `apply` is set
/// and only sizing it otherwise. `cwds_in_use` is asked again before each
/// tree, so a session that starts mid-sweep is still respected.
pub fn sweep_worktrees(
    worktrees: &[PathBuf],
    cwds_in_use: &dyn Fn() -> Vec<String>,
    apply: bool,
    at: &str,
) -> SweepReport {
    let mut report = SweepReport {
        at: at.to_string(),
        worktrees: worktrees.len(),
        ..SweepReport::default()
    };
    for worktree in worktrees {
        let in_use = cwds_in_use();
        if worktree_clean::shared_with(worktree, in_use.iter().map(String::as_str)) {
            report.in_use += 1;
            continue;
        }
        let found = if apply {
            worktree_clean::clean(worktree)
        } else {
            worktree_clean::plan_clean(worktree)
        };
        if found.is_empty() {
            continue;
        }
        let bytes: u64 = found.iter().map(|c| c.bytes).sum();
        report.trees += 1;
        report.bytes_reclaimable = report.bytes_reclaimable.saturating_add(bytes);
        if apply {
            report.bytes_reclaimed = report.bytes_reclaimed.saturating_add(bytes);
            report.cleaned.push(worktree.to_string_lossy().into_owned());
        }
    }
    tracing::info!(
        target: "vogt::audit",
        event = "worktree.sweep",
        apply,
        worktrees = report.worktrees,
        in_use = report.in_use,
        trees = report.trees,
        bytes_reclaimable = report.bytes_reclaimable,
        bytes_reclaimed = report.bytes_reclaimed,
        "worktree sweep"
    );
    report
}

/// The sweep's mode and its last report, for `GET /api/status`.
#[derive(Debug, Default)]
pub struct WorktreeSweepState {
    mode: SweepMode,
    last: Mutex<Option<SweepReport>>,
}

#[derive(Debug, Serialize)]
pub struct WorktreeSweepStatus {
    pub mode: SweepMode,
    /// `None` until the first sweep has run.
    pub last: Option<SweepReport>,
}

impl WorktreeSweepState {
    pub fn new(mode: SweepMode) -> Self {
        Self {
            mode,
            last: Mutex::new(None),
        }
    }

    pub fn mode(&self) -> SweepMode {
        self.mode
    }

    pub fn status(&self) -> WorktreeSweepStatus {
        WorktreeSweepStatus {
            mode: self.mode,
            last: self.last.lock().clone(),
        }
    }

    fn record(&self, report: SweepReport) {
        *self.last.lock() = Some(report);
    }
}

/// Run one sweep over the workspace: find the linked worktrees, skip the ones
/// in use, clean the rest when `apply`, stamp `cleaned_at` on the ones cleaned
/// and keep the report.
pub async fn run_sweep(state: Arc<AppState>, apply: bool) -> SweepReport {
    let root = state.config.workspace_root.clone();
    let worktrees = tokio::task::spawn_blocking(move || {
        worktree_clean::linked_worktrees_under(&root, MAX_DEPTH)
    })
    .await
    .unwrap_or_default();
    let at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let sessions = Arc::clone(&state.sessions);
    let cwds = {
        let sessions = Arc::clone(&sessions);
        move || sessions.cwds_in_use(None, true)
    };
    let stamp = at.clone();
    let report =
        tokio::task::spawn_blocking(move || sweep_worktrees(&worktrees, &cwds, apply, &stamp))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "worktree sweep task failed");
                SweepReport {
                    at: at.clone(),
                    ..SweepReport::default()
                }
            });
    for path in &report.cleaned {
        sessions.mark_workspace_cleaned(path, &at);
    }
    state.worktree_sweep.record(report.clone());
    report
}

/// Run the sweep on its schedule for the life of the process. Off does not
/// spawn anything. The first sweep is due a short settle delay after boot when
/// the last one is missing or a day old, so a restart neither walks the whole
/// workspace at once nor, by restarting often, postpones it for ever.
pub fn spawn_nightly_sweep(state: Arc<AppState>) {
    let mode = state.worktree_sweep.mode();
    if mode == SweepMode::Off {
        return;
    }
    let schedule = Schedule::from_env();
    let state_dir = state.config.state_dir.clone();
    let task_state = Arc::clone(&state);
    Arc::clone(&state.boot_tasks).spawn("worktree-sweep", async move {
        drive_schedule(
            schedule,
            &state_dir,
            || time::OffsetDateTime::now_utc().unix_timestamp(),
            tokio::time::sleep,
            || {
                let state = Arc::clone(&task_state);
                async move {
                    run_sweep(state, mode == SweepMode::On).await;
                }
            },
            None,
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn mode_parse_table() {
        assert_eq!(SweepMode::parse(Some("off")), Ok(SweepMode::Off));
        assert_eq!(SweepMode::parse(Some("0")), Ok(SweepMode::Off));
        assert_eq!(SweepMode::parse(Some("dry-run")), Ok(SweepMode::DryRun));
        assert_eq!(SweepMode::parse(Some("on")), Ok(SweepMode::On));
        assert_eq!(SweepMode::parse(Some("true")), Ok(SweepMode::On));
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    /// A main checkout with one commit, and a linked worktree of it holding a
    /// tracked crate plus regenerable `target/` output. Returns
    /// `(main, linked)`, both canonical.
    fn fixture(tmp: &Path) -> (PathBuf, PathBuf) {
        let main = tmp.join("repo");
        std::fs::create_dir(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["config", "user.email", "t@example.com"]);
        git(&main, &["config", "user.name", "t"]);
        std::fs::write(main.join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
        git(&main, &["add", "Cargo.toml"]);
        git(&main, &["commit", "-qm", "init"]);

        // The main checkout's own build output must survive every sweep.
        let main_target = main.join("target");
        std::fs::create_dir(&main_target).unwrap();
        std::fs::write(main_target.join("out"), "main").unwrap();

        let linked = tmp.join("linked");
        git(&main, &["worktree", "add", "-q", linked.to_str().unwrap()]);
        let linked = std::fs::canonicalize(&linked).unwrap();
        let target = linked.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("out"), "x".repeat(4096)).unwrap();
        let main = std::fs::canonicalize(&main).unwrap();
        (main, linked)
    }

    fn no_sessions() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn mode_defaults_to_dry_run_and_rejects_nonsense() {
        assert_eq!(SweepMode::parse(None), Ok(SweepMode::DryRun));
        assert_eq!(SweepMode::parse(Some("  ")), Ok(SweepMode::DryRun));
        assert_eq!(SweepMode::parse(Some("off")), Ok(SweepMode::Off));
        assert_eq!(SweepMode::parse(Some("dry-run")), Ok(SweepMode::DryRun));
        assert_eq!(SweepMode::parse(Some("on")), Ok(SweepMode::On));
        assert!(SweepMode::parse(Some("sometimes")).is_err());
    }

    #[test]
    fn finds_linked_worktrees_and_never_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (main, linked) = fixture(&root);
        // A nested worktree two levels down is still under the workspace.
        let deep = root.join("ws").join("ses");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        git(&main, &["worktree", "add", "-q", deep.to_str().unwrap()]);

        let found = worktree_clean::linked_worktrees_under(&root, 4);
        assert!(found.contains(&linked), "{found:?}");
        assert!(found.contains(&std::fs::canonicalize(&deep).unwrap()));
        assert!(
            !found.contains(&main),
            "a main checkout is never a sweep target"
        );
        assert_eq!(found.len(), 2);
    }

    /// The core of the change: a tree a live or hibernated session works in
    /// is never touched, even when it holds regenerable output.
    #[test]
    fn a_tree_a_live_session_uses_is_never_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (_main, linked) = fixture(&root);
        let session_cwd = linked.join("crate").to_string_lossy().into_owned();
        std::fs::create_dir(linked.join("crate")).unwrap();

        for apply in [true, false] {
            let in_use = vec![session_cwd.clone()];
            let report = sweep_worktrees(
                std::slice::from_ref(&linked),
                &|| in_use.clone(),
                apply,
                "t",
            );
            assert_eq!(report.in_use, 1, "apply={apply}");
            assert_eq!(report.bytes_reclaimable, 0, "apply={apply}");
            assert_eq!(report.bytes_reclaimed, 0, "apply={apply}");
            assert!(linked.join("target").join("out").exists(), "apply={apply}");
        }
    }

    #[test]
    fn dry_run_reports_bytes_and_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (_main, linked) = fixture(&root);

        let report = sweep_worktrees(std::slice::from_ref(&linked), &no_sessions, false, "t");
        assert!(report.bytes_reclaimable >= 4096, "{report:?}");
        assert_eq!(report.bytes_reclaimed, 0);
        assert!(linked.join("target").join("out").exists());
    }

    #[test]
    fn apply_removes_build_output_but_not_tracked_files_or_the_main_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (main, linked) = fixture(&root);

        let report = sweep_worktrees(std::slice::from_ref(&linked), &no_sessions, true, "t");
        assert!(report.bytes_reclaimed >= 4096, "{report:?}");
        assert_eq!(report.bytes_reclaimed, report.bytes_reclaimable);
        assert!(!linked.join("target").exists());
        assert!(linked.join("Cargo.toml").exists(), "a tracked file stays");
        assert!(
            main.join("target").join("out").exists(),
            "the main checkout stays"
        );
        assert!(linked.exists(), "the worktree itself is never removed");
        assert_eq!(report.cleaned, vec![linked.to_string_lossy().into_owned()]);
    }

    /// A fake clock: `sleep` advances it, so the schedule can be walked
    /// through days in microseconds.
    struct FakeClock(std::cell::Cell<i64>);

    impl FakeClock {
        fn now(&self) -> i64 {
            self.0.get()
        }
        async fn sleep(&self, d: Duration) {
            self.0.set(self.0.get() + d.as_secs() as i64);
        }
    }

    const H: i64 = 3600;

    /// Boot at `now` with `last` recorded; return the clock times at which
    /// sweeps ran over `runs` runs.
    fn run_times(last: Option<i64>, now: i64, runs: usize) -> Vec<i64> {
        let dir = tempfile::tempdir().unwrap();
        if let Some(last) = last {
            record_last_run(dir.path(), last).unwrap();
        }
        let clock = FakeClock(std::cell::Cell::new(now));
        let times = std::cell::RefCell::new(Vec::new());
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(drive_schedule(
            Schedule::default(),
            dir.path(),
            || clock.now(),
            |d| clock.sleep(d),
            || {
                times.borrow_mut().push(clock.now());
                std::future::ready(())
            },
            Some(runs),
        ));
        times.into_inner()
    }

    #[test]
    fn a_restart_two_hours_after_the_last_run_does_not_sweep_again() {
        let now = 1_000_000;
        let times = run_times(Some(now - 2 * H), now, 2);
        // Nothing runs at boot or after the settle delay: the first sweep is
        // 22 hours out, the next a full day after that.
        assert_eq!(times, vec![now + 22 * H, now + 46 * H]);
    }

    #[test]
    fn a_boot_with_the_last_run_25_hours_ago_sweeps_after_the_short_delay() {
        let now = 1_000_000;
        let times = run_times(Some(now - 25 * H), now, 2);
        assert_eq!(
            times,
            vec![now + SETTLE_DELAY.as_secs() as i64, now + 600 + 24 * H]
        );
    }

    #[test]
    fn a_missing_timestamp_sweeps_once_after_the_short_delay_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_last_run(dir.path()), None);
        let now = 1_000_000;
        let times = run_times(None, now, 1);
        assert_eq!(times, vec![now + 600]);

        let clock = FakeClock(std::cell::Cell::new(now));
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(drive_schedule(
            Schedule::default(),
            dir.path(),
            || clock.now(),
            |d| clock.sleep(d),
            || std::future::ready(()),
            Some(1),
        ));
        // Recorded, so the next boot does not sweep a second time at once.
        assert_eq!(load_last_run(dir.path()), Some(now + 600));
        assert_eq!(
            Schedule::default().first_wait(load_last_run(dir.path()), now + 700),
            Duration::from_secs(24 * 3600 - 100)
        );
    }

    #[test]
    fn a_future_or_unreadable_timestamp_counts_as_missing() {
        let s = Schedule::default();
        assert_eq!(s.first_wait(Some(2_000_000), 1_000_000), s.settle);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LAST_RUN_FILE), "garbage").unwrap();
        assert_eq!(load_last_run(dir.path()), None);
    }

    #[test]
    fn a_nearly_due_sweep_still_waits_the_settle_delay() {
        let s = Schedule::default();
        let now = 1_000_000;
        assert_eq!(s.first_wait(Some(now - 24 * H + 5), now), s.settle);
    }
}
