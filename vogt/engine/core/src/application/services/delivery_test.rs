//! `work.get` delivery and the agent-only verification gate on done (WI-1330).
//!
//! Ports `tests/test_work_delivery.py`; the pure projection is tested in
//! `delivery_state_test.rs`.

use serde_json::{json, Value};

use super::sessions_test::{moment, opened};
use super::work::{comment_op, get_op, transition_op};
use crate::application::context::{build_context, Built};
use crate::core::{
    ActorKind, LinkState, Origin, Principal, Priority, Project, StepClock, TrustState, WorkItem,
    WorkKind,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ProjectUpdate, ReadView, WriteTxn};

const WHY: &str = "delivery test";

fn project(built: &Built, required: bool) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut project = Project::new("prj_vogt", "vogt", "vogt", "/work/tree", moment());
    project.link_state = LinkState::Unlinked;
    project.agent_policy.verification = Some(json!({"required_for_done": required}));
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_project(&project).unwrap();
    txn.commit().unwrap();
}

fn item(built: &Built, reference: &str, kind: WorkKind) {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_work_item(&WorkItem {
        id: format!("wrk_{reference}"),
        reference: reference.to_string(),
        kind,
        title: "Gate".to_string(),
        body: String::new(),
        state: "review".to_string(),
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
}

/// An instance over the same data, acting as an agent session.
fn agent(built: &Built) -> Built {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    build_context(
        crate::config::VogtConfig {
            data_dir: ctx.config.data_dir.clone(),
            ..crate::config::VogtConfig::default()
        },
        Some(Principal::new("agent:session:ses_x", ActorKind::Agent, "ses_x").unwrap()),
        Some(StepClock::new(moment())),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap()
}

fn gated(required: bool) -> Built {
    let built = opened(None);
    project(&built, required);
    item(&built, "WI-1", WorkKind::Feature);
    built
}

fn verify(built: &Built, reference: &str, lane: &str) {
    comment_op(
        built,
        json!({"ref": reference, "body": "checked", "kind": "verification",
               "verification": {"lane": lane, "what": "it works", "evidence": "200"},
               "reason": WHY}),
    )
    .unwrap();
}

fn done(built: &Built, reference: &str, walk: bool) -> Result<Value, VogtError> {
    transition_op(
        built,
        json!({"ref": reference, "to_state": "done", "walk": walk, "reason": WHY}),
    )
}

#[test]
fn an_agent_is_refused_without_a_verification() {
    let built = gated(true);
    for walk in [false, true] {
        let error = done(&agent(&built), "WI-1", walk).unwrap_err();
        assert!(
            matches!(&error, VogtError::PersonRequired(text)
                if text.contains("work_comment kind=verification")),
            "walk={walk}: {error:?}"
        );
    }
}

#[test]
fn an_agent_closes_once_verified() {
    let built = gated(true);
    verify(&built, "WI-1", "dev");
    let closed = done(&agent(&built), "WI-1", false).unwrap();
    assert_eq!(closed["item"]["state"], "done");
}

#[test]
fn a_person_is_never_refused() {
    let built = gated(true);
    assert_eq!(
        done(&built, "WI-1", false).unwrap()["item"]["state"],
        "done"
    );
}

#[test]
fn no_gate_when_the_policy_is_off_or_the_kind_is_exempt() {
    let off = gated(false);
    assert_eq!(
        done(&agent(&off), "WI-1", false).unwrap()["item"]["state"],
        "done"
    );
    let on = gated(true);
    item(&on, "WI-2", WorkKind::Chore);
    assert_eq!(
        done(&agent(&on), "WI-2", false).unwrap()["item"]["state"],
        "done"
    );
}

#[test]
fn a_verification_on_another_lane_does_not_count() {
    let built = gated(true);
    {
        let Built::StepSequential(ctx) = &built else {
            unreachable!()
        };
        let mut stored = ctx
            .declared
            .read()
            .unwrap()
            .project_by_id("prj_vogt")
            .unwrap()
            .unwrap();
        stored.agent_policy.verification =
            Some(json!({"required_for_done": true, "lanes": ["prod"]}));
        let mut txn = ctx.declared.write().unwrap();
        txn.update_project(
            "prj_vogt",
            &ProjectUpdate {
                agent_policy: Some(stored.agent_policy),
                ..ProjectUpdate::default()
            },
            moment(),
        )
        .unwrap();
        txn.commit().unwrap();
    }
    verify(&built, "WI-1", "dev");
    let error = done(&agent(&built), "WI-1", false).unwrap_err();
    assert!(
        matches!(&error, VogtError::PersonRequired(text) if text.contains("on lane prod")),
        "{error:?}"
    );
}

#[test]
fn work_get_shows_delivery() {
    let built = gated(true);
    comment_op(
        &built,
        json!({"ref": "WI-1", "body": "handed off", "kind": "handoff",
               "handoff": {"branch": "wi-1-gate", "head_sha": "abc"}, "reason": WHY}),
    )
    .unwrap();
    verify(&built, "WI-1", "dev");
    let got = get_op(&built, json!({"ref": "WI-1"})).unwrap();
    let delivery = &got["delivery"];
    // The branch has not been observed, so nothing counts as landed.
    assert!(delivery["landed"].is_null(), "{delivery}");
    assert_eq!(delivery["deployed"], json!([]));
    assert_eq!(delivery["verified"]["lane"], "dev");
    assert_eq!(delivery["verified"]["by"], "local:test-user");
}

fn handoff_comment(branch: &str) -> crate::core::Comment {
    crate::core::Comment {
        id: "c1".to_string(),
        work_item_id: "w1".to_string(),
        actor_id: "a1".to_string(),
        actor_display_name: "worker".to_string(),
        body: "done".to_string(),
        created_at: moment(),
        kind: "handoff".to_string(),
        handoff: Some(json!({"branch": branch})),
        verdict: None,
        verification: None,
    }
}

#[test]
fn landed_needs_the_branch_observed_with_nothing_ahead() {
    use super::work::landed_of;
    let merged =
        json!({"name": "wi-1", "tip": "f00", "ahead": 0, "behind": 3, "default_branch": "main"});
    let landed = landed_of(&[handoff_comment("wi-1")], std::slice::from_ref(&merged)).unwrap();
    assert_eq!(landed.sha, "f00");
    assert!(landed.evidence.contains("main"));
    let mut open_branch = merged;
    open_branch["ahead"] = json!(2);
    assert!(landed_of(&[handoff_comment("wi-1")], &[open_branch]).is_none());
    assert!(landed_of(&[handoff_comment("wi-1")], &[]).is_none());
}
