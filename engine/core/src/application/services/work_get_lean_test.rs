//! `work.get` is lean by default and `include` chooses the rest (WI-4726).

use serde_json::{json, Value};

use super::sessions_test::{moment, opened};
use super::work::{bind_branch_op, comment_op, get_op};
use crate::application::context::Built;
use crate::core::{
    ActorKind, CodingSession, LinkState, Moment, Origin, Priority, Project, TrustState, WorkItem,
    WorkKind,
};
use crate::storage::interface::{DeclaredStore, WriteTxn};

const WHY: &str = "lean get test";

/// A worked item: 10 sessions (1 live, 9 stopped), 8 declared branches and 6
/// comments, the shape an oversight sweep reads.
fn worked() -> Built {
    let built = opened(None);
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
    txn.insert_work_item(&WorkItem {
        id: "wrk_1".to_string(),
        reference: "WI-1".to_string(),
        kind: WorkKind::Feature,
        title: "Lean get".to_string(),
        body: "body".to_string(),
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
    })
    .unwrap();
    for n in 0..10_i64 {
        let started = Moment::from_unix(1_700_000_000 + n * 60, 0);
        txn.insert_session(&CodingSession {
            id: format!("ses_{n:026}"),
            engine_session_id: format!("eng-{n}"),
            project_id: "prj_vogt".to_string(),
            work_item_id: Some("wrk_1".to_string()),
            actor_id: "act_w".to_string(),
            cwd: format!("/home/vogt/Working/session-worktrees/wi-1-lean-get-{n}"),
            template: Some("implement".to_string()),
            model: Some("claude-sonnet-5-5".to_string()),
            effort: Some("medium".to_string()),
            reason: "Implement WI-1: lean work_get default with an include parameter".to_string(),
            purpose: None,
            started_by_session: None,
            started_by_kind: None,
            started_at: started,
            // Session 5 is the only live one.
            stopped_at: (n != 5).then(|| Moment::from_unix(1_700_000_000 + n * 60 + 30, 0)),
        })
        .unwrap();
    }
    txn.commit().unwrap();
    for n in 0..8 {
        bind_branch_op(
            &built,
            json!({"ref": "WI-1", "branch": format!("wi-1-lean-get-attempt-{n}"), "reason": WHY}),
        )
        .unwrap();
    }
    for n in 0..6 {
        comment_op(
            &built,
            json!({"ref": "WI-1", "body": format!("note {n}: a hand-off style comment with some length"), "reason": WHY}),
        )
        .unwrap();
    }
    built
}

fn size(value: &Value) -> usize {
    serde_json::to_string(value).unwrap().len()
}

#[test]
fn the_default_read_is_at_least_70_percent_smaller_than_all() {
    let built = worked();
    let lean = get_op(&built, json!({"ref": "WI-1"})).unwrap();
    let all = get_op(&built, json!({"ref": "WI-1", "include": ["all"]})).unwrap();
    let (lean_size, all_size) = (size(&lean), size(&all));
    assert!(
        lean_size * 10 <= all_size * 3,
        "default {lean_size} bytes is not 70% under all {all_size} bytes"
    );
}

#[test]
fn the_default_is_item_newest_three_comments_relations_and_delivery() {
    let built = worked();
    let got = get_op(&built, json!({"ref": "WI-1"})).unwrap();
    let mut keys: Vec<&str> = got
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["comments", "delivery", "item"], "{got}");
    let comments = got["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 3);
    assert!(comments[0]["body"].as_str().unwrap().starts_with("note 5"));
    assert!(got["item"]["relations"].is_array());
}

#[test]
fn all_returns_the_old_shape() {
    let built = worked();
    let got = get_op(&built, json!({"ref": "WI-1", "include": ["all"]})).unwrap();
    let mut keys: Vec<&str> = got
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "branches",
            "claim_check",
            "comments",
            "delivery",
            "git",
            "item",
            "live_sessions",
            "sessions",
            "walked"
        ]
    );
    assert_eq!(got["sessions"].as_array().unwrap().len(), 10);
    assert_eq!(got["branches"].as_array().unwrap().len(), 8);
    assert_eq!(got["comments"].as_array().unwrap().len(), 6);
    // The old shape repeated the branches inside the git story.
    assert_eq!(got["git"]["branches"], got["branches"]);
    assert!(got.get("sessions_omitted").is_none());
}

#[test]
fn sessions_keep_the_live_and_the_newest_stopped_with_a_count() {
    let built = worked();
    let got = get_op(&built, json!({"ref": "WI-1", "include": ["sessions"]})).unwrap();
    let sessions = got["sessions"].as_array().unwrap();
    // Live session 5, plus stopped 9, 8, 7.
    assert_eq!(sessions.len(), 4, "{got}");
    assert_eq!(got["sessions_omitted"], 6);
    let ids: Vec<&str> = sessions.iter().map(|s| s["id"].as_str().unwrap()).collect();
    for n in [5, 7, 8, 9] {
        assert!(ids.contains(&format!("ses_{n:026}").as_str()), "{ids:?}");
    }
}

#[test]
fn git_does_not_repeat_branches_when_both_are_asked_for() {
    let built = worked();
    let both = get_op(
        &built,
        json!({"ref": "WI-1", "include": ["branches", "git"]}),
    )
    .unwrap();
    assert_eq!(both["branches"].as_array().unwrap().len(), 8);
    assert!(both["git"].get("branches").is_none(), "{both}");
    let git_only = get_op(&built, json!({"ref": "WI-1", "include": ["git"]})).unwrap();
    assert!(git_only.get("branches").is_none());
    assert_eq!(git_only["git"]["branches"].as_array().unwrap().len(), 8);
}

#[test]
fn comment_limit_and_unknown_sections() {
    let built = worked();
    let got = get_op(&built, json!({"ref": "WI-1", "comment_limit": 5})).unwrap();
    assert_eq!(got["comments"].as_array().unwrap().len(), 5);
    let none = get_op(&built, json!({"ref": "WI-1", "include": ["delivery"]})).unwrap();
    assert!(none.get("comments").is_none());
    let bad = get_op(&built, json!({"ref": "WI-1", "include": ["everything"]}));
    assert!(bad.is_err());
}
