//! Hand-off comments (design C11).
//!
//! Ports the cases of `tests/test_handoff.py` the Rust core can express. A
//! hand-off is a comment with a kind and structured fields; the worker brief
//! leads with the latest one and a review brief checks the diff it names. The
//! upstream refusal is ported in `upstream_truth_test.rs`
//! (`a_handoff_comment_on_an_upstream_item_is_refused`), and the refusal of a
//! hand-off without fields is the registry validator's
//! (`a_handoff_comment_without_fields_is_refused` in `registry/validate.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};

use super::sessions::start_session;
use super::sessions_test::{insert_project, moment, opened, script_with, start_params};
use super::work::{comment_op, get_op};
use crate::application::brief::{brief_for_work_item, review_brief_for_work_item, WorkBrief};
use crate::application::context::Built;
use crate::core::{AgentPolicy, Origin, Priority, Project, TrustState, WorkItem, WorkKind};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const WHY: &str = "hand-off test";

/// The base hand-off fields, with `overrides` laid over them.
fn fields(overrides: Value) -> Value {
    let mut base = json!({
        "branch": "wi-1244-wave2",
        "head_sha": "abc123",
        "done": ["kind round-trips"],
        "remaining": ["phase 2"],
    });
    for (key, value) in overrides.as_object().unwrap() {
        base[key] = value.clone();
    }
    base
}

fn built() -> Built {
    let built = opened(None);
    insert_project(&built, "vogt");
    built
}

