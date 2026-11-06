//! `work.land` (WI-1328). Ports `tests/test_work_land.py`: land a reviewed
//! branch only at a `no_blockers` verdict for the exact tip, from a reviewer
//! who is not the worker.
//!
//! Verdict comments are inserted straight into the store where a test needs
//! one the comment gate (WI-1319) would refuse: the land re-checks the
//! reviewer's provenance itself and must not rely on that gate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::adapters::engine::EngineClient;
use crate::application::context::{build_context, write_of, Built};
use crate::application::services::work::{comment_op, get_op};
use crate::application::work_land::work_land_op;
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{
    ActorKind, AgentPolicy, CodingSession, Comment, Moment, Origin, Principal, Priority, Project,
    StepClock, TrustState, WorkItem, WorkKind,
};
use crate::errors::VogtError;
use crate::storage::interface::{AuditQuery, DeclaredStore, ReadView, WriteTxn};
use crate::with_ctx;

const WHY: &str = "land test";
const TIP: &str = "abc1234def";
const ROOT: &str = "/work/vogt";

/// What the stand-in engine was sent, the roles it answers with, and the
/// refusal it gives a land when one is set.
#[derive(Clone, Default)]
struct Engine {
    sent: Arc<Mutex<Vec<(String, Value)>>>,
    roles: Arc<Mutex<HashMap<String, String>>>,
    refusal: Arc<Mutex<Option<(u16, String)>>>,
}

impl Engine {
    fn client(&self) -> EngineClient {
        let engine = self.clone();
        EngineClient::new(
            "http://engine",
            None,
            Some(Arc::new(move |url, _, body, method| {
                let path = url.trim_start_matches("http://engine").to_string();
                let sent = serde_json::from_slice(body).unwrap_or(Value::Null);
                if method == "POST" && path == "/api/git/op" {
                    if let Some((status, said)) = engine.refusal.lock().unwrap().clone() {
                        return (status, json!({"error": said}).to_string().into_bytes());
                    }
                    engine.sent.lock().unwrap().push((path, sent));
                    return (
                        200,
                        json!({
                            "ok": true,
                            "onto_before": "base0000",
                            "onto_after": "land2222",
                            "commits": ["land1111", "land2222"],
                            "rebased": true,
                        })
                        .to_string()
                        .into_bytes(),
                    );
                }
                let id = path.trim_start_matches("/api/sessions/");
                let role = engine
                    .roles
                    .lock()
                    .unwrap()
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "worker".to_string());
                (
                    200,
                    json!({"id": id, "alive": true, "activity": "idle", "role": role})
                        .to_string()
                        .into_bytes(),
                )
            })),
        )
    }

    fn lands(&self) -> Vec<Value> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, body)| body["op"] == "land")
            .map(|(_, body)| body.clone())
            .collect()
    }
}

fn person() -> Principal {
    Principal::new("local:someone", ActorKind::Human, "Someone").unwrap()
}

fn at(dir: &Path, principal: Principal, engine: &Engine) -> Built {
    let _env = crate::config::env_lock();
    build_context(
        crate::config::VogtConfig {
            data_dir: dir.to_path_buf(),
            ..crate::config::VogtConfig::default()
        },
        Some(principal),
        Some(StepClock::new(Moment::from_unix(1_700_000_000, 0))),
        None,
        None,
        Some(engine.client()),
        None,
        None,
    )
    .unwrap()
}

fn landing(merge_by: &str) -> AgentPolicy {
    AgentPolicy {
        forbidden_text: vec!["do-not-ship".to_string()],
        landing: Some(json!({
            "base_branch": "main",
            "branch_pattern": "^wi-{n}-[a-z0-9-]+$",
            "worker_lands_by": "push_branch",
            "merge_by": merge_by,
            "merge_mode": "rebase",
            "linear_history": true,
            "deploy_by": "operator",
        })),
        ..AgentPolicy::default()
    }
}

/// Insert project `vogt` under `policy`.
fn set_policy(built: &Built, policy: AgentPolicy) {
    with_ctx!(built, |ctx| {
        let mut writing = write_of(ctx);
        audited_write(&mut writing, "project.update", WHY, move |txn, _| {
            let mut project = Project::new(
                "prj_vogt",
                "vogt",
                "Vogt",
                ROOT,
                Moment::from_unix(1_700_000_000, 0),
            );
            project.agent_policy = policy;
            txn.insert_project(&project)?;
            Ok(WriteOutcome::new(
                (),
                "project",
                "prj_vogt",
                json!({}),
                "project.updated",
                json!({}),
            ))
        })
        .unwrap();
    });
}

