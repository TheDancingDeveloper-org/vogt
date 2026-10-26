//! The landing rule against a start's task, and oversight answers.
//!
//! Ports the `tests/test_sessions.py` cases of WI-1832 (the policy overrides
//! a contradicting task, in the brief and in `effective_policy`) and WI-1841
//! (the base resolved per initiative and per named long-lived branch, and an
//! oversight session's own landing work keeps its merge), and the
//! `tests/test_session_drive.py` case of WI-1831 (an oversight session's
//! answer is relayed by its engine id, not as a person's).

use std::sync::Arc;

use serde_json::{json, Value};

use super::sessions::{answer, start_session, AnswerParams, StartSessionResult};
use super::sessions_test::{moment, opened, opened_as, script_with, start_params, Script};
use crate::application::context::Built;
use crate::core::{
    ActorKind, AgentPolicy, Initiative, InitiativeState, LinkState, Origin, Principal, Priority,
    Project, TrustState, WorkItem, WorkKind,
};
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const WHY: &str = "landing policy test";

const STARTED: &str = r#"{"id":"eng-1","name":"vogt","cwd":"/work/tree"}"#;

/// What `get_session` answers for the calling session, in `role`.
fn caller_as(role: &str) -> String {
    format!(
        r#"{{"id":"eng-parent","name":"parent","cwd":"/work/tree","activity":"idle","alive":true,"autopilot":false,"autopilot_nudges":0,"keep_awake":false,"role":"{role}"}}"#
    )
}

/// Python's `_push_only_landing`, with `landing` laid over it.
fn landing_policy(landing: Value) -> AgentPolicy {
    let mut rule = json!({
        "base_branch": "main",
        "worker_lands_by": "push_branch",
        "merge_by": "oversight",
        "merge_mode": "rebase",
    });
    for (key, value) in landing.as_object().unwrap() {
        rule[key] = value.clone();
    }
    serde_json::from_value(json!({"base_branch": "main", "landing": rule})).unwrap()
}

