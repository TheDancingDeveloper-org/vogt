//! The forge module in use: consolidation (`forge.onboard`), the ranked views
//! that read what it collected (`bugs`, `backlog`, `brief`), write-back on an
//! adopted item, and forge drift from a sweep.
//!
//! Ports the forge-module cases of `tests/test_forge_module.py`. Most run against
//! the Forgejo stand-in in `forge_ops_test.rs`, which is provider-neutral for the
//! service logic. The posture cases need a Dependabot-style surface, which only
//! GitHub has, so they run against a GitHub stand-in defined here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::application::collect::sweep_op;
use crate::application::context::{build_context, Built};
use crate::application::drift::{drift_detect_op, drift_list_op, drift_resolve_op};
use crate::application::forge_transport::SharedForgeTransport;
use crate::application::projects::brief_op;
use crate::application::services::forge_link::link_op;
use crate::application::services::forge_onboard::onboard_op;
use crate::application::services::forge_ops_test::{
    a_directory_that_is_not_a_repository, insert_project, with_forgejo, Forgejo,
};
use crate::application::services::forge_writeback::{writeback_list_op, writeback_op};
use crate::application::services::history::audit_list_op;
use crate::application::services::services_test::{moment, opened, principal, unique};
use crate::application::services::work::{
    adopt_op, comment_op, create_op, get_op, transition_op, update_op,
};
use crate::application::views::{backlog_op, bugs_op};
use crate::config::VogtConfig;
use crate::core::{SequentialIds, StepClock};

pub(super) const WHY: &str = "forge module test";
pub(super) const SLUG: &str = "acme";

/// Four issues, the shapes the Python fixture pinned: an open labelled bug, two
/// closed ones (one of them a labelled bug, the case `bugs` once hid behind a
/// filter), and an open issue with no labels at all.
pub(super) fn four_issues() -> Value {
    json!([
        issue(
            12,
            "Segment fetch retries forever",
            "open",
            &["bug", "p1"],
            None
        ),
        issue(
            5,
            "Ancient closed thing",
            "closed",
            &[],
            Some("2025-01-01T00:00:00Z")
        ),
        issue(
            7,
            "Fixed weeks ago",
            "closed",
            &["bug"],
            Some("2025-02-01T00:00:00Z")
        ),
        issue(9, "Open and unlabelled", "open", &[], None),
    ])
}

fn issue(number: i64, title: &str, state: &str, labels: &[&str], closed_at: Option<&str>) -> Value {
    json!({
        "number": number,
        "title": title,
        "state": state,
        "labels": labels.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
        "html_url": format!("https://github.com/acme/widget/issues/{number}"),
        "updated_at": closed_at.unwrap_or("2026-08-01T00:00:00Z"),
        "created_at": "2026-07-01T00:00:00Z",
        "closed_at": closed_at,
        "user": {"login": "octo"},
        "body": "",
    })
}

/// A Forgejo-hosted project `acme` over `issues`, linked when asked. The fake's
/// request log is cleared after linking, so a test sees only its own traffic.
pub(super) fn forgejo_with(issues: Value, linked: bool) -> (Built, Forgejo) {
    let forgejo = Forgejo::new();
    *forgejo.issues.lock().unwrap() = issues;
    *forgejo.labels.lock().unwrap() = json!([{"name": "bug", "color": "d73a4a"}]);
    let built = with_forgejo(&forgejo);
    if linked {
        link_op(&built, json!({"project": SLUG, "reason": WHY})).unwrap();
    }
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();
    (built, forgejo)
}

/// Move one upstream issue to `state`, the way a maintainer does between sweeps.
pub(super) fn set_upstream_state(issues: &Arc<Mutex<Value>>, number: i64, state: &str) {
    let mut issues = issues.lock().unwrap();
    for issue in issues.as_array_mut().unwrap() {
        if issue["number"] == json!(number) {
            issue["state"] = json!(state);
        }
    }
}

pub(super) fn onboard(built: &Built) -> Value {
    onboard_op(built, json!({"project": SLUG, "reason": WHY})).unwrap()
}

fn titles(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["title"].as_str().map(str::to_owned))
        .collect()
}