fn insert_item(built: &Built, reference: &str) {
    with_ctx!(built, |ctx| {
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_work_item(&WorkItem {
            id: format!("wrk_{reference}"),
            reference: reference.to_string(),
            kind: WorkKind::Feature,
            title: "Land me".to_string(),
            body: String::new(),
            state: "open".to_string(),
            priority: Priority::P2,
            effort: None,
            project_id: Some("prj_vogt".to_string()),
            project_slug: Some("vogt".to_string()),
            initiative_id: None,
            origin: Origin::Created,
            trust_state: TrustState::Unverified,
            assignee_actor_id: None,
            assignee_identity_ref: None,
            labels: Vec::new(),
            relations: Vec::new(),
            superseded_by: None,
            created_at: Moment::from_unix(1_700_000_000, 0),
            updated_at: Moment::from_unix(1_700_000_000, 0),
        })
        .unwrap();
        txn.commit().unwrap();
    });
}

/// An oversight-merged project and WI-1, which worker `eng-ses_worker`
/// handed off on branch `wi-1-land`.
fn wired(name: &str, handoff: bool) -> (PathBuf, Built, Engine) {
    wired_under(name, handoff, "oversight")
}

fn wired_under(name: &str, handoff: bool, merge_by: &str) -> (PathBuf, Built, Engine) {
    let dir = std::env::temp_dir().join(format!("vogt-land-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let _env = crate::config::env_lock();
        let (mut clock, mut ids) = (None, None);
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    }
    let engine = Engine::default();
    let built = at(&dir, person(), &engine);
    set_policy(&built, landing(merge_by));
    insert_item(&built, "WI-1");
    if handoff {
        comment_op(
            &built,
            json!({"ref": "WI-1", "body": "done", "kind": "handoff",
                   "handoff": {"branch": "wi-1-land", "head_sha": TIP,
                               "session": "eng-ses_worker"},
                   "reason": WHY}),
        )
        .unwrap();
    }
    (dir, built, engine)
}

fn actor_of(built: &Built) -> String {
    with_ctx!(built, |ctx| ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity("local:someone")
        .unwrap()
        .unwrap()
        .id)
}

/// Record session `sid` for `item_id`; return an instance acting as it.
fn session(
    dir: &Path,
    built: &Built,
    engine: &Engine,
    item_id: &str,
    sid: &str,
    purpose: Option<&str>,
    started_by: Option<&str>,
) -> Built {
    let actor = actor_of(built);
    with_ctx!(built, |ctx| {
        let mut txn = ctx.declared.write().unwrap();
        txn.insert_session(&CodingSession {
            id: sid.to_string(),
            engine_session_id: format!("eng-{sid}"),
            project_id: "prj_vogt".to_string(),
            work_item_id: Some(item_id.to_string()),
            actor_id: actor.clone(),
            cwd: ROOT.to_string(),
            template: None,
            model: None,
            effort: None,
            reason: WHY.to_string(),
            purpose: purpose.map(str::to_string),
            started_by_session: None,
            started_by_kind: started_by.map(str::to_string),
            started_at: Moment::from_unix(1_700_000_000, 0),
            stopped_at: None,
        })
        .unwrap();
        txn.commit().unwrap();
    });
    let principal = Principal::new(&format!("agent:session:{sid}"), ActorKind::Agent, sid).unwrap();
    at(dir, principal, engine)
}

/// Store a verdict as-is, past the comment gate.
fn verdict(built: &Built, fields: Value) {
    let mut stored = json!({
        "head_sha": TIP,
        "base": null,
        "verdict": "no_blockers",
        "blockers": [],
        "deployment_paths_covered": [],
        "reviewer_session": null,
        "reviewer_started_by": null,
    });
    for (key, value) in fields.as_object().unwrap() {
        stored[key] = value.clone();
    }
    let actor = actor_of(built);
    with_ctx!(built, |ctx| {
        let mut txn = ctx.declared.write().unwrap();
        let count = txn.comments_for("wrk_WI-1", 500, true).unwrap().len();
        txn.insert_comment(&Comment {
            id: format!("cmt_verdict_{count}"),
            work_item_id: "wrk_WI-1".to_string(),
            actor_id: actor.clone(),
            actor_display_name: "reviewer".to_string(),
            body: "review".to_string(),
            // After the hand-off, so newest-first reads it first.
            created_at: Moment::from_unix(1_800_000_000 + count as i64, 0),
            kind: "verdict".to_string(),
            handoff: None,
            verdict: Some(stored),
            verification: None,
        })
        .unwrap();
        txn.commit().unwrap();
    });
}

fn overseer(dir: &Path, built: &Built, engine: &Engine) -> Built {
    engine
        .roles
        .lock()
        .unwrap()
        .insert("eng-ses_over".to_string(), "oversight".to_string());
    session(
        dir,
        built,
        engine,
        "wrk_WI-1",
        "ses_over",
        None,
        Some("oversight"),
    )
}

/// WI-1 reviewed by `ses_rev` (a review oversight started, unless the
/// arguments say otherwise), its verdict stored.
fn reviewed(dir: &Path, built: &Built, engine: &Engine, purpose: &str, started_by: &str) {
    session(
        dir,
        built,
        engine,
        "wrk_WI-1",
        "ses_rev",
        Some(purpose),
        Some(started_by),
    );
    verdict(built, json!({"reviewer_session": "eng-ses_rev"}));
}

fn land(built: &Built, head: &str) -> Result<Value, VogtError> {
    work_land_op(
        built,
        json!({"ref": "WI-1", "head_sha": head, "reason": WHY}),
    )
}

fn audits(built: &Built) -> usize {
    with_ctx!(built, |ctx| ctx
        .declared
        .read()
        .unwrap()
        .list_audit(&AuditQuery {
            limit: 100,
            offset: 0,
            actor_id: None,
            operation: Some("work.land".to_string()),
            entity_id: None,
            project_id: None,
            since: None,
            until: None,
        })
        .unwrap()
        .len())
}

fn events(built: &Built, kind: &str) -> Vec<Value> {
    with_ctx!(built, |ctx| ctx
        .declared
        .read()
        .unwrap()
        .list_events(0, 1000, None)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == kind)
        .map(|event| event.summary)
        .collect())
}

