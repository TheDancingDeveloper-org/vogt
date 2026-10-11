//! Assembling a work item's git story — branch, PR, phase — with provenance.
//! Ports `services/branches.py` and `services/git_story.py`.
//!
//! Two collected facts meet here: the branches bound to the item and the pull
//! request whose `implements` edges name it. Read back together they answer
//! where the item is in git. The derivation itself is pure
//! (`crate::git_story`); this module only fetches the observations and dresses
//! each fact with its provenance and age.
//!
//! Everything here is derived and read-only: nothing is written onto the item,
//! and the phase never competes with the workflow state. `work.get` is not
//! ported, so these assemblers are not yet called from an operation; they are
//! what that read will call.

use serde_json::{json, Value};

use crate::application::context::AppContext;
use crate::application::git_signals::forge_number;
use crate::core::{Clock, IdFactory, Moment, Observation, WorkItem, TERMINAL_STATES};
use crate::errors::VogtError;
use crate::git_story::{derive_drift, derive_phase, derive_pr_state, GitPhase, PrState};
use crate::storage::interface::{ObservedStore, ReadView};

/// Which derived PR state defines the item when more than one PR names it: a
/// merged PR is the definitive answer, a live one outranks a draft, and a dead
/// one is the last resort.
fn state_rank(state: &str) -> i64 {
    match state {
        "merged" => 5,
        "in-review" => 4,
        "open" => 3,
        "draft" => 2,
        "closed" => 1,
        _ => 0,
    }
}

/// Every branch bound to `item`, declared and observed, kept separate.
///
/// A branch present on one side and not the other is drift: `source` says
/// which side it came from rather than averaging the two away. Age is derived
/// from the observation's stable `last_commit_at` at read time.
pub fn branch_views_for<C, I>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    item: &WorkItem,
) -> Result<Vec<Value>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let overlay = view.work_overlay(&item.reference)?;
    let declared: Vec<String> = overlay.map(|row| row.branches).unwrap_or_default();

    let mut observed: Vec<(String, Observation)> = Vec::new();
    if item.project_id.is_some() && ctx.observed.has_evidence_tables()? {
        let number = forge_number(&item.reference);
        let rows = ctx.observed.latest(
            &["git.branch".to_string()],
            item.project_id.as_deref(),
            false,
            false,
            1000,
        )?;
        for observation in rows {
            let Some(name) = observation
                .payload
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            if binds(&observation, &item.reference, number) {
                observed.push((name.to_string(), observation));
            }
        }
    }

    let now = super::services::now_of(&ctx.clock);
    let mut views = Vec::new();
    for name in &declared {
        let observation = observed.iter().find(|(found, _)| found == name);
        views.push(branch_view(
            name,
            true,
            observation.map(|(_, row)| row),
            now,
        ));
    }
    for (name, observation) in &observed {
        if declared.iter().any(|declared| declared == name) {
            continue;
        }
        views.push(branch_view(name, false, Some(observation), now));
    }
    Ok(views)
}

/// Whether this branch observation is for `work_ref`: a vogt ref matches
/// directly, and a forge number matches the item's own issue number, which is
/// how a `gh-264-…` branch reaches the upstream item whose subject ends `#264`.
fn binds(observation: &Observation, work_ref: &str, forge_number: Option<i64>) -> bool {
    if observation
        .payload
        .get("work_item_ref")
        .and_then(Value::as_str)
        == Some(work_ref)
    {
        return true;
    }
    forge_number.is_some_and(|number| {
        observation
            .payload
            .get("forge_number")
            .and_then(Value::as_i64)
            == Some(number)
    })
}