pub(super) fn reference(value: &Value) -> String {
    value["item"]["ref"]
        .as_str()
        .or_else(|| value["ref"].as_str())
        .unwrap_or_else(|| panic!("no ref in {value}"))
        .to_owned()
}

pub(super) fn finish(built: &Built, reference: &str) {
    for state in ["in_progress", "review", "done"] {
        transition_op(
            built,
            json!({"ref": reference, "to_state": state, "reason": WHY}),
        )
        .unwrap();
    }
}

pub(super) fn adopt(built: &Built, subject: &str) -> String {
    let adopted = adopt_op(
        built,
        json!({"subject": subject, "reason": "taking this on"}),
    )
    .unwrap();
    reference(&adopted)
}

const FORGEJO_SUBJECT_12: &str = "forge:forge.example.net/acme/Widgets#12";

fn state_mismatch(raised: &Value) -> Option<Value> {
    raised["raised"]
        .as_array()?
        .iter()
        .find(|proposal| proposal["kind"] == "forge_state_mismatch")
        .cloned()
}

fn raised_kinds(raised: &Value) -> Vec<String> {
    raised["raised"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|proposal| proposal["kind"].as_str().map(str::to_owned))
        .collect()
}

// -- consolidation is read-only -------------------------------------------

#[test]
fn onboarding_reads_history_and_mutates_nothing() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    let result = onboard(&built);

    assert_eq!(
        result["issues"], 4,
        "open *and* closed: history is the point"
    );
    assert_eq!(result["labels"], 1);
    assert_eq!(result["mutations"], 0);
    assert!(
        forgejo.mutations().is_empty(),
        "onboarding must change nothing upstream: {:?}",
        forgejo.mutations()
    );
    assert!(
        forgejo.bodies.lock().unwrap().is_empty(),
        "and sent no bodies"
    );
}

#[test]
fn closed_history_does_not_flood_the_backlog() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let page = bugs_op(&built, json!({"limit": 50})).unwrap();
    let listed = titles(&page);
    assert!(listed.iter().any(|t| t.contains("Segment fetch retries")));
    assert!(!listed.iter().any(|t| t.contains("Ancient closed thing")));
}

#[test]
fn a_closed_issue_is_not_an_open_bug() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let listed = titles(&bugs_op(&built, json!({"limit": 50})).unwrap());
    assert!(
        !listed.iter().any(|t| t.contains("Fixed weeks ago")),
        "closed upstream, labelled a bug, must not read as outstanding: {listed:?}"
    );
    assert!(
        listed.iter().any(|t| t.contains("Segment fetch retries")),
        "and the open one is still there: {listed:?}"
    );
}

#[test]
fn what_the_filter_removed_is_reported_rather_than_dropped() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let listed = backlog_op(&built, json!({"limit": 50})).unwrap();
    assert!(
        listed["closed_upstream"].as_i64().unwrap() >= 2,
        "two closed issues in the fixture: {listed}"
    );
}

#[test]
fn a_closed_subject_stays_observable() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let seen = crate::application::observations::observations_list_op(
        &built,
        json!({"kind": "forge.issue", "limit": 50, "offset": 0}),
    )
    .unwrap();
    let observed = seen["observations"].as_array().unwrap();
    assert!(
        observed
            .iter()
            .any(|o| o["payload"]["title"] == "Fixed weeks ago"),
        "the closed observation is evidence and stays queryable: {seen}"
    );
}

#[test]
fn an_unlabelled_issue_says_its_kind_was_guessed() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let ranked = backlog_op(&built, json!({"limit": 50})).unwrap();
    let row = |needle: &str| {
        ranked["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["title"].as_str().is_some_and(|t| t.contains(needle)))
            .unwrap_or_else(|| panic!("no row for {needle}: {ranked}"))
            .clone()
    };
    assert_eq!(row("Open and unlabelled")["classified"], false);
    assert_eq!(row("Segment fetch retries")["classified"], true);
}

