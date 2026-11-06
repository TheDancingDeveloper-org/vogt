//! Ports `tests/test_workspaces.py`: `workspace.list` (WI-1340), engine
//! workspace rows joined to Vogt's project, session and merged state, and
//! `workspace.clean` (WI-1325), who may force and what the engine is sent.

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::adapters::engine::EngineClient;
use crate::application::context::{build_context, Built};
use crate::application::writes::audited_write;
use crate::core::{ActorKind, CodingSession, Moment, Principal, Project, StepClock, SweepOutcome};
use crate::errors::VogtError;
use crate::registry::default_registry;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView, WriteTxn};
use crate::storage::observed_types::PendingObservation;

const ROOT: &str = "/srv/estate/vogt";
const UNLINKED: &str = "0b0e5f0c-1111-4222-8333-944455556666";

type Rows = Arc<Mutex<Option<Vec<Value>>>>;

/// What the stand-in engine was sent on `POST /api/workspaces/clean`, and the
/// refusal it answers with (status 200 cleans).
#[derive(Default)]
struct Cleans {
    sent: Vec<Value>,
    status: u16,
    refusal: String,
}

type CleanLog = Arc<Mutex<Cleans>>;

/// An engine that answers `GET /api/workspaces` with `rows` (a 404 when
/// `None`) and `POST /api/workspaces/clean` from `cleans`; anything else
/// fails like an engine that is down.
fn engine(rows: &Rows, cleans: &CleanLog) -> EngineClient {
    let rows = Arc::clone(rows);
    let cleans = Arc::clone(cleans);
    EngineClient::new(
        "http://engine.test",
        None,
        Some(Arc::new(move |url, _, body, method| {
            if method == "POST" && url == "http://engine.test/api/workspaces/clean" {
                let sent: Value = serde_json::from_slice(body).unwrap();
                let mut cleans = cleans.lock().unwrap();
                cleans.sent.push(sent.clone());
                if cleans.status != 200 {
                    let text = json!({"error": format!("conflict: {}", cleans.refusal)});
                    return (cleans.status, text.to_string().into_bytes());
                }
                let done = json!({
                    "path": sent["path"],
                    "mode": sent["mode"],
                    "bytes_freed": 1234,
                    "removed": sent["mode"] == "remove",
                });
                return (200, done.to_string().into_bytes());
            }
            if method == "GET" && url == "http://engine.test/api/workspaces" {
                return match rows.lock().unwrap().as_ref() {
                    None => (404, br#"{"error":"not found"}"#.to_vec()),
                    Some(rows) => (200, json!({"workspaces": rows}).to_string().into_bytes()),
                };
            }
            (503, b"{}".to_vec())
        })),
    )
}

fn cleans() -> CleanLog {
    Arc::new(Mutex::new(Cleans {
        status: 200,
        ..Cleans::default()
    }))
}

fn wired(name: &str, rows: &Rows) -> Built {
    wired_as(name, rows, &cleans(), "local:ada", ActorKind::Human)
}

fn wired_as(
    name: &str,
    rows: &Rows,
    cleans: &CleanLog,
    identity_ref: &str,
    kind: ActorKind,
) -> Built {
    let dir = std::env::temp_dir().join(format!("vogt-workspaces-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut clock = None;
    let mut ids = None;
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir,
        ..crate::config::VogtConfig::default()
    };
    let principal = Principal::new(identity_ref, kind, "Caller").unwrap();
    let built = build_context(
        config,
        Some(principal),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
        None,
        None,
        Some(engine(rows, cleans)),
        None,
        None,
    )
    .unwrap();
    seed(&built);
    seed_branch(&built, "wi-1-landed", 0);
    seed_branch(&built, "wi-2-open", 3);
    built
}

fn session(id: &str, engine_id: &str, actor_id: &str) -> CodingSession {
    CodingSession {
        id: id.to_string(),
        engine_session_id: engine_id.to_string(),
        project_id: "prj_1".to_string(),
        work_item_id: None,
        actor_id: actor_id.to_string(),
        cwd: ROOT.to_string(),
        template: Some("claude".to_string()),
        model: None,
        effort: None,
        reason: "test".to_string(),
        purpose: None,
        started_by_session: None,
        started_by_kind: None,
        started_at: Moment::from_unix(1_700_000_000, 0),
        stopped_at: None,
    }
}

/// Project `vogt` with two linked sessions, `eng-1` and `eng-2`.
fn seed(built: &Built) {
    crate::with_ctx!(built, |ctx| {
        let mut write = crate::application::context::write_of(ctx);
        audited_write(&mut write, "seed", "test", |txn, actor| {
            txn.insert_project(&Project::new(
                "prj_1",
                "vogt",
                "Vogt",
                ROOT,
                Moment::from_unix(1_700_000_000, 0),
            ))?;
            txn.insert_session(&session("ses_0001", "eng-1", &actor.id))?;
            txn.insert_session(&session("ses_0002", "eng-2", &actor.id))?;
            Ok(crate::application::writes::WriteOutcome {
                result: json!({}),
                entity_kind: "session".to_string(),
                entity_id: "ses_0001".to_string(),
                payload: json!({}),
                event_kind: "session.started".to_string(),
                summary: json!({}),
            })
        })
    })
    .unwrap();
}

/// A `git.branch` observation, as the branch collector writes it.
fn seed_branch(built: &Built, name: &str, ahead: i64) {
    crate::with_ctx!(built, |ctx| {
        let at = Moment::from_unix(1_700_000_000, 0);
        let sweep = ctx
            .observed
            .begin_sweep("test", &["prj_1".to_string()], at)
            .unwrap();
        let finding = PendingObservation {
            kind: "git.branch".to_string(),
            subject_key: format!("branch:prj_1:{name}"),
            payload: json!({"name": name, "ahead": ahead}),
            content_digest: format!("branch-{name}-{ahead}"),
            project_id: Some("prj_1".to_string()),
            source_url: None,
            promoted: false,
        };
        ctx.observed.append(&sweep.id, &[finding], at).unwrap();
        ctx.observed
            .finish_sweep(&sweep.id, SweepOutcome::Ok, &[("projects", 1)], at, None)
            .unwrap();
        ctx.observed.rebuild_latest().unwrap();
    });
}

fn row(session_id: &str, branch: &str, alive: bool) -> Value {
    json!({
        "session_id": session_id,
        "alive": alive,
        "size_bytes": 4096,
        "kind": "worktree",
        "path": format!("/w/{branch}"),
        "repo_root": ROOT,
        "branch": branch,
        "base_sha": "a".repeat(40),
        "created_at": "2026-10-30T00:00:00Z",
        "baseline_dirty": [],
        "build_dirs": [],
    })
}

fn list(built: &Built, params: Value) -> Result<Vec<Value>, VogtError> {
    let out = default_registry()
        .get("workspace.list")
        .expect("the operation is registered")
        .run(Some(built), params)?;
    Ok(out["workspaces"].as_array().unwrap().clone())
}

#[test]
fn rows_join_project_session_and_merged() {
    let rows: Rows = Arc::new(Mutex::new(Some(vec![
        row("eng-1", "wi-1-landed", true),
        row(UNLINKED, "wi-2-open", false),
    ])));
    let built = wired("join", &rows);

    let every = list(&built, json!({})).unwrap();
    assert_eq!(every.len(), 2);
    let linked = every.iter().find(|r| r["session"] == "ses_0001").unwrap();
    assert_eq!(linked["project"], "vogt");
    assert_eq!(linked["merged"], true);
    assert_eq!(linked["size_bytes"], 4096);
    assert_eq!(linked["alive"], true);
    assert_eq!(linked["base_sha"], "a".repeat(40));
    let unlinked = every.iter().find(|r| r["session"] == UNLINKED).unwrap();
    // No Vogt session, so no project to look the branch up in.
    assert_eq!(unlinked["project"], Value::Null);
    assert_eq!(unlinked["merged"], Value::Null);

    let only = list(&built, json!({"project": "vogt"})).unwrap();
    let sessions: Vec<&Value> = only.iter().map(|r| &r["session"]).collect();
    assert_eq!(sessions, vec!["ses_0001"]);
    assert_eq!(
        list(&built, json!({"alive": false})).unwrap(),
        vec![unlinked.clone()]
    );
}

#[test]
fn a_row_carries_python_field_order() {
    let rows: Rows = Arc::new(Mutex::new(Some(vec![row("eng-1", "wi-1-landed", true)])));
    let built = wired("order", &rows);
    let every = list(&built, json!({})).unwrap();
    let keys: Vec<&str> = every[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "path",
            "kind",
            "branch",
            "base_sha",
            "size_bytes",
            "cleaned_at",
            "alive",
            "merged",
            "work_item",
            "session",
            "project"
        ]
    );
}

#[test]
fn an_open_branch_is_not_merged_and_an_unobserved_one_is_null() {
    let rows: Rows = Arc::new(Mutex::new(Some(vec![
        row("eng-1", "wi-2-open", true),
        row("eng-2", "wi-9-unseen", true),
    ])));
    let built = wired("unseen", &rows);
    let every = list(&built, json!({})).unwrap();
    assert_eq!(every[0]["merged"], false);
    assert_eq!(every[1]["merged"], Value::Null);
}

#[test]
fn an_unknown_project_is_not_found() {
    let rows: Rows = Arc::new(Mutex::new(Some(Vec::new())));
    let built = wired("nope", &rows);
    match list(&built, json!({"project": "nope"})) {
        Err(VogtError::NotFound(message)) => assert_eq!(message, "no project 'nope'"),
        other => panic!("expected not found, got {other:?}"),
    }
}

#[test]
fn an_engine_without_the_route_says_so() {
    let rows: Rows = Arc::new(Mutex::new(None));
    let built = wired("missing", &rows);
    match list(&built, json!({})) {
        Err(VogtError::EngineUnavailable(message)) => assert_eq!(
            message,
            "the session engine does not list workspaces yet (it has no \
             GET /api/workspaces route); upgrade the engine"
        ),
        other => panic!("expected engine unavailable, got {other:?}"),
    }
}

fn clean(built: &Built, params: Value) -> Result<Value, VogtError> {
    default_registry()
        .get("workspace.clean")
        .expect("the operation is registered")
        .run(Some(built), params)
}

const WHY: &str = "drive test";

#[test]
fn clean_by_session_sends_its_path_and_audits() {
    let rows: Rows = Arc::new(Mutex::new(Some(vec![row("eng-1", "wi-1-landed", true)])));
    let log = cleans();
    let built = wired_as("clean-id", &rows, &log, "local:ada", ActorKind::Human);
    let done = clean(
        &built,
        json!({"id": "ses_0001", "mode": "build_output", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(
        done,
        json!({
            "path": "/w/wi-1-landed",
            "mode": "build_output",
            "bytes_freed": 1234,
            "removed": false,
            "branch_deleted": null,
            "cleaned_at": null,
        })
    );
    assert_eq!(
        log.lock().unwrap().sent.last().unwrap(),
        &json!({
            "path": "/w/wi-1-landed",
            "mode": "build_output",
            "force": false,
            "reason": WHY,
        })
    );
    let audited = crate::with_ctx!(&built, |ctx| {
        let view = ctx.declared.read().unwrap();
        view.list_events(0, 500, None)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "workspace.cleaned")
            .count()
    });
    assert_eq!(audited, 1, "one workspace.cleaned event per clean");
}

#[test]
fn an_engine_refusal_is_a_conflict() {
    let rows: Rows = Arc::new(Mutex::new(Some(Vec::new())));
    let log = cleans();
    {
        let mut log = log.lock().unwrap();
        log.status = 409;
        log.refusal = "a live or hibernated session works in this tree".to_string();
    }
    let built = wired_as("clean-409", &rows, &log, "local:ada", ActorKind::Human);
    match clean(
        &built,
        json!({"path": "/w/x", "mode": "remove", "reason": WHY}),
    ) {
        Err(VogtError::Conflict(message)) => {
            assert!(message.contains("live or hibernated"), "{message}")
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn only_a_person_forces_a_removal() {
    let rows: Rows = Arc::new(Mutex::new(Some(Vec::new())));
    let log = cleans();
    let agent = wired_as(
        "clean-force",
        &rows,
        &log,
        "agent:session:ses_x",
        ActorKind::Agent,
    );
    match clean(
        &agent,
        json!({"path": "/w/x", "mode": "remove", "force": true, "reason": WHY}),
    ) {
        Err(VogtError::CleanRefused(message)) => assert_eq!(
            message,
            "only a person forces a workspace removal, not an agent \
             (agent:session:ses_x); ask a person to do it"
        ),
        other => panic!("expected clean_refused, got {other:?}"),
    }
    assert!(log.lock().unwrap().sent.is_empty());
    clean(
        &agent,
        json!({"path": "/w/x", "mode": "remove", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(log.lock().unwrap().sent.last().unwrap()["force"], false);
}

#[test]
fn clean_needs_exactly_one_of_id_and_path() {
    let rows: Rows = Arc::new(Mutex::new(Some(Vec::new())));
    let built = wired("clean-one", &rows);
    for params in [
        json!({"mode": "remove", "reason": WHY}),
        json!({"id": "ses_0001", "path": "/w/x", "mode": "remove", "reason": WHY}),
    ] {
        match clean(&built, params) {
            Err(VogtError::InvalidRequest(message)) => assert_eq!(
                message,
                "give exactly one of `id` (a session) or `path` (a workspace)"
            ),
            other => panic!("expected invalid request, got {other:?}"),
        }
    }
}

#[test]
fn a_session_without_a_workspace_is_not_found() {
    let rows: Rows = Arc::new(Mutex::new(Some(Vec::new())));
    let built = wired("clean-none", &rows);
    match clean(
        &built,
        json!({"id": "ses_0001", "mode": "build_output", "reason": WHY}),
    ) {
        Err(VogtError::NotFound(message)) => {
            assert_eq!(message, "session 'ses_0001' has no workspace")
        }
        other => panic!("expected not found, got {other:?}"),
    }
}