fn branch_view(
    name: &str,
    declared: bool,
    observation: Option<&Observation>,
    now: Moment,
) -> Value {
    let source = match (observation.is_some(), declared) {
        (false, _) => "declared",
        (true, true) => "both",
        (true, false) => "observed",
    };
    let payload = observation.map(|row| &row.payload);
    let last_commit_at = payload
        .and_then(|payload| payload.get("last_commit_at"))
        .and_then(Value::as_str)
        .and_then(|text| crate::core::from_iso(text).ok());
    json!({
        "name": name,
        "source": source,
        "drift": source != "both",
        "tip": opt_str(payload, "tip"),
        "ahead": opt_int(payload, "ahead"),
        "behind": opt_int(payload, "behind"),
        "default_branch": opt_str(payload, "default_branch"),
        "last_commit_at": last_commit_at.map(Moment::to_json),
        "last_commit_age_seconds": age(last_commit_at, now),
        "observed_at": observation.map(|row| row.observed_at.to_json()),
    })
}

/// The derived git story for `item`, or `None` when there is no evidence.
///
/// Reuses the branch views already assembled rather than re-reading them, finds
/// the pull request that implements the item, derives the phase and the drift,
/// and stamps each fact with its freshness. No git evidence at all is not a
/// story: an item nobody has started in git gets no block rather than a
/// `no_branch` one nobody asked for.
pub fn git_story_for<C, I>(
    ctx: &AppContext<C, I>,
    item: &WorkItem,
    branches: &[Value],
) -> Result<Option<Value>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    let now = super::services::now_of(&ctx.clock);
    let pull = pull_request_for(ctx, item, now)?;
    let has_branch = !branches.is_empty();
    let has_active_branch = branches.iter().any(|branch| {
        matches!(
            branch.get("source").and_then(Value::as_str),
            Some("observed" | "both")
        )
    });
    if !has_branch && pull.is_none() {
        return Ok(None);
    }
    let pr_state = pull
        .as_ref()
        .and_then(|pull| pull.get("state").and_then(Value::as_str).map(pr_state_of));
    let phase = match derive_phase(has_branch, pr_state) {
        GitPhase::NoBranch => "no_branch",
        GitPhase::BranchActive => "branch_active",
        GitPhase::PrOpen => "pr_open",
        GitPhase::InReview => "in_review",
        GitPhase::Merged => "merged",
    };
    let drift: Vec<Value> = derive_drift(
        TERMINAL_STATES.contains(&item.state.as_str()),
        pr_state,
        has_active_branch,
    )
    .into_iter()
    .map(|(code, message)| {
        json!({
            "code": code,
            "message": message,
            "provenance": drift_provenance(code, pull.as_ref()),
        })
    })
    .collect();
    Ok(Some(json!({
        "phase": phase,
        "workflow_state": item.state,
        "branches": branches,
        "pull_request": pull,
        "drift": drift,
        // The task-run conclusion is an engine-side record not surfaced through
        // the work item, so it does not feed the phase yet.
        "task_conclusion_available": false,
    })))
}

/// The pull request that implements `item`, if one was observed. When several
/// name it, the defining one wins: the higher derived state, then the later
/// observation.
fn pull_request_for<C, I>(
    ctx: &AppContext<C, I>,
    item: &WorkItem,
    now: Moment,
) -> Result<Option<Value>, VogtError>
where
    C: Clock,
    I: IdFactory,
{
    if item.project_id.is_none() || !ctx.observed.has_evidence_tables()? {
        return Ok(None);
    }
    let number = forge_number(&item.reference);
    let rows = ctx.observed.latest(
        &["forge.pull_request".to_string()],
        item.project_id.as_deref(),
        false,
        false,
        1000,
    )?;
    let mut best: Option<Value> = None;
    let mut best_key: Option<(i64, Moment)> = None;
    for observation in &rows {
        let Some(provenance) = edge_provenance(observation, &item.reference, number) else {
            continue;
        };
        let view = pull_view(observation, provenance.as_deref(), now);
        let observed = observation.observed_at;
        let key = (
            state_rank(view.get("state").and_then(Value::as_str).unwrap_or("")),
            observed,
        );
        if best_key.is_none_or(|kept| key > kept) {
            best = Some(view);
            best_key = Some(key);
        }
    }
    Ok(best)
}