#[test]
fn onboarding_without_the_adapter_says_not_collected() {
    // A GitHub repository on an instance with no GitHub token: the host is
    // readable in principle, but the adapter is not configured.
    let built = opened();
    insert_project(
        &built,
        "unwired",
        Some("https://github.com/acme/widget"),
        "/tmp",
        false,
    );
    let result = onboard_op(&built, json!({"project": "unwired", "reason": WHY})).unwrap();
    assert_eq!(result["issues"], 0);
    let detail = result["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("not collected"), "{detail}");
}

#[test]
fn a_consolidation_is_audited_with_the_reason_it_was_given() {
    let (built, _) = forgejo_with(four_issues(), true);
    let why = "Consolidate acme's existing history as playbook phase 5.";
    onboard_op(&built, json!({"project": SLUG, "reason": why})).unwrap();

    let rows = audit_rows(&built, "forge.onboard");
    assert_eq!(rows.len(), 1, "one run, one row");
    assert_eq!(
        rows[0]["reason"], why,
        "the reason is stored, not merely demanded"
    );
    assert_eq!(rows[0]["entity_kind"], "project");
}

#[test]
fn a_consolidation_that_read_nothing_is_audited_too() {
    let built = opened();
    insert_project(
        &built,
        "unwired",
        Some("https://github.com/acme/widget"),
        "/tmp",
        false,
    );
    onboard_op(
        &built,
        json!({"project": "unwired", "reason": "Check upstream."}),
    )
    .unwrap();

    let rows = audit_rows(&built, "forge.onboard");
    let reasons: Vec<&str> = rows.iter().filter_map(|r| r["reason"].as_str()).collect();
    assert_eq!(reasons, vec!["Check upstream."]);
}

/// The audit rows for one operation, newest first.
pub(super) fn audit_rows(built: &Built, operation: &str) -> Vec<Value> {
    let listed = audit_list_op(built, json!({"limit": 100})).unwrap();
    listed["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|record| record["operation"] == operation)
        .cloned()
        .collect()
}

#[test]
fn a_consolidated_project_does_not_brief_as_empty() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);

    let brief = brief_op(&built, json!({"slug": SLUG})).unwrap();
    let ranked = backlog_op(&built, json!({"project": SLUG, "limit": 50})).unwrap();

    assert!(ranked["total_considered"].as_i64().unwrap() > 0);
    assert_eq!(brief["open_work"], ranked["total_considered"]);
    let top: Vec<&Value> = brief["top_backlog"].as_array().unwrap().iter().collect();
    assert!(!top.is_empty(), "not an empty list next to a count");
    let ranked_refs: Vec<&Value> = ranked["items"]
        .as_array()
        .unwrap()
        .iter()
        .take(top.len())
        .map(|item| &item["ref"])
        .collect();
    let brief_refs: Vec<&Value> = top.iter().map(|entry| &entry["ref"]).collect();
    assert_eq!(
        brief_refs, ranked_refs,
        "same ordering, not merely the same size"
    );
}

#[test]
fn the_brief_says_which_population_it_counted() {
    let (built, _) = forgejo_with(four_issues(), true);
    let before = brief_op(&built, json!({"slug": SLUG})).unwrap();
    assert_eq!(
        (
            before["declared_work"].as_i64(),
            before["observed_work"].as_i64()
        ),
        (Some(0), Some(0))
    );

    onboard(&built);

    let after = brief_op(&built, json!({"slug": SLUG})).unwrap();
    assert!(
        after["observed_work"].as_i64().unwrap() > 0,
        "consolidation is visible as observed"
    );
    assert_eq!(
        after["declared_work"], 0,
        "and is not miscounted as declared"
    );
    assert_eq!(
        after["open_work"].as_i64(),
        Some(after["declared_work"].as_i64().unwrap() + after["observed_work"].as_i64().unwrap())
    );
}

#[test]
fn by_state_stays_declared_only_and_says_so() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let brief = brief_op(&built, json!({"slug": SLUG})).unwrap();
    assert!(
        brief["by_state"].as_object().unwrap().is_empty(),
        "nothing declared, so nothing to break down: {brief}"
    );
    assert!(
        brief["open_work"].as_i64().unwrap() > 0,
        "which is not the same as nothing outstanding"
    );
}

// -- the Forgejo host and the unreadable zeros -----------------------------

