//! Ports `tests/test_budget_handoff.py` (WI-1833): budget stops leave a
//! hand-off and continue from it.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::budget_handoff::{read_git_state, settle_budget_stops, RESTART_NOTE};
use super::preferences::preference_set_op;
use super::sessions::{start_session, StartSessionParams};
use super::sessions_test::{insert_project, moment};
use super::work::{comment_op, get_op};
use crate::adapters::engine::EngineClient;
use crate::application::context::{build_context, Built};
use crate::application::dispatch;
use crate::core::{
    ActorKind, Origin, Principal, Priority, SequentialIds, StepClock, TrustState, WorkItem,
    WorkKind,
};
use crate::storage::interface::{DeclaredStore, ObservedStore, WriteTxn};

const WHY: &str = "budget hand-off test";

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "vogt-budget-{name}-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Python `BudgetEngine` over `StandInEngine`: starts sessions `eng-N`, and
/// lists the ones in `stopped` as hibernated at their budget.
#[derive(Default)]
struct BudgetEngine {
    counter: u64,
    alive: Vec<(String, String)>,
    stopped: Vec<(String, String)>,
    budgets: BTreeMap<String, Value>,
    last_spec: Value,
}

impl BudgetEngine {
    fn stop_at_budget(&mut self, engine_id: &str, cwd: &str) {
        self.alive.retain(|(id, _)| id != engine_id);
        self.stopped.push((engine_id.to_string(), cwd.to_string()));
    }

    fn answer(&mut self, url: &str, body: &[u8], method: &str) -> (u16, Vec<u8>) {
        let path = url.split('?').next().unwrap_or(url);
        if method == "POST" && path.ends_with("/api/sessions") {
            self.counter += 1;
            let id = format!("eng-{}", self.counter);
            let spec: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            let cwd = spec["cwd"].as_str().unwrap_or("/work/tree").to_string();
            self.alive.push((id.clone(), cwd.clone()));
            self.last_spec = spec;
            let reply = json!({"id": id, "name": id, "cwd": cwd});
            return (200, reply.to_string().into_bytes());
        }
        if method == "GET" && path.ends_with("/api/sessions") {
            let mut sessions: Vec<Value> = self
                .alive
                .iter()
                .map(|(id, cwd)| {
                    json!({"id": id, "name": id, "activity": "idle", "cwd": cwd, "alive": true})
                })
                .collect();
            sessions.extend(self.stopped.iter().map(|(id, cwd)| {
                json!({
                    "id": id,
                    "name": id,
                    "activity": "hibernated",
                    "cwd": cwd,
                    "alive": false,
                    "budget": self.budgets.get(id).cloned().unwrap_or(Value::Null),
                    "hibernation": {
                        "at": "2026-10-18T00:00:00Z",
                        "trigger": "budget",
                        "reason": "turns 120 of 120",
                    },
                })
            }));
            return (200, json!({"sessions": sessions}).to_string().into_bytes());
        }
        (200, b"{}".to_vec())
    }
}

