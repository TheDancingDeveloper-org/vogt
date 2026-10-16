//! Ports the cases in `tests/test_sessions.py` that stand on the service:
//! a session opens in the project's checkout, a blank reason never reaches
//! the engine, both subjects are refused, a task lands in the brief, stopping
//! revokes the token, and a dead engine is reported rather than rendered as
//! an empty list.

use std::sync::{Arc, Mutex};

use super::sessions::{
    list_sessions, start_session, stop_session, ListSessionsParams, StartSessionParams,
    StopSessionParams,
};
use crate::adapters::engine::EngineClient;
use crate::application::context::{build_context, Built};
use crate::core::{ActorKind, LinkState, Moment, Principal, Project, SequentialIds, StepClock};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WriteTxn};

fn moment() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// What the engine was asked, in order, and the body it answers with.
struct Script {
    seen: Mutex<Vec<String>>,
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

fn opened(script: Option<Arc<Script>>) -> Built {
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
        Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
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

fn insert_project(built: &Built, slug: &str) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut project = Project::new(&format!("prj_{slug}"), slug, slug, "/work/tree", moment());
    project.link_state = LinkState::Unlinked;
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    txn.commit().unwrap();
}

fn start_params(project: Option<&str>, task: Option<&str>, reason: &str) -> StartSessionParams {
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
    }
}

fn script_with(answers: Vec<(u16, String)>) -> Arc<Script> {
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
        },
    )
    .unwrap();
    assert_eq!(listed.sessions.len(), 1, "{listed:?}");
    assert!(
        listed.engine.is_some(),
        "a dead engine must be reported, not rendered as a clean list"
    );
}
