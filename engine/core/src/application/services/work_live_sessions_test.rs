//! `work.transition` into a finished state, and `work.get` with `include=all`,
//! name the sessions still bound to the item (WI-4746, WI-998 L2). A warning,
//! never a refusal: the transition succeeds and nothing is unbound.

use std::sync::Arc;

use serde_json::{json, Value};

use super::sessions_test::{moment, opened, script_with, Script};
use super::work::{get_op, transition_op};
use crate::application::context::Built;
use crate::core::{
    ActorKind, CodingSession, LinkState, Origin, Priority, Project, TrustState, WorkItem, WorkKind,
};
use crate::storage::interface::{DeclaredStore, WriteTxn};

const WHY: &str = "live sessions test";

const ENGINE_ROWS: &str = r#"{"sessions":[
    {"id":"eng-live","name":"live","cwd":"/w","activity":"idle","alive":true},
    {"id":"eng-asleep","name":"asleep","cwd":"/w","activity":"hibernated","alive":false,"hibernated":true},
    {"id":"eng-gone","name":"gone","cwd":"/w","activity":"exited","alive":false}
]}"#;

fn session(id: &str, engine: &str, item: &str) -> CodingSession {
    CodingSession {
        id: id.to_string(),
        engine_session_id: engine.to_string(),
        project_id: "prj_vogt".to_string(),
        work_item_id: Some(item.to_string()),
        actor_id: "act_w".to_string(),
        cwd: "/w".to_string(),
        template: None,
        model: None,
        effort: None,
        reason: "bound".to_string(),
        purpose: None,
        started_by_session: None,
        started_by_kind: None,
        started_at: moment(),
        stopped_at: None,
    }
}

fn item(id: &str, reference: &str) -> WorkItem {
    WorkItem {
        id: id.to_string(),
        reference: reference.to_string(),
        kind: WorkKind::Feature,
        title: "Live sessions".to_string(),
        body: String::new(),
        state: "in_progress".to_string(),
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
    }
}

/// WI-1 has a live, a hibernated and an exited session bound; WI-2 has none.
fn built(script: &Arc<Script>) -> Built {
    let built = opened(Some(Arc::clone(script)));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut project = Project::new("prj_vogt", "vogt", "vogt", "/work/tree", moment());
    project.link_state = LinkState::Unlinked;
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    txn.insert_actor(&crate::core::Actor {
        id: "act_w".to_string(),
        kind: ActorKind::Agent,
        display_name: "Worker".to_string(),
        identity_ref: "agent:worker".to_string(),
        disabled: false,
        created_at: moment(),
    })
    .unwrap();
    txn.insert_work_item(&item("wrk_1", "WI-1")).unwrap();
    txn.insert_work_item(&item("wrk_2", "WI-2")).unwrap();
    txn.insert_session(&session("ses_live", "eng-live", "wrk_1"))
        .unwrap();
    txn.insert_session(&session("ses_asleep", "eng-asleep", "wrk_1"))
        .unwrap();
    txn.insert_session(&session("ses_gone", "eng-gone", "wrk_1"))
        .unwrap();
    txn.commit().unwrap();
    built
}

fn ids(value: &Value) -> Vec<String> {
    let mut ids: Vec<String> = value["live_sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn finishing_an_item_names_its_live_and_hibernated_sessions() {
    let script = script_with(vec![(200, ENGINE_ROWS.to_string())]);
    let built = built(&script);
    let done = transition_op(
        &built,
        json!({"ref": "WI-1", "to_state": "done", "walk": true, "reason": WHY}),
    )
    .unwrap();
    assert_eq!(done["item"]["state"], "done", "{done}");
    assert_eq!(ids(&done), ["ses_asleep", "ses_live"], "{done}");
}

#[test]
fn finishing_an_item_with_no_bound_session_warns_of_nothing() {
    let script = script_with(vec![(200, ENGINE_ROWS.to_string())]);
    let built = built(&script);
    let done = transition_op(
        &built,
        json!({"ref": "WI-2", "to_state": "done", "walk": true, "reason": WHY}),
    )
    .unwrap();
    assert_eq!(done["item"]["state"], "done", "{done}");
    assert_eq!(done["live_sessions"], json!([]), "{done}");
}

#[test]
fn a_move_to_an_unfinished_state_does_not_warn() {
    let script = script_with(vec![(200, ENGINE_ROWS.to_string())]);
    let built = built(&script);
    let moved = transition_op(
        &built,
        json!({"ref": "WI-1", "to_state": "review", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(moved["item"]["state"], "review", "{moved}");
    assert_eq!(moved["live_sessions"], json!([]), "{moved}");
}

#[test]
fn get_all_shows_the_live_sessions_and_the_default_does_not() {
    let script = script_with(vec![
        (200, ENGINE_ROWS.to_string()),
        (200, ENGINE_ROWS.to_string()),
    ]);
    let built = built(&script);
    let all = get_op(&built, json!({"ref": "WI-1", "include": ["all"]})).unwrap();
    assert_eq!(ids(&all), ["ses_asleep", "ses_live"], "{all}");
    let lean = get_op(&built, json!({"ref": "WI-1"})).unwrap();
    assert!(lean.get("live_sessions").is_none(), "{lean}");
}
