//! CI watching on the branches work items are bound to. Ports
//! `services/ci_watch.py`'s announce (WI-855).
//!
//! A branch bound to a work item is watched: the `forge-checks` sweep observes
//! its runs, and once every workflow on the branch's newest revision has
//! concluded this module
//!
//! - leaves the Inbox entry that `inbox.rs` builds from the same branches, and
//! - after the sweep, publishes one `ci.branch_concluded` event on the declared
//!   feed and types a one-line notice into each live session started for the
//!   item, so the agent wakes on the result.
//!
//! A run still queued or in progress past `ci_hung_queued_after_minutes` /
//! `ci_hung_running_after_minutes` (WI-1334) is a `ci.run_hung` Inbox entry
//! (gone once the run concludes) and one notice per run into the same
//! sessions, recorded as a `ci.run_hung` event keyed by the run. Detection
//! only: nothing is cancelled.
//!
//! "Once" is the event: its entity id names the item, branch, revision and
//! verdict, so a conclusion that already has an event is never re-announced,
//! not by the next sweep and not by a restart. Only fresh conclusions (the last
//! run finished within [`FRESH_WINDOW_SECONDS`]) are announced, so binding an
//! old branch never floods a session with history.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapters::forge::KIND_CHECK;
use crate::application::context::AppContext;
use crate::application::services::sessions::audit_system_input;
use crate::config::VogtConfig;
use crate::core::{Clock, IdFactory, Moment, Observation, WorkOverlay, TERMINAL_STATES};
use crate::decisions::{self, BranchCi, BranchState, HungPhase, HungRun, HungThresholds};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

/// `ci_watch.py` `CI_BRANCH_CONCLUDED_EVENT`.
pub const CI_BRANCH_CONCLUDED_EVENT: &str = "ci.branch_concluded";
/// `ci_watch.py` `CI_RUN_HUNG_EVENT`.
pub const CI_RUN_HUNG_EVENT: &str = "ci.run_hung";
/// How recent a conclusion must be to be announced to a session: six hours.
const FRESH_WINDOW_SECONDS: i64 = 6 * 3600;
/// Bound overlays considered per read. Python's `ci_watch.MAX_BOUND`.
const MAX_BOUND: i64 = 500;
/// Live sessions nudged per conclusion. Python's `ci_watch.MAX_SESSIONS`.
const MAX_SESSIONS: i64 = 10;
/// Python's `ci_watch.NOTICE_MAX_CHARS`, counted in characters.
const NOTICE_MAX_CHARS: usize = 400;
/// The check observations read per project for the announce. Python's limit.
const CHECKS_PER_PROJECT: i64 = 2000;

/// The announce's failure seam. A test sets `FAIL` and the next announce fails
/// the way an engine that fell over would, after counting the attempt in
/// `ATTEMPTS`. Thread-local, so each test owns its own switch.
#[cfg(test)]
pub(crate) mod seam {
    use std::cell::Cell;

    thread_local! {
        pub(crate) static FAIL: Cell<bool> = const { Cell::new(false) };
        pub(crate) static ATTEMPTS: Cell<u32> = const { Cell::new(0) };
    }
}

/// One branch a still-open work item is bound to, and its CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundBranch {
    pub work_item_ref: String,
    pub work_item_id: Option<String>,
    pub project_id: String,
    pub branch: String,
    pub ci: BranchCi,
}

impl BoundBranch {
    /// `ci_watch.py` `BoundBranch.event_entity`: the event's identity.
    pub fn event_entity(&self) -> String {
        format!(
            "{}:{}@{}:{}",
            self.work_item_ref,
            self.branch,
            self.ci.revision,
            state_word(self.ci.state)
        )
    }
}

/// The lower-case word Python's `BranchCi.state` carries.
fn state_word(state: BranchState) -> &'static str {
    match state {
        BranchState::Running => "running",
        BranchState::Passed => "passed",
        BranchState::Failed => "failed",
        BranchState::Cancelled => "cancelled",
    }
}

/// The sweep's clock, read once per use as Python's `ctx.clock()` is.
fn moment_now<C: Clock, I: IdFactory>(ctx: &AppContext<C, I>) -> Moment {
    let mut clock = ctx
        .clock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    clock.now()
}

/// A conclusion is settled once no run is still going.
fn settled(ci: &BranchCi) -> bool {
    matches!(
        ci.state,
        BranchState::Passed | BranchState::Failed | BranchState::Cancelled
    )
}

/// The configured hung-run thresholds; zero minutes is off.
pub fn hung_thresholds(config: &VogtConfig) -> HungThresholds {
    let seconds = |minutes: i64| (minutes > 0).then(|| minutes * 60);
    HungThresholds {
        queued: seconds(config.ci_hung_queued_after_minutes),
        running: seconds(config.ci_hung_running_after_minutes),
    }
}

/// What a hung run is deduped by: the run (and attempt) itself, or the
/// observation's subject where the forge gave no run id.
pub fn hung_entity(hung: &HungRun) -> String {
    let payload = &hung.observation.payload;
    if hung.run_id.is_none() {
        return format!("run:{}", hung.observation.subject_key);
    }
    let suffix = payload
        .get("run_attempt")
        .and_then(Value::as_i64)
        .map(|attempt| format!(".{attempt}"))
        .unwrap_or_default();
    let repo = payload.get("repo").and_then(Value::as_str).unwrap_or("");
    let run_id = hung.run_id.unwrap_or_default();
    format!("run:{repo}:{run_id}{suffix}")
}

