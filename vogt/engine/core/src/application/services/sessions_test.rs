//! Ports the cases in `tests/test_sessions.py` that stand on the service:
//! a session opens in the project's checkout, a blank reason never reaches
//! the engine, both subjects are refused, a task lands in the brief, stopping
//! revokes the token, and a dead engine is reported rather than rendered as
//! an empty list.

use std::sync::{Arc, Mutex};

use super::sessions::SessionBudget;
use super::sessions::{
    list_session_templates_op, list_sessions, list_sessions_op, start_session, stop_session,
    ListSessionsParams, StartSessionParams, StopSessionParams,
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
        placement: None,
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
        mcp_set: None,
        cold_wake_opt_out: false,
        wake_on: Vec::new(),
        explicit_null: Vec::new(),
        override_reason: None,
    }
}

#[test]
fn template_list_is_forwarded_to_the_engine() {
    let script = script_with(vec![(
        200,
        r#"[{"name":"claude","agent":"claude-code","placements":["host"],"default_placement":"host"}]"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(_ctx) = &built else {
        unreachable!()
    };

    let result = list_session_templates_op(&built, json!({})).unwrap();
    assert_eq!(result["templates"][0]["name"], "claude");
    assert_eq!(result["templates"][0]["placements"], json!(["host"]));
    assert_eq!(result["templates"][0]["default_placement"], "host");
    assert_eq!(
        script.seen.lock().unwrap()[0],
        "GET http://engine/api/session-templates "
    );
}

#[test]
fn start_forwards_explicit_placement() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-placement","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };

    let mut params = start_params(Some("beta"), None, "because");
    params.template = Some("claude".to_string());
    params.placement = Some("host".to_string());
    start_session(ctx, &params).unwrap();
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(asked.contains(r#""placement":"host""#), "{asked}");
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
    let mut params = start_params(Some("beta"), Some("check the containers"), "because");
    params.template = Some("claude".to_string());
    start_session(ctx, &params).unwrap();
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(
        asked.contains("check the containers"),
        "the task did not reach the engine: {asked}"
    );
}

#[test]
fn an_explicit_mcp_set_full_reaches_the_engine() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-11","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(Some("beta"), None, "because");
    params.mcp_set = Some("full".to_string());
    start_session(ctx, &params).unwrap();
    let body = engine_create_body(&script);
    assert!(body.contains(r#""mcp_set":"full""#), "{body}");
}

#[test]
fn an_mcp_set_other_than_full_or_worker_is_refused() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let error = super::sessions::start_session_op(
        &built,
        serde_json::json!({"project": "beta", "reason": "because", "mcp_set": "everything"}),
    )
    .unwrap_err();
    assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    assert!(script.seen.lock().unwrap().is_empty());
}

#[test]
fn a_task_without_a_template_is_refused_before_the_engine_is_asked() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let error = start_session(
        ctx,
        &start_params(Some("beta"), Some("check the containers"), "because"),
    )
    .unwrap_err();
    assert!(matches!(error, VogtError::InvalidRequest(_)), "{error:?}");
    assert!(script.seen.lock().unwrap().is_empty());
}

#[test]
fn a_task_on_a_purpose_that_sets_a_template_is_not_refused() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-3","name":"gamma","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let params = with_purpose(
        start_params(Some("beta"), Some("check the containers"), "because"),
        "implement",
    );
    start_session(ctx, &params).unwrap();
    assert!(!script.seen.lock().unwrap().is_empty());
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

/// Seeds the Vogt row of a session an agent or a person started, so a child
/// start can read its origin (WI-4471).
fn seed_started_session(built: &Built, engine_id: &str, started_by_kind: Option<&str>) {
    seed_child_session(built, engine_id, started_by_kind, None);
}

fn seed_child_session(
    built: &Built,
    engine_id: &str,
    started_by_kind: Option<&str>,
    started_by_session: Option<&str>,
) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let actor = ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity("local:test-user")
        .unwrap()
        .unwrap();
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_session(&crate::core::CodingSession {
        id: format!("ses_{engine_id}"),
        engine_session_id: engine_id.to_string(),
        project_id: "prj_beta".to_string(),
        work_item_id: None,
        actor_id: actor.id,
        cwd: "/work/tree".to_string(),
        template: None,
        model: None,
        effort: None,
        reason: "because".to_string(),
        purpose: None,
        started_by_session: started_by_session.map(str::to_string),
        started_by_kind: started_by_kind.map(str::to_string),
        started_at: moment(),
        stopped_at: None,
    })
    .unwrap();
    txn.commit().unwrap();
}

