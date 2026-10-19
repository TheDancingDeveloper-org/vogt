//! Upstream-truth work items and the local overlay, as the core can express them
//! today.
//!
//! Ports the cases of `tests/test_upstream_truth.py`. `work.create` and
//! `work.update` write through to the forge (WI-1068), and so do comment,
//! close and reopen (WI-1780).

use serde_json::json;

use crate::application::collect::sweep_op;
use crate::application::context::Built;
use crate::application::services::forge_link::link_op;
use crate::application::services::forge_module_test::{
    adopt, forgejo_with, four_issues, onboard, reference, SLUG, WHY,
};
use crate::application::services::forge_onboard::onboard_op;
use crate::application::services::forge_ops_test::{
    a_directory_that_is_not_a_repository, insert_project,
};
use crate::application::services::forge_writeback::{writeback_list_op, writeback_op};
use crate::application::services::history::audit_list_op;
use crate::application::services::native_migration::open_native_items;
use crate::application::services::work::{
    board_list_op, comment_op, create_op, get_op, relate_op, transition_op, update_op,
};
use crate::application::taxonomy::create_label_op;
use crate::application::views::backlog_op;
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

/// The subject key of issue `number` on the Forgejo stand-in host.
fn subject(number: i64) -> String {
    format!("forge:forge.example.net/acme/Widgets#{number}")
}

/// Three open issues filed upstream, none of them through Vogt.
fn three_issues() -> serde_json::Value {
    json!([
        {"number": 1, "title": "First", "state": "open", "labels": [{"name": "bug"}],
         "html_url": "https://forge.example.net/acme/Widgets/issues/1",
         "updated_at": "2026-08-01T00:00:01Z", "created_at": "2026-08-01T00:00:01Z",
         "user": {"login": "octo"}, "body": ""},
        {"number": 2, "title": "Second", "state": "open", "labels": [],
         "html_url": "https://forge.example.net/acme/Widgets/issues/2",
         "updated_at": "2026-08-01T00:00:02Z", "created_at": "2026-08-01T00:00:02Z",
         "user": {"login": "octo"}, "body": ""},
        {"number": 3, "title": "Filed on the forge", "state": "open", "labels": [],
         "html_url": "https://forge.example.net/acme/Widgets/issues/3",
         "updated_at": "2026-08-01T00:00:03Z", "created_at": "2026-08-01T00:00:03Z",
         "user": {"login": "octo"}, "body": ""},
    ])
}

/// A folder with no repository, registered on a linked-Forgejo instance.
fn add_folder(built: &Built) {
    insert_project(
        built,
        "folder",
        None,
        &a_directory_that_is_not_a_repository().display().to_string(),
        false,
    );
}

/// Vogt-only changes on an upstream item send nothing to the forge: priority,
/// effort and a non-terminal move stay in the local overlay.
#[test]
fn vogt_only_changes_produce_zero_provider_calls() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, &subject(9));
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();

    update_op(
        &built,
        json!({"ref": reference, "priority": "p0", "effort": "s", "reason": WHY}),
    )
    .unwrap();
    transition_op(
        &built,
        json!({"ref": reference, "to_state": "in_progress", "reason": WHY}),
    )
    .unwrap();

    assert!(
        forgejo.requests.lock().unwrap().is_empty(),
        "priority, effort and a vogt-only state move are overlay-only: {:?}",
        forgejo.requests.lock().unwrap()
    );
    let work = get_op(&built, json!({"ref": reference})).unwrap();
    assert_eq!(work["item"]["priority"], "p0");
    assert_eq!(work["item"]["effort"], "s");
    assert_eq!(work["item"]["state"], "in_progress");
}

/// The default create on an unlinked project is the typed refusal, and it names
/// both ways forward and what still works, so an agent does not conclude its
/// native items cannot be commented on or closed.
#[test]
fn create_on_an_unlinked_project_is_the_typed_refusal() {
    let (built, _) = forgejo_with(four_issues(), false);
    add_folder(&built);
    let refused = create_op(
        &built,
        json!({"kind": "bug", "title": "Nope", "project": "folder", "reason": WHY}),
    );
    let message = match refused {
        Err(VogtError::NotLinked(message)) => message,
        other => panic!("expected the typed refusal, got {other:?}"),
    };
    assert!(
        message.contains("forge link") && message.contains("publish"),
        "{message}"
    );
    assert!(message.contains("local_only: true"), "{message}");
    assert!(message.contains("comments, transitions"), "{message}");
}