/// CI on the newest revision of every branch an open item is bound to.
///
/// `checks_by_branch` is keyed `(project_id, branch)`, built once from the
/// checks the caller already holds, so nothing here re-reads observations per
/// branch. An item's state is read only for an overlay with observed runs.
pub fn bound_branches(
    view: &impl ReadView,
    checks_by_branch: &BTreeMap<(String, String), Vec<Observation>>,
    hung_after: Option<(i64, HungThresholds)>,
) -> Result<Vec<BoundBranch>, VogtError> {
    let mut found = Vec::new();
    for overlay in view.bound_branch_overlays(MAX_BOUND)? {
        let settled_branches = settled_on(&overlay, checks_by_branch, hung_after);
        if settled_branches.is_empty() {
            continue;
        }
        let item = view.work_item_by_ref(&overlay.subject_key)?;
        let state = match &item {
            Some(item) => Some(item.state.as_str().to_owned()),
            None => overlay.workflow_state.clone(),
        };
        if state
            .as_deref()
            .is_some_and(|state| TERMINAL_STATES.contains(&state))
        {
            continue;
        }
        for (branch, ci) in settled_branches {
            found.push(BoundBranch {
                work_item_ref: overlay.subject_key.clone(),
                work_item_id: item.as_ref().map(|item| item.id.clone()),
                project_id: overlay.project_id.clone(),
                branch,
                ci,
            });
        }
    }
    Ok(found)
}

/// The branches of one overlay that have runs observed, with their CI.
fn settled_on(
    overlay: &WorkOverlay,
    checks_by_branch: &BTreeMap<(String, String), Vec<Observation>>,
    hung_after: Option<(i64, HungThresholds)>,
) -> Vec<(String, BranchCi)> {
    let mut found = Vec::new();
    for branch in &overlay.branches {
        let Some(runs) = checks_by_branch.get(&(overlay.project_id.clone(), branch.clone())) else {
            continue;
        };
        if let Some(ci) = decisions::branch_ci_at(runs, branch, hung_after) {
            found.push((branch.clone(), ci));
        }
    }
    found
}

/// Check observations keyed `(project_id, branch)`. An observation without a
/// project or a branch is not CI on any branch, so it is skipped.
pub fn index_by_branch<'a>(
    checks: impl IntoIterator<Item = &'a Observation>,
) -> BTreeMap<(String, String), Vec<Observation>> {
    let mut index: BTreeMap<(String, String), Vec<Observation>> = BTreeMap::new();
    for check in checks {
        let Some(project_id) = check.project_id.clone() else {
            continue;
        };
        let Some(branch) = check.payload.get("branch").and_then(Value::as_str) else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        index
            .entry((project_id, branch.to_owned()))
            .or_default()
            .push(check.clone());
    }
    index
}

/// The label that registers a work item as a known flaky test or job.
pub const FLAKY_LABEL: &str = "ci:flaky";
/// Open `ci:flaky` items read per failing branch.
const MAX_FLAKY_ITEMS: i64 = 200;

/// Lower-case alphanumeric tokens of `text`; `_` joins a token, so a test name
/// like `a_claude_typed` is one token.
fn tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Whether `needle` occurs in `haystack` as a whole-token run.
fn contains_run(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Which registered flaky items a failure probably is (WI-1335).
///
/// `failed_jobs` is the check payload's `failed_jobs` array (built by
/// `adapters/forge/collectors.rs::failed_jobs` from `ForgeProvider::failed_jobs`):
/// each entry is `{"name": <job name>, "conclusion": <string|null>,
/// "url": <string|null>}`. It carries **job names only**: no step names and no
/// test names, and log parsing is out of scope. So matching is on job names.
///
/// `flaky_items` is `(ref, title)` of the open `ci:flaky` items. An item is a
/// suspect when its title contains a failed job's name, case-insensitively and
/// as whole tokens. Returns `(ref, matched job name)`, one per item, in the
/// order of `flaky_items`.
pub fn flaky_suspects(
    failed_jobs: &[Value],
    flaky_items: &[(String, String)],
) -> Vec<(String, String)> {
    let names: Vec<(&str, Vec<String>)> = failed_jobs
        .iter()
        .filter_map(|job| job.get("name").and_then(Value::as_str))
        .map(|name| (name, tokens(name)))
        .collect();
    let mut found = Vec::new();
    for (reference, title) in flaky_items {
        let title_tokens = tokens(title);
        if let Some((name, _)) = names
            .iter()
            .find(|(_, name_tokens)| contains_run(&title_tokens, name_tokens))
        {
            found.push((reference.clone(), (*name).to_owned()));
        }
    }
    found
}

/// Suspects for a failed branch: the failed jobs of its failing runs against
/// the project's open `ci:flaky` items. Empty unless the branch failed.
pub fn branch_flaky_suspects(
    view: &impl ReadView,
    project_id: &str,
    ci: &BranchCi,
) -> Result<Vec<(String, String)>, VogtError> {
    if ci.state != BranchState::Failed {
        return Ok(Vec::new());
    }
    let jobs: Vec<Value> = ci
        .runs
        .iter()
        .filter(|run| ci.failing.contains(&run.workflow))
        .filter_map(|run| run.observation.payload.get("failed_jobs"))
        .filter_map(Value::as_array)
        .flatten()
        .cloned()
        .collect();
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    let items = view.list_work_items(&crate::storage::interface::WorkFilter {
        project_id: Some(project_id.to_owned()),
        label: Some(FLAKY_LABEL.to_owned()),
        exclude_terminal: true,
        limit: MAX_FLAKY_ITEMS,
        ..Default::default()
    })?;
    let flaky: Vec<(String, String)> = items
        .into_iter()
        .map(|item| (item.reference, item.title))
        .collect();
    Ok(flaky_suspects(&jobs, &flaky))
}

/// The one line typed into a bound session. Plain text, no control characters:
/// it is data for the agent, not a command.
pub fn notice_text(bound: &BoundBranch, suspects: &[(String, String)]) -> String {
    let ci = &bound.ci;
    let verdict = match ci.state {
        BranchState::Passed => "PASSED".to_owned(),
        BranchState::Failed => "FAILED".to_owned(),
        BranchState::Cancelled => "CANCELLED".to_owned(),
        BranchState::Running => "RUNNING".to_owned(),
    };
    let detail = if ci.failing.is_empty() {
        format!("; {} workflow(s)", ci.runs.len())
    } else {
        format!("; failing: {}", ci.failing.join(", "))
    };
    let link = ci
        .runs
        .iter()
        .find(|run| ci.failing.contains(&run.workflow) && run.observation.source_url.is_some())
        .and_then(|run| run.observation.source_url.clone());
    let tail = link.map(|link| format!(" — {link}")).unwrap_or_default();
    let short: String = ci.revision.chars().take(12).collect();
    let flaky = if suspects.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = suspects
            .iter()
            .map(|(reference, matched)| format!("{reference} ({matched})"))
            .collect();
        format!("; probably flaky: {}", list.join(", "))
    };
    let line = format!(
        "[vogt] CI {verdict} on {} @ {short} ({}){detail}{flaky}{tail}",
        bound.branch, bound.work_item_ref
    );
    // Workflow names and URLs come from the forge: strip every character that
    // is not printable (an Esc or a carriage return would be a keystroke, not
    // text) and bound the length, so the line can only ever be read.
    line.chars()
        .filter(|ch| crate::core::py_printable(*ch))
        .take(NOTICE_MAX_CHARS)
        .collect()
}