/// The project `vogt` under `policy`, and its item WI-1 (in `initiative`).
fn seeded(built: &Built, policy: AgentPolicy, initiative: Option<&str>) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut project = Project::new("prj_vogt", "vogt", "vogt", "/work/tree", moment());
    project.link_state = LinkState::Unlinked;
    project.agent_policy = policy;
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    if let Some(slug) = initiative {
        txn.insert_initiative(&Initiative {
            id: format!("ini_{slug}"),
            slug: slug.to_string(),
            title: "Rust port".to_string(),
            body: String::new(),
            state: InitiativeState::Open,
            weight: 0,
            created_at: moment(),
            updated_at: moment(),
        })
        .unwrap();
    }
    txn.insert_work_item(&WorkItem {
        id: "wrk_1".to_string(),
        reference: "WI-1".to_string(),
        kind: WorkKind::Feature,
        title: "land me".to_string(),
        body: String::new(),
        state: "open".to_string(),
        priority: Priority::P2,
        effort: None,
        project_id: Some("prj_vogt".to_string()),
        project_slug: Some("vogt".to_string()),
        initiative_id: initiative.map(|slug| format!("ini_{slug}")),
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

/// Start a session on WI-1 with `task`, as `built`'s principal.
fn start(built: &Built, task: &str, worktree: bool, purpose: Option<&str>) -> StartSessionResult {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(None, Some(task), WHY);
    params.work_item = Some("WI-1".to_string());
    params.worktree = worktree.then_some(true);
    params.purpose = purpose.map(str::to_string);
    start_session(ctx, &params).unwrap()
}

/// The body of the create request the engine was sent.
fn spec(script: &Script) -> Value {
    let seen = script.seen.lock().unwrap();
    let line = seen
        .iter()
        .find(|line| line.starts_with("POST http://engine/api/sessions "))
        .expect("a create request");
    let body = line.splitn(3, ' ').nth(2).unwrap();
    serde_json::from_str(body).unwrap()
}

fn prompt(script: &Script) -> String {
    spec(script)["prompt"].as_str().unwrap().to_string()
}

#[test]
fn a_task_asking_for_a_pr_on_a_push_only_project_is_overridden() {
    // WI-1832: the policy wins, the brief says so, the start result shows it.
    let script = script_with(vec![(200, STARTED.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    seeded(&built, landing_policy(json!({})), None);
    let result = start(&built, "Fix it, then open a PR against main.", false, None);
    let brief = prompt(&script);
    assert!(brief.contains("## Policy overrides task"), "{brief}");
    assert!(brief.contains("Policy overrides task: push only."));
    assert!(brief.find("## Policy overrides task").unwrap() < brief.find("## Task").unwrap());
    let policy = result.effective_policy.expect("an effective policy");
    assert_eq!(policy.lands_by, "push_branch");
    assert_eq!(policy.base_branch, "main");
    assert_eq!(policy.overrides.len(), 1);
    assert!(policy.overrides[0].starts_with("Policy overrides task: push only."));
    let wire = serde_json::to_value(&policy).unwrap();
    let keys: Vec<&String> = wire.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "base_branch",
            "lands_by",
            "merge_by",
            "merge_mode",
            "deploy_by",
            "overrides"
        ]
    );
}

#[test]
fn a_task_that_agrees_with_the_policy_adds_no_override() {
    let script = script_with(vec![(200, STARTED.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    seeded(&built, landing_policy(json!({})), None);
    let result = start(
        &built,
        "Open NO PR, push the branch only. Never merge or deploy.",
        false,
        None,
    );
    assert!(!prompt(&script).contains("Policy overrides task"));
    assert_eq!(
        result.effective_policy.unwrap().overrides,
        Vec::<String>::new()
    );
}

#[test]
fn a_project_without_a_landing_rule_has_no_effective_policy() {
    let script = script_with(vec![(200, STARTED.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    seeded(&built, AgentPolicy::default(), None);
    let result = start(&built, "Open a PR and merge it.", false, None);
    assert!(result.effective_policy.is_none());
    assert!(!prompt(&script).contains("Policy overrides task"));
    let wire = serde_json::to_value(&result).unwrap();
    assert_eq!(wire["effective_policy"], Value::Null);
}

#[test]
fn an_initiative_base_replaces_the_project_base() {
    // WI-1841: the Rust port's items cut from and land onto `rust-core`.
    let script = script_with(vec![(200, STARTED.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    seeded(
        &built,
        landing_policy(json!({"initiative_bases": {"rust-port": "rust-core"}})),
        Some("rust-port"),
    );
    let result = start(
        &built,
        "Cut a branch from rust-core and land it onto rust-core.",
        true,
        None,
    );
    let brief = prompt(&script);
    assert!(!brief.contains("Policy overrides task"), "{brief}");
    assert!(brief.contains("**Base** `rust-core`"));
    assert_eq!(spec(&script)["worktree"]["base_ref"], "rust-core");
    let policy = result.effective_policy.unwrap();
    assert_eq!(policy.base_branch, "rust-core");
    assert_eq!(policy.overrides, Vec::<String>::new());
}

#[test]
fn an_initiative_base_is_found_by_the_initiative_id_too() {
    let script = script_with(vec![(200, STARTED.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    seeded(
        &built,
        landing_policy(json!({"initiative_bases": {"ini_rust-port": "rust-core"}})),
        Some("rust-port"),
    );
    let result = start(&built, "Port it.", true, None);
    assert_eq!(spec(&script)["worktree"]["base_ref"], "rust-core");
    assert_eq!(result.effective_policy.unwrap().base_branch, "rust-core");
}

#[test]
fn a_task_naming_a_known_long_lived_branch_is_taken_at_its_word() {
    let script = script_with(vec![(200, STARTED.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    seeded(
        &built,
        landing_policy(json!({"long_lived_branches": ["rust-core"]})),
        None,
    );
    let result = start(&built, "Branch off rust-core and fix it.", false, None);
    let policy = result.effective_policy.unwrap();
    assert_eq!(policy.base_branch, "rust-core");
    assert_eq!(policy.overrides, Vec::<String>::new());
    assert!(prompt(&script).contains("**Base** `rust-core`"));
}

#[test]
fn an_unknown_base_in_a_task_is_still_overridden() {
    let script = script_with(vec![(200, STARTED.to_string())]);
    let built = opened(Some(Arc::clone(&script)));
    seeded(
        &built,
        landing_policy(json!({"long_lived_branches": ["rust-core"]})),
        None,
    );
    let result = start(&built, "Branch off develop and fix it.", false, None);
    let policy = result.effective_policy.unwrap();
    assert_eq!(policy.base_branch, "main");
    assert!(policy
        .overrides
        .iter()
        .any(|line| line.contains("base `main`")));
}

#[test]
fn oversight_landing_work_is_not_told_not_to_merge() {
    // WI-1841: where oversight merges, its own landing task keeps its merge.
    for (role, purpose, overridden) in [
        ("oversight", "mechanical", false),
        ("oversight", "implement", true),
        ("worker", "mechanical", true),
    ] {
        // The engine answers the caller's own lookup first, then the start.
        let script = script_with(vec![(200, STARTED.to_string()), (200, caller_as(role))]);
        let built = opened_as(
            Some(Arc::clone(&script)),
            Principal::new("agent:engine:eng-parent", ActorKind::Agent, "Parent").unwrap(),
        );
        seeded(&built, landing_policy(json!({})), None);
        let result = start(
            &built,
            "Rebase the branch, then merge it into main.",
            false,
            Some(purpose),
        );
        let merged = result
            .effective_policy
            .unwrap()
            .overrides
            .iter()
            .any(|line| line.contains("do not merge"));
        assert_eq!(merged, overridden, "{role} {purpose}");
        assert_eq!(prompt(&script).contains("do not merge"), overridden);
    }
}

/// The summary of the one `session.answered` event.
fn answered_audit(built: &Built) -> Value {
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
        .find(|event| event.kind == "session.answered")
        .expect("a session.answered event")
        .summary
}

fn answer_as(role: &str) -> (Arc<Script>, Built) {
    let script = script_with(vec![
        (
            200,
            r#"{"question":"Run git status?","kind":"permission","chosen":{"number":1,"label":"Yes"}}"#
                .to_string(),
        ),
        (200, caller_as(role)),
    ]);
    let built = opened_as(
        Some(Arc::clone(&script)),
        Principal::new("agent:engine:eng-parent", ActorKind::Agent, "Parent").unwrap(),
    );
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let result = answer(
        ctx,
        &AnswerParams {
            id: "eng-worker".to_string(),
            option: Some(1),
            label: None,
            expect_question: None,
            reason: WHY.to_string(),
        },
    )
    .unwrap();
    assert_eq!(result["kind"], "permission");
    (script, built)
}

#[test]
fn an_oversight_session_answers_a_permission_prompt_and_is_audited() {
    // WI-1831: relayed by its engine id, not as a person, so the engine can
    // hold it to allow-listed commands in the worker's own worktree.
    let (script, built) = answer_as("oversight");
    let seen = script.seen.lock().unwrap();
    let sent = seen
        .iter()
        .find(|line| line.contains("/answer"))
        .expect("an answer request");
    assert!(
        sent.contains(r#""person":false,"overseer":"eng-parent""#),
        "{sent}"
    );
    let audit = answered_audit(&built);
    assert_eq!(audit["person"], false);
    assert_eq!(audit["oversight"], true);
}

#[test]
fn a_worker_session_answer_names_no_overseer() {
    let (script, built) = answer_as("worker");
    let seen = script.seen.lock().unwrap();
    let sent = seen.iter().find(|line| line.contains("/answer")).unwrap();
    assert!(!sent.contains("overseer"), "{sent}");
    let audit = answered_audit(&built);
    assert_eq!(audit["person"], false);
    assert_eq!(audit["oversight"], false);
}
