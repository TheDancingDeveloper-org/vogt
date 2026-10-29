//! Ports the cases in `tests/test_sessions.py` that stand on the service:
//! a session opens in the project's checkout, a blank reason never reaches
//! the engine, both subjects are refused, a task lands in the brief, stopping
//! revokes the token, and a dead engine is reported rather than rendered as
//! an empty list.

use std::sync::{Arc, Mutex};

use super::sessions::SessionBudget;
use super::sessions::{
    list_sessions, start_session, stop_session, ListSessionsParams, StartSessionParams,
    StopSessionParams,
};
use crate::adapters::engine::EngineClient;
use crate::application::context::{build_context, Built};
use crate::core::{
    ActorKind, ActorPreference, LinkState, Moment, Origin, Principal, Priority, Project,
    SequentialIds, StepClock, TrustState, WorkItem, WorkKind,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WriteTxn};
use serde_json::json;

pub(super) fn moment() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// What the engine was asked, in order, and the body it answers with.
pub(super) struct Script {
    pub(super) seen: Mutex<Vec<String>>,
    /// Status and body for the next call. Empty means 200 with an empty object.
    answers: Mutex<Vec<(u16, String)>>,
}

fn engine(script: Arc<Script>) -> EngineClient {
    let transport: crate::adapters::engine::Transport =
        Arc::new(move |url, _headers, body, method| {
            script
                .seen
                .lock()
                .unwrap()
                .push(format!("{method} {url} {}", String::from_utf8_lossy(body)));
            script
                .answers
                .lock()
                .unwrap()
                .pop()
                .map(|(status, body)| (status, body.into_bytes()))
                .unwrap_or((200, b"{}".to_vec()))
        });
    EngineClient::new("http://engine", Some("tok".to_string()), Some(transport))
}

pub(super) fn opened(script: Option<Arc<Script>>) -> Built {
    opened_as(
        script,
        Principal::new("local:test-user", ActorKind::Human, "Test").unwrap(),
    )
}

fn opened_as(script: Option<Arc<Script>>, principal: Principal) -> Built {
    let dir = std::env::temp_dir().join(format!("vogt-ses-{}-{}", std::process::id(), unique()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir,
        ..crate::config::VogtConfig::default()
    };
    let client = script.map(engine);
    let built = build_context(
        config,
        Some(principal),
        Some(StepClock::new(moment())),
        Some(SequentialIds::new(None).unwrap()),
        None,
        client,
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    ctx.declared.migrate().unwrap();
    ctx.declared
        .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
        .unwrap();
    ctx.observed.migrate().unwrap();
    built
}

pub(super) fn insert_project(built: &Built, slug: &str) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut project = Project::new(&format!("prj_{slug}"), slug, slug, "/work/tree", moment());
    project.link_state = LinkState::Unlinked;
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    txn.commit().unwrap();
}

pub(super) fn start_params(
    project: Option<&str>,
    task: Option<&str>,
    reason: &str,
) -> StartSessionParams {
    StartSessionParams {
        project: project.map(str::to_string),
        work_item: None,
        name: None,
        template: None,
        task: task.map(str::to_string),
        reason: reason.to_string(),
        model: None,
        effort: None,
        resume: None,
        permission_mode: "default".to_string(),
        autopilot: None,
        role: "worker".to_string(),
        purpose: None,
        review_of: None,
        from_handoff: None,
        budget: None,
        worktree: None,
        repo_map: None,
        explicit_null: Vec::new(),
    }
}

pub(super) fn script_with(answers: Vec<(u16, String)>) -> Arc<Script> {
    Arc::new(Script {
        seen: Mutex::new(Vec::new()),
        answers: Mutex::new(answers),
    })
}

#[test]
fn a_session_opens_in_the_projects_checkout() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-1","name":"beta","cwd":"/work/tree","activity":"idle"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let started = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    assert_eq!(started.session.cwd, "/work/tree");
    assert_eq!(started.session.project.as_deref(), Some("beta"));
    assert_eq!(started.session.engine_session_id, "eng-1");
    assert!(started.session.stopped_at.is_none());
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(asked.contains("POST"), "{asked}");
    assert!(asked.contains("/work/tree"), "{asked}");
}