/// The one line typed into a bound session about a hung run.
pub fn hung_notice_text(bound: &BoundBranch, hung: &HungRun) -> String {
    let run = hung
        .run_id
        .map_or_else(|| "?".to_owned(), |id| id.to_string());
    let verb = match hung.phase {
        HungPhase::Queued => "been queued",
        HungPhase::Running => "been running",
    };
    let line = format!(
        "[vogt] CI run {run} ({}) on {} has {verb} {} min; it may be hung ({})",
        hung.workflow, bound.branch, hung.elapsed_minutes, bound.work_item_ref
    );
    line.chars()
        .filter(|ch| crate::core::py_printable(*ch))
        .take(NOTICE_MAX_CHARS)
        .collect()
}

/// Whether a conclusion finished recently enough to announce. `concluded_at`
/// must carry an offset, as Python's `fromisoformat` check requires: a naive
/// timestamp is never fresh.
fn fresh(concluded_at: Option<&str>, now_unix: i64) -> bool {
    let Some(text) = concluded_at else {
        return false;
    };
    let Ok(moment) = chrono::DateTime::parse_from_rfc3339(text) else {
        return false;
    };
    moment.timestamp() >= now_unix - FRESH_WINDOW_SECONDS
}

/// Announce fresh, not-yet-announced bound-branch conclusions.
///
/// Called after a sweep's projections are rebuilt. Returns how many
/// conclusions were announced. An engine that cannot take the notice is not an
/// error here: the Inbox entry already says the fact, so the sweep goes on.
pub fn announce_concluded<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
) -> Result<usize, VogtError> {
    #[cfg(test)]
    {
        seam::ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
        if seam::FAIL.with(std::cell::Cell::get) {
            return Err(VogtError::EngineUnavailable(
                "announce failure injected by the test seam".to_owned(),
            ));
        }
    }
    let found = {
        let view = ctx.declared.read()?;
        let mut projects: Vec<String> = view
            .bound_branch_overlays(MAX_BOUND)?
            .into_iter()
            .map(|overlay| overlay.project_id)
            .collect();
        projects.sort();
        projects.dedup();
        if projects.is_empty() {
            return Ok(0);
        }
        let mut checks: Vec<Observation> = Vec::new();
        for project_id in &projects {
            checks.extend(ctx.observed.latest(
                &[KIND_CHECK.to_owned()],
                Some(project_id.as_str()),
                false,
                false,
                CHECKS_PER_PROJECT,
            )?);
        }
        let now = moment_now(ctx).unix_seconds();
        let hung_after = Some((now, hung_thresholds(&ctx.config)));
        bound_branches(&view, &index_by_branch(&checks), hung_after)?
    };
    let mut announced = 0;
    for bound in found {
        announce_hung(ctx, &bound)?;
        let now = moment_now(ctx).unix_seconds();
        if !settled(&bound.ci) || !fresh(bound.ci.concluded_at.as_deref(), now) {
            continue;
        }
        let entity = bound.event_entity();
        let (sessions, suspects): (Vec<String>, Vec<(String, String)>) = {
            let view = ctx.declared.read()?;
            if !view.list_events(0, 1, Some(entity.as_str()))?.is_empty() {
                continue;
            }
            let sessions = match &bound.work_item_id {
                None => Vec::new(),
                Some(work_item_id) => view
                    .list_sessions(None, Some(work_item_id), false, MAX_SESSIONS, 0)?
                    .into_iter()
                    .map(|session| session.engine_session_id)
                    .collect(),
            };
            let suspects = branch_flaky_suspects(&view, &bound.project_id, &bound.ci)?;
            (sessions, suspects)
        };
        let nudged = nudge(ctx, &sessions, &notice_text(&bound, &suspects))?;
        ctx.declared.publish_event(
            CI_BRANCH_CONCLUDED_EVENT,
            "work_item_branch",
            &entity,
            &json!({
                "work_item": bound.work_item_ref,
                "branch": bound.branch,
                "revision": bound.ci.revision,
                "state": state_word(bound.ci.state),
                "failing": bound.ci.failing,
                "flaky_suspects": suspects.iter().map(|(reference, _)| reference).collect::<Vec<_>>(),
                "sessions_notified": nudged,
            }),
            moment_now(ctx),
        )?;
        announced += 1;
    }
    Ok(announced)
}

