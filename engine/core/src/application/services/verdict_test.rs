//! Verdict and verification comment kinds (WI-1319).
//!
//! Ports the verdict cases of `tests/test_handoff.py`: who may post a review
//! verdict or a verification, and the fields Vogt fills from the acting
//! session rather than from the parameters. The field rules (a `blockers`
//! verdict names a blocker; each field set only on its own kind) are the
//! registry validator's, tested in `registry/validate.rs`.

use serde_json::{json, Value};

use super::sessions_test::{insert_project, moment, opened};
use super::work::{comment_op, get_op};
use crate::application::context::{build_context, Built};
use crate::core::{
    ActorKind, CodingSession, Origin, Principal, Priority, StepClock, TrustState, WorkItem,
    WorkKind,
};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const WHY: &str = "verdict test";

fn item(built: &Built, reference: &str) -> String {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_work_item(&WorkItem {
        id: format!("wrk_{reference}"),
        reference: reference.to_string(),
        kind: WorkKind::Feature,
        title: "Verdicts".to_string(),
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

/// A person's instance with WI-1, whose latest hand-off names worker
/// session `eng-ses_worker`.
fn item_with_worker() -> Built {
    let built = opened(None);
    insert_project(&built, "vogt");
    item(&built, "WI-1");
    comment_op(
        &built,
        json!({"ref": "WI-1", "body": "done", "kind": "handoff",
               "handoff": {"branch": "wi-1", "head_sha": "abc123",
                           "session": "eng-ses_worker"},
               "reason": WHY}),
    )
    .unwrap();
    built
}

/// Record session `sid` for `item_id` and return an instance over the same
/// data acting as that session.
fn session(
    built: &Built,
    item_id: &str,
    sid: &str,
    purpose: Option<&str>,
    started_by: Option<&str>,
) -> Built {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let actor = ctx
        .declared
        .read()
        .unwrap()
        .actor_by_identity("local:test-user")
        .unwrap()
        .unwrap();
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_session(&CodingSession {
        id: sid.to_string(),
        engine_session_id: format!("eng-{sid}"),
        project_id: "prj_vogt".to_string(),
        work_item_id: Some(item_id.to_string()),
        actor_id: actor.id,
        cwd: "/w".to_string(),
        template: None,
        model: None,
        effort: None,
        reason: WHY.to_string(),
        purpose: purpose.map(str::to_string),
        started_by_session: None,
        started_by_kind: started_by.map(str::to_string),
        started_at: moment(),
        stopped_at: None,
    })
    .unwrap();
    txn.commit().unwrap();
    build_context(
        crate::config::VogtConfig {
            data_dir: ctx.config.data_dir.clone(),
            ..crate::config::VogtConfig::default()
        },
        Some(Principal::new(&format!("agent:session:{sid}"), ActorKind::Agent, sid).unwrap()),
        Some(StepClock::new(moment())),
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap()
}

fn review(sid: &str) -> (Option<&'static str>, Option<&'static str>, String) {
    (Some("review"), Some("oversight"), sid.to_string())
}

fn post_verdict(built: &Built, verdict: Value) -> Result<Value, VogtError> {
    let mut fields = json!({"head_sha": "abc123", "verdict": "no_blockers"});
    for (key, value) in verdict.as_object().unwrap() {
        fields[key] = value.clone();
    }
    comment_op(
        built,
        json!({"ref": "WI-1", "body": "review", "kind": "verdict",
               "verdict": fields, "reason": WHY}),
    )
    .map(|result| result["comment"].clone())
}

fn post_verification(built: &Built) -> Result<Value, VogtError> {
    comment_op(
        built,
        json!({"ref": "WI-1", "body": "checked", "kind": "verification",
               "verification": {"lane": "dev", "what": "the land", "evidence": "run 7"},
               "reason": WHY}),
    )
    .map(|result| result["comment"].clone())
}

fn refusal(result: Result<Value, VogtError>) -> String {
    match result {
        Err(VogtError::CommentRefused(message)) => message,
        other => panic!("expected comment_refused, got {other:?}"),
    }
}

fn comments(built: &Built) -> Vec<Value> {
    get_op(built, json!({"ref": "WI-1"})).unwrap()["comments"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn a_person_posts_a_verdict() {
    let built = item_with_worker();
    let posted = post_verdict(&built, json!({})).unwrap();
    assert_eq!(posted["verdict"]["reviewer_started_by"], "person");
    assert_eq!(posted["verdict"]["reviewer_session"], Value::Null);
    let read = comments(&built);
    assert!(read
        .iter()
        .any(|c| c["kind"] == "verdict" && c["verdict"] == posted["verdict"]));
}

#[test]
fn a_verdict_stores_every_field_in_python_order() {
    let built = item_with_worker();
    let posted = post_verdict(&built, json!({"verdict": "blockers", "blockers": ["b1"]})).unwrap();
    let keys: Vec<&String> = posted["verdict"].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "head_sha",
            "base",
            "verdict",
            "blockers",
            "deployment_paths_covered",
            "reviewer_session",
            "reviewer_started_by"
        ]
    );
    let keys: Vec<&String> = posted.as_object().unwrap().keys().collect();
    assert_eq!(
        &keys[keys.len() - 4..],
        ["kind", "handoff", "verdict", "verification"]
    );
}

#[test]
fn an_oversight_review_session_posts_on_its_own_item() {
    let built = item_with_worker();
    let (purpose, started_by, sid) = review("ses_rev");
    let reviewer = session(&built, "wrk_WI-1", &sid, purpose, started_by);
    let posted = post_verdict(
        &reviewer,
        json!({"reviewer_session": "forged", "reviewer_started_by": "person"}),
    )
    .unwrap();
    assert_eq!(posted["verdict"]["reviewer_session"], "eng-ses_rev");
    assert_eq!(posted["verdict"]["reviewer_started_by"], "oversight");
}

#[test]
fn a_worker_started_review_session_is_refused() {
    let built = item_with_worker();
    let reviewer = session(
        &built,
        "wrk_WI-1",
        "ses_rev",
        Some("review"),
        Some("worker"),
    );
    assert_eq!(
        refusal(post_verdict(&reviewer, json!({}))),
        "a verdict is posted by a person, or by a review session started by a \
         person or oversight, on the item it reviews: this review was started by worker"
    );
}

#[test]
fn a_review_of_another_item_is_refused() {
    let built = item_with_worker();
    item(&built, "WI-2");
    let reviewer = session(
        &built,
        "wrk_WI-2",
        "ses_rev",
        Some("review"),
        Some("oversight"),
    );
    assert!(
        refusal(post_verdict(&reviewer, json!({}))).ends_with("this review is not bound to WI-1")
    );
}

#[test]
fn the_worker_session_is_refused_on_both_kinds() {
    let built = item_with_worker();
    let worker = session(
        &built,
        "wrk_WI-1",
        "ses_worker",
        Some("review"),
        Some("oversight"),
    );
    assert!(refusal(post_verdict(&worker, json!({}))).ends_with("this session is the worker"));
    assert_eq!(
        refusal(post_verification(&worker)),
        "a verification is posted by someone other than the worker"
    );
}

#[test]
fn a_non_review_session_is_refused() {
    let built = item_with_worker();
    let plain = session(
        &built,
        "wrk_WI-1",
        "ses_plain",
        Some("implement"),
        Some("oversight"),
    );
    assert!(refusal(post_verdict(&plain, json!({}))).ends_with("this session is not a review"));
}

#[test]
fn a_verification_from_another_session_or_a_person_stores() {
    let built = item_with_worker();
    let other = session(&built, "wrk_WI-1", "ses_qa", None, None);
    let posted = post_verification(&other).unwrap();
    assert_eq!(posted["verification"]["by"], "eng-ses_qa");
    let posted = post_verification(&built).unwrap();
    assert_eq!(posted["verification"]["by"], "local:test-user");
    let kinds = comments(&built)
        .iter()
        .filter(|c| c["kind"] == "verification")
        .count();
    assert_eq!(kinds, 2);
}

#[test]
fn a_verdict_on_an_upstream_kind_check_names_the_kind() {
    // A structured field set on the wrong kind is refused before anything is
    // read, with the same words as the validator.
    let built = item_with_worker();
    let err = comment_op(
        &built,
        json!({"ref": "WI-1", "body": "x", "kind": "note",
               "verification": {"lane": "l", "what": "w", "evidence": "e"},
               "reason": WHY}),
    )
    .unwrap_err();
    assert!(err.message().contains("kind verification"), "{err:?}");
}
