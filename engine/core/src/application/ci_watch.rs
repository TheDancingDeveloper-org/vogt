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
//! "Once" is the event: its entity id names the item, branch, revision and
//! verdict, so a conclusion that already has an event is never re-announced,
//! not by the next sweep and not by a restart. Only fresh conclusions (the last
//! run finished within [`FRESH_WINDOW_SECONDS`]) are announced, so binding an
//! old branch never floods a session with history.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::adapters::engine::EngineClient;
use crate::adapters::forge::KIND_CHECK;
use crate::application::context::AppContext;
use crate::core::{Clock, IdFactory, Moment, Observation, WorkOverlay, TERMINAL_STATES};
use crate::decisions::{self, BranchCi, BranchState};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

/// `ci_watch.py` `CI_BRANCH_CONCLUDED_EVENT`.
pub const CI_BRANCH_CONCLUDED_EVENT: &str = "ci.branch_concluded";
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

/// CI on the newest revision of every branch an open item is bound to.
///
/// `checks_by_branch` is keyed `(project_id, branch)`, built once from the
/// checks the caller already holds, so nothing here re-reads observations per
/// branch. An item's state is read only for an overlay with observed runs.
pub fn bound_branches(
    view: &impl ReadView,
    checks_by_branch: &BTreeMap<(String, String), Vec<Observation>>,
) -> Result<Vec<BoundBranch>, VogtError> {
    let mut found = Vec::new();
    for overlay in view.bound_branch_overlays(MAX_BOUND)? {
        let settled_branches = settled_on(&overlay, checks_by_branch);
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
) -> Vec<(String, BranchCi)> {
    let mut found = Vec::new();
    for branch in &overlay.branches {
        let Some(runs) = checks_by_branch.get(&(overlay.project_id.clone(), branch.clone())) else {
            continue;
        };
        if let Some(ci) = decisions::branch_ci(runs, branch) {
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

/// The one line typed into a bound session. Plain text, no control characters:
/// it is data for the agent, not a command.
pub fn notice_text(bound: &BoundBranch) -> String {
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
    let line = format!(
        "[vogt] CI {verdict} on {} @ {short} ({}){detail}{tail}",
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
        bound_branches(&view, &index_by_branch(&checks))?
    };
    let mut announced = 0;
    for bound in found {
        let now = moment_now(ctx).unix_seconds();
        if !settled(&bound.ci) || !fresh(bound.ci.concluded_at.as_deref(), now) {
            continue;
        }
        let entity = bound.event_entity();
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
        let nudged = nudge(
            ctx.engine.as_ref(),
            ctx.config.ci_watch_notify_sessions,
            &sessions,
            &bound,
        )?;
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
fn nudge(
    engine: Option<&EngineClient>,
    notify: bool,
    engine_ids: &[String],
    bound: &BoundBranch,
) -> Result<usize, VogtError> {
    let Some(engine) = engine.filter(|_| notify) else {
        return Ok(0);
    };
    let text = notice_text(bound);
    let mut sent = 0;
    for engine_id in engine_ids {
        // The text, then the Enter that submits it. The Enter is sent only when
        // the text was taken.
        let delivered = || -> Result<bool, VogtError> {
            Ok(engine.send_input(engine_id, &text, false, false)?
                && engine.send_input(engine_id, "", true, false)?)
        };
        match delivered() {
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
    use crate::core::{ActorKind, Moment, Principal, SequentialIds, StepClock, SweepOutcome};
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

    fn settled_ci(state: BranchState, failing: &[&str]) -> BranchCi {
        BranchCi {
            branch: "wi-9".to_owned(),
            revision: "a".repeat(40),
            state,
            runs: Vec::new(),
            failing: failing.iter().map(|name| (*name).to_owned()).collect(),
            concluded_at: Some("2026-10-04T00:00:00Z".to_owned()),
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
        let text = notice_text(&BoundBranch {
            work_item_ref: "WI-9".to_owned(),
            work_item_id: None,
            project_id: "prj_1".to_owned(),
            branch: "wi-9".to_owned(),
            ci: settled_ci(BranchState::Passed, &[]),
        });
        assert!(
            text.starts_with("[vogt] CI PASSED on wi-9 @ aaaaaaaaaaaa (WI-9)"),
            "{text}"
        );
        assert!(!text.contains('\n') && !text.contains('\r'));

        let hostile = notice_text(&BoundBranch {
            work_item_ref: "WI-9".to_owned(),
            work_item_id: None,
            project_id: "prj_1".to_owned(),
            branch: "wi-9".to_owned(),
            ci: settled_ci(BranchState::Failed, &["ci\u{1b}[2J\rrm -rf"]),
        });
        assert!(
            !hostile.contains('\u{1b}') && !hostile.contains('\r'),
            "{hostile:?}"
        );
        assert!(hostile.chars().count() <= NOTICE_MAX_CHARS);
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
    fn a_stale_conclusion_is_never_announced() {
        let built = bound_instance("stale", None);
        let ctx = context(&built);
        seed(ctx, &[("ci", Some("success"))], "2000-01-01T00:00:00Z");
        assert_eq!(announce_concluded(ctx).unwrap(), 0);
        assert!(concluded_events(&built).is_empty());
    }
}
