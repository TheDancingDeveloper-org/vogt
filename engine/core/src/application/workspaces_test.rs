//! Ports `tests/test_workspaces.py` (`workspace.list`, WI-1340): engine
//! workspace rows joined to Vogt's project, session and merged state.

use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::adapters::engine::EngineClient;
use crate::application::context::{build_context, Built};
use crate::application::writes::audited_write;
use crate::core::{ActorKind, CodingSession, Moment, Principal, Project, StepClock, SweepOutcome};
use crate::errors::VogtError;
use crate::registry::default_registry;
use crate::storage::interface::{ObservedStore, WriteTxn};
use crate::storage::observed_types::PendingObservation;

const ROOT: &str = "/srv/estate/vogt";
const UNLINKED: &str = "0b0e5f0c-1111-4222-8333-944455556666";

type Rows = Arc<Mutex<Option<Vec<Value>>>>;

/// An engine that answers `GET /api/workspaces` with `rows` (a 404 when
/// `None`); anything else fails like an engine that is down.
fn engine(rows: &Rows) -> EngineClient {
    let rows = Arc::clone(rows);
    EngineClient::new(
        "http://engine.test",
        None,
        Some(Arc::new(move |url, _, _, method| {
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

fn wired(name: &str, rows: &Rows) -> Built {
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
    let ada = Principal::new("local:ada", ActorKind::Human, "Ada").unwrap();
    let built = build_context(
        config,
        Some(ada),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
        None,
        None,
        Some(engine(rows)),
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