fn item(built: &Built, reference: &str, title: &str, body: &str) -> String {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let mut txn = ctx.declared.write().unwrap();
    txn.insert_work_item(&WorkItem {
        id: format!("wrk_{reference}"),
        reference: reference.to_string(),
        kind: WorkKind::Feature,
        title: title.to_string(),
        body: body.to_string(),
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

fn note(built: &Built, reference: &str, body: &str) -> Value {
    comment_op(
        built,
        json!({"ref": reference, "body": body, "reason": WHY}),
    )
    .unwrap()
}

fn handoff(built: &Built, reference: &str, body: &str, handoff: Value) -> Value {
    comment_op(
        built,
        json!({"ref": reference, "body": body, "kind": "handoff",
               "handoff": handoff, "reason": WHY}),
    )
    .unwrap()
}

/// The worker brief, or the review brief, for `reference`.
fn brief(built: &Built, reference: &str, review: bool, project: Option<&Project>) -> String {
    let Built::StepSequential(ctx) = built else {
        unreachable!("opened() builds a step clock");
    };
    let view = ctx.declared.read().unwrap();
    let stored = view.work_item_by_ref(reference).unwrap().unwrap();
    let comments = view.comments_for(&stored.id, 200, true).unwrap();
    let related = BTreeMap::new();
    let input = WorkBrief {
        item: &stored,
        session_id: "ses-1",
        ranking: None,
        project,
        comments: &comments,
        related: &related,
    };
    if review {
        review_brief_for_work_item(&input).text
    } else {
        brief_for_work_item(&input).unwrap().text
    }
}

fn policied(policy: Value) -> Project {
    let mut project = Project::new("p1", "vogt", "Vogt", "/work/tree", moment());
    project.agent_policy = serde_json::from_value::<AgentPolicy>(policy).unwrap();
    project
}

/// Ports `_review_of`: a fresh item with one hand-off, reviewed.
fn review_of(built: &Built, reference: &str, project: Option<&Project>, over: Value) -> String {
    item(built, reference, "Review me", "Done means X.");
    handoff(built, reference, "all green, trust me", fields(over));
    brief(built, reference, true, project)
}

#[test]
fn a_handoff_round_trips_and_a_note_stays_a_note() {
    let built = built();
    let reference = item(&built, "WI-1", "Hand-offs", "");
    let noted = note(&built, &reference, "a remark");
    let handed = handoff(
        &built,
        &reference,
        "stopped after the storage write",
        fields(json!({})),
    );

    assert_eq!(noted["comment"]["kind"], "note");
    assert!(noted["comment"]["handoff"].is_null());
    assert_eq!(handed["comment"]["kind"], "handoff");
    assert_eq!(handed["comment"]["handoff"]["branch"], "wi-1244-wave2");
    assert_eq!(
        handed["comment"]["handoff"]["done"],
        json!(["kind round-trips"])
    );

    let got = get_op(&built, json!({"ref": reference})).unwrap();
    let comments = got["comments"].as_array().unwrap();
    let by_body = |body: &str| {
        comments
            .iter()
            .find(|one| one["body"] == body)
            .unwrap()
            .clone()
    };
    assert_eq!(by_body("a remark")["kind"], "note");
    assert_eq!(
        by_body("stopped after the storage write")["handoff"]["head_sha"],
        "abc123"
    );
}

/// Python refuses both halves at model validation. The service refuses fields
/// on a note; a hand-off without fields is refused by the registry validator
/// (`a_handoff_comment_without_fields_is_refused` in `registry/validate.rs`).
#[test]
fn fields_on_a_note_are_refused() {
    let built = built();
    let reference = item(&built, "WI-1", "Hand-offs", "");
    let err = comment_op(
        &built,
        json!({"ref": reference, "body": "a note", "handoff": fields(json!({})), "reason": WHY}),
    )
    .unwrap_err();
    assert!(
        matches!(&err, VogtError::InvalidRequest(m) if m.contains("belong on a handoff comment")),
        "{err:?}"
    );
}

#[test]
fn comment_order_keeps_one_end_of_the_thread() {
    let built = built();
    let reference = item(&built, "WI-1", "A long thread", "");
    for n in 0..5 {
        note(&built, &reference, &format!("note {n}"));
    }
    let bodies = |order: &str| -> Vec<String> {
        get_op(
            &built,
            json!({"ref": reference, "comment_limit": 2, "comment_order": order}),
        )
        .unwrap()["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|one| one["body"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(bodies("newest"), ["note 4", "note 3"]);
    assert_eq!(bodies("oldest"), ["note 0", "note 1"]);
}

#[test]
fn the_brief_leads_with_the_latest_handoff() {
    let built = built();
    let reference = item(&built, "WI-1", "Brief me", "");
    handoff(
        &built,
        &reference,
        "first stop",
        fields(json!({"branch": "old-branch", "remaining": ["superseded"]})),
    );
    note(&built, &reference, "a later remark");
    handoff(
        &built,
        &reference,
        "second stop, the one that counts",
        fields(json!({"branch": "new-branch", "remaining": ["the rest"]})),
    );
    note(&built, &reference, "after the hand-off");

    let brief = brief(&built, &reference, false, None);
    assert!(brief.contains("## Hand-off"), "{brief}");
    assert!(brief.find("## Hand-off").unwrap() < brief.find("## Comments").unwrap());
    assert!(brief.contains("new-branch"));
    assert!(brief.contains("second stop, the one that counts"));
    assert!(!brief.contains("old-branch"));
    // A remark made before the latest hand-off is superseded by it.
    assert!(!brief.contains("a later remark"));
    assert!(brief.contains("after the hand-off"));
}

#[test]
fn a_review_brief_checks_the_diff_against_the_items_own_criteria() {
    let built = built();
    let reference = item(
        &built,
        "WI-1",
        "Brief me",
        "Done means the brief names the diff and the acceptance criteria.",
    );
    note(&built, &reference, "a remark, not the review");
    handoff(
        &built,
        &reference,
        "I finished the composer",
        fields(json!({"branch": "wi-1299-review-brief", "base": "integrate/vogt-next"})),
    );
    let worker = brief(&built, &reference, false, None);
    let review = brief(&built, &reference, true, None);

    assert!(review.starts_with("# Review "), "{review}");
    assert!(review.contains("## Done means"));
    assert!(review.contains("the brief names the diff"));
    assert!(review.contains("## The diff"));
    assert!(review.contains("wi-1299-review-brief"));
    assert!(review.contains("integrate/vogt-next"));
    assert!(review.contains("`git diff integrate/vogt-next...abc123`"));
    assert!(!review.contains("<base>") && !review.contains("<head>"));
    assert!(review.contains("I finished the composer"));
    assert!(!review.contains("a remark, not the review"));
    assert!(!review.contains("## Why this is ranked"));
    assert!(!review.contains("## Comments"));
    assert!(worker.contains("## Hand-off") && !worker.contains("## The diff"));
}

#[test]
fn a_review_brief_states_the_verdict_format() {
    let built = built();
    let review = review_of(&built, "WI-1", None, json!({"base": "main"}));
    assert!(review.contains("`no blockers (head <sha>)`"), "{review}");
    assert!(review.contains("`blockers (head <sha>):` followed by a numbered list"));
    assert!(review.contains("which deployment paths the tests cover"));
    assert!(review.contains("Do not write `no blockers` while describing a regression"));
}

#[test]
fn a_review_brief_is_read_only_and_carries_no_worker_landing_policy() {
    let built = built();
    let project = policied(json!({
        "commit_identity": "sprooty <sprooty@sprooty.com>",
        "test_recipe": "uv run pytest",
        "rules": ["Commit after every step and push after every commit."],
    }));
    let review = review_of(&built, "WI-1", Some(&project), json!({"base": "main"}));
    assert!(review.contains("This review is read-only"), "{review}");
    for forbidden in ["edits", "commits, pushes, merges or deploys"] {
        assert!(review.contains(forbidden));
    }
    assert!(!review.contains("## Project policy"));
    assert!(!review.contains("Commit after every step"));
    assert!(!review.contains("**Commit identity**"));
    assert!(review.contains("- **Tests** uv run pytest"));
}

#[test]
fn every_handoff_field_is_rendered_as_a_claim() {
    let built = built();
    let review = review_of(
        &built,
        "WI-1",
        None,
        json!({"base": "main", "done": ["composer done"], "remaining": ["phase 2"],
               "gates_run": ["pytest green"], "blockers": ["none really"]}),
    );
    let at = review.find("## What the worker claims").unwrap();
    let claims = &review[at..];
    assert!(claims.contains("a claim, not evidence"), "{review}");
    for value in ["composer done", "phase 2", "pytest green", "none really"] {
        assert!(claims.contains(&format!("claimed, not verified: {value}")));
        assert!(!review[..at].contains(value));
    }
    assert!(claims.contains("all green, trust me"));
}

#[test]
fn a_review_brief_fills_in_the_real_diff_command() {
    let built = built();
    let pinned = review_of(
        &built,
        "WI-1",
        None,
        json!({"branch": "wi-9-x", "head_sha": "deadbeef", "base": "b0"}),
    );
    assert!(pinned.contains("`git diff b0...deadbeef`"), "{pinned}");
    assert!(pinned.contains("`git rev-parse wi-9-x`"));
    assert!(pinned.contains("review `deadbeef`"));

    let unpinned = review_of(
        &built,
        "WI-2",
        None,
        json!({"branch": "wi-9-y", "head_sha": null, "base": "b0"}),
    );
    assert!(unpinned.contains("`git diff b0...wi-9-y`"), "{unpinned}");
    assert!(unpinned.contains("The hand-off names no head sha"));
}

#[test]
fn a_review_brief_renders_the_deployed_setup() {
    let built = built();
    let split = "AWS runs the sessions service as a separate container.";
    let project = policied(json!({"deployed_setup": split}));
    let with_notes = review_of(&built, "WI-1", Some(&project), json!({"base": "main"}));
    assert!(with_notes.contains("## Deployed setup"));
    assert!(with_notes.contains(split));

    let bare = policied(json!({}));
    let without = review_of(&built, "WI-2", Some(&bare), json!({"base": "main"}));
    assert!(without.contains("## Deployed setup"));
    assert!(without.contains("records no deployed-setup notes"));
}

#[test]
fn a_review_with_no_handoff_says_there_is_no_diff() {
    let built = built();
    let reference = item(&built, "WI-1", "Bare", "");
    let review = brief(&built, &reference, true, None);
    assert!(review.contains("no hand-off"), "{review}");
    assert!(review.contains("states no acceptance criteria"));
    assert!(!review.contains("git diff"));
}

#[test]
fn review_of_opens_a_session_on_the_review_brief() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-10","name":"vogt","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "vogt");
    let reference = item(&built, "WI-7", "Review me", "Done means it builds.");
    handoff(&built, &reference, "ready", fields(json!({"base": "main"})));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(None, Some("watch the migration"), WHY);
    params.review_of = Some(reference.clone());
    let started = start_session(ctx, &params).unwrap();
    assert_eq!(
        serde_json::to_value(&started).unwrap()["session"]["work_item"],
        reference
    );
    let seen = script.seen.lock().unwrap().join("\n");
    assert!(seen.contains("# Review "), "{seen}");
    assert!(seen.contains("## The diff"));
    // The launch prompt waits without a Task section, so the review has one;
    // the caller's own task stays under "Also asked" (WI-1854).
    let task = seen.find("## Task").expect("a Task section");
    let also = seen.find("## Also asked").expect("an Also asked section");
    assert!(seen[task..].contains("Review the branch"));
    assert!(!seen[task..also].contains("watch the migration"));
    assert!(seen[also..].contains("watch the migration"));
}

#[test]
fn review_of_refuses_a_different_item() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "vogt");
    let first = item(&built, "WI-1", "One", "");
    let second = item(&built, "WI-2", "Two", "");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(None, None, WHY);
    params.review_of = Some(first);
    params.work_item = Some(second);
    let err = start_session(ctx, &params).unwrap_err();
    assert!(
        matches!(&err, VogtError::InvalidRequest(m) if m.contains("work_item names")),
        "{err:?}"
    );
    assert!(script.seen.lock().unwrap().is_empty());
}

#[test]
fn from_handoff_opens_on_the_item() {
    let script = script_with(vec![(
        200,
        r#"{"id":"eng-10","name":"vogt","cwd":"/work/tree"}"#.to_string(),
    )]);
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "vogt");
    let reference = item(&built, "WI-7", "Resume me", "");
    handoff(&built, &reference, "where I stopped", fields(json!({})));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(None, None, WHY);
    params.from_handoff = Some(reference.clone());
    let started = start_session(ctx, &params).unwrap();
    assert_eq!(
        serde_json::to_value(&started).unwrap()["session"]["work_item"],
        reference
    );
    let seen = script.seen.lock().unwrap().join("\n");
    assert!(seen.contains("## Hand-off"), "{seen}");
    assert!(seen.contains("where I stopped"));
}