#[test]
fn the_forgejo_host_is_read_not_refused() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    let result = onboard(&built);

    assert_eq!(
        result["supported"], true,
        "the host is readable, and says so"
    );
    assert!(
        result["detail"].is_null(),
        "nothing to explain: the counts are real"
    );
    assert_eq!(result["issues"], 4);
    assert!(
        forgejo.mutations().is_empty(),
        "and onboarding asked nobody anything"
    );
}

#[test]
fn an_honest_empty_and_an_unread_one_are_still_distinguishable() {
    let (built, _) = forgejo_with(four_issues(), true);
    let readable = onboard(&built);
    assert_eq!(readable["supported"], true);
    assert!(readable["detail"].is_null());

    insert_project(
        &built,
        "unreadable",
        Some("https://gitlab.com/group/project"),
        &a_directory_that_is_not_a_repository().display().to_string(),
        false,
    );
    let unreadable = onboard_op(&built, json!({"project": "unreadable", "reason": WHY})).unwrap();
    assert_eq!(unreadable["issues"], 0);
    assert_eq!(unreadable["supported"], false);
    let detail = unreadable["detail"].as_str().expect("a detail");
    assert!(
        detail.contains("gitlab.com"),
        "name the host it could not read: {detail}"
    );
    assert!(
        detail.contains("forge.example.net"),
        "and what it can: {detail}"
    );
}

#[test]
fn a_project_with_no_repository_url_is_not_an_empty_repository() {
    let (built, _) = forgejo_with(four_issues(), true);
    insert_project(
        &built,
        "local",
        None,
        &a_directory_that_is_not_a_repository().display().to_string(),
        false,
    );
    let result = onboard_op(&built, json!({"project": "local", "reason": WHY})).unwrap();
    assert_eq!(result["supported"], false);
    let detail = result["detail"].as_str().unwrap_or_default();
    assert!(detail.contains("no repository URL"), "{detail}");
}

// -- write-back policy ------------------------------------------------------

#[test]
fn the_default_policy_is_none() {
    let (built, _) = forgejo_with(four_issues(), true);
    let project = crate::application::projects::get_op(&built, json!({"slug": SLUG})).unwrap();
    assert_eq!(project["project"]["write_back"], "none");
}

// -- write-back in practice -----------------------------------------------

#[test]
fn inbound_comments_are_never_copied_into_ours() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, FORGEJO_SUBJECT_12);
    let work = get_op(&built, json!({"ref": reference})).unwrap();
    assert_eq!(
        work["comments"],
        json!([]),
        "nothing upstream has been copied in"
    );
}