/// One event and one session notice per hung run, ever. Returns how many runs
/// were announced.
fn announce_hung<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    bound: &BoundBranch,
) -> Result<usize, VogtError> {
    let mut announced = 0;
    for hung in &bound.ci.hung {
        let entity = hung_entity(hung);
        let sessions: Vec<String> = {
            let view = ctx.declared.read()?;
            if !view.list_events(0, 1, Some(entity.as_str()))?.is_empty() {
                continue;
            }
            match &bound.work_item_id {
                None => Vec::new(),
                Some(work_item_id) => view
                    .list_sessions(None, Some(work_item_id), false, MAX_SESSIONS, 0)?
                    .into_iter()
                    .map(|session| session.engine_session_id)
                    .collect(),
            }
        };
        let nudged = nudge(ctx, &sessions, &hung_notice_text(bound, hung))?;
        ctx.declared.publish_event(
            CI_RUN_HUNG_EVENT,
            "ci_run",
            &entity,
            &json!({
                "work_item": bound.work_item_ref,
                "branch": bound.branch,
                "workflow": hung.workflow,
                "run_id": hung.run_id,
                "phase": match hung.phase {
                    HungPhase::Queued => "queued",
                    HungPhase::Running => "running",
                },
                "elapsed_minutes": hung.elapsed_minutes,
                "sessions_notified": nudged,
            }),
            moment_now(ctx),
        )?;
        announced += 1;
    }
    Ok(announced)
}