/// A worker that a person started, with no role given, is a worker by its
/// engine role only by default. Its child is not labelled a worker (WI-4471).
#[test]
fn a_child_of_a_person_started_worker_is_not_labelled_worker() {
    let script = script_with(vec![
        (
            200,
            r#"{"id":"eng-c","name":"beta","cwd":"/work/tree"}"#.to_string(),
        ),
        (
            200,
            r#"{"id":"eng-p","name":"parent","cwd":"/work/tree","activity":"idle","alive":true,"autopilot":false,"autopilot_nudges":0,"keep_awake":false,"role":"worker"}"#
                .to_string(),
        ),
    ]);
    let built = opened_as(
        Some(Arc::clone(&script)),
        Principal::new("agent:engine:eng-p", ActorKind::Agent, "Parent").unwrap(),
    );
    insert_project(&built, "beta");
    seed_started_session(&built, "eng-p", Some("person"));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let started = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    assert_eq!(started.session.started_by_session.as_deref(), Some("eng-p"));
    assert_eq!(started.session.started_by_kind, None);
}

/// A worker that an oversight session started is a worker's child (WI-4471).
#[test]
fn a_child_of_an_oversight_started_worker_is_a_worker() {
    let script = script_with(vec![
        (
            200,
            r#"{"id":"eng-c","name":"beta","cwd":"/work/tree"}"#.to_string(),
        ),
        (
            200,
            r#"{"id":"eng-p","name":"parent","cwd":"/work/tree","activity":"idle","alive":true,"autopilot":false,"autopilot_nudges":0,"keep_awake":false,"role":"worker"}"#
                .to_string(),
        ),
    ]);
    let built = opened_as(
        Some(Arc::clone(&script)),
        Principal::new("agent:engine:eng-p", ActorKind::Agent, "Parent").unwrap(),
    );
    insert_project(&built, "beta");
    seed_started_session(&built, "eng-p", Some("oversight"));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let started = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    assert_eq!(started.session.started_by_kind.as_deref(), Some("worker"));
}

/// The body of the engine's `POST` that created a session.
fn engine_create_body(script: &Script) -> String {
    script
        .seen
        .lock()
        .unwrap()
        .iter()
        .find(|line| line.starts_with("POST "))
        .expect("a session create")
        .clone()
}