/// Whether — and by what provenance — this PR implements `work_ref`. A
/// subject-key match binds directly, and a same-repo edge whose number is the
/// item's own issue number binds a `gh-<n>` branch's PR to the upstream item.
/// `None` when the PR names other work, never a guess.
fn edge_provenance(
    observation: &Observation,
    work_ref: &str,
    forge_number: Option<i64>,
) -> Option<Option<String>> {
    let edges = observation
        .payload
        .get("implements")
        .and_then(Value::as_array)?;
    for edge in edges {
        let subject_match = edge.get("subject").and_then(Value::as_str) == Some(work_ref);
        let number_match = forge_number
            .is_some_and(|number| edge.get("number").and_then(Value::as_i64) == Some(number));
        if subject_match || number_match {
            return Some(nonempty_str(edge.get("provenance")));
        }
    }
    None
}

fn pull_view(observation: &Observation, provenance: Option<&str>, now: Moment) -> Value {
    let payload = &observation.payload;
    let review_decision = nonempty_str(payload.get("review_state"));
    let state = derive_pr_state(
        nonempty_str(payload.get("state")).as_deref(),
        payload
            .get("draft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        review_decision.as_deref(),
    );
    let updated_at = nonempty_str(payload.get("updated_at"))
        .as_deref()
        .and_then(|text| crate::core::from_iso(text).ok());
    json!({
        "number": payload.get("number").and_then(Value::as_i64).unwrap_or(0),
        "state": pr_state_text(state),
        "title": nonempty_str(payload.get("title")),
        "url": observation.source_url.clone(),
        "draft": payload.get("draft").and_then(Value::as_bool).unwrap_or(false),
        "review_decision": review_decision,
        "checks": nonempty_str(payload.get("checks")),
        "mergeable": nonempty_str(payload.get("mergeable")),
        "head_ref": nonempty_str(payload.get("head_ref")),
        "base": nonempty_str(payload.get("base")),
        "provenance": provenance,
        "updated_at": updated_at.map(Moment::to_json),
        "updated_age_seconds": age(updated_at, now),
        "observed_at": observation.observed_at.to_json(),
        "observed_age_seconds": age(Some(observation.observed_at), now),
    })
}

fn pr_state_of(text: &str) -> PrState {
    match text {
        "draft" => PrState::Draft,
        "in-review" => PrState::InReview,
        "merged" => PrState::Merged,
        "closed" => PrState::Closed,
        _ => PrState::Open,
    }
}

fn pr_state_text(state: PrState) -> &'static str {
    match state {
        PrState::Draft => "draft",
        PrState::Open => "open",
        PrState::InReview => "in-review",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    }
}

/// Where a drift finding was read from, for a reader to check it against.
fn drift_provenance(code: &str, pull: Option<&Value>) -> Option<String> {
    if code == "active_branch_done_item" {
        return Some("local checkout (git-local sweep)".to_string());
    }
    pull.and_then(|pull| pull.get("number").and_then(Value::as_i64))
        .map(|number| format!("forge PR #{number}"))
}

/// Age in whole seconds, floored at zero. A naive timestamp — no timezone — is
/// not an age, matching Python's refusal to subtract an unaware datetime.
fn age(moment: Option<Moment>, now: Moment) -> Option<i64> {
    moment.map(|moment| (now.unix_seconds() - moment.unix_seconds()).max(0))
}

