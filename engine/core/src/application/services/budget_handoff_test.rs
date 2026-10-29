//! Ports `tests/test_budget_handoff.py` (WI-1833, WI-2021).
//!
//! The clock is the step clock `sessions_test::opened` builds, so a restart
//! started after a budget stop is later than the stopped session.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::budget_handoff::{read_git_state, settle_budget_stops, RESTART_NOTE};
use super::preferences::preference_set_op;
use super::sessions::{start_session, StartSessionParams};
use super::sessions_test::{insert_project, opened, script_with, start_params};
use super::work::{comment_work, get_op};
use crate::application::context::Built;
use crate::application::dispatch::BUDGET_RESTARTS_KEY;
use crate::core::{ActorKind, Moment, Origin, Principal, Priority, TrustState, WorkItem, WorkKind};
use crate::storage::interface::{DeclaredStore, ObservedStore, WriteTxn};

const WHY: &str = "budget hand-off test";

fn moment() -> Moment {
    Moment::from_unix(1_700_000_000, 0)
}

fn item(built: &Built, reference: &str, title: &str) -> String {
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

/// Answers POSTs in order (`ids`) and lists every id that has been created as
/// hibernated at its budget, with the cwd and budget object the test set.
struct BudgetScript {
    ids: Mutex<Vec<String>>,
    created: Mutex<Vec<String>>,
    seen: Mutex<Vec<String>>,
    cwd: Mutex<String>,
    budgets: Mutex<Vec<(String, Value)>>,
}

fn budget_script(ids: Vec<&str>) -> Arc<BudgetScript> {
    Arc::new(BudgetScript {
        ids: Mutex::new(ids.into_iter().map(str::to_string).collect()),
        created: Mutex::new(Vec::new()),
        seen: Mutex::new(Vec::new()),
        cwd: Mutex::new("/work/tree".to_string()),
        budgets: Mutex::new(Vec::new()),
    })
}

fn engine_for(script: Arc<BudgetScript>) -> crate::adapters::engine::EngineClient {
    let transport: crate::adapters::engine::Transport =
        Arc::new(move |url, _headers, body, method| {
            let text = String::from_utf8_lossy(body).into_owned();
            script
                .seen
                .lock()
                .unwrap()
                .push(format!("{method} {url} {text}"));
            let path = url.split('?').next().unwrap_or(url);
            if method == "GET" && path.ends_with("/api/sessions") {
                let cwd = script.cwd.lock().unwrap().clone();
                let budgets = script.budgets.lock().unwrap().clone();
                let sessions: Vec<Value> = script
                    .created
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|id| {
                        let budget = budgets
                            .iter()
                            .find(|(key, _)| key == id)
                            .map(|(_, value)| value.clone());
                        json!({
                            "id": id,
                            "name": id,
                            "activity": "hibernated",
                            "cwd": cwd,
                            "alive": false,
                            "budget": budget,
                            "hibernation": {
                                "at": "2026-10-18T00:00:00Z",
                                "trigger": "budget",
                                "reason": "turns 120 of 120",
                            },
                        })
                    })
                    .collect();
                return (
                    200,
                    serde_json::to_vec(&json!({"sessions": sessions})).unwrap(),
                );
            }
            if method == "POST" && path.ends_with("/api/sessions") {
                let id = script
                    .ids
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| "eng-x".to_string());
                script.created.lock().unwrap().push(id.clone());
                return (200, format!(r#"{{"id":"{id}"}}"#).into_bytes());
            }
            (200, b"{}".to_vec())
        });
    crate::adapters::engine::EngineClient::new(
        "http://engine",
        Some("tok".to_string()),
        Some(transport),
    )
}

fn opened_budget(script: Arc<BudgetScript>) -> Built {
    let dir = std::env::temp_dir().join(format!(
        "vogt-bgt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config = crate::config::VogtConfig {
        data_dir: dir,
        ..crate::config::VogtConfig::default()
    };
    let built = crate::application::context::build_context(
        config,
        Some(Principal::new("local:test-user", ActorKind::Human, "Test").unwrap()),
        Some(crate::core::StepClock::new(moment())),
        Some(crate::core::SequentialIds::new(None).unwrap()),
        None,
        Some(engine_for(script)),
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = &built else {
        unreachable!("a step clock and sequential ids");
    };
    ctx.declared.migrate().unwrap();
    ctx.declared
        .bootstrap(&Principal::new("local:test-user", ActorKind::Human, "Test").unwrap())
        .unwrap();
    ctx.observed.migrate().unwrap();
    built
}

fn opt_str(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

fn start_for(built: &Built, reference: &str, extra: Value) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    // A start takes a work item or a project, not both. The item already
    // names the project.
    let mut typed = start_params(None, Some("loop on CI"), WHY);
    typed.work_item = Some(reference.to_string());
    if let Some(object) = extra.as_object() {
        for (key, value) in object {
            match key.as_str() {
                "template" => typed.template = opt_str(value),
                "task" => typed.task = opt_str(value),
                "model" => typed.model = opt_str(value),
                "effort" => typed.effort = opt_str(value),
                "purpose" => typed.purpose = opt_str(value),
                other => panic!("start_for does not set {other}"),
            }
        }
    }
    let _: &StartSessionParams = &typed;
    start_session(ctx, &typed).unwrap();
}

fn comments(built: &Built, reference: &str) -> Vec<Value> {
    get_op(built, json!({"ref": reference, "comment_limit": 50})).unwrap()["comments"]
        .as_array()
        .unwrap()
        .clone()
}

fn repo() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "vogt-git-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&path)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(status.status.success(), "{args:?} {:?}", status);
    };
    git(&["init", "-q", "-b", "wi-9-loop"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "x",
    ]);
    std::fs::write(path.join("half.py"), "print(1)\n").unwrap();
    path
}

#[test]
fn git_state_reads_branch_head_and_dirty_paths() {
    let path = repo();
    let state = read_git_state(path.to_str().unwrap()).unwrap();
    assert_eq!(state.branch.as_deref(), Some("wi-9-loop"));
    assert_eq!(state.head_sha.as_ref().map(String::len), Some(40));
    assert_eq!(state.dirty, vec!["half.py".to_string()]);
    assert!(read_git_state(path.join("missing").to_str().unwrap()).is_none());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn a_budget_stop_without_a_handoff_gets_one_from_git() {
    let script = budget_script(vec!["eng-1"]);
    let path = repo();
    *script.cwd.lock().unwrap() = path.to_string_lossy().into_owned();
    let built = opened_budget(Arc::clone(&script));
    insert_project(&built, "vogt");
    let reference = item(&built, "WI-1", "Long loop");
    preference_set_op(
        &built,
        json!({"key": BUDGET_RESTARTS_KEY, "value": {"value": 0}, "reason": WHY}),
    )
    .unwrap();
    start_for(&built, &reference, json!({"template": "claude"}));

    let Built::StepSequential(ctx) = &built else {
        unreachable!();
    };
    let first = settle_budget_stops(ctx).unwrap();
    let again = settle_budget_stops(ctx).unwrap();
    assert_eq!(first.handoffs_written, vec![reference.clone()]);
    assert!(again.handoffs_written.is_empty());
    assert!(first.restarted.is_empty());
    let handoffs: Vec<Value> = comments(&built, &reference)
        .into_iter()
        .filter(|one| one["kind"] == "handoff")
        .collect();
    assert_eq!(handoffs.len(), 1);
    assert_eq!(handoffs[0]["handoff"]["branch"], "wi-9-loop");
    assert_eq!(handoffs[0]["handoff"]["session"], "eng-1");
    assert_eq!(handoffs[0]["handoff"]["touch_points"], json!(["half.py"]));
    assert!(handoffs[0]["body"]
        .as_str()
        .unwrap()
        .contains("turns 120 of 120"));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn the_agents_own_handoff_is_kept_and_the_item_restarts_once() {
    // Stack: the first POST pops eng-1, the restart pops eng-2.
    let script = budget_script(vec!["eng-2", "eng-1"]);
    let built = opened_budget(Arc::clone(&script));
    insert_project(&built, "vogt");
    let reference = item(&built, "WI-1", "Long loop");
    start_for(&built, &reference, json!({"template": "claude"}));
    let Built::StepSequential(ctx) = &built else {
        unreachable!();
    };
    comment_work(
        ctx,
        json!({
            "ref": reference,
            "body": "rebased, CI red on lint",
            "kind": "handoff",
            "handoff": {"branch": "wi-9-loop", "remaining": ["fix lint"]},
            "reason": WHY,
        }),
    )
    .unwrap();

    let first = settle_budget_stops(ctx).unwrap();
    assert!(first.handoffs_written.is_empty());
    assert_eq!(first.restarted, vec![reference.clone()]);
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(
        asked.contains("rebased, CI red on lint"),
        "the restart prompt did not carry the hand-off: {asked}"
    );

    let second = settle_budget_stops(ctx).unwrap();
    assert_eq!(second.handoffs_written, vec![reference.clone()]);
    assert!(second.restarted.is_empty());
    let notes: Vec<Value> = comments(&built, &reference)
        .into_iter()
        .filter(|one| {
            one["body"]
                .as_str()
                .is_some_and(|body| body.starts_with(RESTART_NOTE))
        })
        .collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(script.created.lock().unwrap().len(), 2);
}

#[test]
fn a_purpose_start_on_haiku_gets_the_larger_budget() {
    let script = script_with(vec![(200, r#"{"id":"eng-1"}"#.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "vogt");
    let reference = item(&built, "WI-1", "Haiku");
    start_for(
        &built,
        &reference,
        json!({"purpose": "implement", "model": "haiku", "task": "do it", "template": null}),
    );
    let asked = script.seen.lock().unwrap().join("\n");
    assert!(
        asked.contains(r#""turns":200"#),
        "haiku did not get the larger budget: {asked}"
    );
}

#[test]
fn a_purpose_started_session_restarts_on_its_own_model_and_budget() {
    let script = budget_script(vec!["eng-2", "eng-1"]);
    script.budgets.lock().unwrap().push((
        "eng-1".to_string(),
        json!({"turns": 200, "wall_clock_minutes": 90}),
    ));
    let built = opened_budget(Arc::clone(&script));
    insert_project(&built, "vogt");
    let reference = item(&built, "WI-1", "Haiku loop");
    start_for(
        &built,
        &reference,
        json!({
            "purpose": "implement",
            "model": "haiku",
            "effort": "high",
            "task": "loop on CI",
            "template": null,
        }),
    );
    let Built::StepSequential(ctx) = &built else {
        unreachable!();
    };
    let first = settle_budget_stops(ctx).unwrap();
    assert_eq!(first.restarted, vec![reference.clone()]);
    let posts: Vec<String> = script
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.starts_with("POST"))
        .cloned()
        .collect();
    let restart = posts.last().unwrap();
    assert!(restart.contains(r#""model":"haiku""#), "{restart}");
    assert!(restart.contains(r#""effort":"high""#), "{restart}");
    assert!(restart.contains(r#""turns":200"#), "{restart}");
    assert!(restart.contains(r#""wall_clock_minutes":90"#), "{restart}");
}