/// WI-1521: a start by an oversight session names it as the supervisor, and
/// passes the wake kinds it asked for, so the engine can wake it.
#[test]
fn an_oversight_start_sends_its_supervisor_and_wake_on_to_the_engine() {
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
    let mut params = start_params(Some("beta"), None, "because");
    params.wake_on = vec!["done".to_string(), "blocked".to_string()];
    start_session(ctx, &params).unwrap();
    let body = engine_create_body(&script);
    assert!(body.contains(r#""supervisor":"eng-parent""#), "{body}");
    assert!(body.contains(r#""wake_on":["done","blocked"]"#), "{body}");
}

/// WI-1521: a worker's start names no supervisor and sends no wake kinds.
#[test]
fn a_worker_start_sends_no_supervisor_to_the_engine() {
    let script = script_with(vec![
        (
            200,
            r#"{"id":"eng-8","name":"beta","cwd":"/work/tree"}"#.to_string(),
        ),
        (
            200,
            r#"{"id":"eng-worker","name":"worker","cwd":"/work/tree","activity":"idle","alive":true,"autopilot":false,"autopilot_nudges":0,"keep_awake":false,"role":"worker"}"#
                .to_string(),
        ),
    ]);
    let built = opened_as(
        Some(Arc::clone(&script)),
        Principal::new("agent:engine:eng-worker", ActorKind::Agent, "Worker").unwrap(),
    );
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    let body = engine_create_body(&script);
    assert!(!body.contains("supervisor"), "{body}");
    assert!(!body.contains("wake_on"), "{body}");
}

fn worker_caller_script() -> Arc<Script> {
    script_with(vec![
        (
            200,
            r#"{"id":"eng-8","name":"beta","cwd":"/work/tree"}"#.to_string(),
        ),
        (
            200,
            r#"{"id":"eng-worker","name":"worker","cwd":"/work/tree","activity":"idle","alive":true,"autopilot":false,"autopilot_nudges":0,"keep_awake":false,"role":"worker"}"#
                .to_string(),
        ),
    ])
}

/// WI-4489: a worker's child never outranks it. A worker that asks for an
/// oversight session, or for the full MCP set, is refused before the engine
/// is asked, whatever the supervisor or autopilot fields say.
#[test]
fn a_worker_cannot_start_an_oversight_or_full_set_session() {
    for (role, mcp_set, refused) in [
        ("oversight", None, "role=oversight"),
        ("worker", Some("full"), "mcp_set=full"),
    ] {
        let script = worker_caller_script();
        let built = opened_as(
            Some(Arc::clone(&script)),
            Principal::new("agent:engine:eng-worker", ActorKind::Agent, "Worker").unwrap(),
        );
        insert_project(&built, "beta");
        let Built::StepSequential(ctx) = &built else {
            unreachable!("opened() builds a step clock");
        };
        let mut params = start_params(Some("beta"), None, "because");
        params.role = role.to_string();
        params.mcp_set = mcp_set.map(str::to_string);
        params.autopilot = Some(true);
        let error = start_session(ctx, &params).unwrap_err().to_string();
        assert!(error.contains(refused), "{error}");
        assert!(
            !script
                .seen
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("POST ")),
            "the engine was asked to start it"
        );
    }
}

/// WI-4489: a worker's start names its caller to the engine as `started_by`,
/// so the engine holds the child to the parent's standing too.
#[test]
fn a_worker_start_names_its_caller_as_started_by() {
    let script = worker_caller_script();
    let built = opened_as(
        Some(Arc::clone(&script)),
        Principal::new("agent:engine:eng-worker", ActorKind::Agent, "Worker").unwrap(),
    );
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    let body = engine_create_body(&script);
    assert!(body.contains(r#""started_by":"eng-worker""#), "{body}");
}

/// WI-4489: an oversight session may still start one on the full set.
#[test]
fn an_oversight_session_may_start_a_full_set_session() {
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
    let mut params = start_params(Some("beta"), None, "because");
    params.mcp_set = Some("full".to_string());
    start_session(ctx, &params).unwrap();
    let body = engine_create_body(&script);
    assert!(body.contains(r#""mcp_set":"full""#), "{body}");
    assert!(body.contains(r#""started_by":"eng-parent""#), "{body}");
}

/// WI-1521: a person's start names no supervisor. Wake kinds stay unset unless
/// the person asks for them, which is how an overseer is started.
#[test]
fn a_person_start_sends_no_supervisor_to_the_engine() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-8","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();
    let body = engine_create_body(&script);
    assert!(!body.contains("supervisor"), "{body}");
    assert!(!body.contains("wake_on"), "{body}");
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

#[test]
fn full_mode_with_fields_slims_each_row_to_id_and_the_named_fields() {
    let body =
        r#"{"sessions":[{"id":"live","name":"live","cwd":"/w","activity":"idle","alive":true}]}"#;
    let script = script_with(vec![(200, body.to_string()), (200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = digest_params();
    params.mode = "full".to_string();
    let whole = serde_json::to_value(list_sessions(ctx, &params).unwrap()).unwrap();
    // Default output is unchanged: no fields, every column.
    let mut untouched = whole.clone();
    super::sessions::slim_full_rows(&mut untouched, None);
    assert_eq!(untouched, whole);
    super::sessions::slim_full_rows(&mut untouched, Some(&[]));
    assert_eq!(untouched, whole);
    let mut slim = whole.clone();
    super::sessions::slim_full_rows(
        &mut slim,
        Some(&["activity".to_string(), "no_such_field".to_string()]),
    );
    let row = slim["sessions"][0].as_object().unwrap();
    let mut keys: Vec<&str> = row.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["activity", "id"], "{row:?}");
    assert!(whole["sessions"][0].as_object().unwrap().len() > 2);
    assert_eq!(slim["total"], whole["total"]);
}

/// WI-3987: a worker started on a bare project must not get a work item
/// created for it, and its name must not become the task text. The Python
/// line's autopilot chore ("Session: <task>") is not carried onto rust-2; this
/// pins that a project-only start writes no item and names the session after
/// the project slug, so the junk cannot come back through the engine path.
#[test]
fn a_project_only_worker_start_creates_no_work_item() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-9","name":"beta","cwd":"/work/tree","activity":"idle"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(Some("beta"), Some("Ship the thing\nsecond line"), "because");
    params.template = Some("claude".to_string());
    params.autopilot = Some(true);
    let started = start_session(ctx, &params).unwrap();
    let name = started
        .session
        .name
        .as_deref()
        .expect("a started session has a name");
    assert_eq!(
        name, "beta",
        "a task-only start names the session after the project slug"
    );
    assert!(
        name.chars().count() <= 80,
        "the session name is bounded: {name}"
    );
    let items = ctx
        .declared
        .read()
        .unwrap()
        .list_work_items(&crate::storage::interface::WorkFilter::default())
        .unwrap();
    assert!(
        items.is_empty(),
        "a project-only start must not create a work item, found {:?}",
        items
            .iter()
            .map(|item| item.title.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_engine_budget_keeps_only_positive_limits() {
    use super::sessions::budget_of_engine;
    use serde_json::json;
    let row = |value: serde_json::Value| value.as_object().cloned();
    // Zero, negative, fractional and non-numeric limits are no limit (Python
    // `_budget_params` keeps positive ints only).
    let budget = budget_of_engine(
        row(
            json!({"turns": 0, "context_tokens": 100000, "wall_clock_minutes": -5,
                   "session_wall_clock_minutes": 2.5, "unknown": 9}),
        )
        .as_ref(),
    )
    .expect("one positive limit survives");
    assert_eq!(budget.context_tokens, Some(100_000));
    assert_eq!(budget.turns, None);
    assert_eq!(budget.wall_clock_minutes, None);
    assert_eq!(budget.session_wall_clock_minutes, None);
    // A budget that names no positive limit is no budget at all.
    assert!(budget_of_engine(row(json!({"turns": 0})).as_ref()).is_none());
    assert!(budget_of_engine(None).is_none());
}

#[test]
fn a_sessions_account_and_budget_come_back_from_the_engine() {
    // Mirrors Python's test_a_sessions_account_comes_back_from_the_engine,
    // through the list path: a live row's account and budget reach the
    // session list in full mode and when `fields` names them.
    let live_row = r#"{"id":"eng-acct","name":"acct","cwd":"/work/tree","activity":"idle","alive":true,"account":"acct2","budget":{"turns":4}}"#;
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-acct","name":"acct","cwd":"/work/tree","activity":"idle","alive":true}"#
            .to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap();

    script
        .answers
        .lock()
        .unwrap()
        .push((200, format!(r#"{{"sessions":[{live_row}]}}"#)));
    let full = list_sessions_op(&built, json!({"mode": "full"})).unwrap();
    let row = &full["sessions"][0];
    assert_eq!(row["account"], json!("acct2"), "{full}");
    // The budget keeps all four keys; the three limits not set are null.
    assert_eq!(
        row["budget"],
        json!({
            "turns": 4,
            "context_tokens": null,
            "wall_clock_minutes": null,
            "session_wall_clock_minutes": null
        }),
        "{full}"
    );

    script
        .answers
        .lock()
        .unwrap()
        .push((200, format!(r#"{{"sessions":[{live_row}]}}"#)));
    let slim = list_sessions_op(
        &built,
        json!({"mode": "full", "fields": ["account", "budget"]}),
    )
    .unwrap();
    let mut keys: Vec<&str> = slim["sessions"][0]
        .as_object()
        .expect("a slimmed row is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["account", "budget", "id"], "{slim}");
    assert_eq!(slim["sessions"][0]["account"], json!("acct2"), "{slim}");
    assert_eq!(slim["sessions"][0]["budget"]["turns"], json!(4), "{slim}");
}

// -- dispatch enforcement (WI-1329) -------------------------------------------

/// A person's `dispatch.*` preferences, as the dispatch table reads them.
fn set_dispatch(built: &Built, rows: &[(&str, serde_json::Value)]) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let human = ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity("local:test-user")
        .unwrap()
        .expect("the bootstrapped human");
    let mut txn = ctx.declared.write().unwrap();
    for (key, value) in rows {
        txn.upsert_actor_preference(&ActorPreference {
            actor_id: human.id.clone(),
            key: key.to_string(),
            value: value.clone(),
            version: 1,
            updated_at: moment(),
        })
        .unwrap();
    }
    txn.commit().unwrap();
}

fn enforcing(built: &Built) {
    set_dispatch(built, &[("dispatch.enforce", json!({"value": true}))]);
}

fn caller_role_answer(role: &str) -> (u16, String) {
    (
        200,
        format!(
            r#"{{"id":"eng-caller","name":"caller","cwd":"/work/tree","activity":"idle","alive":true,"autopilot":false,"autopilot_nudges":0,"keep_awake":false,"role":"{role}"}}"#
        ),
    )
}

/// An agent session built over a scripted engine: the caller's lookup answers
/// with `role`, and the start with a new session.
fn agent_caller(role: &str) -> (Built, Arc<Script>) {
    let script = script_with(vec![
        (
            200,
            r#"{"id":"eng-new","name":"beta","cwd":"/work/tree"}"#.to_string(),
        ),
        caller_role_answer(role),
    ]);
    let built = opened_as(
        Some(Arc::clone(&script)),
        Principal::new("agent:engine:eng-caller", ActorKind::Agent, "Caller").unwrap(),
    );
    insert_project(&built, "beta");
    if role == "worker" {
        // A worker's kind is its origin (WI-4471): an agent started it.
        seed_started_session(&built, "eng-caller", Some("oversight"));
    }
    (built, script)
}

fn created(script: &Script) -> bool {
    script
        .seen
        .lock()
        .unwrap()
        .iter()
        .any(|line| line.starts_with("POST "))
}

fn started_summary(built: &Built) -> serde_json::Value {
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
        .rev()
        .find(|event| event.kind == "session.started")
        .expect("a session.started event")
        .summary
}

fn override_audits(built: &Built) -> Vec<crate::core::AuditRecord> {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    ctx.declared
        .read()
        .unwrap()
        .list_audit(&crate::storage::interface::AuditQuery {
            limit: 100,
            offset: 0,
            actor_id: None,
            operation: Some("dispatch.override".to_string()),
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap()
}

fn review_params() -> StartSessionParams {
    with_purpose(start_params(Some("beta"), None, "because"), "review")
}

#[test]
fn an_enforced_worker_start_of_a_reviewer_is_refused_before_the_engine() {
    let (built, script) = agent_caller("worker");
    enforcing(&built);
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    let error = start_session(ctx, &review_params()).unwrap_err();
    assert!(
        matches!(&error, VogtError::InvalidRequest(text)
            if text.contains("dispatch refused this start")
                && text.contains("review_by_oversight_only")),
        "{error:?}"
    );
    assert!(!created(&script), "no terminal may be started");
}

#[test]
fn without_enforcement_the_broken_rule_is_recorded_not_refused() {
    let (built, script) = agent_caller("worker");
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    start_session(ctx, &review_params()).unwrap();
    assert!(created(&script));
    let notes = started_summary(&built)["dispatch_notes"].clone();
    assert!(
        notes.to_string().contains("review_by_oversight_only"),
        "{notes}"
    );
}

#[test]
fn a_person_is_never_refused_even_when_enforcing() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-new","name":"beta","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    enforcing(&built);
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    let mut params = with_purpose(start_params(Some("beta"), None, "because"), "implement");
    params.model = Some("haiku".to_string());
    start_session(ctx, &params).unwrap();
    assert!(created(&script));
}

#[test]
fn an_enforced_start_must_run_its_profiles_model() {
    let (built, script) = agent_caller("oversight");
    enforcing(&built);
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    // No purpose is judged as implement (sonnet by default).
    let mut params = start_params(Some("beta"), None, "because");
    params.model = Some("haiku".to_string());
    let error = start_session(ctx, &params).unwrap_err();
    assert!(
        matches!(&error, VogtError::InvalidRequest(text) if text.contains("profile")),
        "{error:?}"
    );
    assert!(!created(&script));
}

#[test]
fn an_enforced_start_past_the_worker_cap_is_refused() {
    let (built, script) = agent_caller("oversight");
    set_dispatch(
        &built,
        &[
            ("dispatch.enforce", json!({"value": true})),
            ("dispatch.max_workers_per_overseer", json!({"value": 1})),
        ],
    );
    seed_child_session(&built, "eng-live", Some("oversight"), Some("eng-caller"));
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    let error = start_session(ctx, &start_params(Some("beta"), None, "because")).unwrap_err();
    assert!(
        matches!(&error, VogtError::InvalidRequest(text) if text.contains("max_workers_per_overseer=1")),
        "{error:?}"
    );
    assert!(!created(&script));
}

#[test]
fn an_oversight_override_starts_past_a_rule_and_is_audited() {
    let (built, script) = agent_caller("oversight");
    enforcing(&built);
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    let mut params = start_params(Some("beta"), None, "because");
    params.model = Some("haiku".to_string());
    params.override_reason = Some("a spike on the small model".to_string());
    start_session(ctx, &params).unwrap();
    assert!(created(&script));
    let audits = override_audits(&built);
    assert_eq!(audits.len(), 1, "{audits:?}");
    assert_eq!(audits[0].reason, "a spike on the small model");
    assert_eq!(audits[0].actor_identity_ref, "agent:engine:eng-caller");
    let summary = started_summary(&built);
    assert_eq!(summary["override_reason"], "a spike on the small model");
}

/// The Inbox lists every oversight override; without the inbox change the
/// entry is absent.
#[test]
fn an_override_is_raised_in_the_inbox_and_an_escalation_is_not_in_its_audit() {
    let (built, _script) = agent_caller("oversight");
    set_dispatch(
        &built,
        &[
            ("dispatch.max_workers_per_overseer", json!({"value": 1})),
            (
                "dispatch.escalate_after_failed_reviews",
                json!({"value": 1}),
            ),
        ],
    );
    seed_work_item_with_blockers(&built, 1);
    seed_child_session(&built, "eng-live", Some("oversight"), Some("eng-caller"));
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    // Enforcement is off, so the escalation is only a note on the start; the
    // override's own audit lists the broken rule alone.
    let mut params = with_purpose(start_params(None, None, "because"), "implement");
    params.work_item = Some("WI-1".to_string());
    params.override_reason = Some("a spike past the cap".to_string());
    start_session(ctx, &params).unwrap();
    let listed = super::inbox::inbox_list(ctx, json!({"limit": 50})).unwrap();
    let raised: Vec<_> = listed["entries"]
        .as_array()
        .expect("inbox entries")
        .iter()
        .filter(|entry| entry["kind"] == "dispatch.override")
        .collect();
    assert_eq!(raised.len(), 1, "{listed}");
    assert_eq!(raised[0]["summary"], "a spike past the cap");
    assert!(raised[0]["title"]
        .as_str()
        .unwrap()
        .contains("started by agent:engine:eng-caller"));
    assert_eq!(raised[0]["provisional"], true);
    let summary = started_summary(&built);
    assert!(summary["dispatch_notes"]
        .to_string()
        .contains("escalate_after_failed_reviews"));
    let events = ctx
        .declared
        .read()
        .unwrap()
        .list_events(0, 100, None)
        .unwrap();
    let audited = events
        .iter()
        .find(|event| event.kind == "dispatch.override")
        .expect("an override event");
    let notes = audited.summary["notes"].to_string();
    assert!(notes.contains("max_workers_per_overseer") && !notes.contains("escalate"));
}

#[test]
fn a_worker_cannot_override() {
    let (built, script) = agent_caller("worker");
    enforcing(&built);
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    let mut params = start_params(Some("beta"), None, "because");
    params.override_reason = Some("let me".to_string());
    let error = start_session(ctx, &params).unwrap_err();
    assert!(
        matches!(&error, VogtError::InvalidRequest(text)
            if text.contains("override is accepted only from an oversight session")),
        "{error:?}"
    );
    assert!(!created(&script));
    assert!(override_audits(&built).is_empty());
}

#[test]
fn an_item_that_keeps_failing_review_escalates_an_enforced_implement_start() {
    let (built, _script) = agent_caller("oversight");
    set_dispatch(
        &built,
        &[
            ("dispatch.enforce", json!({"value": true})),
            (
                "dispatch.escalate_after_failed_reviews",
                json!({"value": 1}),
            ),
        ],
    );
    seed_work_item_with_blockers(&built, 1);
    let Built::StepSequential(ctx) = &built else {
        unreachable!()
    };
    let mut params = with_purpose(start_params(None, None, "because"), "implement");
    params.work_item = Some("WI-1".to_string());
    let started = start_session(ctx, &params).unwrap();
    assert_eq!(started.escalated_to.as_deref(), Some("implement-careful"));
    assert_eq!(
        started.session.purpose.as_deref(),
        Some("implement-careful")
    );
}

/// WI-1 with `blockers` stored `verdict=blockers` comments.
fn seed_work_item_with_blockers(built: &Built, blockers: usize) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let actor = ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity("local:test-user")
        .unwrap()
        .unwrap();
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_work_item(&WorkItem {
        id: "wrk_WI-1".to_string(),
        reference: "WI-1".to_string(),
        kind: WorkKind::Feature,
        title: "an item".to_string(),
        body: String::new(),
        state: "open".to_string(),
        priority: Priority::P2,
        effort: None,
        project_id: Some("prj_beta".to_string()),
        project_slug: Some("beta".to_string()),
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
    for n in 0..blockers {
        txn.insert_comment(&crate::core::Comment {
            id: format!("cmt_{n}"),
            work_item_id: "wrk_WI-1".to_string(),
            actor_id: actor.id.clone(),
            actor_display_name: "reviewer".to_string(),
            body: "review".to_string(),
            created_at: moment(),
            kind: "verdict".to_string(),
            handoff: None,
            verdict: Some(json!({"verdict": "blockers"})),
            verification: None,
        })
        .unwrap();
    }
    txn.commit().unwrap();
}

fn transcript_reads() -> Vec<String> {
    super::sessions::TRANSCRIPT_READS.with(|reads| std::mem::take(&mut *reads.borrow_mut()))
}

/// A full page reads transcripts for its own rows only, and a page past the
/// last row reads none (WI-4531).
#[test]
fn a_full_page_reads_transcripts_only_for_its_own_rows() {
    let body = r#"{
        "sessions": [
            {"id":"eng-a","name":"a","cwd":"/w","activity":"idle","alive":true},
            {"id":"eng-b","name":"b","cwd":"/w","activity":"idle","alive":true},
            {"id":"eng-c","name":"c","cwd":"/w","activity":"idle","alive":true}
        ]
    }"#;
    let script = script_with(vec![(200, body.to_string()), (200, body.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = digest_params();
    params.mode = "full".to_string();
    params.limit = 1;
    params.offset = 1;
    transcript_reads();
    let listed = list_sessions(ctx, &params).unwrap();
    assert_eq!(listed.total, 3, "{listed:?}");
    assert_eq!(listed.sessions.len(), 1, "{listed:?}");
    assert_eq!(listed.sessions[0].engine_session_id, "eng-b");
    assert_eq!(transcript_reads(), vec!["eng-b".to_string()]);
    params.offset = 500;
    let past = list_sessions(ctx, &params).unwrap();
    assert!(past.sessions.is_empty(), "{past:?}");
    assert!(transcript_reads().is_empty());
}

/// A full list that asks `starters_only` is read from the declared
/// table: no engine call and no transcript read, stopped sessions included
/// (WI-4531).
#[test]
fn a_starters_read_asks_neither_the_engine_nor_a_transcript() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    seed_child_session(&built, "eng-parent", None, None);
    seed_child_session(&built, "eng-child", Some("oversight"), Some("eng-parent"));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let db = ctx.config.resolved_data_dir().join("declared.sqlite3");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE coding_sessions SET stopped_at = '2026-11-23T00:00:00Z' WHERE engine_session_id = 'eng-parent'",
            [],
        )
        .unwrap();
    transcript_reads();
    let listed = list_sessions_op(
        &built,
        json!({
            "mode": "full",
            "include_stopped": true,
            "limit": 500,
            "fields": ["engine_session_id", "started_by_session"],
            "starters_only": true,
        }),
    )
    .unwrap();
    assert!(script.seen.lock().unwrap().is_empty(), "engine was asked");
    assert!(transcript_reads().is_empty());
    let rows = listed["sessions"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listed}");
    let child = rows
        .iter()
        .find(|row| row["engine_session_id"] == "eng-child")
        .unwrap();
    assert_eq!(child["started_by_session"], "eng-parent");
    assert_eq!(child["id"], "ses_eng-child");
    assert!(rows
        .iter()
        .any(|row| row["engine_session_id"] == "eng-parent"));
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["truncated"], false);

    let paged = list_sessions_op(
        &built,
        json!({
            "mode": "full",
            "include_stopped": true,
            "limit": 1,
            "fields": ["engine_session_id", "started_by_session"],
            "starters_only": true,
        }),
    )
    .unwrap();
    assert_eq!(paged["sessions"].as_array().unwrap().len(), 1, "{paged}");
    assert_eq!(paged["truncated"], true);
    assert!(script.seen.lock().unwrap().is_empty(), "engine was asked");
}

/// Naming starter columns in `fields` alone does not select the fast path: the
/// caller must ask `starters_only`, and a flag that cannot be honoured is an
/// error, not a silent change of output (WI-4543).
#[test]
fn starter_fields_without_the_flag_keep_the_ordinary_full_rows() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    seed_child_session(&built, "eng-parent", None, None);
    seed_child_session(&built, "eng-child", Some("oversight"), Some("eng-parent"));
    let plain = list_sessions_op(
        &built,
        json!({
            "mode": "full",
            "include_stopped": true,
            "fields": ["engine_session_id", "started_by_session"],
        }),
    )
    .unwrap();
    // The ordinary path asks the engine; the starters path never does.
    assert!(!script.seen.lock().unwrap().is_empty(), "engine not asked");
    assert!(plain["sessions"].is_array(), "{plain}");

    let refused = list_sessions_op(
        &built,
        json!({"mode": "digest", "starters_only": true, "fields": ["engine_session_id"]}),
    )
    .unwrap_err();
    assert_eq!(refused.code(), "invalid_request", "{refused:?}");
    let wide = list_sessions_op(
        &built,
        json!({"mode": "full", "starters_only": true, "fields": ["name"]}),
    )
    .unwrap_err();
    assert_eq!(wide.code(), "invalid_request", "{wide:?}");
}

/// A terminal reads the work item its session was opened for through the same
/// declared-table path: `work_item` is a starters column (WI-4543).
#[test]
fn a_starters_read_can_name_the_work_item_without_asking_the_engine() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "beta");
    insert_work_item(&built, "beta", "WI-9");
    seed_child_session(&built, "eng-a", None, None);
    seed_child_session(&built, "eng-b", None, None);
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let db = ctx.config.resolved_data_dir().join("declared.sqlite3");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE coding_sessions SET work_item_id = 'wrk_WI-9' WHERE engine_session_id = 'eng-a'",
            [],
        )
        .unwrap();
    let listed = list_sessions_op(
        &built,
        json!({
            "mode": "full",
            "starters_only": true,
            "fields": ["engine_session_id", "work_item"],
        }),
    )
    .unwrap();
    assert!(script.seen.lock().unwrap().is_empty(), "engine was asked");
    let rows = listed["sessions"].as_array().unwrap();
    let find = |id: &str| {
        rows.iter()
            .find(|row| row["engine_session_id"] == id)
            .unwrap()
    };
    assert_eq!(find("eng-a")["work_item"], "WI-9");
    assert!(find("eng-b")["work_item"].is_null());
    assert!(find("eng-a").get("started_by_session").is_none());
}