/// What `local_only` can create on an unlinked project, it can also discuss and
/// close, and nothing reaches the forge.
#[test]
fn a_local_only_item_on_an_unlinked_project_comments_and_closes() {
    let (built, forgejo) = forgejo_with(four_issues(), false);
    add_folder(&built);
    let created = create_op(
        &built,
        json!({"kind": "bug", "title": "Decide", "project": "folder", "local_only": true, "reason": WHY}),
    )
    .unwrap();
    let reference = reference(&created);

    let commented = comment_op(
        &built,
        json!({"ref": reference, "body": "decided", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(commented["write_back"], "skipped");
    for state in ["in_progress", "review", "done"] {
        let moved = transition_op(
            &built,
            json!({"ref": reference, "to_state": state, "reason": WHY}),
        )
        .unwrap();
        assert_eq!(moved["item"]["state"], state);
    }
    let reopened = transition_op(
        &built,
        json!({"ref": reference, "to_state": "open", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(reopened["item"]["state"], "open");
    assert!(forgejo.mutations().is_empty(), "nothing reached the forge");
}

/// A projectless item is local from the start and moves without any refusal.
#[test]
fn projectless_items_are_untouched_by_the_refusal() {
    let (built, _) = forgejo_with(four_issues(), true);
    let created = create_op(
        &built,
        json!({"kind": "chore", "title": "Loose", "reason": WHY}),
    )
    .unwrap();
    let reference = reference(&created);
    assert!(reference.starts_with("WI-"), "{reference}");
    let moved = transition_op(
        &built,
        json!({"ref": reference, "to_state": "in_progress", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(moved["item"]["state"], "in_progress");
}

/// Each upstream issue appears once in the ranked backlog and once on the board,
/// including one that was filed upstream and never through Vogt.
#[test]
fn board_and_backlog_count_each_upstream_issue_once() {
    let (built, _) = forgejo_with(three_issues(), true);
    onboard(&built);
    // The board reads declared rows, so each issue is adopted before it is
    // counted. Upstream-only issues are absent from the board in this core.
    for number in 1..=3 {
        adopt(&built, &subject(number));
    }

    let ranked = backlog_op(&built, json!({"project": SLUG, "limit": 50})).unwrap();
    let mut refs: Vec<String> = ranked["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["ref"].as_str().unwrap().to_owned())
        .collect();
    refs.sort();
    assert_eq!(refs.len(), 3, "three issues, each once: {refs:?}");
    refs.dedup();
    assert_eq!(refs.len(), 3, "and no ref twice: {refs:?}");

    let board = board_list_op(
        &built,
        json!({
            "project": SLUG,
            "lane_mode": "none",
            "cells": [{"lane_key": "", "state": "open"}],
            "page_size": 10,
        }),
    )
    .unwrap();
    let cell = &board["cells"][0];
    let mut on_board: Vec<String> = cell["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["ref"].as_str().unwrap().to_owned())
        .collect();
    on_board.sort();
    assert_eq!(on_board, refs);
    assert_eq!(cell["total"], 3);
    assert_eq!(board["total"], 3);
}

/// Relations between two upstream items are refused, and the refusal says why.
#[test]
fn relations_between_upstream_items_are_refused_with_the_reason() {
    let (built, _) = forgejo_with(three_issues(), true);
    onboard(&built);
    // Upstream issues that are not adopted: a relation between them is refused
    // with the reason, rather than as a bare not-found.
    let refused = relate_op(
        &built,
        json!({"ref": subject(1), "kind": "depends_on", "target": subject(2), "reason": WHY}),
    );
    match refused {
        Err(error) => assert!(
            error.to_string().contains("upstream-truth"),
            "the refusal names the model: {error}"
        ),
        Ok(value) => panic!("a relation between upstream items must be refused: {value}"),
    }
}

/// `forge.link` on a project with no repository URL is refused, and the project
/// stays unlinked.
#[test]
fn link_refuses_without_a_repo_url() {
    let (built, _) = forgejo_with(four_issues(), false);
    add_folder(&built);
    let refused = link_op(&built, json!({"project": "folder", "reason": WHY}));
    match refused {
        Err(VogtError::LinkRefused(message)) => {
            assert!(message.contains("no repository URL"), "{message}");
        }
        other => panic!("expected a link refusal, got {other:?}"),
    }
    let view = link_state(&built, "folder");
    assert_eq!(view, "unlinked");
}

/// Linking is an explicit, audited act.
#[test]
fn link_is_an_audited_explicit_act() {
    let (built, _) = forgejo_with(four_issues(), true);
    assert_eq!(link_state(&built, SLUG), "linked");
    let listed = audit_list_op(&built, json!({"limit": 100})).unwrap();
    let operations: Vec<&str> = listed["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|record| record["operation"].as_str())
        .collect();
    assert!(operations.contains(&"forge.link"), "{operations:?}");
}

fn link_state(built: &Built, slug: &str) -> String {
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let project = ctx
        .declared
        .read()
        .unwrap()
        .project_by_slug(slug)
        .unwrap()
        .unwrap();
    project.link_state.to_string()
}

/// The explicit local-only create on a linked project with policy `none`: no
/// forge call, a native ref, and the item is what a later link would migrate.
#[test]
fn local_only_create_succeeds_under_policy_none() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    let created = create_op(
        &built,
        json!({"kind": "chore", "title": "Local only", "project": SLUG, "local_only": true, "reason": WHY}),
    )
    .unwrap();

    let reference = reference(&created);
    assert!(
        reference.starts_with("WI-"),
        "a native ref, not a subject key: {reference}"
    );
    assert_eq!(created["item"]["origin"], "created");
    assert_eq!(created["item"]["project_slug"], SLUG);
    assert!(
        forgejo.mutations().is_empty(),
        "local-only wrote nothing upstream"
    );

    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    let view = ctx.declared.read().unwrap();
    let project = view.project_by_slug(SLUG).unwrap().unwrap();
    let pending: Vec<String> = open_native_items(&view, &project)
        .unwrap()
        .into_iter()
        .map(|item| item.reference)
        .collect();
    assert_eq!(pending, vec![reference]);
}

/// Without `local_only`, a create on a linked policy-`none` project refuses
/// before anything is sent, and the refusal names the escape hatch.
#[test]
fn default_create_on_linked_none_still_refuses() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    let refused = create_op(
        &built,
        json!({"kind": "bug", "title": "Refused", "project": SLUG, "reason": WHY}),
    );
    match refused {
        Err(VogtError::UpstreamWriteRefused(message)) => {
            assert!(message.contains("local_only"), "{message}");
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    assert!(forgejo.mutations().is_empty());
}

/// The refusal names the whole chain, not just the first gate: the policy and
/// the credential, and the escape hatch.
#[test]
fn refusal_enumerates_every_missing_gate() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    // Withdraw the only credential the link used, leaving no way upstream.
    let Built::StepSequential(ctx) = &built else {
        panic!("expected a step clock");
    };
    for token in ctx.config.forge_token_files.values() {
        std::fs::remove_file(token).unwrap();
    }
    let refused = create_op(
        &built,
        json!({"kind": "bug", "title": "Refused", "project": SLUG, "reason": WHY}),
    );
    let message = match refused {
        Err(VogtError::UpstreamWriteRefused(message)) => message,
        other => panic!("expected the refusal, got {other:?}"),
    };
    assert!(
        message.contains("'none'"),
        "the policy gate is named: {message}"
    );
    assert!(message.contains("does not permit 'create'"), "{message}");
    assert!(
        message.contains("forge account link"),
        "the credential gate is named: {message}"
    );
    assert!(message.contains("local_only"), "{message}");
    assert!(forgejo.mutations().is_empty());
}

/// `local_only` is the sanctioned opt-in past the unlinked refusal too.
#[test]
fn local_only_create_works_on_an_unlinked_project() {
    let (built, forgejo) = forgejo_with(four_issues(), false);
    add_folder(&built);
    let refused = create_op(
        &built,
        json!({"kind": "bug", "title": "Nope", "project": "folder", "reason": WHY}),
    );
    assert!(
        matches!(refused, Err(VogtError::NotLinked(_))),
        "{refused:?}"
    );

    let created = create_op(
        &built,
        json!({"kind": "chore", "title": "Local", "project": "folder", "local_only": true, "reason": WHY}),
    )
    .unwrap();
    assert_eq!(created["item"]["project_slug"], "folder");
    assert!(reference(&created).starts_with("WI-"));
    assert!(forgejo.mutations().is_empty());
}

/// An observed-only issue onboarded through `forge.onboard` is in the backlog
/// before anything is adopted, and the sweep keeps it to one row.
#[test]
fn an_onboarded_issue_is_one_backlog_row() {
    let (built, _) = forgejo_with(three_issues(), true);
    onboard_op(&built, json!({"project": SLUG, "reason": WHY})).unwrap();
    sweep_op(&built, json!({"project": SLUG, "reason": "again"})).unwrap();
    let ranked = backlog_op(&built, json!({"project": SLUG, "limit": 50})).unwrap();
    let refs: Vec<&str> = ranked["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["ref"].as_str())
        .collect();
    let unique: std::collections::BTreeSet<&str> = refs.iter().copied().collect();
    assert_eq!(
        refs.len(),
        unique.len(),
        "each upstream issue exactly once: {refs:?}"
    );
    assert_eq!(refs.len(), 3);
}

// -- parity regressions the core does not yet meet ---------------------------
//
// These pin behaviour the Python core has and the Rust core does not. They are
// ignored, not deleted, so the gap stays visible in the test run. Each one
// names the work item that owns the fix; the item is not filed yet (see the
// disposition row), so the reason names the review instead.

#[test]
fn board_lists_an_upstream_only_issue_once() {
    let (built, _) = forgejo_with(three_issues(), true);
    onboard(&built);
    adopt(&built, &subject(1));
    adopt(&built, &subject(2));

    let board = board_list_op(
        &built,
        json!({
            "project": SLUG,
            "lane_mode": "none",
            "cells": [{"lane_key": "", "state": "open"}],
            "page_size": 10,
        }),
    )
    .unwrap();
    assert_eq!(board["total"], 3, "{board}");
}

#[test]
fn the_board_pages_with_stable_cursors() {
    let (built, _) = forgejo_with(three_issues(), true);
    onboard(&built);
    for number in 1..=3 {
        adopt(&built, &subject(number));
    }

    let first = board_list_op(
        &built,
        json!({
            "project": SLUG,
            "lane_mode": "none",
            "cells": [{"lane_key": "", "state": "open"}],
            "page_size": 2,
        }),
    )
    .unwrap();
    let cursor = first["cells"][0]["next_cursor"].clone();
    assert!(cursor.is_string(), "{first}");

    let second = board_list_op(
        &built,
        json!({
            "project": SLUG,
            "lane_mode": "none",
            "cells": [{"lane_key": "", "state": "open", "cursor": cursor}],
            "page_size": 2,
            "snapshot": first["snapshot"],
        }),
    )
    .unwrap();
    let first_refs: Vec<&str> = first["cells"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["ref"].as_str())
        .collect();
    let second_refs: Vec<&str> = second["cells"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["ref"].as_str())
        .collect();
    assert_eq!(second_refs.len(), 1, "{second}");
    assert!(
        second_refs
            .iter()
            .all(|reference| !first_refs.contains(reference)),
        "page two must not repeat page one: {first_refs:?} / {second_refs:?}"
    );
    assert!(second["cells"][0]["next_cursor"].is_null(), "{second}");
}

/// An adopted subject is a native row (`observed_first.adopt` inserts one), so
/// Python's `update_work` takes the native path: title, body and label
/// removal are local edits and nothing is sent. The upstream-truth refusals
/// apply to the un-adopted subject (`title_and_body_edits_on_an_upstream_item_are_refused`).
#[test]
fn title_body_and_label_removal_on_an_adopted_item_are_local_edits() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    create_label_op(&built, json!({"name": "shared", "reason": WHY})).unwrap();
    onboard(&built);
    let reference = adopt(&built, &subject(9));
    update_op(
        &built,
        json!({"ref": reference, "add_labels": ["shared"], "reason": WHY}),
    )
    .unwrap();
    forgejo.requests.lock().unwrap().clear();

    let updated = update_op(
        &built,
        json!({"ref": reference, "title": "New", "body": "New body", "remove_labels": ["shared"], "reason": WHY}),
    )
    .unwrap();
    assert_eq!(updated["item"]["title"], json!("New"), "{updated}");
    assert_eq!(updated["item"]["body"], json!("New body"), "{updated}");
    assert!(
        !updated["item"]["labels"]
            .as_array()
            .unwrap()
            .contains(&json!("shared")),
        "{updated}"
    );
    assert!(forgejo.mutations().is_empty(), "{:?}", forgejo.mutations());
}

// -- write-through: create and update (WI-1068) ----------------------------

/// The subject the Forgejo stand-in answers every issue write with.
const CREATED: &str = "forge:forge.example.net/acme/Widgets#12";

/// A linked `acme` with write-back armed, onboarded so its issues resolve.
fn armed() -> (Built, crate::application::services::forge_ops_test::Forgejo) {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    writeback_op(
        &built,
        json!({"project": SLUG, "policy": "full", "reason": WHY}),
    )
    .unwrap();
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();
    (built, forgejo)
}

fn overlay(built: &Built, subject: &str) -> Option<crate::core::WorkOverlay> {
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    ctx.declared.read().unwrap().work_overlay(subject).unwrap()
}

/// A create on a linked project opens the issue upstream, and its subject key
/// is the ref; the overlay carries the vogt-local fields and the ledger records
/// the write.
#[test]
fn create_on_linked_opens_the_issue_and_keys_the_overlay() {
    let (built, forgejo) = armed();
    create_label_op(&built, json!({"name": "bug-ish", "reason": WHY})).unwrap();
    let created = create_op(
        &built,
        json!({"kind": "bug", "title": "Retries forever", "body": "The loop never backs off.",
               "priority": "p1", "project": SLUG, "labels": ["bug-ish"], "reason": WHY}),
    )
    .unwrap();

    assert_eq!(reference(&created), CREATED, "the subject key is the ref");
    assert_eq!(created["item"]["id"], json!(CREATED));
    assert_eq!(created["item"]["origin"], json!("observed"));
    // Forgejo opens the issue, then attaches the labels in a second write.
    let mutations = forgejo.mutations();
    assert!(
        mutations[0].starts_with("POST ") && mutations[0].ends_with("/issues"),
        "{mutations:?}"
    );
    assert!(
        mutations[1..].iter().all(|m| m.ends_with("/labels")),
        "{mutations:?}"
    );
    let bodies = forgejo.bodies.lock().unwrap().clone();
    assert_eq!(bodies[0]["title"], json!("Retries forever"));
    assert!(
        bodies
            .iter()
            .any(|body| body.to_string().contains("bug-ish")),
        "labels travel upstream at create: {bodies:?}"
    );

    let stored = overlay(&built, CREATED).expect("the keyed overlay row exists");
    assert_eq!(stored.priority, Some(crate::core::Priority::P1));
    assert_eq!(stored.workflow_state.as_deref(), Some("open"));

    let ledger = writeback_list_op(&built, json!({})).unwrap();
    let actions = ledger["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{ledger}");
    assert_eq!(actions[0]["action"], json!("create"));
    assert_eq!(actions[0]["outcome"], json!("succeeded"));
    assert_eq!(actions[0]["subject_key"], json!(CREATED));
}

/// A create the forge refuses raises and stores nothing: no overlay and no
/// `work.create` audit row.
#[test]
fn a_failed_create_raises_and_commits_nothing() {
    let (built, _forgejo) = armed();
    // Every write now fails on the forge side.
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let built = Built::StepSequential(ctx.with_forge_transport(|_url, _headers, _body, method| {
        if method == "GET" {
            (200, b"[]".to_vec(), std::collections::BTreeMap::new())
        } else {
            (500, b"{}".to_vec(), std::collections::BTreeMap::new())
        }
    }));
    let refused = create_op(
        &built,
        json!({"kind": "bug", "title": "Lost", "project": SLUG, "reason": WHY}),
    );
    assert!(
        matches!(refused, Err(VogtError::UpstreamWriteFailed(_))),
        "{refused:?}"
    );
    assert!(overlay(&built, CREATED).is_none(), "no overlay row");
    let audit = audit_list_op(&built, json!({"limit": 100})).unwrap();
    assert!(
        !audit.to_string().contains("\"work.create\""),
        "a create the forge refused is not on the record: {audit}"
    );
}

/// Priority and effort on an upstream item are overlay-only: nothing is sent.
#[test]
fn update_of_an_upstream_item_is_overlay_only() {
    let (built, forgejo) = armed();
    onboard(&built);
    forgejo.requests.lock().unwrap().clear();
    let updated = update_op(
        &built,
        json!({"ref": subject(9), "priority": "p0", "effort": "s", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(updated["item"]["priority"], json!("p0"));
    assert!(forgejo.mutations().is_empty(), "{:?}", forgejo.mutations());
    let stored = overlay(&built, &subject(9)).expect("overlay written");
    assert_eq!(stored.priority, Some(crate::core::Priority::P0));
    assert_eq!(stored.effort, Some(crate::core::Effort::S));
}

/// Label adds write through; removals are refused as append-only.
#[test]
fn label_adds_write_through_and_removals_are_refused() {
    let (built, forgejo) = armed();
    create_label_op(&built, json!({"name": "shared", "reason": WHY})).unwrap();
    onboard(&built);
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();

    let updated = update_op(
        &built,
        json!({"ref": subject(9), "add_labels": ["shared"], "reason": WHY}),
    )
    .unwrap();
    assert!(
        updated["item"]["labels"]
            .as_array()
            .unwrap()
            .contains(&json!("shared")),
        "{updated}"
    );
    assert_eq!(forgejo.mutations().len(), 1, "{:?}", forgejo.mutations());
    assert_eq!(
        forgejo.bodies.lock().unwrap().last().cloned(),
        Some(json!({"labels": ["shared"]}))
    );
    let ledger = writeback_list_op(&built, json!({})).unwrap();
    assert_eq!(ledger["actions"][0]["action"], json!("label"), "{ledger}");

    match update_op(
        &built,
        json!({"ref": subject(9), "remove_labels": ["shared"], "reason": WHY}),
    ) {
        Err(VogtError::InvalidRequest(message)) => {
            assert!(message.contains("append-only"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Title and body are upstream truth: an edit is refused before anything is sent.
#[test]
fn title_and_body_edits_on_an_upstream_item_are_refused() {
    let (built, forgejo) = armed();
    onboard(&built);
    forgejo.requests.lock().unwrap().clear();
    for edit in [json!({"title": "New"}), json!({"body": "New"})] {
        let mut params = json!({"ref": subject(9), "reason": WHY});
        params
            .as_object_mut()
            .unwrap()
            .extend(edit.as_object().unwrap().clone());
        match update_op(&built, params) {
            Err(VogtError::InvalidRequest(message)) => {
                assert!(message.contains("upstream truth"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(forgejo.mutations().is_empty());
}

// -- write-through: comment, close and reopen (WI-1780) ----------------------

/// Entering a terminal state closes the issue upstream; leaving it reopens it.
#[test]
fn closing_and_reopening_write_through() {
    let (built, forgejo) = armed();
    onboard(&built);
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();

    let closed = transition_op(
        &built,
        json!({"ref": subject(9), "to_state": "wont_do", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(closed["item"]["state"], json!("wont_do"), "{closed}");
    let mutations = forgejo.mutations();
    assert_eq!(mutations.len(), 1, "{mutations:?}");
    assert!(mutations[0].starts_with("PATCH "), "{mutations:?}");
    assert_eq!(
        forgejo.bodies.lock().unwrap().last().cloned(),
        Some(json!({"state": "closed"}))
    );
    let stored = overlay(&built, &subject(9)).expect("overlay written");
    assert_eq!(stored.workflow_state.as_deref(), Some("wont_do"));

    let reopened = transition_op(
        &built,
        json!({"ref": subject(9), "to_state": "open", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(reopened["item"]["state"], json!("open"), "{reopened}");
    assert_eq!(
        forgejo.bodies.lock().unwrap().last().cloned(),
        Some(json!({"state": "open"}))
    );
    let ledger = writeback_list_op(&built, json!({})).unwrap();
    let actions: Vec<&str> = ledger["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["action"].as_str())
        .collect();
    assert!(
        actions.contains(&"close") && actions.contains(&"reopen"),
        "{ledger}"
    );
}

/// A move between open states is overlay-only and sends nothing.
#[test]
fn an_open_to_open_move_on_an_upstream_item_sends_nothing() {
    let (built, forgejo) = armed();
    onboard(&built);
    forgejo.requests.lock().unwrap().clear();
    let moved = transition_op(
        &built,
        json!({"ref": subject(9), "to_state": "in_progress", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(moved["item"]["state"], json!("in_progress"));
    assert!(forgejo.mutations().is_empty(), "{:?}", forgejo.mutations());
}

/// A comment posts to the forge thread and stores no local comment row.
#[test]
fn a_comment_posts_upstream_and_stores_no_local_row() {
    let (built, forgejo) = armed();
    onboard(&built);
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();

    let result = comment_op(
        &built,
        json!({"ref": subject(9), "body": "Looking now", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(result["write_back"], json!("succeeded"), "{result}");
    let mutations = forgejo.mutations();
    assert_eq!(mutations.len(), 1, "{mutations:?}");
    assert!(mutations[0].starts_with("POST "), "{mutations:?}");
    assert_eq!(
        forgejo.bodies.lock().unwrap().last().unwrap()["body"],
        json!("Looking now")
    );
    let got = get_op(&built, json!({"ref": subject(9)})).unwrap();
    assert_eq!(got["comments"], json!([]), "{got}");
}

/// A hand-off comment is a local record and is refused on an upstream item.
#[test]
fn a_handoff_comment_on_an_upstream_item_is_refused() {
    let (built, forgejo) = armed();
    onboard(&built);
    forgejo.requests.lock().unwrap().clear();
    let refused = comment_op(
        &built,
        json!({"ref": subject(9), "body": "over to you", "kind": "handoff",
               "handoff": {"branch": "x"}, "reason": WHY}),
    );
    match refused {
        Err(VogtError::InvalidRequest(message)) => {
            assert!(message.contains("hand-off"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(forgejo.mutations().is_empty());
}

/// A comment or close the forge refuses raises and changes nothing.
#[test]
fn a_failed_comment_and_close_raise_and_change_nothing() {
    let (built, _forgejo) = armed();
    onboard(&built);
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let built = Built::StepSequential(ctx.with_forge_transport(|_url, _headers, _body, method| {
        if method == "GET" {
            (200, b"[]".to_vec(), std::collections::BTreeMap::new())
        } else {
            (500, b"{}".to_vec(), std::collections::BTreeMap::new())
        }
    }));
    let commented = comment_op(
        &built,
        json!({"ref": subject(9), "body": "lost", "reason": WHY}),
    );
    assert!(
        matches!(commented, Err(VogtError::UpstreamWriteFailed(_))),
        "{commented:?}"
    );
    let closed = transition_op(
        &built,
        json!({"ref": subject(9), "to_state": "wont_do", "reason": WHY}),
    );
    assert!(
        matches!(closed, Err(VogtError::UpstreamWriteFailed(_))),
        "{closed:?}"
    );
    let got = get_op(&built, json!({"ref": subject(9)})).unwrap();
    assert_eq!(
        got["item"]["state"],
        json!("open"),
        "the refused close did not move it"
    );
    let ledger = writeback_list_op(&built, json!({})).unwrap();
    assert!(
        ledger["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["action"] != json!("comment") && row["action"] != json!("close")),
        "{ledger}"
    );
}

/// A walk checks every hop's write-back policy before moving anything: under
/// `comment_only` the closing hop is refused, so the open-state hops before it
/// do not land either.
#[test]
fn a_walk_refused_mid_way_moves_nothing() {
    let (built, forgejo) = armed();
    onboard(&built);
    writeback_op(
        &built,
        json!({"project": SLUG, "policy": "comment_only", "reason": WHY}),
    )
    .unwrap();
    forgejo.requests.lock().unwrap().clear();
    let walked = transition_op(
        &built,
        json!({"ref": subject(9), "to_state": "done", "walk": true, "reason": WHY}),
    );
    match walked {
        Err(VogtError::UpstreamWriteRefused(message)) => {
            assert!(message.contains("'close'"), "{message}");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(forgejo.mutations().is_empty(), "{:?}", forgejo.mutations());
    assert!(
        overlay(&built, &subject(9)).is_none_or(|o| o.workflow_state.is_none()),
        "no hop landed"
    );
    let got = get_op(&built, json!({"ref": subject(9)})).unwrap();
    assert_eq!(got["item"]["state"], json!("open"), "{got}");
}