#[test]
fn nothing_is_sent_under_the_default_policy() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, FORGEJO_SUBJECT_12);
    forgejo.requests.lock().unwrap().clear();

    let result = comment_op(
        &built,
        json!({"ref": reference, "body": "Local only", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(result["write_back"], "skipped");
    assert!(forgejo.mutations().is_empty(), "{:?}", forgejo.mutations());

    let ledger = writeback_list_op(&built, json!({})).unwrap();
    let first = &ledger["actions"][0];
    assert_eq!(first["outcome"], "skipped");
    assert!(
        first["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("write-back is off"),
        "{first}"
    );
}

#[test]
fn every_write_back_is_recorded_with_its_reason() {
    let (built, _) = forgejo_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, FORGEJO_SUBJECT_12);
    writeback_op(
        &built,
        json!({"project": SLUG, "policy": "comment_only", "reason": WHY}),
    )
    .unwrap();
    comment_op(
        &built,
        json!({"ref": reference, "body": "x", "reason": "because a human asked"}),
    )
    .unwrap();

    let ledger = writeback_list_op(&built, json!({})).unwrap();
    let posted = ledger["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == "comment")
        .expect("the comment is in the ledger");
    assert_eq!(posted["reason"], "because a human asked");
    assert_eq!(posted["policy"], "comment_only");
    assert!(!posted["source_url"].is_null(), "{posted}");
}

#[test]
fn a_comment_posts_upstream_under_comment_only() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, FORGEJO_SUBJECT_12);
    writeback_op(
        &built,
        json!({"project": SLUG, "policy": "comment_only", "reason": WHY}),
    )
    .unwrap();
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();

    let result = comment_op(
        &built,
        json!({"ref": reference, "body": "Looking at this now", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(result["write_back"], "succeeded", "{result}");
    let mutations = forgejo.mutations();
    assert_eq!(mutations.len(), 1, "{mutations:?}");
    assert!(mutations[0].starts_with("POST "), "{mutations:?}");
    let posted = forgejo.bodies.lock().unwrap()[0]["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(posted.contains("Looking at this now"), "{posted}");
    assert!(
        posted.contains(reference.as_str()),
        "the upstream comment says where it came from: {posted}"
    );
}

#[test]
fn finishing_an_item_closes_the_linked_issue_under_full() {
    let (built, forgejo) = forgejo_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, FORGEJO_SUBJECT_12);
    writeback_op(
        &built,
        json!({"project": SLUG, "policy": "full", "reason": WHY}),
    )
    .unwrap();
    forgejo.requests.lock().unwrap().clear();
    forgejo.bodies.lock().unwrap().clear();
    for state in ["in_progress", "review", "done"] {
        transition_op(
            &built,
            json!({"ref": reference, "to_state": state, "reason": WHY}),
        )
        .unwrap();
    }
    let mutations = forgejo.mutations();
    assert_eq!(mutations.len(), 1, "{mutations:?}");
    assert!(mutations[0].starts_with("PATCH "), "{mutations:?}");
    assert_eq!(
        forgejo.bodies.lock().unwrap().last().cloned(),
        Some(json!({"state": "closed"}))
    );
    let ledger = writeback_list_op(&built, json!({"outcome": "succeeded"})).unwrap();
    let actions: Vec<&str> = ledger["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["action"].as_str())
        .collect();
    assert_eq!(actions, ["close"], "{ledger}");
}

// -- forge drift ------------------------------------------------------------
//
// Drift's state comparison reads GitHub subject keys (`gh:`), so these run on
// the GitHub stand-in, as the Python fixture did.

const GITHUB_SUBJECT_12: &str = "gh:acme/widget#12";

/// A GitHub stand-in for the drift and posture cases. The automation toggles
/// and the Dependabot config answer 404, which is what a repository with none
/// set reads as.
#[derive(Clone, Default)]
struct GitHub {
    issues: Arc<Mutex<Value>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl GitHub {
    fn transport(&self) -> SharedForgeTransport {
        let issues = Arc::clone(&self.issues);
        let requests = Arc::clone(&self.requests);
        Arc::new(
            move |url: &str, _headers: &BTreeMap<String, String>, _body: &[u8], method: &str| {
                requests.lock().unwrap().push(format!("{method} {url}"));
                if method != "GET" {
                    let answer = json!({"number": 12, "html_url": "https://github.com/acme/widget/issues/12"});
                    return (200, answer.to_string().into_bytes(), BTreeMap::new());
                }
                if url.contains("/vulnerability-alerts")
                    || url.contains("/automated-security-fixes")
                    || url.contains("/contents/")
                {
                    return (404, Vec::new(), BTreeMap::new());
                }
                let body = if url.contains("/issues") && !url.contains("/comments") {
                    issues.lock().unwrap().clone()
                } else {
                    json!([])
                };
                (200, body.to_string().into_bytes(), BTreeMap::new())
            },
        )
    }
}

/// A GitHub-hosted project `acme` with `issues` upstream. `linked` says whether
/// it has taken the upstream-truth link, which write verbs need.
fn github_with(issues: Value, linked: bool) -> (Built, GitHub) {
    let github = GitHub::default();
    *github.issues.lock().unwrap() = issues;
    let dir = std::env::temp_dir().join(format!(
        "vogt-forge-module-{}-{}",
        std::process::id(),
        unique()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let token = dir.join("forge-token");
    std::fs::write(&token, "secret-token\n").unwrap();
    let mut clock = Some(StepClock::new(moment()));
    let mut ids = Some(SequentialIds::new(None).unwrap());
    crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
    let built = build_context(
        VogtConfig {
            data_dir: dir,
            github_token_file: Some(token),
            ..VogtConfig::default()
        },
        Some(principal()),
        clock,
        ids,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let transport = github.transport();
    let built =
        Built::StepSequential(ctx.with_forge_transport(move |url, headers, body, method| {
            transport(url, headers, body, method)
        }));
    insert_project(
        &built,
        SLUG,
        Some("https://github.com/acme/widget"),
        &a_directory_that_is_not_a_repository().display().to_string(),
        false,
    );
    if linked {
        link_op(&built, json!({"project": SLUG, "reason": WHY})).unwrap();
    }
    (built, github)
}

fn drift_detect(built: &Built, auto_accept: bool) -> Value {
    drift_detect_op(built, json!({"auto_accept": auto_accept, "reason": WHY})).unwrap()
}

#[test]
fn closing_upstream_raises_a_state_mismatch() {
    let (built, github) = github_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, GITHUB_SUBJECT_12);
    set_upstream_state(&github.issues, 12, "closed");
    sweep_op(&built, json!({"project": SLUG, "reason": "look again"})).unwrap();

    let raised = drift_detect(&built, false);
    let proposal = state_mismatch(&raised).expect("a state mismatch is raised");
    let summary = proposal["summary"].as_str().unwrap_or_default();
    assert!(
        summary.contains("closed upstream") || summary.contains("is closed"),
        "{summary}"
    );
    assert_eq!(proposal["proposed_change"]["work_ref"], reference);
}

#[test]
fn accepting_a_state_mismatch_closes_the_item_with_provenance() {
    let (built, github) = github_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, GITHUB_SUBJECT_12);
    set_upstream_state(&github.issues, 12, "closed");
    sweep_op(&built, json!({"project": SLUG, "reason": "look again"})).unwrap();
    let proposal =
        state_mismatch(&drift_detect(&built, false)).expect("a state mismatch is raised");

    drift_resolve_op(
        &built,
        json!({
            "id": proposal["id"],
            "resolution": "accepted",
            "reason": "it was fixed upstream and closed there",
        }),
    )
    .unwrap();

    let work = get_op(&built, json!({"ref": reference})).unwrap();
    assert_eq!(work["item"]["state"], "done");
    let latest = audit_list_op(&built, json!({"limit": 1})).unwrap();
    let record = &latest["records"][0];
    assert_eq!(record["operation"], "drift.resolve");
    assert!(record["reason"]
        .as_str()
        .unwrap()
        .contains("fixed upstream"));
}

#[test]
fn an_item_finished_while_the_issue_it_names_is_open_is_drift() {
    // Unlinked, with `local_only`: the case is a native row that names an
    // issue without being linked to it. A linked write would be the subject.
    let (built, _) = github_with(four_issues(), false);
    onboard(&built);
    let created = create_op(
        &built,
        json!({
            "kind": "bug",
            "title": "Retries forever",
            "body": "GitHub: https://github.com/acme/widget/issues/12 (fix in PR https://github.com/acme/widget/pull/13)",
            "project": SLUG,
            "local_only": true,
            "reason": WHY,
        }),
    )
    .unwrap();
    finish(&built, &reference(&created));

    let raised = drift_detect(&built, true);
    let proposal = raised["raised"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "referenced_issue_state_mismatch")
        .expect("a referenced-issue mismatch is raised")
        .clone();

    assert!(
        proposal["summary"]
            .as_str()
            .unwrap()
            .contains("gh:acme/widget#12"),
        "{proposal}"
    );
    assert!(
        !raised["auto_accepted"]
            .as_array()
            .unwrap()
            .contains(&proposal["id"]),
        "read-only and human-resolved"
    );
    assert_eq!(proposal["proposed_change"]["action"], "review");

    let listed = drift_list_op(&built, json!({"limit": 50})).unwrap();
    assert!(
        listed["human_gated"]["referenced_issue_state_mismatch"].is_string(),
        "{listed}"
    );
}

#[test]
fn a_pull_request_number_in_a_title_is_not_an_issue_reference() {
    let (built, _) = github_with(four_issues(), false);
    onboard(&built);
    let created = create_op(
        &built,
        json!({
            "kind": "bug",
            "title": "Regression from #12",
            "body": "Fixed in #13. See also #9.",
            "project": SLUG,
            "local_only": true,
            "reason": WHY,
        }),
    )
    .unwrap();
    finish(&built, &reference(&created));

    let raised = drift_detect(&built, false);
    assert!(
        !raised_kinds(&raised)
            .iter()
            .any(|k| k == "referenced_issue_state_mismatch"),
        "a sentence describing history is not a reference: {raised}"
    );
}

#[test]
fn an_adopted_link_raises_one_proposal_not_two() {
    let (built, _) = github_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, GITHUB_SUBJECT_12);
    update_op(
        &built,
        json!({
            "ref": reference,
            "body": "GitHub: https://github.com/acme/widget/issues/12",
            "reason": WHY,
        }),
    )
    .unwrap();
    finish(&built, &reference);

    let kinds = raised_kinds(&drift_detect(&built, false));
    assert!(
        !kinds.iter().any(|k| k == "referenced_issue_state_mismatch"),
        "the adopted link already has a proposal; two would be one disagreement: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "forge_state_mismatch"),
        "{kinds:?}"
    );
}

#[test]
fn reopening_finished_work_is_automatic_once_closure_is_observed() {
    let (built, _) = github_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, GITHUB_SUBJECT_12);
    finish(&built, &reference);
    assert_eq!(
        get_op(&built, json!({"ref": reference})).unwrap()["item"]["state"],
        "done"
    );

    let raised = drift_detect(&built, true);
    let mismatch = state_mismatch(&raised).expect("a state mismatch is raised");

    assert_eq!(mismatch["proposed_change"]["to"], "open");
    assert!(
        raised["auto_accepted"]
            .as_array()
            .unwrap()
            .contains(&mismatch["id"]),
        "an observed reopen is automatic: {raised}"
    );
    assert_ne!(
        get_op(&built, json!({"ref": reference})).unwrap()["item"]["state"],
        "done"
    );
    assert!(
        mismatch["summary"]
            .as_str()
            .unwrap()
            .contains("observed reopen"),
        "the proposal says its evidence is a produced fact: {mismatch}"
    );
}

// -- the GitHub posture surface ---------------------------------------------

#[test]
fn a_missing_automation_toggle_names_which_one() {
    let (built, _) = github_with(json!([]), true);
    sweep_op(&built, json!({"project": SLUG, "reason": WHY})).unwrap();
    drift_detect(&built, false);

    let listed = drift_list_op(&built, json!({"limit": 50})).unwrap();
    let gap = listed["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["kind"] == "update_automation_gap")
        .expect("an automation gap is raised")
        .clone();
    let mut missing: Vec<&str> = gap["proposed_change"]["missing"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    missing.sort_unstable();
    assert_eq!(
        missing,
        vec![
            "automated security fixes",
            "version updates",
            "vulnerability alerts"
        ],
        "all three named individually, because they are set in three places"
    );
    assert!(listed["human_gated"]["update_automation_gap"].is_string());
}

#[test]
fn posture_is_three_facts_not_one_boolean() {
    let (built, _) = github_with(json!([]), true);
    sweep_op(&built, json!({"project": SLUG, "reason": WHY})).unwrap();
    let seen = crate::application::observations::observations_list_op(
        &built,
        json!({"kind": "forge.posture", "limit": 1, "offset": 0}),
    )
    .unwrap();
    let payload = &seen["observations"][0]["payload"];
    for key in [
        "version_updates",
        "vulnerability_alerts",
        "automated_security_fixes",
    ] {
        assert!(payload.get(key).is_some(), "posture carries {key}: {seen}");
    }
}

// -- the forge demo ---------------------------------------------------------

/// Enable the module, see that onboarding changed nothing upstream and every
/// issue is in the views with its labels, then close one upstream that an
/// adopted item names: a drift proposal, and accepting it closes the item with
/// provenance.
#[test]
fn the_forge_demo_runs_end_to_end() {
    let (built, github) = github_with(four_issues(), true);
    let result = onboard(&built);
    assert!(
        github
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET ")),
        "not one non-GET request: {:?}",
        github.requests.lock().unwrap()
    );
    assert_eq!(result["mutations"], 0);
    assert_eq!(result["issues"], 4);

    let page = bugs_op(&built, json!({"limit": 50})).unwrap();
    let open_bug = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| {
            item["title"]
                .as_str()
                .is_some_and(|t| t.contains("Segment fetch retries"))
        })
        .expect("the open bug is in the view")
        .clone();
    let labels: Vec<&str> = open_bug["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        labels.contains(&"bug") && labels.contains(&"p1"),
        "labels intact: {labels:?}"
    );

    let subject = open_bug["ref"].as_str().unwrap().to_owned();
    let adopted = adopt_op(
        &built,
        json!({"subject": subject, "reason": "we are fixing this"}),
    )
    .unwrap();
    set_upstream_state(&github.issues, 12, "closed");
    sweep_op(&built, json!({"project": SLUG, "reason": "see what moved"})).unwrap();

    let proposal = state_mismatch(&drift_detect(&built, false)).expect("the closure is a proposal");
    assert_eq!(
        proposal["evidence_snapshot"]["subject_key"],
        subject.as_str()
    );

    drift_resolve_op(
        &built,
        json!({
            "id": proposal["id"],
            "resolution": "accepted",
            "reason": "closed upstream by the maintainer",
        }),
    )
    .unwrap();
    let item_ref = reference(&adopted);
    assert_eq!(
        get_op(&built, json!({"ref": item_ref})).unwrap()["item"]["state"],
        "done"
    );

    let trail: Vec<String> = audit_list_op(&built, json!({"limit": 20})).unwrap()["records"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["operation"].as_str().map(str::to_owned))
        .collect();
    assert!(trail.iter().any(|op| op == "drift.resolve"), "{trail:?}");
    assert!(trail.iter().any(|op| op == "work.adopt"), "{trail:?}");
}

/// The forge is called before the declared write transaction opens, and a
/// refused comment or close is recorded as `failed` without rolling the local
/// change back. Each forge write probes the store for a held write lock.
#[test]
fn a_failed_write_back_keeps_the_local_change_and_records_the_failure() {
    let (built, _forgejo) = forgejo_with(four_issues(), true);
    onboard(&built);
    let reference = adopt(&built, FORGEJO_SUBJECT_12);
    writeback_op(
        &built,
        json!({"project": SLUG, "policy": "full", "reason": WHY}),
    )
    .unwrap();
    let Built::StepSequential(ctx) = built else {
        panic!("expected a step clock");
    };
    let store = ctx.declared.path().to_path_buf();
    let locked_writes = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&locked_writes);
    let built = Built::StepSequential(ctx.with_forge_transport(
        move |url, _headers, _body, method| {
            if method == "GET" {
                return (200, b"[]".to_vec(), BTreeMap::new());
            }
            // A write transaction held by the caller makes BEGIN IMMEDIATE busy.
            let probe = rusqlite::Connection::open(&store).unwrap();
            probe.busy_timeout(std::time::Duration::ZERO).unwrap();
            match probe.execute_batch("BEGIN IMMEDIATE; ROLLBACK;") {
                Ok(()) => {}
                Err(_) => seen.lock().unwrap().push(format!("{method} {url}")),
            }
            (500, b"{}".to_vec(), BTreeMap::new())
        },
    ));
    let commented = comment_op(
        &built,
        json!({"ref": reference, "body": "kept locally", "reason": WHY}),
    )
    .unwrap();
    assert_eq!(commented["write_back"], json!("failed"), "{commented}");
    for state in ["in_progress", "review", "done"] {
        transition_op(
            &built,
            json!({"ref": reference, "to_state": state, "reason": WHY}),
        )
        .unwrap();
    }
    let got = get_op(&built, json!({"ref": reference})).unwrap();
    assert_eq!(got["item"]["state"], json!("done"), "{got}");
    assert_eq!(got["comments"][0]["body"], json!("kept locally"), "{got}");
    let ledger = writeback_list_op(&built, json!({"outcome": "failed"})).unwrap();
    let actions: Vec<&str> = ledger["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["action"].as_str())
        .collect();
    assert!(
        actions.contains(&"comment") && actions.contains(&"close"),
        "{ledger}"
    );
    assert_eq!(
        *locked_writes.lock().unwrap(),
        Vec::<String>::new(),
        "no forge write runs inside the declared write transaction"
    );
}