#[test]
fn from_handoff_refuses_a_different_work_item() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "vogt");
    let first = item(&built, "WI-1", "One", "");
    let second = item(&built, "WI-2", "Two", "");
    handoff(&built, &first, "stopped", fields(json!({})));
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(None, None, WHY);
    params.from_handoff = Some(first);
    params.work_item = Some(second);
    let err = start_session(ctx, &params).unwrap_err();
    assert!(
        matches!(&err, VogtError::InvalidRequest(m)
            if m == "from_handoff names WI-1, but work_item names WI-2"),
        "{err:?}"
    );
    assert!(script.seen.lock().unwrap().is_empty());
}

#[test]
fn from_handoff_refuses_an_item_with_no_handoff() {
    let script = script_with(Vec::new());
    let built = opened(Some(Arc::clone(&script)));
    insert_project(&built, "vogt");
    let reference = item(&built, "WI-1", "Bare", "");
    note(&built, &reference, "only a note");
    let Built::StepSequential(ctx) = &built else {
        unreachable!("opened() builds a step clock");
    };
    let mut params = start_params(None, None, WHY);
    params.from_handoff = Some(reference);
    let err = start_session(ctx, &params).unwrap_err();
    assert!(
        matches!(&err, VogtError::InvalidRequest(m)
            if m == "WI-1 has no hand-off comment to start a session from"),
        "{err:?}"
    );
    assert!(script.seen.lock().unwrap().is_empty());
}