/// Type the notice and then Enter into each session. A session at a permission
/// prompt refuses the input (WI-983): that is a person's answer to give, so the
/// session hears about the conclusion the next time it reads the Inbox.
fn nudge<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    engine_ids: &[String],
    text: &str,
) -> Result<usize, VogtError> {
    let Some(engine) = ctx
        .engine
        .as_ref()
        .filter(|_| ctx.config.ci_watch_notify_sessions)
    else {
        return Ok(0);
    };
    let mut sent = 0;
    for engine_id in engine_ids {
        // The text, then the Enter that submits it. The Enter is sent only when
        // the text was taken. Text that was typed is audited even when the
        // Enter was refused: it is on the terminal either way.
        let mut typed = false;
        let mut delivered = || -> Result<bool, VogtError> {
            typed = engine.send_input(engine_id, text, false, false, None)?;
            Ok(typed && engine.send_input(engine_id, "", true, false, None)?)
        };
        let result = delivered();
        if typed {
            // The audit never holds the text. A failed audit is logged, not
            // retried: retyping the notice would be worse than the gap.
            if let Err(error) = audit_system_input(
                ctx,
                engine_id,
                "ci_watch",
                "CI conclusion notice typed into a session bound to the branch",
                text.len(),
                matches!(result, Ok(true)),
            ) {
                tracing::warn!(target: "vogt.ci_watch", session = %engine_id, error = %error.message(), "could not audit a CI notice typed into a session");
            }
        }
        match result {
            Ok(true) => sent += 1,
            Ok(false) => {}
            Err(VogtError::PersonRequired(_)) => {
                tracing::info!(target: "vogt.ci_watch", session = %engine_id, "not notifying a session at a permission prompt of a CI conclusion");
            }
            Err(VogtError::EngineUnavailable(error)) => {
                tracing::warn!(target: "vogt.ci_watch", session = %engine_id, error = %error, "could not notify a bound session of a CI conclusion");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::engine::{EngineClient, Transport};
    use crate::adapters::forge::{COLLECTOR_CHECKS, KIND_CHECK};
    use crate::application::context::{build_context, Built};
    use crate::application::projects::register_op;
    use crate::application::services::work::{bind_branch_op, create_op};
    use crate::config::VogtConfig;
    use crate::core::{
        ActorKind, CodingSession, Moment, Principal, SequentialIds, StepClock, SweepOutcome,
    };
    use crate::storage::interface::WriteTxn;
    use crate::storage::observed_types::PendingObservation;
    use std::sync::{Arc, Mutex};

    fn unique_dir(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vogt-ci-watch-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A step-clock instance with one registered project `app` and one work
    /// item `WI-1` bound to branch `wi-1`. Nothing else is configured: no engine
    /// and no forge, so the announce reaches the declared feed only.
    fn bound_instance(label: &str, engine: Option<EngineClient>) -> Built {
        let dir = unique_dir(label);
        let mut clock = Some(StepClock::new(Moment::from_unix(1_700_000_000, 0)));
        let mut ids = Some(SequentialIds::new(None).unwrap());
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let config = VogtConfig {
            data_dir: dir.clone(),
            ..VogtConfig::default()
        };
        let built = build_context(
            config,
            Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
            clock,
            ids,
            None,
            engine,
            None,
            None,
        )
        .unwrap();
        register_op(
            &built,
            json!({"name": "app", "root_path": dir.to_string_lossy(), "reason": "test"}),
        )
        .unwrap();
        create_op(
            &built,
            json!({"kind": "chore", "title": "Watch me", "project": "app", "local_only": true, "reason": "test"}),
        )
        .unwrap();
        bind_branch_op(
            &built,
            json!({"ref": "WI-1", "branch": "wi-1", "reason": "test"}),
        )
        .unwrap();
        built
    }

    fn context(built: &Built) -> &AppContext<StepClock, SequentialIds> {
        match built {
            Built::StepSequential(ctx) => ctx,
            _ => unreachable!("bound_instance builds a step clock"),
        }
    }

    /// A check observation on `wi-1`, stamped `updated_at`, sealed as one sweep.
    fn seed(
        ctx: &AppContext<StepClock, SequentialIds>,
        checks: &[(&str, Option<&str>)],
        updated_at: &str,
    ) {
        seed_with(ctx, checks, updated_at, None);
    }

    fn seed_with(
        ctx: &AppContext<StepClock, SequentialIds>,
        checks: &[(&str, Option<&str>)],
        updated_at: &str,
        failed_jobs: Option<Value>,
    ) {
        let project_id = ctx
            .declared
            .read()
            .unwrap()
            .list_projects(10, 0)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .id;
        let at = moment_now(ctx);
        let findings: Vec<PendingObservation> = checks
            .iter()
            .map(|(name, conclusion)| PendingObservation {
                kind: KIND_CHECK.to_owned(),
                subject_key: format!("ci:acme/app@{}:{name}@wi-1", "b".repeat(40)),
                payload: json!({
                    "revision": "b".repeat(40),
                    "check": name,
                    "conclusion": conclusion,
                    "status": if conclusion.is_some() { "completed" } else { "in_progress" },
                    "branch": "wi-1",
                    "event": "pull_request",
                    "updated_at": updated_at,
                    "failed_jobs": failed_jobs.clone().filter(|_| *conclusion == Some("failure")),
                }),
                content_digest: format!("{name}-{conclusion:?}-{updated_at}"),
                project_id: Some(project_id.clone()),
                source_url: Some(format!("https://github.com/acme/app/actions/runs/{name}")),
                promoted: false,
            })
            .collect();
        let row = ctx
            .observed
            .begin_sweep(COLLECTOR_CHECKS, std::slice::from_ref(&project_id), at)
            .unwrap();
        ctx.observed.append(&row.id, &findings, at).unwrap();
        ctx.observed
            .finish_sweep(&row.id, SweepOutcome::Ok, &[("projects", 1)], at, None)
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
    }

    fn stamp(ctx: &AppContext<StepClock, SequentialIds>) -> String {
        let now = moment_now(ctx).unix_seconds();
        chrono::DateTime::from_timestamp(now - 300, 0)
            .unwrap()
            .to_rfc3339()
    }

    fn concluded_events(built: &Built) -> Vec<Value> {
        let ctx = context(built);
        let view = ctx.declared.read().unwrap();
        view.list_events(0, 500, None)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == CI_BRANCH_CONCLUDED_EVENT)
            .map(|event| event.summary)
            .collect()
    }

    /// A transport that records each engine request and answers 200 `{"ok":true}`.
    fn recording_engine() -> (Arc<Mutex<Vec<String>>>, EngineClient) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let transport: Transport = Arc::new(move |url, _headers, body, method| {
            record
                .lock()
                .unwrap()
                .push(format!("{method} {url} {}", String::from_utf8_lossy(body)));
            (200, br#"{"ok":true}"#.to_vec())
        });
        (
            seen,
            EngineClient::new("http://engine", Some("tok".to_string()), Some(transport)),
        )
    }

    /// A transport that answers every request as a session showing a
    /// permission prompt does: the engine refuses input with `person required`.
    fn prompting_engine() -> (Arc<Mutex<Vec<String>>>, EngineClient) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let transport: Transport = Arc::new(move |url, _headers, body, method| {
            record
                .lock()
                .unwrap()
                .push(format!("{method} {url} {}", String::from_utf8_lossy(body)));
            (
                403,
                br#"{"error":"forbidden: person required: a permission prompt"}"#.to_vec(),
            )
        });
        (
            seen,
            EngineClient::new("http://engine", Some("tok".to_string()), Some(transport)),
        )
    }

    /// Start a live session for the item `WI-1` is in: a declared row with the
    /// engine id the nudge types into. Written the way `start_session` writes
    /// one, so the announce finds it through the item.
    fn live_session(ctx: &AppContext<StepClock, SequentialIds>, engine_id: &str) {
        let (project_id, work_item_id, actor_id) = {
            let view = ctx.declared.read().unwrap();
            let project_id = view
                .list_projects(10, 0)
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
                .id;
            let work_item_id = view
                .work_item_by_ref("WI-1")
                .unwrap()
                .expect("WI-1 exists")
                .id;
            let actor_id = view
                .actor_by_identity("local:test-user")
                .unwrap()
                .expect("the test principal is an actor")
                .id;
            (project_id, work_item_id, actor_id)
        };
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_session(&CodingSession {
            id: format!("ses_{engine_id}"),
            engine_session_id: engine_id.to_owned(),
            project_id,
            work_item_id: Some(work_item_id),
            actor_id,
            cwd: "/tmp/app".to_owned(),
            template: None,
            model: None,
            effort: None,
            reason: "test".to_owned(),
            purpose: None,
            started_by_session: None,
            started_by_kind: None,
            started_at: moment_now(ctx),
            stopped_at: None,
        })
        .unwrap();
        txn.commit().unwrap();
    }

    fn settled_ci(state: BranchState, failing: &[&str]) -> BranchCi {
        BranchCi {
            branch: "wi-9".to_owned(),
            revision: "a".repeat(40),
            state,
            runs: Vec::new(),
            failing: failing.iter().map(|name| (*name).to_owned()).collect(),
            concluded_at: Some("2026-10-04T00:00:00Z".to_owned()),
            hung: Vec::new(),
        }
    }

    #[test]
    fn a_fresh_conclusion_is_one_with_an_offset_inside_the_window() {
        let now = 1_700_000_000;
        let inside = chrono::DateTime::from_timestamp(now - 3600, 0)
            .unwrap()
            .to_rfc3339();
        let outside = chrono::DateTime::from_timestamp(now - FRESH_WINDOW_SECONDS - 60, 0)
            .unwrap()
            .to_rfc3339();
        assert!(fresh(Some(&inside), now));
        assert!(!fresh(Some(&outside), now), "older than the window");
        assert!(
            !fresh(Some("2023-11-14T22:13:20"), now),
            "naive is never fresh"
        );
        assert!(!fresh(Some("not a time"), now));
        assert!(!fresh(None, now));
    }

    #[test]
    fn notice_text_is_one_plain_line() {
        let text = notice_text(
            &BoundBranch {
                work_item_ref: "WI-9".to_owned(),
                work_item_id: None,
                project_id: "prj_1".to_owned(),
                branch: "wi-9".to_owned(),
                ci: settled_ci(BranchState::Passed, &[]),
            },
            &[],
        );
        assert!(
            text.starts_with("[vogt] CI PASSED on wi-9 @ aaaaaaaaaaaa (WI-9)"),
            "{text}"
        );
        assert!(!text.contains('\n') && !text.contains('\r'));

        let hostile = notice_text(
            &BoundBranch {
                work_item_ref: "WI-9".to_owned(),
                work_item_id: None,
                project_id: "prj_1".to_owned(),
                branch: "wi-9".to_owned(),
                ci: settled_ci(BranchState::Failed, &["ci\u{1b}[2J\rrm -rf"]),
            },
            &[],
        );
        assert!(
            !hostile.contains('\u{1b}') && !hostile.contains('\r'),
            "{hostile:?}"
        );
        assert!(hostile.chars().count() <= NOTICE_MAX_CHARS);
    }

    fn jobs(names: &[&str]) -> Vec<Value> {
        names
            .iter()
            .map(|name| json!({"name": name, "conclusion": "failure", "url": null}))
            .collect()
    }

    fn item(reference: &str, title: &str) -> (String, String) {
        (reference.to_owned(), title.to_owned())
    }

    #[test]
    fn a_flaky_item_naming_a_failed_job_is_a_suspect() {
        let flaky = [
            item("WI-1309", "Flaky engine test: a_claude_typed_into_a_shell"),
            item("WI-1305", "Flaky nzb-postproc test: ETXTBSY"),
        ];
        assert_eq!(
            flaky_suspects(&jobs(&["Engine"]), &flaky),
            vec![("WI-1309".to_owned(), "Engine".to_owned())]
        );
        assert!(flaky_suspects(&jobs(&["lint"]), &flaky).is_empty());
        // whole tokens only: `eng` is not `engine`
        assert!(flaky_suspects(&jobs(&["eng"]), &flaky).is_empty());
        assert!(flaky_suspects(&[], &flaky).is_empty());
    }

    #[test]
    fn the_notice_names_the_suspects() {
        let bound = BoundBranch {
            work_item_ref: "WI-9".to_owned(),
            work_item_id: None,
            project_id: "prj_1".to_owned(),
            branch: "wi-9".to_owned(),
            ci: settled_ci(BranchState::Failed, &["ci"]),
        };
        let text = notice_text(&bound, &[("WI-1309".to_owned(), "engine".to_owned())]);
        assert!(
            text.contains("; probably flaky: WI-1309 (engine)"),
            "{text}"
        );
        assert!(!notice_text(&bound, &[]).contains("flaky"));
    }

    #[test]
    fn a_failure_in_a_flaky_job_carries_the_suspect_to_the_event_and_notice() {
        let (seen, engine) = recording_engine();
        let built = bound_instance("flaky", Some(engine));
        let ctx = context(&built);
        crate::application::taxonomy::create_label_op(
            &built,
            json!({"name": "ci:flaky", "reason": "test"}),
        )
        .unwrap();
        create_op(
            &built,
            json!({"kind": "bug", "title": "Flaky engine test: a_claude_typed", "project": "app", "local_only": true, "labels": ["ci:flaky"], "reason": "test"}),
        )
        .unwrap();
        live_session(ctx, "eng-flaky");
        let at = stamp(ctx);
        seed_with(
            ctx,
            &[("ci", Some("failure"))],
            &at,
            Some(json!([{"name": "engine", "conclusion": "failure", "url": null}])),
        );
        assert_eq!(announce_concluded(ctx).unwrap(), 1);
        let events = concluded_events(&built);
        assert_eq!(events[0]["flaky_suspects"], json!(["WI-2"]), "{events:?}");
        let listed = crate::application::services::inbox::inbox_list_op(&built, json!({})).unwrap();
        let rows = listed.to_string();
        assert!(rows.contains("\"flaky_suspects\":[\"WI-2\"]"), "{rows}");
        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|text| text.contains("probably flaky: WI-2 (engine)")),
            "{:?}",
            seen.lock().unwrap()
        );
    }

    #[test]
    fn a_bound_branch_conclusion_reaches_the_feed_once() {
        let (seen, engine) = recording_engine();
        let built = bound_instance("once", Some(engine));
        let ctx = context(&built);
        let at = stamp(ctx);

        seed(ctx, &[("ci", None), ("lint", Some("success"))], &at);
        assert_eq!(announce_concluded(ctx).unwrap(), 0, "still running");
        assert!(concluded_events(&built).is_empty());

        seed(
            ctx,
            &[("ci", Some("failure")), ("lint", Some("success"))],
            &at,
        );
        assert_eq!(announce_concluded(ctx).unwrap(), 1);
        let events = concluded_events(&built);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["state"], "failed");
        assert_eq!(events[0]["failing"], json!(["ci"]));
        assert_eq!(events[0]["work_item"], "WI-1");
        // No session is started for the item, so nothing is typed into one.
        assert_eq!(events[0]["sessions_notified"], 0);
        assert!(seen.lock().unwrap().is_empty());

        assert_eq!(announce_concluded(ctx).unwrap(), 0, "once per conclusion");
        assert_eq!(concluded_events(&built).len(), 1);
    }

    #[test]
    fn a_nudge_typed_into_a_session_leaves_one_audit_row_and_never_the_text() {
        // WI-834: the notice is text on a PTY, so it is audited like any
        // `session.input`: session, byte count, submit; never the text.
        let (seen, engine) = recording_engine();
        let built = bound_instance("nudge-audit", Some(engine));
        let ctx = context(&built);
        live_session(ctx, "eng-nudge");
        let at = stamp(ctx);
        seed(
            ctx,
            &[("ci", Some("failure")), ("lint", Some("success"))],
            &at,
        );
        assert_eq!(announce_concluded(ctx).unwrap(), 1);
        let typed = seen.lock().unwrap()[0].clone();
        assert!(typed.contains("[vogt] CI FAILED on wi-1"), "{typed}");

        let view = ctx.declared.read().unwrap();
        let rows = view
            .list_audit(&crate::storage::interface::AuditQuery {
                limit: 20,
                offset: 0,
                actor_id: None,
                operation: Some("session.input".to_string()),
                entity_id: None,
                project_id: None,
                since: None,
                until: None,
            })
            .unwrap();
        assert_eq!(rows.len(), 1, "one audit row per nudged session: {rows:?}");
        let row = &rows[0];
        assert_eq!(row.entity_kind, "session");
        let session = view
            .session_by_engine_id("eng-nudge")
            .unwrap()
            .expect("the session is linked");
        assert_eq!(row.entity_id, session.id);
        let events = view.list_events(0, 500, None).unwrap();
        let input: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "session.input")
            .collect();
        assert_eq!(input.len(), 1, "{events:?}");
        assert_eq!(input[0].summary["bytes"], notice_len(&typed));
        assert_eq!(input[0].summary["submit"], true);
        assert_eq!(input[0].summary["source"], "ci_watch");
        assert_eq!(input[0].summary["keys"], json!([]));
        assert!(
            !format!("{rows:?}").contains("CI FAILED")
                && !events.iter().any(|e| {
                    e.kind == "session.input" && format!("{e:?}").contains("CI FAILED")
                }),
            "an audit row or its event holds the typed text"
        );
    }

    /// The `text` byte length in a recorded `POST .../input` line.
    fn notice_len(line: &str) -> usize {
        let body = line.split_once("\r\n\r\n").map_or(line, |(_, body)| body);
        let start = body.find('{').expect("a JSON body");
        let value: Value = serde_json::from_str(&body[start..]).expect("JSON body");
        value["text"].as_str().expect("text").len()
    }

    #[test]
    fn a_bound_session_is_nudged_once_with_the_notice_then_enter() {
        let (seen, engine) = recording_engine();
        let built = bound_instance("nudge", Some(engine));
        let ctx = context(&built);
        live_session(ctx, "eng-nudge");
        let at = stamp(ctx);

        seed(
            ctx,
            &[("ci", Some("failure")), ("lint", Some("success"))],
            &at,
        );
        assert_eq!(announce_concluded(ctx).unwrap(), 1);

        // The notice is typed, then the Enter that submits it, and nothing
        // else: the person flag stays off, so the engine can refuse a prompt.
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(sent[0].starts_with("POST ") && sent[0].contains("/eng-nudge/input"));
        assert!(sent[0].contains(r#""submit":false"#), "{}", sent[0]);
        assert!(sent[0].contains("[vogt] CI FAILED on wi-1"), "{}", sent[0]);
        assert!(sent[0].contains(r#""person":false"#), "{}", sent[0]);
        assert!(sent[1].contains(r#""text":"""#), "{}", sent[1]);
        assert!(sent[1].contains(r#""submit":true"#), "{}", sent[1]);

        let events = concluded_events(&built);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["sessions_notified"], 1);

        // Once per conclusion: the next announce types nothing new.
        assert_eq!(announce_concluded(ctx).unwrap(), 0);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_session_at_a_permission_prompt_gets_no_enter_and_the_announce_stands() {
        let (seen, engine) = prompting_engine();
        let built = bound_instance("prompt", Some(engine));
        let ctx = context(&built);
        live_session(ctx, "eng-prompt");
        let at = stamp(ctx);

        seed(ctx, &[("ci", Some("failure"))], &at);
        assert_eq!(announce_concluded(ctx).unwrap(), 1);

        // The engine refused the notice, so the Enter after it was never sent.
        let sent = seen.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].contains(r#""submit":false"#), "{}", sent[0]);
        let events = concluded_events(&built);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["sessions_notified"], 0);
    }

    #[test]
    fn a_stale_conclusion_is_never_announced() {
        let built = bound_instance("stale", None);
        let ctx = context(&built);
        seed(ctx, &[("ci", Some("success"))], "2000-01-01T00:00:00Z");
        assert_eq!(announce_concluded(ctx).unwrap(), 0);
        assert!(concluded_events(&built).is_empty());
    }

    /// One run of workflow `ci` on `wi-1`, still going or concluded, started
    /// `started` (an RFC 3339 stamp); sealed as one sweep.
    fn seed_run(
        ctx: &AppContext<StepClock, SequentialIds>,
        conclusion: Option<&str>,
        started: &str,
        status: &str,
    ) {
        let project_id = ctx
            .declared
            .read()
            .unwrap()
            .list_projects(10, 0)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .id;
        let at = moment_now(ctx);
        let finding = PendingObservation {
            kind: KIND_CHECK.to_owned(),
            subject_key: format!("ci:acme/app@{}:ci@wi-1", "b".repeat(40)),
            payload: json!({
                "revision": "b".repeat(40),
                "check": "ci",
                "conclusion": conclusion,
                "status": if conclusion.is_some() { "completed" } else { status },
                "branch": "wi-1",
                "event": "push",
                "updated_at": started,
                "run_started_at": started,
                "run_id": 4242,
                "run_attempt": 1,
                "repo": "acme/app",
            }),
            content_digest: format!("run-{conclusion:?}-{started}"),
            project_id: Some(project_id.clone()),
            source_url: Some("https://github.com/acme/app/actions/runs/4242".to_owned()),
            promoted: false,
        };
        let row = ctx
            .observed
            .begin_sweep(COLLECTOR_CHECKS, std::slice::from_ref(&project_id), at)
            .unwrap();
        ctx.observed.append(&row.id, &[finding], at).unwrap();
        ctx.observed
            .finish_sweep(&row.id, SweepOutcome::Ok, &[("projects", 1)], at, None)
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
    }

    fn minutes_ago(ctx: &AppContext<StepClock, SequentialIds>, minutes: i64) -> String {
        chrono::DateTime::from_timestamp(moment_now(ctx).unix_seconds() - minutes * 60, 0)
            .unwrap()
            .to_rfc3339()
    }

    fn hung_rows(built: &Built) -> Vec<Value> {
        let listed = crate::application::services::inbox::inbox_list_op(built, json!({})).unwrap();
        listed["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry["kind"] == "ci.run_hung")
            .collect()
    }

    fn hung_events(built: &Built) -> Vec<Value> {
        let ctx = context(built);
        let view = ctx.declared.read().unwrap();
        view.list_events(0, 500, None)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == CI_RUN_HUNG_EVENT)
            .map(|event| event.summary)
            .collect()
    }

    #[test]
    fn a_hung_run_is_one_inbox_row_and_one_notice_until_it_concludes() {
        let (seen, engine) = recording_engine();
        let built = bound_instance("hung", Some(engine));
        let ctx = context(&built);
        live_session(ctx, "eng-hung");

        seed_run(ctx, None, &minutes_ago(ctx, 10), "in_progress");
        assert!(hung_rows(&built).is_empty(), "10 min is inside the limit");

        seed_run(ctx, None, &minutes_ago(ctx, 120), "in_progress");
        let rows = hung_rows(&built);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["work_item_ref"], "WI-1");
        assert_eq!(
            rows[0]["source_url"],
            "https://github.com/acme/app/actions/runs/4242"
        );
        let summary = rows[0]["summary"].as_str().unwrap();
        assert!(summary.contains("gh run cancel 4242"), "{summary}");
        assert!(summary.contains("min, since"), "{summary}");
        assert_eq!(rows[0]["entry_key"], "ci:hung:run:acme/app:4242.1");

        announce_concluded(ctx).unwrap();
        announce_concluded(ctx).unwrap();
        let sent = seen.lock().unwrap().clone();
        let typed: Vec<&String> = sent
            .iter()
            .filter(|request| request.contains("[vogt] CI run 4242 (ci)"))
            .collect();
        assert_eq!(typed.len(), 1, "{sent:?}");
        assert!(typed[0].contains("it may be hung"), "{}", typed[0]);
        let events = hung_events(&built);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["sessions_notified"], 1);
        assert_eq!(events[0]["phase"], "running");

        seed_run(ctx, Some("success"), &minutes_ago(ctx, 120), "in_progress");
        assert!(hung_rows(&built).is_empty(), "gone once the run concludes");
    }

    #[test]
    fn a_queued_run_uses_the_queued_limit() {
        let built = bound_instance("hung-queued", None);
        let ctx = context(&built);
        seed_run(ctx, None, &minutes_ago(ctx, 35), "queued");
        assert_eq!(hung_rows(&built).len(), 1, "queued past 30 min");
        seed_run(ctx, None, &minutes_ago(ctx, 35), "in_progress");
        assert!(hung_rows(&built).is_empty(), "running is allowed 90 min");
    }

    #[test]
    fn zero_minutes_turns_a_phase_off() {
        let config = VogtConfig {
            ci_hung_queued_after_minutes: 0,
            ..VogtConfig::default()
        };
        let thresholds = hung_thresholds(&config);
        assert_eq!(thresholds.queued, None);
        assert_eq!(thresholds.running, Some(90 * 60));
    }
}