#[test]
fn a_task_is_folded_into_the_brief_the_engine_receives() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-2","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(
        ctx,
        &start_params(Some("beta"), Some("check the containers"), "because"),
    )
    .unwrap();
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(
        asked.contains("check the containers"),
        "the task did not reach the engine: {asked}"
    );
}

#[test]
fn a_blank_reason_is_refused_before_the_engine_is_asked() {
    let script = script_with(vec![]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let error = start_session(ctx, &start_params(Some("beta"), None, "   ")).unwrap_err();
    assert!(matches!(error, VogtError::MissingReason(_)), "{error}");
    assert!(script.seen.lock().unwrap().is_empty());
}

#[test]
fn a_session_needs_exactly_one_subject() {
    let built = opened(Some(script_with(vec![])));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut both = start_params(Some("beta"), None, "because");
    both.work_item = Some("WI-1".to_string());
    let error = start_session(ctx, &both).unwrap_err();
    assert!(matches!(error, VogtError::InvalidRequest(ref text) if text.contains("at most one")));

    let error = start_session(ctx, &start_params(None, None, "because")).unwrap_err();
    assert!(
        matches!(error, VogtError::InvalidRequest(ref text) if text.contains("needs a project"))
    );
}

#[test]
fn with_no_engine_configured_starting_says_so() {
    let built = opened(None);
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let error = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap_err();
    assert!(matches!(error, VogtError::EngineUnavailable(_)), "{error}");
}

#[test]
fn the_engines_refusal_reaches_the_caller() {
    let script = script_with(vec![(
        403,
        r#"{"error":"forbidden: no such template"}"#.to_string(),
    )]);
    let built = opened(Some(script));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let error = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap_err();
    let text = error.to_string();
    assert!(
        text.contains("no such template") || text.contains("forbidden"),
        "{text}"
    );
}

#[test]
fn stopping_a_session_revokes_what_it_held() {
    let script = script_with(vec![
        (200, r#"{"killed":true}"#.to_string()),
        (
            200,
            r#"{"id":"eng-3","name":"beta","cwd":"/work/tree","activity":"idle"}"#.to_string(),
        ),
    ]);
    let built = opened(Some(script));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let started = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    let stopped = stop_session(
        ctx,
        &StopSessionParams {
            id: started.session.id.clone(),
            reason: "done".to_string(),
        },
    )
    .unwrap();
    assert!(stopped.session.stopped_at.is_some());
    let record = ctx
        .declared
        .read()
        .unwrap()
        .session_by_id(&started.session.id)
        .unwrap()
        .unwrap();
    let tokens = ctx
        .declared
        .read()
        .unwrap()
        .tokens_for_actor(&record.actor_id, true)
        .unwrap();
    assert!(!tokens.is_empty());
    assert!(tokens.iter().all(|token| token.revoked_at.is_some()));
}

#[test]
fn stopping_an_unknown_session_is_not_found() {
    let built = opened(Some(script_with(vec![])));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let error = stop_session(
        ctx,
        &StopSessionParams {
            id: "ses_missing".to_string(),
            reason: "done".to_string(),
        },
    )
    .unwrap_err();
    assert!(matches!(error, VogtError::NotFound(_)), "{error}");
}

#[test]
fn a_dead_engine_hides_no_linked_sessions() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-4","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    // The engine is now down: the next call fails.
    script
        .answers
        .lock()
        .unwrap()
        .push((500, r#"{"error":"engine down"}"#.to_string()));
    let listed = list_sessions(
        ctx,
        &ListSessionsParams {
            project: None,
            work_item: None,
            include_stopped: false,
            limit: 50,
            offset: 0,
            order: "started".to_string(),
            mode: "digest".to_string(),
            states: None,
            stall_after_minutes: 10,
            include_hidden: false,
            fields: None,
        },
    )
    .unwrap();
    // Digest mode is the default: the link is a digest row, not a summary.
    // A dead engine leaves liveness unknown, and unknown is a live digest state.
    assert!(listed.sessions.is_empty(), "{listed:?}");
    assert_eq!(listed.digests.len(), 1, "{listed:?}");
    assert!(listed.total >= 1, "{listed:?}");
    assert!(
        listed.engine.is_some(),
        "a dead engine must be reported, not rendered as a clean list"
    );
}

fn with_purpose(mut params: StartSessionParams, purpose: &str) -> StartSessionParams {
    params.purpose = Some(purpose.to_string());
    params
}

#[test]
fn a_purpose_fills_the_fields_the_start_left_unset() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-3","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let params = with_purpose(start_params(Some("beta"), None, "because"), "review");
    let started = start_session(ctx, &params).unwrap();
    assert_eq!(started.session.purpose.as_deref(), Some("review"));
    assert_eq!(started.session.model.as_deref(), Some("opus"));
    assert_eq!(started.session.template.as_deref(), Some("claude"));
    assert_eq!(started.session.effort.as_deref(), Some("low"));
    // A review profile is read-only, and the brief says so.
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(asked.contains("## Read only"), "{asked}");
}

#[test]
fn a_field_the_start_set_beats_the_profile() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-4","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = with_purpose(start_params(Some("beta"), None, "because"), "review");
    params.model = Some("haiku".to_string());
    let started = start_session(ctx, &params).unwrap();
    assert_eq!(started.session.model.as_deref(), Some("haiku"));
    assert_eq!(started.session.template.as_deref(), Some("claude"));
}

#[test]
fn a_person_stored_profile_overrides_the_code_default() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-5","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    {
        let human = ctx
            .declared
            .read()
            .unwrap()
            .actor_by_identity("local:test-user")
            .unwrap()
            .expect("the bootstrapped human");
        let mut txn = ctx.declared.write().unwrap();
        txn.upsert_actor_preference(&ActorPreference {
            actor_id: human.id,
            key: "dispatch.profiles".to_string(),
            value: json!({"review": {"model": "sonnet"}}),
            version: 1,
            updated_at: moment(),
        })
        .unwrap();
        txn.commit().unwrap();
    }
    let params = with_purpose(start_params(Some("beta"), None, "because"), "review");
    let started = start_session(ctx, &params).unwrap();
    assert_eq!(started.session.model.as_deref(), Some("sonnet"));
    assert_eq!(started.session.effort.as_deref(), Some("low"));
}