/// A non-empty string. An empty one and any non-string read as absent, which is
/// what Python's `_str` does.
fn nonempty_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn opt_str(payload: Option<&Value>, key: &str) -> Option<String> {
    payload
        .and_then(|payload| payload.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn opt_int(payload: Option<&Value>, key: &str) -> Option<i64> {
    payload
        .and_then(|payload| payload.get(key))
        .and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ActorKind, Origin, Principal, Priority, SequentialIds, TrustState};
    use crate::storage::interface::{DeclaredStore, ObservedStore, WriteTxn};
    use crate::storage::observed_types::PendingObservation;

    fn moment() -> Moment {
        Moment::from_unix(1_700_000_000, 0)
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    /// A clock that always reads the same instant. Ages are the gap between a
    /// stored stamp and this read, so a clock that moved would make the gap depend
    /// on how many reads happened first.
    struct FixedClock(Moment);

    impl Clock for FixedClock {
        fn now(&mut self) -> Moment {
            self.0
        }
    }

    fn opened() -> AppContext<FixedClock, SequentialIds> {
        let dir = std::env::temp_dir().join(format!(
            "vogt-git-story-{}-{}",
            std::process::id(),
            unique()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir.clone(),
            ..crate::config::VogtConfig::default()
        };
        // The seeded stamps and the clock read the same instant, so every derived
        // age is zero however many times the clock is read.
        let clock = std::sync::Arc::new(std::sync::Mutex::new(FixedClock(moment())));
        let ids = std::sync::Arc::new(std::sync::Mutex::new(SequentialIds::new(None).unwrap()));
        let ctx = AppContext {
            declared: crate::storage::sqlite::declared::SqliteDeclaredStore::shared(
                config.declared_db_path(),
                std::sync::Arc::clone(&clock),
                std::sync::Arc::clone(&ids),
                config.sqlite_synchronous.as_str(),
            ),
            observed: crate::storage::sqlite::observed::SqliteObservedStore::shared(
                config.observed_db_path(),
                std::sync::Arc::clone(&clock),
                std::sync::Arc::clone(&ids),
                config.sqlite_synchronous.as_str(),
            ),
            principal: Principal::new("local:test-user", ActorKind::Human, "Test").unwrap(),
            token: None,
            clock,
            id_factory: ids,
            cloner: crate::adapters::git::clone_repository,
            pusher: crate::adapters::git::push_branch,
            engine: None,
            peer: None,
            public_identity: crate::application::context::PublicIdentity::default(),
            config,
        };
        ctx.declared.migrate().unwrap();
        ctx.declared
            .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
            .unwrap();
        ctx.observed.migrate().unwrap();
        ctx
    }

    fn item(reference: &str, project_id: Option<&str>, state: &str) -> WorkItem {
        WorkItem {
            id: "wrk_1".to_string(),
            reference: reference.to_string(),
            kind: crate::core::WorkKind::Feature,
            title: "In flight".to_string(),
            body: String::new(),
            state: state.to_string(),
            priority: Priority::P2,
            effort: None,
            project_id: project_id.map(str::to_string),
            project_slug: None,
            initiative_id: None,
            origin: Origin::Created,
            trust_state: TrustState::Unverified,
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: moment(),
            updated_at: moment(),
        }
    }

    fn seed(ctx: &AppContext<FixedClock, SequentialIds>, finding: PendingObservation) {
        let sweep = ctx.observed.begin_sweep("test", &[], moment()).unwrap();
        ctx.observed
            .append(&sweep.id, &[finding], moment())
            .unwrap();
        ctx.observed
            .finish_sweep(
                &sweep.id,
                crate::core::SweepOutcome::Ok,
                &[],
                moment(),
                None,
            )
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
    }

    fn seed_branch(
        ctx: &AppContext<FixedClock, SequentialIds>,
        project: &str,
        name: &str,
        work_ref: &str,
    ) {
        seed(
            ctx,
            PendingObservation {
                kind: "git.branch".to_string(),
                subject_key: format!("branch:{project}:{name}"),
                payload: json!({
                    "name": name,
                    "work_item_ref": work_ref,
                    "tip": "deadbeef",
                    "ahead": 1,
                    "behind": 0,
                    "default_branch": "main",
                    "last_commit_at": "2023-11-14T22:13:20+00:00",
                }),
                content_digest: format!("branch-{name}"),
                project_id: Some(project.to_string()),
                source_url: None,
                promoted: false,
            },
        );
    }

    fn seed_pr(
        ctx: &AppContext<FixedClock, SequentialIds>,
        project: &str,
        number: i64,
        subject: &str,
        state: &str,
        review_state: Option<&str>,
    ) {
        seed(
            ctx,
            PendingObservation {
                kind: "forge.pull_request".to_string(),
                subject_key: format!("gh:acme/app#{number}"),
                payload: json!({
                    "number": number,
                    "title": format!("PR {number}"),
                    "state": state,
                    "draft": false,
                    "head_ref": "wi-1",
                    "base": "main",
                    "review_state": review_state,
                    "checks": "success",
                    "implements": [{"subject": subject, "number": number, "provenance": "from PR body"}],
                    "updated_at": "2023-11-14T22:13:20+00:00",
                }),
                content_digest: format!("pr-{number}-{state}"),
                project_id: Some(project.to_string()),
                source_url: Some(format!("https://github.com/acme/app/pull/{number}")),
                promoted: false,
            },
        );
    }

    #[test]
    fn no_evidence_is_no_story() {
        let ctx = opened();
        let item = item("WI-1", Some("prj_1"), "open");
        let view = ctx.declared.read().unwrap();
        let branches = branch_views_for(&ctx, &view, &item).unwrap();
        assert!(branches.is_empty());
        assert!(git_story_for(&ctx, &item, &branches).unwrap().is_none());
    }

    #[test]
    fn an_observed_branch_and_an_open_pr_make_a_pr_open_story() {
        let ctx = opened();
        seed_branch(&ctx, "prj_1", "wi-1", "WI-1");
        seed_pr(&ctx, "prj_1", 7, "WI-1", "open", None);
        let item = item("WI-1", Some("prj_1"), "in_progress");
        let view = ctx.declared.read().unwrap();
        let branches = branch_views_for(&ctx, &view, &item).unwrap();
        let story = git_story_for(&ctx, &item, &branches).unwrap().unwrap();
        assert_eq!(story["phase"], "pr_open");
        assert_eq!(story["workflow_state"], "in_progress");
        assert_eq!(story["task_conclusion_available"], false);
        // The branch carries its derived age: the commit stamp and the clock read
        // the same instant, so the age is zero.
        assert_eq!(branches[0]["source"], "observed");
        assert_eq!(branches[0]["drift"], true);
        assert_eq!(branches[0]["last_commit_age_seconds"], 0);
        assert_eq!(branches[0]["tip"], "deadbeef");
        let pull = &story["pull_request"];
        assert_eq!(pull["number"], 7);
        assert_eq!(pull["state"], "open");
        assert_eq!(pull["checks"], "success");
        assert_eq!(pull["url"], "https://github.com/acme/app/pull/7");
        assert_eq!(pull["provenance"], "from PR body");
        assert_eq!(pull["observed_age_seconds"], 0);
        assert!(story["drift"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_reviewed_pr_reads_in_review() {
        let ctx = opened();
        seed_pr(&ctx, "prj_1", 8, "WI-1", "open", Some("changes_requested"));
        let item = item("WI-1", Some("prj_1"), "in_progress");
        let story = git_story_for(&ctx, &item, &[]).unwrap().unwrap();
        assert_eq!(story["phase"], "in_review");
        assert_eq!(story["pull_request"]["state"], "in-review");
    }

    #[test]
    fn a_merged_pr_under_an_open_item_is_drift() {
        let ctx = opened();
        seed_pr(&ctx, "prj_1", 9, "WI-1", "merged", None);
        let item = item("WI-1", Some("prj_1"), "in_progress");
        let story = git_story_for(&ctx, &item, &[]).unwrap().unwrap();
        assert_eq!(story["phase"], "merged");
        assert_eq!(story["drift"][0]["code"], "merged_pr_open_item");
        assert_eq!(story["drift"][0]["provenance"], "forge PR #9");
    }

    #[test]
    fn a_done_item_with_a_live_branch_and_an_open_pr_carries_both_drifts() {
        let ctx = opened();
        seed_branch(&ctx, "prj_1", "wi-1", "WI-1");
        seed_pr(&ctx, "prj_1", 10, "WI-1", "open", None);
        let item = item("WI-1", Some("prj_1"), "done");
        let view = ctx.declared.read().unwrap();
        let branches = branch_views_for(&ctx, &view, &item).unwrap();
        let story = git_story_for(&ctx, &item, &branches).unwrap().unwrap();
        let codes: Vec<&str> = story["drift"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["code"].as_str().unwrap())
            .collect();
        assert_eq!(
            codes,
            vec!["closed_item_open_pr", "active_branch_done_item"]
        );
        assert_eq!(
            story["drift"][1]["provenance"],
            "local checkout (git-local sweep)"
        );
    }

    #[test]
    fn the_defining_pr_wins_when_several_name_the_item() {
        let ctx = opened();
        // The merged one is recorded first, so recency alone would not pick it;
        // the state rank must.
        seed_pr(&ctx, "prj_1", 3, "WI-1", "merged", None);
        seed_pr(&ctx, "prj_1", 4, "WI-1", "open", None);
        let item = item("WI-1", Some("prj_1"), "in_progress");
        let story = git_story_for(&ctx, &item, &[]).unwrap().unwrap();
        assert_eq!(story["pull_request"]["number"], 3);
        assert_eq!(story["phase"], "merged");
    }

    #[test]
    fn a_pr_for_other_work_does_not_bind() {
        let ctx = opened();
        seed_pr(&ctx, "prj_1", 5, "WI-2", "open", None);
        let item = item("WI-1", Some("prj_1"), "open");
        assert!(git_story_for(&ctx, &item, &[]).unwrap().is_none());
    }

    #[test]
    fn a_forge_number_binds_a_branch_and_a_pr_to_an_upstream_item() {
        let ctx = opened();
        seed(
            &ctx,
            PendingObservation {
                kind: "git.branch".to_string(),
                subject_key: "branch:prj_1:gh-12".to_string(),
                payload: json!({"name": "gh-12-widget", "forge_number": 12, "tip": "abc"}),
                content_digest: "branch-gh-12".to_string(),
                project_id: Some("prj_1".to_string()),
                source_url: None,
                promoted: false,
            },
        );
        seed(
            &ctx,
            PendingObservation {
                kind: "forge.pull_request".to_string(),
                subject_key: "gh:acme/app#6".to_string(),
                payload: json!({
                    "number": 6,
                    "state": "open",
                    "implements": [{"subject": "gh:acme/app#99", "number": 12, "provenance": "from branch"}],
                }),
                content_digest: "pr-6".to_string(),
                project_id: Some("prj_1".to_string()),
                source_url: None,
                promoted: false,
            },
        );
        let item = item("gh:acme/app#12", Some("prj_1"), "open");
        let view = ctx.declared.read().unwrap();
        let branches = branch_views_for(&ctx, &view, &item).unwrap();
        assert_eq!(branches[0]["name"], "gh-12-widget");
        let story = git_story_for(&ctx, &item, &branches).unwrap().unwrap();
        assert_eq!(story["pull_request"]["number"], 6);
        assert_eq!(story["pull_request"]["provenance"], "from branch");
        assert_eq!(story["phase"], "pr_open");
    }

    #[test]
    fn a_declared_branch_with_no_observation_is_declared_drift() {
        let ctx = opened();
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_project(&crate::core::Project::new(
            "prj_1",
            "app",
            "App",
            "/srv/app",
            moment(),
        ))
        .unwrap();
        txn.upsert_work_overlay(&crate::core::WorkOverlay {
            subject_key: "WI-1".to_string(),
            project_id: "prj_1".to_string(),
            rank: None,
            workflow_state: None,
            priority: None,
            effort: None,
            assignee_actor_id: None,
            initiative_id: None,
            branches: vec!["feature".to_string()],
            created_at: moment(),
            updated_at: moment(),
        })
        .unwrap();
        txn.commit().unwrap();
        let item = item("WI-1", Some("prj_1"), "open");
        let view = ctx.declared.read().unwrap();
        let branches = branch_views_for(&ctx, &view, &item).unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0]["source"], "declared");
        assert_eq!(branches[0]["drift"], true);
        assert!(branches[0]["observed_at"].is_null());
        let story = git_story_for(&ctx, &item, &branches).unwrap().unwrap();
        assert_eq!(story["phase"], "branch_active");
    }
}