fn notes(built: &Built) -> Vec<String> {
    get_op(built, json!({"ref": "WI-1"})).unwrap()["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].as_str().unwrap().to_string())
        .collect()
}

/// The land is refused with `rule` in its message, audited, and an event
/// names the rule.
fn refused(built: &Built, result: Result<Value, VogtError>, rule: &str) {
    match result {
        Err(VogtError::LandRefused(message)) => {
            assert!(message.contains(rule), "{message}");
        }
        other => panic!("expected land_refused, got {other:?}"),
    }
    assert!(audits(built) > 0, "a refusal is audited");
    let refusals = events(built, "work.land_refused");
    assert!(refusals.last().unwrap()["refused"]
        .as_str()
        .is_some_and(|r| !r.is_empty()));
}

#[test]
fn an_overseer_lands_at_an_oversight_reviewers_verdict() {
    let (dir, built, engine) = wired("overseer", true);
    reviewed(&dir, &built, &engine, "review", "oversight");
    let result = land(&overseer(&dir, &built, &engine), TIP).unwrap();
    assert_eq!(
        result,
        json!({"onto_before": "base0000", "onto_after": "land2222",
               "commits": ["land1111", "land2222"], "rebased": true})
    );
    assert_eq!(
        engine.lands(),
        vec![json!({
            "op": "land",
            "repo": ROOT,
            "branch": "wi-1-land",
            "head_sha": TIP,
            "onto": "main",
            "mode": "rebase",
            "forbidden_text": ["do-not-ship"],
            "linear_history": true,
        })]
    );
    assert!(notes(&built)
        .contains(&"landed land2222 onto main (2 commits; verdict by eng-ses_rev)".to_string()));
    let landed = events(&built, "work.landed");
    assert_eq!(landed.last().unwrap()["reviewer_session"], "eng-ses_rev");
    assert!(audits(&built) > 0);
}

#[test]
fn a_person_lands_at_a_persons_verdict() {
    let (_dir, built, _engine) = wired("person", true);
    verdict(&built, json!({}));
    assert_eq!(land(&built, TIP).unwrap()["onto_after"], "land2222");
}

#[test]
fn a_worker_session_is_refused() {
    let (dir, built, engine) = wired("worker", true);
    reviewed(&dir, &built, &engine, "review", "oversight");
    let worker = session(
        &dir,
        &built,
        &engine,
        "wrk_WI-1",
        "ses_worker",
        Some("implement"),
        None,
    );
    refused(
        &built,
        land(&worker, TIP),
        "only a person or an oversight session lands a branch (eng-ses_worker is worker)",
    );
    assert!(engine.lands().is_empty());
}

#[test]
fn an_operator_merged_project_is_refused() {
    let (dir, built, engine) = wired_under("operator", true, "operator");
    reviewed(&dir, &built, &engine, "review", "oversight");
    let caller = overseer(&dir, &built, &engine);
    refused(
        &built,
        land(&caller, TIP),
        "WI-1 not landed: this project's merges are the operator's",
    );
}

#[test]
fn no_branch_is_refused() {
    let (_dir, built, _engine) = wired("nobranch", false);
    refused(&built, land(&built, TIP), "no hand-off names a branch");
}

#[test]
fn a_stale_sha_is_refused() {
    let (dir, built, engine) = wired("stale", true);
    reviewed(&dir, &built, &engine, "review", "oversight");
    let caller = overseer(&dir, &built, &engine);
    refused(
        &built,
        land(&caller, "fffffff1"),
        "the latest verdict is for abc1234def, not fffffff1; the branch moved since the review",
    );
}

#[test]
fn a_blockers_verdict_is_refused() {
    let (_dir, built, _engine) = wired("blockers", true);
    verdict(
        &built,
        json!({"verdict": "blockers", "blockers": ["tests"]}),
    );
    refused(&built, land(&built, TIP), "has blockers");
}

#[test]
fn no_verdict_is_refused() {
    let (_dir, built, _engine) = wired("noverdict", true);
    refused(&built, land(&built, TIP), "there is no review verdict");
}

#[test]
fn the_workers_own_verdict_is_refused() {
    let (dir, built, engine) = wired("own", true);
    session(
        &dir,
        &built,
        &engine,
        "wrk_WI-1",
        "ses_worker",
        Some("review"),
        Some("oversight"),
    );
    verdict(&built, json!({"reviewer_session": "eng-ses_worker"}));
    refused(&built, land(&built, TIP), "the verdict is the worker's own");
}

#[test]
fn a_worker_started_reviewer_is_refused() {
    let (dir, built, engine) = wired("startedby", true);
    reviewed(&dir, &built, &engine, "review", "worker");
    refused(
        &built,
        land(&built, TIP),
        "the reviewer session eng-ses_rev was started by worker",
    );
}

#[test]
fn a_reviewer_of_another_item_is_refused() {
    let (dir, built, engine) = wired("otheritem", true);
    insert_item(&built, "WI-2");
    session(
        &dir,
        &built,
        &engine,
        "wrk_WI-2",
        "ses_rev",
        Some("review"),
        Some("oversight"),
    );
    verdict(&built, json!({"reviewer_session": "eng-ses_rev"}));
    refused(&built, land(&built, TIP), "not bound to WI-1");
}

#[test]
fn an_unknown_reviewer_session_is_refused() {
    let (_dir, built, _engine) = wired("unknown", true);
    verdict(&built, json!({"reviewer_session": "eng-ghost"}));
    refused(
        &built,
        land(&built, TIP),
        "eng-ghost is not in the session store",
    );
}

#[test]
fn a_non_review_session_verdict_is_refused() {
    let (dir, built, engine) = wired("nonreview", true);
    reviewed(&dir, &built, &engine, "implement", "oversight");
    refused(&built, land(&built, TIP), "is not a review");
}

#[test]
fn the_engines_refusal_is_surfaced_verbatim() {
    let (dir, built, engine) = wired("engine", true);
    reviewed(&dir, &built, &engine, "review", "oversight");
    *engine.refusal.lock().unwrap() = Some((
        400,
        "wi-1-land moved: tip is 999 not abc1234def".to_string(),
    ));
    match land(&built, TIP) {
        Err(VogtError::InvalidRequest(message)) => {
            assert!(message.contains("tip is 999 not abc1234def"), "{message}");
        }
        other => panic!("expected invalid_request, got {other:?}"),
    }
    assert!(!notes(&built).iter().any(|n| n.starts_with("landed ")));
}

#[test]
fn a_short_head_sha_is_refused_by_the_validator() {
    let err = crate::registry::validate::prepare(
        "work.land",
        json!({"ref": "WI-1", "head_sha": "abc", "reason": WHY}),
    )
    .unwrap_err();
    assert!(err.message().contains("at least 7 characters"), "{err:?}");
}