#[test]
fn an_unknown_purpose_is_refused_before_the_engine_is_asked() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let params = with_purpose(start_params(Some("beta"), None, "because"), "bogus");
    let error = start_session(ctx, &params).unwrap_err();
    assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    assert!(script.seen.lock().unwrap().is_empty());
}

#[test]
fn a_person_start_records_a_person_as_its_starter() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-6","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let started = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    assert_eq!(started.session.purpose, None);
    assert_eq!(started.session.started_by_kind.as_deref(), Some("person"));
    assert_eq!(started.session.started_by_session, None);
}

#[test]
fn a_worker_session_start_records_the_starting_session_and_its_role() {
    // The engine answers the caller's own lookup first, then the start.
    let script = script_with(vec![
        (
            200,
            r#"{"id":"eng-8","name":"beta","cwd":"/work/tree"}"#.to_string(),
        ),
        (
            200,
            r#"{"id":"eng-parent","name":"parent","cwd":"/work/tree","activity":"idle","alive":true,"autopilot":false,"autopilot_nudges":0,"keep_awake":false,"role":"oversight"}"#
                .to_string(),
        ),
    ]);
    let built = opened_as(
        Some(Arc::clone(&script)),
        Principal::new("agent:engine:eng-parent", ActorKind::Agent, "Parent").unwrap(),
    );
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let started = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    assert_eq!(
        started.session.started_by_session.as_deref(),
        Some("eng-parent")
    );
    assert_eq!(
        started.session.started_by_kind.as_deref(),
        Some("oversight")
    );
}

/// The summary of the `session.started` event: where the start's audited
/// facts (purpose, overrides, budget, worktree, repo map) are recorded.
fn session_start_audit(built: &Built) -> serde_json::Value {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let events = ctx
        .declared
        .read()
        .unwrap()
        .list_events(0, 100, None)
        .unwrap();
    events
        .into_iter()
        .find(|event| event.kind == "session.started")
        .expect("a session.started event")
        .summary
}