fn opened(engine: &Arc<Mutex<BudgetEngine>>) -> Built {
    let shared = Arc::clone(engine);
    let transport: crate::adapters::engine::Transport =
        Arc::new(move |url, _headers, body, method| {
            shared.lock().unwrap().answer(url, body, method)
        });
    let client = EngineClient::new("http://engine", Some("tok".to_string()), Some(transport));
    let config = crate::config::VogtConfig {
        data_dir: scratch("data"),
        ..crate::config::VogtConfig::default()
    };
    let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
    let built = build_context(
        config,
        Some(principal.clone()),
        Some(StepClock::new(moment())),
        Some(SequentialIds::new(None).unwrap()),
        None,
        Some(client),
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    ctx.declared.migrate().unwrap();
    ctx.declared.bootstrap(&principal).unwrap();
    ctx.observed.migrate().unwrap();
    built
}

/// A native work item in the `vogt` project.
fn native_item(built: &Built, reference: &str, title: &str) -> String {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_work_item(&WorkItem {
        id: format!("wrk_{reference}"),
        reference: reference.to_string(),
        kind: WorkKind::Feature,
        title: title.to_string(),
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
        created_at: moment(),
        updated_at: moment(),
    })
    .unwrap();
    txn.commit().unwrap();
    reference.to_string()
}

fn start_params(reference: &str, task: &str) -> StartSessionParams {
    StartSessionParams {
        project: None,
        work_item: Some(reference.to_string()),
        name: None,
        template: None,
        task: Some(task.to_string()),
        reason: WHY.to_string(),
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

fn start(built: &Built, params: &StartSessionParams) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    start_session(ctx, params).unwrap();
}

fn settle(built: &Built) -> super::budget_handoff::BudgetSettlement {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    settle_budget_stops(ctx).unwrap()
}

/// Python `_setup`: a project, an item, and a template start on it.
fn setup() -> (Built, Arc<Mutex<BudgetEngine>>, String) {
    let engine = Arc::new(Mutex::new(BudgetEngine::default()));
    let built = opened(&engine);
    insert_project(&built, "vogt");
    let reference = native_item(&built, "WI-9", "Long loop");
    let mut params = start_params(&reference, "loop on CI");
    params.template = Some("claude".to_string());
    start(&built, &params);
    (built, engine, reference)
}

fn comments(built: &Built, reference: &str) -> Vec<Value> {
    get_op(built, json!({"ref": reference, "comment_limit": 50})).unwrap()["comments"]
        .as_array()
        .unwrap()
        .clone()
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Python `_repo`: a checkout on `wi-9-loop` with one commit and one
/// untracked file.
fn repo() -> PathBuf {
    let dir = scratch("repo");
    git(&dir, &["init", "-q", "-b", "wi-9-loop"]);
    git(
        &dir,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ],
    );
    std::fs::write(dir.join("half.py"), "print(1)\n").unwrap();
    dir
}

#[test]
fn git_state_reads_branch_head_and_dirty_paths() {
    let dir = repo();
    let state = read_git_state(dir.to_str().unwrap()).unwrap();
    assert_eq!(state.branch.as_deref(), Some("wi-9-loop"));
    assert_eq!(state.head_sha.as_deref().map(str::len), Some(40));
    assert_eq!(state.dirty, vec!["half.py".to_string()]);
    assert!(read_git_state(dir.join("missing").to_str().unwrap()).is_none());
}

#[test]
fn a_budget_stop_without_a_handoff_gets_one_from_git() {
    let (built, engine, reference) = setup();
    preference_set_op(
        &built,
        json!({"key": dispatch::BUDGET_RESTARTS_KEY, "value": {"value": 0}, "reason": WHY}),
    )
    .unwrap();
    let dir = repo();
    engine
        .lock()
        .unwrap()
        .stop_at_budget("eng-1", dir.to_str().unwrap());

    let first = settle(&built);
    let again = settle(&built);

    assert_eq!(first.handoffs_written, vec![reference.clone()]);
    assert!(again.handoffs_written.is_empty());
    assert!(first.restarted.is_empty());
    let handoffs: Vec<Value> = comments(&built, &reference)
        .into_iter()
        .filter(|one| one["kind"] == "handoff")
        .collect();
    assert_eq!(handoffs.len(), 1);
    let fields = &handoffs[0]["handoff"];
    assert_eq!(
        (&fields["branch"], &fields["session"]),
        (&json!("wi-9-loop"), &json!("eng-1"))
    );
    assert_eq!(fields["touch_points"], json!(["half.py"]));
    assert!(handoffs[0]["body"]
        .as_str()
        .unwrap()
        .contains("turns 120 of 120"));
}

#[test]
fn the_agents_own_handoff_is_kept_and_the_item_restarts_once() {
    let (built, engine, reference) = setup();
    comment_op(
        &built,
        json!({
            "ref": reference,
            "body": "rebased, CI red on lint",
            "kind": "handoff",
            "handoff": {"branch": "wi-9-loop", "remaining": ["fix lint"]},
            "reason": WHY,
        }),
    )
    .unwrap();
    let dir = repo();
    let cwd = dir.to_str().unwrap().to_string();
    engine.lock().unwrap().stop_at_budget("eng-1", &cwd);

    let first = settle(&built);
    assert!(first.handoffs_written.is_empty());
    assert_eq!(first.restarted, vec![reference.clone()]);
    let prompt = engine.lock().unwrap().last_spec["prompt"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt.contains("rebased, CI red on lint"), "{prompt}");

    // The restart is itself stopped at its budget: the cap of one is spent.
    engine.lock().unwrap().stop_at_budget("eng-2", &cwd);
    let second = settle(&built);
    assert_eq!(second.handoffs_written, vec![reference.clone()]);
    assert!(second.restarted.is_empty());
    let notes = comments(&built, &reference)
        .into_iter()
        .filter(|one| one["body"].as_str().unwrap().starts_with(RESTART_NOTE))
        .count();
    assert_eq!(notes, 1);
    assert_eq!(engine.lock().unwrap().counter, 2);
}

#[test]
fn a_purpose_start_on_haiku_gets_the_larger_budget() {
    let engine = Arc::new(Mutex::new(BudgetEngine::default()));
    let built = opened(&engine);
    insert_project(&built, "vogt");
    let reference = native_item(&built, "WI-10", "Haiku");
    let mut params = start_params(&reference, "do it");
    params.purpose = Some("implement".to_string());
    params.model = Some("haiku".to_string());
    start(&built, &params);
    assert_eq!(engine.lock().unwrap().last_spec["budget"]["turns"], 200);
}

#[test]
fn a_purpose_started_session_restarts_on_its_own_model_and_budget() {
    let engine = Arc::new(Mutex::new(BudgetEngine::default()));
    let built = opened(&engine);
    insert_project(&built, "vogt");
    let reference = native_item(&built, "WI-11", "Haiku loop");
    let mut params = start_params(&reference, "loop on CI");
    params.purpose = Some("implement".to_string());
    params.model = Some("haiku".to_string());
    params.effort = Some("high".to_string());
    start(&built, &params);
    let dir = repo();
    {
        let mut engine = engine.lock().unwrap();
        engine.budgets.insert(
            "eng-1".to_string(),
            json!({"turns": 200, "wall_clock_minutes": 90}),
        );
        engine.stop_at_budget("eng-1", dir.to_str().unwrap());
    }

    let settled = settle(&built);

    assert_eq!(settled.restarted, vec![reference]);
    let spec = engine.lock().unwrap().last_spec.clone();
    assert_eq!(
        spec["budget"],
        json!({"turns": 200, "wall_clock_minutes": 90})
    );
    assert_eq!(spec["model"], "haiku");
    assert_eq!(spec["effort"], "high");
}