#[test]
fn a_profile_sets_budget_worktree_and_repo_map_and_the_audit_records_them() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-7","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let params = with_purpose(start_params(Some("beta"), None, "because"), "implement");
    start_session(ctx, &params).unwrap();
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(asked.contains(r#""turns":120"#), "{asked}");
    assert!(asked.contains(r#""wall_clock_minutes":90"#), "{asked}");
    assert!(asked.contains(r#""base_ref":"main""#), "{asked}");
    let audit = session_start_audit(&built);
    assert_eq!(audit["purpose"], "implement");
    assert_eq!(audit["overrides"], serde_json::json!([]));
    assert_eq!(audit["worktree"], true);
    assert_eq!(audit["repo_map"], true);
    assert_eq!(
        audit["budget"],
        serde_json::json!({"turns": 120, "wall_clock_minutes": 90})
    );
}

#[test]
fn a_field_the_caller_set_is_listed_as_an_override_in_the_audit() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-8","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = with_purpose(start_params(Some("beta"), None, "because"), "review");
    params.model = Some("haiku".to_string());
    params.budget = Some(SessionBudget {
        turns: Some(7),
        ..SessionBudget::default()
    });
    start_session(ctx, &params).unwrap();
    let audit = session_start_audit(&built);
    assert_eq!(audit["overrides"], serde_json::json!(["model", "budget"]));
    assert_eq!(audit["budget"], serde_json::json!({"turns": 7}));
    assert_eq!(audit["worktree"], false);
}

#[test]
fn a_worker_task_with_no_budget_takes_the_default_limits() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-9","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(Some("beta"), Some("check the containers"), "because");
    params.template = Some("claude".to_string());
    start_session(ctx, &params).unwrap();
    let audit = session_start_audit(&built);
    assert_eq!(
        audit["budget"],
        serde_json::json!({"turns": 120, "wall_clock_minutes": 90})
    );
    assert_eq!(audit["overrides"], serde_json::json!([]));
}

fn insert_work_item(built: &Built, project: &str, reference: &str) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_work_item(&WorkItem {
        id: format!("wrk_{reference}"),
        reference: reference.to_string(),
        kind: WorkKind::Feature,
        title: "The thing".to_string(),
        body: String::new(),
        state: "open".to_string(),
        priority: Priority::P2,
        effort: None,
        project_id: Some(format!("prj_{project}")),
        project_slug: Some(project.to_string()),
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
    })
    .unwrap();
    txn.commit().unwrap();
}

#[test]
fn a_review_start_records_the_resolved_item_it_reviews_in_the_audit() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-10","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    insert_work_item(&built, "beta", "WI-7");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = with_purpose(start_params(None, None, "because"), "review");
    params.review_of = Some("WI-7".to_string());
    start_session(ctx, &params).unwrap();
    let audit = session_start_audit(&built);
    assert_eq!(audit["review_of"], "WI-7");
    assert_eq!(audit["work_item"], "WI-7");
}

#[test]
fn a_start_with_no_purpose_records_null_worktree_and_repo_map_and_no_review() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-11","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(Some("beta"), Some("check the containers"), "because");
    params.template = Some("claude".to_string());
    start_session(ctx, &params).unwrap();
    let audit = session_start_audit(&built);
    assert_eq!(audit["worktree"], serde_json::Value::Null);
    assert_eq!(audit["repo_map"], serde_json::Value::Null);
    assert_eq!(audit["review_of"], serde_json::Value::Null);
}

#[test]
fn an_explicit_null_is_an_override_the_profile_does_not_fill() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-12","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = with_purpose(start_params(Some("beta"), None, "because"), "implement");
    params.explicit_null = vec!["worktree"];
    start_session(ctx, &params).unwrap();
    let audit = session_start_audit(&built);
    assert_eq!(audit["overrides"], serde_json::json!(["worktree"]));
    assert_eq!(audit["worktree"], serde_json::Value::Null);
    assert_eq!(audit["repo_map"], true);
}

fn digest_params() -> ListSessionsParams {
    ListSessionsParams {
        project: None,
        work_item: None,
        include_stopped: false,
        limit: 50,
        offset: 0,
        order: "started".to_string(),
        mode: "digest".to_string(),
        states: None,
        stall_after_minutes: 10,
        include_hidden: false,
        fields: None,
    }
}

fn listed_activities(body: &str) -> Vec<Option<String>> {
    let script = script_with(vec![(200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let listed = list_sessions(ctx, &digest_params()).unwrap();
    listed
        .digests
        .iter()
        .map(|row| {
            // activity is private; the public wire is the serialised row.
            serde_json::to_value(row).unwrap()["activity"]
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn the_default_state_filter_drops_hibernated_stopped_and_exited() {
    let body = r#"{
        "sessions": [
            {"id":"live","name":"live","cwd":"/w","activity":"idle","alive":true},
            {"id":"asleep","name":"asleep","cwd":"/w","activity":"hibernated","alive":false,"hibernated":true},
            {"id":"halted","name":"halted","cwd":"/w","activity":"stopped","alive":false},
            {"id":"gone","name":"gone","cwd":"/w","activity":"exited","alive":false}
        ],
        "hidden_count": 2
    }"#;
    let script = script_with(vec![(200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let listed = list_sessions(ctx, &digest_params()).unwrap();
    assert_eq!(listed.hidden_count, 2);
    let activities: Vec<_> = listed
        .digests
        .iter()
        .map(|row| {
            serde_json::to_value(row).unwrap()["activity"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(activities, vec!["idle".to_string()], "{activities:?}");
    assert_eq!(listed.total, 1);
    assert!(!listed.truncated);
}

#[test]
fn an_empty_states_list_keeps_every_state() {
    let body = r#"{
        "sessions": [
            {"id":"live","name":"live","cwd":"/w","activity":"idle","alive":true},
            {"id":"asleep","name":"asleep","cwd":"/w","activity":"hibernated","alive":false,"hibernated":true},
            {"id":"gone","name":"gone","cwd":"/w","activity":"exited","alive":false}
        ]
    }"#;
    let script = script_with(vec![(200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = digest_params();
    params.states = Some(Vec::new());
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.digests.len(), 3, "{listed:?}");
    assert_eq!(listed.total, 3);
}

#[test]
fn full_mode_returns_summaries_and_no_digests() {
    let body =
        r#"{"sessions":[{"id":"live","name":"live","cwd":"/w","activity":"idle","alive":true}]}"#;
    let script = script_with(vec![(200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = digest_params();
    params.mode = "full".to_string();
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.sessions.len(), 1);
    assert!(listed.digests.is_empty());
    assert!(!listed.sessions[0].linked);
}

#[test]
fn paging_happens_after_the_state_filter() {
    // Two hibernated rows would fill a page of two if the filter ran after
    // the cut. The idle row must still be the first page.
    let body = r#"{
        "sessions": [
            {"id":"a","name":"a","cwd":"/w","activity":"hibernated","alive":false,"hibernated":true},
            {"id":"b","name":"b","cwd":"/w","activity":"hibernated","alive":false,"hibernated":true},
            {"id":"c","name":"c","cwd":"/w","activity":"idle","alive":true}
        ]
    }"#;
    let script = script_with(vec![(200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = digest_params();
    params.limit = 1;
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.total, 1);
    assert!(!listed.truncated);
    let id = serde_json::to_value(&listed.digests[0]).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(id, "c");
}

#[test]
fn total_and_truncated_describe_the_filtered_set() {
    let body = r#"{
        "sessions": [
            {"id":"a","name":"a","cwd":"/w","activity":"idle","alive":true},
            {"id":"b","name":"b","cwd":"/w","activity":"running","alive":true},
            {"id":"c","name":"c","cwd":"/w","activity":"idle","alive":true}
        ]
    }"#;
    let script = script_with(vec![(200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = digest_params();
    params.limit = 2;
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.total, 3);
    assert!(listed.truncated);
    assert_eq!(listed.digests.len(), 2);
}

#[test]
fn an_unlinked_engine_session_survives_onto_page_two() {
    // Linked declared rows fill page one. The unlinked engine row is joined
    // before the page is cut, so offset 1 still returns it (Python sessions.py).
    let start = script_with(vec![(
        200,
        r#"{"id":"eng-linked","name":"linked","cwd":"/work/tree","activity":"idle","alive":true}"#
            .to_string(),
    )]);
    let built = opened(Some(Arc::clone(&start)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    start.answers.lock().unwrap().push((
        200,
        r#"{
            "sessions": [
                {"id":"eng-linked","name":"linked","cwd":"/work/tree","activity":"idle","alive":true},
                {"id":"eng-free","name":"free","cwd":"/elsewhere","activity":"idle","alive":true}
            ]
        }"#
        .to_string(),
    ));
    let mut params = digest_params();
    params.limit = 1;
    params.offset = 1;
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.total, 2, "{listed:?}");
    assert_eq!(listed.digests.len(), 1, "{listed:?}");
    let row = serde_json::to_value(&listed.digests[0]).unwrap();
    assert_eq!(row["id"].as_str(), Some("eng-free"), "{row}");
}

#[test]
fn a_digest_carries_done_at_budget_report_and_extra_fields() {
    let body = r#"{
        "sessions": [{
            "id": "eng-done",
            "name": "done-one",
            "cwd": "/w",
            "activity": "waiting-for-input",
            "alive": true,
            "done_at": "2026-10-28T00:00:00Z",
            "last_reply_excerpt": "a long reply that should not survive a report",
            "economics": {"turns": 4, "compactions": 1, "usage_measured": false}
        }]
    }"#;
    let script = script_with(vec![(200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let db = ctx.config.resolved_data_dir().join("declared.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let actor_id: String = conn
        .query_row(
            "SELECT id FROM actors WHERE identity_ref = 'local:test-user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO session_reports (id, actor_id, engine_session_id, kind, summary, data, created_at)
         VALUES ('rep-1', ?1, 'eng-done', 'result', 'landed it',
                 '{\"branch\":\"backlog-1\",\"sha\":\"abcdef0123456789\",\"pr\":\"9\"}',
                 '2026-10-28T00:00:01Z')",
        [&actor_id],
    )
    .unwrap();
    let mut params = digest_params();
    params.fields = Some(vec!["name".to_string(), "command".to_string()]);
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.digests.len(), 1);
    let row = serde_json::to_value(&listed.digests[0]).unwrap();
    assert_eq!(row["attention"].as_str(), Some("done"), "{row}");
    assert_eq!(row["done_at"].as_str(), Some("2026-10-28T00:00:00Z"));
    assert!(row["last_reply_excerpt"].is_null(), "{row}");
    let report = row["report"].as_str().unwrap();
    assert!(report.starts_with("result: landed it"), "{report}");
    assert!(report.contains("sha=abcdef012345"), "{report}");
    assert_eq!(row["budget"].as_str(), Some("4 turns, 1 compaction"));
    let extra = &row["extra"];
    assert_eq!(extra["name"].as_str(), Some("done-one"));
    assert!(extra.get("command").is_none(), "{extra}");
}

#[test]
fn declared_overflow_is_a_floor_on_total_and_stays_truncated() {
    // One past the declared fetch ceiling. The extra row is not joined, but
    // total is a floor and the page stays truncated (Python `_DECLARED_FETCH`).
    let script = script_with(vec![(200, r#"{"sessions":[]}"#.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let db = ctx.config.resolved_data_dir().join("declared.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let actor_id: String = conn
        .query_row(
            "SELECT id FROM actors WHERE identity_ref = 'local:test-user'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let project_id: String = conn
        .query_row("SELECT id FROM projects WHERE slug = 'beta'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    for index in 0..501 {
        tx.execute(
            "INSERT INTO coding_sessions (
                id, engine_session_id, project_id, actor_id, cwd, reason, started_at
             ) VALUES (?1, ?2, ?3, ?4, '/w', 'seed', ?5)",
            (
                format!("ses_{index:04}"),
                format!("eng_{index:04}"),
                &project_id,
                &actor_id,
                format!("2026-10-28T00:{:02}:{:02}Z", index / 60, index % 60),
            ),
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let mut params = digest_params();
    params.limit = 50;
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.total, 501, "floor is the kept 500 plus one");
    assert!(listed.truncated);
    assert_eq!(listed.digests.len(), 50);
}
